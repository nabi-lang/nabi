//! Module descriptors: what a plugin (or a host) tells the loader about
//! itself, in one exported data symbol.
//!
//! A plugin exports `nabi_module` (Mach-O `_nabi_module`); a host defines
//! `nabi_host_module`. The descriptor is read-only except the tables
//! marked `*mut`, which the loader fills. Pointers are absolute. Layout
//! rules as in `typedesc`: no implicit padding at either pointer width,
//! word lists for the compiler, fields only appended.
//!
//! The runtime entries both sides call are listed at the bottom.

use crate::typedesc::Word::{Bytes, Ptr, U16, U32, U64};
use crate::typedesc::{Slice, Str, TypeDesc, Word};

/// `ModuleDesc::magic`: "NABM" read as a little-endian `u32`.
pub const MAGIC: u32 = u32::from_le_bytes(*b"NABM");
/// `ModuleDesc::version` of this layout.
pub const VERSION: u16 = 1;

/// The symbol a plugin exports its descriptor under (no Mach-O `_`).
pub const PLUGIN_SYMBOL: &str = "nabi_module";
/// The symbol of a host's descriptor.
pub const HOST_SYMBOL: &str = "nabi_host_module";

/// `ModuleDesc::flags`.
pub mod flags {
    /// A program that loads plugins (keeps its export table).
    pub const HOST: u16 = 1;
    pub const PLUGIN: u16 = 2;
    /// A hot-reload build: `hot_version` and `functions` are set.
    pub const HOT: u16 = 4;
    /// Gave a C callback of its own code out: never unmapped.
    pub const NO_UNLOAD: u16 = 8;
}

/// `Import::kind` and `Export::kind`.
pub mod sym {
    /// A function called through a stub; the slot holds its address.
    pub const FN: usize = 0;
    /// Data: the slot holds the address of the data.
    pub const DATA: usize = 1;
    /// As `FN`, rebound by the hot loader.
    pub const HOT_FN: usize = 2;
    /// An export only: the function-value thunk (`FnValue::code`) of a
    /// function.
    pub const FN_VALUE: usize = 3;
}

/// A package the module provides or needs.
#[repr(C)]
#[derive(Debug)]
pub struct PkgDesc {
    pub name: Str,
    pub version: Str,
    /// The 8 hex digits of the package hash in its `_Y` symbols.
    pub pkg8: [u8; 8],
    /// The full SHA-256 of the package's content.
    pub sha256: [u8; 32],
}

/// A symbol the module takes from the process.
#[repr(C)]
#[derive(Debug)]
pub struct Import {
    /// The mangled name, the binding key.
    pub sym: Str,
    /// First 64 bits of SHA-256 of the canonical signature text written
    /// with type keys; a static's type key for data.
    pub sig: u64,
    pub slot: *mut usize,
    /// [`sym`] kind.
    pub kind: usize,
}

/// A symbol the module gives the process, sorted by `sym`.
#[repr(C)]
#[derive(Debug)]
pub struct Export {
    pub sym: Str,
    /// `enemies::spawn`: set for `pub` non-generic functions and statics
    /// (what `Plugin::get` finds), else empty.
    pub path: Str,
    pub sig: u64,
    pub addr: usize,
    pub kind: usize,
    /// The type `sig` hashes, as `type_name` writes it (`fn(i32) ->
    /// List<str>`), for error messages; empty when `sig` is 0.
    pub sig_text: Str,
}

/// The module's GC scan tables; `ids` is filled by the loader with the
/// type id it gave each table.
#[repr(C)]
#[derive(Debug)]
pub struct GcTypes {
    pub tables: *const *const u8,
    pub ids: *mut u32,
    pub n: u32,
    pub reserved: u32,
}

/// One `any` tag the module's code uses: the type key and its
/// `fnv64`.
#[repr(C)]
#[derive(Debug)]
pub struct TagKey {
    pub key: Str,
    pub hash: u64,
}

/// The module's `any` tags; `tags[i]` is the tag of `keys[i]`, filled by
/// the loader (a host has its compile-time tags there already).
#[repr(C)]
#[derive(Debug)]
pub struct Tags {
    pub keys: *const TagKey,
    pub tags: *mut u32,
    pub n: u32,
    pub reserved: u32,
}

/// A static root: a word that may hold a reference (`any`: the payload
/// word of an `any`, whose tag is the word before).
#[repr(C)]
#[derive(Debug)]
pub struct Root {
    pub word: *const usize,
    pub any: usize,
}

/// An `any Tr` method body for one tag: what `nabi_any_dispatch` answers.
#[repr(C)]
#[derive(Debug)]
pub struct Dispatch {
    /// The method key's hash (trait key and method name).
    pub method: u64,
    pub code: usize,
    /// Index into the module's `Tags`.
    pub tag_index: usize,
}

/// The extent of one function (hot builds), sorted by `start`.
#[repr(C)]
#[derive(Debug)]
pub struct FnRange {
    /// From `text_lo`.
    pub start: u32,
    pub len: u32,
    /// What the frame holds by value across safepoints; the hot loader's
    /// format.
    pub frame: *const u8,
    pub reserved: usize,
}

/// One module.
#[repr(C)]
#[derive(Debug)]
pub struct ModuleDesc {
    pub magic: u32,
    pub version: u16,
    /// [`flags`] bits.
    pub flags: u16,
    /// 0 unless `HOT`.
    pub hot_version: u64,
    /// The module's code is `[text_lo, text_hi)`.
    pub text_lo: usize,
    pub text_hi: usize,
    /// The compiler id: modules of one process match exactly.
    pub toolchain: Str,
    /// The target triple.
    pub target: Str,
    /// The plugin's package name; a host's program name.
    pub name: Str,
    pub provides: Slice<PkgDesc>,
    /// The packages it imports from the process.
    pub needs: Slice<PkgDesc>,
    pub imports: Slice<Import>,
    pub exports: Slice<Export>,
    pub gc_types: GcTypes,
    pub tags: Tags,
    pub roots: Slice<Root>,
    pub types: Slice<*const TypeDesc>,
    pub dispatch: Slice<Dispatch>,
    /// `HOT` only.
    pub functions: Slice<FnRange>,
    /// `HOT` only, else null.
    pub hot: *const HotDesc,
    pub reserved: usize,
}

/// `HotDesc::magic`: "NABH".
pub const HOT_MAGIC: u32 = u32::from_le_bytes(*b"NABH");
/// `FrameInfo::magic`: "NABF".
pub const FRAME_MAGIC: u32 = u32::from_le_bytes(*b"NABF");

/// What a hot build adds to its descriptor.
#[repr(C)]
#[derive(Debug)]
pub struct HotDesc {
    pub magic: u32,
    pub version: u16,
    pub flags: u16,
    /// The version this one was diffed against; 0 for the first.
    pub base_version: u64,
    /// What each of `ModuleDesc::gc_types` holds, in the same order.
    pub gc: Slice<GcKey>,
    /// The signature of every hot-package function the code calls
    /// through a `HOT_FN` slot, sorted by `sym`.
    pub sigs: Slice<SymSig>,
}

/// `HotDesc::flags`.
pub mod hot_flags {
    /// Each `FrameInfo` address is also in a root word of its frame.
    pub const FRAMES_ROOTED: u16 = 1;
}

/// `GcKey::kind`.
pub mod gc_kind {
    /// No hot-package type by value.
    pub const NONE: u32 = 0;
    /// One value of type `key_hash`.
    pub const VALUE: u32 = 1;
    /// A buffer of elements of type `key_hash`.
    pub const ELEMS: u32 = 2;
    /// Another aggregate holding the `held` types by value (a closure
    /// environment, a tuple, a coroutine frame).
    pub const HOLDS: u32 = 3;
    /// Laid out like each of the `held` types: which one is not known.
    pub const AMBIGUOUS: u32 = 4;
}

/// The identity of one GC scan table of a hot module.
#[repr(C)]
#[derive(Debug)]
pub struct GcKey {
    /// `TypeDesc::key_hash` (VALUE, ELEMS).
    pub key_hash: u64,
    /// [`gc_kind`].
    pub kind: u32,
    pub reserved: u32,
    /// `TypeDesc::key_hash`es (HOLDS, AMBIGUOUS).
    pub held: Slice<u64>,
}

/// A symbol and its signature hash (as `Export::sig`).
#[repr(C)]
#[derive(Debug)]
pub struct SymSig {
    pub sym: Str,
    pub sig: u64,
}

/// What a function of a hot module holds by value (the word before
/// its shadow-frame descriptor points here; `FnRange::frame` too).
#[repr(C)]
#[derive(Debug)]
pub struct FrameInfo {
    pub magic: u32,
    /// Bytes of code from `code`; 0 when not known.
    pub len: u32,
    /// The function's first instruction; 0 when not known.
    pub code: usize,
    /// Its first line.
    pub line: usize,
    /// As messages print it: `game::spawn_wave`.
    pub name: Str,
    /// As diagnostics print it: `src/wave.nabi`.
    pub file: Str,
    pub held: Slice<Held>,
}

/// One hot-package type a function holds by value.
#[repr(C)]
#[derive(Debug)]
pub struct Held {
    /// `TypeDesc::key_hash`.
    pub key_hash: u64,
    /// The first line using it; 0 when not known.
    pub line: u32,
    pub reserved: u32,
    /// As messages print it: `World`.
    pub name: Str,
}

// SAFETY: read-only once the loader has filled its tables, which it does
// with the world stopped.
unsafe impl Sync for ModuleDesc {}
// SAFETY: read-only data.
unsafe impl Sync for TagKey {}
// SAFETY: read-only data.
unsafe impl Sync for HotDesc {}
// SAFETY: read-only data.
unsafe impl Sync for FrameInfo {}

/// [`PkgDesc`], word by word.
pub const PKG_DESC: &[Word] = &[Ptr, Ptr, Ptr, Ptr, Bytes(8), Bytes(32)];
/// [`Import`].
pub const IMPORT: &[Word] = &[Ptr, Ptr, U64, Ptr, Ptr];
/// [`Export`].
pub const EXPORT: &[Word] = &[Ptr, Ptr, Ptr, Ptr, U64, Ptr, Ptr, Ptr, Ptr];
/// [`GcTypes`] and [`Tags`].
pub const TABLE: &[Word] = &[Ptr, Ptr, U32, U32];
/// [`TagKey`].
pub const TAG_KEY: &[Word] = &[Ptr, Ptr, U64];
/// [`Root`].
pub const ROOT: &[Word] = &[Ptr, Ptr];
/// [`Dispatch`].
pub const DISPATCH: &[Word] = &[U64, Ptr, Ptr];
/// [`FnRange`].
pub const FN_RANGE: &[Word] = &[U32, U32, Ptr, Ptr];
/// [`ModuleDesc`].
pub const MODULE_DESC: &[Word] = &[
    U32, U16, U16, // magic, version, flags
    U64, // hot_version
    Ptr, Ptr, // text_lo, text_hi
    Ptr, Ptr, // toolchain
    Ptr, Ptr, // target
    Ptr, Ptr, // name
    Ptr, Ptr, // provides
    Ptr, Ptr, // needs
    Ptr, Ptr, // imports
    Ptr, Ptr, // exports
    Ptr, Ptr, U32, U32, // gc_types
    Ptr, Ptr, U32, U32, // tags
    Ptr, Ptr, // roots
    Ptr, Ptr, // types
    Ptr, Ptr, // dispatch
    Ptr, Ptr, // functions
    Ptr, Ptr, // hot
];
/// [`HotDesc`].
pub const HOT_DESC: &[Word] = &[U32, U16, U16, U64, Ptr, Ptr, Ptr, Ptr];
/// [`GcKey`].
pub const GC_KEY: &[Word] = &[U64, U32, U32, Ptr, Ptr];
/// [`SymSig`].
pub const SYM_SIG: &[Word] = &[Ptr, Ptr, U64];
/// [`FrameInfo`].
pub const FRAME_INFO: &[Word] = &[U32, U32, Ptr, Ptr, Ptr, Ptr, Ptr, Ptr, Ptr, Ptr];
/// [`Held`].
pub const HELD: &[Word] = &[U64, U32, U32, Ptr, Ptr];

/// The runtime entries of the contract, by name, as C prototypes. The
/// type registry is `nabir`'s `types` module, the module loader its
/// `plugin` module.
pub mod entry {
    use crate::module::{ModuleDesc, TagKey};
    use crate::typedesc::TypeDesc;

    /// `nabi_types_register(descs, n, tags, out_tags, nt) -> i32`.
    ///
    /// Registers `n` descriptors and resolves `nt` tags: `out_tags[i]`
    /// already set (not `u32::MAX`) is kept, which is how a host passes
    /// its compile-time tags; a key registered before gets its tag; a
    /// new key gets FNV-31 of the key, probed past every tag in use.
    /// Returns 0, or `1 + i` when `descs[i]`'s key is registered with
    /// another `layout_hash` (nothing is registered then).
    pub type TypesRegister = unsafe extern "C" fn(
        descs: *const *const TypeDesc,
        n: usize,
        tags: *const TagKey,
        out_tags: *mut u32,
        nt: usize,
    ) -> i32;
    /// `nabi_types_by_tag(tag) -> *const TypeDesc`: null when no
    /// descriptor has the tag (the `ANY_BOXED` bit is ignored).
    pub type TypesByTag = unsafe extern "C" fn(tag: u32) -> *const TypeDesc;
    /// `nabi_types_by_path(p, len) -> *const TypeDesc`: the newest
    /// registration of path `p`, or null.
    pub type TypesByPath = unsafe extern "C" fn(p: *const u8, len: usize) -> *const TypeDesc;
    /// `nabi_types_unregister(descs, n)`: drops the registrations of a
    /// module being freed.
    pub type TypesUnregister = unsafe extern "C" fn(descs: *const *const TypeDesc, n: usize);
    /// `nabi_module_register(m) -> i32`: loads a module's tables into the
    /// process inside a short stop-the-world; 0 on success.
    pub type ModuleRegister = unsafe extern "C" fn(m: *const ModuleDesc) -> i32;
    /// `nabi_any_dispatch(method, tag) -> usize`: the code of `method` for
    /// a value of `tag` from a loaded module; 0 when none.
    pub type AnyDispatch = unsafe extern "C" fn(method: u64, tag: u32) -> usize;
    /// `nabi_tls_get() -> *mut u8`: this thread's `nabi_tls` block in the
    /// host.
    pub type TlsGet = unsafe extern "C" fn() -> *mut u8;

    pub const TYPES_REGISTER: &str = "nabi_types_register";
    pub const TYPES_BY_TAG: &str = "nabi_types_by_tag";
    pub const TYPES_BY_PATH: &str = "nabi_types_by_path";
    pub const TYPES_UNREGISTER: &str = "nabi_types_unregister";
    pub const MODULE_REGISTER: &str = "nabi_module_register";
    pub const ANY_DISPATCH: &str = "nabi_any_dispatch";
    pub const TLS_GET: &str = "nabi_tls_get";
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typedesc::{offset_of, size_of};

    #[test]
    fn layouts_have_no_padding() {
        for words in [
            PKG_DESC,
            IMPORT,
            EXPORT,
            TABLE,
            TAG_KEY,
            ROOT,
            DISPATCH,
            FN_RANGE,
            MODULE_DESC,
            HOT_DESC,
            GC_KEY,
            SYM_SIG,
            FRAME_INFO,
            HELD,
        ] {
            for ptr in [4, 8] {
                for i in 0..words.len() {
                    assert_eq!(offset_of(words, i, ptr) % words[i].align(ptr), 0);
                }
                assert_eq!(size_of(words, ptr) % 8, 0);
            }
        }
    }

    #[test]
    fn word_lists_match_the_structs() {
        let p = usize::BITS / 8;
        let sz = |w: &[Word]| size_of(w, p) as usize;
        assert_eq!(sz(PKG_DESC), core::mem::size_of::<PkgDesc>());
        assert_eq!(sz(IMPORT), core::mem::size_of::<Import>());
        assert_eq!(sz(EXPORT), core::mem::size_of::<Export>());
        assert_eq!(sz(TABLE), core::mem::size_of::<GcTypes>());
        assert_eq!(sz(TABLE), core::mem::size_of::<Tags>());
        assert_eq!(sz(TAG_KEY), core::mem::size_of::<TagKey>());
        assert_eq!(sz(ROOT), core::mem::size_of::<Root>());
        assert_eq!(sz(DISPATCH), core::mem::size_of::<Dispatch>());
        assert_eq!(sz(FN_RANGE), core::mem::size_of::<FnRange>());
        assert_eq!(sz(MODULE_DESC), core::mem::size_of::<ModuleDesc>());
        let off = |i| offset_of(MODULE_DESC, i, p) as usize;
        assert_eq!(off(3), core::mem::offset_of!(ModuleDesc, hot_version));
        assert_eq!(off(6), core::mem::offset_of!(ModuleDesc, toolchain));
        assert_eq!(off(20), core::mem::offset_of!(ModuleDesc, gc_types));
        assert_eq!(off(24), core::mem::offset_of!(ModuleDesc, tags));
        assert_eq!(off(28), core::mem::offset_of!(ModuleDesc, roots));
        assert_eq!(off(30), core::mem::offset_of!(ModuleDesc, types));
        assert_eq!(off(34), core::mem::offset_of!(ModuleDesc, functions));
        assert_eq!(off(36), core::mem::offset_of!(ModuleDesc, hot));
        assert_eq!(&MAGIC.to_le_bytes(), b"NABM");
    }

    #[test]
    fn hot_word_lists_match_the_structs() {
        let p = usize::BITS / 8;
        let sz = |w: &[Word]| size_of(w, p) as usize;
        assert_eq!(sz(HOT_DESC), core::mem::size_of::<HotDesc>());
        assert_eq!(sz(GC_KEY), core::mem::size_of::<GcKey>());
        assert_eq!(sz(SYM_SIG), core::mem::size_of::<SymSig>());
        assert_eq!(sz(FRAME_INFO), core::mem::size_of::<FrameInfo>());
        assert_eq!(sz(HELD), core::mem::size_of::<Held>());
        assert_eq!(&HOT_MAGIC.to_le_bytes(), b"NABH");
        assert_eq!(&FRAME_MAGIC.to_le_bytes(), b"NABF");
    }
}
