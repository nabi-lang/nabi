//! Blocks, statements, call fusion and element addresses.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnLower<'_, '_> {
    pub(crate) fn block(&mut self, pos: usize, b: &BasicBlock) {
        let l = self.block_label[&b.id.0];
        self.push(Inst::Label(l));
        if let Some(d) = self.deaths.on_entry.get(pos).cloned() {
            for l in d {
                self.clear(l);
            }
        }
        let poll = self.back_edges.contains(&b.id.0);
        // A vector loop in front: the scalar loop's back edge skips it.
        if let Some((ch, inner)) = self.vec.chains.remove(&b.id.0) {
            self.vectorize(&ch, inner);
            self.vec.chains.insert(b.id.0, (ch, inner));
        }
        // The integer comparison the branch reads goes after the poll,
        // right before the branch, which then takes its flags.
        let late = if poll && self.c.opt {
            self.late_cmp(b)
        } else {
            None
        };
        for (i, s) in b.stmts.iter().enumerate() {
            if late == Some(i) {
                self.poll();
            }
            if let Some(packs) = self.slp.remove(&(pos, i)) {
                for pk in &packs {
                    self.pack(pk);
                }
            } else {
                self.plain(pos, i, s);
            }
            let dead = self
                .deaths
                .after_stmt
                .get(pos)
                .and_then(|d| d.get(i))
                .cloned()
                .unwrap_or_default();
            for l in dead {
                self.clear(l);
            }
        }
        if poll && late.is_none() {
            self.poll();
        }
        let back = self.vec.latch.get(&b.id.0).copied();
        let entry = back.map(|h| {
            let inner = self.vec.chains[&h].1;
            (h, self.block_label.insert(h, inner))
        });
        self.terminator(&b.terminator);
        if let Some((h, Some(l))) = entry {
            self.block_label.insert(h, l);
        }
    }

    /// Statement `i` of block `pos`, as other plans have it.
    fn plain(&mut self, pos: usize, i: usize, s: &Statement) {
        match self.combined.remove(&(pos, i)) {
            Some(Some(w)) => self.wide_load(&w),
            Some(None) => {}
            None if self.fuse.skip.contains(&(pos, i)) => {}
            None if self.wb.contains_key(&(pos, i)) => self.writeback(s, pos, i),
            None if self.dead.contains(&(pos, i)) => {}
            None if self.fstores.contains_key(&(pos, i)) => self.field_store(s, pos, i),
            None if self.fuse.inits.contains(&(pos, i)) => self.virt_init(s),
            None => match self.fuse.rets.get(&(pos, i)).cloned() {
                Some(to) => self.fused_call(s, &to),
                None => self.stmt(s),
            },
        }
    }

    /// `fuse::Plan` of this function's builds and calls; its
    /// parameters' was made before they were stored.
    pub(crate) fn fuse_plan(&mut self) {
        let t = self.c.t;
        let fns = self.c.fns;
        let (quiet, pkgs) = (self.c.quiet, self.c.pkgs);
        let own_ret = self.sret.is_none() && self.ret_parts.is_some();
        let arg_parts = |callee: FunctionId, n: usize| {
            fns.get(&callee).is_some_and(|s| {
                s.c.is_none()
                    && !s.throw_site
                    && s.simd.is_none()
                    && s.params.get(n).is_some_and(|ty| !nabi_by_ref(ty, t))
            })
        };
        let ret_parts = |callee: FunctionId| {
            // A `core::simd` call may be vector code reading and writing
            // memory: its operands and result are stored.
            fns.get(&callee).is_some_and(|s| {
                s.c.is_none() && !s.throw_site && s.simd.is_none() && !nabi_by_ref(&s.ret, t)
            })
        };
        let safepoint = |s: &Statement| nabic_codegen::analysis::stmt_safepoint(s, quiet, pkgs, t);
        let p = fuse::plan(
            self.f,
            &fuse::Facts {
                t,
                addressed: &self.addressed,
                rooted: &self.rooted,
                deaths: &self.deaths,
                arg_parts: &arg_parts,
                ret_parts: &ret_parts,
                own_ret,
                safepoint: &safepoint,
            },
        );
        self.fuse.inits = p.inits;
        self.fuse.rets = p.rets;
        self.fuse.skip.extend(p.skip);
    }

    /// Where parameter `p`'s parts at each offset go instead of its slot
    /// (`fuse::Plan::params`).
    pub(crate) fn fuse_param(&self, p: LocalId) -> Vec<(u32, Mem)> {
        let Some(to) = self.fuse.params.get(&p) else {
            return Vec::new();
        };
        to.iter()
            .filter_map(|&(off, x, at)| Some((off, self.local_mem(x)?.at(at))))
            .collect()
    }

    /// Per leaf of the fields `fs` read into the locals `to`: its offset
    /// in the struct and its place in the local.
    pub(crate) fn field_parts(
        &self,
        to: &[Option<LocalId>],
        fs: &[(MirTy, u32)],
    ) -> Vec<(u32, Mem)> {
        let mut out = Vec::new();
        for (x, (fty, off)) in to.iter().zip(fs) {
            let Some(m) = x.and_then(|x| self.local_mem(x)) else {
                continue;
            };
            for l in fuse::leaves(fty, self.c.t) {
                out.push((off + l.off, m.at(l.off)));
            }
        }
        out
    }

    /// A struct build `fuse` keeps out of memory: where its fields are.
    pub(crate) fn virt_init(&mut self, s: &Statement) {
        let Statement::Assign {
            target,
            rvalue: RValue::StructInit { ty, fields },
        } = s
        else {
            return self.stmt(s);
        };
        let (Some(fs), Some(Mem::Frame(slot, 0))) =
            (fuse::flat(ty, self.c.t), self.local_mem(*target))
        else {
            return self.stmt(s);
        };
        let mut at = Vec::with_capacity(fs.len());
        for ((fty, off), op) in fs.iter().zip(fields) {
            match self.val(op, fty) {
                Val::S(v, _) => at.push((*off, v)),
                Val::M(m) => {
                    for l in fuse::leaves(fty, self.c.t) {
                        at.push((off + l.off, V::M(m.at(l.off))));
                    }
                }
                Val::Unit => {}
            }
        }
        self.virt.insert(slot, at);
    }

    /// A call whose result parts go to the locals reading its fields
    /// (`fuse::Plan::rets`).
    pub(crate) fn fused_call(&mut self, s: &Statement, to: &[Option<LocalId>]) {
        let Statement::Assign {
            target,
            rvalue: RValue::Call { callee, .. },
        } = s
        else {
            return self.stmt(s);
        };
        let ty = self.local_ty(*target);
        let (Some(fs), Some(d), Some(sig)) = (
            fuse::flat(&ty, self.c.t),
            self.local_mem(*target),
            self.c.fns.get(callee),
        ) else {
            return self.stmt(s);
        };
        let map = self.field_parts(to, &fs);
        self.ret_to = Some((sig.sym, map));
        self.stmt(s);
        // Not a call taking it: the field reads, from the whole result.
        if self.ret_to.take().is_some() {
            for (x, (fty, off)) in to.iter().zip(&fs) {
                if let Some(m) = x.and_then(|x| self.local_mem(x)) {
                    let size = self.c.t.size_align(fty).0;
                    self.push(Inst::Copy {
                        dst: m,
                        src: d.at(*off),
                        size,
                    });
                }
            }
        }
    }

    /// A combined load (`combine`).
    pub(crate) fn wide_load(&mut self, w: &combine::Wide) {
        let Some(dst) = self.local_mem(w.target) else {
            return;
        };
        let (_, p) = self.buf_elem(&w.buf, &w.index, &MirTy::Prim(PrimTy::U8));
        self.store(Val::M(Mem::Ind(p, HEADER_SIZE as i32)), &w.ty, dst);
        if w.swap {
            let k = cc::scalar_k(&w.ty, self.c.t).unwrap_or(K::I64);
            self.push(Inst::Un {
                op: Un::Bswap,
                k,
                dst,
                a: V::M(dst),
            });
        }
    }

    /// The statement of `b` computing its branch condition as a
    /// comparison of integers, with only positions after it.
    pub(crate) fn late_cmp(&self, b: &BasicBlock) -> Option<usize> {
        let Terminator::If {
            cond: Operand::Copy(c),
            ..
        } = &b.terminator
        else {
            return None;
        };
        let i = b
            .stmts
            .iter()
            .rposition(|s| !matches!(s, Statement::Loc(..)))?;
        let Statement::Assign {
            target,
            rvalue:
                RValue::BinOp {
                    op: PrimOp::Lt | PrimOp::Le | PrimOp::Gt | PrimOp::Ge | PrimOp::Eq | PrimOp::Ne,
                    op_ty: MirTy::Prim(p),
                    ..
                },
        } = &b.stmts[i]
        else {
            return None;
        };
        (target == c && p.is_int() && !self.rooted.contains(&c.0)).then_some(i)
    }

    pub(crate) fn stmt(&mut self, s: &Statement) {
        let t = self.c.t;
        match s {
            Statement::Assign { target, rvalue } => {
                // Read in place: nothing to copy (lazy statics start).
                if let Some(ro) = self.ro.get(target) {
                    if let Ro::Static(name) = ro.clone() {
                        self.ensure_static_init(&name);
                    }
                    return;
                }
                let ty = self.local_ty(*target);
                if ty == MirTy::Unit && matches!(rvalue, RValue::Use(_)) {
                    return;
                }
                let dst = self.local_mem(*target);
                // A callee may build a by-address result as it
                // goes: a local a pointer may read gets it once done.
                if let Some(d) = dst.filter(|_| {
                    self.addressed.contains(target)
                        && nabi_by_ref(&ty, t)
                        && matches!(rvalue, RValue::Call { .. } | RValue::CallIndirect { .. })
                }) {
                    let tmp = self.tmp(&ty);
                    self.rvalue(&ty, rvalue, Some(tmp));
                    self.store(Val::M(tmp), &ty, d);
                    return;
                }
                if let (Some(d), RValue::ArrayInit { elem_ty, elements }) = (dst, rvalue) {
                    if self.c.opt && self.simd_build(*target, elem_ty, elements, d) {
                        return;
                    }
                }
                self.rvalue(&ty, rvalue, dst);
            }
            Statement::ArrayWrite {
                array,
                offset,
                index,
                value,
                elem_ty,
                len,
            } => {
                let Some(base) = self.local_mem(*array) else {
                    return;
                };
                let base = base.at(*offset);
                let p = self.elem_addr(base, index, elem_ty, *len);
                let v = self.val(value, elem_ty);
                self.store(v, elem_ty, Mem::Ind(p, 0));
            }
            Statement::StorageDead(l) => {
                if self.rooted.contains(&l.0) {
                    self.clear(*l);
                }
            }
            Statement::BufStore { elem_ty, .. } if *elem_ty == MirTy::Unit => {}
            Statement::BufStore {
                buf,
                index,
                value,
                elem_ty,
            } => {
                let (b, p) = self.buf_elem(buf, index, elem_ty);
                if t.has_gc_ref(elem_ty) {
                    let len = t.size_align(elem_ty).0;
                    let a = self.word_tmp();
                    self.push(Inst::Addr {
                        dst: a,
                        src: Mem::Ind(p, HEADER_SIZE as i32),
                    });
                    let Mem::Frame(asl, _) = a else {
                        unreachable!()
                    };
                    let size = t.size_align(elem_ty).0;
                    self.write_barrier(b, asl, len, Card::Elem(size));
                }
                let v = self.val(value, elem_ty);
                self.store(v, elem_ty, Mem::Ind(p, HEADER_SIZE as i32));
            }
            Statement::Loc(sp, scope) => {
                self.at = *sp;
                self.push(Inst::Line(sp.line, sp.col.max(1), scope.id));
            }
            Statement::RawStore {
                addr, value, ty, ..
            } => {
                let w = MirTy::Prim(PrimTy::Usize);
                let (a, k) = self.scalar(addr, &w);
                let ps = self.in_slot(a, k);
                let v = self.val(value, ty);
                self.store(v, ty, Mem::Ind(ps, 0));
            }
            Statement::HeapStore {
                ptr,
                ty,
                index,
                value,
            } => {
                let field = struct_fields(ty)[*index as usize].clone();
                let off = HEADER_SIZE + t.field_offsets(ty)[*index as usize];
                let (pv, k) = self.scalar(ptr, &MirTy::GcRef);
                let ps = self.in_slot(pv, k);
                if t.has_gc_ref(&field) {
                    let len = t.size_align(&field).0;
                    let a = self.word_tmp();
                    self.push(Inst::Addr {
                        dst: a,
                        src: Mem::Ind(ps, off as i32),
                    });
                    let Mem::Frame(asl, _) = a else {
                        unreachable!()
                    };
                    self.write_barrier(ps, asl, len, Card::At(off));
                }
                let v = self.val(value, &field);
                self.store(v, &field, Mem::Ind(ps, off as i32));
            }
            Statement::StaticStore { name, value, ty } => {
                self.ensure_static_init(name);
                let v = self.val(value, ty);
                let p = self.static_addr(name);
                self.store(v, ty, Mem::Ind(p, 0));
            }
        }
    }

    /// `b[i] = S { .. }` where the build takes most fields from the same
    /// element as just read (`b[i].k`, or `heap_field b.k` for element
    /// 0) with nothing between that writes memory: a store of the other
    /// fields only, each with a barrier only if it holds a reference.
    /// The load and the build go.
    pub(crate) fn field_stores(&mut self) {
        let f = self.f;
        let mut reads = vec![0u32; f.locals.len()];
        for b in &f.blocks {
            for s in &b.stmts {
                nabic_mir::stmt_reads(s, &mut |l| reads[l.0 as usize] += 1);
            }
            nabic_mir::term_reads(&b.terminator, &mut |l| reads[l.0 as usize] += 1);
        }
        for (pos, b) in f.blocks.iter().enumerate() {
            for (i, s) in b.stmts.iter().enumerate() {
                let Statement::BufStore {
                    buf,
                    index,
                    value: Operand::Copy(x),
                    elem_ty,
                } = s
                else {
                    continue;
                };
                let n = struct_fields(elem_ty).len();
                if n == 0 || reads[x.0 as usize] != 1 || self.rooted.contains(&x.0) {
                    continue;
                }
                let zero = matches!(index, Operand::Const(_))
                    && nabic_codegen::analysis::const_int(index) == Some(0);
                // Back from the store: the build, the field reads of this
                // element (`HeapField` / `FieldRead` of its `BufLoad`) and
                // the last statement that may write memory.
                let mut init = None;
                let mut heap: HashMap<LocalId, (u32, usize)> = HashMap::default();
                let mut field: HashMap<LocalId, (LocalId, u32, usize)> = HashMap::default();
                let mut elems: HashMap<LocalId, usize> = HashMap::default();
                let mut wall = None;
                // An earlier store of a build to this element: what each
                // field holds since.
                let mut prior: Option<Vec<Operand>> = None;
                for j in (0..i).rev() {
                    let st = &b.stmts[j];
                    let Statement::Assign { target, rvalue } = st else {
                        match st {
                            Statement::Loc(..) => continue,
                            // Another type's buffer is another object.
                            Statement::BufStore { elem_ty: e, .. } if e != elem_ty => continue,
                            Statement::BufStore {
                                buf: pb,
                                index: pi,
                                value: Operand::Copy(y),
                                ..
                            } if pb == buf && pi == index => {
                                prior = b.stmts[..j].iter().rev().find_map(|s| match s {
                                    Statement::Assign {
                                        target,
                                        rvalue: RValue::StructInit { fields, .. },
                                    } if target == y => Some(fields.clone()),
                                    _ => None,
                                });
                                // Its fields' values must hold until here.
                                let held = prior.as_ref().is_some_and(|fs| {
                                    !b.stmts[j + 1..i].iter().any(|s| {
                                        matches!(s, Statement::Assign { target, .. }
                                            if fs.contains(&Operand::Copy(*target)))
                                    })
                                });
                                if !held {
                                    prior = None;
                                }
                            }
                            _ => {}
                        }
                        wall = Some(j);
                        break;
                    };
                    if Operand::Copy(*target) == *buf || Operand::Copy(*target) == *index {
                        wall = Some(j);
                        break;
                    }
                    match rvalue {
                        RValue::StructInit { ty, fields } if target == x && ty == elem_ty => {
                            init = init.or_else(|| Some((j, fields.clone())));
                        }
                        RValue::HeapField { ptr, ty, index: k }
                            if zero && ptr == buf && ty == elem_ty =>
                        {
                            heap.entry(*target).or_insert((*k, j));
                        }
                        RValue::FieldRead {
                            target: Operand::Copy(e),
                            index: k,
                            ..
                        } => {
                            field.entry(*target).or_insert((*e, *k, j));
                        }
                        RValue::BufLoad {
                            buf: lb,
                            index: li,
                            elem_ty: le,
                        } if lb == buf && li == index && le == elem_ty => {
                            elems.insert(*target, j);
                        }
                        RValue::Use(_)
                        | RValue::BinOp { .. }
                        | RValue::UnaryOp { .. }
                        | RValue::StructInit { .. }
                        | RValue::Cast { .. }
                        | RValue::Bitcast { .. }
                        | RValue::WithField { .. }
                        | RValue::HeapField { .. }
                        | RValue::BufLoad { .. }
                        | RValue::SizeOf { .. } => {}
                        _ => {
                            wall = Some(j);
                            break;
                        }
                    }
                }
                let Some((j, fields)) = init else {
                    continue;
                };
                // The local holding field `k` of this element as read
                // after the wall, and the statements reading it.
                let after = |at: usize| wall.is_none_or(|w| at > w);
                // `Some(reads)`: field `k` stays as it is; `reads` the
                // statements reading it that may go.
                let own = |k: usize, o: &Operand| -> Option<Vec<usize>> {
                    if prior.as_ref().is_some_and(|fs| fs.get(k) == Some(o)) {
                        return Some(Vec::new());
                    }
                    let Operand::Copy(l) = o else { return None };
                    let single = reads[l.0 as usize] == 1 && !self.rooted.contains(&l.0);
                    let go = |at: usize| if single { vec![at] } else { Vec::new() };
                    if let Some(&(hk, at)) = heap.get(l) {
                        return (hk as usize == k && after(at) && at < j).then(|| go(at));
                    }
                    let &(e, fk, at) = field.get(l)?;
                    let &le = elems.get(&e)?;
                    (fk as usize == k && after(le) && le < at && at < j).then(|| go(at))
                };
                let mut skip = Vec::new();
                let mut changed = Vec::new();
                let mut per_elem: HashMap<LocalId, u32> = HashMap::default();
                for (k, o) in fields.iter().enumerate() {
                    match own(k, o) {
                        Some(at) => {
                            // A root's slot is written still (`own`): one
                            // never written would be shared, and its clear
                            // would hit another root.
                            if let (Operand::Copy(l), false) = (o, at.is_empty()) {
                                if let Some(&(e, _, _)) = field.get(l) {
                                    *per_elem.entry(e).or_default() += 1;
                                }
                            }
                            skip.extend(at);
                        }
                        None => changed.push((k as u32, o.clone())),
                    }
                }
                // Nothing between the build and the store assigns a value
                // it takes.
                let reassigned = b.stmts[j + 1..i].iter().any(|st| {
                    matches!(st, Statement::Assign { target, .. }
                        if changed.iter().any(|(_, o)| *o == Operand::Copy(*target)))
                });
                if changed.len() == n || !after(j) || reassigned {
                    continue;
                }
                // The element's load goes when the build's reads were all
                // its reads; else it stays (its other readers need it).
                for (e, c) in per_elem {
                    if c == reads[e.0 as usize] && !self.rooted.contains(&e.0) {
                        skip.push(elems[&e]);
                    }
                }
                // The values are read at the store now: they die there.
                if let Some(ds) = self.deaths.after_stmt.get_mut(pos) {
                    for (_, o) in &changed {
                        let Operand::Copy(l) = o else { continue };
                        let mut died = false;
                        for d in &mut ds[j..i] {
                            died |= d.contains(l);
                            d.retain(|x| x != l);
                        }
                        if died && !ds[i].contains(l) {
                            ds[i].push(*l);
                        }
                    }
                }
                self.dead.insert((pos, j));
                for &k in &skip {
                    self.dead.insert((pos, k));
                }
                self.fstores.insert((pos, i), changed);
            }
        }
    }

    /// The store `field_stores` planned for statement `s`.
    pub(crate) fn field_store(&mut self, s: &Statement, pos: usize, i: usize) {
        let (
            Some(changed),
            Statement::BufStore {
                buf,
                index,
                elem_ty,
                ..
            },
        ) = (self.fstores.get(&(pos, i)).cloned(), s)
        else {
            return self.stmt(s);
        };
        let t = self.c.t;
        let (b, p) = self.buf_elem(buf, index, elem_ty);
        let offs = t.field_offsets(elem_ty);
        let size = t.size_align(elem_ty).0;
        for (k, v) in changed {
            let fty = struct_fields(elem_ty)[k as usize].clone();
            let at = Mem::Ind(p, (HEADER_SIZE + offs[k as usize]) as i32);
            if t.has_gc_ref(&fty) {
                let len = t.size_align(&fty).0;
                let a = self.word_tmp();
                self.push(Inst::Addr { dst: a, src: at });
                let Mem::Frame(asl, _) = a else {
                    unreachable!()
                };
                self.write_barrier(b, asl, len, Card::Elem(size));
            }
            let v = self.val(&v, &fty);
            self.store(v, &fty, at);
        }
    }

    /// Address of element `index` of the array at `base` (bounds checked).
    pub(crate) fn elem_addr(
        &mut self,
        base: Mem,
        index: &Operand,
        elem_ty: &MirTy,
        len: u32,
    ) -> Slot {
        let ity = self.op_ty(index);
        let (iv, ik) = self.scalar(index, &ity);
        let in_range = const_int(index).is_some_and(|c| (0..i128::from(len)).contains(&c));
        if !in_range {
            let fail = self.cmp(Cc::Ge, ik, false, iv, V::I(i64::from(len)));
            self.check(fail, "index out of bounds");
        }
        let signed = matches!(ity, MirTy::Prim(p) if p.is_signed_int());
        let w = self.word();
        let i64_ = self.word_tmp();
        self.push(Inst::Conv {
            conv: Conv::Int { signed },
            from: ik,
            to: w,
            dst: i64_,
            a: iv,
        });
        let size = self.c.t.size_align(elem_ty).0;
        let off = self.word_tmp();
        self.push(Inst::Bin {
            op: Bin::Mul,
            k: w,
            signed: false,
            dst: off,
            a: V::M(i64_),
            b: V::I(i64::from(size)),
        });
        let a = self.word_tmp();
        self.push(Inst::Addr { dst: a, src: base });
        let p = self.word_tmp();
        self.push(Inst::Bin {
            op: Bin::Add,
            k: w,
            signed: false,
            dst: p,
            a: V::M(a),
            b: V::M(off),
        });
        let Mem::Frame(ps, _) = p else { unreachable!() };
        ps
    }

    /// `(buffer slot, element address minus the header)` of a `BufAlloc`
    /// element.
    pub(crate) fn buf_elem(
        &mut self,
        buf: &Operand,
        index: &Operand,
        elem_ty: &MirTy,
    ) -> (Slot, Slot) {
        let w = self.word();
        let (bv, bk) = self.scalar(buf, &MirTy::GcRef);
        let bs = self.in_slot(bv, bk);
        let (iv, _) = self.scalar(index, &MirTy::Prim(PrimTy::Usize));
        let size = i64::from(self.c.t.size_align(elem_ty).0);
        let off = if let V::I(i) = iv {
            V::I(i.wrapping_mul(size))
        } else {
            let o = self.word_tmp();
            self.push(Inst::Bin {
                op: Bin::Mul,
                k: w,
                signed: false,
                dst: o,
                a: iv,
                b: V::I(size),
            });
            V::M(o)
        };
        let p = self.word_tmp();
        self.push(Inst::Bin {
            op: Bin::Add,
            k: w,
            signed: false,
            dst: p,
            a: off,
            b: V::M(Mem::Frame(bs, 0)),
        });
        let Mem::Frame(ps, _) = p else { unreachable!() };
        (bs, ps)
    }
}
