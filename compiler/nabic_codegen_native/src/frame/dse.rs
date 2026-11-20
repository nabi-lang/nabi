//! Dead stores to frame slots.

#[allow(clippy::wildcard_imports)]
use crate::frame::*;

/// How a slot may be reached besides its own operands.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Reach {
    /// Only by them.
    Own,
    /// Also by a collection (a root the shadow-stack frame lists).
    Root,
    /// Also through a pointer, or by the debugger: never dropped.
    Any,
}

/// Drop the stores to frame slots that nothing reads before they are
/// overwritten or the frame dies (a return or a trap), on any path: for
/// one, the zeroing of a dead root right before a return, or of a root
/// every path fills before reading. A call may run a collection, which
/// reads every root. `None` when nothing is dropped.
pub fn dse(f: &Func, ptr: u32) -> Option<Func> {
    let n = f.slots.len();
    let mut reach = vec![Reach::Own; n];
    let mut addr = Vec::new();
    let acc = Accs::of(f, ptr, &mut addr);
    let fs = f.gc_frame.as_ref().map(|g| g.0);
    for (s, dst) in &addr {
        let gc = matches!((dst, fs), (Mem::Frame(d, _), Some(fs)) if *d == fs);
        let r = &mut reach[*s as usize];
        if gc && *r == Reach::Own {
            *r = Reach::Root;
        } else if !gc {
            *r = Reach::Any;
        }
    }
    if let Some(fs) = fs {
        reach[fs as usize] = Reach::Any;
    }
    for i in &f.insts {
        if let Inst::VLoop(l) = i {
            reach[l.io as usize] = Reach::Any;
        }
    }
    if !f.track {
        for v in f.debug.iter().flat_map(|d| &d.vars) {
            if let Some(r) = reach.get_mut(v.slot as usize) {
                *r = Reach::Any;
            }
            for p in &v.parts {
                if let crate::lir::PartSrc::Slot(s) = p.2 {
                    reach[s as usize] = Reach::Any;
                }
            }
        }
    }
    let roots: Vec<Slot> = (0..n as Slot)
        .filter(|&s| reach[s as usize] == Reach::Root)
        .collect();
    let cfg = Cfg::new(f);
    let live_out = live_out(f, &cfg, &acc, &reach, &roots);
    let mut drop = vec![false; f.insts.len()];
    let mut any = false;
    // Per slot, in the block: dead byte ranges once `set`; until then
    // dead whole if no successor may read it.
    let mut dead: Vec<Vec<(u32, u32)>> = vec![Vec::new(); n];
    let mut set = vec![false; n];
    let mut touched: Vec<Slot> = Vec::new();
    for b in 0..cfg.starts.len() {
        let (s0, e) = (cfg.starts[b], cfg.end(b));
        if e <= s0 {
            continue;
        }
        while let Some(s) = touched.pop() {
            set[s as usize] = false;
            dead[s as usize].clear();
        }
        // A block leaving the code another way than a return or a trap
        // keeps everything.
        let open =
            cfg.succ[b].is_empty() && !matches!(f.insts[e - 1], Inst::Ret { .. } | Inst::Trap);
        let out = |s: Slot| {
            open || live_out.as_ref().is_some_and(|l| {
                let v = l.0[s as usize];
                v != u32::MAX && l.1.has(b, v as usize)
            })
        };
        // Slot `s` from here back: its dead bytes listed.
        // Slot `s` from here back: its dead bytes listed.
        let list = |s: Slot, set: &mut [bool], dead: &mut [Vec<(u32, u32)>], t: &mut Vec<Slot>| {
            let x = s as usize;
            if !set[x] {
                set[x] = true;
                t.push(s);
                if !out(s) {
                    dead[x].push((0, f.slots[x].0));
                }
            }
            x
        };
        for at in (s0..e).rev() {
            let i = &f.insts[at];
            let here = acc.at(at);
            // Its one write, to a slot only its operands reach, already
            // dead after it.
            let mut w = here.iter().filter_map(|&(s, a)| match a {
                Acc::Write(o, z) => Some((s, o, z)),
                Acc::Read => None,
            });
            let one = match (w.next(), w.next()) {
                (Some(x), None) => Some(x),
                _ => None,
            };
            let gone = removable(i)
                && one.is_some_and(|(s, o, z)| {
                    z > 0
                        && reach[s as usize] != Reach::Any
                        && if set[s as usize] {
                            dead[s as usize].iter().any(|&(a, e)| a <= o && o + z <= e)
                        } else {
                            !out(s)
                        }
                });
            if gone {
                drop[at] = true;
                any = true;
                continue;
            }
            for &(s, a) in here {
                if let Acc::Write(o, z) = a {
                    if z == 0 || reach[s as usize] == Reach::Any {
                        continue;
                    }
                    let x = list(s, &mut set, &mut dead, &mut touched);
                    let d = &mut dead[x];
                    d.push((o, o + z));
                    if d.len() > 1 {
                        d.sort_unstable();
                        let mut k = 0;
                        for j in 1..d.len() {
                            if d[j].0 <= d[k].1 {
                                d[k].1 = d[k].1.max(d[j].1);
                            } else {
                                k += 1;
                                d[k] = d[j];
                            }
                        }
                        d.truncate(k + 1);
                    }
                }
            }
            let calls = matches!(i, Inst::Call { .. });
            let reads = here
                .iter()
                .filter(|x| x.1 == Acc::Read)
                .map(|x| x.0)
                .chain(roots.iter().copied().filter(|_| calls));
            for s in reads {
                let x = s as usize;
                if !set[x] {
                    set[x] = true;
                    touched.push(s);
                }
                dead[x].clear();
            }
        }
    }
    if !any {
        return None;
    }
    let mut k = 0;
    let insts = f
        .insts
        .iter()
        .filter(|_| {
            k += 1;
            !drop[k - 1]
        })
        .cloned()
        .collect();
    Some(f.with_insts(insts))
}

/// Every instruction's `accesses`, packed.
pub(crate) struct Accs {
    items: Vec<(Slot, Acc)>,
    ends: Vec<u32>,
}

impl Accs {
    /// Of `f`'s instructions; the slots whose address they take to
    /// `addr`.
    pub(crate) fn of(f: &Func, ptr: u32, addr: &mut Vec<(Slot, Mem)>) -> Self {
        let mut items = Vec::with_capacity(f.insts.len() * 2);
        let mut ends = Vec::with_capacity(f.insts.len());
        for i in &f.insts {
            accesses(i, ptr, &mut items, addr);
            ends.push(items.len() as u32);
        }
        Self { items, ends }
    }

    pub(crate) fn at(&self, i: usize) -> &[(Slot, Acc)] {
        let s = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.items[s..self.ends[i] as usize]
    }
}

/// Whether `dse` may drop `i` when its one write is dead.
pub(crate) fn removable(i: &Inst) -> bool {
    crate::regalloc::pure(i)
        || matches!(
            i,
            Inst::Zero {
                dst: Mem::Frame(..),
                ..
            } | Inst::Copy {
                dst: Mem::Frame(..),
                src: Mem::Frame(..),
                ..
            }
        )
}

/// `live_out`'s state of a slot.
#[derive(Clone, Default)]
pub(crate) struct Scan {
    /// In the current block: read first (1), overwritten whole first
    /// (2), neither yet (0).
    first: u8,
    /// In the current block: written by a store `dse` may drop; the
    /// backward pass may find it dead at the block's end, whatever reads
    /// follow (they may go with their own results).
    wrote: bool,
    /// Such a store in a block with a successor.
    cand: bool,
    /// Reached by more than one block, or read first in one: may be
    /// live across blocks.
    glob: bool,
    /// The last block that reached it (the current one: listed in
    /// `touched`).
    home: u32,
    /// In the current block: bytes written before a read or a whole
    /// overwrite.
    run: Vec<(u32, u32)>,
}

/// The slots `dse` may drop stores to that some path from the end of
/// each block may read (a call may read every root): `(index per slot,
/// u32::MAX for none; rows)`. Only slots a droppable store writes in a
/// block with a successor, and that may be live across blocks, are
/// indexed; `None` when there are none.
pub(crate) fn live_out(
    f: &Func,
    cfg: &Cfg,
    acc: &Accs,
    reach: &[Reach],
    roots: &[Slot],
) -> Option<(Vec<u32>, Rows)> {
    let nb = cfg.starts.len();
    if cfg.succ.iter().all(Vec::is_empty) {
        return None;
    }
    let n = reach.len();
    let mut st = vec![
        Scan {
            home: u32::MAX,
            ..Scan::default()
        };
        n
    ];
    let mut touched: Vec<Slot> = Vec::new();
    // `(block, slot, read first)`.
    let mut ev: Vec<(u32, Slot, bool)> = Vec::new();
    for b in 0..=nb {
        // The block before: a store it leaves may be read on.
        while let Some(s) = touched.pop() {
            let x = &mut st[s as usize];
            x.cand |= x.wrote && !cfg.succ[b - 1].is_empty();
            x.glob |= x.first == 1;
            x.first = 0;
            x.wrote = false;
            x.run.clear();
        }
        if b == nb {
            break;
        }
        let touch = |st: &mut [Scan], t: &mut Vec<Slot>, s: Slot| {
            let x = &mut st[s as usize];
            if x.home != b as u32 {
                x.glob |= x.home != u32::MAX;
                x.home = b as u32;
                t.push(s);
            }
        };
        for i in cfg.starts[b]..cfg.end(b) {
            let here = acc.at(i);
            let calls = matches!(f.insts[i], Inst::Call { .. });
            let reads = here
                .iter()
                .filter(|x| x.1 == Acc::Read && reach[x.0 as usize] != Reach::Any)
                .map(|x| x.0)
                .chain(roots.iter().copied().filter(|_| calls));
            for s in reads {
                touch(&mut st, &mut touched, s);
                let x = &mut st[s as usize];
                if x.first == 0 {
                    x.first = 1;
                    ev.push((b as u32, s, true));
                }
            }
            let droppable = removable(&f.insts[i]);
            for &(s, a) in here {
                let Acc::Write(o, z) = a else { continue };
                if z == 0 || reach[s as usize] == Reach::Any {
                    continue;
                }
                touch(&mut st, &mut touched, s);
                let size = f.slots[s as usize].0;
                let x = &mut st[s as usize];
                x.wrote |= droppable;
                if x.first == 0 {
                    x.run.push((o, o + z));
                    if covers(&mut x.run, size) {
                        x.first = 2;
                        ev.push((b as u32, s, false));
                    }
                }
            }
        }
    }
    let mut idx = vec![u32::MAX; n];
    let mut nc = 0;
    for (s, x) in st.iter().enumerate() {
        if x.cand && x.glob {
            idx[s] = nc as u32;
            nc += 1;
        }
    }
    if nc == 0 {
        return None;
    }
    let mut gen_ = Rows::new(nb, nc);
    let mut kill = Rows::new(nb, nc);
    for &(b, s, r) in &ev {
        let v = idx[s as usize];
        if v != u32::MAX {
            if r {
                gen_.set(b as usize, v as usize);
            } else {
                kill.set(b as usize, v as usize);
            }
        }
    }
    let w = nc.div_ceil(64);
    let mut live_in = Rows::new(nb, nc);
    let mut out = Rows::new(nb, nc);
    let mut tmp = vec![0u64; w];
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..nb).rev() {
            tmp.fill(0);
            for &s in &cfg.succ[b] {
                for (t, x) in tmp.iter_mut().zip(live_in.row(s)) {
                    *t |= x;
                }
            }
            out.row_mut(b).copy_from_slice(&tmp);
            for (t, (g, k)) in tmp.iter_mut().zip(gen_.row(b).iter().zip(kill.row(b))) {
                *t = (*t & !k) | g;
            }
            if tmp != live_in.row(b) {
                live_in.row_mut(b).copy_from_slice(&tmp);
                changed = true;
            }
        }
    }
    Some((idx, out))
}
