//! TLS, safepoints, GC objects, write barriers and the shadow-stack frame.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

impl FnLower<'_, '_> {
    /// Slot `slot` of this thread's TLS block `nabi_tls`: the block's address is
    /// taken once, in the prologue.
    pub(crate) fn tls(&mut self, slot: tls_abi::Slot) -> Mem {
        let out = &mut self.out;
        let base = *self.tls.get_or_insert_with(|| out.slot(8, 8));
        let off = slot.offset(self.pw() / 8);
        Mem::Ind(base, i32::try_from(off).expect("TLS offset"))
    }

    /// The `nabi_tls` address a nabi body takes last: this function's
    /// (filled in the prologue), else (a generated function) read here.
    pub(crate) fn tls_val(&mut self) -> Val {
        let base = if let Some(s) = self.tls {
            s
        } else {
            let s = self.out.slot(8, 8);
            self.tls = Some(s);
            if !self.lowering {
                let at = self.tls_addr(Mem::Frame(s, 0));
                self.push(at);
            }
            s
        };
        Val::S(V::M(Mem::Frame(base, 0)), self.word())
    }

    /// What puts this thread's `nabi_tls` address in `dst`: the
    /// thread-local's, a plugin's from the host (`nabi_tls_get`).
    pub(crate) fn tls_addr(&mut self, dst: Mem) -> Inst {
        // Android: bionic lets no dlopened library read TLS initial-exec,
        // so the runtime's accessor (a TLS descriptor, relaxed in an
        // executable) finds the block.
        let get = if self.c.plugin == nabic_mir::plugin::Mode::Plugin {
            nabir_abi::module::entry::TLS_GET
        } else if self.c.t.os == "android" {
            "nabi_tls_addr"
        } else {
            let sym = self.c.sym("nabi_tls");
            return Inst::Tls { dst, sym };
        };
        let k = self.word();
        self.call_rt(get, &[], Some((dst, k)));
        self.out.insts.pop().expect("the call")
    }

    pub(crate) fn call_rt(&mut self, name: &str, args: &[(V, K)], ret: Option<(Mem, K)>) {
        let sym = self.c.sym(name);
        let shapes: Vec<Shape> = args
            .iter()
            .map(|(_, k)| Shape {
                leaves: vec![cc::Leaf {
                    off: 0,
                    k: *k,
                    signed: false,
                }],
                array: false,
                align: k.size(),
                byval: false,
                aggregate: false,
                block: false,
                vector: false,
            })
            .collect();
        let rs = ret.map(|(_, k)| Shape {
            leaves: vec![cc::Leaf {
                off: 0,
                k,
                signed: false,
            }],
            array: false,
            align: k.size(),
            byval: false,
            aggregate: false,
            block: false,
            vector: false,
        });
        let locs = cc::assign(self.c.conv, &shapes, rs.as_ref(), false, None);
        let args = locs
            .params
            .iter()
            .zip(args)
            .map(|(p, (v, _))| (p[0], *v))
            .collect();
        let rets = match (ret, locs.ret) {
            (Some((m, _)), Some(p)) => vec![(p[0], m)],
            _ => Vec::new(),
        };
        self.push(Inst::Call {
            callee: Callee::Sym(sym),
            args,
            rets,
            sret: None,
            variadic: false,
            pop: locs.pop,
        });
    }

    /// Safepoint poll: `nabi_runtime_yield` when the thread's yield flag is set.
    pub(crate) fn poll(&mut self) {
        let flag = self.tls(tls_abi::YIELD_FLAG);
        let v = self.tmp_k(K::I8);
        self.push(Inst::Mov {
            k: K::I8,
            dst: v,
            src: V::M(flag),
        });
        let (y, run) = (self.label(), self.label());
        self.push(Inst::Br {
            c: V::M(v),
            t: y,
            f: run,
        });
        self.push(Inst::Label(y));
        self.out.cold.push(y);
        self.call_rt("nabi_runtime_yield", &[], None);
        self.push(Inst::Jmp(run));
        self.push(Inst::Label(run));
    }

    /// Into (`enter`) or out of a C call.
    pub(crate) fn native_edge(&mut self, enter: bool) {
        let (state, slow) = if enter {
            (2, "nabi_enter_native")
        } else {
            (0, "nabi_leave_native")
        };
        let st = self.tls(tls_abi::MUTATOR_STATE);
        let fl = self.tls(tls_abi::YIELD_FLAG);
        self.push(Inst::AtomicStore {
            k: K::I8,
            ptr: st,
            src: V::I(state),
            // A collector stores the flag, then reads the state: both
            // sides seq_cst, or each may miss the other's store.
            ord: Order::SeqCst,
        });
        let v = self.tmp_k(K::I8);
        self.push(Inst::AtomicLoad {
            k: K::I8,
            dst: v,
            ptr: fl,
            ord: Order::SeqCst,
        });
        let (s, fast) = (self.label(), self.label());
        self.push(Inst::Br {
            c: V::M(v),
            t: s,
            f: fast,
        });
        self.push(Inst::Label(s));
        self.out.cold.push(s);
        self.call_rt(slow, &[], None);
        self.push(Inst::Jmp(fast));
        self.push(Inst::Label(fast));
    }
}

impl FnLower<'_, '_> {
    /// A zeroed GC object with a `ty` payload; its address in a word temp.
    pub(crate) fn gc_object(&mut self, ty: &MirTy) -> Mem {
        let w = self.word();
        let tid = self.gc_tid(ty, false);
        let (size, align) = gc_object_size(ty, self.c.t);
        let obj = self.word_tmp();
        let args = [
            (V::I(i64::from(size)), w),
            (V::I(i64::from(align)), w),
            (tid, K::I32),
        ];
        if size >= LOS_THRESHOLD || align > 8 {
            self.call_rt("nabi_gc_alloc_slow", &args, Some((obj, w)));
            return obj;
        }
        let cp = self.tls(tls_abi::TLAB_CURSOR);
        let lp = self.tls(tls_abi::TLAB_LIMIT);
        let [cur, lim, room] = [(); 3].map(|()| self.word_tmp());
        self.push(Inst::Mov {
            k: w,
            dst: cur,
            src: V::M(cp),
        });
        self.push(Inst::Mov {
            k: w,
            dst: lim,
            src: V::M(lp),
        });
        self.push(Inst::Bin {
            op: Bin::Sub,
            k: w,
            signed: false,
            dst: room,
            a: V::M(lim),
            b: V::M(cur),
        });
        let fits = self.cmp(Cc::Le, w, false, V::I(i64::from(size)), V::M(room));
        let (fast, slow, done) = (self.label(), self.label(), self.label());
        self.push(Inst::Br {
            c: fits,
            t: fast,
            f: slow,
        });
        self.push(Inst::Label(fast));
        self.push(Inst::Bin {
            op: Bin::Add,
            k: w,
            signed: false,
            dst: cp,
            a: V::M(cur),
            b: V::I(i64::from(size)),
        });
        let Mem::Frame(cs, _) = cur else {
            unreachable!()
        };
        self.push(Inst::Mov {
            k: K::I32,
            dst: Mem::Ind(cs, 0),
            src: tid,
        });
        self.push(Inst::Mov {
            k: w,
            dst: Mem::Ind(cs, GC_WORD_OFFSET as i32),
            src: V::I(i64::from(size)),
        });
        self.push(Inst::Mov {
            k: w,
            dst: obj,
            src: V::M(cur),
        });
        self.push(Inst::Jmp(done));
        self.push(Inst::Label(slow));
        self.out.cold.push(slow);
        self.call_rt("nabi_gc_alloc_slow", &args, Some((obj, w)));
        self.push(Inst::Jmp(done));
        self.push(Inst::Label(done));
        obj
    }

    /// Write barrier before storing a reference into `obj` at address `slot`
    /// (both word slots), `len` bytes. `at`: what is known of the offset
    /// in the object (`Card`).
    pub(crate) fn write_barrier(&mut self, obj: Slot, slot: Slot, len: u32, at: Card) {
        let w = self.word();
        let flags = self.tmp_k(K::I32);
        self.push(Inst::Mov {
            k: K::I32,
            dst: flags,
            src: V::M(Mem::Ind(obj, GC_FLAGS_OFFSET as i32)),
        });
        let masked = self.tmp_k(K::I32);
        let bit = |s: &mut Self, mask: u32| {
            // Above -O0 each test its own: a test of a flag then.
            let masked = if s.c.opt { s.tmp_k(K::I32) } else { masked };
            s.push(Inst::Bin {
                op: Bin::And,
                k: K::I32,
                signed: false,
                dst: masked,
                a: V::M(flags),
                b: V::I(i64::from(mask)),
            });
            s.cmp(Cc::Ne, K::I32, false, V::M(masked), V::I(0))
        };
        let set = bit(self, FLAG_UNLOGGED | FLAG_CARDED);
        let [wb, slow, cq, card, ok] = [(); 5].map(|()| self.label());
        self.push(Inst::Br {
            c: set,
            t: wb,
            f: ok,
        });
        self.push(Inst::Label(wb));
        let unlogged = bit(self, FLAG_UNLOGGED);
        self.push(Inst::Br {
            c: unlogged,
            t: slow,
            f: cq,
        });
        self.push(Inst::Label(slow));
        self.out.cold.push(slow);
        self.call_rt(
            "nabi_gc_wb_slow",
            &[
                (V::M(Mem::Frame(obj, 0)), w),
                (V::M(Mem::Frame(slot, 0)), w),
                (V::I(i64::from(len)), w),
            ],
            None,
        );
        self.push(Inst::Jmp(cq));
        self.push(Inst::Label(cq));
        let carded = bit(self, FLAG_CARDED);
        self.push(Inst::Br {
            c: carded,
            t: card,
            f: ok,
        });
        self.push(Inst::Label(card));
        let wb_ = (self.pw() / 8) as i32;
        let one = self.c.opt && Self::one_card(at, len);
        if one {
            let ct = self.word_tmp();
            self.push(Inst::Mov {
                k: w,
                dst: ct,
                src: V::M(Mem::Ind(obj, -wb_)),
            });
            let Mem::Frame(cs, _) = ct else {
                unreachable!()
            };
            let dst = if let Card::At(off) = at {
                // The card is known: its byte past the table.
                Mem::Ind(cs, (off >> CARD_SHIFT) as i32)
            } else {
                let [off, i0, c0] = [(); 3].map(|()| self.word_tmp());
                for (op, dst, a, b) in [
                    (
                        Bin::Sub,
                        off,
                        V::M(Mem::Frame(slot, 0)),
                        V::M(Mem::Frame(obj, 0)),
                    ),
                    (Bin::Shr, i0, V::M(off), V::I(i64::from(CARD_SHIFT))),
                    (Bin::Add, c0, V::M(ct), V::M(i0)),
                ] {
                    self.push(Inst::Bin {
                        op,
                        k: w,
                        signed: false,
                        dst,
                        a,
                        b,
                    });
                }
                let Mem::Frame(s, _) = c0 else { unreachable!() };
                Mem::Ind(s, 0)
            };
            self.push(Inst::Mov {
                k: K::I8,
                dst,
                src: V::I(1),
            });
            self.push(Inst::Jmp(ok));
            self.push(Inst::Label(ok));
            return;
        }
        let [ct, off, i0, e1, i1, c0, c1] = [(); 7].map(|()| self.word_tmp());
        self.push(Inst::Mov {
            k: w,
            dst: ct,
            src: V::M(Mem::Ind(obj, -wb_)),
        });
        let bin = |s: &mut Self, op, dst, a, b| {
            s.push(Inst::Bin {
                op,
                k: w,
                signed: false,
                dst,
                a,
                b,
            });
        };
        bin(
            self,
            Bin::Sub,
            off,
            V::M(Mem::Frame(slot, 0)),
            V::M(Mem::Frame(obj, 0)),
        );
        bin(self, Bin::Shr, i0, V::M(off), V::I(i64::from(CARD_SHIFT)));
        bin(
            self,
            Bin::Add,
            e1,
            V::M(off),
            V::I(i64::from(len.max(1) - 1)),
        );
        bin(self, Bin::Shr, i1, V::M(e1), V::I(i64::from(CARD_SHIFT)));
        bin(self, Bin::Add, c0, V::M(ct), V::M(i0));
        bin(self, Bin::Add, c1, V::M(ct), V::M(i1));
        for c in [c0, c1] {
            let Mem::Frame(s, _) = c else { unreachable!() };
            self.push(Inst::Mov {
                k: K::I8,
                dst: Mem::Ind(s, 0),
                src: V::I(1),
            });
        }
        // A value wider than a card spans at least (len - 1) / card + 1
        // cards: the ones between the first and the last are c0 + 1 ..=
        // c0 + (len - 1) / card, never past c1.
        let Mem::Frame(s0, _) = c0 else {
            unreachable!()
        };
        for k in 1..=(len.max(1) - 1) >> CARD_SHIFT {
            self.push(Inst::Mov {
                k: K::I8,
                dst: Mem::Ind(s0, k as i32),
                src: V::I(1),
            });
        }
        self.push(Inst::Jmp(ok));
        self.push(Inst::Label(ok));
    }

    /// Whether the `len` bytes stored at `at` lie in one card.
    pub(crate) fn one_card(at: Card, len: u32) -> bool {
        let card = 1u32 << CARD_SHIFT;
        match at {
            Card::At(off) => off >> CARD_SHIFT == (off + len.max(1) - 1) >> CARD_SHIFT,
            // Elements of a power-of-two size at a multiple of it.
            Card::Elem(size) => {
                size.is_power_of_two() && size <= card && HEADER_SIZE % size == 0 && len <= size
            }
        }
    }

    /// Before touching a lazily initialized static: `@sym.init()` unless
    /// its state byte reads 2 (ready).
    pub(crate) fn ensure_static_init(&mut self, name: &str) {
        if !self.c.pkgs.lazy_statics.contains_key(name) {
            return;
        }
        let sym = self.c.pkgs.static_sym(name);
        let local = self.c.local_statics.contains(name);
        let ps = self.data_addr(&format!("{sym}.state"), local, false);
        let s = self.tmp_k(K::I8);
        self.push(Inst::AtomicLoad {
            k: K::I8,
            dst: s,
            ptr: Mem::Ind(ps, 0),
            // Pairs with the initializer's store of 2.
            ord: self.order(Order::Acquire),
        });
        let ready = self.cmp(Cc::Eq, K::I8, false, V::M(s), V::I(2));
        let (r, init) = (self.label(), self.label());
        self.push(Inst::Br {
            c: ready,
            t: r,
            f: init,
        });
        self.push(Inst::Label(init));
        // The location of this read: a recursive initialization's panic.
        let str_ty = MirTy::Prim(PrimTy::Str);
        let at = self.tmp(&str_ty);
        let loc = self.c.sources.location(self.at);
        self.str_lit(&loc, at);
        let f = self.c.sym(&format!("{sym}.init"));
        let at = self.val_of(at, &str_ty);
        self.call_sym(f, &[str_ty], &MirTy::Unit, &[at], None);
        self.push(Inst::Jmp(r));
        self.push(Inst::Label(r));
    }

    /// GC type id of `ty` objects (`array`: buffers): a constant, or in a
    /// unit read from the glue's symbol (`units::gc_symbol`).
    pub(crate) fn gc_tid(&mut self, ty: &MirTy, array: bool) -> V {
        let i = self.c.gc_type(ty, array);
        if self.c.plugin == nabic_mir::plugin::Mode::Plugin {
            return self.plugin_gc_tid(i);
        }
        let Some(pkg) = self.c.gc_pkg else {
            return V::I(i64::from(self.c.gc_type_base + i as u32));
        };
        let name = nabic_codegen::units::gc_symbol(pkg, &self.c.gc_types[i]);
        let s = self.c.sym(&name);
        // The glue is in the same executable, as a unit's statics are.
        let p = self.word_tmp();
        self.push(Inst::Sym {
            dst: p,
            sym: s,
            got: false,
        });
        let Mem::Frame(ps, _) = p else { unreachable!() };
        let tid = self.tmp_k(K::I32);
        self.push(Inst::Mov {
            k: K::I32,
            dst: tid,
            src: V::M(Mem::Ind(ps, 0)),
        });
        V::M(tid)
    }

    /// The address of static `name` in a word temp.
    pub(crate) fn static_addr(&mut self, name: &str) -> Slot {
        let sym = self.c.pkgs.static_sym(name);
        if self.c.pkgs.tls_statics.contains(name) {
            let s = self.c.sym(&sym);
            let p = self.word_tmp();
            self.push(Inst::Tls { dst: p, sym: s });
            let Mem::Frame(ps, _) = p else { unreachable!() };
            return ps;
        }
        let local = self.c.local_statics.contains(name);
        let dll = self.c.pkgs.dll_statics.contains(name);
        self.data_addr(&sym, local, dll)
    }

    /// The address of data symbol `sym` in a word temp: direct when this
    /// object defines it (`local`), else through the GOT. A DLL's
    /// (`dll`), and on Windows a plugin's import, is the address at its
    /// import address `__imp_<sym>`.
    pub(crate) fn data_addr(&mut self, sym: &str, local: bool, dll: bool) -> Slot {
        let plugin = self.c.plugin == nabic_mir::plugin::Mode::Plugin;
        let dll = dll || (plugin && self.c.obj.coff && !local);
        let s = self.c.sym(&if dll {
            format!("__imp_{sym}")
        } else {
            sym.to_owned()
        });
        let p = self.word_tmp();
        self.push(Inst::Sym {
            dst: p,
            sym: s,
            got: !local,
        });
        let Mem::Frame(ps, _) = p else { unreachable!() };
        if !dll {
            return ps;
        }
        let q = self.word_tmp();
        let k = self.word();
        self.push(Inst::Mov {
            k,
            dst: q,
            src: V::M(Mem::Ind(ps, 0)),
        });
        let Mem::Frame(qs, _) = q else { unreachable!() };
        qs
    }

    /// Whether a collection can run while this function is active.
    pub(crate) fn reaches_safepoint(&self) -> bool {
        nabic_codegen::analysis::body_safepoints(self.f, self.c.quiet, self.c.pkgs, self.c.t)
    }

    /// The shadow-stack frame, when the function needs one.
    pub(crate) fn gc_frame(&mut self) {
        if self.cfn || self.back_edges.is_empty() && !self.reaches_safepoint() {
            return;
        }
        let (t, quiet, pkgs) = (self.c.t, self.c.quiet, self.c.pkgs);
        // Each local's root words, once.
        let words: Vec<Vec<u32>> = self
            .f
            .locals
            .iter()
            .map(|l| t.root_offsets(&l.ty))
            .collect();
        let live = nabic_mir::live_across(
            self.f,
            |l| !words[l.0 as usize].is_empty(),
            |s| nabic_codegen::analysis::stmt_safepoint(s, quiet, pkgs, t).then_some(true),
            |b| self.back_edges.contains(&b.0),
            true,
        );
        let caller = self.f.params.last().filter(|_| self.f.facts.track_caller);
        let mut roots: Vec<Mem> = Vec::new();
        let mut slots = Vec::new();
        for (local, offsets) in self.f.locals.iter().zip(words) {
            // A read-only array is read where it is (`read_only`); an
            // in-place parameter or result is the caller's.
            if offsets.is_empty()
                || caller == Some(&local.id)
                || !live.contains(&local.id)
                || self.ro.contains_key(&local.id)
                || self.in_place.contains_key(&local.id)
            {
                continue;
            }
            let Some(m) = self.local_mem(local.id) else {
                continue;
            };
            self.rooted.insert(local.id.0);
            // The locals a parameter's parts went to hold them already.
            let arrived = self.fuse.params.values().flatten().any(|x| x.1 == local.id);
            if !self.f.params.contains(&local.id) && !arrived {
                let size = self.c.t.size_align(&local.ty).0;
                self.push(Inst::Zero { dst: m, size });
            }
            roots.extend(offsets.into_iter().map(|o| m.at(o)));
            if let Mem::Frame(s, _) = m {
                slots.push(s);
            }
        }
        // A hot build links the frame of a function holding a hot type
        // even with no roots: the hot runtime finds it there.
        let info = crate::hot::frame_info(self.c, self.f, self.out.sym);
        if roots.is_empty() && info.is_none() {
            return;
        }
        if self.c.hot.is_some() {
            self.c.obj.hot_frames.insert(self.out.sym, info);
        }
        // Its address in a root word too: the hot runtime walks roots.
        if let Some(i) = info {
            let at = self.word_tmp();
            self.push(Inst::Sym {
                dst: at,
                sym: i,
                got: false,
            });
            if let Mem::Frame(s, _) = at {
                slots.push(s);
            }
            roots.push(at);
        }
        let wb = self.pw() / 8;
        let w = self.word();
        let n = roots.len() as u32;
        let frame = self.out.slot(wb * (2 + n.max(1)), wb);
        let top = self.tls(tls_abi::SHADOW_STACK_TOP);
        self.push(Inst::Mov {
            k: w,
            dst: Mem::Frame(frame, 0),
            src: V::M(top),
        });
        self.push(Inst::Mov {
            k: w,
            dst: Mem::Frame(frame, wb),
            src: V::I(i64::from(n)),
        });
        for (i, r) in roots.into_iter().enumerate() {
            self.push(Inst::Addr {
                dst: Mem::Frame(frame, wb * (2 + i as u32)),
                src: r,
            });
        }
        self.push(Inst::Addr {
            dst: top,
            src: Mem::Frame(frame, 0),
        });
        self.gc_frame = Some((frame, top));
        self.out.gc_frame = Some((frame, slots));
        let rooted = self.rooted.clone();
        self.deaths = nabic_mir::deaths(self.f, |l| rooted.contains(&l.0));
        if self.c.opt {
            self.quiet_deaths();
        }
    }

    /// Drop the death clears no collection can follow: from there every
    /// path returns (or ends in a call that never returns) before a
    /// safepoint, so the dead root is never scanned.
    pub(crate) fn quiet_deaths(&mut self) {
        let (t, quiet, pkgs) = (self.c.t, self.c.quiet, self.c.pkgs);
        let blocks = &self.f.blocks;
        let pos: HashMap<u32, usize> = blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id.0, i))
            .collect();
        // Per block, from each statement on: whether it holds a safepoint.
        let own: Vec<Vec<bool>> = blocks
            .iter()
            .map(|b| {
                let skip = nabic_mir::noreturn_call(b);
                let poll = self.back_edges.contains(&b.id.0);
                let mut after = vec![poll; b.stmts.len() + 1];
                for (i, st) in b.stmts.iter().enumerate().rev() {
                    let sp = Some(i) != skip
                        && nabic_codegen::analysis::stmt_safepoint(st, quiet, pkgs, t);
                    after[i] = after[i + 1] || sp;
                }
                after
            })
            .collect();
        let succs: Vec<Vec<usize>> = blocks
            .iter()
            .map(|b| {
                nabic_mir::succs(b)
                    .iter()
                    .filter_map(|s| pos.get(s).copied())
                    .collect()
            })
            .collect();
        let mut reach: Vec<bool> = own.iter().map(|a| a[0]).collect();
        let mut changed = true;
        while changed {
            changed = false;
            for b in (0..blocks.len()).rev() {
                if !reach[b] && succs[b].iter().any(|&s| reach[s]) {
                    reach[b] = true;
                    changed = true;
                }
            }
        }
        for (b, d) in self.deaths.on_entry.iter_mut().enumerate() {
            if !reach[b] {
                d.clear();
            }
        }
        for (b, ds) in self.deaths.after_stmt.iter_mut().enumerate() {
            let later = succs[b].iter().any(|&s| reach[s]);
            for (i, d) in ds.iter_mut().enumerate() {
                if !(later || own[b][i + 1]) {
                    d.clear();
                }
            }
        }
    }

    pub(crate) fn gc_frame_pop(&mut self) {
        if let Some((frame, top)) = self.gc_frame {
            let w = self.word();
            self.push(Inst::Mov {
                k: w,
                dst: top,
                src: V::M(Mem::Frame(frame, 0)),
            });
        }
    }

    pub(crate) fn clear(&mut self, l: LocalId) {
        if let Some(m) = self.local_mem(l) {
            let size = self.c.t.size_align(&self.local_ty(l)).0;
            if size > 0 {
                self.push(Inst::Zero { dst: m, size });
            }
        }
    }
}
