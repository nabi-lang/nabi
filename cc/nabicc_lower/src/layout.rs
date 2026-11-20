//! C types as MIR types, with the target's C layout.

use crate::{Lx, R};
use nabic_ir_common::PrimTy;
use nabic_mir::MirTy;
use nabicc_lex::Loc;
use nabicc_parse::ty::{Func, Type, TypeId};
use nabicc_target::Prim;

impl Lx<'_> {
    pub(crate) fn func_type(&self, t: TypeId) -> Option<Func> {
        let ty = &self.u.types;
        match ty.get(ty.canon(t)) {
            Type::Func(f) => Some(f.clone()),
            _ => None,
        }
    }

    /// The MIR type of a C value or object of type `t`.
    pub(crate) fn mir_ty(&mut self, t: TypeId, loc: Loc) -> R<MirTy> {
        let types = &self.u.types;
        let c = types.canon(t);
        Ok(match types.get(c).clone() {
            Type::Prim(p) => MirTy::Prim(match self.prim(p) {
                Some(p) => p,
                None if p == Prim::Void => return Ok(MirTy::Unit),
                None => return Err(self.unsupported(p, loc)),
            }),
            Type::Enum(e) => {
                let int = types.enums[e as usize]
                    .int
                    .and_then(|i| types.prim_of(i))
                    .unwrap_or(Prim::Int);
                MirTy::Prim(self.prim(int).unwrap_or(PrimTy::I32))
            }
            Type::Pointer(_) | Type::Block(_) => MirTy::Prim(PrimTy::Usize),
            Type::SizedPointer(_, n) => MirTy::Prim(if n == 4 { PrimTy::U32 } else { PrimTy::U64 }),
            Type::Atomic(q) | Type::TypeOf(q) => return self.mir_ty(q.ty, loc),
            Type::Typedef(_) => unreachable!("canonical"),
            Type::Array { elem, len } => {
                let e = self.mir_ty(elem.ty, loc)?;
                MirTy::Array(Box::new(e), len.unwrap_or(0) as u32)
            }
            Type::Record(r) => self.record_ty(r, loc)?,
            Type::Complex(e) => {
                let e = self.mir_ty(e, loc)?;
                MirTy::Tuple(vec![e.clone(), e])
            }
            Type::Func(_) => {
                return Err(self.err(loc, "a function type where an object type is needed"));
            }
            Type::Vector { .. } => {
                return Err(self.err(
                    loc,
                    "vector types are not supported by `nabi cc` yet; build the library's \
                     scalar code path (its `NO_SIMD` / `NO_SSE` option)",
                ));
            }
            Type::Sizeless(n) => {
                return Err(self.err(loc, &format!("`{n}` (SVE) is not supported by `nabi cc`")));
            }
        })
    }

    /// The MIR type of a parameter or result (`void` is `()`).
    pub(crate) fn param_ty(&mut self, t: TypeId, loc: Loc) -> R<MirTy> {
        self.mir_ty(t, loc)
    }

    /// The MIR primitive of C type `p`.
    pub(crate) const fn prim(&self, p: Prim) -> Option<PrimTy> {
        let t = &self.u.types.t;
        let size = t.prim(p).0;
        let signed = t.signed(p);
        Some(match p {
            Prim::Void => return None,
            Prim::Bool => PrimTy::Bool,
            Prim::Float => PrimTy::F32,
            Prim::Double => PrimTy::F64,
            Prim::LongDouble if size == 8 => PrimTy::F64,
            Prim::LongDouble
            | Prim::Float128
            | Prim::Ibm128
            | Prim::Float16
            | Prim::Fp16
            | Prim::BF16
            | Prim::Int128
            | Prim::UInt128 => {
                return None;
            }
            _ => match (size, signed) {
                (1, true) => PrimTy::I8,
                (1, false) => PrimTy::U8,
                (2, true) => PrimTy::I16,
                (2, false) => PrimTy::U16,
                (4, true) => PrimTy::I32,
                (4, false) => PrimTy::U32,
                (8, true) => PrimTy::I64,
                _ => PrimTy::U64,
            },
        })
    }

    fn unsupported(&self, p: Prim, loc: Loc) -> nabicc_pp::Error {
        let msg = match p {
            Prim::LongDouble => format!(
                "`long double` is {}-bit on {}, which `nabi cc` does not compute with yet; use \
                 `double` (many libraries take `-DLONGDOUBLE_TYPE=double` or similar)",
                self.u.types.t.prim(p).0 * 8,
                self.u.types.t.triple
            ),
            Prim::Int128 | Prim::UInt128 => {
                "`__int128` is not supported by `nabi cc` yet; split the value into two 64-bit \
                 halves"
                    .to_owned()
            }
            _ => format!(
                "`{}` arithmetic is not supported by `nabi cc` yet; convert to `float` first",
                p.name()
            ),
        };
        self.err(loc, &msg)
    }

    /// A record as a MIR struct of its C layout: its natural fields when
    /// they give that layout, else packed pieces at the C offsets.
    fn record_ty(&mut self, r: u32, loc: Loc) -> R<MirTy> {
        if let Some((_, t)) = self.records.get(&r) {
            return Ok(t.clone());
        }
        let types = &self.u.types;
        let rec = types.records[r as usize].clone();
        let Some(def) = rec.def.clone() else {
            let name = rec
                .name
                .clone()
                .or_else(|| rec.typedef_name.clone())
                .unwrap_or_default();
            return Err(self.err(
                loc,
                &format!("`struct {name}` is incomplete here: define it before this use"),
            ));
        };
        let Some(l) = types.layout(r) else {
            return Err(self.err(loc, "a record whose layout is unknown"));
        };
        let id = self.records.len() as u32 + 1_000_000;
        // A placeholder: a record holding a pointer to itself.
        self.records
            .insert(r, (id, MirTy::Struct(nabic_hir::StructId(id), Vec::new())));
        let bits = def.fields.iter().any(|f| f.bits.is_some());
        let mut fields = Vec::new();
        let mut names = Vec::new();
        // Natural fields give the C layout, or it is theirs over-aligned
        // (`aligned(N)`): (natural, extra alignment).
        let (natural, over) = if !rec.union && !bits {
            for f in &def.fields {
                fields.push(self.mir_ty(f.ty.ty, loc)?);
                names.push(f.name.clone().unwrap_or_default());
            }
            let (offs, size, align) = self.t.fields_layout(None, &fields);
            let (want, walign) = (l.size as u32, l.align as u32);
            let at = offs
                .iter()
                .zip(&l.offsets)
                .all(|(a, b)| u64::from(*a) * 8 == *b);
            let over = walign > align && want == size.next_multiple_of(walign);
            let natural = at && ((size, align) == (want, walign) || over);
            (natural, natural && over)
        } else {
            (false, false)
        };
        let mut repr = nabic_hir::Repr {
            c: true,
            // The fields as they are, as the ABI sees them.
            align: if over { l.align as u32 } else { 0 },
            ..nabic_hir::Repr::default()
        };
        if !natural {
            // Pieces at their offsets: the most aligned member first for
            // a union, else every member that is not a bitfield, bytes
            // between.
            fields.clear();
            names.clear();
            let mut at = 0u64;
            let mut pieces: Vec<(u64, MirTy, String)> = Vec::new();
            if rec.union {
                let mut best: Option<(u64, u64, MirTy, String)> = None;
                for f in &def.fields {
                    if f.bits.is_some() {
                        continue;
                    }
                    let t = self.mir_ty(f.ty.ty, loc)?;
                    let (s, a) = self.t.size_align(&t);
                    let better = best
                        .as_ref()
                        .is_none_or(|b| (a as u64, s as u64) > (b.0, b.1));
                    if better {
                        best = Some((a as u64, s as u64, t, f.name.clone().unwrap_or_default()));
                    }
                }
                if let Some((_, _, t, n)) = best {
                    pieces.push((0, t, n));
                }
            } else {
                for (f, off) in def.fields.iter().zip(&l.offsets) {
                    if f.bits.is_some() {
                        continue;
                    }
                    let t = self.mir_ty(f.ty.ty, loc)?;
                    pieces.push((off / 8, t, f.name.clone().unwrap_or_default()));
                }
            }
            for (off, t, n) in pieces {
                if off < at {
                    continue;
                }
                if off > at {
                    fields.push(MirTy::Array(
                        Box::new(MirTy::Prim(PrimTy::U8)),
                        (off - at) as u32,
                    ));
                    names.push(String::new());
                }
                let s = u64::from(self.t.size_align(&t).0);
                fields.push(t);
                names.push(n);
                at = off + s;
            }
            if l.size > at {
                fields.push(MirTy::Array(
                    Box::new(MirTy::Prim(PrimTy::U8)),
                    (l.size - at) as u32,
                ));
                names.push(String::new());
            }
            repr.packed = true;
            repr.align = l.align as u32;
        }
        if rec.union {
            // The C ABI passes a union by its members, not its storage.
            let mut ms = Vec::new();
            for f in def.fields.iter().filter(|f| f.bits != Some(0)) {
                ms.push(self.mir_ty(f.ty.ty, loc)?);
            }
            let mut unions = (*self.t.unions).clone();
            unions.insert(id, ms);
            self.t.unions = std::sync::Arc::new(unions);
        }
        let ty = MirTy::Struct(nabic_hir::StructId(id), fields);
        self.reprs.insert(id, repr);
        let name = rec
            .name
            .clone()
            .or_else(|| rec.typedef_name.clone())
            .unwrap_or_else(|| "(unnamed)".to_owned());
        self.struct_names.insert(id, (name, names));
        self.records.insert(r, (id, ty.clone()));
        // The target's layout sees the repr from here on.
        let mut reprs = (*self.t.reprs).clone();
        reprs.insert(id, repr);
        self.t.reprs = std::sync::Arc::new(reprs);
        Ok(ty)
    }

    /// Size in bytes of C type `t`.
    pub(crate) fn size(&self, t: TypeId) -> u64 {
        self.u
            .types
            .size_align(self.u.types.canon(t))
            .map_or(0, |x| x.0)
    }
}
