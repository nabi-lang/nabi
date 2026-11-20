//! Cleanups around allocation: dead code, coalescing, word splits and constant hoisting.

#[allow(clippy::wildcard_imports)]
use crate::regalloc::*;

/// Whether `i` only computes its destinations (no memory reached through a
/// pointer, no side effect): dead when they are.
pub fn pure(i: &Inst) -> bool {
    let ind = |v: &V| matches!(v, V::M(Mem::Ind(..)));
    match i {
        Inst::Mov { dst, src, .. } => !matches!(dst, Mem::Ind(..)) && !ind(src),
        Inst::Copy { dst, src, size } => {
            scalar(*size).is_some() && matches!((dst, src), (Mem::Frame(..), Mem::Frame(..)))
        }
        Inst::Bin { dst, a, b, .. } | Inst::Cmp { dst, a, b, .. } => {
            !matches!(dst, Mem::Ind(..)) && !ind(a) && !ind(b)
        }
        Inst::Un { dst, a, .. } | Inst::Conv { dst, a, .. } => {
            !matches!(dst, Mem::Ind(..)) && !ind(a)
        }
        Inst::Fma { dst, a, b, c, .. } => {
            !matches!(dst, Mem::Ind(..)) && !ind(a) && !ind(b) && !ind(c)
        }
        Inst::Sym { dst, .. } => !matches!(dst, Mem::Ind(..)),
        Inst::V(x) => {
            !matches!(x.dst, Mem::Ind(..))
                && x.srcs()
                    .chain(x.scalar().map(|s| s.0))
                    .all(|m| matches!(m, Mem::Frame(..)))
        }
        _ => false,
    }
}

/// Drop pure instructions whose results no one reads; the liveness of
/// the result, if the last round found nothing to drop.
pub(crate) fn dce(f: &mut Func, regs: &Regs) -> Option<Live> {
    let mut ops = Vec::new();
    for _ in 0..4 {
        let lv = live(f, regs);
        let n = f.insts.len();
        let mut dead = vec![false; n];
        let mut any = false;
        for b in 0..lv.cfg.starts.len() {
            let (s, e) = (lv.cfg.starts[b], lv.cfg.end(b));
            let mut live = lv.live_out.bits(b);
            for at in (s..e).rev() {
                let i = &f.insts[at];
                if pure(i) && !lv.defs[at].is_empty() {
                    // Every destination a candidate (else memory others
                    // read), none live.
                    ops.clear();
                    operands(i, regs.ptr, &mut ops);
                    let writes = ops.iter().filter(|o| o.3 == Acc::Write).count();
                    if writes == lv.defs[at].len() && lv.defs[at].iter().all(|&d| !live.has(d)) {
                        dead[at] = true;
                        any = true;
                        continue;
                    }
                }
                for &d in &lv.defs[at] {
                    live.clear(d);
                }
                for &x in &lv.uses[at] {
                    live.set(x);
                }
            }
        }
        if !any {
            return Some(lv);
        }
        let mut k = 0;
        f.insts.retain(|_| {
            k += 1;
            !dead[k - 1]
        });
    }
    None
}

/// `t = ..; a = t` with `t` dead after the move: computed straight into
/// `a`, the move dropped.
pub(crate) fn coalesce(f: &mut Func, lv: &Live) -> bool {
    let mut drop = vec![false; f.insts.len()];
    let mut any = false;
    for b in 0..lv.cfg.starts.len() {
        let (s, e) = (lv.cfg.starts[b], lv.cfg.end(b));
        let mut live = lv.live_out.bits(b);
        for at in (s..e).rev() {
            if at > s {
                if let (Some((size, a, t @ Mem::Frame(..))), [vt]) =
                    (a_move(&f.insts[at]), &lv.defs[at - 1])
                {
                    let dead = !live.has(*vt) && lv.uses[at].contains(vt);
                    let c = lv.cands[*vt];
                    if dead && a != t && c.0 == part_of(t) && c.1 == size {
                        if let Some(x) = retarget(&f.insts[at - 1], t, a, size) {
                            f.insts[at - 1] = x;
                            drop[at] = true;
                            any = true;
                        }
                    }
                }
            }
            for &d in &lv.defs[at] {
                live.clear(d);
            }
            for &x in &lv.uses[at] {
                live.set(x);
            }
        }
    }
    if any {
        let mut k = 0;
        f.insts.retain(|_| {
            k += 1;
            !drop[k - 1]
        });
    }
    any
}

/// The `(slot, offset)` of a frame part.
pub(crate) const fn part_of(m: Mem) -> (Slot, u32) {
    match m {
        Mem::Frame(s, o) => (s, o),
        Mem::Ind(s, _) => (s, 0),
    }
}

/// A comparison only the branch right after it reads, of a part also
/// written elsewhere, into a temporary of its own: the branch then takes
/// the flags and nothing is kept.
pub(crate) fn own_conds(f: &mut Func, lv: &Live) -> bool {
    let n = f.slots.len();
    let mut block = 0;
    for at in 0..f.insts.len().saturating_sub(1) {
        while lv.cfg.end(block) <= at + 1 {
            block += 1;
        }
        let (Inst::Cmp { dst, .. }, Inst::Br { c: V::M(c), .. }) = (&f.insts[at], &f.insts[at + 1])
        else {
            continue;
        };
        if dst != c || lv.defs[at].len() != 1 {
            continue;
        }
        let v = lv.defs[at][0];
        // The branch ends its block: nothing reads the part after it.
        if lv.cfg.end(block) != at + 2 || lv.live_out.has(block, v) {
            continue;
        }
        let t = Mem::Frame(f.slot(1, 1), 0);
        if let Inst::Cmp { dst, .. } = &mut f.insts[at] {
            *dst = t;
        }
        if let Inst::Br { c, .. } = &mut f.insts[at + 1] {
            *c = V::M(t);
        }
    }
    f.slots.len() != n
}

/// `f` with the copies and clears of small aggregates (two to four
/// words) split into word moves, where every other access of the slot is
/// a word at a word offset: its words may then live in registers.
pub(crate) fn split_words(f: &Func, regs: &Regs) -> Option<Func> {
    let w = regs.word;
    let wk = scalar(w)?;
    let frame = f.gc_frame.as_ref().map(|g| g.0);
    let mut ok: Vec<bool> = f
        .slots
        .iter()
        .map(|&(size, align)| size > w && size % w == 0 && size <= 8 * w && align >= w)
        .collect();
    let mut want = vec![false; ok.len()];
    for v in f.debug.iter().filter(|_| !f.track).flat_map(|d| &d.vars) {
        ok[v.slot as usize] = false;
    }
    let words = |ok: &mut Vec<bool>, want: &mut Vec<bool>, m: Mem, size: u32| {
        if let Mem::Frame(s, o) = m {
            let s = s as usize;
            if o % w == 0 && size % w == 0 && o + size <= f.slots[s].0 {
                want[s] = true;
            } else {
                ok[s] = false;
            }
        }
    };
    let mut ops = Vec::new();
    for i in &f.insts {
        match i {
            Inst::Copy { dst, src, size } if scalar(*size).is_none() => {
                if dst == src
                    || matches!((dst, src), (Mem::Frame(a, _), Mem::Frame(b, _)) if a == b)
                {
                    for m in [dst, src] {
                        if let Mem::Frame(s, _) = m {
                            ok[*s as usize] = false;
                        }
                    }
                    continue;
                }
                words(&mut ok, &mut want, *dst, *size);
                words(&mut ok, &mut want, *src, *size);
                for m in [dst, src] {
                    if let Mem::Ind(s, _) = m {
                        if regs.ptr != w {
                            ok[*s as usize] = false;
                        }
                    }
                }
            }
            Inst::Zero { dst, size } => words(&mut ok, &mut want, *dst, *size),
            Inst::Addr {
                dst: Mem::Frame(d, _),
                src: Mem::Frame(..),
            } if Some(*d) == frame => {}
            _ => {
                ops.clear();
                operands(i, regs.ptr, &mut ops);
                for &(m, size, _, acc) in &ops {
                    let (s, o, size) = match m {
                        Mem::Frame(s, o) => (s, o, size),
                        Mem::Ind(s, _) => (s, 0, regs.ptr),
                    };
                    if acc == Acc::Whole || size != w || o % w != 0 {
                        ok[s as usize] = false;
                    }
                }
            }
        }
    }
    if !ok.iter().zip(&want).any(|(a, b)| *a && *b) {
        return None;
    }
    let split = |m: &Mem| matches!(m, Mem::Frame(s, _) if ok[*s as usize] && want[*s as usize]);
    let roots = f.gc_frame.as_ref().map_or(&[][..], |g| &g.1[..]);
    let rooted = |m: &Mem| matches!(m, Mem::Frame(s, _) if roots.contains(s));
    let mut out = f.with_insts(Vec::with_capacity(f.insts.len() + 8));
    let mut any = false;
    for i in &f.insts {
        match i {
            Inst::Copy { dst, src, size }
                if scalar(*size).is_none() && (split(dst) || split(src)) =>
            {
                for k in 0..size / w {
                    out.insts.push(Inst::Copy {
                        dst: dst.at(k * w),
                        src: src.at(k * w),
                        size: w,
                    });
                }
                any = true;
            }
            // A root's clear stays whole: `cache_roots` clears its parts.
            Inst::Zero { dst, size } if split(dst) && !rooted(dst) => {
                for k in 0..size / w {
                    out.insts.push(Inst::Mov {
                        k: wk,
                        dst: dst.at(k * w),
                        src: V::I(0),
                    });
                }
                any = true;
            }
            i => out.insts.push(i.clone()),
        }
    }
    any.then_some(out)
}

/// `f` with `t = ..; a = t` (`t` read only there) computed straight into
/// `a`.
pub(crate) fn forward_moves(f: &Func) -> Option<Func> {
    let movs = f
        .insts
        .iter()
        .any(|i| matches!(a_move(i), Some((_, Mem::Frame(..), Mem::Frame(..)))));
    if !movs {
        return None;
    }
    let (waits, _) = crate::lir::waitable(f);
    let mut out = f.with_insts(Vec::with_capacity(f.insts.len()));
    let mut any = false;
    let mut at = 0;
    while at < f.insts.len() {
        let i = &f.insts[at];
        if let Some((size, a @ Mem::Frame(..), t @ Mem::Frame(ts, to))) =
            f.insts.get(at + 1).and_then(a_move)
        {
            if a != t && waits.get(&(ts, to)) == Some(&(size, 1)) {
                if let Some(x) = retarget(i, t, a, size) {
                    out.insts.push(x);
                    any = true;
                    at += 2;
                    continue;
                }
            }
        }
        // `t = j; .. t ..`, `t` read only by the next instruction: it
        // reads `j`.
        if let (
            Inst::Mov {
                k,
                dst: t @ Mem::Frame(ts, to),
                src: V::M(j @ Mem::Frame(js, jo)),
            },
            Some(next),
        ) = (i, f.insts.get(at + 1))
        {
            if t != j && waits.get(&(*ts, *to)) == Some(&(k.size(), 1)) {
                let mut y = next.clone();
                let mut hits = 0;
                map_mems(&mut y, &mut |m, acc| match *m {
                    Mem::Frame(..) if *m == *t && acc == Acc::Read => {
                        *m = *j;
                        hits += 1;
                    }
                    Mem::Ind(s, d) if s == *ts && *to == 0 && *jo == 0 => {
                        *m = Mem::Ind(*js, d);
                        hits += 1;
                    }
                    _ => {}
                });
                if hits == 1 {
                    out.insts.push(y);
                    any = true;
                    at += 2;
                    continue;
                }
            }
        }
        out.insts.push(i.clone());
        at += 1;
    }
    any.then_some(out)
}

/// A move of `size` bytes, `(size, dst, src)`: a `Mov` of memory or a
/// scalar `Copy`.
pub(crate) fn a_move(i: &Inst) -> Option<(u32, Mem, Mem)> {
    match i {
        Inst::Mov {
            k,
            dst,
            src: V::M(src),
        } => Some((k.size(), *dst, *src)),
        Inst::Copy { dst, src, size } if scalar(*size).is_some() => Some((*size, *dst, *src)),
        Inst::V(x) if matches!(x.op, VxOp::Mov) => match (x.dst, x.a) {
            (d @ Mem::Frame(..), s @ Mem::Frame(..)) => Some((x.vb, d, s)),
            _ => None,
        },
        _ => None,
    }
}

/// `i`, which writes `t` (`size` bytes), writing `a` instead.
pub(crate) fn retarget(i: &Inst, t: Mem, a: Mem, size: u32) -> Option<Inst> {
    let mut i = i.clone();
    if let Inst::V(x) = &mut i {
        if x.dst != t || x.dst_size() != size {
            return None;
        }
        x.dst = a;
        return Some(i);
    }
    let (dst, ik) = match &mut i {
        Inst::Bin { k, dst, .. } | Inst::Un { k, dst, .. } | Inst::Mov { k, dst, .. } => (dst, *k),
        Inst::Conv { to, dst, .. } => (dst, *to),
        Inst::Cmp { dst, .. } => (dst, K::I8),
        _ => return None,
    };
    if *dst != t || ik.size() != size {
        return None;
    }
    *dst = a;
    Some(i)
}

/// `f` with the float constants its loops use, and the integer ones an
/// instruction cannot take as they are (`Regs::costly_imm`), computed
/// once on entry into temporaries.
pub(crate) fn hoist_consts(f: &Func, regs: &Regs, fq: &Freq) -> Option<Func> {
    let floats = has_file(regs, true);
    let costly = regs.costly_imm.filter(|_| has_file(regs, false));
    let big = |op: Option<Bin>, v: &V| matches!(*v, V::I(x) if costly.is_some_and(|c| c(op, x)));
    let some = |i: &Inst| match i {
        Inst::Bin { k, a, b, op, .. } => {
            if k.is_float() {
                floats && (matches!(a, V::I(_)) || matches!(b, V::I(_)))
            } else {
                big(Some(*op), b)
            }
        }
        Inst::Cmp { k, a, b, .. } => {
            if k.is_float() {
                floats && (matches!(a, V::I(_)) || matches!(b, V::I(_)))
            } else {
                big(None, a) || big(None, b)
            }
        }
        Inst::Mov { k, src: a, .. } | Inst::Un { k, a, .. } => {
            floats && k.is_float() && matches!(a, V::I(_))
        }
        _ => false,
    };
    if !f.insts.iter().any(some) {
        return None;
    }
    let freq = fq.at(f, &Cfg::new(f));
    let mut out = f.clone();
    let mut tmp: Vec<(K, i64, Slot)> = Vec::new();
    for (at, i) in out.insts.iter_mut().enumerate() {
        if freq[at] < 4 * UNIT {
            continue;
        }
        if !some(i) {
            continue;
        }
        let k = match i {
            Inst::Bin { k, .. }
            | Inst::Cmp { k, .. }
            | Inst::Mov { k, .. }
            | Inst::Un { k, .. } => *k,
            _ => continue,
        };
        let op = match i {
            Inst::Bin { op, .. } => Some(*op),
            _ => None,
        };
        let mut each = |v: &mut V| {
            if let V::I(bits) = *v {
                let t = if let Some(t) = tmp.iter().find(|t| t.0 == k && t.1 == bits) {
                    t.2
                } else {
                    let t = (f.slots.len() + tmp.len()) as Slot;
                    tmp.push((k, bits, t));
                    t
                };
                *v = V::M(Mem::Frame(t, 0));
            }
        };
        match i {
            Inst::Bin { a, b, .. } | Inst::Cmp { a, b, .. } if k.is_float() => {
                each(a);
                each(b);
            }
            Inst::Bin { b, .. } => each(b),
            Inst::Cmp { a, b, .. } => {
                if big(op, a) {
                    each(a);
                }
                if big(op, b) {
                    each(b);
                }
            }
            Inst::Mov { src: a, .. } | Inst::Un { a, .. } => each(a),
            _ => {}
        }
    }
    if tmp.is_empty() {
        return None;
    }
    let at = out
        .insts
        .iter()
        .position(|i| !matches!(i, Inst::Param { .. }))
        .unwrap_or(out.insts.len());
    let movs: Vec<Inst> = tmp
        .iter()
        .map(|&(k, bits, t)| {
            out.slots.push((k.size(), k.size()));
            Inst::Mov {
                k,
                dst: Mem::Frame(t, 0),
                src: V::I(bits),
            }
        })
        .collect();
    out.insts.splice(at..at, movs);
    Some(out)
}
