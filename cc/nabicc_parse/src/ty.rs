//! C types and the target's record layout (System V / Itanium rules,
//! or Microsoft's on MSVC targets).

use nabic_core::HashMap;
use nabicc_lex::Loc;
use nabicc_target::{Prim, Target, VaList};
use std::cell::RefCell;

pub type TypeId = u32;
pub type RecordId = u32;
pub type EnumId = u32;
pub type TypedefId = u32;

/// A type with its `const`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct QualType {
    pub ty: TypeId,
    pub konst: bool,
}

impl QualType {
    pub const fn new(ty: TypeId) -> Self {
        Self { ty, konst: false }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CallConv {
    C,
    Stdcall,
    Fastcall,
    Vectorcall,
    Thiscall,
    Regcall,
    Win64,
    SysV,
    /// Another target-specific one, by its attribute.
    Other(&'static str),
}

impl CallConv {
    pub const fn name(self) -> &'static str {
        match self {
            Self::C => "cdecl",
            Self::Stdcall => "stdcall",
            Self::Fastcall => "fastcall",
            Self::Vectorcall => "vectorcall",
            Self::Thiscall => "thiscall",
            Self::Regcall => "regcall",
            Self::Win64 => "ms_abi",
            Self::SysV => "sysv_abi",
            Self::Other(n) => n,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Func {
    pub ret: QualType,
    pub params: Vec<QualType>,
    pub variadic: bool,
    /// Declared with a prototype (not `f()`).
    pub proto: bool,
    pub cc: CallConv,
}

#[derive(Clone, Debug)]
pub enum Type {
    Prim(Prim),
    Complex(TypeId),
    Pointer(QualType),
    /// A block pointer (`^`, Apple).
    Block(QualType),
    Array {
        elem: QualType,
        /// `None`: `T[]`.
        len: Option<u64>,
    },
    Func(Func),
    Record(RecordId),
    Enum(EnumId),
    Typedef(TypedefId),
    Vector {
        elem: TypeId,
        len: u64,
        /// The attribute that made it, as clang spells such types.
        attr: &'static str,
    },
    Atomic(QualType),
    /// `typeof(..)`: sugar clang does not expose.
    TypeOf(QualType),
    /// A builtin type without a size (`AArch64`'s SVE vectors).
    Sizeless(&'static str),
    /// Microsoft's `__ptr32` / `__ptr64` pointer of this many bytes.
    SizedPointer(QualType, u64),
}

#[derive(Clone, Debug)]
pub struct Field {
    pub name: Option<String>,
    pub ty: QualType,
    pub bits: Option<u64>,
    pub packed: bool,
    /// `aligned` on the field, bytes (0: none).
    pub align: u64,
    pub loc: Loc,
    /// A C11 anonymous struct or union member.
    pub anon: bool,
}

#[derive(Clone, Debug)]
pub struct RecordDef {
    pub fields: Vec<Field>,
    pub packed: bool,
    /// `aligned` on the record, bytes (0: none).
    pub align: u64,
    /// `#pragma pack` where it was defined, bytes (0: none).
    pub pack: u64,
}

#[derive(Clone, Debug)]
pub struct Record {
    pub name: Option<String>,
    pub union: bool,
    pub def: Option<RecordDef>,
    /// The typedef naming an unnamed record for linkage.
    pub typedef_name: Option<String>,
    /// Defined in the target's implicit declarations (no file).
    pub builtin: bool,
}

#[derive(Clone, Debug)]
pub struct Enumerator {
    pub name: String,
    pub value: i128,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct Enum {
    pub name: Option<String>,
    pub typedef_name: Option<String>,
    /// The integer type, once defined or fixed.
    pub int: Option<TypeId>,
    pub fixed: bool,
    pub values: Vec<Enumerator>,
    pub complete: bool,
}

#[derive(Clone, Debug)]
pub struct Typedef {
    pub name: String,
    pub ty: QualType,
    /// `aligned` on the typedef, bytes (0: none): it can lower alignment.
    pub align: u64,
    pub loc: Loc,
}

/// A record's layout.
#[derive(Clone, Debug)]
pub struct Layout {
    pub size: u64,
    pub align: u64,
    /// Field offsets in bits, by field.
    pub offsets: Vec<u64>,
    /// MSVC: alignment required by `__declspec(align)` within.
    pub required: u64,
}

pub struct Types {
    pub t: Target,
    pub types: Vec<Type>,
    pub records: Vec<Record>,
    pub enums: Vec<Enum>,
    pub typedefs: Vec<Typedef>,
    layouts: RefCell<HashMap<RecordId, Option<Layout>>>,
    prims: HashMap<Prim, TypeId>,
    /// The implicit `__builtin_va_list` and its kin.
    pub va_list: TypedefId,
    pub ms_va_list: Option<TypedefId>,
    pub int128_t: Option<TypedefId>,
    pub uint128_t: Option<TypedefId>,
    /// `AArch64`'s SVE types, by name.
    pub sizeless: Vec<(&'static str, TypeId)>,
}

const PRIMS: [Prim; 23] = [
    Prim::Void,
    Prim::Bool,
    Prim::Char,
    Prim::SChar,
    Prim::UChar,
    Prim::Short,
    Prim::UShort,
    Prim::Int,
    Prim::UInt,
    Prim::Long,
    Prim::ULong,
    Prim::LongLong,
    Prim::ULongLong,
    Prim::Int128,
    Prim::UInt128,
    Prim::Float16,
    Prim::Fp16,
    Prim::BF16,
    Prim::Float,
    Prim::Double,
    Prim::LongDouble,
    Prim::Float128,
    Prim::Ibm128,
];

/// `AArch64`'s SVE ACLE builtin types.
const SVE_TYPES: [&str; 15] = [
    "__SVInt8_t",
    "__SVInt16_t",
    "__SVInt32_t",
    "__SVInt64_t",
    "__SVUint8_t",
    "__SVUint16_t",
    "__SVUint32_t",
    "__SVUint64_t",
    "__SVFloat16_t",
    "__SVFloat32_t",
    "__SVFloat64_t",
    "__SVBfloat16_t",
    "__SVMfloat8_t",
    "__SVBool_t",
    "__SVCount_t",
];

const fn round_up(x: u64, a: u64) -> u64 {
    if a == 0 { x } else { x.div_ceil(a) * a }
}

impl Types {
    pub fn new(t: Target) -> Self {
        let mut ty = Self {
            t,
            types: Vec::new(),
            records: Vec::new(),
            enums: Vec::new(),
            typedefs: Vec::new(),
            layouts: RefCell::new(HashMap::default()),
            prims: HashMap::default(),
            va_list: 0,
            ms_va_list: None,
            int128_t: None,
            uint128_t: None,
            sizeless: Vec::new(),
        };
        for p in PRIMS {
            let id = ty.add(Type::Prim(p));
            ty.prims.insert(p, id);
        }
        ty.builtins();
        ty
    }

    pub fn add(&mut self, t: Type) -> TypeId {
        self.types.push(t);
        (self.types.len() - 1) as TypeId
    }

    pub fn prim(&self, p: Prim) -> TypeId {
        self.prims[&p]
    }

    pub fn pointer(&mut self, to: QualType) -> TypeId {
        self.add(Type::Pointer(to))
    }

    fn builtin_typedef(&mut self, name: &str, ty: TypeId) -> TypedefId {
        self.typedefs.push(Typedef {
            name: name.to_owned(),
            ty: QualType::new(ty),
            align: 0,
            loc: Loc::default(),
        });
        (self.typedefs.len() - 1) as TypedefId
    }

    fn builtin_record(&mut self, name: &str, fields: &[(&str, TypeId)]) -> TypeId {
        let fields = fields
            .iter()
            .map(|(n, t)| Field {
                name: Some((*n).to_owned()),
                ty: QualType::new(*t),
                bits: None,
                packed: false,
                align: 0,
                loc: Loc::default(),
                anon: false,
            })
            .collect();
        self.records.push(Record {
            name: Some(name.to_owned()),
            union: false,
            def: Some(RecordDef {
                fields,
                packed: false,
                align: 0,
                pack: 0,
            }),
            typedef_name: None,
            builtin: true,
        });
        let r = (self.records.len() - 1) as RecordId;
        self.add(Type::Record(r))
    }

    /// clang's implicit declarations: `__builtin_va_list` and the like.
    fn builtins(&mut self) {
        let void_p = self.pointer(QualType::new(self.prim(Prim::Void)));
        let char_p = self.pointer(QualType::new(self.prim(Prim::Char)));
        let (uint, int, long) = (
            self.prim(Prim::UInt),
            self.prim(Prim::Int),
            self.prim(Prim::Long),
        );
        let va = match self.t.va_list {
            VaList::CharPtr => char_p,
            VaList::VoidPtr => void_p,
            VaList::X86_64 => {
                let r = self.builtin_record(
                    "__va_list_tag",
                    &[
                        ("gp_offset", uint),
                        ("fp_offset", uint),
                        ("overflow_arg_area", void_p),
                        ("reg_save_area", void_p),
                    ],
                );
                self.add(Type::Array {
                    elem: QualType::new(r),
                    len: Some(1),
                })
            }
            VaList::S390x => {
                let r = self.builtin_record(
                    "__va_list_tag",
                    &[
                        ("__gpr", long),
                        ("__fpr", long),
                        ("__overflow_arg_area", void_p),
                        ("__reg_save_area", void_p),
                    ],
                );
                self.add(Type::Array {
                    elem: QualType::new(r),
                    len: Some(1),
                })
            }
            VaList::Aarch64 => self.builtin_record(
                "__va_list",
                &[
                    ("__stack", void_p),
                    ("__gr_top", void_p),
                    ("__vr_top", void_p),
                    ("__gr_offs", int),
                    ("__vr_offs", int),
                ],
            ),
            VaList::Arm => self.builtin_record("__va_list", &[("__ap", void_p)]),
        };
        self.va_list = self.builtin_typedef("__builtin_va_list", va);
        if self.t.arch == nabicc_target::Arch::X86_64 {
            self.ms_va_list = Some(self.builtin_typedef("__builtin_ms_va_list", char_p));
        }
        if self.t.arch == nabicc_target::Arch::Aarch64 {
            for n in SVE_TYPES {
                let t = self.add(Type::Sizeless(n));
                self.sizeless.push((n, t));
            }
        }
        if self.t.int128 {
            let (i, u) = (self.prim(Prim::Int128), self.prim(Prim::UInt128));
            self.int128_t = Some(self.builtin_typedef("__int128_t", i));
            self.uint128_t = Some(self.builtin_typedef("__uint128_t", u));
        }
    }

    /// Through typedefs and `typeof`.
    pub fn canon(&self, mut t: TypeId) -> TypeId {
        loop {
            match &self.types[t as usize] {
                Type::Typedef(d) => t = self.typedefs[*d as usize].ty.ty,
                Type::TypeOf(q) => t = q.ty,
                _ => return t,
            }
        }
    }

    /// Through typedefs and `typeof`, keeping any `const` met.
    pub fn canon_q(&self, mut q: QualType) -> QualType {
        loop {
            match &self.types[q.ty as usize] {
                Type::Typedef(d) => {
                    let u = self.typedefs[*d as usize].ty;
                    q = QualType {
                        ty: u.ty,
                        konst: q.konst || u.konst,
                    };
                }
                Type::TypeOf(u) => {
                    q = QualType {
                        ty: u.ty,
                        konst: q.konst || u.konst,
                    };
                }
                _ => return q,
            }
        }
    }

    pub fn get(&self, t: TypeId) -> &Type {
        &self.types[t as usize]
    }

    /// The builtin a type names, through typedefs (enums: their integer).
    pub fn prim_of(&self, t: TypeId) -> Option<Prim> {
        match self.get(self.canon(t)) {
            Type::Prim(p) => Some(*p),
            Type::Enum(e) => self.enums[*e as usize].int.and_then(|i| self.prim_of(i)),
            _ => None,
        }
    }

    pub fn is_void(&self, t: TypeId) -> bool {
        matches!(self.get(self.canon(t)), Type::Prim(Prim::Void))
    }

    /// Size and alignment in bytes; `None` for incomplete and function
    /// types.
    pub fn size_align(&self, t: TypeId) -> Option<(u64, u64)> {
        match self.get(t) {
            Type::Prim(Prim::Void) | Type::Func(_) | Type::Sizeless(_) => None,
            Type::Prim(p) => Some(self.t.prim(*p)),
            Type::Complex(e) => self.size_align(*e).map(|(s, a)| (2 * s, a)),
            Type::Pointer(_) | Type::Block(_) => Some((self.t.ptr, self.t.ptr)),
            Type::SizedPointer(_, n) => Some((*n, *n)),
            Type::Array { elem, len } => {
                let (s, a) = self.size_align(elem.ty)?;
                Some((s * (*len)?, a))
            }
            Type::Record(r) => self.layout(*r).map(|l| (l.size, l.align)),
            Type::Enum(e) => self.size_align(self.enums[*e as usize].int?),
            Type::Typedef(d) => {
                let td = &self.typedefs[*d as usize];
                let (s, a) = self.size_align(td.ty.ty)?;
                Some((s, if td.align > 0 { td.align } else { a }))
            }
            Type::Vector { elem, len, .. } => {
                let (s, _) = self.size_align(*elem)?;
                let max = if self.t.arch == nabicc_target::Arch::Arm {
                    8
                } else {
                    16
                };
                let a = (s * len).next_power_of_two().min(max);
                Some((round_up(s * len, a), a))
            }
            Type::Atomic(q) => {
                let (s, a) = self.size_align(q.ty)?;
                if s <= 16 && s.is_power_of_two() {
                    Some((s, s.max(a)))
                } else if s <= 16 {
                    let p = s.next_power_of_two();
                    Some((p, p))
                } else {
                    Some((s, a))
                }
            }
            Type::TypeOf(q) => self.size_align(q.ty),
        }
    }

    /// MSVC: an alignment the type demands beyond the natural one
    /// (`aligned` on a typedef or record).
    fn align_required(&self, t: TypeId) -> bool {
        match self.get(t) {
            Type::Typedef(d) => {
                let td = &self.typedefs[*d as usize];
                td.align > 0 || self.align_required(td.ty.ty)
            }
            Type::TypeOf(q) | Type::Array { elem: q, .. } => self.align_required(q.ty),
            Type::Record(r) => self.records[*r as usize]
                .def
                .as_ref()
                .is_some_and(|d| d.align > 0),
            _ => false,
        }
    }

    pub fn layout(&self, r: RecordId) -> Option<Layout> {
        if let Some(l) = self.layouts.borrow().get(&r) {
            return l.clone();
        }
        // A record containing itself is invalid C; this keeps it finite.
        self.layouts.borrow_mut().insert(r, None);
        let rec = &self.records[r as usize];
        let l = rec.def.as_ref().and_then(|d| {
            if self.t.msvc {
                self.layout_ms(d, rec.union)
            } else {
                self.layout_sysv(d, rec.union)
            }
        });
        self.layouts.borrow_mut().insert(r, l.clone());
        l
    }

    /// Forget layouts (a record completed after one was asked for).
    pub fn reset_layout(&self, r: RecordId) {
        self.layouts.borrow_mut().remove(&r);
    }

    /// Field size and alignment: flexible arrays are empty.
    fn field_info(&self, f: &Field) -> Option<(u64, u64)> {
        match self.get(self.canon(f.ty.ty)) {
            Type::Array { elem, len: None } => {
                let a = self.size_align(elem.ty)?.1;
                // The typedef'd element's own alignment counts.
                Some((0, a))
            }
            _ => self.size_align(f.ty.ty),
        }
    }

    fn layout_sysv(&self, d: &RecordDef, union: bool) -> Option<Layout> {
        let pack = d.pack;
        let mut align = d.align.max(1);
        let mut data_bits = 0u64;
        let mut size_bits = 0u64;
        let mut unfilled = 0u64;
        let mut offsets = Vec::with_capacity(d.fields.len());
        for f in &d.fields {
            let (fsize, falign) = self.field_info(f)?;
            let packed = d.packed || f.packed;
            let Some(width) = f.bits else {
                unfilled = 0;
                let mut packed_align = 1u64.max(f.align);
                let mut unpacked = falign.max(f.align);
                if pack > 0 {
                    packed_align = packed_align.min(pack);
                    unpacked = unpacked.min(pack);
                }
                let fa = if packed { packed_align } else { unpacked };
                let off = if union {
                    0
                } else {
                    round_up(data_bits / 8, fa)
                };
                offsets.push(off * 8);
                data_bits = if union {
                    data_bits.max(fsize * 8)
                } else {
                    (off + fsize) * 8
                };
                size_bits = size_bits.max(data_bits);
                align = align.max(fa);
                continue;
            };
            let unit = fsize * 8;
            let mut fa = falign * 8;
            let mut unpacked = fa;
            let mut off = if union { 0 } else { data_bits - unfilled };
            if packed && width != 0 {
                fa = 8;
            }
            let explicit = f.align * 8;
            if explicit > 0 {
                fa = fa.max(explicit);
                unpacked = unpacked.max(explicit);
            }
            if pack > 0 && width != 0 {
                unpacked = unpacked.min(pack * 8);
                fa = if packed { unpacked } else { fa.min(pack * 8) };
            }
            if width == 0 || (pack == 0 && (off % fa) + width > unit) {
                off = round_up(off, fa);
            } else if explicit > 0 && (pack == 0 || explicit <= pack * 8) {
                off = round_up(off, explicit);
            }
            offsets.push(off);
            let counts = f.name.is_some() || self.t.unnamed_bitfield_align;
            if union {
                data_bits = data_bits.max(round_up(width, 8));
            } else {
                let end = off + width;
                data_bits = round_up(end, 8);
                unfilled = data_bits - end;
            }
            size_bits = size_bits.max(data_bits);
            if counts {
                align = align.max(fa / 8);
            }
        }
        let size = round_up(size_bits.div_ceil(8), align);
        Some(Layout {
            size,
            align,
            offsets,
            required: 0,
        })
    }

    fn layout_ms(&self, d: &RecordDef, union: bool) -> Option<Layout> {
        let ptr = self.t.ptr;
        let max_field = if d.packed {
            1
        } else if d.pack > 0 && d.pack <= ptr {
            d.pack
        } else {
            0
        };
        let mut size = 0u64;
        let mut align = 1u64;
        let mut required = u64::from(ptr == 8);
        let mut offsets = Vec::with_capacity(d.fields.len());
        let mut last_bitfield = false;
        let mut cur_bitfield_size = 0u64;
        let mut remaining = 0u64;
        for f in &d.fields {
            let (mut fsize, _) = self.field_info(f)?;
            // The natural alignment: alignment attributes on a typedef
            // count only as required alignment below.
            let mut falign = match self.get(self.canon(f.ty.ty)) {
                Type::Array { elem, len: None } => {
                    let (s, a) = self.size_align(elem.ty)?;
                    // MSVC sizes a union's flexible array as one element.
                    if union {
                        fsize = s;
                    }
                    a
                }
                _ => self.size_align(self.canon(f.ty.ty))?.1,
            };
            let mut req = f.align;
            if self.align_required(f.ty.ty) {
                req = req.max(self.size_align(f.ty.ty).map_or(1, |x| x.1));
            }
            if f.bits.is_some() {
                falign = falign.max(req);
            } else {
                let mut base = self.canon(f.ty.ty);
                while let Type::Array { elem, .. } = self.get(base) {
                    base = self.canon(elem.ty);
                }
                if let Type::Record(r) = self.get(base) {
                    req = req.max(self.layout(*r).map_or(0, |l| l.required));
                }
                required = required.max(req);
            }
            if max_field > 0 {
                falign = falign.min(max_field);
            }
            if f.packed {
                falign = 1;
            }
            falign = falign.max(req);
            match f.bits {
                None => {
                    last_bitfield = false;
                    align = align.max(falign);
                    let off = if union { 0 } else { round_up(size, falign) };
                    offsets.push(off * 8);
                    size = size.max(off + fsize);
                }
                Some(0) => {
                    if !last_bitfield {
                        offsets.push(if union { 0 } else { size * 8 });
                        continue;
                    }
                    last_bitfield = false;
                    if union {
                        offsets.push(0);
                        size = size.max(fsize);
                    } else {
                        let off = round_up(size, falign);
                        offsets.push(off * 8);
                        size = off;
                        align = align.max(falign);
                    }
                }
                Some(w) => {
                    let w = w.min(fsize * 8);
                    if !union && last_bitfield && cur_bitfield_size == fsize && w <= remaining {
                        offsets.push(size * 8 - remaining);
                        remaining -= w;
                        continue;
                    }
                    last_bitfield = true;
                    cur_bitfield_size = fsize;
                    if union {
                        offsets.push(0);
                        size = size.max(fsize);
                    } else {
                        let off = round_up(size, falign);
                        offsets.push(off * 8);
                        size = off + fsize;
                        align = align.max(falign);
                        remaining = fsize * 8 - w;
                    }
                }
            }
        }
        size = round_up(size, align);
        required = required.max(d.align);
        if required > 0 {
            align = align.max(required);
            let mut rounding = align;
            if max_field > 0 {
                rounding = rounding.min(max_field);
            }
            rounding = rounding.max(required);
            size = round_up(size, rounding);
        }
        if size == 0 {
            size = if required >= 4 { align } else { 4 };
        }
        Some(Layout {
            size,
            align,
            offsets,
            required,
        })
    }

    /// A type as clang spells it, for messages.
    pub fn spell(&self, q: QualType) -> String {
        self.spell_with(q, "")
    }

    fn spell_with(&self, q: QualType, inner: &str) -> String {
        let c = if q.konst { "const " } else { "" };
        let join = |base: String, inner: &str| {
            if inner.is_empty() {
                base
            } else {
                format!("{base} {inner}")
            }
        };
        match self.get(q.ty) {
            Type::Prim(p) => join(format!("{c}{}", p.name()), inner),
            Type::Sizeless(n) => join(format!("{c}{n}"), inner),
            Type::SizedPointer(p, n) => {
                let q = if *n == 4 { "__ptr32" } else { "__ptr64" };
                self.spell_with(*p, &format!("* {q}{inner}"))
            }
            Type::Complex(e) => join(
                format!("{c}_Complex {}", self.spell(QualType::new(*e))),
                inner,
            ),
            Type::Record(r) => {
                let rec = &self.records[*r as usize];
                let kw = if rec.union { "union" } else { "struct" };
                let n = rec.name.clone().or_else(|| rec.typedef_name.clone());
                join(
                    format!("{c}{kw} {}", n.as_deref().unwrap_or("(unnamed)")),
                    inner,
                )
            }
            Type::Enum(e) => {
                let en = &self.enums[*e as usize];
                let n = en.name.clone().or_else(|| en.typedef_name.clone());
                join(
                    format!("{c}enum {}", n.as_deref().unwrap_or("(unnamed)")),
                    inner,
                )
            }
            Type::Typedef(d) => join(format!("{c}{}", self.typedefs[*d as usize].name), inner),
            Type::Vector { elem, len, attr } => {
                let e = self.spell(QualType::new(*elem));
                let s = match *attr {
                    "vector_size" => {
                        format!("{c}__attribute__((__vector_size__({len} * sizeof({e})))) {e}")
                    }
                    "ext_vector_type" => {
                        format!("{c}{e} __attribute__((ext_vector_type({len})))")
                    }
                    a => format!("{c}__attribute__(({a}({len}))) {e}"),
                };
                join(s, inner)
            }
            Type::Atomic(t) => join(format!("{c}_Atomic({})", self.spell(*t)), inner),
            Type::TypeOf(t) => self.spell_with(
                QualType {
                    ty: t.ty,
                    konst: q.konst || t.konst,
                },
                inner,
            ),
            Type::Pointer(p) | Type::Block(p) => {
                let star = if matches!(self.get(q.ty), Type::Block(_)) {
                    "^"
                } else {
                    "*"
                };
                let me = format!("{star}{}{inner}", if q.konst { "const " } else { "" });
                let me = me.trim_end().to_owned();
                match self.get(p.ty) {
                    Type::Func(_) | Type::Array { .. } => self.spell_with(*p, &format!("({me})")),
                    _ => self.spell_with(*p, &me),
                }
            }
            Type::Array { elem, len } => {
                let n = len.map(|n| n.to_string()).unwrap_or_default();
                self.spell_with(*elem, &format!("{inner}[{n}]"))
            }
            Type::Func(f) => {
                let mut ps: Vec<String> = f.params.iter().map(|p| self.spell(*p)).collect();
                if f.variadic {
                    ps.push("...".to_owned());
                }
                if ps.is_empty() && f.proto {
                    ps.push("void".to_owned());
                }
                let ret = self.spell(f.ret);
                format!("{ret} {inner}({})", ps.join(", ")).replace("  ", " ")
            }
        }
    }
}
