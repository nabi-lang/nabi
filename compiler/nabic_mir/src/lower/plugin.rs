//! Plugins in lowering: a plugin's code reads its `any` tags from the
//! table the loader fills.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;
use nabic_hir::plugin::Mode;

/// The functions the module descriptor takes as values: a plugin's
/// exports, every dispatcher of a plugin or host.
pub(crate) fn valued(module: &HirModule) -> Vec<FunctionId> {
    let p = &module.plugin;
    let exports = p.exports.iter().map(|e| e.id);
    exports.chain(p.dispatchers.iter().map(|d| d.id)).collect()
}

impl FnBuilder<'_> {
    /// `any` tag `tag` (`boxed`: with `ANY_BOXED`): a constant, in a
    /// plugin loaded from its tag table.
    pub(crate) fn any_tag_op(&mut self, tag: u32, boxed: bool) -> Operand {
        let bits = if boxed {
            tag | crate::gc_abi::ANY_BOXED
        } else {
            tag
        };
        let konst = Operand::Const(ConstVal::Int {
            value: i128::from(bits as i32),
            ty: PrimTy::I32,
        });
        if self.module.plugin.mode != Mode::Plugin {
            return konst;
        }
        let Some(i) = self.module.any_tags.iter().position(|(_, t)| *t == tag) else {
            return konst;
        };
        let (word, i32_) = (MirTy::Prim(PrimTy::Usize), MirTy::Prim(PrimTy::I32));
        let [base, at] = [(); 2].map(|()| self.fresh_local(word.clone()));
        self.push_stmt(Statement::Assign {
            target: base,
            rvalue: RValue::StaticAddr(crate::plugin::TAGS.to_owned()),
        });
        self.push_stmt(Statement::Assign {
            target: at,
            rvalue: RValue::BinOp {
                op: PrimOp::Add,
                op_ty: word,
                lhs: Operand::Copy(base),
                rhs: Operand::Const(ConstVal::Int {
                    value: 4 * i as i128,
                    ty: PrimTy::Usize,
                }),
            },
        });
        let t = self.fresh_local(i32_.clone());
        self.push_stmt(Statement::Assign {
            target: t,
            rvalue: RValue::RawLoad {
                addr: Operand::Copy(at),
                ty: i32_.clone(),
                volatile: false,
            },
        });
        if !boxed {
            return Operand::Copy(t);
        }
        let b = self.fresh_local(i32_.clone());
        self.push_stmt(Statement::Assign {
            target: b,
            rvalue: RValue::BinOp {
                op: PrimOp::BitOr,
                op_ty: i32_,
                lhs: Operand::Copy(t),
                rhs: Operand::Const(ConstVal::Int {
                    value: i128::from(crate::gc_abi::ANY_BOXED as i32),
                    ty: PrimTy::I32,
                }),
            },
        });
        Operand::Copy(b)
    }

    /// `tag == value` for variant pattern `pat`: an `any`'s tag in a
    /// plugin is the loaded one.
    pub(crate) fn tag_eq(
        &mut self,
        pat: &HirPattern,
        tag: Operand,
        value: &ConstVal,
        ty: PrimTy,
    ) -> Operand {
        let any = matches!(
            pat,
            HirPattern::SumVariant {
                sum_ty: HirTy::AnyTrait(_),
                ..
            }
        );
        let (HirPattern::SumVariant { variant_index, .. }, true, Mode::Plugin) =
            (pat, any, self.module.plugin.mode)
        else {
            return self.build_eq_op(tag, value, ty);
        };
        let want = self.any_tag_op(*variant_index, false);
        let result = self.fresh_local(MirTy::Prim(PrimTy::Bool));
        self.push_stmt(Statement::Assign {
            target: result,
            rvalue: RValue::BinOp {
                op: PrimOp::Eq,
                op_ty: MirTy::Prim(ty),
                lhs: tag,
                rhs: want,
            },
        });
        Operand::Copy(result)
    }

    /// `code as F` (`Plugin::get`): function value `{code, null}` of nabi
    /// function type `F`.
    pub(crate) fn fn_from_code(
        &mut self,
        code: &Operand,
        from: &HirTy,
        to: &HirTy,
    ) -> Option<RValue> {
        if *from != HirTy::Prim(PrimTy::Usize) || !matches!(to, HirTy::Fn(..)) || to.is_c_fn() {
            return None;
        }
        let MirTy::Tuple(parts) = self.ty(to) else {
            return None;
        };
        let c = self.fresh_local(parts[0].clone());
        self.push_stmt(Statement::Assign {
            target: c,
            rvalue: RValue::Bitcast {
                source: code.clone(),
                from_ty: MirTy::Prim(PrimTy::Usize),
                to_ty: parts[0].clone(),
            },
        });
        Some(RValue::StructInit {
            ty: MirTy::Tuple(parts),
            fields: vec![
                Operand::Copy(c),
                Operand::Const(ConstVal::Int {
                    value: 0,
                    ty: PrimTy::Usize,
                }),
            ],
        })
    }
}
