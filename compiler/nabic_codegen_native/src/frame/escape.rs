//! Address flow: where the frame addresses a function takes may go.

#[allow(clippy::wildcard_imports)]
use crate::frame::*;

/// Where the frame addresses a function takes may go. A source is a
/// slot whose address it takes (numbered as the slot), or a pointer
/// parameter (after the slots). Flow-insensitive: a slot holds what any
/// instruction may put in it.
pub(crate) struct Flow {
    /// Per slot: the sources a pointer in it may come from, sorted.
    from: Vec<Vec<u32>>,
    /// Per source: the pointer may outlive what the function sees of it:
    /// stored to memory or to a slot whose address is taken, passed to
    /// a call that may keep it, a code pointer.
    pub(crate) esc: Vec<bool>,
    /// Per source: may be returned.
    ret: Vec<bool>,
}

/// What `callee` does with a pointer argument at `loc` (`None`: may
/// keep it).
pub(crate) fn cap_of(caps: Option<&Caps>, callee: &Callee, loc: Loc) -> Option<Cap> {
    let Callee::Sym(s) = callee else {
        return None;
    };
    caps?.get(s)?.iter().find(|c| c.0 == loc).map(|c| c.1)
}

/// Whether `Addr { dst, src: x }` lists root `x` in the shadow-stack
/// frame (the collector, not the code, goes through it).
pub(crate) fn listing(f: &Func, dst: &Mem, x: Slot) -> bool {
    matches!((dst, &f.gc_frame), (Mem::Frame(d, _), Some((fs, rs))) if d == fs && rs.contains(&x))
}

/// Add `from[t]` to `from[d]`; whether it grew.
pub(crate) fn join(from: &mut [Vec<u32>], d: Slot, t: Slot) -> bool {
    let (d, t) = (d as usize, t as usize);
    if d == t || from[t].is_empty() {
        return false;
    }
    let src = std::mem::take(&mut from[t]);
    let dst = &mut from[d];
    let len = dst.len();
    if src.iter().all(|x| dst.binary_search(x).is_ok()) {
        from[t] = src;
        return false;
    }
    dst.extend_from_slice(&src);
    dst.sort_unstable();
    dst.dedup();
    let grew = dst.len() != len;
    from[t] = src;
    grew
}

/// The frame slots of a scalar operand.
pub(crate) const fn slot_of(v: &V) -> Option<Slot> {
    match v {
        V::M(Mem::Frame(s, _)) => Some(*s),
        _ => None,
    }
}

/// `f`'s address flow; `params`: the slots its pointer parameters land
/// in, sources from `f.slots.len()` on. `None` when it has no source.
pub(crate) fn flow(f: &Func, params: &[Slot], caps: Option<&Caps>) -> Option<Flow> {
    let n = f.slots.len();
    let mut from: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut addressed = vec![false; n];
    let mut esc = vec![false; n + params.len()];
    let mut any = false;
    for i in &f.insts {
        if let Inst::Addr {
            dst,
            src: Mem::Frame(x, _),
        } = i
        {
            addressed[*x as usize] = true;
            if listing(f, dst, *x) {
                continue;
            }
            any = true;
            match dst {
                Mem::Frame(d, _) => from[*d as usize].push(*x),
                Mem::Ind(..) => esc[*x as usize] = true,
            }
        }
    }
    if !any && params.is_empty() {
        return None;
    }
    for (k, &d) in params.iter().enumerate() {
        from[d as usize].push((n + k) as u32);
    }
    for v in &mut from {
        v.sort_unstable();
        v.dedup();
    }
    // What moves a pointer from slot to slot, `(to, from)`; then to a
    // fixed point.
    let mut edges: Vec<(Slot, Slot)> = Vec::new();
    for i in &f.insts {
        let (dst, a, b) = match i {
            Inst::Addr {
                dst,
                src: Mem::Ind(p, _),
            } => (dst, Some(*p), None),
            Inst::Mov { dst, src, .. } => (dst, slot_of(src), None),
            Inst::Copy {
                dst,
                src: Mem::Frame(t, _),
                ..
            } => (dst, Some(*t), None),
            Inst::Bin { dst, a, b, .. } | Inst::Ovf { dst, a, b, .. } => {
                (dst, slot_of(a), slot_of(b))
            }
            Inst::Un { dst, a, .. } | Inst::Conv { dst, a, .. } => (dst, slot_of(a), None),
            Inst::Call {
                callee, args, rets, ..
            } => {
                for (p, v) in args {
                    let Some(t) = slot_of(v) else { continue };
                    if cap_of(caps, callee, p.loc) == Some(Cap::Ret) {
                        for (_, m) in rets {
                            if let Mem::Frame(d, _) = m {
                                edges.push((*d, t));
                            }
                        }
                    }
                }
                continue;
            }
            _ => continue,
        };
        if let Mem::Frame(d, _) = dst {
            edges.extend([a, b].into_iter().flatten().map(|t| (*d, t)));
        }
    }
    let mut changed = true;
    while changed {
        changed = false;
        for &(d, t) in &edges {
            changed |= join(&mut from, d, t);
        }
    }
    let mut ret = vec![false; esc.len()];
    let mark = |esc: &mut [bool], t: Option<Slot>| {
        for &x in t.map_or(&[][..], |t| &from[t as usize]) {
            esc[x as usize] = true;
        }
    };
    for i in &f.insts {
        match i {
            Inst::Mov {
                dst: Mem::Ind(..),
                src,
                ..
            } => mark(&mut esc, slot_of(src)),
            Inst::Copy {
                dst: Mem::Ind(..),
                src: Mem::Frame(t, _),
                ..
            } => mark(&mut esc, Some(*t)),
            Inst::Addr {
                dst: Mem::Ind(..),
                src: Mem::Ind(p, _),
            } => mark(&mut esc, Some(*p)),
            Inst::Bin {
                dst: Mem::Ind(..),
                a,
                b,
                ..
            }
            | Inst::Ovf {
                dst: Mem::Ind(..),
                a,
                b,
                ..
            } => {
                mark(&mut esc, slot_of(a));
                mark(&mut esc, slot_of(b));
            }
            Inst::Un {
                dst: Mem::Ind(..),
                a,
                ..
            }
            | Inst::Conv {
                dst: Mem::Ind(..),
                a,
                ..
            } => mark(&mut esc, slot_of(a)),
            Inst::AtomicStore { src: v, .. }
            | Inst::AtomicRmw { val: v, .. }
            | Inst::Cas { new: v, .. } => mark(&mut esc, slot_of(v)),
            Inst::Call { callee, args, .. } => {
                if let Callee::Ptr(Mem::Frame(t, _)) = callee {
                    mark(&mut esc, Some(*t));
                }
                for (p, v) in args {
                    if cap_of(caps, callee, p.loc).is_none() {
                        mark(&mut esc, slot_of(v));
                    }
                }
            }
            Inst::Ret { vals } => {
                for (_, v) in vals {
                    mark(&mut ret, slot_of(v));
                }
            }
            _ => {}
        }
    }
    // A pointer in memory something else may read.
    for (d, a) in addressed.iter().enumerate() {
        if *a {
            mark(&mut esc, Some(d as Slot));
        }
    }
    // A slot's own address returned dangles: kept all the same.
    for x in 0..n {
        esc[x] |= ret[x];
    }
    Some(Flow { from, esc, ret })
}

/// The sources `i` reads or writes through pointers (`flow`).
pub(crate) fn reached(i: &Inst, fl: &Flow, ms: &mut Vec<Slot>, out: &mut Vec<Slot>) {
    ms.clear();
    inds(i, ms);
    for &q in ms.iter() {
        out.extend_from_slice(&fl.from[q as usize]);
    }
    let mut add = |t: Slot| out.extend_from_slice(&fl.from[t as usize]);
    match i {
        Inst::AtomicLoad {
            ptr: Mem::Frame(q, _),
            ..
        }
        | Inst::AtomicStore {
            ptr: Mem::Frame(q, _),
            ..
        }
        | Inst::AtomicRmw {
            ptr: Mem::Frame(q, _),
            ..
        }
        | Inst::Cas {
            ptr: Mem::Frame(q, _),
            ..
        } => add(*q),
        Inst::Call { args, .. } => {
            for (_, v) in args {
                if let Some(t) = slot_of(v) {
                    add(t);
                }
            }
        }
        // Its bases are in its block.
        Inst::VLoop(l) => add(l.io),
        _ => {}
    }
}

/// The slots holding the pointers `i`'s memory operands go through.
pub(crate) fn inds(i: &Inst, out: &mut Vec<Slot>) {
    fn m(out: &mut Vec<Slot>, x: &Mem) {
        if let Mem::Ind(q, _) = x {
            out.push(*q);
        }
    }
    fn v(out: &mut Vec<Slot>, x: &V) {
        if let V::M(x) = x {
            m(out, x);
        }
    }
    match i {
        Inst::Label(_) | Inst::Jmp(_) | Inst::Trap | Inst::Line(..) | Inst::VLoop(_) => {}
        Inst::Br { c, .. } => v(out, c),
        Inst::V(x) => {
            m(out, &x.a);
            m(out, &x.b);
            m(out, &x.dst);
        }
        Inst::Mov { dst, src, .. } => {
            v(out, src);
            m(out, dst);
        }
        Inst::Copy { dst, src, .. } | Inst::Addr { dst, src } => {
            m(out, src);
            m(out, dst);
        }
        Inst::Zero { dst, .. }
        | Inst::Sym { dst, .. }
        | Inst::Tls { dst, .. }
        | Inst::Param { dst, .. } => m(out, dst),
        Inst::Bin { dst, a, b, .. } | Inst::Ovf { dst, a, b, .. } | Inst::Cmp { dst, a, b, .. } => {
            v(out, a);
            v(out, b);
            m(out, dst);
        }
        Inst::Un { dst, a, .. } | Inst::Conv { dst, a, .. } => {
            v(out, a);
            m(out, dst);
        }
        Inst::Fma { dst, a, b, c, .. } => {
            v(out, a);
            v(out, b);
            v(out, c);
            m(out, dst);
        }
        Inst::AtomicLoad { dst, ptr, .. } => {
            m(out, ptr);
            m(out, dst);
        }
        Inst::AtomicStore { ptr, src, .. } => {
            m(out, ptr);
            v(out, src);
        }
        Inst::AtomicRmw { dst, ptr, val, .. } => {
            m(out, ptr);
            v(out, val);
            m(out, dst);
        }
        Inst::Cas {
            dst,
            ptr,
            expected,
            new,
            ..
        } => {
            m(out, ptr);
            v(out, expected);
            v(out, new);
            m(out, dst);
        }
        Inst::Call {
            callee,
            args,
            rets,
            sret,
            ..
        } => {
            if let Callee::Ptr(x) = callee {
                m(out, x);
            }
            for (_, x) in args {
                v(out, x);
            }
            for (_, x) in rets {
                m(out, x);
            }
            if let Some(x) = sret {
                m(out, x);
            }
        }
        Inst::Ret { vals } => {
            for (_, x) in vals {
                v(out, x);
            }
        }
    }
}

/// Each function's pointer parameters it keeps no longer than a call:
/// the convention's (`byref`, from `Func::byref`), and of the others a
/// call here passes a frame address, what their bodies show. Callees
/// first: a parameter is taken as not kept until shown otherwise, so
/// recursion keeps what it can. `ptr` is the pointer size.
#[must_use]
pub fn caps(funcs: &[&Func], byref: &[(SymId, Loc, Cap)], ptr: u32) -> Caps {
    let mut out: Caps = HashMap::default();
    for &(s, loc, cap) in byref {
        let v = out.entry(s).or_default();
        match v.iter_mut().find(|c| c.0 == loc) {
            Some(c) => {
                if cap == Cap::Ret {
                    c.1 = Cap::Ret;
                }
            }
            None => v.push((loc, cap)),
        }
    }
    let conv = out.clone();
    let by_sym: HashMap<SymId, usize> = funcs.iter().enumerate().map(|(k, f)| (f.sym, k)).collect();
    // The parameters worth a look, by function; who calls each.
    let mut want: Vec<Vec<Loc>> = vec![Vec::new(); funcs.len()];
    let mut callers: Vec<Vec<usize>> = vec![Vec::new(); funcs.len()];
    let mut addrs: Vec<Slot> = Vec::new();
    for (k, f) in funcs.iter().enumerate() {
        addrs.clear();
        for i in &f.insts {
            match i {
                Inst::Addr {
                    dst: Mem::Frame(d, _),
                    src: Mem::Frame(x, _),
                } if !listing(f, &Mem::Frame(*d, 0), *x) => addrs.push(*d),
                Inst::Call {
                    callee: Callee::Sym(s),
                    args,
                    ..
                } => {
                    let Some(&c) = by_sym.get(s) else { continue };
                    if callers[c].last() != Some(&k) {
                        callers[c].push(k);
                    }
                    for (p, v) in args {
                        let t = slot_of(v);
                        if p.k.is_float()
                            || p.k.size() != ptr
                            || !t.is_some_and(|t| addrs.contains(&t))
                            || want[c].contains(&p.loc)
                            || conv.get(s).is_some_and(|v| v.iter().any(|c| c.0 == p.loc))
                        {
                            continue;
                        }
                        want[c].push(p.loc);
                        out.entry(*s).or_default().push((p.loc, Cap::No));
                    }
                }
                _ => {}
            }
        }
    }
    let mut work: Vec<usize> = (0..funcs.len())
        .rev()
        .filter(|&k| !want[k].is_empty())
        .collect();
    let mut queued = vec![false; funcs.len()];
    for &k in &work {
        queued[k] = true;
    }
    while let Some(k) = work.pop() {
        queued[k] = false;
        let f = funcs[k];
        let ps: Vec<(Loc, Slot)> = f
            .insts
            .iter()
            .filter_map(|i| match i {
                Inst::Param {
                    part,
                    dst: Mem::Frame(d, _),
                } if want[k].contains(&part.loc) => Some((part.loc, *d)),
                _ => None,
            })
            .collect();
        let slots: Vec<Slot> = ps.iter().map(|p| p.1).collect();
        let fl = flow(f, &slots, Some(&out));
        let n = f.slots.len();
        let mut now: Vec<(Loc, Cap)> = conv.get(&f.sym).cloned().unwrap_or_default();
        for (j, &(loc, _)) in ps.iter().enumerate() {
            let Some(fl) = &fl else { break };
            if !fl.esc[n + j] {
                now.push((loc, if fl.ret[n + j] { Cap::Ret } else { Cap::No }));
            }
        }
        if out.get(&f.sym) != Some(&now) {
            out.insert(f.sym, now);
            for &c in &callers[k] {
                if !queued[c] && !want[c].is_empty() {
                    queued[c] = true;
                    work.push(c);
                }
            }
        }
    }
    out.retain(|_, v| !v.is_empty());
    out
}
