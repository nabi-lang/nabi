//! Blocks, successors and loop-weighted frequencies.

#[allow(clippy::wildcard_imports)]
use crate::regalloc::*;

/// One run of an instruction per run of its function (`Cfg::freqs`).
pub const UNIT: u64 = 1 << 10;

/// The most runs per run of the function `Cfg::freqs` says.
pub(crate) const MAX_RUNS: f64 = 32768.0;

/// The most a loop multiplies its body's runs by.
pub(crate) const LOOP_MAX: f64 = 64.0;

/// The probability of a branch into a rarely run path.
pub(crate) const RARE: f64 = 1.0 / 2048.0;

/// The loop-weighted uses and writes a value needs to open a callee-saved
/// register.
pub(crate) const OPEN_SAVED: u64 = 6 * UNIT;

/// The loop-weighted uses and writes a value left in memory needs to be
/// split (`split_ranges`): a loop's worth.
pub(crate) const SPLIT_MIN: u64 = 8 * UNIT;

/// The loop-weighted uses and writes a two-word value needs to take two
/// registers: more than a loop's worth (8), so a pair holds a value a
/// loop works on, not one read once on the way.
pub const PAIR_MIN: u64 = 12 * UNIT;

/// A function's blocks.
pub struct Cfg {
    /// Block starts; a block ends where the next starts.
    pub starts: Vec<usize>,
    pub succ: Vec<Vec<usize>>,
    n: usize,
}

impl Cfg {
    pub fn new(f: &Func) -> Self {
        let n = f.insts.len();
        let mut label_at = vec![usize::MAX; f.labels as usize];
        for (at, i) in f.insts.iter().enumerate() {
            if let Inst::Label(l) = i {
                label_at[*l as usize] = at;
            }
        }
        let mut starts = vec![0usize];
        for (at, i) in f.insts.iter().enumerate() {
            if matches!(i, Inst::Label(_)) && at != 0 && *starts.last().unwrap_or(&0) != at {
                starts.push(at);
            }
            let (_, k, fall) = succs(i);
            if (k > 0 || !fall) && at + 1 < n {
                starts.push(at + 1);
            }
        }
        starts.dedup();
        let nb = starts.len();
        let mut block_of = vec![0usize; n.max(1)];
        for b in 0..nb {
            let end = starts.get(b + 1).copied().unwrap_or(n);
            for x in &mut block_of[starts[b]..end] {
                *x = b;
            }
        }
        let mut succ: Vec<Vec<usize>> = vec![Vec::new(); nb];
        for b in 0..nb {
            let end = starts.get(b + 1).copied().unwrap_or(n);
            if end <= starts[b] {
                continue;
            }
            let (ls, k, fall) = succs(&f.insts[end - 1]);
            for &l in &ls[..k] {
                let at = label_at[l as usize];
                if at != usize::MAX {
                    succ[b].push(block_of[at]);
                }
            }
            if fall && b + 1 < nb {
                succ[b].push(b + 1);
            }
        }
        Self { starts, succ, n }
    }

    pub fn end(&self, b: usize) -> usize {
        self.starts.get(b + 1).copied().unwrap_or(self.n)
    }

    /// How often each instruction runs per run of the function, in
    /// [`UNIT`]s: block frequencies from static branch probabilities
    /// (`probs`), loops scaled by how often they come back (Wu, Larus),
    /// [`MAX_RUNS`] at most, one unit at least.
    #[cfg(test)]
    pub(crate) fn freqs(&self, f: &Func) -> Vec<u64> {
        Freq::of(f).at(f, self)
    }

    /// Each block's key: the label it starts with, or the one before and
    /// how many blocks on from it. The passes of `prepare` add and drop
    /// no branches or labels, so keys outlive them.
    pub(crate) fn keys(&self, f: &Func) -> Vec<(u32, u32)> {
        let mut out = Vec::with_capacity(self.starts.len());
        let mut cur = (u32::MAX, 0);
        for &s in &self.starts {
            cur = match f.insts.get(s) {
                Some(Inst::Label(l)) => (*l, 0),
                _ if s == 0 => cur,
                _ => (cur.0, cur.1 + 1),
            };
            out.push(cur);
        }
        out
    }

    /// Each block's runs per run of the function.
    pub(crate) fn block_freqs(&self, f: &Func) -> (Vec<f64>, Vec<(usize, Bits)>) {
        let nb = self.starts.len();
        let mut pred: Vec<Vec<usize>> = vec![Vec::new(); nb];
        for (b, ss) in self.succ.iter().enumerate() {
            for &s in ss {
                pred[s].push(b);
            }
        }
        // Reverse postorder from the entry.
        let mut post = Vec::with_capacity(nb);
        let mut seen = vec![false; nb];
        let mut stack = vec![(0usize, 0usize)];
        seen[0] = nb > 0;
        while let Some(&mut (b, ref mut k)) = stack.last_mut() {
            if let Some(&s) = self.succ[b].get(*k) {
                *k += 1;
                if !seen[s] {
                    seen[s] = true;
                    stack.push((s, 0));
                }
            } else {
                post.push(b);
                stack.pop();
            }
        }
        let mut po = vec![usize::MAX; nb];
        for (n, &b) in post.iter().enumerate() {
            po[b] = n;
        }
        // Immediate dominators (Cooper, Harvey, Kennedy).
        let none = usize::MAX;
        let mut idom = vec![none; nb];
        if nb > 0 {
            idom[0] = 0;
        }
        let mut changed = true;
        while changed {
            changed = false;
            for &b in post.iter().rev().skip(1) {
                let mut new = none;
                for &p in &pred[b] {
                    if idom[p] == none {
                        continue;
                    }
                    new = if new == none {
                        p
                    } else {
                        let (mut x, mut y) = (p, new);
                        while x != y {
                            while po[x] < po[y] {
                                x = idom[x];
                            }
                            while po[y] < po[x] {
                                y = idom[y];
                            }
                        }
                        x
                    };
                }
                if new != idom[b] {
                    idom[b] = new;
                    changed = true;
                }
            }
        }
        let dominates = |h: usize, mut b: usize| loop {
            if b == h {
                return true;
            }
            if idom[b] == none || idom[b] == b {
                return false;
            }
            b = idom[b];
        };
        // Natural loops, merged by header.
        let mut bodies: HashMap<usize, Bits> = HashMap::default();
        for b in 0..nb {
            if !seen[b] {
                continue;
            }
            for &h in &self.succ[b] {
                if !dominates(h, b) {
                    continue;
                }
                let body = bodies.entry(h).or_insert_with(|| Bits::new(nb));
                body.set(h);
                let mut work = vec![b];
                while let Some(x) = work.pop() {
                    if body.has(x) {
                        continue;
                    }
                    body.set(x);
                    work.extend(pred[x].iter().copied().filter(|&p| seen[p]));
                }
            }
        }
        // Loops innermost first; each block's innermost loop.
        let mut heads: Vec<(Vec<usize>, usize)> = bodies
            .iter()
            .map(|(&h, body)| {
                let mut m: Vec<usize> = body.iter().collect();
                m.sort_unstable_by_key(|&b| std::cmp::Reverse(po[b]));
                (m, h)
            })
            .collect();
        heads.sort_unstable_by_key(|x| (x.0.len(), x.1));
        let mut inner = vec![none; nb];
        for (m, h) in heads.iter().rev() {
            for &b in m {
                inner[b] = *h;
            }
        }
        let probs = self.probs(f, &inner, &bodies);
        // Each header's body.
        let mut body_of: Vec<Option<&Bits>> = vec![None; nb];
        for (h, body) in &bodies {
            body_of[*h] = Some(body);
        }
        let back = |p: usize, s: usize| body_of[s].is_some_and(|x| x.has(p));
        // How often each loop runs its header per entry.
        let mut mult = vec![1.0f64; nb];
        let mut local = vec![0.0f64; nb];
        for (m, h) in &heads {
            for &b in m {
                local[b] = 0.0;
            }
            local[*h] = 1.0;
            let mut cyc = 0.0;
            for &b in m {
                if b != *h {
                    local[b] *= mult[b];
                }
                let x = local[b];
                for (k, &s) in self.succ[b].iter().enumerate() {
                    if s == *h {
                        cyc += x * probs[b][k];
                    } else if body_of[*h].is_some_and(|x| x.has(s)) && !back(b, s) {
                        local[s] += x * probs[b][k];
                    }
                }
            }
            mult[*h] = 1.0 / (1.0 - cyc.min(1.0 - 1.0 / LOOP_MAX));
        }
        let mut freq = vec![0.0f64; nb];
        if nb > 0 {
            freq[0] = 1.0;
        }
        for &b in post.iter().rev() {
            freq[b] *= mult[b];
            let x = freq[b];
            for (k, &s) in self.succ[b].iter().enumerate() {
                if !back(b, s) {
                    freq[s] += x * probs[b][k];
                }
            }
        }
        let loops = heads
            .into_iter()
            .map(|(_, h)| (h, bodies.remove(&h).unwrap_or_else(|| Bits::new(nb))))
            .collect();
        (freq, loops)
    }

    /// The probability of each edge out of each block, by static
    /// heuristics (Ball, Larus; combined as Wu, Larus do): rarely run
    /// paths (cold, or only ever reaching a trap) are not taken, nor a
    /// loop's exit, a block with a call or one returning against one that
    /// does not.
    pub(crate) fn probs(
        &self,
        f: &Func,
        inner: &[usize],
        bodies: &HashMap<usize, Bits>,
    ) -> Vec<[f64; 2]> {
        let nb = self.starts.len();
        let mut cold = Bits::new(f.labels as usize);
        for &l in &f.cold {
            if (l as usize) < f.labels as usize {
                cold.set(l as usize);
            }
        }
        let mut dies = vec![false; nb];
        let mut rare = vec![false; nb];
        let mut call = vec![false; nb];
        let mut ret = vec![false; nb];
        for b in 0..nb {
            let (s, e) = (self.starts[b], self.end(b));
            if e <= s {
                continue;
            }
            dies[b] = matches!(f.insts[e - 1], Inst::Trap);
            ret[b] = matches!(f.insts[e - 1], Inst::Ret { .. });
            rare[b] = matches!(f.insts[s], Inst::Label(l) if (l as usize) < f.labels as usize && cold.has(l as usize));
            for i in &f.insts[s..e] {
                if let Inst::Call { callee, .. } = i {
                    call[b] = true;
                    rare[b] |= matches!(callee, Callee::Sym(x) if f.cold_fns.contains(x));
                }
            }
        }
        // Blocks that only ever reach a trap.
        let mut changed = true;
        while changed {
            changed = false;
            for b in (0..nb).rev() {
                if !dies[b] && !self.succ[b].is_empty() && self.succ[b].iter().all(|&s| dies[s]) {
                    dies[b] = true;
                    changed = true;
                }
            }
        }
        let mut npred = vec![0u32; nb];
        for ss in &self.succ {
            for &x in ss {
                npred[x] += 1;
            }
        }
        // A call on the way from `b`, before anything joins it.
        let calls = |mut b: usize| {
            for _ in 0..4 {
                if call[b] {
                    return true;
                }
                let mut on = self.succ[b].iter().filter(|&&x| !dies[x]);
                match (on.next(), on.next()) {
                    (Some(&x), None) if npred[x] == 1 => b = x,
                    _ => return false,
                }
            }
            false
        };
        // Straight on to a return.
        let returns = |mut b: usize| {
            for _ in 0..4 {
                if ret[b] {
                    return true;
                }
                match self.succ[b][..] {
                    [s] => b = s,
                    _ => return false,
                }
            }
            false
        };
        let ds = |p: f64, q: f64| p * q / p.mul_add(q, (1.0 - p) * (1.0 - q));
        let mut out = vec![[1.0, 0.0]; nb];
        for b in 0..nb {
            let [s0, s1] = self.succ[b][..] else {
                continue;
            };
            if s0 == s1 {
                out[b] = [0.5, 0.5];
                continue;
            }
            let (r0, r1) = (rare[s0] || dies[s0], rare[s1] || dies[s1]);
            let p = if r0 == r1 {
                let mut p = 0.5;
                if let Some(body) = bodies.get(&inner[b]) {
                    let (i0, i1) = (body.has(s0), body.has(s1));
                    if i0 != i1 {
                        p = ds(p, if i0 { 0.88 } else { 0.12 });
                    }
                }
                // A call on one side only, where the other side does not
                // just go on to it (the call then runs either way).
                let joins = |a: usize, c: usize| self.succ[a][..] == [c];
                let (c0, c1) = (calls(s0), calls(s1));
                if c0 != c1 && !(c0 && joins(s1, s0)) && !(c1 && joins(s0, s1)) {
                    p = ds(p, if c0 { 0.22 } else { 0.78 });
                }
                let (t0, t1) = (returns(s0), returns(s1));
                if t0 != t1 {
                    p = ds(p, if t0 { 0.28 } else { 0.72 });
                }
                p
            } else if r0 {
                RARE
            } else {
                1.0 - RARE
            };
            out[b] = [p, 1.0 - p];
        }
        out
    }
}

/// Block frequencies of a function in [`UNIT`]s by block key
/// (`Cfg::keys`): computed once, read by every pass after.
pub struct Freq(HashMap<(u32, u32), u64>);

impl Freq {
    #[must_use]
    pub fn of(f: &Func) -> Self {
        let cfg = Cfg::new(f);
        let (bf, _) = cfg.block_freqs(f);
        let keys = cfg.keys(f);
        Self(
            keys.into_iter()
                .zip(bf)
                .map(|(k, x)| (k, ((x.min(MAX_RUNS) * UNIT as f64) as u64).max(1)))
                .collect(),
        )
    }

    /// How often each instruction of `f` runs (`Cfg::freqs`); a block
    /// the passes made, as often as the one before.
    #[must_use]
    pub fn at(&self, f: &Func, cfg: &Cfg) -> Vec<u64> {
        let mut out = vec![UNIT; cfg.n];
        let mut last = UNIT;
        for (b, k) in cfg.keys(f).into_iter().enumerate() {
            let w = self.0.get(&k).copied().unwrap_or(last);
            last = w;
            for y in &mut out[cfg.starts[b]..cfg.end(b)] {
                *y = w;
            }
        }
        out
    }
}
