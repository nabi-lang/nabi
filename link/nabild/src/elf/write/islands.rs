//! POWER long-branch islands.

#[allow(clippy::wildcard_imports)]
use crate::elf::write::*;

/// The reach of a POWER `bl`, less room for what layout adds.
pub(crate) const BRANCH_REACH: i64 = (1 << 25) - (2 << 20);

/// Islands go at most this far apart.
pub(crate) const ISLAND_SPACING: u64 = 16 << 20;

/// POWER: long-branch stubs for calls a `bl` cannot reach, while the code
/// is larger than its range. An island goes before each `.text` input
/// section that would end more than `ISLAND_SPACING` past the last one,
/// and one at the end; each far call goes through the island nearest it,
/// which jumps TOC-relative to the target. Addresses are estimates from
/// section sizes, hence the margin.
pub(crate) fn plan_islands(out: &mut Out<'_>, secs: &mut [OSec]) {
    let l = out.l;
    let mut base: HashMap<usize, u64> = HashMap::default();
    let (mut at, mut start) = (0u64, None);
    for (i, s) in secs.iter().enumerate() {
        if s.alloc() && s.flags & SHF_EXECINSTR != 0 {
            at = at.next_multiple_of(s.align.max(1));
            start.get_or_insert(at);
            base.insert(i, at);
            at += s.size;
        }
    }
    let Some(ti) = secs
        .iter()
        .position(|s| s.synth == Synth::Input && s.name == ".text")
    else {
        return;
    };
    if (at - start.unwrap_or(0)) as i64 <= BRANCH_REACH {
        return;
    }
    let mut code: Vec<usize> = base.keys().copied().collect();
    code.sort_unstable();
    let mut sec_of: HashMap<AtomId, usize> = HashMap::default();
    for &i in &code {
        for &a in &secs[i].atoms {
            sec_of.insert(a, i);
        }
    }
    let est_atom =
        |a: AtomId| -> Option<i64> { Some((base[sec_of.get(&a)?] + out.addrs[a as usize]) as i64) };
    let plt = secs.iter().position(|s| s.synth == Synth::Plt);
    // The target of a call, and the addend a stub adds to its address.
    let target = |t: Target, a: i64| -> Option<(i64, i64)> {
        match t {
            Target::Atom(x) => Some((est_atom(x)? + a, a)),
            Target::Sym(id) => match l.syms.get(id).def {
                Def::Defined { atom, off } => {
                    let e = a + ppc64::local_entry(l.ext(id).other) as i64;
                    Some((est_atom(atom)? + off as i64 + e, e))
                }
                Def::Dylib { .. } => {
                    let k = *out.plt_at.get(&id)? as i64;
                    Some((base[&plt?] as i64 + ppc64::PLT_ENTRY as i64 * k + a, a))
                }
                _ => None,
            },
        }
    };
    // Island places: before an atom (its index), or at the end.
    let text = &secs[ti];
    let mut cuts: Vec<(usize, i64)> = Vec::new();
    let mut last = 0u64;
    for (k, &a) in text.atoms.iter().enumerate() {
        let off = out.addrs[a as usize];
        if k > 0 && off + l.atoms[a as usize].size - last > ISLAND_SPACING {
            cuts.push((k, (base[&ti] + off) as i64));
            last = off;
        }
    }
    cuts.push((text.atoms.len(), (base[&ti] + text.size) as i64));
    let mut islands: Vec<Island> = cuts.iter().map(|_| Island::default()).collect();
    let mut stub_at: HashMap<(usize, Target, i64), usize> = HashMap::default();
    let mut far = HashMap::default();
    for &i in &code {
        for &a in &secs[i].atoms {
            let Some(from) = est_atom(a) else { continue };
            for (ri, r) in l.atoms[a as usize].relocs.iter().enumerate() {
                if r.kind != ppc64::REL24 {
                    continue;
                }
                let p = from + i64::from(r.off);
                let Some((t, add)) = target(r.target, r.addend) else {
                    continue;
                };
                if (t - p).abs() <= BRANCH_REACH {
                    continue;
                }
                let (j, d) = cuts
                    .iter()
                    .enumerate()
                    .map(|(j, c)| (j, (c.1 - p).abs()))
                    .min_by_key(|x| x.1)
                    .unwrap_or((0, i64::MAX));
                if d > BRANCH_REACH {
                    continue;
                }
                let n = islands[j].stubs.len();
                let k = *stub_at.entry((j, r.target, add)).or_insert(n);
                if k == n {
                    islands[j].stubs.push((r.target, add));
                }
                far.insert((a, ri), (j, k));
            }
        }
    }
    if far.is_empty() {
        return;
    }
    // `.text` again, with the islands in it.
    let text = &mut secs[ti];
    let mut at = 0u64;
    let mut next = 0;
    for (k, &a) in text.atoms.iter().enumerate() {
        if next < cuts.len() && cuts[next].0 == k {
            at = at.next_multiple_of(16);
            islands[next].off = at;
            at += ppc64::LONG_BRANCH * islands[next].stubs.len() as u64;
            next += 1;
        }
        let x = &l.atoms[a as usize];
        at = at.next_multiple_of(1 << x.align);
        out.addrs[a as usize] = at;
        at += x.size;
    }
    at = at.next_multiple_of(16);
    islands[cuts.len() - 1].off = at;
    at += ppc64::LONG_BRANCH * islands[cuts.len() - 1].stubs.len() as u64;
    text.size = at;
    text.align = text.align.max(16);
    out.islands = islands;
    out.far = far;
}

/// The stubs of [`plan_islands`].
pub(crate) fn write_islands(out: &Out<'_>, buf: &mut [u8]) {
    let Some(text) = out
        .secs
        .iter()
        .find(|s| s.synth == Synth::Input && s.name == ".text")
    else {
        return;
    };
    for island in &out.islands {
        for (k, &(t, add)) in island.stubs.iter().enumerate() {
            let at = text.off + island.off + ppc64::LONG_BRANCH * k as u64;
            let to = (out.addr(t) as i64).wrapping_add(add) as u64;
            ppc64::write_long_branch(buf, at as usize, to, out.got_base());
        }
    }
}

/// The address of POWER far call stub `(island, stub)`.
pub(crate) fn island_stub(out: &Out<'_>, (j, k): (usize, usize)) -> u64 {
    let text = out
        .secs
        .iter()
        .find(|s| s.synth == Synth::Input && s.name == ".text")
        .map_or(0, |s| s.addr);
    text + out.islands[j].off + ppc64::LONG_BRANCH * k as u64
}
