//! Slot parts promoted to locals; variable locations under optimized -g.

#[allow(clippy::wildcard_imports)]
use crate::isa::wasm::*;

/// A slot part: its size, its kind, whether accesses disagree on it.
pub(crate) type Part3 = (u32, Option<K>, bool);

/// Scalar parts of slots that may live in locals: `(slot, offset)` ->
/// `(size, kinds seen)`.
pub(crate) fn promotable(f: &Func) -> HashMap<(u32, u32), (u32, Option<K>)> {
    #[derive(Default, Clone)]
    struct U {
        opaque: bool,
        parts: std::collections::BTreeMap<u32, Part3>,
    }
    fn part(u: &mut [U], m: Mem, size: u32, k: Option<K>) {
        match m {
            Mem::Frame(s, o) => {
                let u = &mut u[s as usize];
                let p = u.parts.entry(o).or_insert((size, k, false));
                if p.0 != size {
                    u.opaque = true;
                }
                // Kinds agree, else the part is held as an integer.
                if p.1 != k {
                    p.2 = true;
                }
            }
            Mem::Ind(s, _) => part(u, Mem::Frame(s, 0), 4, Some(K::I32)),
        }
    }
    fn v(u: &mut [U], x: V, k: K) {
        if let V::M(m) = x {
            part(u, m, k.size(), Some(k));
        }
    }
    fn whole(u: &mut [U], m: Mem) {
        match m {
            Mem::Frame(s, _) => u[s as usize].opaque = true,
            Mem::Ind(s, _) => part(u, Mem::Frame(s, 0), 4, Some(K::I32)),
        }
    }
    let mut u = vec![U::default(); f.slots.len()];
    for i in &f.insts {
        match i {
            Inst::Label(_)
            | Inst::Jmp(_)
            | Inst::Trap
            | Inst::Line(..)
            | Inst::Sym { .. }
            | Inst::Tls { .. } => {}
            Inst::VLoop(l) => whole(&mut u, Mem::Frame(l.io, 0)),
            Inst::V(x) => {
                for m in x.srcs() {
                    whole(&mut u, m);
                }
                if let Some((m, k)) = x.scalar() {
                    v(&mut u, V::M(m), k);
                }
                match x.scalar_dst() {
                    Some(k) => part(&mut u, x.dst, k.size(), Some(k)),
                    None => whole(&mut u, x.dst),
                }
            }
            Inst::Br { c, .. } => v(&mut u, *c, K::I8),
            Inst::Mov { k, dst, src } => {
                v(&mut u, *src, *k);
                part(&mut u, *dst, k.size(), Some(*k));
            }
            Inst::Copy { dst, src, size } => {
                if scalar(*size).is_some() {
                    part(&mut u, *src, *size, None);
                    part(&mut u, *dst, *size, None);
                } else {
                    whole(&mut u, *src);
                    whole(&mut u, *dst);
                }
            }
            Inst::Zero { dst, .. } | Inst::Addr { src: dst, .. } => whole(&mut u, *dst),
            Inst::Param { part: p, dst } => part(&mut u, *dst, p.k.size(), Some(p.k)),
            Inst::Bin { k, a, b, .. } | Inst::Ovf { k, a, b, .. } | Inst::Cmp { k, a, b, .. } => {
                v(&mut u, *a, *k);
                v(&mut u, *b, *k);
            }
            Inst::Un { k, a, .. } => v(&mut u, *a, *k),
            Inst::Fma { k, a, b, c, .. } => {
                v(&mut u, *a, *k);
                v(&mut u, *b, *k);
                v(&mut u, *c, *k);
            }
            Inst::Conv { from, a, .. } => v(&mut u, *a, *from),
            Inst::AtomicLoad { ptr, .. } => whole(&mut u, *ptr),
            Inst::AtomicStore { k, ptr, src, .. } => {
                whole(&mut u, *ptr);
                v(&mut u, *src, *k);
            }
            Inst::AtomicRmw { k, ptr, val, .. } => {
                whole(&mut u, *ptr);
                v(&mut u, *val, *k);
            }
            Inst::Cas {
                k,
                ptr,
                expected,
                new,
                ..
            } => {
                whole(&mut u, *ptr);
                v(&mut u, *expected, *k);
                v(&mut u, *new, *k);
            }
            Inst::Call {
                callee,
                args,
                rets,
                sret,
                ..
            } => {
                if let Callee::Ptr(m) = callee {
                    part(&mut u, *m, 4, Some(K::I32));
                }
                for (p, x) in args {
                    v(&mut u, *x, p.k);
                }
                for (p, m) in rets {
                    part(&mut u, *m, p.k.size(), Some(p.k));
                }
                if let Some(m) = sret {
                    whole(&mut u, *m);
                }
            }
            Inst::Ret { vals } => {
                for (p, x) in vals {
                    v(&mut u, *x, p.k);
                }
            }
        }
        // Destinations of the computing instructions.
        match i {
            Inst::Sym { dst, .. } | Inst::Tls { dst, .. } | Inst::Addr { dst, .. } => {
                part(&mut u, *dst, 4, Some(K::I32));
            }
            Inst::Bin { k, dst, .. }
            | Inst::Ovf { k, dst, .. }
            | Inst::Un { k, dst, .. }
            | Inst::Fma { k, dst, .. } => {
                part(&mut u, *dst, k.size(), Some(*k));
            }
            Inst::Cmp { dst, .. } => part(&mut u, *dst, 1, Some(K::I8)),
            Inst::Conv { to, dst, .. } => part(&mut u, *dst, to.size(), Some(*to)),
            Inst::AtomicLoad { k, dst, .. }
            | Inst::AtomicRmw { k, dst, .. }
            | Inst::Cas { k, dst, .. } => part(&mut u, *dst, k.size(), Some(*k)),
            _ => {}
        }
    }
    // At `-O0` each variable keeps its slot; above, a variable's parts
    // in locals are described as such.
    for d in f.debug.iter().filter(|_| !f.opt).flat_map(|d| &d.vars) {
        u[d.slot as usize].opaque = true;
    }
    let mut out = HashMap::default();
    for (s, x) in u.iter().enumerate() {
        let ps: Vec<(u32, Part3)> = x.parts.iter().map(|(o, p)| (*o, *p)).collect();
        let clean = !x.opaque
            && ps.windows(2).all(|w| w[0].0 + w[0].1.0 <= w[1].0)
            && ps.iter().all(|p| matches!(p.1.0, 1 | 2 | 4 | 8));
        if clean {
            for (o, (size, k, mixed)) in ps {
                out.insert((s as u32, o), (size, if mixed { None } else { k }));
            }
        }
    }
    out
}

/// Slot parts in locals (`Em::regs`).
pub(crate) type Regs = HashMap<(u32, u32), Reg>;

/// Optimized `-g` (`Func::track`): `rows` as line rows, each at the
/// code of the first instruction emitted from where it goes on (`marks`
/// in emission order); a row where the one before is replaces it.
pub(crate) fn rows_at(
    rows: &[(usize, u32, u32, u32)],
    marks: &[(u32, usize)],
) -> Vec<(u32, u32, u32, u32)> {
    let mut by_inst: Vec<(usize, u32)> = marks.iter().map(|&(at, i)| (i, at)).collect();
    by_inst.sort_unstable();
    let mut out: Vec<(u32, u32, u32, u32)> = rows
        .iter()
        .filter_map(|&(k, l, c, s)| {
            let n = by_inst.partition_point(|x| x.0 < k);
            by_inst.get(n).map(|x| (x.1, l, c, s))
        })
        .collect();
    out.sort_by_key(|r| r.0);
    let mut lines: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(out.len());
    for r in out {
        if lines.last().is_some_and(|x| x.0 == r.0) {
            lines.pop();
        }
        lines.push(r);
    }
    lines
}

/// Optimized `-g`: where each variable is over the code (`shift` past
/// the locals' declarations): a part in a local over its live range
/// (`marks` place the instructions), else the variable's slot while its
/// memory holds the value (`live`: over those instructions; none listed:
/// all of them), a constant, or the pieces a value built in place is.
pub(crate) fn var_locals(
    f: &Func,
    (regs, sizes): (&Regs, &HashMap<(u32, u32), u32>),
    (slot_off, live): (&[u32], &HashMap<crate::lir::Slot, crate::regalloc::Ranges>),
    marks: &[(u32, usize)],
    (len, shift): (u32, u32),
) -> Vec<crate::debug::locs::Spans> {
    let Some(d) = &f.debug else {
        return Vec::new();
    };
    let end = len + shift;
    // The slots variables are in.
    let named: HashSet<u32> = d
        .vars
        .iter()
        .flat_map(|v| {
            v.parts
                .iter()
                .filter_map(|p| match p.2 {
                    PartSrc::Slot(s) => Some(s),
                    _ => None,
                })
                .chain(std::iter::once(v.slot))
        })
        .collect();
    let mut parts: Vec<(u32, u32)> = regs
        .keys()
        .copied()
        .filter(|p| named.contains(&p.0))
        .collect();
    parts.sort_unstable();
    let ranges = crate::regalloc::part_ranges(f, &REGS, &parts);
    // Each instruction's code, in code order.
    let mut seg: Vec<(u32, u32, usize)> = Vec::with_capacity(marks.len());
    for (k, &(at, i)) in marks.iter().enumerate() {
        let hi = marks.get(k + 1).map_or(len, |n| n.0);
        if at < hi {
            seg.push((at + shift, hi + shift, i));
        }
    }
    seg.sort_unstable();
    // The code over which `r` holds, once the instruction setting it
    // (`set`) is done.
    let code = |r: &[(usize, usize)], set: &dyn Fn(&Inst) -> bool| {
        let mut out: Vec<(u32, u32)> = Vec::new();
        for &(a, b, i) in &seg {
            let on = r
                .iter()
                .any(|&(x, y)| x <= i && i <= y && !(i == x && set(&f.insts[i])));
            if !on {
                continue;
            }
            match out.last_mut() {
                Some(l) if l.1 == a => l.1 = b,
                _ => out.push((a, b)),
            }
        }
        out
    };
    // A part's code: its local holds it once the instruction setting it
    // is done.
    let held: Vec<Vec<(u32, u32)>> = parts
        .iter()
        .zip(&ranges)
        .map(|(&p, r)| code(r, &|i| crate::regalloc::writes(i, p)))
        .collect();
    // Slots some instruction reaches.
    let mut used = vec![false; f.slots.len()];
    let mut ops = Vec::new();
    for i in &f.insts {
        ops.clear();
        crate::regalloc::mems(i, 4, &mut ops);
        for m in &ops {
            if let Mem::Frame(s, _) = m {
                used[*s as usize] = true;
            }
        }
    }
    let slot_spans = |s: u32| -> Spans {
        let mine: Vec<usize> = (0..parts.len()).filter(|&k| parts[k].0 == s).collect();
        if mine.is_empty() {
            let Some(&off) = slot_off.get(s as usize).filter(|_| used[s as usize]) else {
                return Vec::new();
            };
            let place = Place::Frame(off as i32);
            return match live.get(&s) {
                None => vec![(0, end, place)],
                Some(r) => code(r, &|i| crate::regalloc::writes_slot(i, s))
                    .into_iter()
                    .map(|(a, b)| (a, b, place.clone()))
                    .collect(),
            };
        }
        let size = f.slots[s as usize].0;
        let mut cuts = vec![0, end];
        for &k in &mine {
            for x in &held[k] {
                cuts.extend([x.0, x.1]);
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut out: Spans = Vec::new();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let mut pieces: Vec<(u32, Option<Place>)> = Vec::new();
            let mut at = 0;
            for &k in &mine {
                let (o, n) = (parts[k].1, sizes.get(&parts[k]).copied().unwrap_or(0));
                if o < at || n == 0 {
                    continue;
                }
                if o > at {
                    pieces.push((o - at, None));
                }
                let here = held[k].iter().any(|&(x, y)| x <= a && b <= y);
                pieces.push((n, here.then(|| Place::Local(regs[&parts[k]].local))));
                at = o + n;
            }
            if at < size {
                pieces.push((size - at, None));
            }
            if pieces.iter().all(|p| p.1.is_none()) {
                continue;
            }
            let place = match &pieces[..] {
                [(_, Some(p))] => p.clone(),
                _ => Place::Pieces(pieces),
            };
            match out.last_mut() {
                Some(l) if l.1 == a && l.2 == place => l.1 = b,
                _ => out.push((a, b, place)),
            }
        }
        out
    };
    d.vars
        .iter()
        .map(|v| {
            if v.slot != NO_SLOT {
                return slot_spans(v.slot);
            }
            if v.parts.is_empty() {
                return v
                    .konst
                    .map_or_else(Vec::new, |c| vec![(0, end, Place::Const(c))]);
            }
            // A value built in place: each part where it is, a piece.
            let each: Vec<Spans> = v
                .parts
                .iter()
                .map(|p| match p.2 {
                    PartSrc::Slot(s) => slot_spans(s),
                    PartSrc::Const(c) => vec![(0, end, Place::Const(c))],
                    PartSrc::Gone => Vec::new(),
                })
                .collect();
            let mut cuts: Vec<u32> = vec![0, end];
            for sp in &each {
                for x in sp {
                    cuts.extend([x.0, x.1]);
                }
            }
            cuts.sort_unstable();
            cuts.dedup();
            let mut out: Spans = Vec::new();
            for w in cuts.windows(2) {
                let (a, b) = (w[0], w[1]);
                let mut pieces: Vec<(u32, Option<Place>)> = Vec::new();
                let mut at = 0;
                for (p, sp) in v.parts.iter().zip(&each) {
                    if p.0 < at {
                        continue;
                    }
                    if p.0 > at {
                        pieces.push((p.0 - at, None));
                    }
                    match sp.iter().find(|x| x.0 <= a && b <= x.1).map(|x| &x.2) {
                        Some(Place::Pieces(inner)) => pieces.extend(inner.iter().cloned()),
                        q => pieces.push((p.1, q.cloned())),
                    }
                    at = p.0 + p.1;
                }
                if pieces.iter().all(|p| p.1.is_none()) {
                    continue;
                }
                let place = Place::Pieces(pieces);
                match out.last_mut() {
                    Some(l) if l.1 == a && l.2 == place => l.1 = b,
                    _ => out.push((a, b, place)),
                }
            }
            out
        })
        .collect()
}
