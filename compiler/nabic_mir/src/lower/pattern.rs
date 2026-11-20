//! Pattern tests, tags and bindings.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

/// `p` binds a local.
fn binds(p: &HirPattern) -> bool {
    let mut any = false;
    p.each_binding(&mut |_| any = true);
    any
}

impl FnBuilder<'_> {
    /// Or-pattern test. Chain: the first alt that matches lands in a
    /// per-alt success block, runs its own `bind_pattern`, then jumps to
    /// the arm body. Each alt's bindings reach the arm body
    /// through the same shared locals (allocated in HIR lowering). No
    /// alts match nothing (a type test decided false per instance).
    pub(crate) fn lower_or_test(
        &mut self,
        alts: &[HirPattern],
        scrut_local: LocalId,
        scrut_ty: &MirTy,
        arm_body_bb: BlockId,
        next_bb: BlockId,
    ) {
        if alts.is_empty() {
            self.terminate(Terminator::Goto {
                target: next_bb,
                args: Vec::new(),
            });
        }
        for (i, alt) in alts.iter().enumerate() {
            let local_next = if i + 1 < alts.len() {
                self.new_block()
            } else {
                next_bb
            };
            let alt_success_bb = self.new_block();
            self.lower_pattern_test(alt, scrut_local, scrut_ty, alt_success_bb, local_next);
            self.current = alt_success_bb;
            self.bind_pattern(alt, scrut_local);
            self.terminate(Terminator::Goto {
                target: arm_body_bb,
                args: Vec::new(),
            });
            if i + 1 < alts.len() {
                self.current = local_next;
            }
        }
    }

    /// Emit the comparison(s) that decide whether an arm matches. On a
    /// match the fall-through reaches `arm_body_bb`; otherwise it
    /// reaches `next_bb`. The current block is fully terminated.
    pub(crate) fn lower_pattern_test(
        &mut self,
        pat: &HirPattern,
        scrut_local: LocalId,
        scrut_ty: &MirTy,
        arm_body_bb: BlockId,
        next_bb: BlockId,
    ) {
        match pat {
            HirPattern::Wildcard | HirPattern::Binding { .. } => {
                self.terminate(Terminator::Goto {
                    target: arm_body_bb,
                    args: Vec::new(),
                });
            }
            HirPattern::At { sub, .. } => {
                self.lower_pattern_test(sub, scrut_local, scrut_ty, arm_body_bb, next_bb);
            }
            HirPattern::Literal { value, prim } => {
                let cond = self.build_eq(scrut_local, value, *prim);
                self.terminate(Terminator::If {
                    cond,
                    then_bb: arm_body_bb,
                    then_args: Vec::new(),
                    else_bb: next_bb,
                    else_args: Vec::new(),
                });
            }
            HirPattern::Range {
                start,
                end,
                inclusive,
                prim,
            } => {
                // Check `lo <= x` and (`x < hi` | `x <= hi`); each open
                // bound is omitted. Empty range matches everything.
                if start.is_none() && end.is_none() {
                    self.terminate(Terminator::Goto {
                        target: arm_body_bb,
                        args: Vec::new(),
                    });
                    return;
                }
                let bounds: Vec<(PrimOp, &ConstVal)> = {
                    let mut v: Vec<(PrimOp, &ConstVal)> = Vec::new();
                    if let Some(s) = start {
                        v.push((PrimOp::Le, s));
                    }
                    if let Some(e) = end {
                        v.push((if *inclusive { PrimOp::Ge } else { PrimOp::Gt }, e));
                    }
                    v
                };
                for (i, (op, value)) in bounds.iter().enumerate() {
                    let success_bb = if i + 1 < bounds.len() {
                        self.new_block()
                    } else {
                        arm_body_bb
                    };
                    // A bound `(op, v)` tests `v <op> x`: `(Le, lo)` is
                    // `lo <= x`, `(Gt, hi)` is `hi > x`.
                    let cond = self.build_cmp_const_op(*op, value, scrut_local, *prim);
                    self.terminate(Terminator::If {
                        cond,
                        then_bb: success_bb,
                        then_args: Vec::new(),
                        else_bb: next_bb,
                        else_args: Vec::new(),
                    });
                    if i + 1 < bounds.len() {
                        self.current = success_bb;
                    }
                }
            }
            HirPattern::Or(alts) => {
                self.lower_or_test(alts, scrut_local, scrut_ty, arm_body_bb, next_bb);
            }
            HirPattern::Struct { fields, .. } => {
                let MirTy::Struct(_, elem_tys) = scrut_ty.clone() else {
                    self.terminate(Terminator::Goto {
                        target: arm_body_bb,
                        args: Vec::new(),
                    });
                    return;
                };
                if fields.is_empty() {
                    self.terminate(Terminator::Goto {
                        target: arm_body_bb,
                        args: Vec::new(),
                    });
                    return;
                }
                // Pull each named field once, then chain its sub-test.
                let mut field_locals: Vec<(LocalId, MirTy)> = Vec::with_capacity(fields.len());
                for (idx, _) in fields {
                    let Some(field_ty) = elem_tys.get(*idx as usize).cloned() else {
                        self.terminate(Terminator::Goto {
                            target: next_bb,
                            args: Vec::new(),
                        });
                        return;
                    };
                    let local = self.fresh_local(field_ty.clone());
                    self.push_stmt(Statement::Assign {
                        target: local,
                        rvalue: RValue::FieldRead {
                            target: Operand::Copy(scrut_local),
                            index: *idx,
                            field_ty: field_ty.clone(),
                        },
                    });
                    field_locals.push((local, field_ty));
                }
                for (i, (_, sub_pat)) in fields.iter().enumerate() {
                    let success_bb = if i + 1 < fields.len() {
                        self.new_block()
                    } else {
                        arm_body_bb
                    };
                    let (local, ty) = field_locals[i].clone();
                    self.lower_pattern_test(sub_pat, local, &ty, success_bb, next_bb);
                    if i + 1 < fields.len() {
                        self.current = success_bb;
                    }
                }
            }
            HirPattern::Tuple(sub_pats) => {
                let MirTy::Tuple(elem_tys) = scrut_ty.clone() else {
                    self.terminate(Terminator::Goto {
                        target: arm_body_bb,
                        args: Vec::new(),
                    });
                    return;
                };
                if sub_pats.len() != elem_tys.len() {
                    self.terminate(Terminator::Goto {
                        target: next_bb,
                        args: Vec::new(),
                    });
                    return;
                }
                // Extract each tuple element into a fresh local up front so
                // sub-tests can run against scalar scrutinees.
                let mut elem_locals: Vec<LocalId> = Vec::with_capacity(elem_tys.len());
                for (i, elem_ty) in elem_tys.iter().enumerate() {
                    let local = self.fresh_local(elem_ty.clone());
                    self.push_stmt(Statement::Assign {
                        target: local,
                        rvalue: RValue::FieldRead {
                            target: Operand::Copy(scrut_local),
                            index: u32::try_from(i).expect("tuple index overflow"),
                            field_ty: elem_ty.clone(),
                        },
                    });
                    elem_locals.push(local);
                }
                // Chain sub-tests: any failure jumps to next_bb; the final
                // success falls through to arm_body_bb.
                for (i, sub_pat) in sub_pats.iter().enumerate() {
                    let success_bb = if i + 1 < sub_pats.len() {
                        self.new_block()
                    } else {
                        arm_body_bb
                    };
                    self.lower_pattern_test(
                        sub_pat,
                        elem_locals[i],
                        &elem_tys[i],
                        success_bb,
                        next_bb,
                    );
                    if i + 1 < sub_pats.len() {
                        self.current = success_bb;
                    }
                }
            }
            HirPattern::Slice(s) => self.slice_test(s, scrut_local, arm_body_bb, next_bb),
            HirPattern::SumMember {
                sum_ty,
                variant_index,
                payload_ty,
                sub,
            } => {
                let tag_pat = HirPattern::SumVariant {
                    sum_ty: sum_ty.clone(),
                    variant_index: *variant_index,
                    binder: None,
                    payload_ty: None,
                };
                let tag_ok = self.new_block();
                self.lower_pattern_test(&tag_pat, scrut_local, scrut_ty, tag_ok, next_bb);
                self.current = tag_ok;
                let member_ty = self.ty(payload_ty);
                let member = self.fresh_local(member_ty.clone());
                self.sum_payload(scrut_local, sum_ty, Some(payload_ty), member);
                self.lower_pattern_test(sub, member, &member_ty, arm_body_bb, next_bb);
            }
            HirPattern::EnumVariant { variant_index, .. }
            | HirPattern::SumVariant { variant_index, .. } => {
                // Tag check; payload binding (if any) happens in
                // `bind_pattern` after the arm body block is selected.
                let stored = match pat {
                    HirPattern::SumVariant { sum_ty, .. } => {
                        i128::from(sum_tag(sum_ty, *variant_index))
                    }
                    HirPattern::EnumVariant { enum_id, .. } => {
                        self.module.enums[enum_id.0 as usize].variants[*variant_index as usize].tag
                    }
                    _ => i128::from(*variant_index),
                };
                let tag_ty = variant_tag_ty(pat, scrut_ty);
                let value = ConstVal::Int {
                    value: stored,
                    ty: tag_ty,
                };
                let tag = self.pattern_tag(pat, scrut_local, scrut_ty);
                let cond = self.tag_eq(pat, tag, &value, tag_ty);
                // Refutable payload sub-patterns (`E::A(1)`) are tested
                // after the tag.
                let nested = match pat {
                    HirPattern::EnumVariant {
                        enum_id,
                        variant_index,
                        args,
                    } if args.iter().any(|a| !a.is_irrefutable()) => {
                        Some((*enum_id, *variant_index, args))
                    }
                    _ => None,
                };
                let tag_ok_bb = if nested.is_some() {
                    self.new_block()
                } else {
                    arm_body_bb
                };
                self.terminate(Terminator::If {
                    cond,
                    then_bb: tag_ok_bb,
                    then_args: Vec::new(),
                    else_bb: next_bb,
                    else_args: Vec::new(),
                });
                let Some((enum_id, variant_index, args)) = nested else {
                    return;
                };
                self.current = tag_ok_bb;
                let Some(arg_locals) =
                    self.variant_arg_locals(enum_id, variant_index, scrut_local, args.len())
                else {
                    self.terminate(Terminator::Goto {
                        target: arm_body_bb,
                        args: Vec::new(),
                    });
                    return;
                };
                for (i, (sub_pat, (local, ty))) in args.iter().zip(&arg_locals).enumerate() {
                    let success_bb = if i + 1 < args.len() {
                        self.new_block()
                    } else {
                        arm_body_bb
                    };
                    self.lower_pattern_test(sub_pat, *local, ty, success_bb, next_bb);
                    if i + 1 < args.len() {
                        self.current = success_bb;
                    }
                }
            }
        }
    }

    /// `whole` and `len` of slice pattern `s` on `scrut`.
    fn slice_whole(&mut self, s: &nabic_hir::SlicePattern, scrut: LocalId) {
        self.push_stmt(Statement::Assign {
            target: s.whole,
            rvalue: RValue::Use(Operand::Copy(scrut)),
        });
        let n = self.lower_expr_to_operand(&s.len_expr);
        self.push_stmt(Statement::Assign {
            target: s.len,
            rvalue: RValue::Use(n),
        });
    }

    /// An item of a slice pattern, read into a fresh local.
    fn slice_item(&mut self, e: &HirExpr) -> LocalId {
        let v = self.lower_expr_to_operand(e);
        let ty = self.ty(&e.ty);
        let local = self.fresh_local(ty);
        self.push_stmt(Statement::Assign {
            target: local,
            rvalue: RValue::Use(v),
        });
        local
    }

    /// The length test of slice pattern `s`, then each refutable item's.
    fn slice_test(
        &mut self,
        s: &nabic_hir::SlicePattern,
        scrut: LocalId,
        arm_body_bb: BlockId,
        next_bb: BlockId,
    ) {
        self.slice_whole(s, scrut);
        let usize_ty = MirTy::Prim(PrimTy::Usize);
        let fits = self.fresh_local(MirTy::Prim(PrimTy::Bool));
        self.push_stmt(Statement::Assign {
            target: fits,
            rvalue: RValue::BinOp {
                op: if s.rest.is_some() {
                    PrimOp::Ge
                } else {
                    PrimOp::Eq
                },
                op_ty: usize_ty,
                lhs: Operand::Copy(s.len),
                rhs: Operand::Const(ConstVal::UInt {
                    value: u128::from(s.min_len()),
                    ty: PrimTy::Usize,
                }),
            },
        });
        let tests: Vec<&(HirExpr, HirPattern)> =
            s.items().filter(|(_, p)| !p.is_irrefutable()).collect();
        let first = if tests.is_empty() {
            arm_body_bb
        } else {
            self.new_block()
        };
        self.terminate(Terminator::If {
            cond: Operand::Copy(fits),
            then_bb: first,
            then_args: Vec::new(),
            else_bb: next_bb,
            else_args: Vec::new(),
        });
        for (i, (e, p)) in tests.iter().enumerate() {
            self.current = if i == 0 { first } else { self.current };
            let success_bb = if i + 1 < tests.len() {
                self.new_block()
            } else {
                arm_body_bb
            };
            let item = self.slice_item(e);
            let ty = self.locals[item.0 as usize].ty.clone();
            self.lower_pattern_test(p, item, &ty, success_bb, next_bb);
            if i + 1 < tests.len() {
                self.current = success_bb;
            }
        }
    }

    /// The stored tag a variant pattern tests: `extract_tag`, or a niche
    /// sum's `niche_tag`.
    pub(crate) fn pattern_tag(
        &mut self,
        pat: &HirPattern,
        scrut: LocalId,
        scrut_ty: &MirTy,
    ) -> Operand {
        if let HirPattern::SumVariant { sum_ty, .. } = pat {
            if let Some((_, n)) = self.niche(sum_ty) {
                return self.niche_tag(Operand::Copy(scrut), scrut_ty, &n);
            }
        }
        self.extract_tag(scrut, scrut_ty)
    }

    // Read the tag from an enum scrutinee. Payload-less enums are i32
    // outright; payload-bearing enums are `(i32, payload)` tuples; an
    // `any`'s tag loses its `ANY_BOXED` bit.
    pub(crate) fn extract_tag(&mut self, scrut: LocalId, scrut_ty: &MirTy) -> Operand {
        if let MirTy::Struct(crate::ANY_STRUCT, _) = scrut_ty {
            let i32_ = MirTy::Prim(IrPrimTy::I32);
            let [raw, dst] = [(); 2].map(|()| self.fresh_local(i32_.clone()));
            self.push_stmt(Statement::Assign {
                target: raw,
                rvalue: RValue::FieldRead {
                    target: Operand::Copy(scrut),
                    index: 0,
                    field_ty: i32_.clone(),
                },
            });
            self.push_stmt(Statement::Assign {
                target: dst,
                rvalue: RValue::BinOp {
                    op: PrimOp::BitAnd,
                    op_ty: i32_,
                    lhs: Operand::Copy(raw),
                    rhs: Operand::Const(ConstVal::Int {
                        value: i128::from(!crate::gc_abi::ANY_BOXED),
                        ty: IrPrimTy::I32,
                    }),
                },
            });
            return Operand::Copy(dst);
        }
        match scrut_ty {
            MirTy::Tuple(parts) if parts.len() == 2 => {
                let dst = self.fresh_local(parts[0].clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::FieldRead {
                        target: Operand::Copy(scrut),
                        index: 0,
                        field_ty: parts[0].clone(),
                    },
                });
                Operand::Copy(dst)
            }
            _ => Operand::Copy(scrut),
        }
    }

    pub(crate) fn build_eq_op(
        &mut self,
        lhs: Operand,
        value: &ConstVal,
        prim: IrPrimTy,
    ) -> Operand {
        let result = self.fresh_local(MirTy::Prim(IrPrimTy::Bool));
        self.push_stmt(Statement::Assign {
            target: result,
            rvalue: RValue::BinOp {
                op: PrimOp::Eq,
                op_ty: MirTy::Prim(prim),
                lhs,
                rhs: Operand::Const(value.clone()),
            },
        });
        Operand::Copy(result)
    }

    /// Emit `value <op> scrut` as a primitive compare returning bool.
    /// Used by range tests where the bound is the constant.
    pub(crate) fn build_cmp_const_op(
        &mut self,
        op: PrimOp,
        value: &ConstVal,
        scrut: LocalId,
        prim: PrimTy,
    ) -> Operand {
        let result = self.fresh_local(MirTy::Prim(PrimTy::Bool));
        self.push_stmt(Statement::Assign {
            target: result,
            rvalue: RValue::BinOp {
                op,
                op_ty: MirTy::Prim(prim),
                lhs: Operand::Const(value.clone()),
                rhs: Operand::Copy(scrut),
            },
        });
        Operand::Copy(result)
    }

    pub(crate) fn build_eq(&mut self, scrut: LocalId, value: &ConstVal, prim: PrimTy) -> Operand {
        let result = self.fresh_local(MirTy::Prim(PrimTy::Bool));
        self.push_stmt(Statement::Assign {
            target: result,
            rvalue: RValue::BinOp {
                op: PrimOp::Eq,
                op_ty: MirTy::Prim(prim),
                lhs: Operand::Copy(scrut),
                rhs: Operand::Const(value.clone()),
            },
        });
        Operand::Copy(result)
    }

    /// Store the payload of sum `scrut` (member `hir_payload`) into `local`.
    pub(crate) fn sum_payload(
        &mut self,
        scrut: LocalId,
        sum_ty: &HirTy,
        hir_payload: Option<&HirTy>,
        local: LocalId,
    ) {
        // `n: none` holds no payload: bind the `none` value.
        if matches!(hir_payload, Some(HirTy::None)) {
            self.push_stmt(Statement::Assign {
                target: local,
                rvalue: RValue::Use(Operand::Const(ConstVal::None)),
            });
            return;
        }
        if let Some((member, n)) = self.niche(sum_ty) {
            let v = self.niche_unwrap(Operand::Copy(scrut), &member, &n);
            self.push_stmt(Statement::Assign {
                target: local,
                rvalue: RValue::Use(v),
            });
            return;
        }
        let scrut_ty = self.locals[scrut.0 as usize].ty.clone();
        let (MirTy::Tuple(parts) | MirTy::Struct(_, parts)) = &scrut_ty else {
            return;
        };
        if parts.len() != 2 {
            return;
        }
        let payload_ty = parts[1].clone();
        let payload = self.fresh_local(payload_ty.clone());
        self.push_stmt(Statement::Assign {
            target: payload,
            rvalue: RValue::FieldRead {
                target: Operand::Copy(scrut),
                index: 1,
                field_ty: payload_ty.clone(),
            },
        });
        // The payload slot back to the binder's member type.
        let bind_ty = self.locals[local.0 as usize].ty.clone();
        if matches!(sum_ty, HirTy::AnyTrait(_)) {
            self.push_stmt(Statement::Assign {
                target: local,
                rvalue: RValue::HeapLoad {
                    ptr: Operand::Copy(payload),
                    ty: bind_ty,
                },
            });
            return;
        }
        let v = self.payload_cast(Operand::Copy(payload), &payload_ty, &bind_ty);
        self.push_stmt(Statement::Assign {
            target: local,
            rvalue: RValue::Use(v),
        });
    }

    /// Walk a pattern that successfully matched and store the scrutinee
    /// into any binding locals.
    pub(crate) fn bind_pattern(&mut self, pat: &HirPattern, scrut: LocalId) {
        match pat {
            HirPattern::Binding { local, .. } => {
                self.push_stmt(Statement::Assign {
                    target: *local,
                    rvalue: RValue::Use(Operand::Copy(scrut)),
                });
            }
            HirPattern::At { local, sub, .. } => {
                self.push_stmt(Statement::Assign {
                    target: *local,
                    rvalue: RValue::Use(Operand::Copy(scrut)),
                });
                self.bind_pattern(sub, scrut);
            }
            HirPattern::EnumVariant {
                enum_id,
                variant_index,
                args,
            } => {
                let Some(arg_locals) =
                    self.variant_arg_locals(*enum_id, *variant_index, scrut, args.len())
                else {
                    return;
                };
                for (sub_pat, (local, _)) in args.iter().zip(arg_locals) {
                    self.bind_pattern(sub_pat, local);
                }
            }
            HirPattern::SumVariant {
                binder,
                sum_ty,
                payload_ty: hir_payload,
                ..
            } => {
                let Some(local) = binder else { return };
                self.sum_payload(scrut, sum_ty, hir_payload.as_ref(), *local);
            }
            HirPattern::SumMember {
                sum_ty,
                payload_ty,
                sub,
                ..
            } => {
                let member_ty = self.ty(payload_ty);
                let member = self.fresh_local(member_ty);
                self.sum_payload(scrut, sum_ty, Some(payload_ty), member);
                self.bind_pattern(sub, member);
            }
            HirPattern::Tuple(sub_pats) => {
                let scrut_ty = self.locals[scrut.0 as usize].ty.clone();
                let MirTy::Tuple(elem_tys) = scrut_ty else {
                    return;
                };
                if sub_pats.len() != elem_tys.len() {
                    return;
                }
                for (i, sub_pat) in sub_pats.iter().enumerate() {
                    if matches!(
                        sub_pat,
                        HirPattern::Wildcard
                            | HirPattern::Literal { .. }
                            | HirPattern::Range { .. }
                    ) {
                        continue;
                    }
                    let elem_local = self.fresh_local(elem_tys[i].clone());
                    self.push_stmt(Statement::Assign {
                        target: elem_local,
                        rvalue: RValue::FieldRead {
                            target: Operand::Copy(scrut),
                            index: u32::try_from(i).expect("tuple index overflow"),
                            field_ty: elem_tys[i].clone(),
                        },
                    });
                    self.bind_pattern(sub_pat, elem_local);
                }
            }
            HirPattern::Struct { fields, .. } => {
                let scrut_ty = self.locals[scrut.0 as usize].ty.clone();
                let MirTy::Struct(_, elem_tys) = scrut_ty else {
                    return;
                };
                for (idx, sub_pat) in fields {
                    if matches!(
                        sub_pat,
                        HirPattern::Wildcard
                            | HirPattern::Literal { .. }
                            | HirPattern::Range { .. }
                    ) {
                        continue;
                    }
                    let Some(field_ty) = elem_tys.get(*idx as usize).cloned() else {
                        continue;
                    };
                    let elem_local = self.fresh_local(field_ty.clone());
                    self.push_stmt(Statement::Assign {
                        target: elem_local,
                        rvalue: RValue::FieldRead {
                            target: Operand::Copy(scrut),
                            index: *idx,
                            field_ty,
                        },
                    });
                    self.bind_pattern(sub_pat, elem_local);
                }
            }
            HirPattern::Slice(s) => {
                self.slice_whole(s, scrut);
                for (e, sub_pat) in s.items() {
                    if !binds(sub_pat) {
                        continue;
                    }
                    let item = self.slice_item(e);
                    self.bind_pattern(sub_pat, item);
                }
            }
            // An or-pattern's alternatives bind in their own success
            // blocks (`lower_pattern_test`).
            HirPattern::Wildcard
            | HirPattern::Literal { .. }
            | HirPattern::Range { .. }
            | HirPattern::Or(_) => {}
        }
    }
}
