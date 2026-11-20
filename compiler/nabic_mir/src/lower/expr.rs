//! Statements, expressions, array fills and floored modulo.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnBuilder<'_> {
    pub(crate) fn lower_stmt(&mut self, s: &HirStmt) -> bool {
        let at = match s {
            HirStmt::Let { init: e, .. }
            | HirStmt::Assign { value: e, .. }
            | HirStmt::ArrayAssign { value: e, .. }
            | HirStmt::StaticAssign { value: e, .. }
            | HirStmt::Expr(e)
            | HirStmt::Return(Some(e))
            | HirStmt::Break { value: Some(e), .. }
            | HirStmt::Panic(e) => Some(e.span),
            _ => None,
        };
        if let Some(span) = at {
            self.push_loc(span);
        }
        match s {
            HirStmt::Let { local, init } => {
                // Array literals are built in the target's slot, not in a
                // temporary copied there.
                if let HirExprKind::ArrayInit { elem_ty, elements } = &init.kind {
                    let mir_elem_ty = self.ty(elem_ty);
                    let elem_ops: Vec<Operand> = elements
                        .iter()
                        .map(|e| self.lower_expr_to_operand(e))
                        .collect();
                    self.push_stmt(Statement::Assign {
                        target: *local,
                        rvalue: RValue::ArrayInit {
                            elem_ty: mir_elem_ty,
                            elements: elem_ops,
                        },
                    });
                    return false;
                }
                let rvalue = self.lower_expr_to_rvalue(init);
                self.push_stmt(Statement::Assign {
                    target: *local,
                    rvalue,
                });
                false
            }
            HirStmt::Assign { local, value } => {
                let rvalue = self.lower_expr_to_rvalue(value);
                self.push_stmt(Statement::Assign {
                    target: *local,
                    rvalue,
                });
                false
            }
            HirStmt::ArrayAssign {
                array_local,
                path,
                index,
                value,
                elem_ty,
            } => {
                let idx = self.lower_expr_to_operand(index);
                let val = self.lower_expr_to_operand(value);
                let mir_elem_ty = self.ty(elem_ty);
                let mut ty = self.locals[array_local.0 as usize].ty.clone();
                let mut offset = 0;
                for k in path {
                    offset += self.target.field_offsets(&ty)[*k as usize];
                    ty = match ty {
                        MirTy::Struct(_, ps) | MirTy::Tuple(ps) => ps[*k as usize].clone(),
                        other => other,
                    };
                }
                let len = match ty {
                    MirTy::Array(_, n) => n,
                    _ => 0,
                };
                self.push_stmt(Statement::ArrayWrite {
                    array: *array_local,
                    offset,
                    index: idx,
                    value: val,
                    elem_ty: mir_elem_ty,
                    len,
                });
                false
            }
            HirStmt::Expr(e) => {
                let _ = self.lower_expr_to_operand(e);
                false
            }
            HirStmt::Return(value) => {
                let op = value.as_ref().map(|e| self.lower_expr_to_operand(e));
                self.terminate(Terminator::Return(op));
                true
            }
            HirStmt::Break { label, value } => {
                let value_op = value.as_ref().map(|e| self.lower_expr_to_operand(e));
                let Some(frame) = self.find_loop_frame(label.as_deref()) else {
                    // No enclosing loop; fall back to unreachable so the
                    // caller's pipeline doesn't read uninitialised state.
                    self.terminate(Terminator::Unreachable);
                    return true;
                };
                let break_bb = frame.break_bb;
                let args = match (frame.break_param, value_op) {
                    (Some(_), Some(v)) => vec![v],
                    (Some(_), None) => vec![Operand::Const(ConstVal::Unit)],
                    _ => Vec::new(),
                };
                self.terminate(Terminator::Goto {
                    target: break_bb,
                    args,
                });
                true
            }
            HirStmt::Continue { label } => {
                let Some(frame) = self.find_loop_frame(label.as_deref()) else {
                    self.terminate(Terminator::Unreachable);
                    return true;
                };
                let target = frame.continue_bb;
                self.terminate(Terminator::Goto {
                    target,
                    args: Vec::new(),
                });
                true
            }
            HirStmt::Panic(call) => {
                // The payload calls a never-returning runtime hook.
                // Emit the call, then an `unreachable` terminator so the
                // surrounding block won't fall through into a merge.
                let _ = self.lower_expr_to_operand(call);
                self.terminate(Terminator::Unreachable);
                true
            }
            HirStmt::StaticAssign { name, value } => {
                let mir_ty = self.ty(&value.ty);
                let value_op = self.lower_expr_to_operand(value);
                self.push_stmt(Statement::StaticStore {
                    name: name.clone(),
                    value: value_op,
                    ty: mir_ty,
                });
                false
            }
        }
    }

    pub(crate) fn find_loop_frame(&self, label: Option<&str>) -> Option<&LoopFrame> {
        match label {
            None => self.loop_stack.iter().rev().find(|f| f.is_loop),
            Some(name) => self
                .loop_stack
                .iter()
                .rev()
                .find(|f| f.label.as_deref() == Some(name)),
        }
    }

    pub(crate) fn lower_expr_to_rvalue(&mut self, e: &HirExpr) -> RValue {
        match &e.kind {
            HirExprKind::PrimBinOp {
                op: PrimOp::Mod,
                lhs,
                rhs,
            } => self.lower_floored_mod(lhs, rhs, e.span),
            HirExprKind::PrimBinOp { op, lhs, rhs } => {
                let op_ty = self.ty(&lhs.ty);
                let lhs_op = self.lower_expr_to_operand(lhs);
                let rhs_op = self.lower_expr_to_operand(rhs);
                // An overflow / division check reports this position.
                self.push_loc(e.span);
                RValue::BinOp {
                    op: *op,
                    op_ty,
                    lhs: lhs_op,
                    rhs: rhs_op,
                }
            }
            HirExprKind::PrimUnaryOp { op, operand } => {
                let op_ty = self.ty(&operand.ty);
                let v = self.lower_expr_to_operand(operand);
                self.push_loc(e.span);
                RValue::UnaryOp {
                    op: *op,
                    op_ty,
                    operand: v,
                }
            }
            HirExprKind::Call { callee, args } => {
                let c_callback = self.fnv.c_externs.contains(callee);
                let mut arg_ops = Vec::with_capacity(args.len());
                for a in args {
                    // A named fn passed to C: its own (thin) address.
                    if let (true, HirExprKind::FnRef(id), HirTy::Fn(ps, r, _)) =
                        (c_callback, &a.kind, &a.ty)
                    {
                        let fn_ty = MirTy::Fn(
                            ps.iter().map(|t| self.ty(t)).collect(),
                            Box::new(self.ty(r)),
                        );
                        let l = self.fresh_local(fn_ty.clone());
                        self.push_stmt(Statement::Assign {
                            target: l,
                            rvalue: RValue::FnAddr { callee: *id, fn_ty },
                        });
                        arg_ops.push(Operand::Copy(l));
                        continue;
                    }
                    arg_ops.push(self.lower_expr_to_operand(a));
                }
                let ret_ty = self.fn_ret_tys.get(callee).cloned().unwrap_or(MirTy::Unit);
                RValue::Call {
                    callee: *callee,
                    args: arg_ops,
                    ret_ty,
                }
            }
            // A C-ABI fn as an `extern "C" fn` value: its own address
            // (codegen picks the C entry).
            HirExprKind::FnRef(id) if e.ty.is_c_fn() => RValue::FnAddr {
                callee: *id,
                fn_ty: self.ty(&e.ty),
            },
            // A named fn as a value: `{wrapper, null}`.
            HirExprKind::FnRef(id) => {
                let MirTy::Tuple(parts) = self.ty(&e.ty) else {
                    return RValue::Use(Operand::Const(ConstVal::Unit));
                };
                let wrapper = *self
                    .fnv
                    .wrappers
                    .borrow_mut()
                    .entry(*id)
                    .or_insert_with(|| {
                        let w = FunctionId(self.fnv.next_id.get());
                        self.fnv.next_id.set(w.0 + 1);
                        w
                    });
                let code = self.fresh_local(parts[0].clone());
                self.push_stmt(Statement::Assign {
                    target: code,
                    rvalue: RValue::FnAddr {
                        callee: wrapper,
                        fn_ty: parts[0].clone(),
                    },
                });
                RValue::StructInit {
                    ty: MirTy::Tuple(parts),
                    fields: vec![
                        Operand::Copy(code),
                        Operand::Const(ConstVal::Int {
                            value: 0,
                            ty: IrPrimTy::Usize,
                        }),
                    ],
                }
            }
            HirExprKind::CallIndirect {
                callee,
                args,
                ret_ty,
            } => {
                let callee_op = self.lower_expr_to_operand(callee);
                if callee.ty.is_c_fn() {
                    // A C function pointer: no env, the C ABI.
                    let stdcall = matches!(&callee.ty, HirTy::Fn(_, _, b) if b.stdcall);
                    return RValue::CallIndirect {
                        callee: callee_op,
                        args: args.iter().map(|a| self.lower_expr_to_operand(a)).collect(),
                        arg_tys: args.iter().map(|a| self.ty(&a.ty)).collect(),
                        ret_ty: self.ty(ret_ty),
                        abi: Abi::C {
                            variadic: false,
                            system: stdcall,
                        },
                    };
                }
                self.call_fn_value(callee_op, args, ret_ty)
            }
            HirExprKind::Cast {
                operand,
                from_ty,
                to_ty,
            } => {
                let source = self.lower_expr_to_operand(operand);
                self.push_loc(e.span);
                if let Some(v) = self.fn_from_code(&source, from_ty, to_ty) {
                    return v;
                }
                RValue::Cast {
                    source,
                    from_ty: self.ty(from_ty),
                    to_ty: self.ty(to_ty),
                }
            }
            // A closure value: `{body, boxed captures}`; the body takes
            // the box (see `finish_fn_values`).
            HirExprKind::ClosureValue {
                fn_id,
                env,
                env_ty,
                arg_tys,
                ret_ty,
            } => {
                let env_mir = self.ty(env_ty);
                self.fnv
                    .closures
                    .borrow_mut()
                    .insert(*fn_id, env_mir.clone());
                let args: Vec<MirTy> = arg_tys.iter().map(|t| self.ty(t)).collect();
                let ret = self.ty(ret_ty);
                let code_t = code_ty(args.clone(), ret.clone());
                let env_op = self.lower_expr_to_operand(env);
                let boxed = self.fresh_local(MirTy::GcRef);
                self.push_stmt(Statement::Assign {
                    target: boxed,
                    rvalue: RValue::HeapBox {
                        value: env_op,
                        ty: env_mir,
                    },
                });
                let code = self.fresh_local(code_t.clone());
                self.push_stmt(Statement::Assign {
                    target: code,
                    rvalue: RValue::FnAddr {
                        callee: *fn_id,
                        fn_ty: code_t,
                    },
                });
                RValue::StructInit {
                    ty: fat_fn(args, ret),
                    fields: vec![Operand::Copy(code), Operand::Copy(boxed)],
                }
            }
            HirExprKind::CallClosure {
                closure,
                args,
                ret_ty,
                ..
            } => {
                let closure_op = self.lower_expr_to_operand(closure);
                self.call_fn_value(closure_op, args, ret_ty)
            }
            // `{resume, frame}` from the coroutine's constructor.
            HirExprKind::CoroNew { body_fn, args } => {
                let ctor = *self
                    .fnv
                    .coro_ctors
                    .borrow_mut()
                    .entry(*body_fn)
                    .or_insert_with(|| self.fnv.fresh_id());
                let args = args.iter().map(|a| self.lower_expr_to_operand(a)).collect();
                RValue::Call {
                    callee: ctor,
                    args,
                    ret_ty: self.ty(&e.ty),
                }
            }
            _ => RValue::Use(self.lower_expr_to_operand(e)),
        }
    }

    /// A labeled block: a frame so `break 'label value` exits with a value
    /// via a join block.
    pub(crate) fn lower_labeled_block(
        &mut self,
        b: &HirBlock,
        label: String,
        ty: &HirTy,
    ) -> Operand {
        let exit_bb = self.new_block();
        let result_ty = self.ty(ty);
        let break_param = if matches!(result_ty, MirTy::Unit) {
            None
        } else {
            Some(self.add_block_param(exit_bb, result_ty))
        };
        self.loop_stack.push(LoopFrame {
            // `continue` inside a labeled block isn't
            // meaningful - point it at exit so misuse
            // becomes a no-op rather than a crash.
            continue_bb: exit_bb,
            break_bb: exit_bb,
            break_param,
            label: Some(label),
            is_loop: false,
        });
        let (value, diverged) = self.lower_block_value(b);
        self.loop_stack.pop();
        if !diverged {
            let args = match break_param {
                Some(_) => vec![value],
                None => Vec::new(),
            };
            self.terminate(Terminator::Goto {
                target: exit_bb,
                args,
            });
        }
        self.current = exit_bb;
        match break_param {
            Some(p) => Operand::Copy(p),
            None => Operand::Const(ConstVal::Unit),
        }
    }

    pub(crate) fn lower_expr_to_operand(&mut self, e: &HirExpr) -> Operand {
        match &e.kind {
            HirExprKind::Const(ConstVal::Bytes(b)) => {
                let u8_ty = MirTy::Prim(PrimTy::U8);
                let n = u32::try_from(b.len()).unwrap_or(0);
                let dst = self.fresh_local(MirTy::Array(Box::new(u8_ty.clone()), n));
                let elements = b
                    .iter()
                    .map(|&x| {
                        Operand::Const(ConstVal::Int {
                            value: i128::from(x),
                            ty: PrimTy::U8,
                        })
                    })
                    .collect();
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::ArrayInit {
                        elem_ty: u8_ty,
                        elements,
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::Const(c) => Operand::Const(c.clone()),
            HirExprKind::Local(id) => Operand::Copy(*id),
            HirExprKind::StaticRef(name) => {
                let mir_ty = self.ty(&e.ty);
                let dst = self.fresh_local(mir_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::StaticLoad {
                        name: name.clone(),
                        ty: mir_ty,
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::Block(b) => {
                // Plain block: just execute. Labeled block: push a
                // frame so `break 'label value` exits with a value via
                // a join block.
                if let Some(label) = b.label.clone() {
                    self.lower_labeled_block(b, label, &e.ty)
                } else {
                    for s in &b.stmts {
                        self.lower_stmt(s);
                    }
                    if let Some(t) = &b.trailing {
                        self.lower_expr_to_operand(t)
                    } else {
                        Operand::Const(ConstVal::Unit)
                    }
                }
            }
            // HIR reports every unresolved name; reaching MIR is a bug.
            HirExprKind::UnresolvedPath(p) => {
                panic!(
                    "internal compiler error: unresolved path `{}` reached MIR",
                    p.join("::")
                )
            }
            HirExprKind::Call { callee, .. } => {
                let result_ty = self.fn_ret_tys.get(callee).cloned().unwrap_or(MirTy::Unit);
                let dst = self.fresh_local(result_ty);
                let rvalue = self.lower_expr_to_rvalue(e);
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue,
                });
                Operand::Copy(dst)
            }
            HirExprKind::CallIndirect { ret_ty, .. } => {
                let result_ty = self.ty(ret_ty);
                let dst = self.fresh_local(result_ty);
                let rvalue = self.lower_expr_to_rvalue(e);
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue,
                });
                Operand::Copy(dst)
            }
            HirExprKind::Cast { to_ty, .. } => {
                let result_ty = self.ty(to_ty);
                let dst = self.fresh_local(result_ty);
                let rvalue = self.lower_expr_to_rvalue(e);
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue,
                });
                Operand::Copy(dst)
            }
            HirExprKind::PrimBinOp { .. }
            | HirExprKind::PrimUnaryOp { .. }
            | HirExprKind::FnRef(_)
            | HirExprKind::ClosureValue { .. }
            | HirExprKind::CoroNew { .. }
            | HirExprKind::CallClosure { .. } => {
                let result_ty = self.ty(&e.ty);
                let dst = self.fresh_local(result_ty);
                let rvalue = self.lower_expr_to_rvalue(e);
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue,
                });
                Operand::Copy(dst)
            }
            HirExprKind::Suspend { value, is_final } => {
                let v = self.lower_expr_to_operand(value);
                let unit = matches!(self.ty(&value.ty), MirTy::Unit);
                let resume = self.new_block();
                self.terminate(Terminator::Suspend {
                    value: (!unit).then_some(v),
                    resume,
                    is_final: *is_final,
                });
                self.current = resume;
                Operand::Const(ConstVal::Unit)
            }
            HirExprKind::If {
                cond,
                then_block,
                else_branch,
            } => self.lower_if(cond, then_block, else_branch.as_deref(), &e.ty),
            HirExprKind::While { label, cond, body } => self.lower_while(label.clone(), cond, body),
            HirExprKind::Loop {
                label,
                body,
                result_ty,
            } => self.lower_loop(label.clone(), body, result_ty),
            HirExprKind::Match { scrutinee, arms } => self.lower_match(scrutinee, arms, &e.ty),
            HirExprKind::StructInit { struct_id, fields } => {
                let result_ty = self.ty(&e.ty);
                let field_ops: Vec<Operand> = fields
                    .iter()
                    .map(|f| self.lower_expr_to_operand(f))
                    .collect();
                let dst = self.fresh_local(result_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::StructInit {
                        ty: result_ty,
                        fields: field_ops,
                    },
                });
                let _ = struct_id;
                Operand::Copy(dst)
            }
            HirExprKind::SumWrap {
                sum_ty,
                variant_index,
                value,
            } => {
                let mir_ty = self.ty(sum_ty);
                // A niche sum: the member's bits, or all zero.
                if let Some((member, n)) = self.niche(sum_ty) {
                    if let Some(v) = value {
                        let v = self.lower_expr_to_operand(v);
                        return self.niche_wrap(v, &member, &n);
                    }
                    let dst = self.fresh_local(mir_ty);
                    self.push_stmt(Statement::Assign {
                        target: dst,
                        rvalue: RValue::Use(Operand::Const(ConstVal::None)),
                    });
                    return Operand::Copy(dst);
                }
                let tag_op = Operand::Const(ConstVal::Int {
                    value: i128::from(sum_tag(sum_ty, *variant_index)),
                    ty: IrPrimTy::I32,
                });
                // `any Tr`: the payload is the value itself if it fits the
                // word, else a heap copy; the tag's `ANY_BOXED` bit says
                // which (the collector's `ANY_SLOT`).
                if let (HirTy::AnyTrait(_), Some(v)) = (sum_ty, value) {
                    let raw = self.lower_expr_to_operand(v);
                    let ty = self.ty(&v.ty);
                    let boxed = !self.target.any_inline(&ty);
                    let tag_op = self.any_tag_op(*variant_index, boxed);
                    let ptr = self.fresh_local(MirTy::GcRef);
                    self.push_stmt(Statement::Assign {
                        target: ptr,
                        rvalue: RValue::HeapBox { value: raw, ty },
                    });
                    let dst = self.fresh_local(mir_ty.clone());
                    self.push_stmt(Statement::Assign {
                        target: dst,
                        rvalue: RValue::StructInit {
                            ty: mir_ty,
                            fields: vec![tag_op, Operand::Copy(ptr)],
                        },
                    });
                    return Operand::Copy(dst);
                }
                // Payload-less sum (all-none, exotic) - return tag only.
                let MirTy::Tuple(parts) = &mir_ty else {
                    return tag_op;
                };
                let payload_ty = parts[1].clone();
                // Every member goes through the sum's one payload slot.
                let payload_op = match value {
                    Some(v) => {
                        let raw = self.lower_expr_to_operand(v);
                        let value_ty = self.ty(&v.ty);
                        self.payload_cast(raw, &value_ty, &payload_ty)
                    }
                    None => Operand::Const(zero_const(&payload_ty)),
                };
                let dst = self.fresh_local(mir_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::StructInit {
                        ty: mir_ty,
                        fields: vec![tag_op, payload_op],
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::EnumVariantInit {
                enum_id,
                variant_index,
                args,
            } => {
                // A fieldless enum's tag is its `#[repr]` integer.
                let tag_ty = match self.ty(&HirTy::Enum(*enum_id)) {
                    MirTy::Prim(p) => p,
                    _ => IrPrimTy::I32,
                };
                let tag_op = Operand::Const(ConstVal::Int {
                    value: self.module.enums[enum_id.0 as usize].variants[*variant_index as usize]
                        .tag,
                    ty: tag_ty,
                });
                let Some(payload_ty) = enum_payload_ty(*enum_id, self.module, self.target) else {
                    for a in args {
                        self.lower_expr_to_operand(a);
                    }
                    return tag_op;
                };
                let unit_arg = matches!(args.as_slice(), [a] if self.ty(&a.ty) == MirTy::Unit);
                let payload_op = if args.is_empty() || unit_arg {
                    // A `()` payload has no bits, but its expression still
                    // runs (`Ok(f())` must call `f` and see its throw).
                    for a in args {
                        self.lower_expr_to_operand(a);
                    }
                    Operand::Const(zero_const(&payload_ty))
                } else if args.len() == 1
                    // A one-field named variant's payload is a 1-tuple:
                    // packed below like the multi-arg case.
                    && self.module.enums[enum_id.0 as usize].variants[*variant_index as usize]
                        .named_fields
                        .is_empty()
                {
                    let arg = &args[0];
                    let raw = self.lower_expr_to_operand(arg);
                    self.payload_cast(raw, &self.ty(&arg.ty), &payload_ty)
                } else {
                    // Multi-arg variant: pack the args into the variant's
                    // own tuple, then move it into the shared slot.
                    let variant_ty = self.variant_payload_ty(*enum_id, *variant_index);
                    let MirTy::Tuple(field_tys) = variant_ty.clone() else {
                        return tag_op;
                    };
                    let field_ops: Vec<Operand> = args
                        .iter()
                        .zip(field_tys.iter())
                        .map(|(a, ft)| {
                            let raw = self.lower_expr_to_operand(a);
                            self.cast_to(raw, &self.ty(&a.ty), ft)
                        })
                        .collect();
                    let tup = self.fresh_local(variant_ty.clone());
                    self.push_stmt(Statement::Assign {
                        target: tup,
                        rvalue: RValue::StructInit {
                            ty: variant_ty.clone(),
                            fields: field_ops,
                        },
                    });
                    self.payload_cast(Operand::Copy(tup), &variant_ty, &payload_ty)
                };
                let enum_ty = MirTy::Tuple(vec![MirTy::Prim(IrPrimTy::I32), payload_ty]);
                let dst = self.fresh_local(enum_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::StructInit {
                        ty: enum_ty,
                        fields: vec![tag_op, payload_op],
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::ArrayInit { elem_ty, elements } => {
                let mir_elem_ty = self.ty(elem_ty);
                let n = u32::try_from(elements.len()).unwrap_or(0);
                let array_ty = MirTy::Array(Box::new(mir_elem_ty.clone()), n);
                let elem_ops: Vec<Operand> = elements
                    .iter()
                    .map(|e| self.lower_expr_to_operand(e))
                    .collect();
                let dst = self.fresh_local(array_ty);
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::ArrayInit {
                        elem_ty: mir_elem_ty,
                        elements: elem_ops,
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::ArrayRepeat {
                elem_ty,
                value,
                len,
            } => {
                let elem = self.ty(elem_ty);
                let v = self.lower_expr_to_operand(value);
                let dst = self.fresh_local(MirTy::Array(Box::new(elem.clone()), *len));
                if self.target.has_gc_ref(&elem) {
                    // Unrolled: no element is unset while a collection
                    // could scan the slot.
                    self.push_stmt(Statement::Assign {
                        target: dst,
                        rvalue: RValue::ArrayInit {
                            elem_ty: elem,
                            elements: vec![v; *len as usize],
                        },
                    });
                } else {
                    self.fill_array(dst, &elem, v, *len);
                }
                Operand::Copy(dst)
            }
            HirExprKind::BufNew { elem, len } => {
                let len = self.lower_expr_to_operand(len);
                let dst = self.fresh_local(MirTy::GcRef);
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::BufAlloc {
                        elem_ty: self.ty(elem),
                        len,
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::RawRead { addr, ty, volatile } => {
                let addr = self.lower_expr_to_operand(addr);
                let ty = self.ty(ty);
                let dst = self.fresh_local(ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::RawLoad {
                        addr,
                        ty,
                        volatile: *volatile,
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::RawWrite {
                addr,
                value,
                ty,
                volatile,
            } => {
                let addr = self.lower_expr_to_operand(addr);
                let value = self.lower_expr_to_operand(value);
                self.push_stmt(Statement::RawStore {
                    addr,
                    value,
                    ty: self.ty(ty),
                    volatile: *volatile,
                });
                Operand::Const(ConstVal::Unit)
            }
            HirExprKind::AddrOf { place } => {
                // The place's reads only check its indices (they are
                // locals or constants: `addr_place` reads them again).
                self.lower_expr_to_operand(place);
                self.addr_place(place)
            }
            HirExprKind::TypeDesc(n) => {
                let dst = self.fresh_local(MirTy::Prim(PrimTy::Usize));
                let name = self.module.type_descs[*n as usize].symbol.clone();
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::StaticAddr(name),
                });
                Operand::Copy(dst)
            }
            HirExprKind::SizeOf { ty, align } => {
                let dst = self.fresh_local(MirTy::Prim(PrimTy::Usize));
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::SizeOf {
                        ty: self.ty(ty),
                        align: *align,
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::BufGet { buf, index, elem } => {
                let buf = self.lower_expr_to_operand(buf);
                let index = self.lower_expr_to_operand(index);
                let elem_ty = self.ty(elem);
                // A buffer of `()` stores nothing.
                if elem_ty == MirTy::Unit {
                    return Operand::Const(ConstVal::Unit);
                }
                let dst = self.fresh_local(elem_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::BufLoad {
                        buf,
                        index,
                        elem_ty,
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::BufAtomic {
                op,
                buf,
                index,
                args,
                elem,
                order,
            } => {
                let buf = self.lower_expr_to_operand(buf);
                let index = self.lower_expr_to_operand(index);
                let args = args.iter().map(|a| self.lower_expr_to_operand(a)).collect();
                let elem_ty = self.ty(elem);
                let dst = self.fresh_local(elem_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::BufAtomic {
                        op,
                        buf,
                        index,
                        args,
                        elem_ty,
                        order: *order,
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::BufSet {
                buf,
                index,
                value,
                elem,
            } => {
                let buf = self.lower_expr_to_operand(buf);
                let index = self.lower_expr_to_operand(index);
                let value = self.lower_expr_to_operand(value);
                let elem_ty = self.ty(elem);
                if elem_ty != MirTy::Unit {
                    self.push_stmt(Statement::BufStore {
                        buf,
                        index,
                        value,
                        elem_ty,
                    });
                }
                Operand::Const(ConstVal::Unit)
            }
            HirExprKind::ArrayIndex {
                array,
                index,
                elem_ty,
            } => {
                // `s.buf[i]`: index the field where it lies, not a copy.
                let (arr_local, offset) = match self.field_place(array) {
                    Some(p) => p,
                    None => match self.lower_expr_to_operand(array) {
                        Operand::Copy(l) => (l, 0),
                        Operand::Const(_) => return Operand::Const(ConstVal::Unit),
                    },
                };
                let idx_op = self.lower_expr_to_operand(index);
                self.push_loc(e.span);
                let mir_elem_ty = self.ty(elem_ty);
                let len = match self.ty(&array.ty) {
                    MirTy::Array(_, n) => n,
                    _ => 0,
                };
                let dst = self.fresh_local(mir_elem_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::ArrayIndex {
                        array: arr_local,
                        offset,
                        index: idx_op,
                        elem_ty: mir_elem_ty,
                        len,
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::Tuple { fields } => {
                let result_ty = self.ty(&e.ty);
                let field_ops: Vec<Operand> = fields
                    .iter()
                    .map(|f| self.lower_expr_to_operand(f))
                    .collect();
                let field_ops = self.carrier_fields(&e.ty, fields, field_ops, &result_ty);
                let dst = self.fresh_local(result_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::StructInit {
                        ty: result_ty,
                        fields: field_ops,
                    },
                });
                Operand::Copy(dst)
            }
            // `union` field write / read: the bits of the synthetic
            // `__data` slot (HIR rejects sizes that do not fit).
            HirExprKind::UnionWrite {
                struct_id,
                from_ty,
                value,
                ..
            } => {
                let raw = self.lower_expr_to_operand(value);
                let src_ty = self.ty(from_ty);
                let dst_ty = self.union_storage_ty(*struct_id);
                self.union_bits(raw, &src_ty, &dst_ty)
            }
            HirExprKind::UnionRead {
                struct_id,
                target,
                field_ty,
                ..
            } => {
                let target_op = self.lower_expr_to_operand(target);
                let Operand::Copy(target_local) = target_op else {
                    return Operand::Const(ConstVal::Unit);
                };
                let storage_mir_ty = self.union_storage_ty(*struct_id);
                let storage_local = self.fresh_local(storage_mir_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: storage_local,
                    rvalue: RValue::FieldRead {
                        target: Operand::Copy(target_local),
                        index: 0,
                        field_ty: storage_mir_ty.clone(),
                    },
                });
                let dst_ty = self.ty(field_ty);
                self.union_bits(Operand::Copy(storage_local), &storage_mir_ty, &dst_ty)
            }
            HirExprKind::TupleField {
                target,
                index,
                field_ty,
            } => {
                let target_op = self.lower_expr_to_operand(target);
                self.tuple_field(target_op, &target.ty, *index, field_ty)
            }
            HirExprKind::WithField {
                target,
                field_index,
                value,
                ..
            } => {
                let struct_ty = self.ty(&target.ty);
                let source = self.lower_expr_to_operand(target);
                let new_value = self.lower_expr_to_operand(value);
                let dst = self.fresh_local(struct_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::WithField {
                        source,
                        struct_ty,
                        index: *field_index,
                        value: new_value,
                    },
                });
                Operand::Copy(dst)
            }
            HirExprKind::FieldRead {
                target,
                field_index,
                field_ty,
                ..
            } => {
                let target_op = self.lower_expr_to_operand(target);
                let mir_field_ty = self.ty(field_ty);
                let dst = self.fresh_local(mir_field_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: dst,
                    rvalue: RValue::FieldRead {
                        target: target_op,
                        index: *field_index,
                        field_ty: mir_field_ty,
                    },
                });
                Operand::Copy(dst)
            }
        }
    }

    /// `for i in 0..len { array[i] = value }`.
    pub(crate) fn fill_array(&mut self, array: LocalId, elem_ty: &MirTy, value: Operand, len: u32) {
        let usize_ty = MirTy::Prim(PrimTy::Usize);
        let i = self.fresh_local(usize_ty.clone());
        let int = |v: i128| {
            Operand::Const(ConstVal::Int {
                value: v,
                ty: PrimTy::Usize,
            })
        };
        self.push_stmt(Statement::Assign {
            target: i,
            rvalue: RValue::Use(int(0)),
        });
        let header_bb = self.new_block();
        let body_bb = self.new_block();
        let exit_bb = self.new_block();
        self.terminate(Terminator::Goto {
            target: header_bb,
            args: Vec::new(),
        });
        self.current = header_bb;
        let more = self.fresh_local(MirTy::Prim(PrimTy::Bool));
        self.push_stmt(Statement::Assign {
            target: more,
            rvalue: RValue::BinOp {
                op: PrimOp::Lt,
                op_ty: usize_ty.clone(),
                lhs: Operand::Copy(i),
                rhs: int(i128::from(len)),
            },
        });
        self.terminate(Terminator::If {
            cond: Operand::Copy(more),
            then_bb: body_bb,
            then_args: Vec::new(),
            else_bb: exit_bb,
            else_args: Vec::new(),
        });
        self.current = body_bb;
        self.push_stmt(Statement::ArrayWrite {
            array,
            offset: 0,
            index: Operand::Copy(i),
            value,
            elem_ty: elem_ty.clone(),
            len,
        });
        self.push_stmt(Statement::Assign {
            target: i,
            rvalue: RValue::BinOp {
                op: PrimOp::Add,
                op_ty: usize_ty,
                lhs: Operand::Copy(i),
                rhs: int(1),
            },
        });
        self.terminate(Terminator::Goto {
            target: header_bb,
            args: Vec::new(),
        });
        self.current = exit_bb;
    }

    /// `a %% b`: `r = a % b`, plus `b` when `r` is nonzero and its sign
    /// differs from `b`'s (unsigned: `r`). A float zero takes `b`'s sign.
    pub(crate) fn lower_floored_mod(
        &mut self,
        lhs: &HirExpr,
        rhs: &HirExpr,
        span: nabic_diagnostic::Span,
    ) -> RValue {
        let op_ty = self.ty(&lhs.ty);
        let a = self.lower_expr_to_operand(lhs);
        let b = self.lower_expr_to_operand(rhs);
        self.push_loc(span);
        let rem = RValue::BinOp {
            op: PrimOp::Rem,
            op_ty: op_ty.clone(),
            lhs: a,
            rhs: b.clone(),
        };
        let (zero, neg_zero) = match &op_ty {
            MirTy::Prim(p) if p.is_signed_int() => (ConstVal::Int { value: 0, ty: *p }, None),
            MirTy::Prim(p) if p.is_float() => {
                let sign = if *p == IrPrimTy::F32 {
                    1 << 31
                } else {
                    1 << 63
                };
                let z = |bits| ConstVal::Float { bits, ty: *p };
                (z(0), Some(z(sign)))
            }
            _ => return rem,
        };
        let r = self.fresh_local(op_ty.clone());
        self.push_stmt(Statement::Assign {
            target: r,
            rvalue: rem,
        });
        let bin = |s: &mut Self, op, op_ty: MirTy, lhs, rhs, ty| {
            let t = s.fresh_local(ty);
            let rvalue = RValue::BinOp {
                op,
                op_ty,
                lhs,
                rhs,
            };
            s.push_stmt(Statement::Assign { target: t, rvalue });
            Operand::Copy(t)
        };
        let bool_ty = MirTy::Prim(IrPrimTy::Bool);
        let zero = Operand::Const(zero);
        let r_op = Operand::Copy(r);
        let nz = bin(
            self,
            PrimOp::Ne,
            op_ty.clone(),
            r_op.clone(),
            zero.clone(),
            bool_ty.clone(),
        );
        let r_neg = bin(
            self,
            PrimOp::Lt,
            op_ty.clone(),
            r_op.clone(),
            zero.clone(),
            bool_ty.clone(),
        );
        let b_neg = bin(
            self,
            PrimOp::Lt,
            op_ty.clone(),
            b.clone(),
            zero.clone(),
            bool_ty.clone(),
        );
        let differ = bin(
            self,
            PrimOp::Ne,
            bool_ty.clone(),
            r_neg,
            b_neg.clone(),
            bool_ty.clone(),
        );
        let adjust = bin(
            self,
            PrimOp::BitAnd,
            bool_ty.clone(),
            nz.clone(),
            differ,
            bool_ty,
        );
        let then_bb = self.new_block();
        let merge_bb = self.new_block();
        let out = self.add_block_param(merge_bb, op_ty.clone());
        let keep_bb = if neg_zero.is_some() {
            self.new_block()
        } else {
            merge_bb
        };
        self.terminate(Terminator::If {
            cond: adjust,
            then_bb,
            then_args: Vec::new(),
            else_bb: keep_bb,
            else_args: if neg_zero.is_some() {
                Vec::new()
            } else {
                vec![r_op.clone()]
            },
        });
        // A float zero (`-0.0` from a negative `a`, `0.0` else): `b`'s sign.
        if let Some(neg_zero) = neg_zero {
            self.current = keep_bb;
            let zero_bb = self.new_block();
            self.terminate(Terminator::If {
                cond: nz,
                then_bb: merge_bb,
                then_args: vec![r_op.clone()],
                else_bb: zero_bb,
                else_args: Vec::new(),
            });
            self.current = zero_bb;
            let neg_bb = self.new_block();
            self.terminate(Terminator::If {
                cond: b_neg,
                then_bb: neg_bb,
                then_args: Vec::new(),
                else_bb: merge_bb,
                else_args: vec![zero],
            });
            self.current = neg_bb;
            self.terminate(Terminator::Goto {
                target: merge_bb,
                args: vec![Operand::Const(neg_zero)],
            });
        }
        self.current = then_bb;
        let sum = bin(self, PrimOp::Add, op_ty.clone(), r_op, b, op_ty);
        self.terminate(Terminator::Goto {
            target: merge_bb,
            args: vec![sum],
        });
        self.current = merge_bb;
        RValue::Use(Operand::Copy(out))
    }
}
