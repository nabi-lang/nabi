//! Right-hand sides of assignments.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnLower<'_, '_> {
    pub(crate) fn rvalue(&mut self, dst_ty: &MirTy, r: &RValue, dst: Option<Mem>) {
        let t = self.c.t;
        let pw = self.pw();
        // A destination for a value whose target slot is gone (`()`).
        let out = |s: &mut Self, ty: &MirTy| dst.unwrap_or_else(|| s.tmp(ty));
        match r {
            RValue::Use(op) => {
                let v = self.val(op, dst_ty);
                if let Some(d) = dst {
                    self.store(v, dst_ty, d);
                }
            }
            RValue::BinOp {
                op,
                op_ty,
                lhs,
                rhs,
            } => {
                let d = out(self, dst_ty);
                self.bin_op(*op, op_ty, lhs, rhs, d);
            }
            RValue::UnaryOp { op, op_ty, operand } => {
                let d = out(self, dst_ty);
                self.un_op(*op, op_ty, operand, d);
            }
            RValue::Call { callee, args, .. } => {
                let fns = self.c.fns;
                let sig = &fns[callee];
                if let (true, [at]) = (sig.throw_site, args.as_slice()) {
                    let str_ty = MirTy::Prim(PrimTy::Str);
                    // Its address only (a NUL-terminated constant).
                    let v = self.val(at, &str_ty);
                    let m = self.mem_of(v, &str_ty);
                    let slot = self.tls(tls_abi::THROW_SITE);
                    let k = self.word();
                    self.push(Inst::Mov {
                        k,
                        dst: slot,
                        src: V::M(m),
                    });
                    return;
                }
                if self.cfn
                    && sig.c.is_some()
                    && self.c.obj.syms[sig.sym as usize].name == "__builtin_va_start"
                {
                    if let Some(ap) = args.first() {
                        let w = MirTy::Prim(PrimTy::Usize);
                        let (v, _) = self.scalar(ap, &w);
                        self.va_start(v);
                    }
                    return;
                }
                if self.c.opt && self.str_part(sig.sym, args, dst) {
                    return;
                }
                if self.half_conv(sig.sym, args, dst)
                    || self.fma(sig.sym, args, dst)
                    || self.objc_ref(sig.sym, args, dst)
                {
                    return;
                }
                if sig.cold && self.c.opt && !self.out.cold_fns.contains(&sig.sym) {
                    self.out.cold_fns.push(sig.sym);
                }
                if let Some(abi) = &sig.c {
                    self.call_c(Callee::Sym(sig.sym), &sig.params, &sig.ret, abi, args, dst);
                    return;
                }
                let vals: Vec<Val> = args
                    .iter()
                    .zip(&sig.params)
                    .map(|(a, ty)| self.val(a, ty))
                    .collect();
                if let Some(op) = sig.simd {
                    if self.simd_call(op, &sig.params, &sig.ret, &vals, dst) {
                        return;
                    }
                }
                if sig.nosp {
                    self.call_rt("nabi_nosp_enter", &[], None);
                }
                self.call_body(sig, &vals, dst);
                if sig.nosp {
                    self.call_rt("nabi_nosp_leave", &[], None);
                }
            }
            RValue::StructInit { ty, fields } => {
                let d = out(self, ty);
                let size = t.size_align(ty).0;
                let ftys = struct_fields(ty).to_vec();
                let offs = t.field_offsets(ty);
                if self.c.opt {
                    // Only the padding: the fields stay parts a register
                    // may hold.
                    self.zero_gaps(d, size, &ftys, &offs);
                } else if size > 0 {
                    self.push(Inst::Zero { dst: d, size });
                }
                for (i, f) in fields.iter().enumerate() {
                    let fty = ftys.get(i).cloned().unwrap_or_else(|| self.op_ty(f));
                    let v = self.val(f, &fty);
                    let off = offs.get(i).copied().unwrap_or(0);
                    self.store(v, &fty, d.at(off));
                }
            }
            RValue::FieldRead { target, index, .. } => {
                let tty = self.op_ty(target);
                let v = self.val(target, &tty);
                let src = self.mem_of(v, &tty);
                let fty = struct_fields(&tty)
                    .get(*index as usize)
                    .cloned()
                    .unwrap_or(MirTy::Unit);
                let off = t
                    .field_offsets(&tty)
                    .get(*index as usize)
                    .copied()
                    .unwrap_or(0);
                if let Some(d) = dst {
                    let size = t.size_align(&fty).0;
                    if size > 0 && !(self.c.opt && self.simd_copy(&fty, d, src.at(off))) {
                        self.push(Inst::Copy {
                            dst: d,
                            src: src.at(off),
                            size,
                        });
                    }
                }
            }
            RValue::Cast {
                source,
                from_ty,
                to_ty,
            } => {
                let v = self.val(source, from_ty);
                let c = self.cast(v, from_ty, to_ty);
                if let Some(d) = dst {
                    self.store(c, to_ty, d);
                }
            }
            RValue::Bitcast {
                source,
                from_ty,
                to_ty,
            } => {
                let v = self.val(source, from_ty);
                if let Some(d) = dst {
                    self.bitcast_into(v, from_ty, to_ty, d);
                }
            }
            RValue::WithField {
                source,
                struct_ty,
                index,
                value,
            } => {
                let d = out(self, struct_ty);
                let v = self.val(source, struct_ty);
                self.store(v, struct_ty, d);
                let fty = struct_fields(struct_ty)
                    .get(*index as usize)
                    .cloned()
                    .unwrap_or(MirTy::Any);
                let fv = self.val(value, &fty);
                let off = t.field_offsets(struct_ty)[*index as usize];
                self.store(fv, &fty, d.at(off));
            }
            RValue::ArrayInit { elem_ty, elements } => {
                let Some(d) = dst else { return };
                let size = t.size_align(elem_ty).0;
                // A big one of constants (`b"..."`): one copy from data.
                let all = size * elements.len() as u32;
                let ty = MirTy::Array(Box::new(elem_ty.clone()), elements.len() as u32);
                // All zero bits (`[0u8; N]`): cleared at once.
                if self.c.opt
                    && all > 0
                    && const_bytes(elements, elem_ty, t).is_some_and(|b| b.iter().all(|&x| x == 0))
                {
                    self.push(Inst::Zero { dst: d, size: all });
                    return;
                }
                if nabi_by_ref(&ty, t) {
                    if let Some(bytes) = const_bytes(elements, elem_ty, t) {
                        let sym = self.c.obj.bytes(&bytes);
                        let w = self.word();
                        let p = self.tmp_k(w);
                        self.push(Inst::Sym {
                            dst: p,
                            sym,
                            got: false,
                        });
                        let Mem::Frame(s, _) = p else { unreachable!() };
                        self.push(Inst::Copy {
                            dst: d,
                            src: Mem::Ind(s, 0),
                            size: all,
                        });
                        return;
                    }
                }
                for (i, e) in elements.iter().enumerate() {
                    let v = self.val(e, elem_ty);
                    self.store(v, elem_ty, d.at(size * i as u32));
                }
            }
            RValue::ArrayIndex {
                array,
                offset,
                index,
                elem_ty,
                len,
            } => {
                let Some(base) = self.array_base(*array) else {
                    return;
                };
                let base = base.at(*offset);
                let p = self.elem_addr(base, index, elem_ty, *len);
                if let Some(d) = dst {
                    let size = t.size_align(elem_ty).0;
                    if size > 0 {
                        self.push(Inst::Copy {
                            dst: d,
                            src: Mem::Ind(p, 0),
                            size,
                        });
                    }
                }
            }
            RValue::FnAddr { callee, .. } => {
                let sig = &self.c.fns[callee];
                let (sym, local) = match sig.c_entry {
                    Some(e) => (e, true),
                    // A function value's callers pass no `nabi_tls`: its
                    // thunk (`module::thunk`).
                    None if sig.tls => {
                        let name = format!("{}.v", self.c.obj.syms[sig.sym as usize].name);
                        self.c.thunks.insert(*callee);
                        (self.c.sym(&name), true)
                    }
                    None => (sig.sym, sig.local),
                };
                if let Some(d) = dst {
                    self.push(Inst::Sym {
                        dst: d,
                        sym,
                        got: !local,
                    });
                }
            }
            RValue::CallIndirect {
                callee,
                args,
                arg_tys,
                ret_ty,
                abi,
            } => {
                let fn_ty = MirTy::Fn(arg_tys.clone(), Box::new(ret_ty.clone()));
                let cv = self.val(callee, &fn_ty);
                let cm = self.mem_of(cv, &fn_ty);
                match abi {
                    Abi::Nabi => {
                        let vals: Vec<Val> = args
                            .iter()
                            .zip(arg_tys)
                            .map(|(a, ty)| self.val(a, ty))
                            .collect();
                        self.call_nabi(
                            Callee::Ptr(cm),
                            arg_tys,
                            ret_ty,
                            &vals,
                            dst,
                            How::default(),
                        );
                    }
                    Abi::C { variadic, system } => {
                        let Some(fa) = classify(
                            &mut self.c.errors,
                            self.f.span,
                            t,
                            arg_tys,
                            ret_ty,
                            (*variadic, *system),
                        ) else {
                            return;
                        };
                        self.call_c(Callee::Ptr(cm), arg_tys, ret_ty, &fa, args, dst);
                    }
                }
            }
            RValue::StaticLoad { name, ty } => {
                self.ensure_static_init(name);
                let p = self.static_addr(name);
                if let Some(d) = dst {
                    self.store(Val::M(Mem::Ind(p, 0)), ty, d);
                }
            }
            RValue::HeapBox { value, ty } => {
                let w = self.word();
                if t.any_inline(ty) {
                    let v = self.val(value, ty);
                    let d = out(self, &MirTy::GcRef);
                    self.bitcast_into(v, ty, &MirTy::Prim(PrimTy::Usize), d);
                    return;
                }
                let obj = self.gc_object(ty);
                let Mem::Frame(os, _) = obj else {
                    unreachable!()
                };
                let v = self.val(value, ty);
                self.store(v, ty, Mem::Ind(os, HEADER_SIZE as i32));
                if let Some(d) = dst {
                    self.push(Inst::Mov {
                        k: w,
                        dst: d,
                        src: V::M(obj),
                    });
                }
            }
            RValue::HeapAlloc { ty } => {
                let w = self.word();
                let obj = self.gc_object(ty);
                if let Some(d) = dst {
                    self.push(Inst::Mov {
                        k: w,
                        dst: d,
                        src: V::M(obj),
                    });
                }
            }
            RValue::HeapField { ptr, ty, index } => {
                let (pv, k) = self.scalar(ptr, &MirTy::GcRef);
                let ps = self.in_slot(pv, k);
                let field = struct_fields(ty)[*index as usize].clone();
                let off = HEADER_SIZE + t.field_offsets(ty)[*index as usize];
                if let Some(d) = dst {
                    self.store(Val::M(Mem::Ind(ps, off as i32)), &field, d);
                }
            }
            RValue::HeapLoad { ptr, ty } => {
                let (pv, k) = self.scalar(ptr, &MirTy::GcRef);
                if t.any_inline(ty) {
                    if let Some(d) = dst {
                        let usize_ = MirTy::Prim(PrimTy::Usize);
                        self.bitcast_into(Val::S(pv, k), &usize_, ty, d);
                    }
                    return;
                }
                let ps = self.in_slot(pv, k);
                if let Some(d) = dst {
                    self.store(Val::M(Mem::Ind(ps, HEADER_SIZE as i32)), ty, d);
                }
            }
            RValue::BufAlloc { elem_ty, len } => {
                let w = self.word();
                let (n, nk) = self.scalar(len, &MirTy::Prim(PrimTy::Usize));
                let tid = if t.has_gc_ref(elem_ty) || self.c.hot_holds(elem_ty) {
                    self.gc_tid(elem_ty, true)
                } else {
                    V::I(i64::from(PLAIN_TYPE_ID))
                };
                let (size, align) = t.size_align(elem_ty);
                let align = align.max(8);
                let umax = u64::MAX >> (64 - pw);
                let limit = (umax - u64::from(HEADER_SIZE)) / u64::from(size.max(1));
                let total = self.word_tmp();
                self.push(Inst::Bin {
                    op: Bin::Mul,
                    k: nk,
                    signed: false,
                    dst: total,
                    a: n,
                    b: V::I(i64::from(size)),
                });
                self.push(Inst::Bin {
                    op: Bin::Add,
                    k: w,
                    signed: false,
                    dst: total,
                    a: V::M(total),
                    b: V::I(i64::from(HEADER_SIZE)),
                });
                let ovf = self.cmp(Cc::Gt, w, false, n, V::I(limit as i64));
                let (big, go) = (self.label(), self.label());
                self.push(Inst::Br {
                    c: ovf,
                    t: big,
                    f: go,
                });
                self.push(Inst::Label(big));
                self.push(Inst::Mov {
                    k: w,
                    dst: total,
                    src: V::I(umax as i64),
                });
                self.push(Inst::Jmp(go));
                self.push(Inst::Label(go));
                let d = out(self, &MirTy::GcRef);
                self.call_rt(
                    "nabi_gc_alloc_slow",
                    &[(V::M(total), w), (V::I(i64::from(align)), w), (tid, K::I32)],
                    Some((d, w)),
                );
            }
            RValue::RawLoad { addr, ty, .. } => {
                let (a, k) = self.scalar(addr, &MirTy::Prim(PrimTy::Usize));
                let ps = self.in_slot(a, k);
                if let Some(d) = dst {
                    self.store(Val::M(Mem::Ind(ps, 0)), ty, d);
                }
            }
            RValue::StaticAddr(name) => {
                self.ensure_static_init(name);
                let p = self.static_addr(name);
                if let Some(d) = dst {
                    let w = self.word();
                    self.push(Inst::Mov {
                        k: w,
                        dst: d,
                        src: V::M(Mem::Frame(p, 0)),
                    });
                }
            }
            RValue::AddrOf(l) => {
                let Some(d) = dst else { return };
                match self.local_mem(*l) {
                    Some(m) => self.push(Inst::Addr { dst: d, src: m }),
                    // A `()` local has no storage: any aligned address does.
                    None => {
                        let w = self.word();
                        self.push(Inst::Mov {
                            k: w,
                            dst: d,
                            src: V::I(1),
                        });
                    }
                }
            }
            RValue::SizeOf { ty, align } => {
                let (s, a) = t.size_align(ty);
                let w = self.word();
                if let Some(d) = dst {
                    self.push(Inst::Mov {
                        k: w,
                        dst: d,
                        src: V::I(i64::from(if *align { a } else { s })),
                    });
                }
            }
            RValue::BufLoad { elem_ty, .. } if *elem_ty == MirTy::Unit => {}
            RValue::BufLoad {
                buf,
                index,
                elem_ty,
            } => {
                let (_, p) = self.buf_elem(buf, index, elem_ty);
                if let Some(d) = dst {
                    self.store(Val::M(Mem::Ind(p, HEADER_SIZE as i32)), elem_ty, d);
                }
            }
            RValue::BufAtomic {
                op,
                buf,
                index,
                args,
                elem_ty,
                order,
            } => {
                let (_, p) = self.buf_elem(buf, index, elem_ty);
                let k = cc::scalar_k(elem_ty, t).unwrap_or(K::I64);
                let a = self.word_tmp();
                self.push(Inst::Addr {
                    dst: a,
                    src: Mem::Ind(p, HEADER_SIZE as i32),
                });
                let Mem::Frame(asl, _) = a else {
                    unreachable!()
                };
                let ptr = Mem::Ind(asl, 0);
                let ops: Vec<V> = args.iter().map(|x| self.scalar(x, elem_ty).0).collect();
                let d = out(self, elem_ty);
                // A cas orders its failed load as well: one ordering for both.
                let mut ord = Order::parse(order[0]);
                if *op == "cas" {
                    ord = ord.join(Order::parse(order[1]));
                }
                let ord = self.order(ord);
                match *op {
                    "load" => self.push(Inst::AtomicLoad {
                        k,
                        dst: d,
                        ptr,
                        ord,
                    }),
                    // A seq_cst store is an exchange whose old value is
                    // dropped; either yields the value stored.
                    "store" => {
                        if ord == Order::SeqCst {
                            let old = self.tmp_k(k);
                            self.push(Inst::AtomicRmw {
                                op: Rmw::Xchg,
                                k,
                                dst: old,
                                ptr,
                                val: ops[0],
                                ord,
                            });
                        } else {
                            self.push(Inst::AtomicStore {
                                k,
                                ptr,
                                src: ops[0],
                                ord,
                            });
                        }
                        self.push(Inst::Mov {
                            k,
                            dst: d,
                            src: ops[0],
                        });
                    }
                    "cas" => self.push(Inst::Cas {
                        k,
                        dst: d,
                        ptr,
                        expected: ops[0],
                        new: ops[1],
                        ord,
                    }),
                    o => {
                        let op = match o {
                            "xchg" => Rmw::Xchg,
                            "add" => Rmw::Add,
                            "sub" => Rmw::Sub,
                            "and" => Rmw::And,
                            "or" => Rmw::Or,
                            _ => Rmw::Xor,
                        };
                        self.push(Inst::AtomicRmw {
                            op,
                            k,
                            dst: d,
                            ptr,
                            val: ops[0],
                            ord,
                        });
                    }
                }
            }
        }
    }
}
