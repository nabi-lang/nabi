//! Desugaring helpers: destructuring assignment, `using`, parameter lets.

#[allow(clippy::wildcard_imports)]
use crate::parser::*;

impl Parser<'_> {
    /// `target = value` for a tuple `target` of places (nested tuples and
    /// `_` too): `let (t0, t1) = value;` then `p0 = t0; p1 = t1;`.
    pub(crate) fn destructure_assign(&mut self, target: Expr, value: Expr, out: &mut Vec<Stmt>) {
        let span = target.span;
        let ExprKind::Tuple(elems) = target.kind else {
            out.push(Stmt {
                kind: StmtKind::Assign(target, value),
                span,
            });
            return;
        };
        let mut names = Vec::new();
        let mut rest = Vec::new();
        for e in elems {
            let discard = matches!(&e.kind, ExprKind::Path(p)
                if p.segments.len() == 1 && p.segments[0].name == "_");
            if discard {
                names.push("_".to_owned());
                continue;
            }
            let name = self.fresh_destructure_temp();
            names.push(name.clone());
            let read = Expr {
                span: e.span,
                kind: ExprKind::Path(Path::new(
                    vec![PathSegment {
                        name,
                        generics: Vec::new(),
                    }],
                    e.span,
                )),
            };
            rest.push((e, read));
        }
        out.push(Stmt {
            kind: StmtKind::LetTuple {
                names,
                rest: None,
                ty: None,
                init: value,
            },
            span,
        });
        for (place, read) in rest {
            self.destructure_assign(place, read, out);
        }
    }

    /// `using a = e, b = f { body }` is
    /// `{ let a = e; { let b = f; { body } finally { b.drop(); } }
    /// finally { a.drop(); } }`: reverse order, every exit.
    pub(crate) fn build_using(bindings: Vec<UsingBinding>, mut body: Block, start: Span) -> Expr {
        // The patterns bind in source order, before the body.
        let lets: Vec<Stmt> = bindings
            .iter()
            .filter_map(|(name, span, _, pat)| {
                Some(Stmt {
                    kind: StmtKind::LetPattern {
                        pat: pat.clone()?,
                        ty: None,
                        init: Expr {
                            span: *span,
                            kind: ExprKind::Path(Path {
                                segments: vec![PathSegment {
                                    name: name.clone(),
                                    generics: Vec::new(),
                                }],
                                span: *span,
                                origin: None,
                            }),
                        },
                    },
                    span: *span,
                })
            })
            .collect();
        body.stmts.splice(0..0, lets);
        let mut inner = Expr {
            span: start,
            kind: ExprKind::Block(body),
        };
        for (name, span, init, _) in bindings.into_iter().rev() {
            let path = |name: &str| Expr {
                span,
                kind: ExprKind::Path(Path {
                    segments: vec![PathSegment {
                        name: name.to_owned(),
                        generics: Vec::new(),
                    }],
                    span,
                    origin: None,
                }),
            };
            let drop_call = Stmt {
                kind: StmtKind::Expr(Expr {
                    span,
                    kind: ExprKind::MethodCall {
                        receiver: Box::new(path(&name)),
                        method: nabic_ast::USING_DROP_METHOD.to_owned(),
                        type_args: Vec::new(),
                        args: Vec::new(),
                        trailing_closure: None,
                        origin: None,
                        written: false,
                    },
                }),
                span,
            };
            let guarded = Expr {
                span: start,
                kind: ExprKind::Catch {
                    body: Box::new(inner),
                    catches: Vec::new(),
                    finally: Some(Block {
                        stmts: vec![drop_call],
                        trailing: None,
                        span,
                        label: None,
                    }),
                },
            };
            inner = Expr {
                span: start,
                kind: ExprKind::Block(Block {
                    stmts: vec![Stmt {
                        kind: StmtKind::Let {
                            name,
                            ty: None,
                            init,
                        },
                        span,
                    }],
                    trailing: Some(Box::new(guarded)),
                    span: start,
                    label: None,
                }),
            };
        }
        inner
    }

    /// Each `using x = e;` marker in `stmts` (and `trailing`) takes the
    /// statements after it as its body, innermost first.
    pub(crate) fn fold_using_stmts(
        &mut self,
        stmts: &mut Vec<Stmt>,
        trailing: &mut Option<Box<Expr>>,
    ) {
        if self.using_stmts.is_empty() {
            return;
        }
        let mut i = stmts.len();
        while i > 0 {
            i -= 1;
            let key = match &stmts[i].kind {
                StmtKind::Expr(e) if matches!(&e.kind, ExprKind::Tuple(v) if v.is_empty()) => {
                    (e.span.file, e.span.start)
                }
                _ => continue,
            };
            let Some(bindings) = self.using_stmts.remove(&key) else {
                continue;
            };
            let start = stmts[i].span;
            let rest: Vec<Stmt> = stmts.drain(i + 1..).collect();
            stmts.pop();
            let span = Span {
                end: trailing
                    .as_ref()
                    .map(|t| t.span.end)
                    .or_else(|| rest.last().map(|s| s.span.end))
                    .unwrap_or(start.end),
                ..start
            };
            let body = Block {
                stmts: rest,
                trailing: trailing.take(),
                span,
                label: None,
            };
            let e = Self::build_using(bindings, body, start);
            *trailing = Some(Box::new(e));
        }
    }

    /// `let pat = temp;` for each pattern parameter. A tuple of names,
    /// `_` and tuples is a chain of `let (a, b) = ..` (checked for its
    /// length), as a `let` statement parses.
    pub(crate) fn param_lets(&mut self, pats: Vec<(String, Pattern)>) -> Vec<Stmt> {
        let mut out = Vec::new();
        for (name, pat) in pats {
            self.param_let(name, pat, &mut out);
        }
        out
    }

    pub(crate) fn param_let(&mut self, name: String, pat: Pattern, out: &mut Vec<Stmt>) {
        let span = pat.span;
        let init = Expr {
            span,
            kind: ExprKind::Path(Path::new(
                vec![PathSegment {
                    name,
                    generics: Vec::new(),
                }],
                span,
            )),
        };
        let flat = matches!(&pat.kind, PatternKind::Tuple(ps) if ps.iter().all(|p| matches!(
            p.kind,
            PatternKind::Binding { sub: None, .. } | PatternKind::Wildcard | PatternKind::Tuple(_)
        )));
        let PatternKind::Tuple(ps) = pat.kind else {
            out.push(Stmt {
                kind: StmtKind::LetPattern {
                    pat,
                    ty: None,
                    init,
                },
                span,
            });
            return;
        };
        if !flat {
            out.push(Stmt {
                kind: StmtKind::LetPattern {
                    pat: Pattern {
                        kind: PatternKind::Tuple(ps),
                        span,
                    },
                    ty: None,
                    init,
                },
                span,
            });
            return;
        }
        let mut names = Vec::new();
        let mut nested = Vec::new();
        for p in ps {
            match p.kind {
                PatternKind::Binding { name, .. } => names.push(name),
                PatternKind::Wildcard => names.push("_".to_owned()),
                _ => {
                    let t = self.fresh_destructure_temp();
                    names.push(t.clone());
                    nested.push((t, p));
                }
            }
        }
        out.push(Stmt {
            kind: StmtKind::LetTuple {
                names,
                rest: None,
                ty: None,
                init,
            },
            span,
        });
        for (t, p) in nested {
            self.param_let(t, p, out);
        }
    }

    /// A closure body with its pattern parameters destructured first.
    pub(crate) fn with_param_lets(&mut self, body: Expr, pats: Vec<(String, Pattern)>) -> Expr {
        if pats.is_empty() {
            return body;
        }
        let span = body.span;
        let stmts = self.param_lets(pats);
        let block = match body.kind {
            ExprKind::Block(mut b) if b.label.is_none() => {
                b.stmts.splice(0..0, stmts);
                b
            }
            kind => Block {
                stmts,
                trailing: Some(Box::new(Expr { kind, span })),
                span,
                label: None,
            },
        };
        Expr {
            span,
            kind: ExprKind::Block(block),
        }
    }
}
