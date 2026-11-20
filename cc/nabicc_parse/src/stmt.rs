//! Function bodies: statements and block-scope declarations.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// What a block scope declared, to undo at its end.
#[derive(Default)]
pub(crate) struct Scope {
    ords: Vec<(Sym, Option<Ord>)>,
    tags: Vec<(Sym, Option<Tag>)>,
}

impl Parser<'_, '_> {
    /// Enter a block scope.
    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    /// Leave a block scope: its names are gone.
    pub(crate) fn pop_scope(&mut self) {
        let Some(s) = self.scopes.pop() else { return };
        for (sym, old) in s.ords.into_iter().rev() {
            match old {
                Some(o) => self.ords.insert(sym, o),
                None => self.ords.remove(&sym),
            };
        }
        for (sym, old) in s.tags.into_iter().rev() {
            match old {
                Some(t) => self.tags.insert(sym, t),
                None => self.tags.remove(&sym),
            };
        }
    }

    /// Declare ordinary name `sym` in the innermost scope.
    pub(crate) fn bind(&mut self, sym: Sym, o: Ord) {
        let old = self.ords.insert(sym, o);
        match self.scopes.last_mut() {
            Some(s) => s.ords.push((sym, old)),
            None => {
                self.file_scope.insert(sym, o);
            }
        }
    }

    /// Declare tag `sym` in the innermost scope.
    pub(crate) fn bind_tag(&mut self, sym: Sym, t: Tag) {
        let old = self.tags.insert(sym, t);
        if let Some(s) = self.scopes.last_mut() {
            s.tags.push((sym, old));
        }
    }

    fn local(
        &mut self,
        name: &str,
        sym: Option<Sym>,
        ty: QualType,
        kind: LocalKind,
        loc: Loc,
    ) -> LocalId {
        let id = self.locals.len() as LocalId;
        self.locals.push(Local {
            name: name.to_owned(),
            ty,
            kind,
            align: 0,
            loc,
        });
        if let Some(s) = sym {
            self.bind(s, Ord::Local(id));
        }
        id
    }

    /// The body of function `func` from its `{`, its parameters
    /// `params`.
    pub(crate) fn body(&mut self, func: usize, params: &[(Option<String>, QualType)]) -> R<()> {
        let saved_locals = std::mem::take(&mut self.locals);
        self.current_fn = Some(self.fns[func].name.clone());
        self.push_scope();
        let loc = self.fns[func].loc;
        let mut ids = Vec::new();
        for (name, ty) in params {
            let sym = name.as_deref().map(|n| self.pp.names.intern(n));
            let n = name.clone().unwrap_or_default();
            ids.push(self.local(&n, sym, *ty, LocalKind::Param, loc));
        }
        let r = self.block_items();
        let end = self.last;
        self.pop_scope();
        self.current_fn = None;
        let stmts = r?;
        let locals = std::mem::replace(&mut self.locals, saved_locals);
        self.bodies.push(Body {
            func,
            params: ids,
            locals,
            stmts,
            end,
        });
        self.fns[func].body = Some(self.bodies.len() - 1);
        Ok(())
    }

    /// `{ block-item* }`, in a scope of its own.
    pub(crate) fn block_items(&mut self) -> R<Vec<Stmt>> {
        self.expect("{")?;
        self.push_scope();
        let mut out = Vec::new();
        let r = loop {
            let t = self.peek()?;
            if self.is_p(t, "}") {
                self.bump()?;
                break Ok(());
            }
            if t.kind == Kind::Eof {
                break Err(self.err(t.loc, "expected '}' at the end of this block"));
            }
            match self.block_item() {
                Ok(s) => out.push(s),
                Err(e) => break Err(e),
            }
        };
        self.pop_scope();
        r.map(|()| out)
    }

    fn block_item(&mut self) -> R<Stmt> {
        let t = self.peek()?;
        if self.starts_decl(t)? {
            return self.local_decl();
        }
        self.statement()
    }

    /// Does `t` start a declaration (not an expression statement or a
    /// label)?
    fn starts_decl(&mut self, t: Tok) -> R<bool> {
        if t.kind != Kind::Ident {
            return Ok(false);
        }
        if matches!(
            t.sym,
            kw::TYPEDEF
                | kw::EXTERN
                | kw::STATIC
                | kw::AUTO
                | kw::REGISTER
                | kw::THREAD_LOCAL2
                | kw::THREAD
                | kw::INLINE
                | kw::INLINE2
                | kw::INLINE3
                | kw::STATIC_ASSERT
                | kw::STATIC_ASSERT2
                | kw::NORETURN
                | kw::ALIGNAS2
                | kw::DECLSPEC
        ) {
            return Ok(true);
        }
        if kw::EXTENSION == t.sym {
            return self.second_starts_type();
        }
        // `T:` is a label even when `T` names a type.
        if self.is_typedef_name(t.sym) && self.second_is(":")? {
            return Ok(false);
        }
        Ok(self.starts_type(t))
    }

    /// A declaration in a block.
    fn local_decl(&mut self) -> R<Stmt> {
        let start = self.peek()?;
        if matches!(start.sym, kw::STATIC_ASSERT | kw::STATIC_ASSERT2) {
            self.static_assert()?;
            return Ok(Stmt::new(StmtKind::Null, start.loc));
        }
        let specs = self.specs(false)?;
        if self.eat(";")? {
            return Ok(Stmt::new(StmtKind::Null, start.loc));
        }
        let base = self.base_type(&specs)?;
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let mut pre = Attrs::default();
            if !first {
                self.attributes(&mut pre)?;
            }
            let mut d = self.declarator()?;
            d.attrs.merge(&pre);
            self.attributes(&mut d.attrs)?;
            let Some((name, sym, loc)) = d.name.clone() else {
                let t = self.peek()?;
                return Err(self.err(t.loc, "expected a name in this declaration"));
            };
            let mut attrs = specs.attrs.clone();
            attrs.merge(&d.attrs);
            let ty = self.apply(base, &specs, &d.chunks, &attrs);
            let is_fn = matches!(self.types.get(self.types.canon(ty.ty)), Type::Func(_));
            if specs.storage == Storage::Typedef {
                self.types.typedefs.push(Typedef {
                    name: name.clone(),
                    ty,
                    align: attrs.aligned,
                    loc,
                });
                let id = (self.types.typedefs.len() - 1) as TypedefId;
                if first {
                    self.name_anonymous(ty.ty, &name);
                }
                self.bind(sym, Ord::Typedef(id));
            } else if is_fn || specs.storage == Storage::Extern {
                // A block-scope declaration of something outside.
                let found = self.file_decl(sym, &name);
                match found {
                    Some(o) => self.bind(sym, o),
                    None if is_fn => {
                        let params = match d.chunks.first() {
                            Some(Chunk::Func { params, .. }) => params.clone(),
                            _ => self.synth_params(ty.ty),
                        };
                        self.fns.push(FnDecl {
                            name,
                            ty: ty.ty,
                            params,
                            is_static: false,
                            inline: false,
                            asm: attrs.asm.clone(),
                            loc,
                            body: None,
                        });
                        let id = self.fns.len() - 1;
                        self.file_scope.insert(sym, Ord::Func(id));
                        self.bind(sym, Ord::Func(id));
                    }
                    None => {
                        self.vars.push(VarDecl {
                            name,
                            ty,
                            is_static: false,
                            tls: specs.tls || attrs.tls,
                            asm: attrs.asm.clone(),
                            loc,
                            is_extern: true,
                            init: None,
                        });
                        let id = self.vars.len() - 1;
                        self.file_scope.insert(sym, Ord::Var(id));
                        self.bind(sym, Ord::Var(id));
                    }
                }
            } else {
                if specs.tls || attrs.tls {
                    return Err(self.err(
                        loc,
                        "thread-local variables are not supported by `nabi cc` yet; pass the \
                         state through a parameter",
                    ));
                }
                let kind = match specs.storage {
                    Storage::Static => LocalKind::Static,
                    Storage::Auto if self.name(start.sym) == "register" => LocalKind::Register,
                    _ => LocalKind::Auto,
                };
                // The name is in scope in its own initializer.
                let id = self.local(&name, Some(sym), ty, kind, loc);
                self.locals[id as usize].align = attrs.aligned;
                let init = if self.eat("=")? {
                    let i = self.initializer()?;
                    let q = self.complete(self.locals[id as usize].ty, &i);
                    self.locals[id as usize].ty = q;
                    Some(i)
                } else {
                    None
                };
                items.push((id, init));
                first = false;
                if self.eat(",")? {
                    continue;
                }
                self.expect(";")?;
                break;
            }
            if self.eat("=")? {
                return Err(self.err(
                    loc,
                    &format!(
                        "`{}` declared `extern` cannot have an initializer here",
                        self.name(sym)
                    ),
                ));
            }
            first = false;
            if self.eat(",")? {
                continue;
            }
            self.expect(";")?;
            break;
        }
        Ok(Stmt::new(StmtKind::Decl(items), start.loc))
    }

    /// What `sym` means at file scope, if anything.
    fn file_decl(&self, sym: Sym, _name: &str) -> Option<Ord> {
        self.file_scope.get(&sym).copied()
    }

    pub(crate) fn statement(&mut self) -> R<Stmt> {
        let t = self.peek()?;
        let loc = t.loc;
        if self.is_p(t, "{") {
            let b = self.block_items()?;
            return Ok(Stmt::new(StmtKind::Block(b), loc));
        }
        if self.is_p(t, ";") {
            self.bump()?;
            return Ok(Stmt::new(StmtKind::Null, loc));
        }
        if t.kind == Kind::Ident {
            // A label.
            if !is_reserved(t.sym) && self.second_is(":")? && !self.second_is("::")? {
                self.bump()?;
                self.bump()?;
                let name = self.name(t.sym).to_owned();
                let mut a = Attrs::default();
                self.attributes(&mut a)?;
                let s = if self.at("}")? {
                    Stmt::new(StmtKind::Null, loc)
                } else {
                    let n = self.peek()?;
                    if self.starts_decl(n)? {
                        self.local_decl()?
                    } else {
                        self.statement()?
                    }
                };
                return Ok(Stmt::new(StmtKind::Label(name, Box::new(s)), loc));
            }
            match t.sym {
                kw::IF => {
                    self.bump()?;
                    self.expect("(")?;
                    let c = self.tree_expr()?;
                    self.expect(")")?;
                    let a = self.sub_statement()?;
                    let b = if self.peek()?.sym == kw::ELSE && self.peek()?.kind == Kind::Ident {
                        self.bump()?;
                        Some(Box::new(self.sub_statement()?))
                    } else {
                        None
                    };
                    return Ok(Stmt::new(StmtKind::If(c, Box::new(a), b), loc));
                }
                kw::WHILE => {
                    self.bump()?;
                    self.expect("(")?;
                    let c = self.tree_expr()?;
                    self.expect(")")?;
                    let b = self.sub_statement()?;
                    return Ok(Stmt::new(StmtKind::While(c, Box::new(b)), loc));
                }
                kw::DO => {
                    self.bump()?;
                    let b = self.sub_statement()?;
                    let w = self.bump()?;
                    if w.sym != kw::WHILE || w.kind != Kind::Ident {
                        return Err(self.err(w.loc, "expected 'while' after the body of a `do`"));
                    }
                    self.expect("(")?;
                    let c = self.tree_expr()?;
                    self.expect(")")?;
                    self.expect(";")?;
                    return Ok(Stmt::new(StmtKind::Do(Box::new(b), c), loc));
                }
                kw::FOR => {
                    self.bump()?;
                    self.expect("(")?;
                    self.push_scope();
                    let r = self.for_rest(loc);
                    self.pop_scope();
                    return r;
                }
                kw::SWITCH => {
                    self.bump()?;
                    self.expect("(")?;
                    let c = self.tree_expr()?;
                    self.expect(")")?;
                    let b = self.sub_statement()?;
                    return Ok(Stmt::new(StmtKind::Switch(c, Box::new(b)), loc));
                }
                kw::CASE => {
                    self.bump()?;
                    let a = self.tree_cond()?;
                    let b = if self.eat("...")? {
                        Some(self.tree_cond()?)
                    } else {
                        None
                    };
                    self.expect(":")?;
                    let s = self.case_body(loc)?;
                    return Ok(Stmt::new(StmtKind::Case(a, b, Box::new(s)), loc));
                }
                kw::DEFAULT => {
                    self.bump()?;
                    self.expect(":")?;
                    let s = self.case_body(loc)?;
                    return Ok(Stmt::new(StmtKind::Default(Box::new(s)), loc));
                }
                kw::BREAK => {
                    self.bump()?;
                    self.expect(";")?;
                    return Ok(Stmt::new(StmtKind::Break, loc));
                }
                kw::CONTINUE => {
                    self.bump()?;
                    self.expect(";")?;
                    return Ok(Stmt::new(StmtKind::Continue, loc));
                }
                kw::RETURN => {
                    self.bump()?;
                    let e = if self.at(";")? {
                        None
                    } else {
                        Some(self.tree_expr()?)
                    };
                    self.expect(";")?;
                    return Ok(Stmt::new(StmtKind::Return(e), loc));
                }
                kw::GOTO => {
                    self.bump()?;
                    let l = self.bump()?;
                    if l.kind != Kind::Ident {
                        let msg = if self.is_p(l, "*") {
                            "computed `goto *p` is a GNU extension `nabi cc` does not compile; \
                             dispatch with a `switch` instead"
                        } else {
                            "expected a label name after `goto`"
                        };
                        return Err(self.err(l.loc, msg));
                    }
                    self.expect(";")?;
                    let name = self.name(l.sym).to_owned();
                    return Ok(Stmt::new(StmtKind::Goto(name), loc));
                }
                kw::ASM | kw::ASM2 | kw::ASM3 => return self.asm_statement(),
                _ => {}
            }
        }
        let e = self.tree_expr()?;
        self.expect(";")?;
        Ok(Stmt::new(StmtKind::Expr(e), loc))
    }

    /// The statement after `case x:` (a declaration there is C23's).
    fn case_body(&mut self, loc: Loc) -> R<Stmt> {
        if self.at("}")? {
            return Ok(Stmt::new(StmtKind::Null, loc));
        }
        let n = self.peek()?;
        if self.starts_decl(n)? {
            return self.local_decl();
        }
        self.statement()
    }

    /// A statement in its own scope (C99: the body of `if`, loops).
    fn sub_statement(&mut self) -> R<Stmt> {
        self.push_scope();
        let r = self.statement();
        self.pop_scope();
        r
    }

    fn for_rest(&mut self, loc: Loc) -> R<Stmt> {
        let init = if self.eat(";")? {
            None
        } else {
            let t = self.peek()?;
            if self.starts_decl(t)? {
                Some(Box::new(self.local_decl()?))
            } else {
                let e = self.tree_expr()?;
                self.expect(";")?;
                Some(Box::new(Stmt::new(StmtKind::Expr(e), t.loc)))
            }
        };
        let cond = if self.at(";")? {
            None
        } else {
            Some(self.tree_expr()?)
        };
        self.expect(";")?;
        let step = if self.at(")")? {
            None
        } else {
            Some(self.tree_expr()?)
        };
        self.expect(")")?;
        let body = self.sub_statement()?;
        Ok(Stmt::new(
            StmtKind::For(init, cond, step, Box::new(body)),
            loc,
        ))
    }

    /// `asm ( .. );`: only an empty one (a compiler barrier) compiles.
    fn asm_statement(&mut self) -> R<Stmt> {
        let t = self.bump()?;
        while matches!(
            self.peek()?.sym,
            kw::VOLATILE | kw::VOLATILE2 | kw::VOLATILE3 | kw::INLINE | kw::GOTO
        ) && self.peek()?.kind == Kind::Ident
        {
            self.bump()?;
        }
        self.expect("(")?;
        // An empty template (a compiler barrier) or a lone hint without
        // operands (`pause`, `yield`, `nop`: a spin loop's) is nothing.
        let mut empty = true;
        let mut barrier = false;
        let mut operands = false;
        let mut depth = 1;
        let mut first = true;
        loop {
            let x = self.bump()?;
            if x.kind == Kind::Eof {
                return Err(self.err(t.loc, "this `asm` is never closed"));
            }
            if first {
                let text = self.name(x.sym);
                barrier = x.kind == Kind::Str && text == "\"\"";
                empty = x.kind == Kind::Str
                    && matches!(
                        text,
                        "\"\""
                            | "\"pause\""
                            | "\"yield\""
                            | "\"nop\""
                            | "\"rep; nop\""
                            | "\"rep nop\""
                    );
                first = false;
            } else if self.is_p(x, ":") && depth == 1 {
                operands = true;
            }
            if self.is_p(x, "(") {
                depth += 1;
            } else if self.is_p(x, ")") {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
        }
        self.expect(";")?;
        // `asm("" ::: "memory")` names clobbers only: still a barrier.
        let kind = if empty && (barrier || !operands) {
            StmtKind::Null
        } else {
            StmtKind::Asm
        };
        Ok(Stmt::new(kind, t.loc))
    }
}
