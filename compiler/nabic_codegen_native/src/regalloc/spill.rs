//! Root caching and splitting of values left in memory.

#[allow(clippy::wildcard_imports)]
use crate::regalloc::*;

/// The rooted slots of `f` whose parts may be cached: each reached only
/// as itself, besides the shadow-stack frame taking its address and
/// `Zero` clearing whole parts. `(slot, offset)` -> size.
pub(crate) fn cacheable(f: &Func, regs: &Regs) -> Vec<((Slot, u32), u32)> {
    let Some((frame, roots)) = &f.gc_frame else {
        return Vec::new();
    };
    let mut ok: HashMap<Slot, bool> = roots.iter().map(|s| (*s, true)).collect();
    let mut parts: HashMap<Slot, std::collections::BTreeMap<u32, u32>> = HashMap::default();
    let mut zeros: Vec<(Slot, u32, u32)> = Vec::new();
    let mut ops = Vec::new();
    for i in &f.insts {
        ops.clear();
        operands(i, regs.ptr, &mut ops);
        let reg = matches!(i, Inst::Addr { dst: Mem::Frame(d, _), .. } if d == frame);
        for &(m, size, c, acc) in &ops {
            let (s, o, size, c, acc) = match m {
                Mem::Frame(s, o) => (s, o, size, c, acc),
                Mem::Ind(s, _) => (s, 0, regs.ptr, Class::Int, Acc::Read),
            };
            let Some(good) = ok.get_mut(&s) else { continue };
            match (acc, i) {
                (Acc::Whole, Inst::Addr { .. }) if reg => {}
                (Acc::Whole, Inst::Zero { size, .. }) => zeros.push((s, o, *size)),
                (Acc::Whole, _) => *good = false,
                _ => {
                    let p = parts.entry(s).or_default();
                    let had = *p.entry(o).or_insert(size);
                    *good &= had == size
                        && matches!(c, Class::Int | Class::Any)
                        && matches!(size, 1 | 2 | 4 | 8)
                        && size <= regs.word;
                }
            }
        }
    }
    for v in f.debug.iter().filter(|_| !f.track).flat_map(|d| &d.vars) {
        ok.insert(v.slot, false);
    }
    // Parts must not overlap; a zeroed range covers whole parts.
    for (s, p) in &parts {
        let v: Vec<(u32, u32)> = p.iter().map(|(o, z)| (*o, *z)).collect();
        if !v.windows(2).all(|w| w[0].0 + w[0].1 <= w[1].0) {
            ok.insert(*s, false);
        }
    }
    for &(s, o, size) in &zeros {
        let whole = parts.get(&s).is_none_or(|p| {
            p.iter().all(|(po, pz)| {
                po + pz <= o || o + size <= *po || (o <= *po && po + pz <= o + size)
            })
        });
        if !whole {
            ok.insert(s, false);
        }
    }
    let mut out = Vec::new();
    for s in roots {
        if ok.get(s) == Some(&true) {
            if let Some(p) = parts.get(s) {
                out.extend(p.iter().map(|(o, z)| ((*s, *o), *z)));
            }
        }
    }
    out
}

/// `f` with the parts `cache` names (`cacheable`) read from a temporary
/// instead: written through to the root on every write, reloaded from it
/// after every call (a collection may run there), set to zero where the
/// root is cleared. A root is thus always current in memory, and its
/// temporary may live in a register.
pub(crate) fn cache_roots(f: &Func, regs: &Regs, cache: &[((Slot, u32), u32)]) -> Func {
    // Which parts are live after each call: only those reload.
    let cands = cache.iter().map(|c| (c.0, c.1, false)).collect();
    let lv = live_of(f, &Ops::of(f, regs.ptr), cands, Vec::new());
    let mut reload: HashMap<usize, Vec<usize>> = HashMap::default();
    for b in 0..lv.cfg.starts.len() {
        let mut live = lv.live_out.bits(b);
        for at in (lv.cfg.starts[b]..lv.cfg.end(b)).rev() {
            for &d in &lv.defs[at] {
                live.clear(d);
            }
            if clobbers(&f.insts[at]) {
                reload.insert(at, live.iter().collect());
            }
            for &x in &lv.uses[at] {
                live.set(x);
            }
        }
    }
    let mut out = f.with_insts(Vec::with_capacity(f.insts.len()));
    let mut tmp: HashMap<(Slot, u32), (Slot, K)> = HashMap::default();
    for &(p, size) in cache {
        let t = out.slot(size, size);
        tmp.insert(p, (t, scalar(size).unwrap_or(K::I64)));
    }
    let frame = f.gc_frame.as_ref().map(|g| g.0);
    let mut held = Vec::new();
    for (at, i) in f.insts.iter().enumerate() {
        let mut i = i.clone();
        let mut after = Vec::new();
        let reg = matches!(i, Inst::Addr { dst: Mem::Frame(d, _), .. } if Some(d) == frame);
        if let Inst::Zero {
            dst: Mem::Frame(s, o),
            size,
        } = i
        {
            for &(p @ (ps, po), _) in cache {
                let (t, k) = tmp[&p];
                if ps == s && o <= po && po + k.size() <= o + size {
                    after.push(Inst::Mov {
                        k,
                        dst: Mem::Frame(t, 0),
                        src: V::I(0),
                    });
                }
            }
        } else if !reg {
            let mut written = Vec::new();
            map_mems(&mut i, &mut |m, acc| match *m {
                Mem::Frame(s, o) => {
                    if let Some(&(t, k)) = tmp.get(&(s, o)) {
                        *m = Mem::Frame(t, 0);
                        if acc == Acc::Write {
                            after.push(Inst::Mov {
                                k,
                                dst: Mem::Frame(s, o),
                                src: V::M(Mem::Frame(t, 0)),
                            });
                            written.push((s, o));
                        }
                    }
                }
                Mem::Ind(s, d) => {
                    if let Some(&(t, _)) = tmp.get(&(s, 0)) {
                        *m = Mem::Ind(t, d);
                    }
                }
            });
            for &n in reload.get(&at).map_or(&[][..], Vec::as_slice) {
                let (s, o) = cache[n].0;
                let (t, k) = tmp[&(s, o)];
                if !written.contains(&(s, o)) {
                    after.push(Inst::Mov {
                        k,
                        dst: Mem::Frame(t, 0),
                        src: V::M(Mem::Frame(s, o)),
                    });
                }
            }
        }
        // Write-throughs and reloads in a fixed order; a parameter's after
        // the last parameter, which emitters store first, in order.
        let param = matches!(i, Inst::Param { .. });
        if !param {
            out.insts.append(&mut held);
        }
        out.insts.push(i);
        if param {
            held.extend(after);
        } else {
            out.insts.extend(after);
        }
    }
    out.insts.append(&mut held);
    out
}

/// Split the values live across the calls of rarely run blocks (`cold`,
/// or ending in a trap): copied to a temporary before the call, read from
/// it after (a trap block), or copied back right after the call. The value
/// itself then lives across no call there and may keep a caller-saved
/// register; the temporary, used once, may stay in memory.
pub(crate) fn split_cold(
    f: &mut Func,
    regs: &Regs,
    lv: Option<Live>,
    copies: &mut Vec<Carry>,
) -> Option<Live> {
    let lv = lv.unwrap_or_else(|| live(f, regs));
    let mut before: HashMap<usize, Vec<Inst>> = HashMap::default();
    let mut after: HashMap<usize, Vec<Inst>> = HashMap::default();
    let mut renames: Vec<(usize, usize, Mem, Mem)> = Vec::new();
    let mut slots = f.slots.clone();
    for b in 0..lv.cfg.starts.len() {
        let (s, e) = (lv.cfg.starts[b], lv.cfg.end(b));
        if e <= s {
            continue;
        }
        let trap = matches!(f.insts[e - 1], Inst::Trap);
        let cold = matches!(f.insts[s], Inst::Label(l) if f.cold.contains(&l));
        if !trap && !cold {
            continue;
        }
        let mut live = lv.live_out.bits(b);
        for at in (s..e).rev() {
            for &d in &lv.defs[at] {
                live.clear(d);
            }
            if matches!(f.insts[at], Inst::Call { .. }) {
                for v in live.iter() {
                    let ((vs, vo), size, fp) = lv.cands[v];
                    // No register to keep, so nothing to split for; a
                    // pair is in callee-saved registers anyway.
                    let vec = fp && size > 8;
                    if !has_file(regs, fp) || size > regs.word && !vec {
                        continue;
                    }
                    slots.push((size, size.min(16)));
                    let t = Mem::Frame(slots.len() as Slot - 1, 0);
                    let m = Mem::Frame(vs, vo);
                    copies.push(((vs, vo), (slots.len() as Slot - 1, 0)));
                    let mov = |dst: Mem, src: Mem| {
                        if vec {
                            Inst::V(Box::new(Vx {
                                op: VxOp::Mov,
                                w: 8,
                                vb: size,
                                dst,
                                a: src,
                                b: src,
                            }))
                        } else {
                            Inst::Mov {
                                k: cand_kind(fp, size),
                                dst,
                                src: V::M(src),
                            }
                        }
                    };
                    before.entry(at).or_default().push(mov(t, m));
                    if trap {
                        renames.push((at + 1, e, m, t));
                    } else {
                        after.entry(at).or_default().push(mov(m, t));
                    }
                }
            }
            for &x in &lv.uses[at] {
                live.set(x);
            }
        }
    }
    if before.is_empty() {
        return Some(lv);
    }
    // The copies in place; `pos[at]`: where old instruction `at` went.
    let old = std::mem::take(&mut f.insts);
    let mut pos = Vec::with_capacity(old.len() + 1);
    for (at, i) in old.into_iter().enumerate() {
        if let Some(v) = before.remove(&at) {
            f.insts.extend(v);
        }
        pos.push(f.insts.len());
        f.insts.push(i);
        if let Some(v) = after.remove(&at) {
            f.insts.extend(v);
        }
    }
    pos.push(f.insts.len());
    // Later calls first: an earlier call's rename then covers the copy a
    // later one made. A write of the value ends its rename.
    renames.sort_by_key(|r| std::cmp::Reverse(r.0));
    for &(from, to, m, t) in &renames {
        // Through the block's last instruction.
        for i in &mut f.insts[pos[from - 1] + 1..=pos[to - 1]] {
            let mut wrote = false;
            map_mems(i, &mut |x, acc| match *x {
                Mem::Frame(..) if *x == m => {
                    if acc == Acc::Write {
                        wrote = true;
                    } else {
                        *x = t;
                    }
                }
                Mem::Ind(s, d) if Mem::Frame(s, 0) == m => *x = Mem::Ind(t_slot(t), d),
                _ => {}
            });
            if wrote {
                break;
            }
        }
    }
    f.slots = slots;
    None
}

/// The kind a candidate of `size` bytes moves as.
pub(crate) fn cand_kind(fp: bool, size: u32) -> K {
    match (fp, size) {
        (true, 4) => K::F32,
        (true, _) => K::F64,
        _ => scalar(size).unwrap_or(K::I64),
    }
}

/// After an allocation, the values it left in memory, split where that
/// costs less than the uses a register would serve:
///
/// - around the calls one lives across: in each block, from the first such
///   call to the last (no write of it between), a temporary carries it,
///   copied in before and back after; what reads it there reads the
///   temporary;
/// - out of a loop that uses it no more than its edges run: copied to a
///   temporary on the edges into the loop and back on the edges out
///   where it is live (critical edges split), the temporary standing for
///   it inside, when that costs less than the calls in the loop or, for
///   a value across no call, than half its uses.
///
/// The value then lives across no call and through no such loop, and may
/// take a caller-saved register; the temporary, a callee-saved one or its
/// slot. Whether anything was split.
pub(crate) fn split_ranges(f: &mut Func, lv: &Live, sp: &Spills, copies: &mut Vec<Carry>) -> bool {
    let nv = lv.cands.len();
    let cfg = &lv.cfg;
    let nb = cfg.starts.len();
    let mut want = Bits::new(nv);
    let mut any = false;
    for v in (0..nv).filter(|&v| sp.spill[v] && sp.weight[v] >= SPLIT_MIN) {
        want.set(v);
        any = true;
    }
    if !any {
        return false;
    }
    // Natural loops, innermost first.
    let loops = cfg.block_freqs(f).1;
    let bfreq = |b: usize| sp.freq.get(cfg.starts[b]).copied().unwrap_or(1);
    let mut pred: Vec<Vec<usize>> = vec![Vec::new(); nb];
    for (b, ss) in cfg.succ.iter().enumerate() {
        for &s in ss {
            if !pred[s].contains(&b) {
                pred[s].push(b);
            }
        }
    }
    // The blocks each value is read or written in, with the weight of
    // those accesses.
    let mut touch: HashMap<usize, Vec<(usize, u64)>> = HashMap::default();
    // `(value, block, first, last)`: the calls of a block it lives across.
    let mut regions: Vec<(usize, usize, usize, usize)> = Vec::new();
    let mut open: Vec<(usize, usize, usize, usize)> = Vec::new();
    for b in 0..nb {
        let (s, e) = (cfg.starts[b], cfg.end(b));
        for at in s..e {
            for &x in lv.uses[at].iter().chain(&lv.defs[at]) {
                if want.has(x) {
                    let t = touch.entry(x).or_default();
                    match t.last_mut() {
                        Some(l) if l.0 == b => l.1 += sp.freq[at],
                        _ => t.push((b, sp.freq[at])),
                    }
                }
            }
        }
        if e <= s || !(s..e).any(|at| clobbers(&f.insts[at])) {
            continue;
        }
        let mut live = lv.live_out.bits(b);
        for at in (s..e).rev() {
            for &d in &lv.defs[at] {
                live.clear(d);
                if let Some(k) = open.iter().position(|r| r.0 == d) {
                    regions.push(open.swap_remove(k));
                }
            }
            if clobbers(&f.insts[at]) {
                for (w, (x, y)) in live.0.iter().zip(&want.0).enumerate() {
                    let mut m = x & y;
                    while m != 0 {
                        let v = w * 64 + m.trailing_zeros() as usize;
                        m &= m - 1;
                        match open.iter_mut().find(|r| r.0 == v) {
                            Some(r) => r.2 = at,
                            None => open.push((v, b, at, at)),
                        }
                    }
                }
            }
            for &x in &lv.uses[at] {
                live.set(x);
            }
        }
        regions.append(&mut open);
    }
    regions.sort_unstable();
    // Loops a value lives through untouched, outermost first, where the
    // copies on their edges cost less than those around the calls in them.
    let tail_ok = f.insts.last().is_none_or(|i| !succs(i).2);
    let mut out_of: Vec<(usize, usize)> = Vec::new();
    let mut vals: Vec<usize> = touch.keys().copied().collect();
    vals.sort_unstable();
    let mut cost = vec![0u64; nv];
    for &v in &vals {
        let mut chosen: Vec<usize> = Vec::new();
        let rs = regions.partition_point(|r| r.0 < v);
        let re = regions.partition_point(|r| r.0 <= v);
        let mut calls: u64 = regions[rs..re].iter().map(|r| 2 * bfreq(r.1)).sum();
        for (n, (h, body)) in loops.iter().enumerate().rev() {
            if !tail_ok
                || *h == 0
                || !lv.live_in.has(*h, v)
                || chosen.iter().any(|&c| loops[c].1.has(*h))
            {
                continue;
            }
            // What the loop does with it, the temporary does.
            let used: u64 = touch[&v]
                .iter()
                .filter(|t| body.has(t.0))
                .map(|t| t.1)
                .sum();
            let mut c = 0;
            for &p in pred[*h].iter().filter(|&&p| !body.has(p)) {
                c += bfreq(p).min(bfreq(*h));
            }
            for b in body.iter() {
                for &s in &cfg.succ[b] {
                    if !body.has(s) && lv.live_in.has(s, v) {
                        c += bfreq(b).min(bfreq(s));
                    }
                }
            }
            let inside: u64 = regions[rs..re]
                .iter()
                .filter(|r| body.has(r.1))
                .map(|r| 2 * bfreq(r.1))
                .sum();
            let crossing = re > rs;
            if used <= c && ((crossing && c < inside) || (!crossing && 2 * c < sp.weight[v])) {
                chosen.push(n);
                calls -= inside;
                cost[v] += c;
            }
        }
        cost[v] += calls;
        let margin = if re > rs { 1 } else { 2 };
        if cost[v] == 0 || margin * cost[v] >= sp.weight[v] {
            want.clear(v);
            continue;
        }
        out_of.extend(chosen.into_iter().map(|n| (v, n)));
    }
    regions
        .retain(|r| want.has(r.0) && !out_of.iter().any(|&(v, n)| v == r.0 && loops[n].1.has(r.1)));
    if regions.is_empty() && out_of.is_empty() {
        return false;
    }
    let mut before: HashMap<usize, Vec<Inst>> = HashMap::default();
    let mut after: HashMap<usize, Vec<Inst>> = HashMap::default();
    let temp = |f: &mut Func, v: usize, copies: &mut Vec<Carry>| {
        let ((vs, vo), size, fp) = lv.cands[v];
        let ts = f.slot(size, size);
        copies.push(((vs, vo), (ts, 0)));
        (cand_kind(fp, size), Mem::Frame(vs, vo), Mem::Frame(ts, 0))
    };
    for &(v, _, first, last) in &regions {
        let (k, m, t) = temp(f, v, copies);
        let (vs, vo, ts) = (part_of(m).0, part_of(m).1, part_of(t).0);
        for i in &mut f.insts[first..=last] {
            map_mems(i, &mut |x, acc| match *x {
                Mem::Frame(..) if *x == m && acc != Acc::Write => *x = t,
                Mem::Ind(s, d) if vo == 0 && s == vs => *x = Mem::Ind(ts, d),
                _ => {}
            });
        }
        before.entry(first).or_default().push(Inst::Mov {
            k,
            dst: t,
            src: V::M(m),
        });
        after.entry(last).or_default().push(Inst::Mov {
            k,
            dst: m,
            src: V::M(t),
        });
    }
    // Per edge, the copies out of loops first, then those into them.
    let mut edges: std::collections::BTreeMap<(usize, usize), (Vec<Inst>, Vec<Inst>)> =
        std::collections::BTreeMap::new();
    for &(v, n) in &out_of {
        let (h, body) = &loops[n];
        let (k, m, t) = temp(f, v, copies);
        let (vs, vo, ts) = (part_of(m).0, part_of(m).1, part_of(t).0);
        for b in body.iter() {
            for i in &mut f.insts[cfg.starts[b]..cfg.end(b)] {
                map_mems(i, &mut |x, _| match *x {
                    Mem::Frame(..) if *x == m => *x = t,
                    Mem::Ind(s, d) if vo == 0 && s == vs => *x = Mem::Ind(ts, d),
                    _ => {}
                });
            }
        }
        for &p in pred[*h].iter().filter(|&&p| !body.has(p)) {
            edges.entry((p, *h)).or_default().1.push(Inst::Mov {
                k,
                dst: t,
                src: V::M(m),
            });
        }
        for b in body.iter() {
            for &s in &cfg.succ[b] {
                if !body.has(s) && lv.live_in.has(s, v) {
                    let e = edges.entry((b, s)).or_default();
                    if !e
                        .0
                        .iter()
                        .any(|i| matches!(i, Inst::Mov { dst, .. } if *dst == m))
                    {
                        e.0.push(Inst::Mov {
                            k,
                            dst: m,
                            src: V::M(t),
                        });
                    }
                }
            }
        }
    }
    let mut tail = Vec::new();
    for ((p, s), (outs, ins)) in edges {
        let mut code = outs;
        code.extend(ins);
        let last = cfg.end(p) - 1;
        let start = cfg.starts[s];
        if cfg.succ[p].iter().all(|&x| x == s) {
            if matches!(f.insts[last], Inst::Jmp(_)) {
                before.entry(last).or_default().extend(code);
            } else {
                after.entry(last).or_default().extend(code);
            }
        } else if pred[s].len() == 1 {
            if matches!(f.insts[start], Inst::Label(_)) {
                after.entry(start).or_default().extend(code);
            } else {
                before.entry(start).or_default().extend(code);
            }
        } else {
            let l = match f.insts.get(start) {
                Some(Inst::Label(l)) => Some(*l),
                _ => None,
            };
            let (ls, k, _) = succs(&f.insts[last]);
            match l.filter(|l| ls[..k].contains(l)) {
                Some(l) => {
                    let new = f.labels;
                    f.labels += 1;
                    match &mut f.insts[last] {
                        Inst::Br { t, f: e, .. } => {
                            if *t == l {
                                *t = new;
                            } else {
                                *e = new;
                            }
                        }
                        Inst::Ovf { fail, .. } => *fail = new,
                        Inst::Jmp(x) => *x = new,
                        _ => {}
                    }
                    tail.push(Inst::Label(new));
                    tail.extend(code);
                    tail.push(Inst::Jmp(l));
                }
                // The fall-through edge: the copies go between.
                None => after.entry(last).or_default().extend(code),
            }
        }
    }
    let old = std::mem::take(&mut f.insts);
    f.insts.reserve(old.len() + tail.len());
    for (at, i) in old.into_iter().enumerate() {
        if let Some(v) = before.remove(&at) {
            f.insts.extend(v);
        }
        f.insts.push(i);
        if let Some(v) = after.remove(&at) {
            f.insts.extend(v);
        }
    }
    f.insts.extend(tail);
    true
}

pub(crate) const fn t_slot(m: Mem) -> Slot {
    match m {
        Mem::Frame(s, _) | Mem::Ind(s, _) => s,
    }
}

/// Root parts to cache, `(slot, offset)` -> size (`cacheable`).
pub(crate) type Cache = Vec<((Slot, u32), u32)>;

/// The `cacheable` parts of `f` worth caching: the reads they save
/// outweigh the reloads after calls that remain live. With them all,
/// also `f` with them cached and dead code dropped, and its liveness
/// when the last cleanups changed nothing.
pub(crate) fn worth_caching(
    f: &Func,
    regs: &Regs,
    fq: &Freq,
) -> (Cache, Option<(Func, Option<Live>)>) {
    let mut cache = cacheable(f, regs);
    if cache.is_empty() {
        return (cache, None);
    }
    let base = f.slots.len() as Slot;
    let mut g = cache_roots(f, regs, &cache);
    let lv = dce(&mut g, regs);
    let freq = fq.at(&g, &Cfg::new(&g));
    let mut gain = vec![0i64; cache.len()];
    let tmp = |m: &Mem| match m {
        Mem::Frame(s, _) | Mem::Ind(s, _) => (*s >= base).then(|| (*s - base) as usize),
    };
    let mut ops = Vec::new();
    for (at, i) in g.insts.iter().enumerate() {
        let w = freq[at] as i64;
        if let Inst::Mov {
            dst,
            src: V::M(src @ Mem::Frame(..)),
            ..
        } = i
        {
            match (tmp(dst), tmp(src)) {
                // A reload.
                (Some(t), None) if Mem::Frame(cache[t].0.0, cache[t].0.1) == *src => {
                    gain[t] -= w;
                    continue;
                }
                // A write-through: the store the root needed anyway.
                (None, Some(t)) if Mem::Frame(cache[t].0.0, cache[t].0.1) == *dst => continue,
                _ => {}
            }
        }
        ops.clear();
        operands(i, regs.ptr, &mut ops);
        for (m, _, _, acc) in &ops {
            if let Some(t) = tmp(m) {
                if *acc != Acc::Write || matches!(m, Mem::Ind(..)) {
                    gain[t] += w;
                }
            }
        }
    }
    if gain.iter().all(|&x| x > 0) {
        let mut same = None;
        if let Some(lv) = lv {
            let a = own_conds(&mut g, &lv);
            let b = coalesce(&mut g, &lv);
            same = (!a && !b).then_some(lv);
        }
        return (cache, Some((g, same)));
    }
    let mut n = 0;
    cache.retain(|_| {
        n += 1;
        gain[n - 1] > 0
    });
    (cache, None)
}

/// Locals for `parts` (each with a type key): parts whose live ranges
/// never meet share one. Returns each part's local number within its key.
#[must_use]
pub fn share(f: &Func, regs: &Regs, parts: &[((Slot, u32), u8)]) -> Vec<u32> {
    let cands: Vec<Cand> = parts.iter().map(|p| (p.0, 0, false)).collect();
    let lv = live_of(f, &Ops::of(f, regs.ptr), cands, Vec::new());
    let (ranges, ..) = ranges(f, &lv, None);
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&v| (ranges[v].first().map_or(usize::MAX, |r| r.0), v));
    let mut colors: HashMap<u8, Vec<Ranges>> = HashMap::default();
    let mut out = vec![0u32; parts.len()];
    for v in order {
        let cs = colors.entry(parts[v].1).or_default();
        let c = if let Some(c) = cs.iter().position(|o| !overlap(o, &ranges[v])) {
            c
        } else {
            cs.push(Vec::new());
            cs.len() - 1
        };
        let o = &mut cs[c];
        *o = union(o, &ranges[v]);
        out[v] = c as u32;
    }
    out
}

/// The live ranges of `parts` (in order) that `share` reads: where a
/// part's local holds it (`wasm`'s `-g`).
#[must_use]
pub fn part_ranges(f: &Func, regs: &Regs, parts: &[(Slot, u32)]) -> Vec<Ranges> {
    let cands: Vec<Cand> = parts.iter().map(|&p| (p, 0, false)).collect();
    let lv = live_of(f, &Ops::of(f, regs.ptr), cands, Vec::new());
    ranges(f, &lv, None).0
}
