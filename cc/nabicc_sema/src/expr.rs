//! Expressions: types, lvalues and the implicit conversions.

use crate::check::{Cx, R, Slot};
use crate::{Arith, Bits, Cmp, Def, InitItem, InitKind, TExpr, TInit, TK};
use nabicc_lex::Loc;
use nabicc_parse::tree::{BinOp, Expr, ExprKind, UnOp};
use nabicc_parse::ty::{QualType, Type, TypeId};
use nabicc_target::{Prim, VaList};

fn bx(e: TExpr) -> Box<TExpr> {
    Box::new(e)
}

impl Cx<'_> {
    /// `e` as a value: an array decays to its first element's address,
    /// a function to its address.
    pub(crate) fn rval(&mut self, e: TExpr) -> TExpr {
        if let Some((elem, _)) = self.array_of(e.ty) {
            if let TK::Deref(_) | TK::Local(_) | TK::Global(_) | TK::Member(..) | TK::Compound(..) =
                e.kind
            {
                self.mark_addressed(&e);
            }
            let et = self.unqual(elem.ty);
            let p = self.ptr_to(et);
            let loc = e.loc;
            return TExpr::new(TK::Decay(bx(e)), p, loc);
        }
        if self.func_of(e.ty).is_some() {
            // `*fp` used as a value is `fp` again.
            if let TK::Deref(p) = e.kind {
                return *p;
            }
            let p = self.ptr_to(e.ty);
            let loc = e.loc;
            return TExpr::new(TK::AddrOf(bx(e)), p, loc);
        }
        e
    }

    pub(crate) fn value(&mut self, e: &Expr) -> R<TExpr> {
        let t = self.expr(e)?;
        Ok(self.rval(t))
    }

    /// A scalar value (a condition).
    pub(crate) fn scalar(&mut self, e: &Expr) -> R<TExpr> {
        let v = self.value(e)?;
        if !self.is_scalar(v.ty) {
            return Err(self.err(
                e.loc,
                &format!(
                    "a condition needs a number or a pointer, not `{}`",
                    self.spell(v.ty)
                ),
            ));
        }
        Ok(v)
    }

    /// An object used through its address: it lives in memory.
    pub(crate) fn mark_addressed(&mut self, e: &TExpr) {
        match &e.kind {
            TK::Local(l) => {
                if let Some(x) = self.locals.get_mut(*l as usize) {
                    x.addressed = true;
                }
            }
            TK::Member(b, ..) => self.mark_addressed(b),
            _ => {}
        }
    }

    /// Value `e` converted to `to` as by assignment.
    pub(crate) fn conv(&self, e: TExpr, to: TypeId) -> TExpr {
        let to = self.unqual(to);
        if self.types.canon(e.ty) == to {
            return e;
        }
        if self.record_of(e.ty).is_some() && self.record_of(e.ty) == self.record_of(to) {
            return e;
        }
        if let (Some(a), Some(b)) = (self.prim_of(e.ty), self.prim_of(to)) {
            if a == b {
                return TExpr { ty: to, ..e };
            }
            // Constants fold here: case labels and initializers read them.
            if let TK::Int(v) = e.kind {
                if b.is_integer() {
                    return TExpr::new(TK::Int(self.wrap(b, v)), to, e.loc);
                }
                if b.is_float() {
                    return TExpr::new(TK::Float(v as f64), to, e.loc);
                }
            }
            if let (TK::Float(v), true) = (&e.kind, b.is_float()) {
                let v = if b == Prim::Float {
                    f64::from(*v as f32)
                } else {
                    *v
                };
                return TExpr::new(TK::Float(v), to, e.loc);
            }
        }
        let loc = e.loc;
        TExpr::new(TK::Conv(bx(e)), to, loc)
    }

    /// `v` wrapped to integer type `p`.
    pub(crate) fn wrap(&self, p: Prim, v: i128) -> i128 {
        crate::fold::wrap(&self.types, p, v)
    }

    fn int(&self, v: i128, p: Prim, loc: Loc) -> TExpr {
        TExpr::new(TK::Int(self.wrap(p, v)), self.prim_ty(p), loc)
    }

    /// An arithmetic value promoted.
    fn promoted(&self, e: TExpr) -> TExpr {
        let Some(p) = self.prim_of(e.ty) else {
            return e;
        };
        let q = self.promote(p);
        let t = self.prim_ty(q);
        self.conv(e, t)
    }

    pub(crate) fn expr(&mut self, e: &Expr) -> R<TExpr> {
        let loc = e.loc;
        Ok(match &e.kind {
            ExprKind::Int(v, Prim::Void) => {
                let void = self.prim_ty(Prim::Void);
                let p = self.ptr_to(void);
                TExpr::new(TK::Int(*v), p, loc)
            }
            ExprKind::Int(v, p) => self.int(*v, *p, loc),
            ExprKind::Float(v, p) => {
                let p = if *p == Prim::LongDouble && self.ld_is_double() {
                    Prim::Double
                } else {
                    *p
                };
                TExpr::new(TK::Float(*v), self.prim_ty(p), loc)
            }
            ExprKind::Str(units, elem) => {
                let g = self.string(units, *elem, loc);
                let ty = self.globals[g].ty;
                TExpr::new(TK::Global(g), ty, loc)
            }
            ExprKind::Local(l) => match self.slots.get(*l as usize).copied() {
                Some(Slot::Local(id)) => {
                    let ty = self.locals[id as usize].ty;
                    TExpr::new(TK::Local(id), ty, loc)
                }
                Some(Slot::Global(g)) => {
                    let ty = self.globals[g].ty;
                    TExpr::new(TK::Global(g), ty, loc)
                }
                None => return Err(self.err(loc, "a local outside its function")),
            },
            ExprKind::Global(v) => {
                let g = self.var_map[*v];
                let ty = self.unqual(self.globals[g].ty);
                TExpr::new(TK::Global(g), ty, loc)
            }
            ExprKind::Func(f) => {
                let f = self.fn_map[*f];
                TExpr::new(TK::Func(f), self.funcs[f].ty, loc)
            }
            ExprKind::Builtin(name) => {
                return Err(self.err(
                    loc,
                    &format!("builtin `{name}` can only be called: write `{name}(..)`"),
                ));
            }
            ExprKind::Unary(op, a) => self.unary(*op, a, loc)?,
            ExprKind::Binary(op, a, b) => self.binary(*op, a, b, loc)?,
            ExprKind::Assign(op, a, b) => self.assign(*op, a, b, loc)?,
            ExprKind::Cond(c, a, b) => self.cond_expr(c, a.as_deref(), b, loc)?,
            ExprKind::Call(f, args) => self.call(f, args, loc)?,
            ExprKind::Index(a, i) => {
                let (a, i) = (self.value(a)?, self.value(i)?);
                let (p, i) = if self.is_ptr(a.ty) { (a, i) } else { (i, a) };
                let Some(elem) = self.pointee(p.ty) else {
                    return Err(self.err(loc, "subscripted value is not an array or pointer"));
                };
                if !self.is_int(i.ty) {
                    return Err(self.err(loc, "an array index must be an integer"));
                }
                let et = self.unqual(elem.ty);
                let step = self.step(et);
                let pd = self.prim_ty(self.types.t.ptrdiff_t);
                let i = self.conv(i, pd);
                let pty = p.ty;
                let addr = TExpr::new(TK::PtrAdd(bx(p), bx(i), step), pty, loc);
                TExpr::new(TK::Deref(bx(addr)), et, loc)
            }
            ExprKind::Member(b, name, arrow) => self.member(b, name, *arrow, loc)?,
            ExprKind::Cast(q, a) => {
                let v = self.value(a)?;
                let to = self.unqual(q.ty);
                if self.is_void(to) {
                    return Ok(TExpr::new(TK::Conv(bx(v)), to, loc));
                }
                if !self.is_scalar(to) {
                    if self.record_of(to).is_some() && self.record_of(to) == self.record_of(v.ty) {
                        return Ok(v);
                    }
                    return Err(self.err(
                        loc,
                        &format!(
                            "cannot cast to `{}`: casts convert numbers and pointers only",
                            self.spell(to)
                        ),
                    ));
                }
                if !self.is_scalar(v.ty) {
                    return Err(self.err(
                        loc,
                        &format!("cannot cast `{}` to `{}`", self.spell(v.ty), self.spell(to)),
                    ));
                }
                if self.types.canon(v.ty) == to {
                    return Ok(v);
                }
                let v = self.conv(v, to);
                if matches!(v.kind, TK::Conv(_)) || v.ty == to {
                    v
                } else {
                    TExpr::new(TK::Conv(bx(v)), to, loc)
                }
            }
            ExprKind::SizeofExpr(a) => {
                let t = self.expr(a)?;
                self.size_expr(t.ty, true, loc)?
            }
            ExprKind::SizeofType(q) => self.size_expr(q.ty, true, loc)?,
            ExprKind::AlignofExpr(a) => {
                let t = self.expr(a)?;
                self.size_expr(t.ty, false, loc)?
            }
            ExprKind::AlignofType(q) => self.size_expr(q.ty, false, loc)?,
            ExprKind::Compound(q, init) => {
                let ty = self.unqual(q.ty);
                let (ty, ti) = self.init_object(ty, init, loc)?;
                if self.file_scope {
                    let g = self.hidden_global("compound", ty, q.konst, loc);
                    self.globals[g].def = Def::Init(ti);
                    return Ok(TExpr::new(TK::Global(g), ty, loc));
                }
                let l = self.new_local("", ty, loc);
                self.locals[l as usize].addressed = true;
                let obj = TExpr::new(TK::Local(l), ty, loc);
                TExpr::new(TK::Compound(bx(obj), ti), ty, loc)
            }
            ExprKind::StmtExpr(ss) => {
                let mut out = Vec::new();
                let mut last = None;
                for (i, s) in ss.iter().enumerate() {
                    if i + 1 == ss.len() {
                        if let nabicc_parse::StmtKind::Expr(x) = &s.kind {
                            last = Some(self.value(x)?);
                            continue;
                        }
                    }
                    out.push(self.stmt(s)?);
                }
                let void = self.prim_ty(Prim::Void);
                let ty = last.as_ref().map_or(void, |l| l.ty);
                TExpr::new(TK::StmtExpr(out, last.map(bx)), ty, loc)
            }
            ExprKind::VaArg(ap, q) => {
                let a = self.va_addr(ap)?;
                let ty = self.unqual(q.ty);
                TExpr::new(TK::VaArg(bx(a)), ty, loc)
            }
            ExprKind::Generic(c, arms) => {
                let v = self.value(c)?;
                let mut pick = None;
                let mut default = None;
                for (q, x) in arms {
                    match q {
                        Some(q) => {
                            let t = self.unqual(q.ty);
                            if pick.is_none() && self.generic_match(v.ty, t) {
                                pick = Some(x);
                            }
                        }
                        None => default = Some(x),
                    }
                }
                let Some(x) = pick.or(default) else {
                    return Err(self.err(
                        loc,
                        &format!(
                            "no `_Generic` association matches `{}`: add one, or a `default:`",
                            self.spell(v.ty)
                        ),
                    ));
                };
                self.expr(x)?
            }
        })
    }

    fn generic_match(&self, a: TypeId, b: TypeId) -> bool {
        let (ca, cb) = (self.types.canon(a), self.types.canon(b));
        if ca == cb {
            return true;
        }
        match (self.prim_of(a), self.prim_of(b)) {
            (Some(x), Some(y)) => x == y,
            _ => match (self.pointee(a), self.pointee(b)) {
                (Some(x), Some(y)) => {
                    let (px, py) = (self.prim_of(x.ty), self.prim_of(y.ty));
                    (px.is_some() && px == py)
                        || (self.record_of(x.ty).is_some()
                            && self.record_of(x.ty) == self.record_of(y.ty))
                }
                _ => self.record_of(a).is_some() && self.record_of(a) == self.record_of(b),
            },
        }
    }

    fn size_expr(&self, t: TypeId, size: bool, loc: Loc) -> R<TExpr> {
        let st = self.types.t.size_t;
        let v = if self.is_void(t) || self.func_of(t).is_some() {
            1
        } else {
            match self.types.size_align(t) {
                Some((s, a)) => {
                    if size {
                        s
                    } else {
                        a
                    }
                }
                None => {
                    return Err(self.err(
                        loc,
                        &format!(
                            "`sizeof` of incomplete type `{}`: define it before this use",
                            self.spell(t)
                        ),
                    ));
                }
            }
        };
        Ok(self.int(i128::from(v), st, loc))
    }

    /// The object of string literal `units`.
    pub(crate) fn string(&mut self, units: &[u32], elem: Prim, loc: Loc) -> usize {
        if let Some(g) = self.strings.get(&(units.to_vec(), elem)) {
            return *g;
        }
        let et = self.prim_ty(elem);
        let ty = self.types.add(Type::Array {
            elem: QualType::new(et),
            len: Some(units.len() as u64 + 1),
        });
        let g = self.hidden_global("str", ty, true, loc);
        let size = self.types.t.prim(elem).0 as usize;
        let be = self.types.t.big_endian;
        let mut bytes = Vec::with_capacity((units.len() + 1) * size);
        for u in units.iter().chain([&0]) {
            let b = u.to_le_bytes();
            if be {
                bytes.extend(b[..size].iter().rev());
            } else {
                bytes.extend(&b[..size]);
            }
        }
        self.globals[g].def = Def::Init(TInit {
            items: vec![InitItem {
                off: 0,
                kind: InitKind::Bytes(bytes),
            }],
        });
        self.strings.insert((units.to_vec(), elem), g);
        g
    }

    /// The address of `va_list` `ap`.
    pub(crate) fn va_addr(&mut self, ap: &Expr) -> R<TExpr> {
        let e = self.expr(ap)?;
        let array = matches!(self.types.t.va_list, VaList::X86_64 | VaList::S390x);
        if self.array_of(e.ty).is_some() {
            return Ok(self.rval(e));
        }
        if array && self.is_ptr(e.ty) {
            // A `va_list` parameter: decayed already.
            return Ok(e);
        }
        if !e.is_lvalue() {
            return Err(self.err(ap.loc, "expected a `va_list` variable"));
        }
        self.mark_addressed(&e);
        let p = self.ptr_to(e.ty);
        let loc = e.loc;
        Ok(TExpr::new(TK::AddrOf(bx(e)), p, loc))
    }

    fn unary(&mut self, op: UnOp, a: &Expr, loc: Loc) -> R<TExpr> {
        Ok(match op {
            UnOp::Neg | UnOp::Plus | UnOp::BitNot => {
                let v = self.value(a)?;
                let ok = if op == UnOp::BitNot {
                    self.is_int(v.ty)
                } else {
                    self.is_arith(v.ty)
                };
                if !ok {
                    return Err(self.err(
                        loc,
                        &format!(
                            "invalid operand `{}` for a unary operator",
                            self.spell(v.ty)
                        ),
                    ));
                }
                let v = self.promoted(v);
                let ty = v.ty;
                match (op, &v.kind) {
                    (UnOp::Plus, _) => v,
                    (UnOp::Neg, TK::Int(x)) => {
                        let p = self.prim_of(ty).unwrap_or(Prim::Int);
                        self.int(-x, p, loc)
                    }
                    (UnOp::Neg, TK::Float(x)) => TExpr::new(TK::Float(-x), ty, loc),
                    (UnOp::BitNot, TK::Int(x)) => {
                        let p = self.prim_of(ty).unwrap_or(Prim::Int);
                        self.int(!x, p, loc)
                    }
                    (UnOp::Neg, _) => TExpr::new(TK::Neg(bx(v)), ty, loc),
                    _ => TExpr::new(TK::BitNot(bx(v)), ty, loc),
                }
            }
            UnOp::Not => {
                let v = self.scalar(a)?;
                if let TK::Int(x) = v.kind {
                    return Ok(self.int(i128::from(x == 0), Prim::Int, loc));
                }
                TExpr::new(TK::Not(bx(v)), self.int_ty(), loc)
            }
            UnOp::Deref => {
                let v = self.value(a)?;
                let Some(q) = self.pointee(v.ty) else {
                    return Err(self.err(
                        loc,
                        &format!(
                            "cannot dereference `{}`: it is not a pointer",
                            self.spell(v.ty)
                        ),
                    ));
                };
                let t = self.unqual(q.ty);
                TExpr::new(TK::Deref(bx(v)), t, loc)
            }
            UnOp::Addr => {
                let e = self.expr(a)?;
                if let TK::Func(_) = e.kind {
                    let p = self.ptr_to(e.ty);
                    return Ok(TExpr::new(TK::AddrOf(bx(e)), p, loc));
                }
                if !e.is_lvalue() {
                    return Err(self.err(loc, "cannot take the address of an rvalue"));
                }
                if let TK::Member(_, _, Some(_)) = e.kind {
                    return Err(self.err(
                        loc,
                        "cannot take the address of a bitfield; copy it into a variable first",
                    ));
                }
                let p = self.ptr_to(e.ty);
                if let TK::Deref(inner) = e.kind {
                    return Ok(TExpr { ty: p, ..*inner });
                }
                self.mark_addressed(&e);
                TExpr::new(TK::AddrOf(bx(e)), p, loc)
            }
            UnOp::PreInc | UnOp::PreDec | UnOp::PostInc | UnOp::PostDec => {
                let place = self.place(a)?;
                let step = match self.pointee(place.ty) {
                    Some(q) => {
                        let t = self.unqual(q.ty);
                        self.step(t)
                    }
                    None if self.is_arith(place.ty) => 1,
                    None => {
                        return Err(
                            self.err(loc, &format!("cannot increment `{}`", self.spell(place.ty)))
                        );
                    }
                };
                let pre = matches!(op, UnOp::PreInc | UnOp::PreDec);
                let inc = matches!(op, UnOp::PreInc | UnOp::PostInc);
                let ty = place.ty;
                TExpr::new(TK::IncDec(bx(place), pre, inc, step), ty, loc)
            }
            UnOp::Real | UnOp::Imag => {
                return Err(self.err(
                    loc,
                    "`_Complex` numbers are not supported by `nabi cc` yet; keep the real and \
                     imaginary parts in two variables",
                ));
            }
        })
    }

    /// A modifiable lvalue.
    pub(crate) fn place(&mut self, a: &Expr) -> R<TExpr> {
        let e = self.expr(a)?;
        if !e.is_lvalue() {
            return Err(self.err(a.loc, "expression is not assignable"));
        }
        if self.array_of(e.ty).is_some() {
            return Err(self.err(
                a.loc,
                "an array cannot be assigned; copy it with `memcpy(dst, src, sizeof dst)`",
            ));
        }
        Ok(e)
    }

    fn binary(&mut self, op: BinOp, a: &Expr, b: &Expr, loc: Loc) -> R<TExpr> {
        match op {
            BinOp::Comma => {
                let x = self.value(a)?;
                let y = self.value(b)?;
                let ty = y.ty;
                return Ok(TExpr::new(TK::Comma(bx(x), bx(y)), ty, loc));
            }
            BinOp::And | BinOp::Or => {
                let x = self.scalar(a)?;
                let y = self.scalar(b)?;
                if let (TK::Int(p), TK::Int(q)) = (&x.kind, &y.kind) {
                    let r = if op == BinOp::And {
                        *p != 0 && *q != 0
                    } else {
                        *p != 0 || *q != 0
                    };
                    return Ok(self.int(i128::from(r), Prim::Int, loc));
                }
                let k = if op == BinOp::And {
                    TK::And(bx(x), bx(y))
                } else {
                    TK::Or(bx(x), bx(y))
                };
                return Ok(TExpr::new(k, self.int_ty(), loc));
            }
            _ => {}
        }
        let x = self.value(a)?;
        let y = self.value(b)?;
        self.arith(op, x, y, loc)
    }

    /// `x op y` of values.
    pub(crate) fn arith(&self, op: BinOp, x: TExpr, y: TExpr, loc: Loc) -> R<TExpr> {
        let cmp = match op {
            BinOp::Lt => Some(Cmp::Lt),
            BinOp::Gt => Some(Cmp::Gt),
            BinOp::Le => Some(Cmp::Le),
            BinOp::Ge => Some(Cmp::Ge),
            BinOp::Eq => Some(Cmp::Eq),
            BinOp::Ne => Some(Cmp::Ne),
            _ => None,
        };
        let (xp, yp) = (self.is_ptr(x.ty), self.is_ptr(y.ty));
        if let Some(c) = cmp {
            if xp || yp {
                // Pointers compare as addresses; an integer (null) becomes one.
                let pt = if xp { x.ty } else { y.ty };
                let x = self.conv(x, pt);
                let y = self.conv(y, pt);
                return Ok(TExpr::new(TK::Cmp(c, bx(x), bx(y)), self.int_ty(), loc));
            }
            if !self.is_arith(x.ty) || !self.is_arith(y.ty) {
                return Err(self.bad_operands(&x, &y, loc));
            }
            let (px, py) = (
                self.prim_of(x.ty).unwrap_or(Prim::Int),
                self.prim_of(y.ty).unwrap_or(Prim::Int),
            );
            let ct = self.prim_ty(self.common(px, py));
            let x = self.conv(x, ct);
            let y = self.conv(y, ct);
            if let (TK::Int(p), TK::Int(q)) = (&x.kind, &y.kind) {
                let r = match c {
                    Cmp::Eq => p == q,
                    Cmp::Ne => p != q,
                    Cmp::Lt => p < q,
                    Cmp::Le => p <= q,
                    Cmp::Gt => p > q,
                    Cmp::Ge => p >= q,
                };
                return Ok(self.int(i128::from(r), Prim::Int, loc));
            }
            return Ok(TExpr::new(TK::Cmp(c, bx(x), bx(y)), self.int_ty(), loc));
        }
        let ar = match op {
            BinOp::Mul => Arith::Mul,
            BinOp::Div => Arith::Div,
            BinOp::Rem => Arith::Rem,
            BinOp::Add => Arith::Add,
            BinOp::Sub => Arith::Sub,
            BinOp::Shl => Arith::Shl,
            BinOp::Shr => Arith::Shr,
            BinOp::BitAnd => Arith::And,
            BinOp::BitXor => Arith::Xor,
            BinOp::BitOr => Arith::Or,
            _ => return Err(self.err(loc, "unexpected operator")),
        };
        if xp || yp {
            let pd = self.prim_ty(self.types.t.ptrdiff_t);
            if ar == Arith::Sub && xp && yp {
                let t = self.pointee(x.ty).map_or(x.ty, |q| q.ty);
                let t = self.unqual(t);
                let step = self.step(t);
                return Ok(TExpr::new(TK::PtrDiff(bx(x), bx(y), step), pd, loc));
            }
            if matches!(ar, Arith::Add | Arith::Sub) {
                let (p, i) = if xp { (x, y) } else { (y, x) };
                if !self.is_int(i.ty) || (ar == Arith::Sub && !xp) {
                    return Err(self.bad_operands(&p, &i, loc));
                }
                let t = self.pointee(p.ty).map_or(p.ty, |q| q.ty);
                let t = self.unqual(t);
                let step = self.step(t);
                let mut i = self.conv(i, pd);
                if ar == Arith::Sub {
                    i = TExpr::new(TK::Neg(bx(i)), pd, loc);
                }
                let pty = p.ty;
                return Ok(TExpr::new(TK::PtrAdd(bx(p), bx(i), step), pty, loc));
            }
            return Err(self.bad_operands(&x, &y, loc));
        }
        let int_only = matches!(
            ar,
            Arith::Rem | Arith::Shl | Arith::Shr | Arith::And | Arith::Or | Arith::Xor
        );
        let ok = if int_only {
            self.is_int(x.ty) && self.is_int(y.ty)
        } else {
            self.is_arith(x.ty) && self.is_arith(y.ty)
        };
        if !ok {
            return Err(self.bad_operands(&x, &y, loc));
        }
        let (x, y, ty) = if matches!(ar, Arith::Shl | Arith::Shr) {
            let x = self.promoted(x);
            let y = self.promoted(y);
            let ty = x.ty;
            (x, y, ty)
        } else {
            let px = self.prim_of(x.ty).unwrap_or(Prim::Int);
            let py = self.prim_of(y.ty).unwrap_or(Prim::Int);
            let ct = self.prim_ty(self.common(px, py));
            (self.conv(x, ct), self.conv(y, ct), ct)
        };
        if let Some(v) = self.fold_arith(ar, &x, &y, ty) {
            return Ok(TExpr::new(v, ty, loc));
        }
        Ok(TExpr::new(TK::Bin(ar, bx(x), bx(y)), ty, loc))
    }

    /// `x ar y` of constants.
    fn fold_arith(&self, ar: Arith, x: &TExpr, y: &TExpr, ty: TypeId) -> Option<TK> {
        let p = self.prim_of(ty)?;
        match (&x.kind, &y.kind) {
            (TK::Int(a), TK::Int(b)) => {
                let (a, b) = (*a, *b);
                let bits = self.types.t.bits(p);
                let unsigned = !self.types.t.signed(p);
                let ua = |v: i128| {
                    if unsigned {
                        v & ((1i128 << bits.min(127)) - 1)
                    } else {
                        v
                    }
                };
                let v = match ar {
                    Arith::Add => a.wrapping_add(b),
                    Arith::Sub => a.wrapping_sub(b),
                    Arith::Mul => a.wrapping_mul(b),
                    Arith::Div if b != 0 => ua(a) / ua(b),
                    Arith::Rem if b != 0 => ua(a) % ua(b),
                    Arith::Div | Arith::Rem => return None,
                    Arith::Shl => {
                        if !(0..i128::from(bits)).contains(&b) {
                            return None;
                        }
                        a.wrapping_shl(b as u32)
                    }
                    Arith::Shr => {
                        if !(0..i128::from(bits)).contains(&b) {
                            return None;
                        }
                        ua(a) >> b
                    }
                    Arith::And => a & b,
                    Arith::Or => a | b,
                    Arith::Xor => a ^ b,
                };
                Some(TK::Int(self.wrap(p, v)))
            }
            (TK::Float(a), TK::Float(b)) => {
                let v = match ar {
                    Arith::Add => a + b,
                    Arith::Sub => a - b,
                    Arith::Mul => a * b,
                    Arith::Div => a / b,
                    _ => return None,
                };
                let v = if p == Prim::Float {
                    f64::from(v as f32)
                } else {
                    v
                };
                Some(TK::Float(v))
            }
            _ => None,
        }
    }

    fn bad_operands(&self, x: &TExpr, y: &TExpr, loc: Loc) -> nabicc_pp::Error {
        self.err(
            loc,
            &format!(
                "invalid operands to a binary expression (`{}` and `{}`)",
                self.spell(x.ty),
                self.spell(y.ty)
            ),
        )
    }

    fn assign(&mut self, op: Option<BinOp>, a: &Expr, b: &Expr, loc: Loc) -> R<TExpr> {
        let place = self.place(a)?;
        let v = self.value(b)?;
        let ty = place.ty;
        let Some(op) = op else {
            if self.record_of(ty).is_some() {
                if self.record_of(v.ty) != self.record_of(ty) {
                    return Err(self.err(
                        loc,
                        &format!(
                            "assigning `{}` to `{}`: the types differ",
                            self.spell(v.ty),
                            self.spell(ty)
                        ),
                    ));
                }
                return Ok(TExpr::new(TK::Assign(bx(place), bx(v)), ty, loc));
            }
            if !self.is_scalar(v.ty) {
                return Err(self.err(
                    loc,
                    &format!("assigning `{}` to `{}`", self.spell(v.ty), self.spell(ty)),
                ));
            }
            let v = self.conv(v, ty);
            return Ok(TExpr::new(TK::Assign(bx(place), bx(v)), ty, loc));
        };
        if self.is_ptr(ty) && matches!(op, BinOp::Add | BinOp::Sub) {
            if !self.is_int(v.ty) {
                return Err(self.bad_operands(&place, &v, loc));
            }
            let t = self.pointee(ty).map_or(ty, |q| q.ty);
            let t = self.unqual(t);
            let step = self.step(t);
            let pd = self.prim_ty(self.types.t.ptrdiff_t);
            let v = self.conv(v, pd);
            return Ok(TExpr::new(
                TK::PtrAssign(bx(place), bx(v), step, op == BinOp::Sub),
                ty,
                loc,
            ));
        }
        let ar = match op {
            BinOp::Mul => Arith::Mul,
            BinOp::Div => Arith::Div,
            BinOp::Rem => Arith::Rem,
            BinOp::Add => Arith::Add,
            BinOp::Sub => Arith::Sub,
            BinOp::Shl => Arith::Shl,
            BinOp::Shr => Arith::Shr,
            BinOp::BitAnd => Arith::And,
            BinOp::BitXor => Arith::Xor,
            _ => Arith::Or,
        };
        if !self.is_arith(ty) || !self.is_arith(v.ty) {
            return Err(self.bad_operands(&place, &v, loc));
        }
        let pp = self.prim_of(ty).unwrap_or(Prim::Int);
        let (v, op_ty) = if matches!(ar, Arith::Shl | Arith::Shr) {
            let v = self.promoted(v);
            (v, self.prim_ty(self.promote(pp)))
        } else {
            let pv = self.prim_of(v.ty).unwrap_or(Prim::Int);
            let ct = self.prim_ty(self.common(pp, pv));
            (self.conv(v, ct), ct)
        };
        Ok(TExpr::new(
            TK::OpAssign(ar, bx(place), bx(v), op_ty),
            ty,
            loc,
        ))
    }

    fn cond_expr(&mut self, c: &Expr, a: Option<&Expr>, b: &Expr, loc: Loc) -> R<TExpr> {
        let cv = self.scalar(c)?;
        let (cv, av) = match a {
            Some(a) => (cv, self.value(a)?),
            None if self.file_scope => (cv.clone(), cv),
            None => {
                // `c ?: b`: `c` once.
                let t = self.new_local("", cv.ty, loc);
                let tl = TExpr::new(TK::Local(t), cv.ty, loc);
                let ty = cv.ty;
                let set = TExpr::new(TK::Assign(bx(tl.clone()), bx(cv)), ty, loc);
                let use_ = TExpr::new(TK::Comma(bx(set), bx(tl.clone())), ty, loc);
                (use_, tl)
            }
        };
        let bv = self.value(b)?;
        let ty = if self.is_arith(av.ty) && self.is_arith(bv.ty) {
            let pa = self.prim_of(av.ty).unwrap_or(Prim::Int);
            let pb = self.prim_of(bv.ty).unwrap_or(Prim::Int);
            self.prim_ty(self.common(pa, pb))
        } else if self.is_ptr(av.ty) && self.is_ptr(bv.ty) {
            // A `void *` arm wins, as C's composite type.
            let av_void = self.pointee(av.ty).is_some_and(|q| self.is_void(q.ty));
            if av_void { av.ty } else { bv.ty }
        } else if self.is_ptr(av.ty) {
            av.ty
        } else if self.is_ptr(bv.ty) {
            bv.ty
        } else if self.is_void(av.ty) || self.is_void(bv.ty) {
            self.prim_ty(Prim::Void)
        } else if self.record_of(av.ty).is_some() && self.record_of(av.ty) == self.record_of(bv.ty)
        {
            av.ty
        } else {
            return Err(self.err(
                loc,
                &format!(
                    "the arms of `?:` have incompatible types `{}` and `{}`",
                    self.spell(av.ty),
                    self.spell(bv.ty)
                ),
            ));
        };
        let void = self.is_void(ty);
        let (av, bv) = if void || self.record_of(ty).is_some() {
            (av, bv)
        } else {
            (self.conv(av, ty), self.conv(bv, ty))
        };
        if let TK::Int(x) = cv.kind {
            return Ok(if x != 0 { av } else { bv });
        }
        Ok(TExpr::new(TK::Cond(bx(cv), bx(av), bx(bv)), ty, loc))
    }

    fn call(&mut self, f: &Expr, args: &[Expr], loc: Loc) -> R<TExpr> {
        if let ExprKind::Builtin(name) = &f.kind {
            return self.builtin(name, args, loc);
        }
        let callee = self.expr(f)?;
        // MSVC's intrinsics, declared as functions by its headers.
        if let TK::Func(id) = callee.kind {
            let name = self.funcs[id].name.clone();
            if self.funcs[id].body.is_none() {
                if name.starts_with("_Interlocked") {
                    return self.builtin(&name, args, loc);
                }
                if let Some(b) = crate::builtin::msvc_intrinsic(&name) {
                    return self.builtin(b, args, loc);
                }
            }
        }
        // MSVC's `va_start`: `__va_start(&ap, last)`.
        if let TK::Func(id) = callee.kind {
            if self.funcs[id].name == "__va_start" && !args.is_empty() {
                if !self.variadic {
                    return Err(self.err(
                        loc,
                        "`va_start` in a function without `...`: declare it variadic, \
                         `int f(int n, ...)`",
                    ));
                }
                let a = self.value(&args[0])?;
                let void = self.prim_ty(Prim::Void);
                return Ok(TExpr::new(TK::VaStart(bx(a)), void, loc));
            }
        }
        let callee = if let TK::Func(_) = callee.kind {
            callee
        } else {
            self.rval(callee)
        };
        let fty = if let TK::Func(_) = callee.kind {
            callee.ty
        } else {
            match self.pointee(callee.ty) {
                Some(q) if self.func_of(q.ty).is_some() => self.types.canon(q.ty),
                _ => {
                    return Err(self.err(
                        loc,
                        &format!(
                            "called object type `{}` is not a function",
                            self.spell(callee.ty)
                        ),
                    ));
                }
            }
        };
        let Some(ft) = self.func_of(fty) else {
            return Err(self.err(loc, "called object is not a function"));
        };
        if ft.proto && args.len() < ft.params.len()
            || ft.proto && !ft.variadic && args.len() > ft.params.len()
        {
            return Err(self.err(
                loc,
                &format!(
                    "this call passes {} arguments, the function takes {}{}",
                    args.len(),
                    ft.params.len(),
                    if ft.variadic { " or more" } else { "" }
                ),
            ));
        }
        let mut out = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let v = self.value(a)?;
            let v = match ft.params.get(i).filter(|_| ft.proto) {
                Some(p) => {
                    let pt = self.unqual(p.ty);
                    if self.record_of(pt).is_some() {
                        if self.record_of(pt) != self.record_of(v.ty) {
                            return Err(self.err(
                                a.loc,
                                &format!(
                                    "passing `{}` where `{}` is expected",
                                    self.spell(v.ty),
                                    self.spell(pt)
                                ),
                            ));
                        }
                        v
                    } else {
                        if !self.is_scalar(v.ty) {
                            return Err(self.err(
                                a.loc,
                                &format!(
                                    "passing `{}` where `{}` is expected",
                                    self.spell(v.ty),
                                    self.spell(pt)
                                ),
                            ));
                        }
                        self.conv(v, pt)
                    }
                }
                None => self.default_promote(v),
            };
            out.push(v);
        }
        let ret = self.unqual(ft.ret.ty);
        Ok(TExpr::new(TK::Call(bx(callee), out, fty), ret, loc))
    }

    /// The default argument promotions (variadic and unprototyped
    /// arguments).
    pub(crate) fn default_promote(&self, v: TExpr) -> TExpr {
        match self.prim_of(v.ty) {
            Some(Prim::Float) => {
                let d = self.prim_ty(Prim::Double);
                self.conv(v, d)
            }
            Some(p) if p.is_integer() => self.promoted(v),
            _ => v,
        }
    }

    fn member(&mut self, b: &Expr, name: &str, arrow: bool, loc: Loc) -> R<TExpr> {
        let base = if arrow {
            let v = self.value(b)?;
            let Some(q) = self.pointee(v.ty) else {
                return Err(self.err(
                    loc,
                    &format!(
                        "`->` needs a pointer to a struct, not `{}`; write `.{name}`",
                        self.spell(v.ty)
                    ),
                ));
            };
            let t = self.unqual(q.ty);
            TExpr::new(TK::Deref(bx(v)), t, loc)
        } else {
            self.expr(b)?
        };
        let Some(rid) = self.record_of(base.ty) else {
            let hint = if self.is_ptr(base.ty) {
                format!("; it is a pointer: write `->{name}`")
            } else {
                String::new()
            };
            return Err(self.err(
                loc,
                &format!(
                    "member reference base type `{}` is not a struct or union{hint}",
                    self.spell(base.ty)
                ),
            ));
        };
        let Some((fq, bit_off, width)) = self.find_member(rid, name) else {
            return Err(self.err(
                loc,
                &format!("no member named '{name}' in {}", self.spell(base.ty)),
            ));
        };
        let ft = self.unqual(fq.ty);
        let (off, bits) = match width {
            None => (bit_off / 8, None),
            Some(w) => {
                let (off, b) = self.bits_at(ft, bit_off, w);
                (off, Some(b))
            }
        };
        Ok(TExpr::new(TK::Member(bx(base), off, bits), ft, loc))
    }

    /// Member `name` of record `rid`: its type, bit offset and bitfield
    /// width, through anonymous members.
    pub(crate) fn find_member(&self, rid: u32, name: &str) -> Option<(QualType, u64, Option<u64>)> {
        let rec = &self.types.records[rid as usize];
        let def = rec.def.as_ref()?;
        let layout = self.types.layout(rid)?;
        for (i, f) in def.fields.iter().enumerate() {
            let off = layout.offsets[i];
            if f.name.as_deref() == Some(name) {
                return Some((f.ty, off, f.bits));
            }
            if f.anon {
                if let Some(r) = self.record_of(f.ty.ty) {
                    if let Some((t, o, b)) = self.find_member(r, name) {
                        return Some((t, off + o, b));
                    }
                }
            }
        }
        None
    }

    /// Where bitfield `width` bits at bit `bit_off` of type `ft` are read:
    /// the storage unit's byte offset and the bits in it.
    pub(crate) fn bits_at(&self, ft: TypeId, bit_off: u64, width: u64) -> (u64, Bits) {
        let t = &self.types.t;
        let p = self.prim_of(ft).unwrap_or(Prim::UInt);
        let signed = t.signed(p) && p != Prim::Bool;
        let mut unit = self.size_of(ft).unwrap_or(4).clamp(1, 8);
        let mut start = bit_off / 8 / unit * unit;
        if bit_off - start * 8 + width > unit * 8 {
            // Straddling its unit (a packed record): the bytes it spans.
            start = bit_off / 8;
            let need = bit_off % 8 + width;
            unit = need.div_ceil(8).next_power_of_two().min(8);
        }
        let in_unit = (bit_off - start * 8) as u32;
        let at = if t.big_endian {
            (unit * 8) as u32 - in_unit - width as u32
        } else {
            in_unit
        };
        (
            start,
            Bits {
                at,
                width: width as u32,
                signed,
                unit: unit as u32,
            },
        )
    }
}
