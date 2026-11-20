//! Register assignment: hottest candidates first.

#[allow(clippy::wildcard_imports)]
use crate::regalloc::*;

/// Allocate registers for `f`'s candidate parts: hottest first, each to
/// the first register of its pool whose values' ranges miss its own.
#[must_use]
#[cfg(test)]
pub fn allocate(f: &Func, regs: &Regs) -> Alloc {
    allocate_with(f, regs, &live(f, regs), &Freq::of(f), &[], &|_| 0).0
}

/// What an allocation left in memory that splitting may help.
pub(crate) struct Spills {
    /// Candidates of a register class with no register.
    pub(crate) spill: Vec<bool>,
    pub(crate) weight: Vec<u64>,
    pub(crate) freq: Vec<u64>,
}

/// A part a temporary carries the value of, and the temporary.
pub(crate) type Carry = ((Slot, u32), (Slot, u32));

/// Allocate registers for `f`'s candidate parts, with its liveness at hand.
pub(crate) fn allocate_with(
    f: &Func,
    regs: &Regs,
    lv: &Live,
    fq: &Freq,
    copies: &[Carry],
    kill: &dyn Fn(&Inst) -> u64,
) -> (Alloc, Spills) {
    let Live {
        cands,
        full,
        uses,
        defs,
        cfg,
        ..
    } = lv;
    if cands.is_empty() {
        let none = Spills {
            spill: Vec::new(),
            weight: Vec::new(),
            freq: Vec::new(),
        };
        return (Alloc::default(), none);
    }
    let nv = cands.len();
    let n = f.insts.len();
    let clobber = |i: &Inst| regs.clobber.map_or(0, |c| c(i)) | kill(i);
    let (ranges, crosses, kill) = ranges(f, lv, Some(&clobber));
    let index: HashMap<(Slot, u32), usize> =
        cands.iter().enumerate().map(|(i, c)| (c.0, i)).collect();
    let vreg = |m: Mem| match m {
        Mem::Frame(s, o) => index.get(&(s, o)).copied(),
        Mem::Ind(..) => None,
    };
    let forbid = forbidden(f, regs, &vreg, nv);
    let freq = fq.at(f, cfg);
    let mut weight = vec![0u64; nv];
    for at in 0..n {
        for &v in uses[at].iter().chain(&defs[at]) {
            weight[v] += freq[at];
        }
    }
    let mut order: Vec<(std::cmp::Reverse<u64>, usize, usize)> = (0..nv)
        .filter(|&v| !ranges[v].is_empty())
        .map(|v| {
            let span = ranges[v].iter().map(|r| r.1 - r.0 + 1).sum::<usize>();
            (std::cmp::Reverse(weight[v]), span, v)
        })
        .collect();
    order.sort_unstable();
    let order = order.into_iter().map(|x| x.2);
    let mut assigned: Vec<Option<u32>> = vec![None; nv];
    let mut high: Vec<Option<u32>> = vec![None; nv];
    // What each register holds, by class.
    let mut occ: [Vec<Option<Ranges>>; 2] = [vec![None; 64], vec![None; 64]];
    let mut pool: Vec<u32> = Vec::new();
    for v in order {
        let fp = cands[v].2;
        let c = usize::from(fp);
        let (saved, free, arg) = if fp {
            (regs.fp_saved, regs.fp_free, regs.fp_arg)
        } else {
            (regs.int_saved, regs.int_free, regs.int_arg)
        };
        let no = forbid[v][c] | if fp { 0 } else { kill[v] };
        pool.clear();
        // Calls keep only the low half of some callee-saved vector
        // registers (arm64 `v8`..`v15`, Windows `ymm6`..`ymm15`).
        let size = cands[v].1;
        let saved = if size > 16 || size > 8 && !regs.vec_saved {
            &[][..]
        } else {
            saved
        };
        if crosses[v] {
            pool.extend(saved.iter().filter(|r| no & (1 << **r) == 0));
        } else {
            pool.extend(
                free.iter()
                    .chain(arg)
                    .chain(saved)
                    .filter(|r| no & (1 << **r) == 0)
                    .copied(),
            );
        }
        // A callee-saved register no value used yet costs a save and a
        // restore per call of the function: only for a value used often,
        // or one whose pair partner the prologue saves anyway.
        let fits = |occ: &[Vec<Option<Ranges>>; 2], r: u32| match &occ[c][r as usize] {
            Some(o) => !overlap(o, &ranges[v]),
            None => match saved.iter().position(|&x| x == r) {
                None => true,
                Some(_) if regs.saves_all && !fp => true,
                Some(i) => {
                    weight[v] >= OPEN_SAVED
                        || regs.saved_pairs
                            && saved
                                .get(i ^ 1)
                                .is_some_and(|p| occ[c][*p as usize].is_some())
                }
            },
        };
        let pair = !fp && cands[v].1 > regs.word;
        let mut picked = [None; 2];
        if pair {
            if weight[v] < PAIR_MIN {
                continue;
            }
            let mut two = pool.iter().copied().filter(|&r| fits(&occ, r));
            picked = [two.next(), two.next()];
            if picked[1].is_none() {
                continue;
            }
        } else {
            picked[0] = pool.iter().copied().find(|&r| fits(&occ, r));
        }
        for &r in picked.iter().flatten() {
            let o = occ[c][r as usize].get_or_insert_with(Vec::new);
            *o = union(o, &ranges[v]);
        }
        if let Some(r) = picked[0] {
            assigned[v] = Some(r);
        }
        if let Some(h) = picked[1] {
            high[v] = Some(h);
        }
    }
    let mut out = Alloc::default();
    for (v, r) in assigned.iter().enumerate() {
        if let (Some(lo), Some(hi)) = (*r, high[v]) {
            out.pairs.insert(cands[v].0, (lo, hi));
        } else if let Some(reg) = *r {
            out.map.insert(
                cands[v].0,
                R {
                    reg,
                    fp: cands[v].2,
                },
            );
        }
    }
    out.saved = regs
        .int_saved
        .iter()
        .copied()
        .filter(|r| occ[0][*r as usize].is_some())
        .collect();
    out.saved_fp = regs
        .fp_saved
        .iter()
        .copied()
        .filter(|r| occ[1][*r as usize].is_some())
        .collect();
    if f.track {
        for (v, r) in assigned.iter().enumerate() {
            if r.is_some() {
                out.live.insert(cands[v].0, (cands[v].1, ranges[v].clone()));
            }
        }
        for &(p, t) in copies {
            if let Some(&x) = index.get(&t) {
                out.copies.push(((p, t), ranges[x].clone()));
            }
        }
    }
    let spill = (0..nv)
        .map(|v| {
            let ((_, _), size, fp) = cands[v];
            assigned[v].is_none()
                && !ranges[v].is_empty()
                && size <= regs.word
                && has_file(regs, fp)
        })
        .collect();
    // A slot whose every part is in a register needs no memory.
    let mut gone = full.clone();
    for c in cands {
        if !out.map.contains_key(&c.0) && !out.pairs.contains_key(&c.0) {
            gone[c.0.0 as usize] = false;
        }
    }
    out.gone = gone;
    (
        out,
        Spills {
            spill,
            weight,
            freq,
        },
    )
}
