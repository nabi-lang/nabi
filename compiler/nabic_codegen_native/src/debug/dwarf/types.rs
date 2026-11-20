//! The `.debug_info` unit: type DIEs by key.

#[allow(clippy::wildcard_imports)]
use crate::debug::dwarf::*;

/// The type reference to the `any` being described, patched once it is
/// written: an `any` inside a type stored in one is that variant part.
pub(crate) const ANY_SELF: u32 = u32::MAX;

/// While an `any` is described: where references to it were written and
/// the types since that hold one, dropped from the cache with it.
#[derive(Default)]
pub(crate) struct AnyOpen {
    fixups: Vec<usize>,
    held: Vec<String>,
    held_offs: nabic_core::HashSet<u32>,
    /// The last reference to it or to a held type.
    last: Option<usize>,
}

/// The `.debug_info` unit being written: type DIEs by key.
pub(crate) struct Info<'a> {
    pub(crate) b: Vec<u8>,
    pub(crate) relocs: Vec<Reloc>,
    pub(crate) types: HashMap<String, u32>,
    pub(crate) t: &'a Target,
    pub(crate) m: &'a MirModule,
    /// Structs being described, by key: a reference back to one (a
    /// `GcBuf` field) is a declaration, completed by name.
    pub(crate) building: HashMap<String, String>,
    /// The `any` being described ([`ANY_SELF`] refers to it).
    pub(crate) any_open: Option<AnyOpen>,
    /// `.debug_ranges`, entries relative to the unit's `low_pc`.
    pub(crate) ranges: Vec<u8>,
    pub(crate) ranges_relocs: Vec<Reloc>,
    /// `.debug_loc`, as `ranges`.
    pub(crate) loc: Vec<u8>,
    pub(crate) loc_relocs: Vec<Reloc>,
    pub(crate) cu_low: u32,
    /// The functions' code is in sections of their own (ELF): ranges
    /// are relocated addresses, the unit's `low_pc` 0.
    pub(crate) split: bool,
    /// Bytes of an address.
    pub(crate) aw: usize,
    /// Big-endian target.
    pub(crate) be: bool,
}

impl Info<'_> {
    pub(crate) fn u32(&mut self, v: u32) {
        self.b.extend_from_slice(&int(self.be, u64::from(v), 4));
    }

    /// A reference to type DIE `ty`.
    pub(crate) fn type_ref(&mut self, ty: u32) {
        let at = self.b.len();
        if let Some(open) = &mut self.any_open {
            if ty == ANY_SELF {
                open.fixups.push(at);
            }
            if ty == ANY_SELF || open.held_offs.contains(&ty) {
                open.last = Some(at);
            }
        }
        self.u32(ty);
    }

    /// Cache type DIE `off` (written from `start` on) under `key`.
    pub(crate) fn cache(&mut self, key: String, start: usize, off: u32) {
        if let Some(open) = &mut self.any_open {
            if open.last.is_some_and(|p| p >= start) {
                open.held.push(key.clone());
                open.held_offs.insert(off);
            }
        }
        self.types.insert(key, off);
    }

    /// Offset `v` into debug section `sect` (an index of `Obj::extra`).
    pub(crate) fn sec_off(&mut self, sect: u32, v: u32) {
        self.relocs.push(Reloc {
            off: self.b.len() as u32,
            kind: RelKind::SecOff32,
            sym: sect,
        });
        self.u32(v);
    }

    /// The address `at` of function `sym`: its object address (Mach-O: a
    /// section-relative relocation, as the .dSYM writer and dsymutil read
    /// them).
    pub(crate) fn addr(&mut self, sym: u32, at: u32) {
        self.relocs.push(Reloc {
            off: self.b.len() as u32,
            kind: RelKind::Abs64,
            sym,
        });
        self.b
            .extend_from_slice(&int(self.be, u64::from(at), self.aw));
    }

    /// A `.debug_ranges` entry: `[lo, hi)` of function `f`'s code.
    pub(crate) fn range(&mut self, f: &FnRange, lo: u32, hi: u32) {
        for at in [lo, hi] {
            if self.split {
                self.ranges_relocs.push(Reloc {
                    off: self.ranges.len() as u32,
                    kind: RelKind::Abs64,
                    sym: f.sym,
                });
            }
            let v = f.start + at - self.cu_low;
            self.ranges
                .extend_from_slice(&int(self.be, u64::from(v), self.aw));
        }
    }

    /// A `.debug_loc` list of `[lo, hi)` code ranges of function `f` and
    /// location expressions; its offset.
    pub(crate) fn loc_list(&mut self, f: &FnRange, list: &[(u32, u32, Vec<u8>)]) -> u32 {
        let start = self.loc.len() as u32;
        for (lo, hi, e) in list {
            for at in [*lo, *hi] {
                if self.split {
                    self.loc_relocs.push(Reloc {
                        off: self.loc.len() as u32,
                        kind: RelKind::Abs64,
                        sym: f.sym,
                    });
                }
                let v = f.start + at - self.cu_low;
                self.loc
                    .extend_from_slice(&int(self.be, u64::from(v), self.aw));
            }
            self.loc.extend_from_slice(&int(self.be, e.len() as u64, 2));
            self.loc.extend_from_slice(e);
        }
        self.loc.resize(self.loc.len() + 2 * self.aw, 0);
        start
    }

    pub(crate) fn base(&mut self, p: PrimTy) -> Option<u32> {
        let w = self.t.pointer_width / 8;
        let (name, size, enc) = match p {
            PrimTy::I8 => ("i8", 1, DW_ATE_SIGNED),
            PrimTy::I16 => ("i16", 2, DW_ATE_SIGNED),
            PrimTy::I32 => ("i32", 4, DW_ATE_SIGNED),
            PrimTy::I64 => ("i64", 8, DW_ATE_SIGNED),
            PrimTy::Isize => ("isize", w, DW_ATE_SIGNED),
            PrimTy::U8 => ("u8", 1, DW_ATE_UNSIGNED),
            PrimTy::U16 => ("u16", 2, DW_ATE_UNSIGNED),
            PrimTy::U32 => ("u32", 4, DW_ATE_UNSIGNED),
            PrimTy::U64 => ("u64", 8, DW_ATE_UNSIGNED),
            PrimTy::Usize => ("usize", w, DW_ATE_UNSIGNED),
            PrimTy::F32 => ("f32", 4, DW_ATE_FLOAT),
            PrimTy::F64 => ("f64", 8, DW_ATE_FLOAT),
            PrimTy::Bool => ("bool", 1, DW_ATE_BOOLEAN),
            PrimTy::CChar => ("c_char", 1, DW_ATE_SIGNED_CHAR),
            PrimTy::Char => ("char", 4, DW_ATE_UTF),
            PrimTy::Str => return None,
        };
        if let Some(o) = self.types.get(name) {
            return Some(*o);
        }
        let off = self.b.len() as u32;
        self.b.push(A_BASE);
        cstr(&mut self.b, name);
        self.b.push(enc);
        uleb(&mut self.b, u64::from(size));
        self.types.insert(name.to_owned(), off);
        Some(off)
    }

    pub(crate) fn structure(
        &mut self,
        name: &str,
        size: u32,
        members: &[(String, u32, u32)],
    ) -> u32 {
        let off = self.b.len() as u32;
        if members.is_empty() {
            self.b.push(A_STRUCT_EMPTY);
            cstr(&mut self.b, name);
            uleb(&mut self.b, u64::from(size));
            return off;
        }
        self.b.push(A_STRUCT);
        cstr(&mut self.b, name);
        uleb(&mut self.b, u64::from(size));
        for (n, ty, at) in members {
            self.member(n, *ty, *at);
        }
        self.b.push(0);
        off
    }

    pub(crate) fn member(&mut self, name: &str, ty: u32, at: u32) {
        if at < 256 {
            self.b.push(A_MEMBER1);
            cstr(&mut self.b, name);
            self.type_ref(ty);
            self.b.push(at as u8);
        } else {
            self.b.push(A_MEMBER4);
            cstr(&mut self.b, name);
            self.type_ref(ty);
            self.u32(at);
        }
    }

    pub(crate) fn pointer_to(&mut self, ty: u32) -> u32 {
        let o = self.b.len() as u32;
        self.b.push(A_PTR_TO);
        self.type_ref(ty);
        uleb(&mut self.b, u64::from(self.t.pointer_width / 8));
        o
    }

    /// The type DIE of `d`, written on first use; `None` for `()`.
    pub(crate) fn debug_ty(&mut self, d: &DebugTy) -> Option<u32> {
        let key = format!("{d:?}");
        if let Some(o) = self.types.get(&key) {
            return Some(*o);
        }
        let start = self.b.len();
        let off = match d {
            DebugTy::Mir(t) => return self.ty(t),
            // Shown as the value; the variable's location goes through
            // the reference.
            DebugTy::Cell(elem) => return self.debug_ty(elem),
            // A pointer to the object: its header, then the elements (as
            // many as its size holds).
            DebugTy::Buf(name, elem) => {
                let mut m = self.header()?;
                if let Some(e) = self.debug_ty(elem) {
                    let items = self.b.len() as u32;
                    self.b.push(A_ARRAY_OPEN);
                    self.type_ref(e);
                    m.push(("items".to_owned(), items, HEADER_SIZE));
                }
                let obj = self.structure(name, HEADER_SIZE, &m);
                self.pointer_to(obj)
            }
            // A pointer to the object: its header, then the value.
            DebugTy::Boxed(name, value) => {
                let mut m = self.header()?;
                let mut size = HEADER_SIZE;
                if let Some(v) = self.debug_ty(value) {
                    m.push(("value".to_owned(), v, HEADER_SIZE));
                    size += self.t.size_align(&value.mir()).0;
                }
                let obj = self.structure(name, size, &m);
                self.pointer_to(obj)
            }
            DebugTy::Any => {
                if self.any_open.is_some() {
                    return Some(ANY_SELF);
                }
                self.any_open = Some(AnyOpen::default());
                let m = self.m;
                let off = self.variant_part("any", &nabic_mir::any_ty(), &m.debug.any);
                let open = self.any_open.take().unwrap_or_default();
                let off = off?;
                for at in open.fixups {
                    let off = int(self.be, u64::from(off), 4);
                    self.b[at..at + 4].copy_from_slice(&off);
                }
                for key in open.held {
                    self.types.remove(&key);
                }
                off
            }
            DebugTy::Sum(name, mir, variants) => self.variant_part(name, mir, variants)?,
            // `{code, env}`.
            DebugTy::Closure(name, mir, env) => {
                let MirTy::Tuple(ps) = mir else {
                    return self.ty(mir);
                };
                let code = self.ty(&ps[0])?;
                let env = self.debug_ty(env)?;
                let at = self.t.field_offsets(mir);
                let size = self.t.size_align(mir).0;
                self.structure(
                    name,
                    size,
                    &[("code".into(), code, at[0]), ("env".into(), env, at[1])],
                )
            }
        };
        self.cache(key, start, off);
        Some(off)
    }

    /// A GC object header's members.
    pub(crate) fn header(&mut self) -> Option<Vec<(String, u32, u32)>> {
        let u32_ = self.base(PrimTy::U32)?;
        let u64_ = self.base(PrimTy::U64)?;
        Some(vec![
            ("type_id".to_owned(), u32_, 0),
            ("gc_flags".to_owned(), u32_, 4),
            ("gc_word".to_owned(), u64_, 8),
        ])
    }

    /// A `(i32 tag, payload)` value as a variant part.
    pub(crate) fn variant_part(
        &mut self,
        name: &str,
        mir: &MirTy,
        variants: &[(u32, String, DebugTy)],
    ) -> Option<u32> {
        let size = self.t.size_align(mir).0;
        let at = self.t.field_offsets(mir)[1];
        // Each variant a struct over the whole sum (as Rust
        // describes enums), its payload's parts at their offsets.
        let mut arms = Vec::new();
        for (tag, v, p) in variants {
            let fields: Vec<_> = match p {
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
            arms.push((
                *tag,
                v,
                self.structure(&format!("{name}::{v}"), size, &fields),
            ));
        }
        let tag = self.base(mir.debug_tag())?;
        let off = self.b.len() as u32;
        self.b.push(A_STRUCT);
        cstr(&mut self.b, name);
        uleb(&mut self.b, u64::from(size));
        // The discriminant member follows the variant part's
        // abbreviation code and reference.
        let discr = self.b.len() as u32 + 5;
        self.b.push(A_VARIANT_PART);
        self.u32(discr);
        self.b.push(A_DISCR);
        self.u32(tag);
        self.b.push(0);
        for (t, v, ty) in arms {
            // As the discriminant reads it: an `i32` (a `u8` tag is 0 or 1).
            self.b.push(A_VARIANT);
            sleb(&mut self.b, i64::from(t as i32));
            self.member(v, ty, 0);
            self.b.push(0);
        }
        self.b.extend_from_slice(&[0, 0]);
        Some(off)
    }

    /// The type DIE of `ty`, written on first use; `None` for `()`.
    pub(crate) fn ty(&mut self, ty: &MirTy) -> Option<u32> {
        let key = format!("{ty:?}");
        if let Some(o) = self.types.get(&key) {
            return Some(*o);
        }
        if let Some(name) = self.building.get(&key).cloned() {
            let decl = format!("decl {key}");
            if let Some(o) = self.types.get(&decl) {
                return Some(*o);
            }
            let o = self.b.len() as u32;
            self.b.push(A_STRUCT_DECL);
            cstr(&mut self.b, &name);
            self.types.insert(decl, o);
            return Some(o);
        }
        let start = self.b.len();
        let t = self.t;
        let w = t.pointer_width / 8;
        let size = t.size_align(ty).0;
        let off = match ty {
            MirTy::Unit => return None,
            MirTy::Prim(PrimTy::Str) => {
                let u8_ = self.base(PrimTy::U8)?;
                let ptr = self.pointer_to(u8_);
                let len = self.base(PrimTy::Usize)?;
                self.structure(
                    "str",
                    size,
                    &[("ptr".into(), ptr, 0), ("len".into(), len, w)],
                )
            }
            MirTy::Prim(p) => return self.base(*p),
            // A pointer to its subroutine type (`()` parameters left out).
            MirTy::Fn(args, ret) => {
                let ret = self.ty(ret);
                let args: Vec<u32> = args.iter().filter_map(|a| self.ty(a)).collect();
                let sub = self.b.len() as u32;
                self.b.push(match (ret.is_some(), args.is_empty()) {
                    (true, false) => A_FN_RET,
                    (false, false) => A_FN,
                    (true, true) => A_FN_RET_LEAF,
                    (false, true) => A_FN_LEAF,
                });
                if let Some(r) = ret {
                    self.type_ref(r);
                }
                for a in &args {
                    self.b.push(A_FN_PARAM);
                    self.type_ref(*a);
                }
                if !args.is_empty() {
                    self.b.push(0);
                }
                self.pointer_to(sub)
            }
            MirTy::GcRef => {
                let o = self.b.len() as u32;
                self.b.push(A_PTR_NAMED);
                cstr(&mut self.b, "gcref");
                uleb(&mut self.b, u64::from(w));
                o
            }
            MirTy::Any => {
                let tag = self.base(PrimTy::I32)?;
                let payload = self.base(PrimTy::Usize)?;
                self.structure(
                    "any",
                    size,
                    &[("tag".into(), tag, 0), ("payload".into(), payload, w)],
                )
            }
            MirTy::Tuple(ps) => {
                let m = self.members(ty, ps, &[], &[]);
                self.structure("tuple", size, &m)
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
                self.building.insert(key.clone(), name.clone());
                let fields = self.members(ty, ps, &names, dbg);
                self.building.remove(&key);
                self.structure(&name, size, &fields)
            }
            MirTy::Array(..) | MirTy::Union(..) => {
                let (elem, n) = match ty {
                    MirTy::Array(e, n) => ((**e).clone(), *n),
                    MirTy::Union(_, n, _) => (MirTy::Prim(PrimTy::Usize), *n),
                    _ => unreachable!(),
                };
                let base = self.ty(&elem)?;
                let o = self.b.len() as u32;
                self.b.push(A_ARRAY);
                self.type_ref(base);
                self.b.push(A_SUBRANGE);
                uleb(&mut self.b, u64::from(n));
                self.b.push(0);
                o
            }
        };
        self.cache(key, start, off);
        Some(off)
    }

    /// `(name, type, offset)` of each part of `agg` (`names[i]`, else
    /// `i`; typed `debug[i]` when given); `()` parts are left out.
    pub(crate) fn members(
        &mut self,
        agg: &MirTy,
        parts: &[MirTy],
        names: &[String],
        debug: &[DebugTy],
    ) -> Vec<(String, u32, u32)> {
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
