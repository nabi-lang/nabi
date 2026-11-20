//! Initializers: expressions and braced lists with designators.

#[allow(clippy::wildcard_imports)]
use crate::*;

impl Parser<'_, '_> {
    /// `ty` with an array of unknown size completed by `init`'s length
    /// (`sizeof` of the object may follow at once).
    pub(crate) fn complete(&mut self, ty: QualType, init: &Init) -> QualType {
        let c = self.types.canon(ty.ty);
        let Type::Array { elem, len: None } = self.types.get(c).clone() else {
            return ty;
        };
        let n = match init {
            Init::Expr(Expr {
                kind: ExprKind::Str(u, _),
                ..
            }) => u.len() as u64 + 1,
            Init::List(items, _) => {
                if let [
                    (
                        d,
                        Init::Expr(Expr {
                            kind: ExprKind::Str(u, _),
                            ..
                        }),
                    ),
                ] = items.as_slice()
                {
                    if d.is_empty()
                        && self.types.size_align(elem.ty).is_some_and(|x| x.0 <= 4)
                        && self.types.prim_of(elem.ty).is_some()
                    {
                        return QualType {
                            ty: self.types.add(Type::Array {
                                elem,
                                len: Some(u.len() as u64 + 1),
                            }),
                            ..ty
                        };
                    }
                }
                let (mut i, mut max) = (0u64, 0u64);
                for (d, _) in items {
                    match d.first() {
                        Some(
                            Desig::Index(Expr {
                                kind: ExprKind::Int(v, _),
                                ..
                            })
                            | Desig::Range(
                                _,
                                Expr {
                                    kind: ExprKind::Int(v, _),
                                    ..
                                },
                            ),
                        ) => i = *v as u64,
                        Some(Desig::Index(_) | Desig::Range(..)) => return ty,
                        _ => {}
                    }
                    i += 1;
                    max = max.max(i);
                }
                max
            }
            Init::Expr(_) => return ty,
        };
        QualType {
            ty: self.types.add(Type::Array { elem, len: Some(n) }),
            ..ty
        }
    }

    /// An initializer: an assignment expression or `{ .. }`.
    pub fn initializer(&mut self) -> R<Init> {
        if !self.at("{")? {
            return Ok(Init::Expr(self.tree_assign()?));
        }
        let open = self.bump()?;
        let mut items = Vec::new();
        loop {
            if self.eat("}")? {
                break;
            }
            let mut desig = Vec::new();
            let t = self.peek()?;
            // GNU `field: value`.
            if t.kind == Kind::Ident && !is_reserved(t.sym) && self.second_is(":")? {
                self.bump()?;
                self.bump()?;
                desig.push(Desig::Field(self.name(t.sym).to_owned(), t.loc));
            } else {
                loop {
                    let t = self.peek()?;
                    if self.is_p(t, ".") {
                        self.bump()?;
                        let m = self.bump()?;
                        if m.kind != Kind::Ident {
                            return Err(self.err(m.loc, "expected a member name after '.'"));
                        }
                        desig.push(Desig::Field(self.name(m.sym).to_owned(), m.loc));
                    } else if self.is_p(t, "[") {
                        self.bump()?;
                        let a = self.tree_cond()?;
                        if self.eat("...")? {
                            let b = self.tree_cond()?;
                            desig.push(Desig::Range(a, b));
                        } else {
                            desig.push(Desig::Index(a));
                        }
                        self.expect("]")?;
                    } else {
                        break;
                    }
                }
                if !desig.is_empty() {
                    self.expect("=")?;
                }
            }
            items.push((desig, self.initializer()?));
            if self.eat(",")? {
                continue;
            }
            self.expect("}")?;
            break;
        }
        Ok(Init::List(items, open.loc))
    }
}
