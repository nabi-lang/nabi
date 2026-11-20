//! `-g` on COFF: `CodeView` (C13).
//!
//! `.debug$S`: the object's name and compiler, then the source files'
//! checksum entries and names; per function (a section associated with its
//! code, so both go together) `S_GPROC32` / `S_LPROC32`, `S_FRAMEPROC`, the
//! parameters and named locals as `S_LOCAL` in their frame-pointer relative
//! slots (`S_DEFRANGE_FRAMEPOINTER_REL_FULL_SCOPE`) within `S_BLOCK32`
//! scopes, and a line table. `.debug$T`: the types they name, from the
//! facts the DWARF writer reads (structs as `LF_STRUCTURE`, sums as a struct
//! of the tag and each variant at offset 0, `LF_PROCEDURE` signatures).
//!
//! Optimized code (`FnDebug::opt`): a variable's places by code range
//! (`S_DEFRANGE_REGISTER`, `_FRAMEPOINTER_REL`, and per piece of a
//! struct split across registers and memory `_SUBFIELD_REGISTER` and
//! `_REGISTER_REL` off the frame pointer; none and `optimized out` where
//! its value is gone), a variable of one constant value `S_CONSTANT` (a
//! float one an `S_LDATA32` of the value in a `.rdata` section that goes
//! with the code), the bodies inlined as `S_INLINESITE` of an
//! `LF_FUNC_ID` with their lines as binary annotations
//! (`DEBUG_S_INLINEELINES` giving each callee's file and line), and the
//! function's line table naming the call's line for inlined code.

use crate::debug::locs::{Place, Spans};
use crate::lir::FnDebug;
use crate::obj::{FnRange, Obj};
use nabic_core::HashMap;
use nabic_core::hash::fnv;
use nabic_ir_common::PrimTy;
use nabic_mir::gc_abi::HEADER_SIZE;
use nabic_mir::{DebugTy, MirModule, MirTy, Target};
use std::path::PathBuf;

const SIGNATURE: u32 = 4;

const DEBUG_S_SYMBOLS: u32 = 0xf1;
const DEBUG_S_LINES: u32 = 0xf2;
const DEBUG_S_STRINGTABLE: u32 = 0xf3;
const DEBUG_S_FILECHKSMS: u32 = 0xf4;

const S_END: u16 = 0x0006;
const S_FRAMEPROC: u16 = 0x1012;
const S_OBJNAME: u16 = 0x1101;
const S_BLOCK32: u16 = 0x1103;
const S_CONSTANT: u16 = 0x1107;
const S_LDATA32: u16 = 0x110c;
const S_LPROC32: u16 = 0x110f;
const S_GPROC32: u16 = 0x1110;
const S_COMPILE3: u16 = 0x113c;
const S_LOCAL: u16 = 0x113e;
const S_DEFRANGE_FRAMEPOINTER_REL_FULL_SCOPE: u16 = 0x1144;
const S_DEFRANGE_REGISTER: u16 = 0x1141;
const S_DEFRANGE_FRAMEPOINTER_REL: u16 = 0x1142;
const S_DEFRANGE_SUBFIELD_REGISTER: u16 = 0x1143;
const S_DEFRANGE_REGISTER_REL: u16 = 0x1145;
/// `S_DEFRANGE_REGISTER_REL` flags: a member of the variable, at this
/// shift its offset.
const RANGE_SUBFIELD: u16 = 1;
const RANGE_PARENT_SHIFT: u16 = 4;
const S_INLINESITE: u16 = 0x114d;
const S_INLINESITE_END: u16 = 0x114e;
const S_INLINEES: u16 = 0x1168;
const DEBUG_S_INLINEELINES: u32 = 0xf6;
const LF_FUNC_ID: u16 = 0x1601;
/// `S_LOCAL`: no value anywhere.
const LOCAL_OPTIMIZED_OUT: u16 = 0x100;
/// Binary annotations of an inline site.
const BA_CHANGE_CODE_OFFSET: u32 = 3;
const BA_CHANGE_CODE_LENGTH: u32 = 4;
const BA_CHANGE_LINE_OFFSET: u32 = 6;
const BA_CHANGE_CODE_OFFSET_AND_LINE_OFFSET: u32 = 11;

const LF_POINTER: u16 = 0x1002;
const LF_PROCEDURE: u16 = 0x1008;
const LF_ARGLIST: u16 = 0x1201;
const LF_FIELDLIST: u16 = 0x1203;
const LF_INDEX: u16 = 0x1404;
const LF_ARRAY: u16 = 0x1503;
const LF_STRUCTURE: u16 = 0x1505;
const LF_MEMBER: u16 = 0x150d;
const LF_CHAR: u16 = 0x8000;
const LF_SHORT: u16 = 0x8001;
const LF_USHORT: u16 = 0x8002;
const LF_LONG: u16 = 0x8003;
const LF_ULONG: u16 = 0x8004;
const LF_QUADWORD: u16 = 0x8009;
const LF_UQUADWORD: u16 = 0x800a;

const T_VOID: u32 = 0x0003;
const T_CHAR: u32 = 0x0010;
const T_SHORT: u32 = 0x0011;
const T_QUAD: u32 = 0x0013;
const T_UCHAR: u32 = 0x0020;
const T_USHORT: u32 = 0x0021;
const T_ULONG: u32 = 0x0022;
const T_UQUAD: u32 = 0x0023;
const T_BOOL08: u32 = 0x0030;
const T_REAL32: u32 = 0x0040;
const T_REAL64: u32 = 0x0041;
const T_RCHAR: u32 = 0x0070;
const T_INT4: u32 = 0x0074;
const T_UINT4: u32 = 0x0075;
const T_CHAR32: u32 = 0x007b;
/// Pointer modes of simple types: 32-bit and 64-bit near.
const T_PTR32: u32 = 0x0400;
const T_PTR64: u32 = 0x0600;

/// `LF_STRUCTURE` properties.
const PROP_FWDREF: u16 = 0x80;
const PROP_UNIQUE_NAME: u16 = 0x200;
/// Member access: public.
const ACCESS_PUBLIC: u16 = 3;
/// `S_LOCAL`: a parameter.
const LOCAL_PARAM: u16 = 1;
/// `S_GPROC32`: a frame pointer is set up (no FPO).
const PROC_NOFPO: u8 = 1;
/// `S_GPROC32`: debug info of optimized code (inline sites, ranges).
const PROC_OPTDBGINFO: u8 = 0x80;
/// `S_FRAMEPROC`: locals and parameters are frame-pointer relative
/// (`rbp` / `ebp` / `x29`, encoded 2).
const FRAMEPROC_FP: u32 = (2 << 14) | (2 << 16);
/// `S_COMPILE3` language: Rust (the DWARF unit's too).
const CV_CFL_RUST: u32 = 0x15;
/// Field lists split before this many bytes (a record's length is 16 bits).
const FIELDLIST_MAX: usize = 0xf000;

/// What a `.debug$S` relocation writes: a symbol's offset in its
/// section (32 bits) or its section number (16 bits).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CvRel {
    SecRel,
    Section,
    /// Into the function's constants (`CvSect::consts`).
    ConstSecRel,
    ConstSection,
}

/// A `.debug$S` section's bytes and relocations against function `fn`'s
/// code (`obj.fns` index) or its constants, the place holding the addend.
/// `consts`: the values of its float constant variables, which dbghelp
/// reads only from memory (`S_LDATA32`), for a `.rdata` section that goes
/// with the code.
pub struct CvSect {
    pub data: Vec<u8>,
    pub relocs: Vec<(u32, CvRel, usize)>,
    pub consts: Vec<u8>,
}

/// An object's `CodeView`: the main `.debug$S`, one per function (index
/// in `obj.fns`), the `.debug$T`.
pub struct Cv {
    pub main: Vec<u8>,
    pub fns: Vec<(usize, CvSect)>,
    pub types: Vec<u8>,
}

fn put16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn cstr(b: &mut Vec<u8>, s: &str) {
    b.extend_from_slice(s.as_bytes());
    b.push(0);
}

/// A numeric leaf.
fn numeric(b: &mut Vec<u8>, v: u32) {
    if v < 0x8000 {
        put16(b, v as u16);
    } else {
        put16(b, LF_ULONG);
        put32(b, v);
    }
}

/// The numeric leaf of constant `bits` of integer type `ty`.
fn constant(b: &mut Vec<u8>, ty: u32, bits: u64) {
    let (size, signed) = match ty {
        T_CHAR | T_RCHAR => (1, true),
        T_SHORT => (2, true),
        T_INT4 => (4, true),
        T_QUAD => (8, true),
        T_UCHAR | T_BOOL08 => (1, false),
        T_USHORT => (2, false),
        T_ULONG | T_UINT4 | T_CHAR32 => (4, false),
        _ => (8, false),
    };
    let sh = 64 - 8 * size;
    if signed {
        let v = ((bits << sh) as i64) >> sh;
        if (0..0x8000).contains(&v) {
            put16(b, v as u16);
        } else if let Ok(x) = i8::try_from(v) {
            put16(b, LF_CHAR);
            b.push(x as u8);
        } else if let Ok(x) = i16::try_from(v) {
            put16(b, LF_SHORT);
            put16(b, x as u16);
        } else if let Ok(x) = i32::try_from(v) {
            put16(b, LF_LONG);
            put32(b, x as u32);
        } else {
            put16(b, LF_QUADWORD);
            b.extend_from_slice(&v.to_le_bytes());
        }
        return;
    }
    let v = (bits << sh) >> sh;
    if v < 0x8000 {
        put16(b, v as u16);
    } else if let Ok(x) = u16::try_from(v) {
        put16(b, LF_USHORT);
        put16(b, x);
    } else if let Ok(x) = u32::try_from(v) {
        put16(b, LF_ULONG);
        put32(b, x);
    } else {
        put16(b, LF_UQUADWORD);
        b.extend_from_slice(&v.to_le_bytes());
    }
}

/// `LF_PAD<n>` bytes up to a multiple of 4 of `b`'s length plus `extra`.
fn pad(b: &mut Vec<u8>, extra: usize) {
    while (b.len() + extra) % 4 != 0 {
        let left = 4 - (b.len() + extra) % 4;
        b.push(0xf0 | left as u8);
    }
}

/// A symbol record: length, kind, `body`.
fn sym(out: &mut Vec<u8>, kind: u16, body: &[u8]) {
    put16(out, (body.len() + 2) as u16);
    put16(out, kind);
    out.extend_from_slice(body);
}

/// A subsection of `kind`, padded to 4 bytes.
fn subsection(out: &mut Vec<u8>, kind: u32, data: &[u8]) {
    put32(out, kind);
    put32(out, data.len() as u32);
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// The `.debug$T` being written: records by contents and by key.
struct Types<'a> {
    recs: Vec<u8>,
    count: u32,
    by_bytes: HashMap<Vec<u8>, u32>,
    by_key: HashMap<String, u32>,
    /// Structs being described: their forward references.
    building: HashMap<String, u32>,
    /// The sizes of structs and arrays.
    sizes: HashMap<u32, u32>,
    t: &'a Target,
    m: &'a MirModule,
}

/// A struct member: name, type, offset.
type Member = (String, u32, u32);

impl Types<'_> {
    /// The index of record `kind` with `body` (padded), one per contents.
    fn add(&mut self, kind: u16, mut body: Vec<u8>) -> u32 {
        pad(&mut body, 4);
        let mut rec = Vec::with_capacity(body.len() + 4);
        put16(&mut rec, (body.len() + 2) as u16);
        put16(&mut rec, kind);
        rec.extend_from_slice(&body);
        if let Some(&i) = self.by_bytes.get(&rec) {
            return i;
        }
        let i = 0x1000 + self.count;
        self.count += 1;
        self.recs.extend_from_slice(&rec);
        self.by_bytes.insert(rec, i);
        i
    }

    fn ptr64(&self) -> bool {
        self.t.pointer_width == 64
    }

    fn pointer_to(&mut self, ty: u32) -> u32 {
        if ty < 0x1000 && ty & 0xf00 == 0 {
            return ty | if self.ptr64() { T_PTR64 } else { T_PTR32 };
        }
        let (kind, size) = if self.ptr64() { (0x0c, 8) } else { (0x0a, 4) };
        let mut b = Vec::new();
        put32(&mut b, ty);
        put32(&mut b, kind | (size << 13));
        self.add(LF_POINTER, b)
    }

    fn base(&self, p: PrimTy) -> Option<u32> {
        let w = self.ptr64();
        Some(match p {
            PrimTy::I8 => T_CHAR,
            PrimTy::I16 => T_SHORT,
            PrimTy::I64 => T_QUAD,
            PrimTy::Isize if w => T_QUAD,
            PrimTy::I32 | PrimTy::Isize => T_INT4,
            PrimTy::U8 => T_UCHAR,
            PrimTy::U16 => T_USHORT,
            PrimTy::U64 => T_UQUAD,
            PrimTy::Usize if w => T_UQUAD,
            PrimTy::U32 | PrimTy::Usize => T_UINT4,
            PrimTy::F32 => T_REAL32,
            PrimTy::F64 => T_REAL64,
            PrimTy::Bool => T_BOOL08,
            PrimTy::CChar => T_RCHAR,
            PrimTy::Char => T_CHAR32,
            PrimTy::Str => return None,
        })
    }

    /// The forward reference of struct `name` (unique by `key`).
    fn fwd(&mut self, name: &str, key: &str) -> u32 {
        let mut b = Vec::new();
        put16(&mut b, 0);
        put16(&mut b, PROP_FWDREF | PROP_UNIQUE_NAME);
        put32(&mut b, 0);
        put32(&mut b, 0);
        put32(&mut b, 0);
        numeric(&mut b, 0);
        cstr(&mut b, name);
        cstr(&mut b, &format!("{name}#{:016x}", fnv(key.as_bytes())));
        self.add(LF_STRUCTURE, b)
    }

    /// Field lists of `members`, continued through `LF_INDEX` past the
    /// record size limit; the first one's index.
    fn fieldlist(&mut self, members: &[Member]) -> u32 {
        let mut chunks: Vec<Vec<u8>> = vec![Vec::new()];
        for (n, ty, at) in members {
            let mut m = Vec::new();
            put16(&mut m, LF_MEMBER);
            put16(&mut m, ACCESS_PUBLIC);
            put32(&mut m, *ty);
            numeric(&mut m, *at);
            cstr(&mut m, n);
            pad(&mut m, 0);
            if chunks
                .last()
                .is_some_and(|c| c.len() + m.len() > FIELDLIST_MAX)
            {
                chunks.push(Vec::new());
            }
            chunks.last_mut().expect("a chunk").extend_from_slice(&m);
        }
        let mut next = None;
        for mut c in chunks.into_iter().rev() {
            if let Some(n) = next {
                put16(&mut c, LF_INDEX);
                put16(&mut c, 0);
                put32(&mut c, n);
            }
            next = Some(self.add(LF_FIELDLIST, c));
        }
        next.expect("a field list")
    }

    /// Struct `name` (unique by `key`) of `size` bytes and `members`.
    fn structure(&mut self, name: &str, key: &str, size: u32, members: &[Member]) -> u32 {
        let list = self.fieldlist(members);
        let mut b = Vec::new();
        put16(&mut b, members.len().min(0xffff) as u16);
        put16(&mut b, PROP_UNIQUE_NAME);
        put32(&mut b, list);
        put32(&mut b, 0);
        put32(&mut b, 0);
        numeric(&mut b, size);
        cstr(&mut b, name);
        cstr(&mut b, &format!("{name}#{:016x}", fnv(key.as_bytes())));
        let i = self.add(LF_STRUCTURE, b);
        self.sizes.insert(i, size);
        i
    }

    fn array(&mut self, elem: u32, bytes: u32) -> u32 {
        let mut b = Vec::new();
        put32(&mut b, elem);
        put32(&mut b, if self.ptr64() { T_UQUAD } else { T_ULONG });
        numeric(&mut b, bytes);
        cstr(&mut b, "");
        let i = self.add(LF_ARRAY, b);
        self.sizes.insert(i, bytes);
        i
    }

    /// `LF_PROCEDURE` returning `ret` (`None`: nothing) of `args`.
    fn procedure(&mut self, ret: Option<u32>, args: &[u32]) -> u32 {
        let mut a = Vec::new();
        put32(&mut a, args.len() as u32);
        for &x in args {
            put32(&mut a, x);
        }
        let list = self.add(LF_ARGLIST, a);
        let mut b = Vec::new();
        put32(&mut b, ret.unwrap_or(T_VOID));
        b.push(0);
        b.push(0);
        put16(&mut b, args.len() as u16);
        put32(&mut b, list);
        self.add(LF_PROCEDURE, b)
    }

    /// A GC object header's members.
    fn header() -> Vec<Member> {
        vec![
            ("type_id".to_owned(), T_UINT4, 0),
            ("gc_flags".to_owned(), T_UINT4, 4),
            ("gc_word".to_owned(), T_UQUAD, 8),
        ]
    }

    /// Start struct `key`: a reference back to it while its members are
    /// described is its forward reference.
    fn begin(&mut self, key: &str, name: &str) {
        let f = self.fwd(name, key);
        self.building.insert(key.to_owned(), f);
    }

    fn end(&mut self, key: &str, name: &str, size: u32, members: &[Member]) -> u32 {
        let s = self.structure(name, key, size, members);
        self.building.remove(key);
        s
    }

    /// The type of `d`; `None` for `()`.
    fn debug_ty(&mut self, d: &DebugTy) -> Option<u32> {
        let key = format!("{d:?}");
        if let Some(&o) = self.by_key.get(&key) {
            return Some(o);
        }
        if let Some(&f) = self.building.get(&key) {
            return Some(f);
        }
        let ty = match d {
            DebugTy::Mir(t) => return self.ty(t),
            // A cell object: the header, then the value.
            DebugTy::Cell(elem) => self.boxed("cell", &key, elem),
            DebugTy::Boxed(name, value) => self.boxed(name, &key, value),
            DebugTy::Buf(name, elem) => {
                self.begin(&key, name);
                let mut m = Self::header();
                if let Some(e) = self.debug_ty(elem) {
                    let items = self.array(e, 0);
                    m.push(("items".to_owned(), items, HEADER_SIZE));
                }
                let obj = self.end(&key, name, HEADER_SIZE, &m);
                self.pointer_to(obj)
            }
            DebugTy::Any => {
                let m = self.m;
                self.sum("any", &key, &nabic_mir::any_ty(), &m.debug.any)?
            }
            DebugTy::Sum(name, mir, variants) => self.sum(name, &key, mir, variants)?,
            DebugTy::Closure(name, mir, env) => {
                let MirTy::Tuple(ps) = mir else {
                    return self.ty(mir);
                };
                self.begin(&key, name);
                let code = self.ty(&ps[0]);
                let env = self.debug_ty(env);
                let at = self.t.field_offsets(mir);
                let size = self.t.size_align(mir).0;
                let mut m = Vec::new();
                if let Some(c) = code {
                    m.push(("code".to_owned(), c, at[0]));
                }
                if let Some(e) = env {
                    m.push(("env".to_owned(), e, at[1]));
                }
                self.end(&key, name, size, &m)
            }
        };
        self.by_key.insert(key, ty);
        Some(ty)
    }

    /// A pointer to a GC object `name` holding `value` after its header.
    fn boxed(&mut self, name: &str, key: &str, value: &DebugTy) -> u32 {
        self.begin(key, name);
        let mut m = Self::header();
        let mut size = HEADER_SIZE;
        if let Some(v) = self.debug_ty(value) {
            m.push(("value".to_owned(), v, HEADER_SIZE));
            size += self.t.size_align(&value.mir()).0;
        }
        let obj = self.end(key, name, size, &m);
        self.pointer_to(obj)
    }

    /// A `(tag, payload)` value: its tag, and each variant as a struct over
    /// the whole value at offset 0.
    fn sum(
        &mut self,
        name: &str,
        key: &str,
        mir: &MirTy,
        variants: &[(u32, String, DebugTy)],
    ) -> Option<u32> {
        let size = self.t.size_align(mir).0;
        let at = self.t.field_offsets(mir)[1];
        self.begin(key, name);
        let tag = self.base(mir.debug_tag())?;
        let mut m: Vec<Member> = vec![("tag".to_owned(), tag, 0)];
        for (_, v, p) in variants {
            let fields: Vec<Member> = match p {
                DebugTy::Mir(t @ MirTy::Tuple(ps)) => self
                    .members(t, ps, &[], &[])
                    .into_iter()
                    .map(|(n, ty, o)| (n, ty, at + o))
                    .collect(),
                p => self
                    .debug_ty(p)
                    .map(|ty| ("0".to_owned(), ty, at))
                    .into_iter()
                    .collect(),
            };
            let vname = format!("{name}::{v}");
            let vkey = format!("{key}::{v}");
            let s = self.structure(&vname, &vkey, size, &fields);
            m.push((v.clone(), s, 0));
        }
        Some(self.end(key, name, size, &m))
    }

    /// The type of `ty`; `None` for `()`.
    fn ty(&mut self, ty: &MirTy) -> Option<u32> {
        let key = format!("{ty:?}");
        if let Some(&o) = self.by_key.get(&key) {
            return Some(o);
        }
        if let Some(&f) = self.building.get(&key) {
            return Some(f);
        }
        let t = self.t;
        let w = t.pointer_width / 8;
        let size = t.size_align(ty).0;
        let out = match ty {
            MirTy::Unit => return None,
            MirTy::Prim(PrimTy::Str) => {
                let ptr = self.pointer_to(T_UCHAR);
                let len = self.base(PrimTy::Usize)?;
                self.begin(&key, "str");
                let m = [("ptr".to_owned(), ptr, 0), ("len".to_owned(), len, w)];
                self.end(&key, "str", size, &m)
            }
            MirTy::Prim(p) => return self.base(*p),
            MirTy::Fn(args, ret) => {
                let ret = self.ty(ret);
                let args: Vec<u32> = args.iter().filter_map(|a| self.ty(a)).collect();
                let p = self.procedure(ret, &args);
                self.pointer_to(p)
            }
            MirTy::GcRef => self.pointer_to(T_VOID),
            MirTy::Any => {
                let payload = self.base(PrimTy::Usize)?;
                self.begin(&key, "any");
                let m = [
                    ("tag".to_owned(), T_INT4, 0),
                    ("payload".to_owned(), payload, w),
                ];
                self.end(&key, "any", size, &m)
            }
            MirTy::Tuple(ps) => {
                self.begin(&key, "tuple");
                let m = self.members(ty, ps, &[], &[]);
                self.end(&key, "tuple", size, &m)
            }
            MirTy::Struct(id, ps) => {
                let m = self.m;
                let (name, names) = m
                    .struct_names
                    .get(&id.0)
                    .cloned()
                    .unwrap_or_else(|| (format!("Struct#{}", id.0), Vec::new()));
                let names = if names.len() == ps.len() {
                    names
                } else {
                    Vec::new()
                };
                let dbg = m.debug.fields.get(&id.0).map_or(&[][..], Vec::as_slice);
                self.begin(&key, &name);
                let fields = self.members(ty, ps, &names, dbg);
                self.end(&key, &name, size, &fields)
            }
            MirTy::Array(..) | MirTy::Union(..) => {
                let (elem, n) = match ty {
                    MirTy::Array(e, n) => ((**e).clone(), *n),
                    MirTy::Union(_, n, _) => (MirTy::Prim(PrimTy::Usize), *n),
                    _ => unreachable!(),
                };
                let base = self.ty(&elem)?;
                let esize = t.size_align(&elem).0;
                self.array(base, esize * n)
            }
        };
        self.by_key.insert(key, out);
        Some(out)
    }

    /// `(name, type, offset)` of each part of `agg` (`names[i]`, else `i`;
    /// typed `debug[i]` when given); `()` parts are left out.
    fn members(
        &mut self,
        agg: &MirTy,
        parts: &[MirTy],
        names: &[String],
        debug: &[DebugTy],
    ) -> Vec<Member> {
        let offs = self.t.field_offsets(agg);
        let mut out = Vec::new();
        for (i, (p, off)) in parts.iter().zip(offs).enumerate() {
            let ty = match debug.get(i) {
                Some(d) if d.fits(p) => self.debug_ty(d),
                _ => self.ty(p),
            };
            if let Some(ty) = ty {
                let n = names.get(i).cloned().unwrap_or_else(|| i.to_string());
                out.push((n, ty, off));
            }
        }
        out
    }
}

/// `[start, end)` code ranges of scope `id` and its nested scopes.
fn scope_code(id: u32, d: &FnDebug, f: &FnRange) -> Vec<(u32, u32)> {
    let mut ids = vec![id];
    let mut n = 0;
    while n < ids.len() {
        let p = ids[n];
        ids.extend(d.scopes.iter().filter(|s| s.1 == p).map(|s| s.0));
        n += 1;
    }
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (i, &(at, _, _, sc)) in f.lines.iter().enumerate() {
        let end = f.lines.get(i + 1).map_or(f.len, |n| n.0);
        if end <= at || !ids.contains(&sc) {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.1 == at => last.1 = end,
            _ => out.push((at, end)),
        }
    }
    out
}

/// Scope `s` is `root` or within it.
fn within(d: &FnDebug, mut s: u32, root: u32) -> bool {
    for _ in 0..=d.scopes.len() {
        if s == root {
            return true;
        }
        if s == 0 {
            return false;
        }
        s = d.scopes.iter().find(|x| x.0 == s).map_or(0, |x| x.1);
    }
    false
}

/// The line positions in scope `s` stand for in the code of inline site
/// `site` (or, `site` 0, of the function): their own, or the call's of
/// the body inlined there they are in.
fn line_in(d: &FnDebug, s: u32, line: u32, site: u32) -> u32 {
    let mut out = line;
    let mut x = s;
    for _ in 0..=d.scopes.len() {
        if x == site || x == 0 {
            break;
        }
        if let Some(i) = d.inlined.iter().find(|i| i.scope == x) {
            out = i.call_line;
        }
        x = d.scopes.iter().find(|y| y.0 == x).map_or(0, |y| y.1);
    }
    out
}

/// `v` as a compressed annotation number.
fn compress(b: &mut Vec<u8>, v: u32) {
    if v < 0x80 {
        b.push(v as u8);
    } else if v < 0x4000 {
        b.extend_from_slice(&[(v >> 8) as u8 | 0x80, v as u8]);
    } else {
        b.extend_from_slice(&[
            (v >> 24) as u8 | 0xc0,
            (v >> 16) as u8,
            (v >> 8) as u8,
            v as u8,
        ]);
    }
}

/// A signed annotation number: the magnitude, the sign in bit 0.
const fn signed(v: i64) -> u32 {
    if v < 0 {
        (((-v) as u32) << 1) | 1
    } else {
        (v as u32) << 1
    }
}

/// The binary annotations of inline site `site` (whose callee starts at
/// `line`): its code ranges and their lines, as LLVM encodes them.
fn annotations(d: &FnDebug, f: &FnRange, site: u32, line: u32) -> Vec<u8> {
    let mut b = Vec::new();
    let (mut cur, mut last, mut end) = (line, 0u32, None::<u32>);
    for (i, &(at, l, _, sc)) in f.lines.iter().enumerate() {
        let hi = f.lines.get(i + 1).map_or(f.len, |n| n.0);
        if hi <= at {
            continue;
        }
        if !within(d, sc, site) {
            // A gap: the open range ends.
            if let Some(e) = end.take() {
                b.push(BA_CHANGE_CODE_LENGTH as u8);
                compress(&mut b, e - last);
                last = e;
            }
            continue;
        }
        let l = line_in(d, sc, l, site);
        if end.is_some() && l == cur {
            end = Some(hi);
            continue;
        }
        let ld = signed(i64::from(l) - i64::from(cur));
        let cd = at - last;
        if ld < 8 && cd <= 0xf {
            b.push(BA_CHANGE_CODE_OFFSET_AND_LINE_OFFSET as u8);
            compress(&mut b, (ld << 4) | cd);
        } else {
            if ld != 0 {
                b.push(BA_CHANGE_LINE_OFFSET as u8);
                compress(&mut b, ld);
            }
            b.push(BA_CHANGE_CODE_OFFSET as u8);
            compress(&mut b, cd);
        }
        (last, cur, end) = (at, l, Some(hi));
    }
    if let Some(e) = end {
        b.push(BA_CHANGE_CODE_LENGTH as u8);
        compress(&mut b, e - last);
    }
    b
}

/// A function's symbols being written: records, relocations against it.
struct FnSyms<'a> {
    b: Vec<u8>,
    relocs: Vec<(u32, CvRel, usize)>,
    f: usize,
    d: &'a FnDebug,
    offs: &'a [i32],
    tys: &'a [Option<u32>],
    fr: &'a FnRange,
    /// Optimized: each variable's places (`FnRange::locs`).
    locs: &'a [Spans],
    /// Inline sites with code, and their callees' `LF_FUNC_ID`s.
    sites: &'a [(u32, u32)],
    /// The frame pointer's `CodeView` number.
    fp: u16,
    /// Parameters a dbghelp debugger takes for pointers to their value.
    byref: &'a [bool],
    /// Float constants' values (`CvSect::consts`).
    consts: Vec<u8>,
}

impl FnSyms<'_> {
    /// Offset `at` in the function and its section, relocated.
    fn addr(&mut self, body: &mut Vec<u8>, at: u32) {
        // The record's header (4 bytes) comes first.
        let base = self.b.len() as u32 + 4;
        self.relocs
            .push((base + body.len() as u32, CvRel::SecRel, self.f));
        put32(body, at);
        self.relocs
            .push((base + body.len() as u32, CvRel::Section, self.f));
        put16(body, 0);
    }

    fn var(&mut self, i: usize) {
        let Some(ty) = self.tys[i] else {
            return;
        };
        let v = &self.d.vars[i];
        let all = self.locs.get(i).map_or(&[][..], Vec::as_slice);
        // Optimized, one constant wherever it has a value: `S_CONSTANT`
        // (a range has no constant form).
        if let (true, Some((_, _, Place::Const(c)))) = (self.d.opt, all.first()) {
            let basic = ty < 0x1000 && ty & 0x700 == 0 && !matches!(v.ty, DebugTy::Cell(_));
            if basic && all.iter().all(|s| s.2 == Place::Const(*c)) {
                let mut b = Vec::new();
                put32(&mut b, ty);
                if matches!(ty, T_REAL32 | T_REAL64) {
                    // dbghelp has no float `S_CONSTANT` (cdb fails on any
                    // real or integer leaf of a float type): the value in
                    // memory, a static of the scope.
                    let size = if ty == T_REAL32 { 4 } else { 8 };
                    let at = self.consts.len().next_multiple_of(8);
                    self.consts.resize(at, 0);
                    self.consts.extend_from_slice(&c.to_le_bytes()[..size]);
                    let base = self.b.len() as u32 + 4;
                    self.relocs
                        .push((base + b.len() as u32, CvRel::ConstSecRel, self.f));
                    put32(&mut b, at as u32);
                    self.relocs
                        .push((base + b.len() as u32, CvRel::ConstSection, self.f));
                    put16(&mut b, 0);
                    cstr(&mut b, &v.name);
                    sym(&mut self.b, S_LDATA32, &b);
                    return;
                }
                constant(&mut b, ty, *c);
                cstr(&mut b, &v.name);
                sym(&mut self.b, S_CONSTANT, &b);
                return;
            }
        }
        // Optimized: the places with a `CodeView` form.
        let spans: Option<Vec<(u32, u32, Place)>> = self
            .d
            .opt
            .then(|| all.iter().filter(|s| cv_place(&s.2)).cloned().collect());
        let gone = spans.as_ref().is_some_and(Vec::is_empty);
        let mut b = Vec::new();
        put32(&mut b, ty);
        let mut flags = if v.param && !self.byref[i] {
            LOCAL_PARAM
        } else {
            0
        };
        if gone {
            flags |= LOCAL_OPTIMIZED_OUT;
        }
        put16(&mut b, flags);
        cstr(&mut b, &v.name);
        sym(&mut self.b, S_LOCAL, &b);
        let Some(spans) = spans else {
            sym(
                &mut self.b,
                S_DEFRANGE_FRAMEPOINTER_REL_FULL_SCOPE,
                &self.offs[i].to_le_bytes(),
            );
            return;
        };
        if let [(0, hi, Place::Frame(off))] = spans[..] {
            if hi >= self.fr.len {
                sym(
                    &mut self.b,
                    S_DEFRANGE_FRAMEPOINTER_REL_FULL_SCOPE,
                    &off.to_le_bytes(),
                );
                return;
            }
        }
        for (lo, hi, p) in spans {
            // A range's length is 16 bits.
            let mut at = lo;
            while at < hi {
                let len = (hi - at).min(0xffff);
                match &p {
                    Place::Frame(off) => {
                        let mut b = off.to_le_bytes().to_vec();
                        self.range(&mut b, at, len);
                        sym(&mut self.b, S_DEFRANGE_FRAMEPOINTER_REL, &b);
                    }
                    Place::Reg(_, r) => {
                        let mut b = Vec::new();
                        put16(&mut b, *r);
                        put16(&mut b, 0);
                        self.range(&mut b, at, len);
                        sym(&mut self.b, S_DEFRANGE_REGISTER, &b);
                    }
                    Place::Pieces(ps) => {
                        let mut off = 0;
                        for (size, q) in ps {
                            // The offset in the parent is 12 bits.
                            match q {
                                Some(Place::Reg(_, r)) if *r != 0 && off < 0x1000 => {
                                    let mut b = Vec::new();
                                    put16(&mut b, *r);
                                    put16(&mut b, 0);
                                    put32(&mut b, off);
                                    self.range(&mut b, at, len);
                                    sym(&mut self.b, S_DEFRANGE_SUBFIELD_REGISTER, &b);
                                }
                                Some(Place::Frame(o)) if off < 0x1000 => {
                                    let mut b = Vec::new();
                                    put16(&mut b, self.fp);
                                    put16(
                                        &mut b,
                                        RANGE_SUBFIELD | ((off as u16) << RANGE_PARENT_SHIFT),
                                    );
                                    put32(&mut b, *o as u32);
                                    self.range(&mut b, at, len);
                                    sym(&mut self.b, S_DEFRANGE_REGISTER_REL, &b);
                                }
                                _ => {}
                            }
                            off += size;
                        }
                    }
                    Place::Const(_) | Place::Local(_) => {}
                }
                at += len;
            }
        }
    }

    /// A `CV_LVAR_ADDR_RANGE`: `len` bytes from offset `at`, no gaps.
    fn range(&mut self, body: &mut Vec<u8>, at: u32, len: u32) {
        self.addr(body, at);
        put16(body, len as u16);
    }

    /// The block (or 0, the body) whose variables scope `s`'s are: a scope
    /// of one code range holding a variable of its own is an `S_BLOCK32`,
    /// an inline site with code an `S_INLINESITE`, any other's variables
    /// go with the enclosing one.
    fn owner(&self, mut s: u32, blocks: &[u32]) -> u32 {
        while s != 0 && !blocks.contains(&s) && !self.sites.iter().any(|x| x.0 == s) {
            s = self
                .d
                .scopes
                .iter()
                .find(|(c, _)| *c == s)
                .map_or(0, |x| x.1);
        }
        s
    }

    /// The variables of block `id`, then the blocks and inline sites in
    /// it.
    fn block(&mut self, id: u32, fr: &FnRange, blocks: &[u32]) {
        let d = self.d;
        // Variables of a body inlined and optimized away: none to show.
        let sites = self.sites;
        let dead = |s: u32| {
            d.inlined
                .iter()
                .any(|i| within(d, s, i.scope) && !sites.iter().any(|x| x.0 == i.scope))
        };
        for i in 0..d.vars.len() {
            if self.owner(d.vars[i].scope, blocks) == id && !dead(d.vars[i].scope) {
                self.var(i);
            }
        }
        // Inline sites go in the procedure or the site they are in, never
        // in an `S_BLOCK32` (dbghelp looks for them only there).
        let site = id == 0 || sites.iter().any(|x| x.0 == id);
        for &(c, parent) in &d.scopes {
            if let Some(&(_, func)) = sites.iter().find(|x| x.0 == c) {
                if !site || self.site_of(parent) != id {
                    continue;
                }
                let line = d
                    .inlined
                    .iter()
                    .find(|i| i.scope == c)
                    .map_or(0, |i| i.line);
                let mut b = Vec::new();
                put32(&mut b, 0);
                put32(&mut b, 0);
                put32(&mut b, func);
                b.extend(annotations(d, fr, c, line));
                while b.len() % 4 != 0 {
                    b.push(0);
                }
                sym(&mut self.b, S_INLINESITE, &b);
                self.block(c, fr, blocks);
                sym(&mut self.b, S_INLINESITE_END, &[]);
                continue;
            }
            if !blocks.contains(&c) || self.owner(parent, blocks) != id {
                continue;
            }
            let (lo, hi) = scope_code(c, d, fr)[0];
            let mut b = Vec::new();
            put32(&mut b, 0);
            put32(&mut b, 0);
            put32(&mut b, hi - lo);
            self.addr(&mut b, lo);
            cstr(&mut b, "");
            sym(&mut self.b, S_BLOCK32, &b);
            self.block(c, fr, blocks);
            sym(&mut self.b, S_END, &[]);
        }
    }

    /// The inline site (or 0, the procedure) scope `s` is in.
    fn site_of(&self, mut s: u32) -> u32 {
        while s != 0 && !self.sites.iter().any(|x| x.0 == s) {
            s = self
                .d
                .scopes
                .iter()
                .find(|(c, _)| *c == s)
                .map_or(0, |x| x.1);
        }
        s
    }
}

/// The `CodeView` of `obj`'s functions.
#[must_use]
pub fn write(obj: &Obj, m: &MirModule, sources: &nabic_diagnostic::SourceMap) -> Option<Cv> {
    let t = &m.target;
    let fns: Vec<usize> = (0..obj.fns.len())
        .filter(|&i| obj.fns[i].debug.is_some())
        .collect();
    let first_file = fns
        .iter()
        .filter_map(|&i| obj.fns[i].debug.as_ref().map(|d| d.0.file))
        .find(|&f| f != 0)?;
    let mut used: Vec<u32> = vec![first_file];
    for &i in &fns {
        let d = obj.fns[i].debug.as_ref().map(|d| &d.0);
        let sites = d.iter().flat_map(|d| &d.inlined).map(|i| i.file);
        for id in d.map(|d| d.file).into_iter().chain(sites) {
            if id != 0 && !used.contains(&id) {
                used.push(id);
            }
        }
    }
    let all = sources.files();
    let files: Vec<PathBuf> = used
        .iter()
        .map(|&id| {
            all.get(id as usize - 1)
                .map_or_else(PathBuf::new, |(p, _)| sources.output_path(p).into())
        })
        .collect();
    // A file's checksum entry: 8 bytes each, no checksum.
    let chk = |id: u32| {
        let id = if id == 0 { first_file } else { id };
        used.iter().position(|&u| u == id).unwrap_or(0) as u32 * 8
    };

    let mut types = Types {
        recs: Vec::new(),
        count: 0,
        by_bytes: HashMap::default(),
        by_key: HashMap::default(),
        building: HashMap::default(),
        sizes: HashMap::default(),
        t,
        m,
    };
    // `CV_CFL_PENTIUMIII`, `CV_CFL_X64`, `CV_CFL_ARM64`.
    let machine: u16 = if obj.i386 {
        0x07
    } else if obj.x86 {
        0xd0
    } else {
        0xf6
    };

    // `CV_AMD64_RBP`, `CV_REG_EBP`, `CV_ARM64_FP`.
    let fp: u16 = if obj.i386 {
        22
    } else if obj.x86 {
        334
    } else {
        79
    };

    let mut main = Vec::new();
    put32(&mut main, SIGNATURE);
    let mut syms = Vec::new();
    let mut b = Vec::new();
    put32(&mut b, 0);
    cstr(&mut b, "");
    sym(&mut syms, S_OBJNAME, &b);
    let mut b = Vec::new();
    put32(&mut b, CV_CFL_RUST);
    put16(&mut b, machine);
    let (maj, min, patch) = version();
    for v in [maj, min, patch, 0, maj, min, patch, 0] {
        put16(&mut b, v);
    }
    cstr(&mut b, &format!("nabic {maj}.{min}.{patch}"));
    sym(&mut syms, S_COMPILE3, &b);
    subsection(&mut main, DEBUG_S_SYMBOLS, &syms);
    let mut strtab = vec![0u8];
    let mut checks = Vec::new();
    for f in &files {
        put32(&mut checks, strtab.len() as u32);
        put16(&mut checks, 0);
        put16(&mut checks, 0);
        cstr(&mut strtab, &f.to_string_lossy());
    }
    subsection(&mut main, DEBUG_S_FILECHKSMS, &checks);
    subsection(&mut main, DEBUG_S_STRINGTABLE, &strtab);

    let mut out = Vec::new();
    let mut inlinees: Vec<(u32, u32, u32)> = Vec::new();
    for &i in &fns {
        let f = &obj.fns[i];
        let (d, offs) = f.debug.as_ref().expect("debug");
        let tys: Vec<Option<u32>> = d.vars.iter().map(|v| types.debug_ty(&v.ty)).collect();
        // dbghelp reads a struct or array parameter the ABI passes by
        // reference (x64: not of 1, 2, 4 or 8 bytes; arm64: over 16) as a
        // pointer to it, where the value itself is described: such a one
        // is a plain local.
        let byref: Vec<bool> = d
            .vars
            .iter()
            .zip(&tys)
            .map(|(v, t)| {
                let size = t.and_then(|t| types.sizes.get(&t).copied());
                v.param
                    && size.is_some_and(|n| {
                        if obj.i386 {
                            false
                        } else if obj.x86 {
                            !matches!(n, 1 | 2 | 4 | 8)
                        } else {
                            n > 16
                        }
                    })
            })
            .collect();
        let ret = d.ret.and_then(|p| types.base(p));
        let mut params: Vec<(usize, u32)> = d
            .vars
            .iter()
            .zip(&tys)
            .filter_map(|(v, t)| Some((v.arg?, (*t)?)))
            .collect();
        params.sort_unstable();
        let args: Vec<u32> = params.into_iter().map(|p| p.1).collect();
        let proc_ty = types.procedure(ret, &args);
        // Inline sites with code: their callees' ids.
        let sites: Vec<(u32, u32)> = d
            .inlined
            .iter()
            .filter(|x| !scope_code(x.scope, d, f).is_empty())
            .map(|x| {
                let ty = types.procedure(None, &[]);
                let mut b = Vec::new();
                put32(&mut b, 0);
                put32(&mut b, ty);
                cstr(&mut b, &nabic_core::local::unmangle(&x.name));
                (x.scope, types.add(LF_FUNC_ID, b))
            })
            .collect();

        let mut s = FnSyms {
            b: Vec::new(),
            relocs: Vec::new(),
            f: i,
            d,
            offs,
            tys: &tys,
            fr: f,
            locs: &f.locs,
            sites: &sites,
            fp,
            byref: &byref,
            consts: Vec::new(),
        };
        let global = obj.syms[f.sym as usize].global;
        let mut b = Vec::new();
        for _ in 0..3 {
            put32(&mut b, 0);
        }
        put32(&mut b, f.len);
        // Debug start: the first line row past the prologue.
        let body = f
            .lines
            .iter()
            .map(|l| l.0)
            .find(|&a| a >= f.saves.at)
            .unwrap_or(f.saves.at);
        put32(&mut b, body.min(f.len));
        put32(&mut b, f.len);
        put32(&mut b, proc_ty);
        s.addr(&mut b, 0);
        b.push(if d.opt {
            PROC_NOFPO | PROC_OPTDBGINFO
        } else {
            PROC_NOFPO
        });
        cstr(&mut b, &nabic_core::local::unmangle(&d.name));
        sym(&mut s.b, if global { S_GPROC32 } else { S_LPROC32 }, &b);
        let mut b = Vec::new();
        for _ in 0..5 {
            put32(&mut b, 0);
        }
        put16(&mut b, 0);
        put32(&mut b, FRAMEPROC_FP);
        sym(&mut s.b, S_FRAMEPROC, &b);
        // What is inlined in it, which dbghelp looks for.
        if !sites.is_empty() {
            let mut ids: Vec<u32> = sites.iter().map(|x| x.1).collect();
            ids.sort_unstable();
            ids.dedup();
            let mut b = Vec::new();
            put32(&mut b, ids.len() as u32);
            for id in ids {
                put32(&mut b, id);
            }
            sym(&mut s.b, S_INLINEES, &b);
        }
        // Scopes holding a variable of their own in one code range.
        let blocks: Vec<u32> = d
            .scopes
            .iter()
            .map(|s| s.0)
            .filter(|&c| {
                d.vars
                    .iter()
                    .zip(&tys)
                    .any(|(v, t)| v.scope == c && t.is_some())
                    && scope_code(c, d, f).len() == 1
            })
            .collect();
        s.block(0, f, &blocks);
        sym(&mut s.b, S_END, &[]);

        let mut data = Vec::new();
        put32(&mut data, SIGNATURE);
        let base = data.len() as u32 + 8;
        let mut relocs: Vec<(u32, CvRel, usize)> =
            s.relocs.iter().map(|&(o, k, f)| (o + base, k, f)).collect();
        subsection(&mut data, DEBUG_S_SYMBOLS, &s.b);

        // Each callee inlined here: its file and line.
        for &(c, func) in &sites {
            if let Some(x) = d.inlined.iter().find(|x| x.scope == c) {
                if !inlinees.iter().any(|i| i.0 == func) {
                    inlinees.push((func, chk(x.file), x.line));
                }
            }
        }

        // Line table: offsets ascending, one row per offset, no line 0;
        // inlined code at its call's line.
        let mut rows: Vec<(u32, u32)> = vec![(0, d.line)];
        for &(a, l, _, sc) in &f.lines {
            let l = if d.opt { line_in(d, sc, l, 0) } else { l };
            if l == 0 || a >= f.len {
                continue;
            }
            if rows.last().is_some_and(|r| r.0 == a) {
                rows.pop();
            }
            if rows.last().is_some_and(|r| r.1 == l) {
                continue;
            }
            rows.push((a, l));
        }
        rows.sort_by_key(|r| r.0);
        let mut lines = Vec::new();
        let base = data.len() as u32 + 8;
        relocs.push((base, CvRel::SecRel, i));
        put32(&mut lines, 0);
        relocs.push((base + 4, CvRel::Section, i));
        put16(&mut lines, 0);
        put16(&mut lines, 0);
        put32(&mut lines, f.len);
        put32(&mut lines, chk(d.file));
        put32(&mut lines, rows.len() as u32);
        put32(&mut lines, 12 + 8 * rows.len() as u32);
        for (a, l) in rows {
            put32(&mut lines, a);
            put32(&mut lines, (l & 0xff_ffff) | 0x8000_0000);
        }
        subsection(&mut data, DEBUG_S_LINES, &lines);
        out.push((
            i,
            CvSect {
                data,
                relocs,
                consts: s.consts,
            },
        ));
    }
    // The callees inlined anywhere, in the object's one
    // `DEBUG_S_INLINEELINES` (dbghelp reads only a module's first: one
    // per function hides the inline frames of all but the first).
    if !inlinees.is_empty() {
        let mut inl = Vec::new();
        put32(&mut inl, 0);
        for (func, file, line) in inlinees {
            put32(&mut inl, func);
            put32(&mut inl, file);
            put32(&mut inl, line);
        }
        subsection(&mut main, DEBUG_S_INLINEELINES, &inl);
    }
    let mut tys = Vec::new();
    put32(&mut tys, SIGNATURE);
    tys.extend_from_slice(&types.recs);
    Some(Cv {
        main,
        fns: out,
        types: tys,
    })
}

/// The compiler's version for `S_COMPILE3`.
fn version() -> (u16, u16, u16) {
    let mut v = env!("CARGO_PKG_VERSION")
        .split(['.', '-'])
        .map(|p| p.parse::<u16>().unwrap_or(0));
    (
        v.next().unwrap_or(0),
        v.next().unwrap_or(0),
        v.next().unwrap_or(0),
    )
}

/// Whether `p` has a `CodeView` form: a piece in a register or in
/// memory has one (at an offset the record holds).
fn cv_place(p: &Place) -> bool {
    match p {
        Place::Frame(_) => true,
        Place::Reg(_, r) => *r != 0,
        Place::Pieces(ps) => {
            let mut off = 0;
            ps.iter().any(|(size, q)| {
                let at = off;
                off += size;
                at < 0x1000 && matches!(q, Some(Place::Reg(_, 1..) | Place::Frame(_)))
            })
        }
        Place::Const(_) | Place::Local(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(ty: u32, bits: u64) -> Vec<u8> {
        let mut b = Vec::new();
        constant(&mut b, ty, bits);
        b
    }

    #[test]
    fn constants_as_numeric_leaves() {
        assert_eq!(leaf(T_QUAD, 40), [40, 0]);
        // Sign from the type's width: -1 of an i32, sign-extended or not.
        assert_eq!(leaf(T_INT4, u64::MAX), [0x00, 0x80, 0xff]);
        assert_eq!(leaf(T_INT4, 0xffff_ffff), [0x00, 0x80, 0xff]);
        assert_eq!(leaf(T_SHORT, (-300i64) as u64), [0x01, 0x80, 0xd4, 0xfe]);
        assert_eq!(leaf(T_QUAD, 1 << 40), {
            let mut v = vec![0x09, 0x80];
            v.extend_from_slice(&(1i64 << 40).to_le_bytes());
            v
        });
        assert_eq!(leaf(T_UINT4, 0x9000_0000), [0x04, 0x80, 0, 0, 0, 0x90]);
        assert_eq!(leaf(T_USHORT, 0x9000), [0x02, 0x80, 0, 0x90]);
        assert_eq!(leaf(T_BOOL08, 1), [1, 0]);
    }

    #[test]
    fn pieces_with_a_codeview_form() {
        let reg = |r| Some(Place::Reg(0, r));
        assert!(cv_place(&Place::Pieces(vec![
            (8, None),
            (8, Some(Place::Frame(-8)))
        ])));
        assert!(cv_place(&Place::Pieces(vec![(8, reg(328)), (8, None)])));
        // Unnamed registers, constants and locals have none.
        assert!(!cv_place(&Place::Pieces(vec![
            (8, reg(0)),
            (8, Some(Place::Const(1)))
        ])));
        // Nor a field past the 12-bit offset a record holds.
        assert!(!cv_place(&Place::Pieces(vec![
            (0x1000, None),
            (8, Some(Place::Frame(0)))
        ])));
        assert!(!cv_place(&Place::Const(3)));
        assert!(!cv_place(&Place::Local(1)));
    }
}
