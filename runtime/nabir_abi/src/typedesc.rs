//! Type descriptors: the read-only record of one type that reflection,
//! plugin identity checks and hot reload all read.
//!
//! A descriptor is emitted into read-only data by the compiler and never
//! changes. Pointers are absolute (fixed by relocations), strings are
//! `{ptr, len}` without a NUL the reader may rely on. Every struct here
//! is laid out without implicit padding at both pointer widths: a `u64`
//! sits at a multiple of 8 and pointer words come in pairs, so a 32-bit
//! target whose `u64` alignment is 4 (i686) gets the same offsets as
//! one whose alignment is 8 (wasm32). [`TYPE_DESC`] and its siblings list
//! the words in order, which is what the compiler emits for any target.
//!
//! Fields are only ever appended; `version` says how far a reader may go.

use crate::typedesc::Word::{Ptr, U16, U32, U64};

/// `TypeDesc::magic`: "NABT" read as a little-endian `u32`.
pub const MAGIC: u32 = u32::from_le_bytes(*b"NABT");
/// `TypeDesc::version` of this layout.
pub const VERSION: u16 = 1;

/// `TypeDesc::kind`.
pub mod kind {
    pub const STRUCT: u16 = 1;
    pub const TUPLE_STRUCT: u16 = 2;
    pub const UNIT: u16 = 3;
    pub const ENUM: u16 = 4;
    pub const UNION: u16 = 5;
    pub const SUM: u16 = 6;
    pub const PRIM: u16 = 7;
    pub const STR: u16 = 8;
    pub const LIST: u16 = 9;
    pub const DICT: u16 = 10;
    pub const ARRAY: u16 = 11;
    pub const TUPLE: u16 = 12;
    pub const FN: u16 = 13;
    pub const ANY: u16 = 14;
    /// A type described by name only (a handle whose insides are
    /// private, a pointer, a closure environment).
    pub const OPAQUE: u16 = 15;
}

/// `TypeDesc::flags`.
pub mod flags {
    /// `#[derive(Inspect)]`: fields, attributes and `ops` are filled.
    pub const INSPECT: u32 = 1;
    /// Only what a layout check needs: no attributes, no `ops`.
    pub const LAYOUT_ONLY: u32 = 2;
    /// A generic instance (`generics` is not empty).
    pub const INSTANCE: u32 = 4;
    /// `TypeOps::new_default` makes a value.
    pub const HAS_DEFAULT: u32 = 8;
}

/// `FieldDesc::flags`.
pub mod field {
    /// A plain `pub` field.
    pub const PUB: u32 = 1;
    /// The field has a `= expr` default.
    pub const HAS_DEFAULT: u32 = 2;
    /// `#[inspect(skip)]`: kept for layout checks, hidden from
    /// reflection.
    pub const SKIP: u32 = 4;
}

/// No tag (`TypeDesc::tag_index`): the type is never held in `any`.
pub const NO_TAG: u32 = u32::MAX;
/// No offset (`FieldDesc::offset`): the field is not at a fixed place
/// (a variant payload laid out by a niche).
pub const NO_OFFSET: u64 = u64::MAX;

/// `{ptr, len}`: `len` items at `ptr` (any pointer when `len` is 0).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Slice<T> {
    pub ptr: *const T,
    pub len: usize,
}

/// UTF-8 bytes, `{ptr, len}`.
pub type Str = Slice<u8>;

impl<T> Slice<T> {
    pub const EMPTY: Self = Self {
        ptr: core::ptr::null(),
        len: 0,
    };

    /// The items.
    ///
    /// # Safety
    ///
    /// `ptr` must point at `len` live items, or `len` must be 0.
    #[must_use]
    pub const unsafe fn get(&self) -> &[T] {
        if self.len == 0 {
            return &[];
        }
        // SAFETY: the caller's contract.
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Slice<u8> {
    /// The bytes.
    ///
    /// # Safety
    ///
    /// As [`Slice::get`].
    #[must_use]
    pub const unsafe fn bytes(&self) -> &[u8] {
        // SAFETY: the caller's contract.
        unsafe { self.get() }
    }
}

/// A Nabi function value: the code address and its environment (null
/// for a named function). Called with the Nabi calling convention; the
/// code takes the environment first.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FnValue {
    pub code: usize,
    pub env: usize,
}

impl FnValue {
    pub const NONE: Self = Self { code: 0, env: 0 };
}

/// One type.
#[repr(C)]
#[derive(Debug)]
pub struct TypeDesc {
    pub magic: u32,
    pub version: u16,
    pub kind: u16,
    pub flags: u32,
    /// Index into the module's `Tags` table; [`NO_TAG`] when none.
    pub tag_index: u32,
    /// First 8 bytes of SHA-256 of `key`, little-endian.
    pub key_hash: u64,
    /// Hash of size, alignment, field offsets and type keys, variant
    /// tags and GC scan words: two modules describing one key agree on
    /// it or the layouts differ.
    pub layout_hash: u64,
    pub size: u64,
    pub align: u32,
    pub reserved0: u32,
    /// `Y<pkg8>:game::Player`; an instance names its arguments' keys,
    /// `Y<pkg8>:core::collections::List<Y<pkg8>:game::Player>`. A
    /// primitive is its name (`i32`).
    pub key: Str,
    /// What `Reflect::type_name` prints: `Player`, `List<f64>`.
    pub name: Str,
    /// `game::Player`.
    pub path: Str,
    /// A struct's own fields; empty for an enum (see `variants`).
    pub fields: Slice<FieldDesc>,
    pub variants: Slice<VariantDesc>,
    /// The type arguments of an instance; a sum's members; a tuple's
    /// parts; a container's element (and a dict's key first).
    pub generics: Slice<TypeRef>,
    /// Runtime attributes on the type (`INSPECT` only).
    pub attrs: Slice<AttrDesc>,
    /// `INSPECT` only, else null.
    pub ops: *const TypeOps,
    pub reserved1: usize,
}

/// A reference to a type that may have no descriptor.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TypeRef {
    /// As `TypeDesc::name`.
    pub name: Str,
    /// As `TypeDesc::key_hash`.
    pub key_hash: u64,
    /// Null when none was emitted.
    pub desc: *const TypeDesc,
    /// As `TypeDesc::kind`.
    pub kind: usize,
}

/// One field of a struct or of an enum variant.
#[repr(C)]
#[derive(Debug)]
pub struct FieldDesc {
    /// `"0"`, `"1"`, .. in a tuple struct or variant.
    pub name: Str,
    /// `#[renamed_from("old")]`, else empty.
    pub renamed_from: Str,
    pub ty: TypeRef,
    pub attrs: Slice<AttrDesc>,
    /// From the start of the value; [`NO_OFFSET`] when not fixed.
    pub offset: u64,
    /// [`field`] bits.
    pub flags: u32,
    /// Its position in the struct or variant, as `TypeOps::get` takes it.
    pub index: u32,
    /// Stores the default into the field of the value at the address
    /// given: `fn(usize)`. Hot builds with a default only.
    pub init_default: FnValue,
}

/// One enum variant.
#[repr(C)]
#[derive(Debug)]
pub struct VariantDesc {
    pub name: Str,
    pub fields: Slice<FieldDesc>,
    /// The stored tag.
    pub tag: u32,
    /// Its position in declaration order.
    pub index: u32,
    /// Runtime attributes on the variant (`INSPECT` only).
    pub attrs: Slice<AttrDesc>,
}

/// One runtime attribute: its value, a constant in the attribute
/// struct's layout.
#[repr(C)]
#[derive(Debug)]
pub struct AttrDesc {
    pub ty: TypeRef,
    pub value: *const u8,
    pub reserved: usize,
}

/// Dynamic access to values of an `Inspect` type. Every entry is a Nabi
/// function value (`FnValue::NONE` when the type lacks it):
///
/// - `get: fn(owner: any, field: usize) -> any | none`: the field, or
///   `none` when `owner` is not this type. An enum's field number is
///   `variant << 16 | index` and answers `none` for another variant;
///   `usize::MAX` answers the index of the variant `owner` holds, as a
///   `usize`.
/// - `set: fn(owner: any, field: usize, value: any) -> any | none`: a copy
///   of `owner` with the field set, or `none` when either does not fit.
/// - `build: fn(variant: usize, fields: Dict<str, any>) -> (any | none,
///   str)`: a new value (an enum's `variant`; 0 for a struct), fields
///   missing from `fields` taking their defaults; or `none` and what is
///   wrong.
/// - `new_default: fn() -> any | none`: reserved; `build` with no fields
///   makes the value with every field at its default.
#[repr(C)]
#[derive(Debug)]
pub struct TypeOps {
    pub get: FnValue,
    pub set: FnValue,
    pub build: FnValue,
    pub new_default: FnValue,
    /// `LIST` and `ARRAY` only, else null.
    pub list: *const ListOps,
    /// `DICT` only, else null.
    pub dict: *const DictOps,
}

/// A list's entries, Nabi function values:
///
/// - `len: fn(any) -> usize` (0 for another type);
/// - `get: fn(any, usize) -> any | none`;
/// - `items: fn(any) -> List<any> | none`;
/// - `from: fn(List<any>) -> any | none` (`none`: an item does not fit).
#[repr(C)]
#[derive(Debug)]
pub struct ListOps {
    pub len: FnValue,
    pub get: FnValue,
    pub items: FnValue,
    pub from: FnValue,
}

/// A dict's entries, Nabi function values:
///
/// - `len: fn(any) -> usize`;
/// - `items: fn(any) -> List<(any, any)> | none`;
/// - `from: fn(List<(any, any)>) -> any | none`.
#[repr(C)]
#[derive(Debug)]
pub struct DictOps {
    pub len: FnValue,
    pub items: FnValue,
    pub from: FnValue,
    pub reserved: FnValue,
}

// SAFETY: descriptors are read-only data.
unsafe impl<T: Sync> Sync for Slice<T> {}
// SAFETY: as above.
unsafe impl Sync for TypeDesc {}
// SAFETY: as above.
unsafe impl Sync for TypeRef {}
// SAFETY: as above.
unsafe impl Sync for FieldDesc {}
// SAFETY: as above.
unsafe impl Sync for AttrDesc {}
// SAFETY: as above.
unsafe impl Sync for TypeOps {}

/// An `any` value (`u32` tag, then the payload word).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Any {
    pub tag: u32,
    pub payload: usize,
}

/// One word of a contract struct, for emitting it at another pointer
/// width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Word {
    U8,
    U16,
    U32,
    U64,
    /// A pointer or `usize`.
    Ptr,
    /// `n` bytes (a byte array).
    Bytes(u32),
}

impl Word {
    /// Size in bytes at pointer width `ptr` (bytes).
    #[must_use]
    pub const fn size(self, ptr: u32) -> u32 {
        match self {
            Self::U8 => 1,
            Self::U16 => 2,
            Self::U32 => 4,
            Self::U64 => 8,
            Self::Ptr => ptr,
            Self::Bytes(n) => n,
        }
    }

    /// Natural alignment, a `u64` at 8.
    #[must_use]
    pub const fn align(self, ptr: u32) -> u32 {
        match self {
            Self::Bytes(_) => 1,
            w => w.size(ptr),
        }
    }
}

/// Size of a struct of `words` at pointer width `ptr`: the words packed
/// in order (the layouts here have no padding).
#[must_use]
pub const fn size_of(words: &[Word], ptr: u32) -> u32 {
    let mut i = 0;
    let mut n = 0;
    while i < words.len() {
        n += words[i].size(ptr);
        i += 1;
    }
    n
}

/// Offset of word `at` of `words` at pointer width `ptr`.
#[must_use]
pub const fn offset_of(words: &[Word], at: usize, ptr: u32) -> u32 {
    let mut i = 0;
    let mut n = 0;
    while i < at {
        n += words[i].size(ptr);
        i += 1;
    }
    n
}

/// `Str` / `Slice<T>`.
pub const SLICE: &[Word] = &[Ptr, Ptr];
/// `FnValue`.
pub const FN_VALUE: &[Word] = &[Ptr, Ptr];

/// [`TypeDesc`], word by word.
pub const TYPE_DESC: &[Word] = &[
    U32, U16, U16, // magic, version, kind
    U32, U32, // flags, tag_index
    U64, U64, U64, // key_hash, layout_hash, size
    U32, U32, // align, reserved0
    Ptr, Ptr, // key
    Ptr, Ptr, // name
    Ptr, Ptr, // path
    Ptr, Ptr, // fields
    Ptr, Ptr, // variants
    Ptr, Ptr, // generics
    Ptr, Ptr, // attrs
    Ptr, Ptr, // ops, reserved1
];

/// [`TypeRef`].
pub const TYPE_REF: &[Word] = &[Ptr, Ptr, U64, Ptr, Ptr];

/// [`FieldDesc`].
pub const FIELD_DESC: &[Word] = &[
    Ptr, Ptr, // name
    Ptr, Ptr, // renamed_from
    Ptr, Ptr, U64, Ptr, Ptr, // ty
    Ptr, Ptr, // attrs
    U64, // offset
    U32, U32, // flags, index
    Ptr, Ptr, // init_default
];

/// [`VariantDesc`].
pub const VARIANT_DESC: &[Word] = &[Ptr, Ptr, Ptr, Ptr, U32, U32, Ptr, Ptr];

/// [`AttrDesc`].
pub const ATTR_DESC: &[Word] = &[Ptr, Ptr, U64, Ptr, Ptr, Ptr, Ptr];

/// [`TypeOps`].
pub const TYPE_OPS: &[Word] = &[Ptr, Ptr, Ptr, Ptr, Ptr, Ptr, Ptr, Ptr, Ptr, Ptr];

/// [`ListOps`] and [`DictOps`].
pub const CONTAINER_OPS: &[Word] = &[Ptr, Ptr, Ptr, Ptr, Ptr, Ptr, Ptr, Ptr];

/// Word positions in [`TYPE_DESC`] the reader looks up by name.
pub mod at {
    pub const MAGIC: usize = 0;
    pub const VERSION: usize = 1;
    pub const KIND: usize = 2;
    pub const FLAGS: usize = 3;
    pub const TAG_INDEX: usize = 4;
    pub const KEY_HASH: usize = 5;
    pub const LAYOUT_HASH: usize = 6;
    pub const SIZE: usize = 7;
    pub const ALIGN: usize = 8;
    pub const KEY: usize = 10;
    pub const NAME: usize = 12;
    pub const PATH: usize = 14;
    pub const FIELDS: usize = 16;
    pub const VARIANTS: usize = 18;
    pub const GENERICS: usize = 20;
    pub const ATTRS: usize = 22;
    pub const OPS: usize = 24;
}

/// FNV-1a 64 of `bytes`: how the runtime's tables index keys and paths.
/// `key_hash` is the compiler's SHA-256 prefix, not this.
#[must_use]
pub const fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
        i += 1;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{offset_of, size_of};

    /// Every word at a multiple of its alignment and the size a multiple
    /// of 8, at both pointer widths: no implicit padding anywhere.
    fn packed(words: &[Word]) {
        for ptr in [4, 8] {
            for i in 0..words.len() {
                let off = super::offset_of(words, i, ptr);
                assert_eq!(off % words[i].align(ptr), 0, "word {i} at {ptr}");
            }
            assert_eq!(super::size_of(words, ptr) % 8, 0, "size at {ptr}");
        }
    }

    #[test]
    fn layouts_have_no_padding() {
        for w in [
            SLICE,
            FN_VALUE,
            TYPE_DESC,
            TYPE_REF,
            FIELD_DESC,
            VARIANT_DESC,
            ATTR_DESC,
            TYPE_OPS,
            CONTAINER_OPS,
        ] {
            packed(w);
        }
    }

    #[test]
    fn word_lists_match_the_structs() {
        let p = usize::BITS / 8;
        let sizes = [
            (TYPE_DESC, size_of::<TypeDesc>()),
            (TYPE_REF, size_of::<TypeRef>()),
            (FIELD_DESC, size_of::<FieldDesc>()),
            (VARIANT_DESC, size_of::<VariantDesc>()),
            (ATTR_DESC, size_of::<AttrDesc>()),
            (TYPE_OPS, size_of::<TypeOps>()),
            (CONTAINER_OPS, size_of::<ListOps>()),
            (CONTAINER_OPS, size_of::<DictOps>()),
        ];
        for (w, n) in sizes {
            assert_eq!(super::size_of(w, p) as usize, n);
        }
        let offsets = [
            (TYPE_DESC, at::KIND, offset_of!(TypeDesc, kind)),
            (TYPE_DESC, at::FLAGS, offset_of!(TypeDesc, flags)),
            (TYPE_DESC, at::TAG_INDEX, offset_of!(TypeDesc, tag_index)),
            (TYPE_DESC, at::KEY_HASH, offset_of!(TypeDesc, key_hash)),
            (
                TYPE_DESC,
                at::LAYOUT_HASH,
                offset_of!(TypeDesc, layout_hash),
            ),
            (TYPE_DESC, at::SIZE, offset_of!(TypeDesc, size)),
            (TYPE_DESC, at::ALIGN, offset_of!(TypeDesc, align)),
            (TYPE_DESC, at::KEY, offset_of!(TypeDesc, key)),
            (TYPE_DESC, at::NAME, offset_of!(TypeDesc, name)),
            (TYPE_DESC, at::PATH, offset_of!(TypeDesc, path)),
            (TYPE_DESC, at::FIELDS, offset_of!(TypeDesc, fields)),
            (TYPE_DESC, at::VARIANTS, offset_of!(TypeDesc, variants)),
            (TYPE_DESC, at::GENERICS, offset_of!(TypeDesc, generics)),
            (TYPE_DESC, at::ATTRS, offset_of!(TypeDesc, attrs)),
            (TYPE_DESC, at::OPS, offset_of!(TypeDesc, ops)),
            (FIELD_DESC, 11, offset_of!(FieldDesc, offset)),
            (FIELD_DESC, 12, offset_of!(FieldDesc, flags)),
            (FIELD_DESC, 14, offset_of!(FieldDesc, init_default)),
            (TYPE_REF, 3, offset_of!(TypeRef, desc)),
            (VARIANT_DESC, 4, offset_of!(VariantDesc, tag)),
            (VARIANT_DESC, 6, offset_of!(VariantDesc, attrs)),
            (ATTR_DESC, 5, offset_of!(AttrDesc, value)),
            (TYPE_OPS, 8, offset_of!(TypeOps, list)),
        ];
        for (w, i, off) in offsets {
            assert_eq!(super::offset_of(w, i, p) as usize, off, "word {i}");
        }
    }

    #[test]
    fn magic_reads_nabt() {
        assert_eq!(&MAGIC.to_le_bytes(), b"NABT");
        assert_eq!(fnv64(b""), 0xcbf2_9ce4_8422_2325);
    }
}
