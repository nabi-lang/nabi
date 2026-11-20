//! `-g` in optimized code (`Func::track`): the positions `regalloc`
//! put back (`Func::rows`) as line rows by code offset, and where each
//! variable is over its function's code: a register while it holds the
//! value, its slot while that does, a constant, or nowhere (optimized
//! out).
//!
//! A variable holding a GC reference across a safepoint is rooted, so
//! addressed, so never in a register: its slot is what a collection
//! rewrites, and what the debug info names.

use crate::lir::{Func, NO_SLOT, PartSrc, Slot, Waits};
use crate::regalloc::{Alloc, R, Ranges};
use nabic_core::HashMap;

/// Where a variable's value is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// Memory at this offset from the frame base.
    Frame(i32),
    /// A register: its DWARF and `CodeView` numbers (`CodeView` 0: none).
    Reg(u16, u16),
    /// A constant's bits.
    Const(u64),
    /// A wasm local: its index.
    Local(u32),
    /// Byte pieces in order, `(size, where)`; `None`: not available.
    Pieces(Vec<(u32, Option<Place>)>),
}

/// `[start, end)` code offsets of a function and where a variable is
/// there; none: optimized out.
pub type Spans = Vec<(u32, u32, Place)>;

/// The slot parts whose store may wait (`lir::waitable`), and those
/// whose waiting store went out, with the code offset past it.
pub type Waiting<'a> = (&'a Waits, &'a [((u32, u32), u32)]);

/// A part of a slot in a register: offset, size, `[start, end)` code
/// ranges, the register.
type RegPart = (u32, u32, Vec<(u32, u32)>, Place);

/// The code offsets of a function's instructions, collected as the
/// emitter goes.
pub struct Track<'f> {
    f: &'f Func,
    next: usize,
    pcs: Vec<u32>,
    /// Variables' slots: the instructions their memory holds their
    /// value over (`frame::Layout::live`); none listed: all of them.
    live: HashMap<Slot, Ranges>,
}

impl<'f> Track<'f> {
    #[must_use]
    pub fn new(f: &'f Func, live: HashMap<Slot, Ranges>) -> Option<Self> {
        f.track.then(|| Self {
            f,
            next: 0,
            pcs: Vec::with_capacity(f.insts.len() + 1),
            live,
        })
    }

    /// Instruction `n` starts at code offset `at`: its rows go there (a
    /// row at the offset of the one before replaces it, which then
    /// covers no code).
    pub fn at(&mut self, n: usize, at: u32, lines: &mut Vec<(u32, u32, u32, u32)>) {
        debug_assert_eq!(self.pcs.len(), n);
        self.pcs.push(at);
        while let Some(&(_, l, c, s)) = self.f.rows.get(self.next).filter(|r| r.0 <= n) {
            if lines.last().is_some_and(|x| x.0 == at) {
                lines.pop();
            }
            lines.push((at, l, c, s));
            self.next += 1;
        }
    }

    /// Move each code offset so far by `to` (code compacted after).
    pub fn remap(&mut self, to: &dyn Fn(u32) -> u32) {
        for p in &mut self.pcs {
            *p = to(*p);
        }
    }

    /// Each variable's spans: `end` is the code's length, `ptr` the
    /// pointer size; `frame` gives a slot's frame-base offset, `reg` the
    /// numbers of a register holding a part of a size.
    pub fn finish(
        mut self,
        ra: &Alloc,
        (waits, stored): Waiting<'_>,
        (end, ptr): (u32, u32),
        frame: &dyn Fn(Slot) -> i32,
        reg: &dyn Fn(R, u32) -> Option<(u16, u16)>,
    ) -> Vec<Spans> {
        let f = self.f;
        self.pcs.push(end);
        let Some(d) = &f.debug else {
            return Vec::new();
        };
        // Slots some instruction reaches.
        let mut used = vec![false; f.slots.len()];
        let mut ops = Vec::new();
        for i in &f.insts {
            ops.clear();
            crate::regalloc::mems(i, ptr, &mut ops);
            for m in &ops {
                if let crate::lir::Mem::Frame(s, _) = m {
                    used[*s as usize] = true;
                }
            }
        }
        d.vars
            .iter()
            .map(|v| {
                if v.slot == NO_SLOT {
                    if !v.parts.is_empty() {
                        return self.parts(&v.parts, &used, ra, (waits, stored), end, frame, reg);
                    }
                    return v
                        .konst
                        .map_or_else(Vec::new, |c| vec![(0, end, Place::Const(c))]);
                }
                if !used[v.slot as usize] {
                    return Vec::new();
                }
                self.spans(v.slot, ra, (waits, stored), end, frame, reg)
            })
            .collect()
    }

    /// Where an aggregate built of `parts` is over the code: each part
    /// where it is, a piece.
    #[allow(clippy::too_many_arguments)]
    fn parts(
        &self,
        parts: &[(u32, u32, PartSrc)],
        used: &[bool],
        ra: &Alloc,
        waits: Waiting<'_>,
        end: u32,
        frame: &dyn Fn(Slot) -> i32,
        reg: &dyn Fn(R, u32) -> Option<(u16, u16)>,
    ) -> Spans {
        let each: Vec<Spans> = parts
            .iter()
            .map(|p| match p.2 {
                PartSrc::Slot(s) if used[s as usize] => self.spans(s, ra, waits, end, frame, reg),
                PartSrc::Const(c) => vec![(0, end, Place::Const(c))],
                _ => Vec::new(),
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
            for (p, sp) in parts.iter().zip(&each) {
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
    }

    /// Where slot `s` is over the code.
    fn spans(
        &self,
        s: Slot,
        ra: &Alloc,
        (waits, stored): Waiting<'_>,
        end: u32,
        frame: &dyn Fn(Slot) -> i32,
        reg: &dyn Fn(R, u32) -> Option<(u16, u16)>,
    ) -> Spans {
        let f = self.f;
        let size = f.slots[s as usize].0;
        // Memory holds the bytes no register does, from the start or, if
        // their store waited (`waitable`), from where it went out; never
        // if it did not.
        let mut from = Some(0);
        for k in waits.keys().filter(|k| k.0 == s) {
            let at = stored.iter().filter(|x| x.0 == *k).map(|x| x.1).max();
            from = from.zip(at).map(|(a, b)| a.max(b));
        }
        let from = from.filter(|_| !ra.gone(s));
        // Memory holds the value only over the slot's ranges: not before
        // it is written (so not within the instruction writing it, which
        // may be many), nor once it is dead (a store dropped, the bytes
        // shared).
        let only: Option<Vec<(u32, u32)>> = self.live.get(&s).map(|r| {
            r.iter()
                .map(|&(a, b)| {
                    let set = crate::regalloc::writes_slot(&f.insts[a], s);
                    (self.pcs[if set { a + 1 } else { a }], self.pcs[b + 1])
                })
                .filter(|x| x.0 < x.1)
                .collect()
        });
        let held = |a: u32, b: u32| {
            from.is_some_and(|x| x <= a)
                && only
                    .as_ref()
                    .is_none_or(|o| o.iter().any(|&(x, y)| x <= a && b <= y))
        };
        // `(offset, size, [start, end) code ranges, place)` of the
        // parts in registers.
        let mut parts: Vec<RegPart> = Vec::new();
        for (&(ps, po), (psize, ranges)) in &ra.live {
            if ps != s {
                continue;
            }
            let m = crate::lir::Mem::Frame(ps, po);
            // A pair: each word in its register, the low one first.
            let regs: Vec<(u32, u32, R)> = if let Some((lo, hi)) = ra.pair(m) {
                let w = psize / 2;
                vec![
                    (po, w, R { reg: lo, fp: false }),
                    (po + w, w, R { reg: hi, fp: false }),
                ]
            } else if let Some(r) = ra.get(m) {
                vec![(po, *psize, r)]
            } else {
                continue;
            };
            let Some(places) = regs
                .iter()
                .map(|&(o, size, r)| reg(r, size).map(|(dw, cv)| (o, size, Place::Reg(dw, cv))))
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let code: Vec<(u32, u32)> = ranges
                .iter()
                .filter_map(|&(a, b)| {
                    // Held once the instruction setting it is done.
                    let set = crate::regalloc::writes(&f.insts[a], (ps, po));
                    let lo = self.pcs[if set { a + 1 } else { a }];
                    let hi = self.pcs[b + 1];
                    (lo < hi).then_some((lo, hi))
                })
                .collect();
            for (o, size, place) in places {
                parts.push((o, size, code.clone(), place));
            }
        }
        // A part a temporary carries for a while (`Alloc::copies`) is
        // where the temporary is then; its own place goes first.
        let own = parts.len();
        for (((ps, po), (ts, to)), ranges) in &ra.copies {
            if *ps != s {
                continue;
            }
            let t = crate::lir::Mem::Frame(*ts, *to);
            let size = ra
                .live
                .get(&(*ps, *po))
                .map_or(f.slots[*ts as usize].0, |l| l.0);
            let place = match (ra.get(t), ra.pair(t)) {
                (Some(r), _) => reg(r, size).map(|(dw, cv)| Place::Reg(dw, cv)),
                (None, None) => Some(Place::Frame(frame(*ts) + *to as i32)),
                _ => None,
            };
            let Some(place) = place else { continue };
            let code: Vec<(u32, u32)> = ranges
                .iter()
                .filter_map(|&(a, b)| {
                    let set = crate::regalloc::writes(&f.insts[a], (*ts, *to));
                    let lo = self.pcs[if set { a + 1 } else { a }];
                    let hi = self.pcs[b + 1];
                    (lo < hi).then_some((lo, hi))
                })
                .collect();
            parts.push((*po, size, code, place));
        }
        let mut idx: Vec<usize> = (0..parts.len()).collect();
        idx.sort_by_key(|&i| (parts[i].0, i >= own, i));
        // Whether each part is the slot's own register.
        let mine: Vec<bool> = idx.iter().map(|&i| i < own).collect();
        let parts: Vec<RegPart> = idx.into_iter().map(|i| parts[i].clone()).collect();
        if parts.is_empty() {
            let Some(a) = from else {
                return Vec::new();
            };
            let Some(o) = &only else {
                return vec![(a, end, Place::Frame(frame(s)))];
            };
            return o
                .iter()
                .map(|&(x, y)| (x.max(a), y.min(end)))
                .filter(|x| x.0 < x.1)
                .map(|(x, y)| (x, y, Place::Frame(frame(s))))
                .collect();
        }
        let mut cuts: Vec<u32> = vec![0, end];
        cuts.extend(from);
        for o in only.iter().flatten() {
            cuts.extend([o.0, o.1]);
        }
        for p in &parts {
            for r in &p.2 {
                cuts.push(r.0);
                cuts.push(r.1);
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut out: Spans = Vec::new();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let mem = held(a, b);
            let mut pieces: Vec<(u32, Option<Place>)> = Vec::new();
            let mut at = 0;
            let mut k = 0;
            while k < parts.len() {
                let (po, psize) = (parts[k].0, parts[k].1);
                let mut e = k;
                while e < parts.len() && parts[e].0 == po {
                    e += 1;
                }
                if po < at {
                    k = e;
                    continue;
                }
                if po > at {
                    let m = mem.then(|| Place::Frame(frame(s) + at as i32));
                    pieces.push((po - at, m));
                }
                let held = parts[k..e]
                    .iter()
                    .find(|p| p.2.iter().any(|&(x, y)| x <= a && b <= y))
                    .map(|p| p.3.clone());
                // Not in a register of its own: memory, where nothing
                // carries it.
                let place = held.or_else(|| {
                    (!mine[k..e].iter().any(|&x| x) && mem)
                        .then(|| Place::Frame(frame(s) + po as i32))
                });
                pieces.push((psize, place));
                at = po + psize;
                k = e;
            }
            if at < size {
                let m = mem.then(|| Place::Frame(frame(s) + at as i32));
                pieces.push((size - at, m));
            }
            if pieces.iter().all(|p| p.1.is_none()) {
                continue;
            }
            let place = match &pieces[..] {
                [(_, Some(p))] => p.clone(),
                _ if mem && pieces.iter().all(|p| matches!(p.1, Some(Place::Frame(_)))) => {
                    Place::Frame(frame(s))
                }
                _ => Place::Pieces(pieces),
            };
            match out.last_mut() {
                Some(l) if l.1 == a && l.2 == place => l.1 = b,
                _ => out.push((a, b, place)),
            }
        }
        out
    }
}
