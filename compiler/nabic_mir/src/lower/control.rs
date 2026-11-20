//! Match, if, while and loop.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnBuilder<'_> {
    pub(crate) fn lower_match(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[HirMatchArm],
        result_hir_ty: &HirTy,
    ) -> Operand {
        // Materialize the scrutinee into a stack-slotted local so each
        // arm can compare against it without recomputing.
        let scrut_op = self.lower_expr_to_operand(scrutinee);
        let scrut_ty = self.ty(&scrutinee.ty);
        let scrut_local = match scrut_op {
            Operand::Copy(id) => id,
            other @ Operand::Const(_) => {
                let id = self.fresh_local(scrut_ty.clone());
                self.push_stmt(Statement::Assign {
                    target: id,
                    rvalue: RValue::Use(other),
                });
                id
            }
        };

        let merge_bb = self.new_block();
        let mir_result_ty = self.ty(result_hir_ty);
        let value_producing = !matches!(mir_result_ty, MirTy::Unit);
        let merge_param = if value_producing {
            Some(self.add_block_param(merge_bb, mir_result_ty))
        } else {
            None
        };

        let mut any_arm_to_merge = false;
        for arm in arms {
            let arm_body_bb = self.new_block();
            let next_bb = self.new_block();

            self.lower_pattern_test(&arm.pat, scrut_local, &scrut_ty, arm_body_bb, next_bb);

            // Body block: bind any pattern locals, then evaluate the
            // guard (if any), then run the body.
            self.current = arm_body_bb;
            self.bind_pattern(&arm.pat, scrut_local);
            if let Some(guard) = &arm.guard {
                let cond = self.lower_expr_to_operand(guard);
                let pass_bb = self.new_block();
                self.terminate(Terminator::If {
                    cond,
                    then_bb: pass_bb,
                    then_args: Vec::new(),
                    else_bb: next_bb,
                    else_args: Vec::new(),
                });
                self.current = pass_bb;
            }
            let body_val = self.lower_expr_to_operand(&arm.body);
            // Skip the goto-merge if the body fully diverged (e.g. all
            // its sub-branches `return`-ed out).
            if !self.terminated.contains(&self.current.0) {
                let args = if value_producing {
                    vec![body_val]
                } else {
                    Vec::new()
                };
                self.terminate(Terminator::Goto {
                    target: merge_bb,
                    args,
                });
                any_arm_to_merge = true;
            }

            self.current = next_bb;
        }
        // No arm reaches the merge: nothing may be lowered into it.
        if !any_arm_to_merge {
            self.terminated.insert(merge_bb.0);
        }

        // HIR checked exhaustiveness: falling past the last arm cannot happen.
        self.terminate(Terminator::Unreachable);

        self.current = merge_bb;
        match merge_param {
            Some(p) => Operand::Copy(p),
            None => Operand::Const(ConstVal::Unit),
        }
    }

    pub(crate) fn lower_while(
        &mut self,
        label: Option<String>,
        cond: &HirExpr,
        body: &HirBlock,
    ) -> Operand {
        let header_bb = self.new_block();
        let body_bb = self.new_block();
        let exit_bb = self.new_block();

        // Jump from the current block into the header.
        self.terminate(Terminator::Goto {
            target: header_bb,
            args: Vec::new(),
        });

        // Header: evaluate the condition, branch to body or exit.
        self.current = header_bb;
        let cond_op = self.lower_expr_to_operand(cond);
        self.terminate(Terminator::If {
            cond: cond_op,
            then_bb: body_bb,
            then_args: Vec::new(),
            else_bb: exit_bb,
            else_args: Vec::new(),
        });

        // Body: `lower_block_value`, so a trailing expression is not
        // taken for the function's return.
        self.current = body_bb;
        self.loop_stack.push(LoopFrame {
            continue_bb: header_bb,
            break_bb: exit_bb,
            break_param: None,
            label,
            is_loop: true,
        });
        let (_value, diverged) = self.lower_block_value(body);
        self.loop_stack.pop();
        if !diverged {
            self.terminate(Terminator::Goto {
                target: header_bb,
                args: Vec::new(),
            });
        }

        self.current = exit_bb;
        Operand::Const(ConstVal::Unit)
    }

    pub(crate) fn lower_loop(
        &mut self,
        label: Option<String>,
        body: &HirBlock,
        result_ty: &HirTy,
    ) -> Operand {
        let body_bb = self.new_block();
        let exit_bb = self.new_block();

        let mir_ty = self.ty(result_ty);
        let break_param = if matches!(mir_ty, MirTy::Unit) {
            None
        } else {
            Some(self.add_block_param(exit_bb, mir_ty))
        };

        self.terminate(Terminator::Goto {
            target: body_bb,
            args: Vec::new(),
        });

        self.current = body_bb;
        self.loop_stack.push(LoopFrame {
            continue_bb: body_bb,
            break_bb: exit_bb,
            break_param,
            label,
            is_loop: true,
        });
        let (_value, diverged) = self.lower_block_value(body);
        self.loop_stack.pop();
        if !diverged {
            self.terminate(Terminator::Goto {
                target: body_bb,
                args: Vec::new(),
            });
        }

        self.current = exit_bb;
        match break_param {
            Some(p) => Operand::Copy(p),
            None => Operand::Const(ConstVal::Unit),
        }
    }

    pub(crate) fn lower_if(
        &mut self,
        cond: &HirExpr,
        then_block: &HirBlock,
        else_branch: Option<&HirExpr>,
        result_hir_ty: &HirTy,
    ) -> Operand {
        // A constant condition (`if cfg!(..)`) lowers only the live arm,
        // so the dead one's calls and externs are not emitted at -O0.
        if let Some(c) = const_bool(cond) {
            return match (c, else_branch) {
                (true, Some(_)) => self.lower_block_value(then_block).0,
                (true, None) => {
                    self.lower_block_value(then_block);
                    Operand::Const(ConstVal::Unit)
                }
                (false, Some(e)) => self.lower_expr_to_operand(e),
                (false, None) => Operand::Const(ConstVal::Unit),
            };
        }
        let result_ty = self.ty(result_hir_ty);
        let has_else = else_branch.is_some();
        let value_producing = has_else && !matches!(result_ty, MirTy::Unit);

        let then_bb = self.new_block();
        let merge_bb = self.new_block();
        let else_bb = if has_else { self.new_block() } else { merge_bb };

        // Merge block param holds the unified result when both branches
        // produce a value.
        let merge_param = if value_producing {
            Some(self.add_block_param(merge_bb, result_ty))
        } else {
            None
        };

        // Condition lives in the current block.
        let cond_op = self.lower_expr_to_operand(cond);
        self.terminate(Terminator::If {
            cond: cond_op,
            then_bb,
            then_args: Vec::new(),
            else_bb,
            else_args: Vec::new(),
        });

        // Then arm.
        self.current = then_bb;
        let (then_value, then_diverged) = self.lower_block_value(then_block);
        if !then_diverged {
            let then_args = if value_producing {
                vec![then_value]
            } else {
                Vec::new()
            };
            self.terminate(Terminator::Goto {
                target: merge_bb,
                args: then_args,
            });
        }
        let then_to_merge = !then_diverged;

        // Else arm.
        let mut else_to_merge = !has_else;
        if let Some(else_e) = else_branch {
            self.current = else_bb;
            let else_value = self.lower_expr_to_operand(else_e);
            // The else expression itself may have diverged via a return
            // or break inside; check whether the current block is still
            // live before adding a goto-merge edge.
            if !self.terminated.contains(&self.current.0) {
                let else_args = if value_producing {
                    vec![else_value]
                } else {
                    Vec::new()
                };
                self.terminate(Terminator::Goto {
                    target: merge_bb,
                    args: else_args,
                });
                else_to_merge = true;
            }
        }

        // If neither arm reaches the merge, the merge is unreachable -
        // mark it terminated so a downstream `terminate` won't write a
        // dead edge into it.
        if !then_to_merge && !else_to_merge {
            self.terminated.insert(merge_bb.0);
        }

        self.current = merge_bb;
        match merge_param {
            Some(p) => Operand::Copy(p),
            None => Operand::Const(ConstVal::Unit),
        }
    }
}
