//! Statements: loops, `switch` cases, labels, local initializers.

use crate::check::{Cx, R, Slot, SwitchCx};
use crate::{Def, TS, TStmt};
use nabicc_parse::tree::{Stmt, StmtKind};
use nabicc_target::Prim;

impl Cx<'_> {
    pub(crate) fn stmt(&mut self, s: &Stmt) -> R<TStmt> {
        let loc = s.loc;
        let k = match &s.kind {
            StmtKind::Expr(e) => TS::Expr(self.value(e)?),
            StmtKind::Decl(items) => {
                let mut out = Vec::new();
                for (l, init) in items {
                    match self.slots.get(*l as usize).copied() {
                        Some(Slot::Local(id)) => {
                            let ty = self.locals[id as usize].ty;
                            if self.size_of(ty).is_none() && init.is_none() {
                                return Err(self.err(
                                    self.locals[id as usize].loc,
                                    &format!(
                                        "variable has incomplete type `{}`: define it before \
                                         this declaration",
                                        self.spell(ty)
                                    ),
                                ));
                            }
                            if let Some(init) = init {
                                let (ty, ti) = self.init_object(ty, init, loc)?;
                                self.locals[id as usize].ty = ty;
                                out.push(TStmt::new(TS::Init(id, ti), loc));
                            }
                        }
                        Some(Slot::Global(g)) => {
                            let ty = self.globals[g].ty;
                            match init {
                                Some(init) => {
                                    let saved = self.file_scope;
                                    self.file_scope = true;
                                    let r = self.init_object(ty, init, loc);
                                    self.file_scope = saved;
                                    let (ty, ti) = r?;
                                    self.globals[g].ty = ty;
                                    self.globals[g].def = Def::Init(ti);
                                }
                                None => self.globals[g].def = Def::Tentative,
                            }
                        }
                        None => return Err(self.err(loc, "a local outside its function")),
                    }
                }
                if out.len() == 1 {
                    return Ok(out.pop().expect("one"));
                }
                TS::Block(out)
            }
            StmtKind::Block(ss) => TS::Block(self.stmts(ss)?),
            StmtKind::If(c, a, b) => {
                let c = self.scalar(c)?;
                let a = self.stmt(a)?;
                let b = match b {
                    Some(b) => Some(Box::new(self.stmt(b)?)),
                    None => None,
                };
                TS::If(c, Box::new(a), b)
            }
            StmtKind::While(c, b) => {
                let c = self.scalar(c)?;
                self.loops += 1;
                let b = self.stmt(b);
                self.loops -= 1;
                TS::While(c, Box::new(b?))
            }
            StmtKind::Do(b, c) => {
                self.loops += 1;
                let b = self.stmt(b);
                self.loops -= 1;
                let c = self.scalar(c)?;
                TS::Do(Box::new(b?), c)
            }
            StmtKind::For(init, c, step, b) => {
                let init = match init {
                    Some(i) => Some(Box::new(self.stmt(i)?)),
                    None => None,
                };
                let c = match c {
                    Some(c) => Some(self.scalar(c)?),
                    None => None,
                };
                let step = match step {
                    Some(e) => Some(self.value(e)?),
                    None => None,
                };
                self.loops += 1;
                let b = self.stmt(b);
                self.loops -= 1;
                TS::For(init, c, step, Box::new(b?))
            }
            StmtKind::Switch(v, b) => {
                let v = self.value(v)?;
                let Some(p) = self.prim_of(v.ty).filter(|p| p.is_integer()) else {
                    return Err(self.err(
                        loc,
                        &format!("`switch` needs an integer, not `{}`", self.spell(v.ty)),
                    ));
                };
                let p = self.promote(p);
                let t = self.prim_ty(p);
                let v = self.conv(v, t);
                self.switches.push(SwitchCx {
                    ty: Some(p),
                    ..SwitchCx::default()
                });
                let saved = self.loops;
                let b = self.stmt(b);
                self.loops = saved;
                let sw = self.switches.pop().expect("this switch");
                TS::Switch(v, Box::new(b?), sw.cases, sw.default)
            }
            StmtKind::Case(a, b, body) => {
                let Some(ty) = self.switches.last().and_then(|s| s.ty) else {
                    return Err(self.err(loc, "`case` outside a `switch`"));
                };
                let lo = self.case_value(a, ty)?;
                let hi = match b {
                    Some(b) => self.case_value(b, ty)?,
                    None => lo,
                };
                let l = self.fresh_label();
                if let Some(sw) = self.switches.last_mut() {
                    if sw.cases.iter().any(|(x, y, _)| lo <= *y && *x <= hi) {
                        return Err(self.err(loc, &format!("duplicate case value {lo}")));
                    }
                    sw.cases.push((lo, hi, l));
                }
                let body = self.stmt(body)?;
                TS::Label(l, Box::new(body))
            }
            StmtKind::Default(body) => {
                if self.switches.is_empty() {
                    return Err(self.err(loc, "`default` outside a `switch`"));
                }
                let l = self.fresh_label();
                if let Some(sw) = self.switches.last_mut() {
                    if sw.default.is_some() {
                        return Err(self.err(loc, "multiple `default` labels in one `switch`"));
                    }
                    sw.default = Some(l);
                }
                let body = self.stmt(body)?;
                TS::Label(l, Box::new(body))
            }
            StmtKind::Break => {
                if self.loops == 0 && self.switches.is_empty() {
                    return Err(self.err(loc, "`break` outside a loop or `switch`"));
                }
                TS::Break
            }
            StmtKind::Continue => {
                if self.loops == 0 {
                    return Err(self.err(loc, "`continue` outside a loop"));
                }
                TS::Continue
            }
            StmtKind::Return(e) => {
                let ret = self.ret;
                match e {
                    Some(e) => {
                        let v = self.value(e)?;
                        if self.is_void(ret) {
                            // `return f();` of a void `f` is fine.
                            TS::Block(vec![
                                TStmt::new(TS::Expr(v), loc),
                                TStmt::new(TS::Return(None), loc),
                            ])
                        } else if self.record_of(ret).is_some() {
                            if self.record_of(v.ty) != self.record_of(ret) {
                                return Err(self.err(
                                    loc,
                                    &format!(
                                        "returning `{}` from a function returning `{}`",
                                        self.spell(v.ty),
                                        self.spell(ret)
                                    ),
                                ));
                            }
                            TS::Return(Some(v))
                        } else {
                            if !self.is_scalar(v.ty) {
                                return Err(self
                                    .err(loc, &format!("returning `{}` here", self.spell(v.ty))));
                            }
                            TS::Return(Some(self.conv(v, ret)))
                        }
                    }
                    None => TS::Return(None),
                }
            }
            StmtKind::Goto(name) => TS::Goto(self.label(name, loc)),
            StmtKind::Label(name, body) => {
                let l = self.label(name, loc);
                if let Some(x) = self.labels.get_mut(name) {
                    if x.1 {
                        return Err(self.err(loc, &format!("redefinition of label '{name}'")));
                    }
                    x.1 = true;
                }
                let body = self.stmt(body)?;
                TS::Label(l, Box::new(body))
            }
            StmtKind::Null => TS::Null,
            StmtKind::Asm => TS::Asm,
        };
        Ok(TStmt::new(k, loc))
    }

    fn case_value(&mut self, e: &nabicc_parse::Expr, ty: Prim) -> R<i128> {
        let v = self.value(e)?;
        let t = self.prim_ty(ty);
        let v = self.conv(v, t);
        match self.const_int(&v) {
            Some(x) => Ok(x),
            None => Err(self.err(
                e.loc,
                "a `case` value must be an integer constant expression",
            )),
        }
    }

    /// The `int` value of constant `e`.
    pub(crate) fn const_int(&self, e: &crate::TExpr) -> Option<i128> {
        match self.fold(e)? {
            crate::Const::Int(v) => Some(v),
            _ => None,
        }
    }
}
