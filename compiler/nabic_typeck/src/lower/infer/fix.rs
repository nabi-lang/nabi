//! Literal scopes of a body, and fixing its types once solved.

#[allow(clippy::wildcard_imports)]
use crate::lower::infer::*;

/// A body whose unsuffixed literals are integer / float variables: they
/// unify with their uses as the body is lowered, and at its end each takes
/// what it was fixed to, or its default (`close_scope`).
pub(crate) struct Scope {
    outer: Option<Span>,
    lits: usize,
    insts: usize,
    extras: usize,
}

/// Open the literal scope of a body: every unsuffixed literal lowered
/// until it closes is a variable of it (parameter defaults and field
/// defaults the body fills in too).
pub(crate) fn open_scope(env: &Env<'_>, at: Span) -> Scope {
    let sink = &env.sink;
    let s = Scope {
        outer: sink.lit_open.get(),
        lits: sink.lit_vars.borrow().len(),
        insts: sink.open_insts.borrow().len(),
        extras: sink.extras.borrow().len(),
    };
    sink.lit_open.set(Some(at));
    sink.infer_depth.set(sink.infer_depth.get() + 1);
    s
}

/// End the scope: every type in `f`, its closures and `ret_tys` is
/// resolved, the instances asked for over open types are made for the
/// solved ones, and literals take their types.
pub(crate) fn close_scope(
    ctx: &LowerCtx<'_>,
    s: Scope,
    f: &mut HirFunction,
    ret_tys: Option<&mut Vec<HirTy>>,
) {
    let sink = &ctx.env.sink;
    // A probe of a body over open arguments is undone; what it returns
    // may name them.
    if sink.implicit_depth.get() == 0 {
        fix_body(ctx, &s, Root::Fn(f), ret_tys);
    } else if let Some(tys) = ret_tys {
        for t in tys.iter_mut() {
            *t = resolve(ctx, t);
        }
    }
    sink.lit_vars.borrow_mut().truncate(s.lits);
    sink.open_insts.borrow_mut().truncate(s.insts);
    sink.infer_depth.set(sink.infer_depth.get() - 1);
    sink.lit_open.set(s.outer);
}

/// A constant expression (a `const` item's value, a field or parameter
/// default) is its own scope: its literals are solved before it is
/// evaluated.
pub(crate) fn scoped(
    ctx: &mut LowerCtx<'_>,
    lower: impl FnOnce(&mut LowerCtx<'_>) -> HirExpr,
) -> HirExpr {
    let s = open_scope(ctx.env, ctx.here());
    let mut e = lower(ctx);
    let sink = &ctx.env.sink;
    if sink.implicit_depth.get() == 0 {
        fix_body(ctx, &s, Root::Expr(&mut e), None);
    }
    sink.lit_vars.borrow_mut().truncate(s.lits);
    sink.open_insts.borrow_mut().truncate(s.insts);
    sink.infer_depth.set(sink.infer_depth.get() - 1);
    sink.lit_open.set(s.outer);
    e
}

/// What a scope lowered.
pub(crate) enum Root<'r> {
    Fn(&'r mut HirFunction),
    Expr(&'r mut HirExpr),
}

pub(crate) fn fix_body(
    ctx: &LowerCtx<'_>,
    s: &Scope,
    mut root: Root<'_>,
    ret_tys: Option<&mut Vec<HirTy>>,
) {
    let sink = &ctx.env.sink;
    // Instances the body calls, made for the solved arguments.
    let mut insts: HashMap<FunctionId, Option<FunctionId>> = HashMap::default();
    {
        let pending: nabic_core::HashSet<FunctionId> = sink.open_insts.borrow()[s.insts..]
            .iter()
            .map(|p| p.id)
            .collect();
        let mut seen = |b: &HirBlock| {
            crate::each_block(b, &mut |e| {
                if let Some(id) = callee_of(e).filter(|id| pending.contains(id)) {
                    insts.insert(id, None);
                }
            });
        };
        match &root {
            Root::Fn(f) => {
                if let Some(b) = &f.body {
                    seen(b);
                }
            }
            Root::Expr(e) => seen(&HirBlock {
                stmts: Vec::new(),
                trailing: Some((**e).clone()),
                label: None,
            }),
        }
        for x in &sink.extras.borrow()[s.extras..] {
            if let Some(b) = &x.body {
                seen(b);
            }
        }
    }
    let wanted: Vec<crate::lower::mono::OpenInst> = sink.open_insts.borrow()[s.insts..]
        .iter()
        .filter(|p| insts.contains_key(&p.id))
        .cloned()
        .collect();
    let made_from = sink.extras.borrow().len();
    for p in wanted {
        let real = crate::lower::mono::solve_instance(ctx, &p);
        insts.insert(p.id, real);
    }
    let fix = Fixer { ctx, insts: &insts };
    match &mut root {
        Root::Fn(f) => fix.function(f),
        Root::Expr(e) => {
            let mut b = HirBlock {
                stmts: Vec::new(),
                trailing: Some(std::mem::replace(*e, poison(e.span))),
                label: None,
            };
            fix.block(&mut b);
            if let Some(t) = b.trailing {
                **e = t;
            }
        }
    }
    {
        let mut extras = sink.extras.borrow_mut();
        let end = made_from.min(extras.len());
        let mut own: Vec<HirFunction> = extras.drain(s.extras..end).collect();
        drop(extras);
        for x in &mut own {
            fix.function(x);
        }
        sink.extras.borrow_mut().splice(s.extras..s.extras, own);
    }
    if let Some(tys) = ret_tys {
        for t in tys.iter_mut() {
            *t = zonk(ctx, t);
        }
    }
}

/// The declared default the literal variable `v` holds, if any.
fn default_of(ctx: &LowerCtx<'_>, v: u32) -> Option<(Span, String)> {
    let r = root(ctx, v as usize);
    let vars = ctx.env.sink.infer_vars.borrow();
    vars[r].default_from.clone()
}

/// What to write for a literal out of range for a declared default
/// (the default of `Buf`'s `T`): the type, or a suffix.
fn range_help(
    ctx: &LowerCtx<'_>,
    (at, origin): (Span, &str),
    (src, value): (&str, i128),
    to: PrimTy,
) -> String {
    let owner = origin.split('`').nth(1).unwrap_or("..");
    let order: &[PrimTy] = if to.is_signed_int() {
        &[PrimTy::I16, PrimTy::I32, PrimTy::I64]
    } else {
        &[PrimTy::U16, PrimTy::U32, PrimTy::U64]
    };
    let wider = order
        .iter()
        .find(|p| crate::lower::pattern::int_fits(value, **p))
        .map_or("i64", |p| p.name());
    let path = format!("{owner}::");
    let typed = match ctx.src(at) {
        Some(call) if call.starts_with(&path) => {
            call.replacen(&path, &format!("{owner}::<{wider}>::"), 1)
        }
        _ => format!("{owner}<{wider}>"),
    };
    format!("write the type: `{typed}`, or `{src}i64`")
}

pub(crate) const fn callee_of(e: &HirExpr) -> Option<FunctionId> {
    match &e.kind {
        HirExprKind::Call { callee: id, .. }
        | HirExprKind::FnRef(id)
        | HirExprKind::ClosureValue { fn_id: id, .. }
        | HirExprKind::CoroNew { body_fn: id, .. } => Some(*id),
        _ => None,
    }
}

/// Resolves the types of a lowered body.
pub(crate) struct Fixer<'c, 'a> {
    ctx: &'c LowerCtx<'a>,
    insts: &'c HashMap<FunctionId, Option<FunctionId>>,
}

impl Fixer<'_, '_> {
    pub(crate) fn ty(&self, t: &mut HirTy) {
        if has_var(self.ctx, t) {
            *t = zonk(self.ctx, t);
        }
    }

    pub(crate) fn struct_id(&self, id: &mut StructId) {
        if let HirTy::Struct(n) = zonk(self.ctx, &HirTy::Struct(*id)) {
            *id = n;
        }
    }

    pub(crate) fn enum_id(&self, id: &mut EnumId) {
        if let HirTy::Enum(n) = zonk(self.ctx, &HirTy::Enum(*id)) {
            *id = n;
        }
    }

    /// An f-string hole over an open literal: the runtime call is told
    /// the solved type (`nabi_fmt_int`'s sign and width, `nabi_fmt_f64`'s
    /// `f32`).
    pub(crate) fn fmt_call(&self, e: &mut HirExpr) {
        let HirExprKind::Call { callee, args } = &mut e.kind else {
            return;
        };
        let Some(HirExprKind::Cast { operand, .. }) = args.get(3).map(|a| &a.kind) else {
            return;
        };
        if !matches!(operand.ty, HirTy::Var(_)) {
            return;
        }
        let HirTy::Prim(p) = zonk(self.ctx, &operand.ty) else {
            return;
        };
        let name = self.ctx.env.decl(*callee).map(|d| d.def().name.clone());
        let int = |v: u64, ty: PrimTy, span| HirExpr {
            kind: HirExprKind::Const(ConstVal::Int {
                value: i128::from(v),
                ty,
            }),
            ty: HirTy::Prim(ty),
            span,
        };
        let set = |a: &mut HirExpr, v: u64| {
            if let HirTy::Prim(ty) = a.ty {
                *a = int(v, ty, a.span);
            }
        };
        match name.as_deref().map(leaf_name) {
            Some("nabi_fmt_int") if args.len() == 6 => {
                set(&mut args[4], u64::from(p.is_signed_int()));
                set(
                    &mut args[5],
                    u64::from(p.bit_width(crate::lower::consteval::pointer_width())),
                );
            }
            Some("nabi_fmt_f64") if args.len() == 5 => {
                set(&mut args[4], u64::from(p == PrimTy::F32));
            }
            _ => {}
        }
    }

    /// A sum's members are in canonical order: a solved member may move.
    pub(crate) fn sum_member(&self, sum_ty: &mut HirTy, index: &mut u32) {
        if !has_var(self.ctx, sum_ty) {
            return;
        }
        let member = match &*sum_ty {
            HirTy::Sum(ms) => ms.get(*index as usize).map(|m| zonk(self.ctx, m)),
            _ => None,
        };
        self.ty(sum_ty);
        if let (Some(m), HirTy::Sum(ms)) = (member, &*sum_ty) {
            if let Some(i) = ms.iter().position(|n| *n == m) {
                *index = u32::try_from(i).expect("variant overflow");
            }
        }
    }

    pub(crate) fn function(&self, f: &mut HirFunction) {
        for p in &mut f.params {
            self.ty(&mut p.ty);
        }
        for l in &mut f.locals {
            self.ty(&mut l.ty);
        }
        self.ty(&mut f.ret_ty);
        if let Some(b) = &mut f.body {
            self.block(b);
        }
    }

    pub(crate) fn block(&self, b: &mut HirBlock) {
        self.stmts(b);
        crate::each_block_mut(b, &mut |e| self.expr(e));
    }

    /// The statements of `b` itself; the expression walk reaches the
    /// nested blocks.
    pub(crate) fn stmts(&self, b: &mut HirBlock) {
        for s in &mut b.stmts {
            if let HirStmt::ArrayAssign { elem_ty, .. } = s {
                self.ty(elem_ty);
            }
        }
    }

    pub(crate) fn expr(&self, e: &mut HirExpr) {
        self.fmt_call(e);
        let origin = match (&e.kind, &e.ty) {
            (HirExprKind::Const(_), HirTy::Var(v)) => default_of(self.ctx, *v),
            _ => None,
        };
        self.ty(&mut e.ty);
        if let HirExprKind::Block(b)
        | HirExprKind::Loop { body: b, .. }
        | HirExprKind::While { body: b, .. }
        | HirExprKind::If { then_block: b, .. } = &mut e.kind
        {
            self.stmts(b);
        }
        match &mut e.kind {
            HirExprKind::Const(c) => self.literal_from(c, &e.ty, e.span, origin.as_ref()),
            HirExprKind::Call { callee: id, .. }
            | HirExprKind::FnRef(id)
            | HirExprKind::ClosureValue { fn_id: id, .. }
            | HirExprKind::CoroNew { body_fn: id, .. } => {
                if let Some(Some(real)) = self.insts.get(id) {
                    *id = *real;
                }
            }
            _ => {}
        }
        match &mut e.kind {
            HirExprKind::CallIndirect { ret_ty, .. }
            | HirExprKind::Loop {
                result_ty: ret_ty, ..
            } => {
                self.ty(ret_ty);
            }
            HirExprKind::Cast { from_ty, to_ty, .. } => {
                self.ty(from_ty);
                self.ty(to_ty);
            }
            HirExprKind::ClosureValue {
                env_ty,
                arg_tys,
                ret_ty,
                ..
            }
            | HirExprKind::CallClosure {
                arg_tys,
                ret_ty,
                env_ty,
                ..
            } => {
                self.ty(env_ty);
                arg_tys.iter_mut().for_each(|t| self.ty(t));
                self.ty(ret_ty);
            }
            HirExprKind::Match { arms, scrutinee } => {
                self.ty(&mut scrutinee.ty);
                for a in arms {
                    self.pattern(&mut a.pat);
                    self.retype(&mut a.pat, &scrutinee.ty);
                }
            }
            HirExprKind::FieldRead {
                struct_id,
                field_ty,
                ..
            } => {
                self.struct_id(struct_id);
                self.ty(field_ty);
            }
            HirExprKind::StructInit { struct_id, .. }
            | HirExprKind::WithField { struct_id, .. } => {
                self.struct_id(struct_id);
            }
            HirExprKind::EnumVariantInit { enum_id, .. } => self.enum_id(enum_id),
            HirExprKind::SumWrap {
                sum_ty,
                variant_index,
                ..
            } => self.sum_member(sum_ty, variant_index),
            HirExprKind::TupleField { field_ty, .. } => self.ty(field_ty),
            HirExprKind::ArrayInit { elem_ty, .. }
            | HirExprKind::ArrayRepeat { elem_ty, .. }
            | HirExprKind::ArrayIndex { elem_ty, .. }
            | HirExprKind::BufNew { elem: elem_ty, .. }
            | HirExprKind::BufGet { elem: elem_ty, .. }
            | HirExprKind::BufSet { elem: elem_ty, .. }
            | HirExprKind::BufAtomic { elem: elem_ty, .. }
            | HirExprKind::RawRead { ty: elem_ty, .. }
            | HirExprKind::RawWrite { ty: elem_ty, .. }
            | HirExprKind::SizeOf { ty: elem_ty, .. } => self.ty(elem_ty),
            HirExprKind::UnionWrite {
                struct_id,
                from_ty,
                storage_ty,
                ..
            } => {
                self.struct_id(struct_id);
                self.ty(from_ty);
                self.ty(storage_ty);
            }
            HirExprKind::UnionRead {
                struct_id,
                storage_ty,
                field_ty,
                ..
            } => {
                self.struct_id(struct_id);
                self.ty(storage_ty);
                self.ty(field_ty);
            }
            _ => {}
        }
    }

    pub(crate) fn pattern(&self, p: &mut HirPattern) {
        match p {
            HirPattern::Binding { ty, .. } => self.ty(ty),
            HirPattern::At { ty, sub, .. } => {
                self.ty(ty);
                self.pattern(sub);
            }
            HirPattern::Or(ps) | HirPattern::Tuple(ps) => {
                ps.iter_mut().for_each(|p| self.pattern(p));
            }
            HirPattern::Struct { struct_id, fields } => {
                self.struct_id(struct_id);
                fields.iter_mut().for_each(|(_, p)| self.pattern(p));
            }
            HirPattern::EnumVariant { enum_id, args, .. } => {
                self.enum_id(enum_id);
                args.iter_mut().for_each(|p| self.pattern(p));
            }
            HirPattern::SumVariant {
                sum_ty,
                payload_ty,
                variant_index,
                ..
            } => {
                self.sum_member(sum_ty, variant_index);
                if let Some(t) = payload_ty {
                    self.ty(t);
                }
            }
            HirPattern::SumMember {
                sum_ty,
                payload_ty,
                sub,
                variant_index,
            } => {
                self.sum_member(sum_ty, variant_index);
                self.ty(payload_ty);
                self.pattern(sub);
            }
            HirPattern::Slice(s) => {
                self.expr(&mut s.len_expr);
                let rest = s.rest.iter_mut().flatten();
                for (e, p) in s.head.iter_mut().chain(rest).chain(s.tail.iter_mut()) {
                    self.expr(e);
                    self.pattern(p);
                }
            }
            HirPattern::Wildcard | HirPattern::Literal { .. } | HirPattern::Range { .. } => {}
        }
    }

    /// Literal and range patterns take the solved scrutinee's type.
    pub(crate) fn retype(&self, p: &mut HirPattern, ty: &HirTy) {
        match (p, ty) {
            (HirPattern::Literal { value, prim }, HirTy::Prim(to)) if *prim != *to => {
                if matches!(value, ConstVal::Int { .. } | ConstVal::Float { .. }) {
                    *prim = *to;
                    self.literal(value, ty, self.ctx.here());
                }
            }
            (
                HirPattern::Range {
                    start, end, prim, ..
                },
                HirTy::Prim(to),
            ) if *prim != *to => {
                *prim = *to;
                for v in [start, end].into_iter().flatten() {
                    self.literal(v, ty, self.ctx.here());
                }
            }
            (HirPattern::Tuple(ps), HirTy::Tuple(ts)) if ps.len() == ts.len() => {
                for (p, t) in ps.iter_mut().zip(ts) {
                    self.retype(p, t);
                }
            }
            (HirPattern::Or(ps), _) => ps.iter_mut().for_each(|p| self.retype(p, ty)),
            (HirPattern::At { sub, .. }, _) => self.retype(sub, ty),
            (
                HirPattern::SumMember {
                    sub, payload_ty, ..
                },
                _,
            ) => {
                let t = payload_ty.clone();
                self.retype(sub, &t);
            }
            (HirPattern::Struct { struct_id, fields }, _) => {
                let d = self.ctx.struct_def(*struct_id);
                for (i, p) in fields {
                    if let Some(f) = d.fields.get(*i as usize) {
                        self.retype(p, &f.ty);
                    }
                }
            }
            (
                HirPattern::EnumVariant {
                    enum_id,
                    variant_index,
                    args,
                },
                _,
            ) => {
                let d = self.ctx.enum_def(*enum_id);
                let Some(t) = d
                    .variants
                    .get(*variant_index as usize)
                    .and_then(|v| v.payload_ty.clone())
                else {
                    return;
                };
                match (args.as_mut_slice(), &t) {
                    ([one], _) => self.retype(one, &t),
                    (ps, HirTy::Tuple(ts)) if ps.len() == ts.len() => {
                        for (p, t) in ps.iter_mut().zip(ts) {
                            self.retype(p, t);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// A literal takes the type its variable was solved to.
    pub(crate) fn literal(&self, c: &mut ConstVal, ty: &HirTy, at: Span) {
        self.literal_from(c, ty, at, None);
    }

    /// `literal`; `origin`: the declared default it took, if any.
    pub(crate) fn literal_from(
        &self,
        c: &mut ConstVal,
        ty: &HirTy,
        at: Span,
        origin: Option<&(Span, String)>,
    ) {
        let HirTy::Prim(to) = *ty else {
            return;
        };
        match c {
            ConstVal::Int { value, ty } if *ty != to && to.is_int() => {
                if !crate::lower::pattern::int_fits(*value, to) {
                    let src = self.ctx.src(at).unwrap_or("..").to_owned();
                    let of = origin.map(|(_, o)| format!(", {o}")).unwrap_or_default();
                    self.ctx.emit_error_at(
                        Code::LITERAL_OUT_OF_RANGE,
                        at,
                        format!(
                            "integer literal `{src}` out of range for `{}`{of}",
                            to.name()
                        ),
                    );
                    if let Some((call, o)) = origin {
                        let help = range_help(self.ctx, (*call, o), (&src, *value), to);
                        self.ctx.help_last(Some(help));
                    }
                }
                *ty = to;
            }
            // An integer literal that took a float default (`Vec3::new(1, 2, 3)`).
            ConstVal::Int { value, .. } if to.is_float() => {
                #[allow(clippy::cast_precision_loss)]
                let f = *value as f64;
                #[allow(clippy::cast_possible_truncation)]
                let bits = if to == PrimTy::F32 {
                    u64::from((f as f32).to_bits())
                } else {
                    f.to_bits()
                };
                *c = ConstVal::Float { bits, ty: to };
            }
            ConstVal::Float { bits, ty } if *ty == PrimTy::F64 && to == PrimTy::F32 => {
                #[allow(clippy::cast_possible_truncation)]
                let f = f64::from_bits(*bits) as f32;
                *bits = u64::from(f.to_bits());
                *ty = to;
            }
            _ => {}
        }
    }
}
