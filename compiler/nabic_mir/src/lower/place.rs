//! Places, casts, payloads and carrier fields.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnBuilder<'_> {
    /// A field chain `l.f.g` of local `l` (`self.buf`): `l` and the byte
    /// offset of the field in it. `None` for anything else.
    pub(crate) fn field_place(&self, p: &HirExpr) -> Option<(LocalId, u32)> {
        let mut path = Vec::new();
        let mut cur = p;
        let root = loop {
            match &cur.kind {
                HirExprKind::Local(l) if !path.is_empty() => break *l,
                HirExprKind::FieldRead {
                    target,
                    field_index: i,
                    ..
                }
                | HirExprKind::TupleField {
                    target, index: i, ..
                } => {
                    path.push((self.ty(&target.ty), *i));
                    cur = target;
                }
                _ => return None,
            }
        };
        let offset = path
            .iter()
            .map(|(ty, i)| self.target.field_offsets(ty)[*i as usize])
            .sum();
        Some((root, offset))
    }

    /// The address of place `p`: a local or static, or a field / element
    /// of one.
    pub(crate) fn addr_place(&mut self, p: &HirExpr) -> Operand {
        let usize_ty = MirTy::Prim(PrimTy::Usize);
        let (base, off) = match &p.kind {
            HirExprKind::Local(l) => {
                let dst = self.fresh_local(usize_ty);
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::AddrOf(*l),
                });
                return Operand::Copy(dst);
            }
            HirExprKind::StaticRef(name) => {
                let dst = self.fresh_local(usize_ty);
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::StaticAddr(name.clone()),
                });
                return Operand::Copy(dst);
            }
            HirExprKind::FieldRead {
                target,
                field_index: i,
                ..
            }
            | HirExprKind::TupleField {
                target, index: i, ..
            } => {
                let off = self.target.field_offsets(&self.ty(&target.ty))[*i as usize];
                (
                    target,
                    Operand::Const(ConstVal::UInt {
                        value: off.into(),
                        ty: PrimTy::Usize,
                    }),
                )
            }
            HirExprKind::ArrayIndex {
                array,
                index,
                elem_ty,
            } => {
                let size = self.target.size_align(&self.ty(elem_ty)).0;
                let mut i = self.lower_expr_to_operand(index);
                if index.ty != HirTy::Prim(PrimTy::Usize) {
                    let t = self.fresh_local(usize_ty.clone());
                    self.push_stmt(Statement::Assign {
                        target: t,
                        rvalue: RValue::Cast {
                            source: i,
                            from_ty: self.ty(&index.ty),
                            to_ty: usize_ty.clone(),
                        },
                    });
                    i = Operand::Copy(t);
                }
                let off = self.fresh_local(usize_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: off,
                    rvalue: RValue::BinOp {
                        op: PrimOp::WrappingMul,
                        op_ty: usize_ty.clone(),
                        lhs: i,
                        rhs: Operand::Const(ConstVal::UInt {
                            value: size.into(),
                            ty: PrimTy::Usize,
                        }),
                    },
                });
                (array, Operand::Copy(off))
            }
            _ => unreachable!("addr_of: not a place"),
        };
        let base = self.addr_place(base);
        let dst = self.fresh_local(usize_ty.clone());
        self.push_stmt(Statement::Assign {
            target: dst,
            rvalue: RValue::BinOp {
                op: PrimOp::WrappingAdd,
                op_ty: usize_ty,
                lhs: base,
                rhs: off,
            },
        });
        Operand::Copy(dst)
    }

    // No-op when the types match; otherwise insert a `Cast` rvalue
    // (zext / sext / trunc selected by codegen from from_ty/to_ty).
    pub(crate) fn cast_to(&mut self, source: Operand, from_ty: &MirTy, to_ty: &MirTy) -> Operand {
        if from_ty == to_ty {
            return source;
        }
        let dst = self.fresh_local(to_ty.clone());
        self.push_stmt(Statement::Assign {
            target: dst,
            rvalue: RValue::Cast {
                source,
                from_ty: from_ty.clone(),
                to_ty: to_ty.clone(),
            },
        });
        Operand::Copy(dst)
    }

    /// Call a function value `{code, env}`: `code(env, args..)`.
    pub(crate) fn call_fn_value(
        &mut self,
        callee: Operand,
        args: &[HirExpr],
        ret_ty: &HirTy,
    ) -> RValue {
        let arg_tys: Vec<MirTy> = args.iter().map(|a| self.ty(&a.ty)).collect();
        let ret = self.ty(ret_ty);
        let code_t = code_ty(arg_tys.clone(), ret.clone());
        let code = self.fresh_local(code_t);
        self.push_stmt(Statement::Assign {
            target: code,
            rvalue: RValue::FieldRead {
                target: callee.clone(),
                index: 0,
                field_ty: code_ty(arg_tys.clone(), ret.clone()),
            },
        });
        let env = self.fresh_local(MirTy::GcRef);
        self.push_stmt(Statement::Assign {
            target: env,
            rvalue: RValue::FieldRead {
                target: callee,
                index: 1,
                field_ty: MirTy::GcRef,
            },
        });
        let mut call_args = vec![Operand::Copy(env)];
        call_args.extend(args.iter().map(|a| self.lower_expr_to_operand(a)));
        let mut all_tys = vec![MirTy::GcRef];
        all_tys.extend(arg_tys);
        RValue::CallIndirect {
            callee: Operand::Copy(code),
            args: call_args,
            arg_tys: all_tys,
            ret_ty: ret,
            abi: Abi::Nabi,
        }
    }

    /// Each payload argument of enum variant `scrut` in its own local
    /// (`None` when the variant carries no matching payload).
    pub(crate) fn variant_arg_locals(
        &mut self,
        enum_id: nabic_hir::EnumId,
        variant_index: u32,
        scrut: LocalId,
        n_args: usize,
    ) -> Option<Vec<(LocalId, MirTy)>> {
        if n_args == 0 {
            return None;
        }
        let MirTy::Tuple(parts) = self.locals[scrut.0 as usize].ty.clone() else {
            return None;
        };
        if parts.len() != 2 {
            return None;
        }
        let unified_ty = parts[1].clone();
        let payload = self.fresh_local(unified_ty.clone());
        self.push_stmt(Statement::Assign {
            target: payload,
            rvalue: RValue::FieldRead {
                target: Operand::Copy(scrut),
                index: 1,
                field_ty: unified_ty.clone(),
            },
        });
        let variant_ty = self.variant_payload_ty(enum_id, variant_index);
        let v = self.payload_cast(Operand::Copy(payload), &unified_ty, &variant_ty);
        let local = match v {
            Operand::Copy(l) => l,
            c @ Operand::Const(_) => {
                let l = self.fresh_local(variant_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: l,
                    rvalue: RValue::Use(c),
                });
                l
            }
        };
        // A named-field variant's payload is the tuple of its fields,
        // even with one field.
        let named = !self.module.enums[enum_id.0 as usize].variants[variant_index as usize]
            .named_fields
            .is_empty();
        if n_args == 1 && !named {
            return Some(vec![(local, variant_ty)]);
        }
        let MirTy::Tuple(field_tys) = variant_ty else {
            return None;
        };
        if field_tys.len() != n_args {
            return None;
        }
        Some(
            field_tys
                .into_iter()
                .enumerate()
                .map(|(i, field_ty)| {
                    let elem = self.fresh_local(field_ty.clone());
                    self.push_stmt(Statement::Assign {
                        target: elem,
                        rvalue: RValue::FieldRead {
                            target: Operand::Copy(local),
                            index: u32::try_from(i).expect("field index overflow"),
                            field_ty: field_ty.clone(),
                        },
                    });
                    (elem, field_ty)
                })
                .collect(),
        )
    }

    pub(crate) fn variant_payload_ty(
        &self,
        enum_id: nabic_hir::EnumId,
        variant_index: u32,
    ) -> MirTy {
        self.module.enums[enum_id.0 as usize].variants[variant_index as usize]
            .payload_ty
            .as_ref()
            .map_or(MirTy::Unit, |t| self.ty(t))
    }

    /// A union field write / read reinterprets bits (an
    /// `f32` written and read as `i32` is its IEEE bits, not `1`).
    pub(crate) fn union_bits(
        &mut self,
        source: Operand,
        from_ty: &MirTy,
        to_ty: &MirTy,
    ) -> Operand {
        self.payload_cast(source, from_ty, to_ty)
    }

    /// The MIR type of a union's one storage field.
    pub(crate) fn union_storage_ty(&self, id: nabic_hir::StructId) -> MirTy {
        match self.ty(&HirTy::Struct(id)) {
            MirTy::Struct(_, mut f) if f.len() == 1 => f.remove(0),
            _ => MirTy::Unit,
        }
    }

    /// The MIR fields of a tuple of HIR type `hir_ty` (HIR `fields`,
    /// lowered to `ops`). Of a carrier `(is_err, ok, err)`, the constant
    /// tag picks which value fills the payload; other tuples and carriers
    /// without an `ok` slot stay as they are.
    pub(crate) fn carrier_fields(
        &mut self,
        hir_ty: &HirTy,
        fields: &[HirExpr],
        mut ops: Vec<Operand>,
        ty: &MirTy,
    ) -> Vec<Operand> {
        if !matches!(hir_ty, HirTy::Throws { .. }) {
            return ops;
        }
        let MirTy::Tuple(parts) = ty else {
            unreachable!("a carrier is a tuple")
        };
        if fields.len() != 3 {
            return ops;
        }
        let HirExprKind::Const(ConstVal::Bool(is_err)) = fields[0].kind else {
            unreachable!("a carrier's tag is constant")
        };
        let i = if is_err { 2 } else { 1 };
        let v = ops.swap_remove(i);
        let from = self.ty(&fields[i].ty);
        let payload = self.payload_cast(v, &from, &parts[1]);
        let tag = if parts[0] == MirTy::Prim(IrPrimTy::I32) {
            Operand::Const(ConstVal::Int {
                value: i128::from(is_err),
                ty: IrPrimTy::I32,
            })
        } else {
            Operand::Const(ConstVal::Bool(is_err))
        };
        vec![tag, payload]
    }

    /// Field `index` of a tuple `target` of HIR type `ty`; of a carrier,
    /// through [`Self::carrier_field`].
    pub(crate) fn tuple_field(
        &mut self,
        target: Operand,
        ty: &HirTy,
        index: u32,
        field_ty: &HirTy,
    ) -> Operand {
        let mir_field_ty = self.ty(field_ty);
        let (index, slot_ty) = self
            .carrier_field(ty, index)
            .unwrap_or_else(|| (index, mir_field_ty.clone()));
        let dst = self.fresh_local(slot_ty.clone());
        self.push_stmt(Statement::Assign {
            target: dst,
            rvalue: RValue::FieldRead {
                target,
                index,
                field_ty: slot_ty.clone(),
            },
        });
        // An `i32` carrier tag read as `is_err`.
        if index == 0 && slot_ty != mir_field_ty {
            let b = self.fresh_local(mir_field_ty);
            self.push_stmt(Statement::Assign {
                target: b,
                rvalue: RValue::BinOp {
                    op: PrimOp::Ne,
                    op_ty: slot_ty,
                    lhs: Operand::Copy(dst),
                    rhs: Operand::Const(ConstVal::Int {
                        value: 0,
                        ty: IrPrimTy::I32,
                    }),
                },
            });
            return Operand::Copy(b);
        }
        self.payload_cast(Operand::Copy(dst), &slot_ty, &mir_field_ty)
    }

    /// HIR field `index` of a carrier `ty` (`(is_err, ok, err)`, no
    /// unit `ok`): its MIR field and that field's type. `None` for any
    /// other `ty`.
    pub(crate) fn carrier_field(&self, ty: &HirTy, index: u32) -> Option<(u32, MirTy)> {
        let HirTy::Throws { .. } = ty else {
            return None;
        };
        let MirTy::Tuple(parts) = self.ty(ty) else {
            unreachable!("a carrier is a tuple")
        };
        let i = index.min(1);
        Some((i, parts[i as usize].clone()))
    }

    /// Move a value into / out of a shared payload slot, keeping bits.
    pub(crate) fn payload_cast(
        &mut self,
        source: Operand,
        from_ty: &MirTy,
        to_ty: &MirTy,
    ) -> Operand {
        if from_ty == to_ty {
            return source;
        }
        let dst = self.fresh_local(to_ty.clone());
        self.push_stmt(Statement::Assign {
            target: dst,
            rvalue: RValue::Bitcast {
                source,
                from_ty: from_ty.clone(),
                to_ty: to_ty.clone(),
            },
        });
        Operand::Copy(dst)
    }
}
