//! Initializers: braces, designators and brace elision, flattened to the
//! stores that make the object.

use crate::check::{Cx, R};
use crate::{Bits, InitItem, InitKind, TInit};
use nabicc_lex::Loc;
use nabicc_parse::tree::{Desig, Expr, ExprKind, Init};
use nabicc_parse::ty::{Type, TypeId};
use nabicc_target::Prim;

/// A member a list initializes: its type, byte offset and bits.
#[derive(Clone, Copy)]
struct Member {
    ty: TypeId,
    off: u64,
    bits: Option<Bits>,
    name_index: usize,
}

impl Cx<'_> {
    /// Check `init` for an object of type `ty`: the type (an array of
    /// unknown size completed) and the stores.
    pub(crate) fn init_object(&mut self, ty: TypeId, init: &Init, loc: Loc) -> R<(TypeId, TInit)> {
        let ty = self.unqual(ty);
        let mut out = Vec::new();
        let n = match init {
            Init::List(items, _) => self.braced(ty, 0, items, &mut out)?,
            Init::Expr(e) => {
                let list = [(Vec::new(), init.clone())];
                let mut pos = 0;
                self.elem(ty, 0, None, &list, &mut pos, &mut out)?;
                self.string_len(ty, e)
            }
        };
        let ty = match self.array_of(ty) {
            Some((elem, None)) => {
                if n == 0 && matches!(init, Init::Expr(_)) {
                    return Err(self.err(loc, "an array needs a braced initializer: `{ .. }`"));
                }
                self.types.add(Type::Array { elem, len: Some(n) })
            }
            _ => ty,
        };
        Ok((ty, TInit { items: out }))
    }

    /// The length a string literal initializer gives a character array.
    fn string_len(&self, ty: TypeId, e: &Expr) -> u64 {
        match (&e.kind, self.array_of(ty)) {
            (ExprKind::Str(u, _), Some(_)) => u.len() as u64 + 1,
            _ => 0,
        }
    }

    /// `{ items }` for a `ty` at `off`: the array length it reaches.
    fn braced(
        &mut self,
        ty: TypeId,
        off: u64,
        items: &[(Vec<Desig>, Init)],
        out: &mut Vec<InitItem>,
    ) -> R<u64> {
        if self.is_scalar(ty) {
            // `int x = { 1 };`
            let Some((d, first)) = items.first() else {
                return Ok(0);
            };
            if !d.is_empty() {
                return Err(self.err(
                    Self::init_loc(first),
                    "a designator in a scalar's initializer",
                ));
            }
            let list = [(Vec::new(), first.clone())];
            let mut pos = 0;
            self.elem(ty, off, None, &list, &mut pos, out)?;
            return Ok(0);
        }
        // `char s[] = { "abc" }`.
        if let ([(d, Init::Expr(e))], Some(_)) = (items, self.char_array(ty)) {
            if d.is_empty() && matches!(e.kind, ExprKind::Str(..)) {
                let list = [(Vec::new(), Init::Expr(e.clone()))];
                let mut pos = 0;
                self.elem(ty, off, None, &list, &mut pos, out)?;
                return Ok(self.string_len(ty, e));
            }
        }
        let mut pos = 0;
        let n = self.fill(ty, off, items, &mut pos, true, false, out)?;
        if pos < items.len() {
            let at = Self::init_loc(&items[pos].1);
            return Err(self.err(at, "excess elements in this initializer"));
        }
        Ok(n)
    }

    const fn init_loc(i: &Init) -> Loc {
        match i {
            Init::Expr(e) => e.loc,
            Init::List(_, l) => *l,
        }
    }

    /// A character array type's element type.
    fn char_array(&self, ty: TypeId) -> Option<Prim> {
        let (elem, _) = self.array_of(ty)?;
        let p = self.prim_of(elem.ty)?;
        (p.is_integer() && self.types.t.prim(p).0 <= 4).then_some(p)
    }

    /// One sub-object `ty` at `off` from `list[*pos]` (its designators
    /// taken already): a nested list, a value, or the start of an elided
    /// brace.
    fn elem(
        &mut self,
        ty: TypeId,
        off: u64,
        bits: Option<Bits>,
        list: &[(Vec<Desig>, Init)],
        pos: &mut usize,
        out: &mut Vec<InitItem>,
    ) -> R<()> {
        let init = &list[*pos].1;
        let e = match init {
            Init::List(items, _) => {
                *pos += 1;
                self.braced(ty, off, items, out)?;
                return Ok(());
            }
            Init::Expr(e) => e,
        };
        if self.is_scalar(ty) {
            let v = self.value(e)?;
            if !self.is_scalar(v.ty) {
                return Err(self.err(
                    e.loc,
                    &format!(
                        "initializing `{}` with `{}`",
                        self.spell(ty),
                        self.spell(v.ty)
                    ),
                ));
            }
            let v = self.conv(v, ty);
            out.push(InitItem {
                off,
                kind: match bits {
                    Some(b) => InitKind::Bits(v, b),
                    None => InitKind::Expr(v),
                },
            });
            *pos += 1;
            return Ok(());
        }
        if let (Some(p), ExprKind::Str(units, _)) = (self.char_array(ty), &e.kind) {
            let size = self.types.t.prim(p).0 as usize;
            let len = self
                .array_of(ty)
                .and_then(|a| a.1)
                .map_or(units.len() + 1, |n| n as usize);
            let be = self.types.t.big_endian;
            let mut bytes = Vec::new();
            for u in units.iter().chain([&0]).take(len) {
                let b = u.to_le_bytes();
                if be {
                    bytes.extend(b[..size].iter().rev());
                } else {
                    bytes.extend(&b[..size]);
                }
            }
            out.push(InitItem {
                off,
                kind: InitKind::Bytes(bytes),
            });
            *pos += 1;
            return Ok(());
        }
        if self.record_of(ty).is_some() {
            // A whole value of the record, or the record's first member.
            let v = self.value(e)?;
            if self.record_of(v.ty) == self.record_of(ty) {
                out.push(InitItem {
                    off,
                    kind: InitKind::Expr(v),
                });
                *pos += 1;
                return Ok(());
            }
        }
        self.fill(ty, off, list, pos, false, true, out)?;
        Ok(())
    }

    /// The members of record `ty` a list fills in order (unnamed
    /// bitfields have none to fill).
    fn members(&self, ty: TypeId) -> Vec<Member> {
        let Some(rid) = self.record_of(ty) else {
            return Vec::new();
        };
        let rec = &self.types.records[rid as usize];
        let Some(def) = rec.def.as_ref() else {
            return Vec::new();
        };
        let Some(layout) = self.types.layout(rid) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (i, f) in def.fields.iter().enumerate() {
            if f.name.is_none() && !f.anon {
                continue;
            }
            let bit = layout.offsets[i];
            let ty = self.types.canon(f.ty.ty);
            let (off, bits) = match f.bits {
                Some(w) => {
                    let (o, b) = self.bits_at(ty, bit, w);
                    (o, Some(b))
                }
                None => (bit / 8, None),
            };
            out.push(Member {
                ty,
                off,
                bits,
                name_index: i,
            });
        }
        out
    }

    /// The direct member of `ty` that `name` is, or the anonymous member
    /// holding it: its position in `members`.
    fn member_pos(&self, ty: TypeId, ms: &[Member], name: &str) -> Option<(usize, bool)> {
        let rid = self.record_of(ty)?;
        let def = self.types.records[rid as usize].def.as_ref()?;
        for (k, m) in ms.iter().enumerate() {
            let f = &def.fields[m.name_index];
            if f.name.as_deref() == Some(name) {
                return Some((k, false));
            }
            if f.anon {
                let inner = self.record_of(f.ty.ty)?;
                if self.find_member(inner, name).is_some() {
                    return Some((k, true));
                }
            }
        }
        None
    }

    /// Fill aggregate `ty` at `off` from `list[*pos..]`: `braced`, its own
    /// list (designators apply here), else an elided brace (a designator
    /// ends it). `taken`: the first item's designators are done.
    #[allow(clippy::too_many_arguments)]
    fn fill(
        &mut self,
        ty: TypeId,
        off: u64,
        list: &[(Vec<Desig>, Init)],
        pos: &mut usize,
        braced: bool,
        mut taken: bool,
        out: &mut Vec<InitItem>,
    ) -> R<u64> {
        if let Some((elem, len)) = self.array_of(ty) {
            let et = self.unqual(elem.ty);
            let es = self.size_of(et).unwrap_or(0);
            let mut i = 0u64;
            let mut max = 0u64;
            while *pos < list.len() {
                let (desig, init) = &list[*pos];
                if !desig.is_empty() && !taken {
                    if !braced {
                        break;
                    }
                    let (lo, hi) = match &desig[0] {
                        Desig::Index(e) => {
                            let v = self.index_value(e)?;
                            (v, v)
                        }
                        Desig::Range(a, b) => (self.index_value(a)?, self.index_value(b)?),
                        Desig::Field(n, l) => {
                            return Err(self.err(
                                *l,
                                &format!("field designator `.{n}` in an array initializer"),
                            ));
                        }
                    };
                    if len.is_some_and(|n| hi >= n) {
                        return Err(self.err(
                            Self::init_loc(init),
                            "array designator index exceeds the array bounds",
                        ));
                    }
                    for k in lo..=hi {
                        self.designated(et, off + k * es, &desig[1..], init, out)?;
                    }
                    *pos += 1;
                    i = hi + 1;
                    max = max.max(i);
                    continue;
                }
                if len.is_some_and(|n| i >= n) {
                    if braced {
                        return Err(self.err(
                            Self::init_loc(init),
                            "excess elements in an array initializer",
                        ));
                    }
                    break;
                }
                taken = false;
                self.elem(et, off + i * es, None, list, pos, out)?;
                i += 1;
                max = max.max(i);
            }
            return Ok(max);
        }
        let Some(rid) = self.record_of(ty) else {
            return Err(self.err(
                list.get(*pos)
                    .map_or_else(Loc::default, |x| Self::init_loc(&x.1)),
                &format!("cannot initialize `{}` with a list", self.spell(ty)),
            ));
        };
        let union = self.types.records[rid as usize].union;
        let ms = self.members(ty);
        let mut k = 0usize;
        let mut done = 0usize;
        while *pos < list.len() {
            let (desig, init) = &list[*pos];
            if !desig.is_empty() && !taken {
                if !braced {
                    break;
                }
                let Desig::Field(name, l) = &desig[0] else {
                    return Err(self.err(
                        Self::init_loc(init),
                        "an array designator in a struct initializer",
                    ));
                };
                let Some((p, anon)) = self.member_pos(ty, &ms, name) else {
                    return Err(self.err(
                        *l,
                        &format!(
                            "field designator `{name}` does not refer to any field in `{}`",
                            self.spell(ty)
                        ),
                    ));
                };
                let m = ms[p];
                if anon {
                    self.designated(m.ty, off + m.off, desig, init, out)?;
                } else if desig.len() > 1 {
                    self.designated(m.ty, off + m.off, &desig[1..], init, out)?;
                } else {
                    let one = [(Vec::new(), init.clone())];
                    let mut q = 0;
                    self.elem(m.ty, off + m.off, m.bits, &one, &mut q, out)?;
                }
                *pos += 1;
                k = p + 1;
                done += 1;
                continue;
            }
            if k >= ms.len() || (union && done > 0) {
                if braced {
                    return Err(self.err(
                        Self::init_loc(init),
                        "excess elements in a struct initializer",
                    ));
                }
                break;
            }
            taken = false;
            let m = ms[k];
            self.elem(m.ty, off + m.off, m.bits, list, pos, out)?;
            k += 1;
            done += 1;
        }
        Ok(0)
    }

    /// `init` for the sub-object `desig` names in `ty` at `off` (all its
    /// designators; none: `ty` itself).
    fn designated(
        &mut self,
        ty: TypeId,
        off: u64,
        desig: &[Desig],
        init: &Init,
        out: &mut Vec<InitItem>,
    ) -> R<()> {
        if desig.is_empty() {
            let one = [(Vec::new(), init.clone())];
            let mut q = 0;
            return self.elem(ty, off, None, &one, &mut q, out);
        }
        match &desig[0] {
            Desig::Field(name, l) => {
                let ms = self.members(ty);
                let Some((p, anon)) = self.member_pos(ty, &ms, name) else {
                    return Err(self.err(*l, &format!("no field `{name}` in `{}`", self.spell(ty))));
                };
                let m = ms[p];
                if anon {
                    return self.designated(m.ty, off + m.off, desig, init, out);
                }
                if desig.len() == 1 {
                    let one = [(Vec::new(), init.clone())];
                    let mut q = 0;
                    return self.elem(m.ty, off + m.off, m.bits, &one, &mut q, out);
                }
                self.designated(m.ty, off + m.off, &desig[1..], init, out)
            }
            Desig::Index(e) | Desig::Range(e, _) => {
                let Some((elem, _)) = self.array_of(ty) else {
                    return Err(self.err(e.loc, "an array designator for a non-array"));
                };
                let lo = self.index_value(e)?;
                let hi = match &desig[0] {
                    Desig::Range(_, b) => self.index_value(b)?,
                    _ => lo,
                };
                let et = self.unqual(elem.ty);
                let es = self.size_of(et).unwrap_or(0);
                for k in lo..=hi {
                    self.designated(et, off + k * es, &desig[1..], init, out)?;
                }
                Ok(())
            }
        }
    }

    fn index_value(&mut self, e: &Expr) -> R<u64> {
        let v = self.value(e)?;
        match self.const_int(&v) {
            Some(x) if x >= 0 => Ok(x as u64),
            _ => Err(self.err(
                e.loc,
                "an array designator must be a non-negative integer constant",
            )),
        }
    }
}
