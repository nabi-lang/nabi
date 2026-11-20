//! Putting source positions back after the passes reorder code.

#[allow(clippy::wildcard_imports)]
use crate::regalloc::*;

/// What instructions match by when positions are put back (`anchor`):
/// their kind, and what they jump to or call.
pub(crate) fn shape(i: &Inst) -> (u8, u32) {
    match i {
        Inst::Label(l) => (0, *l),
        Inst::Jmp(l) => (1, *l),
        Inst::Br { t, f, .. } => (2, t ^ f.rotate_left(16)),
        Inst::Call {
            callee: crate::lir::Callee::Sym(s),
            ..
        } => (3, *s),
        Inst::Call { .. } => (3, u32::MAX),
        Inst::Ret { .. } => (4, 0),
        Inst::Param { .. } => (5, 0),
        Inst::Mov { .. } | Inst::Copy { .. } => (6, 0),
        Inst::Zero { .. } => (7, 0),
        Inst::Addr { .. } => (8, 0),
        Inst::Sym { sym, .. } => (9, *sym),
        Inst::Tls { .. } => (10, 0),
        Inst::Bin { op, .. } => (11, *op as u32),
        Inst::Ovf { op, .. } => (12, *op as u32),
        Inst::Un { op, .. } => (13, *op as u32),
        Inst::Cmp { .. } => (14, 0),
        Inst::Conv { .. } => (15, 0),
        Inst::AtomicLoad { .. }
        | Inst::AtomicStore { .. }
        | Inst::AtomicRmw { .. }
        | Inst::Cas { .. } => (16, 0),
        Inst::Trap => (17, 0),
        Inst::Line(..) => (18, 0),
        Inst::VLoop(_) => (19, 0),
        Inst::V(_) => (20, 0),
        Inst::Fma { .. } => (21, 0),
    }
}

/// `[start, end)` of each labelled stretch of `insts` (the first, before
/// any label, under `None`).
pub(crate) fn stretches(insts: &[&Inst]) -> HashMap<Option<Label>, (usize, usize)> {
    let mut out = HashMap::default();
    let mut cur = (None, 0);
    for (at, i) in insts.iter().enumerate() {
        if let Inst::Label(l) = i {
            out.insert(cur.0, (cur.1, at));
            cur = (Some(*l), at);
        }
    }
    out.insert(cur.0, (cur.1, insts.len()));
    out
}

/// For each of `a`, the index of the one of `b` it matches by `shape`,
/// in order (a longest common subsequence; greedy when large).
pub(crate) fn align(a: &[&Inst], b: &[&Inst]) -> Vec<Option<usize>> {
    let ka: Vec<_> = a.iter().map(|i| shape(i)).collect();
    let kb: Vec<_> = b.iter().map(|i| shape(i)).collect();
    let (n, m) = (ka.len(), kb.len());
    let mut out = vec![None; n];
    if n.saturating_mul(m) > 1 << 22 {
        let mut j = 0;
        for (x, k) in ka.iter().enumerate() {
            if let Some(d) = kb[j.min(m)..].iter().take(32).position(|y| y == k) {
                out[x] = Some(j + d);
                j += d + 1;
            }
        }
        return out;
    }
    // Suffix lengths: `t[x][y]` for `a[x..]` and `b[y..]`.
    let w = m + 1;
    let mut t = vec![0u32; (n + 1) * w];
    for x in (0..n).rev() {
        for y in (0..m).rev() {
            t[x * w + y] = if ka[x] == kb[y] {
                t[(x + 1) * w + y + 1] + 1
            } else {
                t[(x + 1) * w + y].max(t[x * w + y + 1])
            };
        }
    }
    let (mut x, mut y) = (0, 0);
    while x < n && y < m {
        if ka[x] == kb[y] {
            out[x] = Some(y);
            x += 1;
            y += 1;
        } else if t[(x + 1) * w + y] >= t[x * w + y + 1] {
            x += 1;
        } else {
            y += 1;
        }
    }
    out
}

/// The positions of `f` (its `Line`s) as rows of `g`, what the passes
/// made of it: each before the instruction of `g` the one after it
/// became, or the next one that survived in its labelled stretch; where
/// none did, at the stretch's end.
pub(crate) fn anchor(f: &Func, g: &Func) -> Vec<(usize, u32, u32, u32)> {
    let mut base: Vec<&Inst> = Vec::with_capacity(f.insts.len());
    let mut rows = Vec::new();
    for i in &f.insts {
        match i {
            Inst::Line(l, c, s) => rows.push((base.len(), *l, *c, *s)),
            i => base.push(i),
        }
    }
    if rows.is_empty() {
        return rows;
    }
    let got: Vec<&Inst> = g.insts.iter().collect();
    let (sa, sb) = (stretches(&base), stretches(&got));
    // Base instruction -> the instruction of `g` the row goes before.
    let mut to: Vec<Option<usize>> = vec![None; base.len() + 1];
    for (l, &(s, e)) in &sa {
        let Some(&(gs, ge)) = sb.get(l) else {
            continue;
        };
        let m = align(&base[s..e], &got[gs..ge]);
        let mut next = ge;
        to[e] = to[e].or(Some(ge));
        for x in (s..e).rev() {
            if let Some(y) = m[x - s] {
                next = gs + y;
            }
            to[x] = Some(next);
        }
    }
    let mut out: Vec<(usize, u32, u32, u32)> = rows
        .into_iter()
        .filter_map(|(k, l, c, s)| Some((to[k]?, l, c, s)))
        .collect();
    out.sort_by_key(|r| r.0);
    out
}
