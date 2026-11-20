//! Frame layout: where each slot lives in the frame.
//!
//! At `-O0` every slot has its own bytes. Above it slots whose values are
//! never live at once share them (stack coloring). A slot's range is
//! where it is written or read, plus where its value may still be read:
//! live (a later read without a whole overwrite between) and written on
//! some path from the entry. A write's range runs on to the next read of
//! the slot in its block, or the block's end: an emitter may hold the
//! store back until then (`lir::waitable`). Slots whose ranges never
//! meet share memory, the biggest first, each in the first group that
//! takes it.
//!
//! A slot whose address is taken shares too when the address does not
//! escape (`flow`): it only reaches loads and stores, the calls that
//! keep no pointer they are passed (`Func::caps`) and a vector loop's
//! block. Such a slot is written where its address is taken and read
//! wherever a pointer that may hold it is used. GC roots share only with
//! roots of the same size and root offsets, so whatever a collection
//! scans holds a reference or null (the frame lists their addresses; the
//! collector only reads roots). An escaping slot keeps its own bytes,
//! and so do a vector loop's block and any slot the debugger reads in
//! place (`-g` without `Func::track`). With `Func::track` the ranges of
//! the slots variables are in go to `locs`, which describes the memory
//! only there.

mod dse;
mod escape;

pub(crate) use crate::frame::dse::*;
pub(crate) use crate::frame::escape::*;

use crate::lir::{Callee, Cap, Caps, Func, Inst, Loc, Mem, Slot, SymId, V};
use crate::regalloc::{Bits, Cfg, Ranges, Rows, merge};
use nabic_core::HashMap;

/// Where `f`'s slots are.
pub struct Layout {
    /// Each slot's offset (`none` for a slot that needs no memory).
    pub off: Vec<u32>,
    /// The end of the last slot.
    pub end: u32,
    /// With `Func::track`: the instructions the memory of a variable's
    /// slot holds its value over.
    pub live: HashMap<Slot, Ranges>,
}

/// Lay out `f`'s slots from `start`, each aligned to at most `cap`; a slot
/// `gone` says needs no memory gets `none`. `ptr` is the pointer size.
pub fn layout(
    f: &Func,
    gone: &dyn Fn(Slot) -> bool,
    (start, cap, none): (u32, u32, u32),
    ptr: u32,
) -> Layout {
    let order: Vec<Slot> = (0..f.slots.len() as Slot).collect();
    layout_in(f, gone, (start, cap, none), ptr, &order)
}

/// `layout`, placing the slots in `order` (a permutation of them all).
pub fn layout_in(
    f: &Func,
    gone: &dyn Fn(Slot) -> bool,
    (start, cap, none): (u32, u32, u32),
    ptr: u32,
    order: &[Slot],
) -> Layout {
    let n = f.slots.len();
    let groups = if f.opt { color(f, gone, ptr) } else { None };
    let mut at = start;
    let mut off = vec![none; n];
    let mut placed: Vec<u32> = Vec::new();
    let mut live = HashMap::default();
    if let Some(g) = &groups {
        placed = vec![u32::MAX; g.groups.len()];
    }
    for &s in order {
        let s = s as usize;
        let (size, align) = f.slots[s];
        if gone(s as Slot) {
            continue;
        }
        let (size, align, group) = match groups.as_ref().and_then(|g| g.of[s].map(|x| (g, x))) {
            Some((g, x)) => (g.groups[x].size, g.groups[x].align, Some(x)),
            None => (size, align, None),
        };
        if let Some(x) = group {
            if placed[x] != u32::MAX {
                off[s] = placed[x];
                continue;
            }
        }
        at = at.next_multiple_of(align.min(cap));
        off[s] = at;
        if let Some(x) = group {
            placed[x] = at;
        }
        at += size;
    }
    if let Some(g) = groups.filter(|_| f.track) {
        live.extend(g.ranges);
    }
    Layout { off, end: at, live }
}

/// Slots sharing memory.
struct Group {
    size: u32,
    align: u32,
    /// Roots: the root offsets (one group per set), else `None`.
    key: Option<u32>,
    /// The instructions its slots' ranges cover, and their bounds.
    occ: Bits,
    lo: usize,
    hi: usize,
    members: u32,
}

struct Groups {
    groups: Vec<Group>,
    /// Each slot's group, if it shares.
    of: Vec<Option<usize>>,
    /// The shared slots' ranges.
    ranges: Vec<(Slot, Ranges)>,
}

/// How an instruction accesses a slot.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Acc {
    Read,
    /// Bytes `[off, off + size)` written.
    Write(u32, u32),
}

/// The frame slots `i` reads and writes, reads first; `addr`: slots whose
/// address it takes, with where it puts it.
fn accesses(i: &Inst, ptr: u32, out: &mut Vec<(Slot, Acc)>, addr: &mut Vec<(Slot, Mem)>) {
    let rd = |out: &mut Vec<(Slot, Acc)>, m: &Mem| match *m {
        Mem::Frame(s, _) | Mem::Ind(s, _) => out.push((s, Acc::Read)),
    };
    let rv = |out: &mut Vec<(Slot, Acc)>, v: &V| {
        if let V::M(m) = v {
            rd(out, m);
        }
    };
    // A write through a pointer reads the pointer.
    let wr = |out: &mut Vec<(Slot, Acc)>, m: &Mem, size: u32| match *m {
        Mem::Frame(s, o) => out.push((s, Acc::Write(o, size))),
        Mem::Ind(s, _) => out.push((s, Acc::Read)),
    };
    match i {
        Inst::Label(_) | Inst::Jmp(_) | Inst::Trap | Inst::Line(..) => {}
        Inst::Br { c, .. } => rv(out, c),
        Inst::Mov { k, dst, src } => {
            rv(out, src);
            wr(out, dst, k.size());
        }
        Inst::Copy { dst, src, size } => {
            rd(out, src);
            wr(out, dst, *size);
        }
        Inst::Zero { dst, size } => wr(out, dst, *size),
        Inst::V(x) => {
            for m in x.srcs() {
                rd(out, &m);
            }
            if let Some((m, _)) = x.scalar() {
                rd(out, &m);
            }
            wr(out, &x.dst, x.dst_size());
        }
        Inst::Addr { dst, src } => {
            match *src {
                Mem::Frame(s, _) => addr.push((s, *dst)),
                Mem::Ind(s, _) => out.push((s, Acc::Read)),
            }
            wr(out, dst, ptr);
        }
        Inst::Sym { dst, .. } | Inst::Tls { dst, .. } => wr(out, dst, ptr),
        Inst::Param { part, dst } => wr(out, dst, part.size()),
        Inst::Bin { k, dst, a, b, .. } | Inst::Ovf { k, dst, a, b, .. } => {
            rv(out, a);
            rv(out, b);
            wr(out, dst, k.size());
        }
        Inst::Cmp { dst, a, b, .. } => {
            rv(out, a);
            rv(out, b);
            wr(out, dst, 1);
        }
        Inst::Un { k, dst, a, .. } => {
            rv(out, a);
            wr(out, dst, k.size());
        }
        Inst::Conv { to, dst, a, .. } => {
            rv(out, a);
            wr(out, dst, to.size());
        }
        Inst::Fma { k, dst, a, b, c } => {
            rv(out, a);
            rv(out, b);
            rv(out, c);
            wr(out, dst, k.size());
        }
        Inst::AtomicLoad { k, dst, ptr: p, .. } => {
            rd(out, p);
            wr(out, dst, k.size());
        }
        Inst::AtomicStore { ptr: p, src, .. } => {
            rd(out, p);
            rv(out, src);
        }
        Inst::AtomicRmw {
            k,
            dst,
            ptr: p,
            val,
            ..
        } => {
            rd(out, p);
            rv(out, val);
            wr(out, dst, k.size());
        }
        Inst::Cas {
            k,
            dst,
            ptr: p,
            expected,
            new,
            ..
        } => {
            rd(out, p);
            rv(out, expected);
            rv(out, new);
            wr(out, dst, k.size());
        }
        Inst::Call {
            callee,
            args,
            rets,
            sret,
            ..
        } => {
            if let Callee::Ptr(m) = callee {
                rd(out, m);
            }
            for (_, v) in args {
                rv(out, v);
            }
            for (p, m) in rets {
                wr(out, m, p.size());
            }
            // The callee writes the result; what it covers is not known.
            if let Some(m) = sret {
                wr(out, m, 0);
            }
        }
        Inst::Ret { vals } => {
            for (_, v) in vals {
                rv(out, v);
            }
        }
        // Its block whole: read, and parts of it written (the block keeps
        // its own bytes: `color`, `dse`).
        Inst::VLoop(l) => {
            out.push((l.io, Acc::Read));
            out.push((l.io, Acc::Write(0, 0)));
        }
    }
}

/// Nothing shares below two slots, and little in a small frame.
const MIN_SLOTS: usize = 2;
const MIN_BYTES: u32 = 256;

/// Which slots share memory and, with `Func::track`, the ranges of the
/// slots variables are in; `None` when neither.
fn color(f: &Func, gone: &dyn Fn(Slot) -> bool, ptr: u32) -> Option<Groups> {
    let n = f.slots.len();
    let mem: u32 = (0..n)
        .filter(|&s| !gone(s as Slot))
        .map(|s| f.slots[s].0)
        .sum();
    let share = n >= MIN_SLOTS && mem >= MIN_BYTES;
    if !share && !f.track {
        return None;
    }
    // A slot whose address only reaches loads, stores and calls that
    // keep no pointer is accessed where those are: at each, through
    // any pointer that may hold it, and from where the address is taken.
    let fl = flow(f, &[], f.caps.as_deref());
    let esc = |s: Slot| fl.as_ref().is_some_and(|fl| fl.esc[s as usize]);
    let mut acc = Vec::new();
    let mut addr = Vec::new();
    let mut ms = Vec::new();
    let mut via = Vec::new();
    // Per instruction, its accesses (packed).
    let mut ends: Vec<u32> = Vec::with_capacity(f.insts.len());
    let mut items: Vec<(Slot, Acc)> = Vec::with_capacity(f.insts.len() * 2);
    for i in &f.insts {
        if let Some(fl) = &fl {
            via.clear();
            reached(i, fl, &mut ms, &mut via);
            items.extend(
                via.iter()
                    .filter(|&&x| !fl.esc[x as usize])
                    .map(|&x| (x, Acc::Read)),
            );
        }
        accesses(i, ptr, &mut acc, &mut addr);
        items.append(&mut acc);
        if let Inst::Addr {
            dst,
            src: Mem::Frame(x, _),
        } = i
        {
            if !listing(f, dst, *x) {
                items.push((*x, Acc::Write(0, 0)));
            }
        }
        ends.push(items.len() as u32);
    }
    // Reached where no instruction shows: memory of its own.
    let mut fixed = vec![false; n];
    if let Some((fs, _)) = &f.gc_frame {
        fixed[*fs as usize] = true;
    }
    for (s, dst) in &addr {
        if !listing(f, dst, *s) && esc(*s) {
            fixed[*s as usize] = true;
        }
    }
    // Root offsets of each root slot, from the shadow-stack frame.
    let mut roots: Vec<Vec<u32>> = vec![Vec::new(); n];
    for i in &f.insts {
        match i {
            Inst::Addr {
                dst,
                src: Mem::Frame(s, o),
            } if listing(f, dst, *s) => roots[*s as usize].push(*o),
            // A vector loop's block: the emitter also saves registers in it.
            Inst::VLoop(l) => fixed[l.io as usize] = true,
            _ => {}
        }
    }
    let mut ok: Vec<bool> = (0..n).map(|s| !gone(s as Slot) && !fixed[s]).collect();
    // With `Func::track` the ranges of variables' slots, to describe
    // their memory only there; without, the debugger reads them in place.
    let mut want = vec![false; n];
    for v in f.debug.iter().flat_map(|d| &d.vars) {
        let parts = v.parts.iter().filter_map(|p| match p.2 {
            crate::lir::PartSrc::Slot(s) => Some(s),
            _ => None,
        });
        for s in std::iter::once(v.slot).chain(parts) {
            if (s as usize) < n {
                if f.track {
                    want[s as usize] = ok[s as usize];
                } else {
                    ok[s as usize] = false;
                }
            }
        }
    }
    let (count, bytes) = (0..n)
        .filter(|&s| ok[s])
        .fold((0, 0), |(c, b), s| (c + 1, b + f.slots[s].0));
    let share = share && count >= MIN_SLOTS && bytes >= MIN_BYTES;
    if !share && !f.track {
        return None;
    }
    // Dense numbering of the slots to range.
    let mut idx = vec![u32::MAX; n];
    let mut slots: Vec<Slot> = Vec::new();
    for s in 0..n {
        if (share && ok[s]) || want[s] {
            idx[s] = slots.len() as u32;
            slots.push(s as Slot);
        }
    }
    let nv = slots.len();
    if nv == 0 {
        return None;
    }
    let ni = f.insts.len();
    let at = |i: usize| -> &[(Slot, Acc)] {
        let s = if i == 0 { 0 } else { ends[i - 1] as usize };
        &items[s..ends[i] as usize]
    };
    let cfg = Cfg::new(f);
    let nb = cfg.starts.len();
    // Whole overwrites: a run of writes, no read of the slot between,
    // that covers it kills it at the run's first write.
    let mut kl: Vec<(u32, u32)> = Vec::new();
    // Per slot: the run's first write (`usize::MAX`: none), its bytes.
    let mut run_at = vec![usize::MAX; nv];
    let mut run: Vec<Vec<(u32, u32)>> = vec![Vec::new(); nv];
    let mut in_run: Vec<usize> = Vec::new();
    // A slot only one block accesses, killed there before any read, is
    // live nowhere across blocks: it needs no dataflow (`glob`).
    let mut home = vec![usize::MAX; nv];
    let mut glob = vec![false; nv];
    let mut killed = vec![false; nv];
    for b in 0..nb {
        for v in std::mem::take(&mut in_run) {
            run_at[v] = usize::MAX;
            run[v].clear();
        }
        for i in cfg.starts[b]..cfg.end(b) {
            for &(s, a) in at(i) {
                let v = idx[s as usize] as usize;
                if v == u32::MAX as usize {
                    continue;
                }
                if home[v] != b {
                    glob[v] |= home[v] != usize::MAX;
                    home[v] = b;
                    killed[v] = false;
                }
                match a {
                    Acc::Read => {
                        glob[v] |= !killed[v];
                        run_at[v] = usize::MAX;
                        run[v].clear();
                    }
                    Acc::Write(o, size) => {
                        if run_at[v] == usize::MAX {
                            run_at[v] = i;
                            in_run.push(v);
                        }
                        let whole = f.slots[s as usize].0;
                        // One write of it all: no need to sort.
                        if o == 0 && size >= whole && run[v].is_empty() {
                            kl.push((i as u32, v as u32));
                            run_at[v] = usize::MAX;
                            killed[v] = true;
                            continue;
                        }
                        run[v].push((o, o + size));
                        if covers(&mut run[v], whole) {
                            kl.push((run_at[v] as u32, v as u32));
                            run_at[v] = usize::MAX;
                            run[v].clear();
                            killed[v] = true;
                        }
                    }
                }
            }
        }
    }
    drop((home, killed));
    // Dense numbering of those that do.
    let mut gi = vec![u32::MAX; nv];
    let mut gv: Vec<usize> = Vec::new();
    for v in 0..nv {
        if glob[v] {
            gi[v] = gv.len() as u32;
            gv.push(v);
        }
    }
    let ng = gv.len();
    kl.sort_unstable();
    let mut kstart = vec![0u32; ni + 1];
    for &(i, _) in &kl {
        kstart[i as usize + 1] += 1;
    }
    for i in 0..ni {
        kstart[i + 1] += kstart[i];
    }
    let kills = |i: usize| &kl[kstart[i] as usize..kstart[i + 1] as usize];
    // Liveness (backward) and written-on-some-path (forward), per block.
    let mut gen_ = Rows::new(nb, ng);
    let mut kill = Rows::new(nb, ng);
    let mut wrote = Rows::new(nb, ng);
    for b in 0..nb {
        for i in (cfg.starts[b]..cfg.end(b)).rev() {
            for &(_, v) in kills(i) {
                let g = gi[v as usize];
                if g != u32::MAX {
                    kill.set(b, g as usize);
                    gen_.clear(b, g as usize);
                }
            }
            for &(s, a) in at(i) {
                let v = idx[s as usize];
                if v == u32::MAX || gi[v as usize] == u32::MAX {
                    continue;
                }
                let g = gi[v as usize] as usize;
                match a {
                    Acc::Read => gen_.set(b, g),
                    Acc::Write(..) => wrote.set(b, g),
                }
            }
        }
    }
    let mut pred: Vec<Vec<usize>> = vec![Vec::new(); nb];
    for (b, ss) in cfg.succ.iter().enumerate() {
        for &s in ss {
            pred[s].push(b);
        }
    }
    let w = ng.div_ceil(64);
    let mut live_in = Rows::new(nb, ng);
    let mut live_out = Rows::new(nb, ng);
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
            live_out.row_mut(b).copy_from_slice(&tmp);
            for (t, (g, k)) in tmp.iter_mut().zip(gen_.row(b).iter().zip(kill.row(b))) {
                *t = (*t & !k) | g;
            }
            if tmp != live_in.row(b) {
                live_in.row_mut(b).copy_from_slice(&tmp);
                changed = true;
            }
        }
    }
    let mut init_in = Rows::new(nb, ng);
    changed = true;
    while changed {
        changed = false;
        for (b, ps) in pred.iter().enumerate() {
            tmp.fill(0);
            for &p in ps {
                for (t, (x, y)) in tmp.iter_mut().zip(init_in.row(p).iter().zip(wrote.row(p))) {
                    *t |= x | y;
                }
            }
            if tmp != init_in.row(b) {
                init_in.row_mut(b).copy_from_slice(&tmp);
                changed = true;
            }
        }
    }
    drop((gen_, kill, wrote, live_in));
    let init = |b: usize, v: usize| gi[v] != u32::MAX && init_in.has(b, gi[v] as usize);
    // Ranges, block by block.
    let mut ranges: Vec<Ranges> = vec![Vec::new(); nv];
    let mut first = vec![usize::MAX; nv];
    let mut pend = vec![usize::MAX; nv];
    let mut seen = vec![false; nv];
    let mut touched: Vec<usize> = Vec::new();
    let mut open = vec![usize::MAX; nv];
    let mut opened: Vec<usize> = Vec::new();
    let mut seg: Vec<(usize, usize, usize)> = Vec::new();
    for b in 0..nb {
        let (s0, e) = (cfg.starts[b], cfg.end(b));
        if e <= s0 {
            continue;
        }
        // Forward: each slot's first write here, and writes no read
        // follows here, held back until the block's end.
        for i in s0..e {
            for &(s, a) in at(i) {
                let v = idx[s as usize];
                if v == u32::MAX {
                    continue;
                }
                let v = v as usize;
                if !seen[v] {
                    seen[v] = true;
                    touched.push(v);
                }
                match a {
                    // A read the live segments do not cover: of bytes
                    // nothing wrote yet.
                    Acc::Read => {
                        pend[v] = usize::MAX;
                        if first[v] == usize::MAX && !init(b, v) {
                            ranges[v].push((i, i));
                        }
                    }
                    Acc::Write(..) => {
                        if first[v] == usize::MAX {
                            first[v] = i;
                        }
                        if pend[v] == usize::MAX {
                            pend[v] = i;
                        }
                    }
                }
            }
        }
        for &v in &touched {
            if pend[v] != usize::MAX {
                ranges[v].push((pend[v], e - 1));
            }
            pend[v] = usize::MAX;
        }
        // Backward: live segments.
        for g in live_out.iter(b) {
            let v = gv[g];
            open[v] = e - 1;
            opened.push(v);
        }
        for i in (s0..e).rev() {
            for &(_, v) in kills(i) {
                let v = v as usize;
                if open[v] != usize::MAX {
                    seg.push((v, i, open[v]));
                    open[v] = usize::MAX;
                }
            }
            for &(s, a) in at(i) {
                let v = idx[s as usize];
                if v != u32::MAX && a == Acc::Read && open[v as usize] == usize::MAX {
                    open[v as usize] = i;
                    opened.push(v as usize);
                }
            }
        }
        for v in std::mem::take(&mut opened) {
            if open[v] != usize::MAX {
                seg.push((v, s0, open[v]));
                open[v] = usize::MAX;
            }
        }
        // Where nothing wrote it yet its bytes hold no value.
        for (v, lo, hi) in std::mem::take(&mut seg) {
            let lo = if init(b, v) {
                lo
            } else if first[v] <= hi {
                lo.max(first[v])
            } else {
                continue;
            };
            ranges[v].push((lo, hi));
        }
        for v in std::mem::take(&mut touched) {
            first[v] = usize::MAX;
            seen[v] = false;
        }
    }
    drop((live_out, init_in));
    let ranges: Vec<Ranges> = ranges.into_iter().map(merge).collect();
    // Groups: roots by their offsets, the rest one kind.
    let mut keys: HashMap<(u32, Vec<u32>), u32> = HashMap::default();
    let key: Vec<Option<u32>> = slots
        .iter()
        .map(|&s| {
            let r = &roots[s as usize];
            if r.is_empty() {
                return None;
            }
            let mut r = r.clone();
            r.sort_unstable();
            let k = (f.slots[s as usize].0, r);
            let next = keys.len() as u32;
            Some(*keys.entry(k).or_insert(next))
        })
        .collect();
    // Roots by key, then the rest; each biggest first, then by start.
    let mut keyed: Vec<(u32, u32, usize, usize)> = (0..nv)
        .filter(|&v| share && ok[slots[v] as usize])
        .map(|v| {
            (
                key[v].map_or(u32::MAX, |k| k),
                u32::MAX - f.slots[slots[v] as usize].0,
                ranges[v].first().map_or(0, |r| r.0),
                v,
            )
        })
        .collect();
    keyed.sort_unstable();
    let order: Vec<usize> = keyed.into_iter().map(|k| k.3).collect();
    let mut groups: Vec<Group> = Vec::new();
    let mut of = vec![None; n];
    // First group of the current key.
    let mut from = 0;
    let mut last_key = None;
    for (k, &v) in order.iter().enumerate() {
        if k == 0 || key[v] != last_key {
            from = groups.len();
            last_key = key[v];
        }
        let s = slots[v] as usize;
        let (size, align) = f.slots[s];
        let r = &ranges[v];
        let (a, b) = (r.first().map_or(0, |x| x.0), r.last().map_or(0, |x| x.1));
        let g = (from..groups.len()).find(|&g| {
            let gr = &groups[g];
            r.is_empty() || b < gr.lo || gr.hi < a || !meets(&gr.occ, r)
        });
        let g = g.unwrap_or_else(|| {
            groups.push(Group {
                size,
                align,
                key: key[v],
                occ: Bits::new(ni),
                lo: usize::MAX,
                hi: 0,
                members: 0,
            });
            groups.len() - 1
        });
        let gr = &mut groups[g];
        debug_assert_eq!(gr.key, key[v]);
        gr.size = gr.size.max(size);
        gr.align = gr.align.max(align);
        gr.members += 1;
        if !r.is_empty() {
            gr.lo = gr.lo.min(a);
            gr.hi = gr.hi.max(b);
        }
        for &(a, b) in r {
            fill(&mut gr.occ, a, b);
        }
        of[s] = Some(g);
    }
    if groups.iter().all(|g| g.members < 2) {
        if !f.track {
            return None;
        }
        groups.clear();
        of.fill(None);
    }
    let ranges = if f.track {
        slots.iter().copied().zip(ranges).collect()
    } else {
        Vec::new()
    };
    Some(Groups { groups, of, ranges })
}

/// The bits of words `[a / 64, b / 64]` that `[a, b]` covers, by word.
fn words(a: usize, b: usize) -> impl Iterator<Item = (usize, u64)> {
    (a / 64..=b / 64).map(move |w| {
        let lo = if w == a / 64 { a % 64 } else { 0 };
        let hi = if w == b / 64 { b % 64 } else { 63 };
        (w, (u64::MAX >> (63 - hi)) & (u64::MAX << lo))
    })
}

/// Whether `occ` has any instruction of `r`.
fn meets(occ: &Bits, r: &Ranges) -> bool {
    r.iter()
        .any(|&(a, b)| words(a, b).any(|(w, m)| occ.0[w] & m != 0))
}

fn fill(occ: &mut Bits, a: usize, b: usize) {
    for (w, m) in words(a, b) {
        occ.0[w] |= m;
    }
}

/// Whether the byte ranges `r` cover `[0, size)` (sorts and merges `r`).
fn covers(r: &mut [(u32, u32)], size: u32) -> bool {
    r.sort_unstable();
    let mut end = 0;
    for &(a, b) in r.iter() {
        if a > end {
            return false;
        }
        end = end.max(b);
    }
    end >= size
}

/// Above `-O0`: an order for `layout_in` putting the slots used most per
/// byte where a short displacement reaches them, `low` bytes of them at
/// the bottom of the frame and `high` at its top (x86: an 8-bit
/// displacement from `rsp` / `esp` and from `rbp` / `ebp`), the rest
/// between in their own order. Uses are counted in the code as written.
#[must_use]
pub fn hot(f: &Func, ptr: u32, low: u32, high: u32) -> Vec<Slot> {
    let n = f.slots.len();
    let mut uses = vec![0u32; n];
    let mut ms = Vec::new();
    for i in &f.insts {
        ms.clear();
        crate::regalloc::mems(i, ptr, &mut ms);
        for m in &ms {
            let (Mem::Frame(s, _) | Mem::Ind(s, _)) = *m;
            uses[s as usize] += 1;
        }
    }
    let mut by: Vec<Slot> = (0..n as Slot).filter(|&s| uses[s as usize] > 0).collect();
    // Densest first; ties in slot order.
    by.sort_by(|&a, &b| {
        let (ua, ub) = (u64::from(uses[a as usize]), u64::from(uses[b as usize]));
        let (za, zb) = (
            u64::from(f.slots[a as usize].0),
            u64::from(f.slots[b as usize].0),
        );
        (ub * za).cmp(&(ua * zb)).then(a.cmp(&b))
    });
    // 0: bottom, 1: between, 2: top.
    let mut side = vec![1u8; n];
    let (mut lo, mut hi) = (0u32, 0u32);
    for s in by {
        let size = f.slots[s as usize].0;
        if lo + size <= low {
            lo += size;
            side[s as usize] = 0;
        } else if hi + size <= high {
            hi += size;
            side[s as usize] = 2;
        }
    }
    let mut order: Vec<Slot> = (0..n as Slot).collect();
    // Bottom, middle, top; the top's densest last (nearest the top).
    order.sort_by_key(|&s| (side[s as usize], s));
    let top = order
        .iter()
        .position(|&s| side[s as usize] == 2)
        .unwrap_or(n);
    let dense = |s: Slot| {
        let u = uses[s as usize].max(1);
        u64::from(f.slots[s as usize].0) * 1_000_000 / u64::from(u)
    };
    order[top..].sort_by_key(|&s| std::cmp::Reverse(dense(s)));
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lir::{K, Loc, Part};

    const fn m(s: Slot) -> Mem {
        Mem::Frame(s, 0)
    }

    const fn int(n: u8) -> Part {
        Part {
            loc: Loc::Int(n),
            k: K::I64,
            signed: false,
        }
    }

    /// Slots of 128 bytes (big enough to share), each filled whole and
    /// read by a call.
    fn func(slots: usize, insts: Vec<Inst>) -> Func {
        Func {
            slots: vec![(128, 8); slots],
            insts,
            opt: true,
            ..Func::default()
        }
    }

    const fn fill(s: Slot) -> Inst {
        Inst::Zero {
            dst: m(s),
            size: 128,
        }
    }

    fn call(args: &[Slot]) -> Inst {
        Inst::Call {
            callee: Callee::Sym(0),
            args: args.iter().map(|&s| (int(0), V::M(m(s)))).collect(),
            rets: Vec::new(),
            sret: None,
            variadic: false,
            pop: 0,
        }
    }

    fn offs(f: &Func) -> Vec<u32> {
        layout(f, &|_| false, (0, 16, 0), 8).off
    }

    /// Slots never live at once share; at `-O0` none does.
    #[test]
    fn disjoint_slots_share() {
        let insts = vec![
            fill(0),
            call(&[0]),
            fill(1),
            call(&[1]),
            Inst::Ret { vals: Vec::new() },
        ];
        let f = func(2, insts);
        assert_eq!(offs(&f), vec![0, 0]);
        let o0 = Func { opt: false, ..f };
        assert_eq!(offs(&o0), vec![0, 128]);
    }

    #[test]
    fn overlapping_slots_do_not_share() {
        let f = func(
            2,
            vec![
                fill(0),
                fill(1),
                call(&[0]),
                call(&[1]),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        assert_eq!(offs(&f), vec![0, 128]);
    }

    /// A store no read follows still lands in memory at the block's end
    /// (an emitter may hold it back): what the next block holds is apart.
    #[test]
    fn held_back_store_keeps_its_bytes() {
        let f = func(
            2,
            vec![
                Inst::Mov {
                    k: K::I64,
                    dst: m(0),
                    src: V::I(1),
                },
                fill(1),
                Inst::Jmp(0),
                Inst::Label(0),
                call(&[1]),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        let f = Func { labels: 1, ..f };
        assert_eq!(offs(&f), vec![0, 128]);
    }

    /// An addressed slot keeps its own bytes.
    #[test]
    fn addressed_slot_stays_apart() {
        let mut f = func(
            3,
            vec![
                Inst::Addr {
                    dst: m(2),
                    src: m(0),
                },
                call(&[2]),
                fill(1),
                call(&[1]),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        f.slots[2] = (8, 8);
        assert_eq!(offs(&f), vec![0, 128, 256]);
    }

    /// Callee 0 keeps no pointer it is passed in `Int(0)`.
    fn keeps_none(f: &mut Func) {
        let mut c = Caps::default();
        c.insert(0, vec![(Loc::Int(0), Cap::No)]);
        f.caps = Some(std::sync::Arc::new(c));
    }

    /// A slot whose address only goes to a call that keeps no pointer
    /// shares; its range runs from where the address is taken to the
    /// call.
    #[test]
    fn unescaped_address_shares() {
        let insts = vec![
            fill(0),
            Inst::Addr {
                dst: m(2),
                src: m(0),
            },
            call(&[2]),
            fill(1),
            call(&[1]),
            Inst::Ret { vals: Vec::new() },
        ];
        let mut f = func(3, insts);
        f.slots[2] = (8, 8);
        keeps_none(&mut f);
        let o = offs(&f);
        assert_eq!(o[0], o[1]);
        // Stored to memory: kept apart.
        f.insts.insert(
            3,
            Inst::Mov {
                k: K::I64,
                dst: Mem::Ind(2, 0),
                src: V::M(m(2)),
            },
        );
        let o = offs(&f);
        assert_ne!(o[0], o[1]);
    }

    /// A pointer derived from the address carries it: a slot written
    /// before its last use stays apart.
    #[test]
    fn derived_pointer_extends_range() {
        let insts = vec![
            Inst::Addr {
                dst: m(2),
                src: m(0),
            },
            Inst::Bin {
                op: crate::lir::Bin::Add,
                k: K::I64,
                signed: false,
                dst: m(3),
                a: V::M(m(2)),
                b: V::I(8),
            },
            Inst::Mov {
                k: K::I64,
                dst: Mem::Ind(3, 0),
                src: V::I(1),
            },
            fill(1),
            call(&[1]),
            Inst::Mov {
                k: K::I64,
                dst: Mem::Frame(4, 0),
                src: V::M(Mem::Ind(3, 0)),
            },
            call(&[4]),
            Inst::Ret { vals: Vec::new() },
        ];
        let mut f = func(5, insts);
        f.slots[2] = (8, 8);
        f.slots[3] = (8, 8);
        f.slots[4] = (8, 8);
        keeps_none(&mut f);
        let o = offs(&f);
        assert_ne!(o[0], o[1]);
    }

    /// Roots share only with roots of the same offsets; the frame lists
    /// them all.
    #[test]
    fn roots_share_by_offsets() {
        let root = |i: u32, s: Slot, o: u32| Inst::Addr {
            dst: Mem::Frame(4, 8 * (2 + i)),
            src: Mem::Frame(s, o),
        };
        let mut f = func(
            5,
            vec![
                root(0, 0, 0),
                root(1, 1, 0),
                root(2, 2, 8),
                fill(0),
                call(&[0]),
                fill(1),
                call(&[1]),
                fill(2),
                call(&[2]),
                fill(3),
                call(&[3]),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        f.slots[4] = (40, 8);
        f.gc_frame = Some((4, vec![0, 1, 2]));
        let o = offs(&f);
        assert_eq!(o[0], o[1]);
        assert_ne!(o[0], o[2]);
        assert_ne!(o[0], o[3]);
        assert_ne!(o[2], o[3]);
    }

    /// A dead root's clearing before a return goes; one before a call (a
    /// collection reads every root) stays, and so does one of an
    /// addressed slot.
    #[test]
    fn dead_stores_before_return() {
        let mut f = func(
            3,
            vec![
                Inst::Addr {
                    dst: Mem::Frame(2, 16),
                    src: m(0),
                },
                fill(0),
                call(&[]),
                Inst::Mov {
                    k: K::I64,
                    dst: m(1),
                    src: V::I(0),
                },
                fill(0),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        f.gc_frame = Some((2, vec![0]));
        let g = dse(&f, 8).expect("dropped");
        assert_eq!(g.insts.len(), 4);
        assert!(matches!(g.insts[1], Inst::Zero { .. }));
        assert!(matches!(g.insts[3], Inst::Ret { .. }));
        // Reached through a pointer: kept.
        f.gc_frame = None;
        f.insts[0] = Inst::Addr {
            dst: m(2),
            src: m(0),
        };
        let g = dse(&f, 8).expect("dropped");
        assert_eq!(g.insts.len(), 5);
    }

    /// A root's zeroing that every path overwrites before a call or a
    /// read goes; with a call before one overwrite it stays.
    #[test]
    fn dead_store_across_blocks() {
        let mut f = func(
            3,
            vec![
                Inst::Addr {
                    dst: Mem::Frame(2, 16),
                    src: m(0),
                },
                fill(0),
                Inst::Br {
                    c: V::I(1),
                    t: 0,
                    f: 1,
                },
                Inst::Label(0),
                fill(0),
                call(&[]),
                Inst::Ret { vals: Vec::new() },
                Inst::Label(1),
                fill(0),
                call(&[]),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        f.labels = 2;
        f.gc_frame = Some((2, vec![0]));
        let g = dse(&f, 8).expect("dropped");
        assert_eq!(g.insts.len(), f.insts.len() - 1);
        assert!(matches!(g.insts[1], Inst::Br { .. }));
        f.insts.insert(8, call(&[]));
        let g = dse(&f, 8);
        assert!(g.is_none());
    }

    /// A parameter only read through stays the callee's; one stored to
    /// memory, or passed to a call that may keep it, does not.
    #[test]
    fn kept_parameters() {
        let p = |s: Slot| Inst::Param {
            part: int(0),
            dst: m(s),
        };
        let reader = Func {
            sym: 1,
            slots: vec![(8, 8), (8, 8)],
            insts: vec![
                p(0),
                Inst::Mov {
                    k: K::I64,
                    dst: m(1),
                    src: V::M(Mem::Ind(0, 8)),
                },
                Inst::Ret { vals: Vec::new() },
            ],
            ..Func::default()
        };
        let mut keeper = reader.clone();
        keeper.sym = 2;
        keeper.insts[1] = Inst::Mov {
            k: K::I64,
            dst: Mem::Ind(0, 8),
            src: V::M(m(0)),
        };
        let mut passer = reader.clone();
        passer.sym = 3;
        passer.insts[1] = call(&[0]);
        // Each callee passed a frame address.
        let caller = |sym: u32| {
            let mut f = func(2, Vec::new());
            f.insts = vec![
                Inst::Addr {
                    dst: m(1),
                    src: m(0),
                },
                Inst::Call {
                    callee: Callee::Sym(sym),
                    args: vec![(int(0), V::M(m(1)))],
                    rets: Vec::new(),
                    sret: None,
                    variadic: false,
                    pop: 0,
                },
            ];
            f
        };
        let (a, b, c) = (caller(1), caller(2), caller(3));
        let c = caps(&[&reader, &keeper, &passer, &a, &b, &c], &[], 8);
        assert_eq!(c.get(&1), Some(&vec![(Loc::Int(0), Cap::No)]));
        assert_eq!(c.get(&2), None);
        assert_eq!(c.get(&3), None);
    }

    /// A read that goes with its dead result keeps no store alive, but a
    /// later block reading the slot does.
    #[test]
    fn dead_reader_keeps_store() {
        let mov = |d: Slot, src: V| Inst::Mov {
            k: K::I64,
            dst: m(d),
            src,
        };
        let mut f = func(
            2,
            vec![
                mov(0, V::I(1)),
                mov(1, V::M(m(0))),
                Inst::Jmp(0),
                Inst::Label(0),
                mov(1, V::I(2)),
                call(&[0, 1]),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        f.labels = 1;
        f.slots = vec![(8, 8); 2];
        let g = dse(&f, 8).expect("dropped");
        assert_eq!(g.insts.len(), f.insts.len() - 1);
        assert!(matches!(g.insts[0], Inst::Mov { src: V::I(1), .. }));
        assert!(matches!(g.insts[1], Inst::Jmp(0)));
    }
}
