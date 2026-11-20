//! Emitting a planned vector loop in front of the scalar one.

#[allow(clippy::wildcard_imports)]
use crate::lower::vector::*;

impl FnLower<'_, '_> {
    /// In front of the scalar loop `ch`, its vector loop when it is one;
    /// then `inner`, where the scalar loop's back edge goes.
    pub(crate) fn vectorize(&mut self, ch: &Body, inner: Label) {
        if let Some(p) = Isa::of(self.c.t).and_then(|isa| self.plan(ch, isa)) {
            self.vector_loop(p, inner);
        }
        self.push(Inst::Label(inner));
    }

    /// Lower plan `p`, `inner` the scalar loop's header.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn vector_loop(&mut self, p: Plan, inner: Label) {
        let wk = self.word();
        let usize_ty = MirTy::Prim(PrimTy::Usize);
        let Some(ivm) = self.local_mem(p.iv) else {
            return;
        };
        if let Some((l, c, s)) = p.line {
            self.push(Inst::Line(l, c, s));
        }
        for (t, rv) in &p.hoist {
            self.stmt(&Statement::Assign {
                target: *t,
                rvalue: rv.clone(),
            });
        }
        let io = self.out.slot(p.io_size, 16);
        let cell = |o: u32| Mem::Frame(io, o);
        let (bound, _) = self.scalar(&p.bound, &usize_ty);
        // The bound the vector loop keeps to: short of every side exit
        // on the counter.
        let stop = if p.limits.is_empty() && p.outer.is_none() {
            bound
        } else {
            let m = self.word_tmp();
            self.push(Inst::Mov {
                k: wk,
                dst: m,
                src: bound,
            });
            if let Some(o) = &p.outer {
                // Past the strip, `MAX_TRIPS` at most: the strip loop
                // polls between.
                let (ov, _) = self.scalar(o, &usize_ty);
                self.push(Inst::Bin {
                    op: Bin::Add,
                    k: wk,
                    signed: false,
                    dst: m,
                    a: V::M(ivm),
                    b: V::I(MAX_TRIPS),
                });
                let lower = self.cmp(Cc::Lt, wk, false, ov, V::M(m));
                let (set, join) = (self.label(), self.label());
                self.push(Inst::Br {
                    c: lower,
                    t: set,
                    f: join,
                });
                self.push(Inst::Label(set));
                self.push(Inst::Mov {
                    k: wk,
                    dst: m,
                    src: ov,
                });
                self.push(Inst::Jmp(join));
                self.push(Inst::Label(join));
            }
            for (l, idx) in &p.limits {
                let (lv, _) = self.scalar(l, &usize_ty);
                let off = idx.c as i64;
                if off > 0 {
                    let under = self.cmp(Cc::Lt, wk, false, lv, V::I(off));
                    let go = self.label();
                    self.push(Inst::Br {
                        c: under,
                        t: inner,
                        f: go,
                    });
                    self.push(Inst::Label(go));
                }
                let t = self.word_tmp();
                self.push(Inst::Bin {
                    op: Bin::Sub,
                    k: wk,
                    signed: false,
                    dst: t,
                    a: lv,
                    b: V::I(off),
                });
                if let Some(x) = idx.inv {
                    // `l - c - inv`, none left below `inv`.
                    let (xv, _) = self.scalar(&Operand::Copy(x), &usize_ty);
                    let under = self.cmp(Cc::Lt, wk, false, V::M(t), xv);
                    let go = self.label();
                    self.push(Inst::Br {
                        c: under,
                        t: inner,
                        f: go,
                    });
                    self.push(Inst::Label(go));
                    self.push(Inst::Bin {
                        op: Bin::Sub,
                        k: wk,
                        signed: false,
                        dst: t,
                        a: V::M(t),
                        b: xv,
                    });
                }
                let lower = self.cmp(Cc::Lt, wk, false, V::M(t), V::M(m));
                let (set, join) = (self.label(), self.label());
                self.push(Inst::Br {
                    c: lower,
                    t: set,
                    f: join,
                });
                self.push(Inst::Label(set));
                self.push(Inst::Mov {
                    k: wk,
                    dst: m,
                    src: V::M(t),
                });
                self.push(Inst::Jmp(join));
                self.push(Inst::Label(join));
            }
            V::M(m)
        };
        for &u in &p.units {
            let (v, _) = self.scalar(&Operand::Copy(u), &usize_ty);
            let strided = self.cmp(Cc::Ne, wk, false, v, V::I(1));
            let go = self.label();
            self.push(Inst::Br {
                c: strided,
                t: inner,
                f: go,
            });
            self.push(Inst::Label(go));
        }
        if let Some(invs) = &p.guard {
            // Every index below `isize::MAX` by far: none overflows or
            // turns negative as an `isize`.
            let top = V::I(1i64 << (wk.size() * 8 - 2));
            let mut vs = vec![stop];
            for x in invs {
                vs.push(self.scalar(&Operand::Copy(*x), &usize_ty).0);
            }
            for v in vs {
                let over = self.cmp(Cc::Gt, wk, false, v, top);
                let go = self.label();
                self.push(Inst::Br {
                    c: over,
                    t: inner,
                    f: go,
                });
                self.push(Inst::Label(go));
            }
        }
        for &(m, k) in &p.nan_free {
            let ty = MirTy::Prim(if k == K::F64 {
                PrimTy::F64
            } else {
                PrimTy::F32
            });
            let (v, _) = self.scalar(&Operand::Copy(m), &ty);
            let nan = self.cmp(Cc::Ne, k, false, v, v);
            let go = self.label();
            self.push(Inst::Br {
                c: nan,
                t: inner,
                f: go,
            });
            self.push(Inst::Label(go));
        }
        let mut base_vals = Vec::new();
        for &(l, idx, e) in &p.bases {
            let (v, _) = self.scalar(&Operand::Copy(l), &MirTy::GcRef);
            let Some(x) = idx.inv else {
                base_vals.push(v);
                continue;
            };
            // The buffer from element `inv` on.
            let (xv, _) = self.scalar(&Operand::Copy(x), &usize_ty);
            let t = self.word_tmp();
            self.push(Inst::Bin {
                op: Bin::Mul,
                k: wk,
                signed: false,
                dst: t,
                a: xv,
                b: V::I(i64::from(e)),
            });
            self.push(Inst::Bin {
                op: Bin::Add,
                k: wk,
                signed: false,
                dst: t,
                a: V::M(t),
                b: v,
            });
            base_vals.push(V::M(t));
        }
        for &(x, y) in &p.distinct {
            let (x, _) = self.scalar(&Operand::Copy(x), &MirTy::GcRef);
            let (y, _) = self.scalar(&Operand::Copy(y), &MirTy::GcRef);
            let same = self.cmp(Cc::Eq, wk, false, x, y);
            let go = self.label();
            self.push(Inst::Br {
                c: same,
                t: inner,
                f: go,
            });
            self.push(Inst::Label(go));
        }
        // Two rows of one buffer: scalar unless `0 < |x - y| < lanes`
        // fails (the indices are far below `isize::MAX`).
        let lanes = i64::from(p.vb / p.w);
        for &(x, y) in &p.apart {
            let mut at = |i: Idx| -> V {
                let t = self.word_tmp();
                let base = match i.inv {
                    Some(l) => self.scalar(&Operand::Copy(l), &usize_ty).0,
                    None => V::I(0),
                };
                self.push(Inst::Bin {
                    op: Bin::Add,
                    k: wk,
                    signed: false,
                    dst: t,
                    a: base,
                    b: V::I(i.c as i64),
                });
                V::M(t)
            };
            let (xv, yv) = (at(x), at(y));
            let d = self.word_tmp();
            self.push(Inst::Bin {
                op: Bin::Sub,
                k: wk,
                signed: false,
                dst: d,
                a: xv,
                b: yv,
            });
            let near = self.word_tmp();
            self.push(Inst::Bin {
                op: Bin::Add,
                k: wk,
                signed: false,
                dst: near,
                a: V::M(d),
                b: V::I(lanes - 1),
            });
            let close = self.cmp(Cc::Lt, wk, false, V::M(near), V::I(2 * lanes - 1));
            let (test, go) = (self.label(), self.label());
            self.push(Inst::Br {
                c: close,
                t: test,
                f: go,
            });
            self.push(Inst::Label(test));
            let apart = self.cmp(Cc::Ne, wk, false, V::M(d), V::I(0));
            self.push(Inst::Br {
                c: apart,
                t: inner,
                f: go,
            });
            self.push(Inst::Label(go));
        }
        self.push(Inst::Mov {
            k: wk,
            dst: cell(VIO_IV),
            src: V::M(ivm),
        });
        self.push(Inst::Mov {
            k: wk,
            dst: cell(VIO_BOUND),
            src: stop,
        });
        for (n, v) in base_vals.iter().enumerate() {
            self.push(Inst::Mov {
                k: wk,
                dst: cell(VIO_BASES + 8 * n as u32),
                src: *v,
            });
        }
        let lk = lane_k(p.w);
        for (o, ty, off) in &p.splats {
            let (v, k) = self.scalar(o, ty);
            if k.size() == p.w {
                self.push(Inst::Mov {
                    k,
                    dst: cell(*off),
                    src: v,
                });
            } else {
                // A `bool`: 0 / 1 in a wider lane.
                self.push(Inst::Conv {
                    conv: Conv::Int { signed: false },
                    from: k,
                    to: lk,
                    dst: cell(*off),
                    a: v,
                });
            }
        }
        let reduce = !p.accs.is_empty();
        let iv0 = self.word_tmp();
        if reduce || p.skip {
            self.push(Inst::Mov {
                k: wk,
                dst: iv0,
                src: V::M(ivm),
            });
        }
        let lanes = p.vb / p.w;
        self.push(Inst::VLoop(Box::new(VLoop {
            io,
            w: p.w,
            vb: p.vb,
            min_left: if p.skip { lanes } else { lanes + 1 },
            bases: p
                .bases
                .iter()
                .map(|(_, idx, e)| (HEADER_SIZE + idx.c as u32 * e) as i32)
                .collect(),
            elem: p.bases.iter().map(|b| b.2).collect(),
            pre: p.pre,
            body: p.body,
            post: p.post.clone(),
            regs: p.regs,
            save: p.save,
        })));
        self.push(Inst::Mov {
            k: wk,
            dst: ivm,
            src: V::M(cell(VIO_IV)),
        });
        if !reduce && !p.skip {
            return;
        }
        let ran = self.cmp(Cc::Ne, wk, false, V::M(ivm), V::M(iv0));
        let after = self.label();
        self.push(Inst::Br {
            c: ran,
            t: after,
            f: inner,
        });
        self.push(Inst::Label(after));
        let restart = self.label();
        let mut rerun = false;
        // Every result first, then the accumulators: a rerun finds them
        // as they were.
        let mut done = Vec::new();
        let mut out = p.post.iter().map(|x| x.1);
        for a in &p.accs {
            let Some(am) = self.local_mem(a.local) else {
                for _ in &a.regs {
                    out.next();
                }
                continue;
            };
            let k = a.k;
            let lane = |o: u32, j: u32| V::M(Mem::Frame(io, o + j * k.size()));
            let n = p.vb / k.size();
            let sum = |s: &mut Self, o: u32, op: Bin, ovf: Option<bool>| -> Mem {
                let t = s.tmp_k(k);
                s.push(Inst::Mov {
                    k,
                    dst: t,
                    src: lane(o, 0),
                });
                for j in 1..n {
                    match ovf {
                        Some(signed) => s.push(Inst::Ovf {
                            op,
                            k,
                            signed,
                            dst: t,
                            a: V::M(t),
                            b: lane(o, j),
                            fail: restart,
                        }),
                        None => s.push(Inst::Bin {
                            op,
                            k,
                            signed: false,
                            dst: t,
                            a: V::M(t),
                            b: lane(o, j),
                        }),
                    }
                }
                t
            };
            let r = self.tmp_k(k);
            match a.red {
                Red::MinMax { max, signed } => {
                    // The lanes started from the accumulator: the least
                    // (greatest) of them. Float lanes hold no NaN, nor
                    // zeros of both signs.
                    let o = out.next().expect("a result");
                    self.push(Inst::Mov {
                        k,
                        dst: r,
                        src: lane(o, 0),
                    });
                    let cc = if max { Cc::Gt } else { Cc::Lt };
                    for j in 1..n {
                        let better = self.cmp(cc, k, signed, lane(o, j), V::M(r));
                        let (set, join) = (self.label(), self.label());
                        self.push(Inst::Br {
                            c: better,
                            t: set,
                            f: join,
                        });
                        self.push(Inst::Label(set));
                        self.push(Inst::Mov {
                            k,
                            dst: r,
                            src: lane(o, j),
                        });
                        self.push(Inst::Jmp(join));
                        self.push(Inst::Label(join));
                    }
                }
                Red::Count { checked, signed } => {
                    // Lanes of counts, zero-extended or narrowed.
                    let o = out.next().expect("a result");
                    let lk = a.lane;
                    let t = self.tmp_k(k);
                    for j in 0..p.vb / lk.size() {
                        let v = V::M(Mem::Frame(io, o + j * lk.size()));
                        let u = if j == 0 { t } else { self.tmp_k(k) };
                        if lk == k {
                            self.push(Inst::Mov { k, dst: u, src: v });
                        } else {
                            self.push(Inst::Conv {
                                conv: Conv::Int { signed: false },
                                from: lk,
                                to: k,
                                dst: u,
                                a: v,
                            });
                        }
                        if j > 0 {
                            self.push(Inst::Bin {
                                op: Bin::Add,
                                k,
                                signed: false,
                                dst: t,
                                a: V::M(t),
                                b: V::M(u),
                            });
                        }
                    }
                    if checked {
                        rerun = true;
                        self.push(Inst::Ovf {
                            op: Bin::Add,
                            k,
                            signed,
                            dst: r,
                            a: V::M(am),
                            b: V::M(t),
                            fail: restart,
                        });
                    } else {
                        self.push(Inst::Bin {
                            op: Bin::Add,
                            k,
                            signed: false,
                            dst: r,
                            a: V::M(am),
                            b: V::M(t),
                        });
                    }
                }
                Red::Dot {
                    checked,
                    signed,
                    acc_signed,
                } => {
                    // The lanes' sums fit: an entry runs `MAX_TRIPS`
                    // iterations at most, each adding four products of
                    // bytes to a lane.
                    let o = out.next().expect("a result");
                    let t = sum(self, o, Bin::Add, None);
                    if checked {
                        rerun = true;
                        // Every partial sum lies within the magnitudes'
                        // sum of the accumulator (products of unsigned
                        // bytes: above it, by their sum itself).
                        let (m, ops) = if signed {
                            let mo = out.next().expect("a result");
                            (sum(self, mo, Bin::Add, None), &[Bin::Add, Bin::Sub][..])
                        } else {
                            (t, &[Bin::Add][..])
                        };
                        for &op in ops {
                            let x = self.tmp_k(k);
                            self.push(Inst::Ovf {
                                op,
                                k,
                                signed: acc_signed,
                                dst: x,
                                a: V::M(am),
                                b: V::M(m),
                                fail: restart,
                            });
                        }
                    }
                    self.push(Inst::Bin {
                        op: Bin::Add,
                        k,
                        signed: false,
                        dst: r,
                        a: V::M(am),
                        b: V::M(t),
                    });
                }
                Red::Wrap(op) => {
                    let o = out.next().expect("a result");
                    let t = sum(self, o, op, None);
                    self.push(Inst::Bin {
                        op,
                        k,
                        signed: false,
                        dst: r,
                        a: V::M(am),
                        b: V::M(t),
                    });
                }
                Red::WrapSub => {
                    let o = out.next().expect("a result");
                    let t = sum(self, o, Bin::Add, None);
                    self.push(Inst::Bin {
                        op: Bin::Sub,
                        k,
                        signed: false,
                        dst: r,
                        a: V::M(am),
                        b: V::M(t),
                    });
                }
                Red::Checked { sub, signed } => {
                    rerun = true;
                    let po = out.next().expect("a result");
                    let no = if signed {
                        Some((out.next().expect("a result"), out.next().expect("a result")))
                    } else {
                        None
                    };
                    let oo = out.next().expect("a result");
                    // A lane's sum overflowed.
                    let any = sum(self, oo, Bin::Or, None);
                    let bad = self.cmp(Cc::Lt, k, true, V::M(any), V::I(0));
                    let ok = self.label();
                    self.push(Inst::Br {
                        c: bad,
                        t: restart,
                        f: ok,
                    });
                    self.push(Inst::Label(ok));
                    let op = if sub { Bin::Sub } else { Bin::Add };
                    let pt = sum(self, po, Bin::Add, Some(signed));
                    self.push(Inst::Ovf {
                        op,
                        k,
                        signed,
                        dst: r,
                        a: V::M(am),
                        b: V::M(pt),
                        fail: restart,
                    });
                    if let Some((no, co)) = no {
                        // The negative elements: `-(sum of !x) - count`,
                        // the count summed negated. Every partial sum
                        // lies between the two bounds.
                        let nn = sum(self, no, Bin::Add, Some(true));
                        let c = sum(self, co, Bin::Add, None);
                        let nt = self.tmp_k(k);
                        self.push(Inst::Ovf {
                            op: Bin::Sub,
                            k,
                            signed: true,
                            dst: nt,
                            a: V::M(c),
                            b: V::M(nn),
                            fail: restart,
                        });
                        let r2 = self.tmp_k(k);
                        self.push(Inst::Ovf {
                            op,
                            k,
                            signed: true,
                            dst: r2,
                            a: V::M(am),
                            b: V::M(nt),
                            fail: restart,
                        });
                        self.push(Inst::Bin {
                            op,
                            k,
                            signed: false,
                            dst: r,
                            a: V::M(r),
                            b: V::M(nt),
                        });
                    }
                }
            }
            done.push((am, r, k));
        }
        for (am, r, k) in done {
            self.push(Inst::Mov {
                k,
                dst: am,
                src: V::M(r),
            });
        }
        if p.skip {
            // Done with the loop, or (a strip's) past its bound.
            let fin = self.cmp(Cc::Ge, wk, false, V::M(ivm), bound);
            let exit = self.label();
            self.push(Inst::Br {
                c: fin,
                t: exit,
                f: inner,
            });
            self.push(Inst::Label(exit));
            if let Some(m) = self.local_mem(p.cond.0) {
                self.push(Inst::Mov {
                    k: K::I8,
                    dst: m,
                    src: V::I(i64::from(p.cond.1)),
                });
            }
            self.edge(p.exit.0.0, &p.exit.1);
        } else {
            self.push(Inst::Jmp(inner));
        }
        if rerun {
            self.push(Inst::Label(restart));
            self.push(Inst::Mov {
                k: wk,
                dst: ivm,
                src: V::M(iv0),
            });
            self.push(Inst::Jmp(inner));
        }
    }
}
