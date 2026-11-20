//! Planning a vector loop: which loops, accesses and their cost.

#[allow(clippy::wildcard_imports)]
use crate::lower::vector::*;

impl FnLower<'_, '_> {
    /// The loops whose headers get a vector loop in front, if they are
    /// vector loops: their chains and the label each back edge takes.
    pub(crate) fn vector_loops(&mut self) -> Loops {
        let mut out = Loops::default();
        // For size, short bodies only: their vector loop is about as long
        // as the scalar one.
        let size = matches!(self.c.level, crate::OptLevel::Os | crate::OptLevel::Oz);
        let cap = if size { SIZE_BODY } else { usize::MAX };
        if !self.c.opt || Isa::of(self.c.t).is_none() {
            return out;
        }
        let f = self.f;
        let at: HashMap<u32, usize> = f
            .blocks
            .iter()
            .enumerate()
            .map(|(i, b)| (b.id.0, i))
            .collect();
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); f.blocks.len()];
        for (i, b) in f.blocks.iter().enumerate() {
            for s in nabic_mir::succs(b) {
                if let Some(&j) = at.get(&s) {
                    preds[j].push(i);
                }
            }
        }
        for h in 0..f.blocks.len() {
            let Some(ch) = body(f, h, &at, &preds) else {
                continue;
            };
            let n: usize = ch
                .order
                .iter()
                .map(|&b| f.blocks[b].stmts.iter().filter(|s| !marker(s)).count())
                .sum();
            if n > cap {
                continue;
            }
            let latch = f.blocks[*ch.order.last().expect("a block")].id.0;
            // A polled loop may run long; one clearing roots on entry
            // would skip that on the way back.
            if self.back_edges.contains(&latch)
                || self.deaths.on_entry.get(h).is_some_and(|d| !d.is_empty())
            {
                continue;
            }
            let inner = self.label();
            out.latch.insert(latch, f.blocks[h].id.0);
            out.chains.insert(f.blocks[h].id.0, (ch, inner));
        }
        out
    }

    /// The vector loop of loop body `ch`, if it is one.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn plan(&mut self, ch: &Body, isa: Isa) -> Option<Plan> {
        let f = self.f;
        let (iv, cond) = (ch.iv, ch.cond);
        let h = ch.order[0];
        let l = *ch.order.last()?;
        // The loop clears no root.
        for &bi in &ch.order {
            let entry = bi != h && self.deaths.on_entry.get(bi).is_some_and(|d| !d.is_empty());
            let after = self
                .deaths
                .after_stmt
                .get(bi)
                .is_some_and(|d| d.iter().any(|x| !x.is_empty()));
            if entry || after {
                return None;
            }
        }
        if self.local_ty(iv) != MirTy::Prim(PrimTy::Usize) {
            return None;
        }
        let inside: HashSet<u32> = ch.order.iter().map(|&b| f.blocks[b].id.0).collect();
        let mut items: Vec<Item<'_>> = Vec::new();
        for &bi in &ch.order {
            let b = &f.blocks[bi];
            items.push(Item::Start(bi));
            let end = if bi == l { ch.step } else { b.stmts.len() };
            for s in &b.stmts[..end] {
                if !matches!(s, Statement::Loc(..)) {
                    items.push(Item::S(s));
                }
            }
            if bi != l {
                items.push(Item::End(bi));
            }
        }
        let all: Vec<&Statement> = ch
            .order
            .iter()
            .flat_map(|&bi| f.blocks[bi].stmts.iter())
            .collect();
        // Written here: everything else is the same in every iteration.
        let mut written: HashSet<LocalId> = HashSet::default();
        for &bi in &ch.order {
            written.extend(f.blocks[bi].params.iter().copied());
        }
        for s in &all {
            match s {
                Statement::Assign { target, .. } => {
                    written.insert(*target);
                }
                Statement::Loc(..) | Statement::BufStore { .. } => {}
                _ => return None,
            }
        }
        if matches!(&ch.bound, Operand::Copy(x) if written.contains(x)) {
            return None;
        }
        let mut assigns: HashMap<LocalId, u32> = HashMap::default();
        let mut reads: HashMap<LocalId, u32> = HashMap::default();
        for s in &all {
            if let Statement::Assign { target, .. } = s {
                *assigns.entry(*target).or_default() += 1;
            }
            nabic_mir::stmt_reads(s, &mut |x| *reads.entry(x).or_default() += 1);
        }
        for &bi in &ch.order {
            nabic_mir::term_reads(&f.blocks[bi].terminator, &mut |x| {
                *reads.entry(x).or_default() += 1;
            });
        }
        let once = |x: &LocalId| assigns.get(x) == Some(&1);
        // Read before written in an iteration: carried from the last one.
        let mut fresh: HashSet<LocalId> = HashSet::default();
        let mut carried: HashSet<LocalId> = HashSet::default();
        let mut forms = Forms {
            iv,
            idx: HashMap::default(),
            alias: HashMap::default(),
            guard: false,
            units: Vec::new(),
            pw: self.c.t.pointer_width,
        };
        for it in &items {
            let mut read = |x: LocalId| {
                if written.contains(&x) && !fresh.contains(&x) {
                    carried.insert(x);
                }
            };
            match it {
                Item::Start(bi) => fresh.extend(f.blocks[*bi].params.iter().copied()),
                Item::End(bi) => nabic_mir::term_reads(&f.blocks[*bi].terminator, &mut read),
                Item::S(s) => {
                    nabic_mir::stmt_reads(s, &mut read);
                    if let Statement::Assign { target, rvalue } = s {
                        fresh.insert(*target);
                        forms.see(*target, rvalue, &written);
                    }
                }
            }
        }
        carried.remove(&iv);
        if carried.iter().any(|x| !fresh.contains(x))
            || forms.idx.keys().chain(forms.alias.keys()).any(|x| !once(x))
            || !once(&iv)
            || !once(&cond.0)
        {
            return None;
        }
        // A carried local is a reduction: read only in its updates `a =
        // a op x`, or a min / max.
        let mut mm: HashMap<LocalId, (bool, bool)> = HashMap::default();
        for a in &carried {
            let ext = |rv: &RValue| self.float_extreme(rv);
            if let Some(k) = minmax(f, &ch.order, *a, &reads, &assigns, &ext) {
                mm.insert(*a, k);
                continue;
            }
            let me = Operand::Copy(*a);
            let ups = all.iter().all(|s| match s {
                Statement::Assign { target, rvalue } if target == a => matches!(
                    rvalue,
                    RValue::BinOp { lhs, rhs, .. } if (*lhs == me) != (*rhs == me)
                ),
                _ => true,
            });
            if !ups || reads.get(a) != assigns.get(a) {
                return None;
            }
        }
        let reductions: HashSet<LocalId> = carried
            .iter()
            .filter(|a| !mm.contains_key(a))
            .copied()
            .collect();
        // Side exits: `(block, test or none, leaves on true)`.
        let mut exits: Vec<(usize, Option<LocalId>, bool)> = Vec::new();
        for &bi in &ch.order {
            if bi == l {
                continue;
            }
            let out = |x: &BlockId| !inside.contains(&x.0);
            match &f.blocks[bi].terminator {
                Terminator::Goto { target, .. } if out(target) => exits.push((bi, None, true)),
                Terminator::Goto { .. } => {}
                Terminator::If {
                    cond: Operand::Copy(c),
                    then_bb,
                    else_bb,
                    ..
                } => match (out(then_bb), out(else_bb)) {
                    (true, true) => return None,
                    (true, false) => exits.push((bi, Some(*c), true)),
                    (false, true) => exits.push((bi, Some(*c), false)),
                    (false, false) => {}
                },
                _ => return None,
            }
        }
        let first_exit = exits
            .iter()
            .filter_map(|e| ch.order.iter().position(|b| *b == e.0))
            .min();
        // A field of an object the loop never writes (a list's buffer
        // or length): loaded once in front, unless a store may write it
        // (a byte buffer may view any object: then the two must differ).
        // Past a side exit only of an object read before it.
        let mut hoist: Vec<(LocalId, RValue)> = Vec::new();
        let mut erased_stores: Vec<LocalId> = Vec::new();
        let mut deref: HashSet<LocalId> = HashSet::default();
        let mut exited = false;
        for it in &items {
            let s = match it {
                Item::S(s) => *s,
                Item::Start(bi) => {
                    exited |=
                        first_exit.is_some_and(|e| ch.order.iter().position(|b| b == bi) > Some(e));
                    continue;
                }
                Item::End(_) => continue,
            };
            match s {
                Statement::Assign {
                    target,
                    rvalue:
                        rv @ RValue::HeapField {
                            ptr: Operand::Copy(p),
                            ..
                        },
                } if once(target)
                    && !carried.contains(target)
                    && (!written.contains(p) || hoist.iter().any(|h| h.0 == *p))
                    && (!exited || deref.contains(p)) =>
                {
                    hoist.push((*target, rv.clone()));
                }
                Statement::BufStore {
                    buf: Operand::Copy(b),
                    elem_ty: MirTy::Prim(PrimTy::U8 | PrimTy::I8),
                    ..
                } => erased_stores.push(*b),
                _ => {}
            }
            if !exited {
                if let Statement::Assign { rvalue, .. } = s {
                    if let Some(x) = load_base(rvalue) {
                        deref.insert(x);
                    }
                }
            }
        }
        let mut distinct: Vec<(LocalId, LocalId)> = Vec::new();
        for (_, rv) in &hoist {
            if let RValue::HeapField {
                ptr: Operand::Copy(p),
                ..
            } = rv
            {
                for b in &erased_stores {
                    if b == p {
                        return None;
                    }
                    distinct.push((*b, *p));
                }
            }
        }
        for (t, _) in &hoist {
            written.remove(t);
        }
        // Side exits testing the counter against what the loop never
        // changes: the vector loop stops short of where one would leave.
        let invariant = |o: &Operand| match o {
            Operand::Copy(l) => !written.contains(l) && *l != iv,
            Operand::Const(_) => true,
        };
        let defs: HashMap<LocalId, &RValue> = all
            .iter()
            .filter_map(|s| match s {
                Statement::Assign { target, rvalue } if once(target) => Some((*target, rvalue)),
                _ => None,
            })
            .collect();
        let mut limits: Vec<(Operand, Idx)> = Vec::new();
        let mut consumed: HashSet<LocalId> = HashSet::default();
        for &(_, c, on_true) in &exits {
            let Some(c) = c else { continue };
            if reads.get(&c) != Some(&1) {
                continue;
            }
            let Some(RValue::BinOp {
                op,
                op_ty: MirTy::Prim(PrimTy::Usize),
                lhs,
                rhs,
            }) = defs.get(&c)
            else {
                continue;
            };
            let idx = |o: &Operand| forms.idx(o);
            // Leaves when `idx >= l`.
            let hit = match (op, on_true) {
                (PrimOp::Ge, true) | (PrimOp::Lt, false) => idx(lhs).zip(Some(rhs)),
                (PrimOp::Le, true) | (PrimOp::Gt, false) => idx(rhs).zip(Some(lhs)),
                _ => None,
            };
            if let Some((off, x)) = hit.filter(|(_, x)| invariant(x)) {
                limits.push((x.clone(), off));
                consumed.insert(c);
            }
        }
        let mut skipped: HashSet<LocalId> = forms.idx.keys().copied().collect();
        skipped.extend(forms.alias.keys().copied());
        skipped.extend(hoist.iter().map(|h| h.0));
        skipped.extend(consumed.iter().copied());
        let body: Vec<&Statement> = items
            .iter()
            .filter_map(|it| match it {
                Item::S(s) => Some(*s),
                _ => None,
            })
            .collect();
        let w = lane_width(self, &body, &skipped, &carried)?;
        // Products of bytes summed: lanes of bytes, `sdot` / `udot`.
        let dot = w == 4
            && isa == Isa::A64
            && self.c.t.cpu_features().has("dotprod")
            && dot_shape(&body, &skipped, &reductions);
        let w = if dot { 1 } else { w };
        let checks = self.c.overflow_checks;
        // Elements every lane may read: those an access in a block every
        // iteration runs reads, and those a bounds check the vector loop
        // keeps to covers (a length and a buffer of one object).
        let mut same: HashMap<LocalId, LocalId> = HashMap::default();
        for (i, (t, rv)) in hoist.iter().enumerate() {
            if let Some((u, _)) = hoist[..i].iter().find(|(_, r)| r == rv) {
                let u = same.get(u).copied().unwrap_or(*u);
                same.insert(*t, u);
            }
        }
        let mut safe: Vec<(LocalId, Option<LocalId>, u64)> = Vec::new();
        let mut blk = h;
        for it in &items {
            let (buf, index) = match it {
                Item::Start(bi) => {
                    blk = *bi;
                    continue;
                }
                Item::S(
                    Statement::BufStore { buf, index, .. }
                    | Statement::Assign {
                        rvalue: RValue::BufLoad { buf, index, .. },
                        ..
                    },
                ) => (buf, index),
                _ => continue,
            };
            if let (Operand::Copy(b), Some(x), true) =
                (buf, forms.idx(index), ch.full.contains(&blk))
            {
                safe.push((same.get(b).copied().unwrap_or(*b), x.inv, x.c));
            }
        }
        let field = |l: LocalId| {
            hoist.iter().find_map(|(t, rv)| match rv {
                RValue::HeapField {
                    ptr: Operand::Copy(o),
                    index,
                    ..
                } if *t == l => Some((*o, *index)),
                _ => None,
            })
        };
        for (l, x) in &limits {
            let Some((o, n)) = (match l {
                Operand::Copy(l) => field(*l),
                Operand::Const(_) => None,
            }) else {
                continue;
            };
            for (t, _) in &hoist {
                if field(*t).is_some_and(|(o2, n2)| o2 == o && n2 != n)
                    && self.local_ty(*t) == MirTy::GcRef
                {
                    safe.push((same.get(t).copied().unwrap_or(*t), x.inv, x.c));
                }
            }
        }
        // A view's length bounds its buffer from its start on.
        if limits
            .iter()
            .any(|(l, x)| x.inv.is_none() && matches!(l, Operand::Copy(_)))
        {
            let views = self.views(&written);
            for (l, x) in &limits {
                let (Operand::Copy(l), None) = (l, x.inv) else {
                    continue;
                };
                let Some(&(v, 2)) = views.get(l) else {
                    continue;
                };
                let field = |n: u32| views.iter().filter(move |(_, d)| **d == (v, n));
                for ((b, _), (o, _)) in field(0).flat_map(|b| field(1).map(move |o| (b, o))) {
                    if self.local_ty(*b) == MirTy::GcRef {
                        safe.push((*b, Some(*o), x.c));
                    }
                }
            }
        }
        let mut succ: HashMap<usize, Vec<usize>> = HashMap::default();
        for &bi in &ch.order {
            if bi == l {
                continue;
            }
            let to: Vec<usize> = nabic_mir::succs(&f.blocks[bi])
                .into_iter()
                .filter_map(|s| ch.order.iter().copied().find(|&b| f.blocks[b].id.0 == s))
                .collect();
            succ.insert(bi, to);
        }
        let mut bl = Builder {
            lw: self,
            isa,
            w,
            checks,
            next: 0,
            pre: Vec::new(),
            body: Vec::new(),
            tail: Vec::new(),
            vals: HashMap::default(),
            inv: HashMap::default(),
            consts: HashMap::default(),
            splats: Vec::new(),
            bases: Vec::new(),
            accesses: Vec::new(),
            ivoff: forms.idx.clone(),
            iv,
            written: written.clone(),
            stored: false,
            pinned: Vec::new(),
            accs: Vec::new(),
            pred: None,
            cur: h,
            inbox: HashMap::default(),
            defs: HashMap::default(),
            succ,
            top: h,
            started: Vec::new(),
            counts: HashSet::default(),
            ups: Vec::new(),
            minmax: Vec::new(),
            fmm: HashSet::default(),
            dot,
            dotcast: HashMap::default(),
            dotmul: HashMap::default(),
            gts: HashMap::default(),
            flts: HashMap::default(),
            pending: Vec::new(),
            safe,
            same,
            widened: HashMap::default(),
            narrow: HashMap::default(),
        };
        let mut nan_free = Vec::new();
        for (&m, &(max, signed)) in &mm {
            // Each lane starts from the accumulator's value.
            let ty = bl.lw.local_ty(m);
            let MirTy::Prim(p) = ty else { return None };
            if !(p.is_int() || p.is_float()) || bl.width(p) != Some(w) {
                return None;
            }
            if p.is_float() {
                // A NaN to start from gives way to the first element
                // (or, signaling, eats it): the scalar loop takes it.
                bl.fmm.insert(m);
                nan_free.push((m, if w == 8 { K::F64 } else { K::F32 }));
            }
            let dst = bl.reg().ok()?;
            let off = bl.splats.len() as u32;
            bl.splats.push((Operand::Copy(m), ty));
            bl.pre.push(VOp::Splat { dst, off });
            bl.pinned.push(dst);
            bl.vals.insert(m, (dst, Form::Num));
            bl.minmax.push((m, max, signed, dst));
        }
        for it in &items {
            match *it {
                Item::S(s) => {
                    if let Statement::Assign { target, .. } = s {
                        if skipped.contains(target) {
                            continue;
                        }
                    }
                    bl.stmt(s, &reductions).ok()?;
                }
                Item::Start(bi) => bl.start(bi, ch.full.contains(&bi)).ok()?,
                Item::End(bi) => bl.end(bi, &inside, &consumed).ok()?,
            }
        }
        bl.finish().ok()?;
        if bl.accesses.is_empty() {
            // The counter indexes nothing: it may be near its top.
            return None;
        }
        // A checked sum may run the range again: nothing is stored.
        let rerun = bl.accs.iter().any(|a| {
            matches!(
                a.red,
                Red::Checked { .. }
                    | Red::Count { checked: true, .. }
                    | Red::Dot { checked: true, .. }
            )
        });
        if rerun && bl.stored {
            return None;
        }
        let lanes = u64::from(isa.bytes() / w);
        let (pairs, apart) = order(&bl.accesses, lanes)?;
        for pair in pairs {
            if !distinct.contains(&pair) {
                distinct.push(pair);
            }
        }
        let Builder {
            mut pre,
            mut body,
            tail,
            pinned,
            mut accs,
            splats,
            bases,
            ..
        } = bl;
        body.extend(tail);
        let regs = allocate(&mut pre, &mut body, &pinned, &mut accs, isa.regs())?;
        // Worth it: a vector costs less than its elements would.
        let scalar = all
            .iter()
            .filter(|s| !matches!(s, Statement::Loc(..)))
            .map(|s| match s {
                Statement::Assign {
                    rvalue:
                        RValue::BinOp {
                            op: PrimOp::Add | PrimOp::Sub | PrimOp::Mul,
                            op_ty: MirTy::Prim(p),
                            ..
                        },
                    ..
                } if checks && (p.is_signed_int() || p.is_unsigned_int()) => 2,
                // A call, and the runtime's tests of NaNs.
                Statement::Assign {
                    rvalue: rv @ RValue::Call { .. },
                    ..
                } if self.float_extreme(rv).is_some() => 10,
                _ => 1,
            })
            .sum::<u64>()
            + ch.order.len() as u64;
        if 5 * cost(isa, w, &body) >= 4 * scalar * lanes {
            return None;
        }
        // io: counter, bound, bases, splats, then the results 16-aligned.
        let mut at = VIO_BASES + 8 * bases.len() as u32;
        let splats: Vec<(Operand, MirTy, u32)> = splats
            .into_iter()
            .map(|(o, t)| {
                let off = at;
                at += 8;
                (o, t, off)
            })
            .collect();
        for op in &mut pre {
            if let VOp::Splat { off, .. } = op {
                *off = splats[*off as usize].2;
            }
        }
        at = at.next_multiple_of(16);
        let mut post = Vec::new();
        for a in &accs {
            for r in &a.regs {
                post.push((*r, at));
                at += isa.bytes();
            }
        }
        let save = at;
        if matches!(isa, Isa::X64 { win: true, .. }) {
            // `xmm6 ..` used and the scratch `xmm15`.
            at += 16 * (u32::from(regs).saturating_sub(6) + 1);
        } else if isa == Isa::I686 {
            // `ebx`.
            at += 16;
        }
        // The values the scalar body leaves read after the loop: it
        // runs once at least, else they would be stale.
        let temps: HashSet<LocalId> = written
            .iter()
            .filter(|l| **l != iv && **l != cond.0 && !carried.contains(l))
            .copied()
            .collect();
        let latch = f.blocks[l].id;
        let mut skip = !ch
            .exit
            .1
            .iter()
            .any(|o| matches!(o, Operand::Copy(l) if temps.contains(l)));
        if skip && !temps.is_empty() {
            let live = nabic_mir::live_out_of(f, |l| temps.contains(&l), |id| id == latch);
            skip = live.get(&latch.0).is_none_or(Vec::is_empty);
        }
        // The strip loop around tests the counter against its bound
        // and polls; the vector loop runs whole strips up to that bound.
        let outer = skip.then(|| strip_bound(f, &ch.exit, iv, cond.0)).flatten();
        let line = f.blocks[h].stmts.iter().find_map(|s| match s {
            Statement::Loc(sp, scope) => Some((sp.line, sp.col.max(1), scope.id)),
            _ => None,
        });
        let guard = forms.guard.then(|| {
            let mut v: Vec<LocalId> = Vec::new();
            for x in bases.iter().map(|b| b.1).chain(limits.iter().map(|l| l.1)) {
                if let Some(l) = x.inv.filter(|l| !v.contains(l)) {
                    v.push(l);
                }
            }
            v
        });
        Some(Plan {
            w,
            vb: isa.bytes(),
            iv,
            bound: ch.bound.clone(),
            cond,
            exit: ch.exit.clone(),
            skip,
            bases,
            splats,
            accs,
            distinct,
            apart,
            hoist,
            limits,
            guard,
            units: forms.units.clone(),
            nan_free,
            outer,
            pre,
            body,
            post,
            regs,
            io_size: at,
            save,
            line,
        })
    }
}

impl FnLower<'_, '_> {
    /// Locals the loop never writes that hold field `n` of a view
    /// (`Ctx::views`) `v`, each set once from a `v` set at most once:
    /// `local -> (v, n)`.
    fn views(&self, written: &HashSet<LocalId>) -> HashMap<LocalId, (LocalId, u32)> {
        let f = self.f;
        let mut sets: HashMap<LocalId, u32> = HashMap::default();
        let mut reads: HashMap<LocalId, (LocalId, u32)> = HashMap::default();
        for s in f.blocks.iter().flat_map(|b| &b.stmts) {
            let Statement::Assign { target, rvalue } = s else {
                continue;
            };
            *sets.entry(*target).or_default() += 1;
            if let RValue::FieldRead {
                target: Operand::Copy(v),
                index,
                ..
            } = rvalue
            {
                reads.insert(*target, (*v, *index));
            }
        }
        // A parameter is set on entry; a block's on every way in.
        for p in &f.params {
            *sets.entry(*p).or_default() += 1;
        }
        for b in &f.blocks {
            for p in &b.params {
                *sets.entry(*p).or_default() += 2;
            }
        }
        let view = |v: &LocalId| {
            matches!(self.local_ty(*v), MirTy::Struct(id, _) if self.c.views.contains(&id.0))
                && sets.get(v).copied().unwrap_or(0) <= 1
                && !self.addressed.contains(v)
                && !written.contains(v)
        };
        reads.retain(|l, (v, _)| {
            sets.get(l) == Some(&1)
                && !written.contains(l)
                && !self.addressed.contains(l)
                && view(v)
        });
        reads
    }
}

/// Instructions a vector iteration takes, about.
pub(crate) fn cost(isa: Isa, w: u32, body: &[VOp]) -> u64 {
    // SSE2's two-operand forms copy first; VEX ones do not.
    let x64 = matches!(isa, Isa::X64 { avx2: false, .. } | Isa::I686);
    4 + body
        .iter()
        .map(|op| match *op {
            VOp::Bail { .. } => 3,
            VOp::Un {
                op: VUn::SignMask, ..
            } if x64 && w == 8 => 3,
            // Two-operand forms copy first.
            VOp::Bin { dst, a, .. } | VOp::Un { dst, a, .. } | VOp::Shift { dst, a, .. }
                if x64 && dst != a =>
            {
                2
            }
            _ => 1,
        })
        .sum::<u64>()
}

pub(crate) const fn lane_k(w: u32) -> K {
    match w {
        1 => K::I8,
        2 => K::I16,
        4 => K::I32,
        _ => K::I64,
    }
}

/// Buffer pairs that must differ for the accesses to keep their order in
/// vectors, and index pairs into one buffer that must be equal or at
/// least `lanes` apart; `None` when two of one buffer would not.
#[allow(clippy::type_complexity)]
pub(crate) fn order(
    acc: &[Access],
    lanes: u64,
) -> Option<(Vec<(LocalId, LocalId)>, Vec<(Idx, Idx)>)> {
    let mut out = Vec::new();
    let mut apart = Vec::new();
    for (ps, s) in acc.iter().enumerate().filter(|(_, a)| a.store) {
        for (px, x) in acc.iter().enumerate() {
            if px == ps {
                continue;
            }
            if x.inv == s.inv && x.e == s.e {
                if x.off == s.off || x.off.abs_diff(s.off) >= lanes {
                    continue;
                }
                // An element both reach, in different lanes: the scalar
                // loop reaches it first in the one with the higher
                // offset.
                if (x.off > s.off) == (px < ps) {
                    continue;
                }
            }
            if x.base == s.base {
                // Offsets the loop never changes: a test in front.
                if x.inv == s.inv || x.e != s.e {
                    return None;
                }
                let (a, b) = (
                    Idx {
                        inv: s.inv,
                        c: s.off,
                    },
                    Idx {
                        inv: x.inv,
                        c: x.off,
                    },
                );
                if !apart.contains(&(a, b)) && !apart.contains(&(b, a)) {
                    apart.push((a, b));
                }
                continue;
            }
            let pair = (x.base.min(s.base), x.base.max(s.base));
            if !out.contains(&pair) {
                out.push(pair);
            }
        }
    }
    Some((out, apart))
}
