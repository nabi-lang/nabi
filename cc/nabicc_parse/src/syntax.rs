//! Expressions of function bodies and initializers, as trees.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Parser<'_, '_> {
    /// `e (, e)*`.
    pub fn tree_expr(&mut self) -> R<Expr> {
        let mut e = self.tree_assign()?;
        while self.at(",")? {
            let t = self.bump()?;
            let r = self.tree_assign()?;
            e = Expr::new(
                ExprKind::Binary(BinOp::Comma, Box::new(e), Box::new(r)),
                t.loc,
            );
        }
        Ok(e)
    }

    pub fn tree_assign(&mut self) -> R<Expr> {
        let l = self.tree_cond()?;
        let t = self.peek()?;
        if t.kind == Kind::Punct {
            if let Some(op) = BinOp::assign(self.name(t.sym)) {
                self.bump()?;
                let r = self.tree_assign()?;
                return Ok(Expr::new(
                    ExprKind::Assign(op, Box::new(l), Box::new(r)),
                    t.loc,
                ));
            }
        }
        Ok(l)
    }

    pub(crate) fn tree_cond(&mut self) -> R<Expr> {
        let c = self.tree_binary(1)?;
        if !self.at("?")? {
            return Ok(c);
        }
        let q = self.bump()?;
        let a = if self.at(":")? {
            None
        } else {
            Some(Box::new(self.tree_expr()?))
        };
        self.expect(":")?;
        let b = self.tree_cond()?;
        Ok(Expr::new(
            ExprKind::Cond(Box::new(c), a, Box::new(b)),
            q.loc,
        ))
    }

    fn tree_binary(&mut self, min: u8) -> R<Expr> {
        let mut l = self.tree_cast()?;
        loop {
            let t = self.peek()?;
            if t.kind != Kind::Punct {
                return Ok(l);
            }
            let Some((op, prec)) = BinOp::of(self.name(t.sym)) else {
                return Ok(l);
            };
            if prec < min {
                return Ok(l);
            }
            self.bump()?;
            let r = self.tree_binary(prec + 1)?;
            l = Expr::new(ExprKind::Binary(op, Box::new(l), Box::new(r)), t.loc);
        }
    }

    fn tree_cast(&mut self) -> R<Expr> {
        if self.at("(")? && self.second_starts_type()? {
            let open = self.bump()?;
            let q = self.type_name()?;
            self.expect(")")?;
            if self.at("{")? {
                let init = self.initializer()?;
                let e = Expr::new(ExprKind::Compound(q, Box::new(init)), open.loc);
                return self.tree_postfix(e);
            }
            let e = self.tree_cast()?;
            return Ok(Expr::new(ExprKind::Cast(q, Box::new(e)), open.loc));
        }
        self.tree_unary()
    }

    fn tree_unary(&mut self) -> R<Expr> {
        let t = self.peek()?;
        if t.kind == Kind::Punct {
            let op = match self.name(t.sym) {
                "-" => Some(UnOp::Neg),
                "+" => Some(UnOp::Plus),
                "~" => Some(UnOp::BitNot),
                "!" => Some(UnOp::Not),
                "*" => Some(UnOp::Deref),
                "&" => Some(UnOp::Addr),
                "++" => Some(UnOp::PreInc),
                "--" => Some(UnOp::PreDec),
                "&&" => {
                    return Err(self.err(
                        t.loc,
                        "`&&label` (a label's address) is a GNU extension `nabi cc` does not \
                         compile; dispatch with a `switch` instead",
                    ));
                }
                _ => None,
            };
            if let Some(op) = op {
                self.bump()?;
                let e = if matches!(op, UnOp::PreInc | UnOp::PreDec) {
                    self.tree_unary()?
                } else {
                    self.tree_cast()?
                };
                return Ok(Expr::new(ExprKind::Unary(op, Box::new(e)), t.loc));
            }
        }
        if t.kind == Kind::Ident {
            match t.sym {
                kw::SIZEOF | kw::ALIGNOF2 | kw::ALIGNOF3 | kw::ALIGNOF4 => {
                    self.bump()?;
                    let size = t.sym == kw::SIZEOF;
                    if self.at("(")? && self.second_starts_type()? {
                        self.bump()?;
                        let q = self.type_name()?;
                        self.expect(")")?;
                        if self.at("{")? {
                            let init = self.initializer()?;
                            let e = Expr::new(ExprKind::Compound(q, Box::new(init)), t.loc);
                            let e = self.tree_postfix(e)?;
                            let k = if size {
                                ExprKind::SizeofExpr(Box::new(e))
                            } else {
                                ExprKind::AlignofExpr(Box::new(e))
                            };
                            return Ok(Expr::new(k, t.loc));
                        }
                        let k = if size {
                            ExprKind::SizeofType(q)
                        } else {
                            ExprKind::AlignofType(q)
                        };
                        return Ok(Expr::new(k, t.loc));
                    }
                    let e = self.tree_unary()?;
                    let k = if size {
                        ExprKind::SizeofExpr(Box::new(e))
                    } else {
                        ExprKind::AlignofExpr(Box::new(e))
                    };
                    return Ok(Expr::new(k, t.loc));
                }
                kw::EXTENSION => {
                    self.bump()?;
                    return self.tree_cast();
                }
                kw::REAL | kw::IMAG => {
                    self.bump()?;
                    let e = self.tree_cast()?;
                    let op = if t.sym == kw::REAL {
                        UnOp::Real
                    } else {
                        UnOp::Imag
                    };
                    return Ok(Expr::new(ExprKind::Unary(op, Box::new(e)), t.loc));
                }
                _ => {}
            }
        }
        let e = self.tree_primary()?;
        self.tree_postfix(e)
    }

    fn tree_postfix(&mut self, mut e: Expr) -> R<Expr> {
        loop {
            let t = self.peek()?;
            if t.kind != Kind::Punct {
                return Ok(e);
            }
            match self.name(t.sym) {
                "[" => {
                    self.bump()?;
                    let i = self.tree_expr()?;
                    self.expect("]")?;
                    e = Expr::new(ExprKind::Index(Box::new(e), Box::new(i)), t.loc);
                }
                "(" => {
                    self.bump()?;
                    let mut args = Vec::new();
                    if !self.eat(")")? {
                        loop {
                            args.push(self.tree_assign()?);
                            if self.eat(",")? {
                                continue;
                            }
                            self.expect(")")?;
                            break;
                        }
                    }
                    e = Expr::new(ExprKind::Call(Box::new(e), args), t.loc);
                }
                "." | "->" => {
                    let arrow = self.name(t.sym) == "->";
                    self.bump()?;
                    let m = self.bump()?;
                    if m.kind != Kind::Ident {
                        return Err(self.err(m.loc, "expected a member name"));
                    }
                    let name = self.name(m.sym).to_owned();
                    e = Expr::new(ExprKind::Member(Box::new(e), name, arrow), t.loc);
                }
                "++" | "--" => {
                    self.bump()?;
                    let op = if self.name(t.sym) == "++" {
                        UnOp::PostInc
                    } else {
                        UnOp::PostDec
                    };
                    e = Expr::new(ExprKind::Unary(op, Box::new(e)), t.loc);
                }
                _ => return Ok(e),
            }
        }
    }

    fn tree_primary(&mut self) -> R<Expr> {
        let t = self.peek()?;
        match t.kind {
            Kind::Num => {
                self.bump()?;
                let v = self.number(t)?;
                let p = self.types.prim_of(v.ty.ty).unwrap_or(Prim::Int);
                Ok(match v.v {
                    Val::Int(i) => Expr::new(ExprKind::Int(i, p), t.loc),
                    Val::Float(f) => Expr::new(ExprKind::Float(f, p), t.loc),
                    _ => return Err(self.err(t.loc, "invalid number")),
                })
            }
            Kind::Char => {
                let v = self.primary()?;
                let p = self.types.prim_of(v.ty.ty).unwrap_or(Prim::Int);
                let Val::Int(i) = v.v else {
                    return Err(self.err(t.loc, "invalid character literal"));
                };
                Ok(Expr::new(ExprKind::Int(i, p), t.loc))
            }
            Kind::Str => self.tree_string(),
            Kind::Punct if self.is_p(t, "(") => {
                self.bump()?;
                if self.at("{")? {
                    let b = self.peek()?;
                    let stmts = self.block_items()?;
                    self.expect(")")?;
                    return Ok(Expr::new(ExprKind::StmtExpr(stmts), b.loc));
                }
                let e = self.tree_expr()?;
                self.expect(")")?;
                Ok(e)
            }
            Kind::Ident => self.tree_ident(),
            _ => Err(self.err(
                t.loc,
                &format!("expected an expression, found {}", self.describe(t)),
            )),
        }
    }

    /// Adjacent string literals as one, in code units of their element
    /// type.
    fn tree_string(&mut self) -> R<Expr> {
        let first = self.peek()?;
        let mut parts: Vec<(String, Vec<u8>)> = Vec::new();
        while self.peek()?.kind == Kind::Str {
            let s = self.bump()?;
            let text = self.name(s.sym).to_owned();
            let prefix = text[..text.find('"').unwrap_or(0)].to_owned();
            parts.push((prefix, str_bytes(&text)));
        }
        let t = &self.types.t;
        let elem = parts
            .iter()
            .find_map(|(p, _)| match p.as_str() {
                "L" => Some(t.wchar),
                "u" => Some(Prim::UShort),
                "U" => Some(Prim::UInt),
                _ => None,
            })
            .unwrap_or(Prim::Char);
        let wide16 = self.types.t.prim(elem).0 == 2;
        let mut units = Vec::new();
        for (_, bytes) in parts {
            if elem == Prim::Char {
                units.extend(bytes.iter().map(|&b| u32::from(b)));
                continue;
            }
            for c in String::from_utf8_lossy(&bytes).chars() {
                if wide16 {
                    let mut b = [0u16; 2];
                    units.extend(c.encode_utf16(&mut b).iter().map(|&u| u32::from(u)));
                } else {
                    units.push(c as u32);
                }
            }
        }
        Ok(Expr::new(ExprKind::Str(units, elem), first.loc))
    }

    fn tree_ident(&mut self) -> R<Expr> {
        let t = self.bump()?;
        match t.sym {
            kw::OFFSETOF | kw::TYPES_COMPATIBLE => {
                let v = self.ident_expr(t)?;
                let p = self.types.prim_of(v.ty.ty).unwrap_or(Prim::Int);
                let Val::Int(i) = v.v else {
                    return Err(self.err(t.loc, "expected an integer constant"));
                };
                return Ok(Expr::new(ExprKind::Int(i, p), t.loc));
            }
            kw::VA_ARG => {
                self.expect("(")?;
                let ap = self.tree_assign()?;
                self.expect(",")?;
                let q = self.type_name()?;
                self.expect(")")?;
                return Ok(Expr::new(ExprKind::VaArg(Box::new(ap), q), t.loc));
            }
            kw::CHOOSE_EXPR => {
                self.expect("(")?;
                let c = self.assign_expr()?;
                self.expect(",")?;
                let a = self.tree_assign()?;
                self.expect(",")?;
                let b = self.tree_assign()?;
                self.expect(")")?;
                return Ok(if self.truth(&c) == Some(false) { b } else { a });
            }
            kw::GENERIC => {
                self.expect("(")?;
                let e = self.tree_assign()?;
                let mut arms = Vec::new();
                while self.eat(",")? {
                    let q = if self.peek()?.sym == kw::DEFAULT {
                        self.bump()?;
                        None
                    } else {
                        Some(self.type_name()?)
                    };
                    self.expect(":")?;
                    arms.push((q, self.tree_assign()?));
                }
                self.expect(")")?;
                return Ok(Expr::new(ExprKind::Generic(Box::new(e), arms), t.loc));
            }
            kw::TRUE | kw::FALSE if !self.ords.contains_key(&t.sym) => {
                return Ok(Expr::new(
                    ExprKind::Int(i128::from(t.sym == kw::TRUE), Prim::Bool),
                    t.loc,
                ));
            }
            kw::NULLPTR => {
                return Ok(Expr::new(ExprKind::Int(0, Prim::Void), t.loc));
            }
            _ => {}
        }
        let name = self.name(t.sym).to_owned();
        if name == "__builtin_offsetof" {
            self.buf.push_front(Tok {
                sym: kw::OFFSETOF,
                ..t
            });
            return self.tree_ident();
        }
        if name == "__func__" || name == "__FUNCTION__" || name == "__PRETTY_FUNCTION__" {
            let f = self.current_fn.clone().unwrap_or_default();
            let units = f.bytes().map(u32::from).collect();
            return Ok(Expr::new(ExprKind::Str(units, Prim::Char), t.loc));
        }
        match self.ords.get(&t.sym).copied() {
            Some(Ord::Local(l)) => Ok(Expr::new(ExprKind::Local(l), t.loc)),
            Some(Ord::Var(v)) => Ok(Expr::new(ExprKind::Global(v), t.loc)),
            Some(Ord::Func(f)) => Ok(Expr::new(ExprKind::Func(f), t.loc)),
            Some(Ord::Enumerator(e, i)) => {
                let en = &self.types.enums[e as usize];
                let v = en.values[i as usize].value;
                let p = if i32::try_from(v).is_ok() && !en.fixed {
                    Prim::Int
                } else {
                    en.int
                        .and_then(|i| self.types.prim_of(i))
                        .unwrap_or(Prim::LongLong)
                };
                Ok(Expr::new(ExprKind::Int(v, p), t.loc))
            }
            Some(Ord::Typedef(_)) => Err(self.err(
                t.loc,
                &format!("unexpected type name '{name}': expected an expression"),
            )),
            None if is_builtin(&name) => Ok(Expr::new(ExprKind::Builtin(name), t.loc)),
            None if matches!(
                name.as_str(),
                "__try" | "__except" | "__finally" | "__leave"
            ) =>
            {
                Err(self.err(
                    t.loc,
                    &format!(
                        "`{name}` (Windows structured exception handling) is not supported by \
                         `nabi cc`; build the library without it (sqlite: \
                         `-DSQLITE_OMIT_SEH`)"
                    ),
                ))
            }
            None if self.at("(")? => Err(self.err(
                t.loc,
                &format!(
                    "call to undeclared function '{name}': include the header that declares it, \
                     or declare its prototype before the call (`int {name}(int x);`)"
                ),
            )),
            None => Err(self.err(t.loc, &format!("use of undeclared identifier '{name}'"))),
        }
    }
}

/// Names `nabi cc` knows as builtins without a declaration.
#[must_use]
pub fn is_builtin(name: &str) -> bool {
    name.starts_with("__builtin_")
        || name.starts_with("__sync_")
        || name.starts_with("__atomic_")
        || name.starts_with("_Interlocked")
        || matches!(
            name,
            "__c11_atomic_load"
                | "__c11_atomic_store"
                | "__c11_atomic_fetch_add"
                | "__c11_atomic_fetch_sub"
                | "__c11_atomic_exchange"
                | "__c11_atomic_compare_exchange_strong"
                | "__c11_atomic_compare_exchange_weak"
                | "__c11_atomic_thread_fence"
                | "_alloca"
                | "__debugbreak"
                | "_InterlockedIncrement"
                | "_InterlockedDecrement"
                | "_InterlockedExchange"
                | "_InterlockedCompareExchange"
                | "_InterlockedExchangeAdd"
                | "_ReadWriteBarrier"
                | "__lzcnt"
                | "__lzcnt64"
                | "__popcnt"
                | "__popcnt64"
                | "_byteswap_ushort"
                | "_byteswap_ulong"
                | "_byteswap_uint64"
        )
}
