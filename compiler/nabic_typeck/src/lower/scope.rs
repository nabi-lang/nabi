//! Locals, cells and scopes.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl LowerCtx<'_> {
    /// The value type of variable `id` (a `[ref]`-captured cell's
    /// element type).
    pub(crate) fn var_ty(&self, id: LocalId) -> HirTy {
        match self.local_ty(id) {
            HirTy::GcBuf(t) if self.is_boxed(id) => *t,
            t => t,
        }
    }

    /// Read variable `id` (through its cell when `[ref]`-captured).
    pub(crate) fn read_local(&self, id: LocalId, span: nabic_diagnostic::Span) -> HirExpr {
        if let Some(view) = self.cell_views.get(&id) {
            return view.clone();
        }
        let ty = self.local_ty(id);
        let HirTy::GcBuf(elem) = &ty else {
            return hir_local(id, &ty, span);
        };
        if !self.is_boxed(id) {
            return hir_local(id, &ty, span);
        }
        HirExpr {
            kind: HirExprKind::BufGet {
                buf: Box::new(hir_local(id, &ty, span)),
                index: Box::new(hir_int(0, PrimTy::Usize, span)),
                elem: (**elem).clone(),
            },
            ty: (**elem).clone(),
            span,
        }
    }

    /// `id = value` (into its cell when `[ref]`-captured).
    pub(crate) fn store_local(&self, id: LocalId, value: HirExpr) -> HirStmt {
        if !self.is_boxed(id) {
            return HirStmt::Assign { local: id, value };
        }
        let span = value.span;
        HirStmt::Expr(buf_store(hir_local(id, &self.local_ty(id), span), value))
    }

    /// Declare `let name: ty = init` (the binding declared at `at`): a
    /// plain local, or a fresh cell for a `[ref]`-captured binding.
    pub(crate) fn declare_var(
        &mut self,
        name: &str,
        ty: HirTy,
        init: HirExpr,
        at: nabic_diagnostic::Span,
    ) -> HirStmt {
        if !self.ref_captured.contains(&(name.to_owned(), at)) {
            let local = self.declare(Some(name.to_owned()), ty);
            return HirStmt::Let { local, init };
        }
        self.cell_let(name, ty, init)
    }

    /// Track `local` if it binds a lock guard (`guard_lets`).
    pub(crate) fn track_guard(&mut self, local: LocalId, ty: &HirTy, at: Span, name: &str) {
        if let Some(g) = lock_guard(self, ty) {
            let depth = self.scopes.len();
            self.guard_lets.push((local, depth, at, name.to_owned(), g));
        }
    }

    pub(crate) fn declare_bound(
        &mut self,
        name: &str,
        ty: HirTy,
        at: nabic_diagnostic::Span,
    ) -> LocalId {
        if let Some(&local) = self.or_pattern_binders.last().and_then(|m| m.get(name)) {
            let existing = self.local_ty(local);
            if matches!(existing, HirTy::Unknown) {
                self.locals[local.0 as usize].ty = ty;
            } else if existing != ty {
                self.emit_error(
                    Code::OR_PATTERN_BINDINGS,
                    format!(
                        "or-pattern binding `{name}` has inconsistent types across \
                         alternatives (`{}` and `{}`); to bind either as \
                         their sum, write one type pattern: `{name}: {} | {}`",
                        self.ty_name(&existing),
                        self.ty_name(&ty),
                        self.ty_name(&existing),
                        self.ty_name(&ty),
                    ),
                );
            }
            return local;
        }
        let local = self.declare(Some(name.to_owned()), ty.clone());
        self.track_guard(local, &ty, at, name);
        if self.ref_captured.contains(&(name.to_owned(), at)) {
            self.pending_cells.push((name.to_owned(), local, ty));
        }
        local
    }

    /// `let name = cell(name)` for the bindings since the last call, run
    /// before their scope's code.
    pub(crate) fn take_cells(&mut self, span: nabic_diagnostic::Span) -> Vec<HirStmt> {
        let mut out = self.take_resums(span);
        let cells: Vec<HirStmt> = std::mem::take(&mut self.pending_cells)
            .into_iter()
            .map(|(name, l, ty)| {
                let init = hir_local(l, &ty, span);
                self.cell_let(&name, ty, init)
            })
            .collect();
        out.extend(cells);
        out
    }

    /// `binder = resum(whole)` for the sub-sum type patterns since the
    /// last call.
    pub(crate) fn take_resums(&mut self, span: nabic_diagnostic::Span) -> Vec<HirStmt> {
        std::mem::take(&mut self.pending_resums)
            .into_iter()
            .map(|(binder, whole)| {
                let (to, from) = (self.local_ty(binder), self.local_ty(whole));
                let init = resum(self, hir_local(whole, &from, span), &to);
                HirStmt::Let {
                    local: binder,
                    init,
                }
            })
            .collect()
    }

    /// `let name = <new cell holding init>`.
    pub(crate) fn cell_let(&mut self, name: &str, ty: HirTy, init: HirExpr) -> HirStmt {
        let span = init.span;
        let cell_ty = HirTy::GcBuf(Box::new(ty.clone()));
        let tmp = self.declare(None, cell_ty.clone());
        let local = self.declare(Some(name.to_owned()), cell_ty.clone());
        self.locals[local.0 as usize].boxed = true;
        let cell = HirExprKind::BufNew {
            elem: ty,
            len: Box::new(hir_int(1, PrimTy::Usize, span)),
        };
        HirStmt::Let {
            local,
            init: HirExpr {
                kind: HirExprKind::Block(Box::new(HirBlock {
                    stmts: vec![
                        HirStmt::Let {
                            local: tmp,
                            init: HirExpr {
                                kind: cell,
                                ty: cell_ty.clone(),
                                span,
                            },
                        },
                        HirStmt::Expr(buf_store(hir_local(tmp, &cell_ty, span), init)),
                    ],
                    trailing: Some(hir_local(tmp, &cell_ty, span)),
                    label: None,
                })),
                ty: cell_ty,
                span,
            },
        }
    }

    /// The named `some` type path `p` names in `module`.
    pub(crate) fn some_type_name(&self, p: &nabic_ast::Path, module: &str) -> Option<String> {
        if !p.segments.last()?.generics.is_empty() {
            return None;
        }
        let segs: Vec<&str> = p.segments.iter().map(|s| s.name.as_str()).collect();
        let module = p.origin.as_deref().unwrap_or(module);
        self.env
            .names
            .canon(&segs, module, &[], self.here())
            .filter(|full| self.env.names.some_types.contains_key(full))
    }

    /// Signature of function `id` (a method's, whatever its key).
    pub(crate) fn sig_by_id(&self, id: FunctionId) -> FnSig {
        self.sig_of_key(self.env.fn_key(id).expect("a declaration"))
    }

    /// `fn_table[key]`, with a return type inferred from the body when
    /// the declaration omits it.
    pub(crate) fn sig_of_key(&self, key: &str) -> FnSig {
        let mut sig = self.env.sig(key).expect("a signature").clone();
        // Named `some` types of other modules: seen through their bounds.
        if !self.env.names.some_types.is_empty() {
            if let Some(decl) = self.env.decl(sig.id) {
                let xs = mono::sig_some_types(self.env, decl.def());
                let sink = &self.env.sink;
                if sink.opaque_check.get() {
                    let view = sink.some_view.borrow().clone();
                    if view.is_some_and(|v| xs.iter().any(|x| !mono::sees_some(&v, x))) {
                        sig = mono::decl_sig(self.env, sig.id.0 as usize, &sig);
                    }
                } else if xs.iter().any(|x| !mono::sees_some(&self.current_module, x)) {
                    sink.opaque_called.set(true);
                }
            }
        }
        if self.env.infers_ret(sig.id) {
            sig.ret_ty = match mono::opaque_ret(self, sig.id) {
                Some(p) => mono::with_ok(sig.ret_ty, p),
                None => mono::decl_ret(self, sig.id, sig.ret_ty),
            };
        }
        sig
    }

    pub(crate) fn into_locals(self) -> Vec<HirLocal> {
        self.locals
    }

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(HashMap::default());
        self.local_uses.push(Vec::new());
        self.nested_fns.push(HashMap::default());
    }

    pub(crate) fn pop_scope(&mut self) {
        let depth = self.scopes.len();
        let (open, kept) = std::mem::take(&mut self.guard_lets)
            .into_iter()
            .partition(|g| g.1 >= depth);
        self.guard_lets = kept;
        for (_, _, at, binder, guard) in open {
            self.emit_error_at(
                Code::GUARD_NOT_RELEASED,
                at,
                format!(
                    "`{binder}` holds a `{guard}` that is never released: only `using` \
                     releases a lock guard, so bind it with `using {binder} = .. {{ .. }}`"
                ),
            );
        }
        self.scopes.pop();
        self.local_uses.pop();
        self.nested_fns.pop();
    }

    pub(crate) fn add_local_use(&mut self, entry: crate::uses::UseEntry) {
        if let Some(top) = self.local_uses.last_mut() {
            top.push(entry);
        }
    }

    pub(crate) fn declare(&mut self, name: Option<String>, ty: HirTy) -> LocalId {
        let id = LocalId(u32::try_from(self.locals.len()).expect("LocalId overflow"));
        if let Some(n) = &name {
            if let Some(scope) = self.scopes.last_mut() {
                scope.insert(n.clone(), id);
            }
        }
        self.locals.push(HirLocal {
            id,
            name,
            ty,
            boxed: false,
        });
        id
    }

    /// Whether `id` is a `[ref]`-captured variable's cell.
    pub(crate) fn is_boxed(&self, id: LocalId) -> bool {
        self.locals[id.0 as usize].boxed
    }

    pub(crate) fn lookup(&self, name: &str) -> Option<LocalId> {
        for scope in self.scopes.iter().rev() {
            if let Some(id) = scope.get(name) {
                return Some(*id);
            }
        }
        None
    }

    /// Look up a name only in the innermost scope.
    pub(crate) fn lookup_in_current_scope(&self, name: &str) -> Option<LocalId> {
        self.scopes.last().and_then(|s| s.get(name).copied())
    }

    pub(crate) fn local_ty(&self, id: LocalId) -> HirTy {
        self.locals
            .get(id.0 as usize)
            .map_or(HirTy::Unknown, |l| l.ty.clone())
    }
}
