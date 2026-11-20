//! Candidates, bit sets and liveness of a function's parts.

#[allow(clippy::wildcard_imports)]
use crate::regalloc::*;

/// A fixed-size bit set.
#[derive(Clone, PartialEq, Eq)]
pub struct Bits(pub Vec<u64>);

impl Bits {
    pub fn new(n: usize) -> Self {
        Self(vec![0; n.div_ceil(64)])
    }
    pub fn set(&mut self, i: usize) {
        self.0[i / 64] |= 1 << (i % 64);
    }
    pub fn clear(&mut self, i: usize) {
        self.0[i / 64] &= !(1 << (i % 64));
    }
    pub fn has(&self, i: usize) -> bool {
        self.0[i / 64] & (1 << (i % 64)) != 0
    }
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        ones(&self.0)
    }
}

/// The set bits of `words`, in order.
pub(crate) fn ones(words: &[u64]) -> impl Iterator<Item = usize> + '_ {
    words.iter().enumerate().flat_map(|(w, &x)| {
        let mut x = x;
        std::iter::from_fn(move || {
            if x == 0 {
                return None;
            }
            let t = x.trailing_zeros() as usize;
            x &= x - 1;
            Some(w * 64 + t)
        })
    })
}

/// A bit set per block, in one buffer.
pub struct Rows {
    w: usize,
    d: Vec<u64>,
}

impl Rows {
    pub fn new(n: usize, bits: usize) -> Self {
        let w = bits.div_ceil(64);
        Self {
            w,
            d: vec![0; n * w],
        }
    }
    pub fn row(&self, b: usize) -> &[u64] {
        &self.d[b * self.w..(b + 1) * self.w]
    }
    pub fn row_mut(&mut self, b: usize) -> &mut [u64] {
        &mut self.d[b * self.w..(b + 1) * self.w]
    }
    pub fn set(&mut self, b: usize, i: usize) {
        self.d[b * self.w + i / 64] |= 1 << (i % 64);
    }
    pub fn clear(&mut self, b: usize, i: usize) {
        self.d[b * self.w + i / 64] &= !(1 << (i % 64));
    }
    pub fn has(&self, b: usize, i: usize) -> bool {
        self.d[b * self.w + i / 64] & (1 << (i % 64)) != 0
    }
    pub fn iter(&self, b: usize) -> impl Iterator<Item = usize> + '_ {
        ones(self.row(b))
    }
    /// Row `b` as a set of its own.
    pub(crate) fn bits(&self, b: usize) -> Bits {
        Bits(self.row(b).to_vec())
    }
}

/// A candidate: `((slot, offset), size, float)`.
pub(crate) type Cand = ((Slot, u32), u32, bool);

/// The candidates of `f`, and the slots whose every part is one.
/// Every instruction's memory operands (`operands`), packed.
pub(crate) struct Ops {
    items: Vec<(Mem, u32, Class, Acc)>,
    ends: Vec<u32>,
}

impl Ops {
    pub(crate) fn of(f: &Func, ptr: u32) -> Self {
        let mut items = Vec::with_capacity(f.insts.len() * 2);
        let mut ends = Vec::with_capacity(f.insts.len());
        for i in &f.insts {
            operands(i, ptr, &mut items);
            ends.push(items.len() as u32);
        }
        Self { items, ends }
    }

    pub(crate) fn at(&self, i: usize) -> &[(Mem, u32, Class, Acc)] {
        let s = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.items[s..self.ends[i] as usize]
    }
}

pub(crate) fn candidates(f: &Func, regs: &Regs, ops: &Ops) -> (Vec<Cand>, Vec<bool>) {
    let mut opaque = vec![false; f.slots.len()];
    // Every scalar access: `(slot, offset, size, float, int)`.
    let mut acc: Vec<(Slot, u32, u32, bool, bool)> = Vec::with_capacity(f.insts.len() * 2);
    for at in 0..f.insts.len() {
        for &(m, size, c, a) in ops.at(at) {
            match (m, a) {
                (Mem::Frame(s, _), Acc::Whole) => opaque[s as usize] = true,
                (Mem::Frame(s, o), _) => {
                    acc.push((
                        s,
                        o,
                        size,
                        matches!(c, Class::Fp | Class::Vec),
                        matches!(c, Class::Int),
                    ));
                }
                (Mem::Ind(s, _), _) => acc.push((s, 0, regs.ptr, false, true)),
            }
        }
    }
    for v in f.debug.iter().filter(|_| !f.track).flat_map(|d| &d.vars) {
        opaque[v.slot as usize] = true;
    }
    // A part a call or return moves through no register `Regs` names (an
    // x87 result) is reached in memory.
    let mut odd = |m: &Mem, loc: Loc, ret: bool| {
        if let Mem::Frame(s, _) = m {
            // A vector moves through memory: 16 bytes no allocated part is.
            let vector = matches!(loc, Loc::Vec(_) | Loc::VecStack(_));
            if vector
                || (!matches!(loc, Loc::Stack(_) | Loc::Area(_))
                    && loc_reg(regs, loc, ret).is_none())
            {
                opaque[*s as usize] = true;
            }
        }
    };
    for i in &f.insts {
        match i {
            Inst::Call { args, rets, .. } => {
                for (p, v) in args {
                    if let V::M(m) = v {
                        odd(m, p.loc, false);
                    }
                }
                for (p, m) in rets {
                    odd(m, p.loc, true);
                }
            }
            Inst::Ret { vals } => {
                for (p, v) in vals {
                    if let V::M(m) = v {
                        odd(m, p.loc, true);
                    }
                }
            }
            Inst::Param { part, dst } if part.vector() => odd(dst, part.loc, false),
            _ => {}
        }
    }
    // By slot (a counting sort: slots are dense), then by offset.
    let mut start = vec![0u32; f.slots.len() + 1];
    for a in &acc {
        start[a.0 as usize + 1] += 1;
    }
    for i in 1..start.len() {
        start[i] += start[i - 1];
    }
    let mut next = start.clone();
    let mut sorted = vec![(0, 0, 0, false, false); acc.len()];
    for a in &acc {
        let k = &mut next[a.0 as usize];
        sorted[*k as usize] = *a;
        *k += 1;
    }
    for w in start.windows(2) {
        sorted[w[0] as usize..w[1] as usize].sort_unstable_by_key(|a| a.1);
    }
    let acc = sorted;
    let mut out = Vec::new();
    let mut full = vec![false; f.slots.len()];
    // The parts of one slot: `(offset, size, float, int)`.
    let mut ps: Vec<(u32, u32, bool, bool)> = Vec::new();
    let mut at = 0;
    while at < acc.len() {
        let s = acc[at].0;
        ps.clear();
        let mut bad = opaque[s as usize];
        while at < acc.len() && acc[at].0 == s {
            let (_, o, size, fk, ik) = acc[at];
            match ps.last_mut() {
                Some(p) if p.0 == o => {
                    bad |= p.1 != size;
                    p.2 |= fk;
                    p.3 |= ik;
                }
                _ => ps.push((o, size, fk, ik)),
            }
            at += 1;
        }
        let clean = !bad && ps.windows(2).all(|w| w[0].0 + w[0].1 <= w[1].0);
        if !clean {
            continue;
        }
        full[s as usize] = true;
        for &(o, size, fk, ik) in &ps {
            let fp = fk
                && !ik
                && (size <= regs.fp_size && matches!(size, 4 | 8)
                    || size <= regs.vec && matches!(size, 16 | 32));
            let int = !fk && matches!(size, 1 | 2 | 4 | 8) && size <= regs.word;
            let pair = regs.pairs && !fk && size == 2 * regs.word;
            if fp || int || pair {
                out.push(((s, o), size, fp));
            } else {
                full[s as usize] = false;
            }
        }
    }
    (out, full)
}

/// A list per instruction, packed.
pub(crate) struct Lists {
    items: Vec<usize>,
    /// Where each list ends in `items`.
    ends: Vec<u32>,
}

impl Lists {
    pub(crate) fn with_capacity(n: usize) -> Self {
        Self {
            items: Vec::with_capacity(n),
            ends: Vec::with_capacity(n),
        }
    }

    /// Close the list of the next instruction.
    pub(crate) fn end(&mut self) {
        self.ends.push(self.items.len() as u32);
    }
}

impl std::ops::Index<usize> for Lists {
    type Output = [usize];

    fn index(&self, at: usize) -> &[usize] {
        let s = if at == 0 {
            0
        } else {
            self.ends[at - 1] as usize
        };
        &self.items[s..self.ends[at] as usize]
    }
}

/// Liveness of a function's candidates.
pub(crate) struct Live {
    pub(crate) cands: Vec<Cand>,
    pub(crate) full: Vec<bool>,
    /// Candidates each instruction reads (a pointer through which it
    /// reaches memory included) and writes.
    pub(crate) uses: Lists,
    pub(crate) defs: Lists,
    pub(crate) cfg: Cfg,
    pub(crate) live_in: Rows,
    pub(crate) live_out: Rows,
}

pub(crate) fn live(f: &Func, regs: &Regs) -> Live {
    let ops = Ops::of(f, regs.ptr);
    let (cands, full) = candidates(f, regs, &ops);
    live_of(f, &ops, cands, full)
}

pub(crate) fn live_of(f: &Func, ops: &Ops, cands: Vec<Cand>, full: Vec<bool>) -> Live {
    // Per slot, its candidates' range in `cands` (`candidates` gives
    // them by slot); else by `(slot, offset)`.
    let by_slot = cands.windows(2).all(|w| w[0].0 < w[1].0);
    let mut first = Vec::new();
    let mut index: HashMap<(Slot, u32), usize> = HashMap::default();
    if by_slot {
        first = vec![0u32; f.slots.len() + 1];
        for c in &cands {
            if let Some(x) = first.get_mut(c.0.0 as usize + 1) {
                *x += 1;
            }
        }
        for i in 1..first.len() {
            first[i] += first[i - 1];
        }
    } else {
        index = cands.iter().enumerate().map(|(i, c)| (c.0, i)).collect();
    }
    let nv = cands.len();
    let n = f.insts.len();
    let vreg = |m: Mem| -> Option<usize> {
        let Mem::Frame(s, o) = m else { return None };
        if !by_slot {
            return index.get(&(s, o)).copied();
        }
        let (a, b) = (*first.get(s as usize)?, *first.get(s as usize + 1)?);
        (a as usize..b as usize).find(|&v| cands[v].0.1 == o)
    };
    let mut uses = Lists::with_capacity(n);
    let mut defs = Lists::with_capacity(n);
    for at in 0..n {
        for &(m, _, _, acc) in ops.at(at) {
            if let Mem::Ind(s, _) = m {
                if let Some(v) = vreg(Mem::Frame(s, 0)) {
                    uses.items.push(v);
                }
            } else if let Some(v) = vreg(m) {
                if acc == Acc::Write {
                    defs.items.push(v);
                } else {
                    uses.items.push(v);
                }
            }
        }
        uses.end();
        defs.end();
    }
    let cfg = Cfg::new(f);
    let nb = cfg.starts.len();
    // Block summaries, then live-in / live-out to a fixpoint.
    let mut gen_ = Rows::new(nb, nv);
    let mut kill = Rows::new(nb, nv);
    for b in 0..nb {
        for at in (cfg.starts[b]..cfg.end(b)).rev() {
            for &d in &defs[at] {
                kill.set(b, d);
                gen_.clear(b, d);
            }
            for &x in &uses[at] {
                gen_.set(b, x);
            }
        }
    }
    let mut live_in = Rows::new(nb, nv);
    let mut live_out = Rows::new(nb, nv);
    let mut out = vec![0u64; nv.div_ceil(64)];
    let mut inn = out.clone();
    // To the fixpoint, revisiting only the predecessors of a block whose
    // live-in changed.
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); nb];
    for (b, ss) in cfg.succ.iter().enumerate() {
        for &s in ss {
            preds[s].push(b);
        }
    }
    let mut work: Vec<usize> = (0..nb).collect();
    let mut queued = vec![true; nb];
    while let Some(b) = work.pop() {
        queued[b] = false;
        out.fill(0);
        for &s in &cfg.succ[b] {
            for (o, x) in out.iter_mut().zip(live_in.row(s)) {
                *o |= x;
            }
        }
        live_out.row_mut(b).copy_from_slice(&out);
        for (w, (o, (g, k))) in inn
            .iter_mut()
            .zip(out.iter().zip(gen_.row(b).iter().zip(kill.row(b))))
        {
            *w = (*o & !k) | g;
        }
        if inn != live_in.row(b) {
            live_in.row_mut(b).copy_from_slice(&inn);
            for &p in &preds[b] {
                if !queued[p] {
                    queued[p] = true;
                    work.push(p);
                }
            }
        }
    }
    Live {
        cands,
        full,
        uses,
        defs,
        cfg,
        live_in,
        live_out,
    }
}

/// Sorted, disjoint `[from, to]` instruction ranges.
pub type Ranges = Vec<(usize, usize)>;

pub fn overlap(a: &Ranges, b: &Ranges) -> bool {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].1 < b[j].0 {
            i += 1;
        } else if b[j].1 < a[i].0 {
            j += 1;
        } else {
            return true;
        }
    }
    false
}

pub fn merge(mut r: Ranges) -> Ranges {
    r.sort_unstable();
    let mut out: Ranges = Vec::with_capacity(r.len());
    for x in r {
        match out.last_mut() {
            Some(l) if x.0 <= l.1 + 1 => l.1 = l.1.max(x.1),
            _ => out.push(x),
        }
    }
    out
}

/// `merge` of two merged range lists.
pub(crate) fn union(a: &Ranges, b: &Ranges) -> Ranges {
    let mut out: Ranges = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        let x = if j == b.len() || (i < a.len() && a[i] <= b[j]) {
            i += 1;
            a[i - 1]
        } else {
            j += 1;
            b[j - 1]
        };
        match out.last_mut() {
            Some(l) if x.0 <= l.1 + 1 => l.1 = l.1.max(x.1),
            _ => out.push(x),
        }
    }
    out
}

/// Each candidate's live ranges (an instruction's operands and results
/// overlap), whether it lives across a call, and the integer registers
/// `clobber` says the instructions of its ranges take.
pub(crate) fn ranges(
    f: &Func,
    lv: &Live,
    clobber: Option<&dyn Fn(&Inst) -> u64>,
) -> (Vec<Ranges>, Vec<bool>, Vec<u64>) {
    let Live {
        cands,
        uses,
        defs,
        cfg,
        live_in,
        live_out,
        ..
    } = lv;
    let nv = cands.len();
    let nb = cfg.starts.len();
    let mut ranges: Vec<Ranges> = vec![Vec::new(); nv];
    let mut crosses = vec![false; nv];
    let mut kill = vec![0u64; nv];
    let mut open = vec![usize::MAX; nv];
    let mut opened: Vec<usize> = Vec::new();
    for b in 0..nb {
        let (s, e) = (cfg.starts[b], cfg.end(b));
        if e <= s {
            continue;
        }
        for v in live_out.iter(b) {
            open[v] = e - 1;
            opened.push(v);
        }
        for at in (s..e).rev() {
            let k = clobber.map_or(0, |c| c(&f.insts[at]));
            if k != 0 {
                for &v in defs[at].iter().chain(&uses[at]) {
                    kill[v] |= k;
                }
                for &v in &opened {
                    if open[v] != usize::MAX {
                        kill[v] |= k;
                    }
                }
            }
            for &d in &defs[at] {
                if open[d] == usize::MAX {
                    ranges[d].push((at, at));
                } else {
                    ranges[d].push((at, open[d]));
                    open[d] = usize::MAX;
                }
            }
            if clobbers(&f.insts[at]) {
                for &v in &opened {
                    if open[v] != usize::MAX {
                        crosses[v] = true;
                    }
                }
            }
            for &x in &uses[at] {
                if open[x] == usize::MAX {
                    open[x] = at;
                    opened.push(x);
                }
            }
        }
        for v in std::mem::take(&mut opened) {
            if open[v] != usize::MAX {
                ranges[v].push((s, open[v]));
                open[v] = usize::MAX;
            }
        }
        debug_assert!(live_in.iter(b).all(|v| ranges[v].iter().any(|r| r.0 == s)));
    }
    (ranges.into_iter().map(merge).collect(), crosses, kill)
}
