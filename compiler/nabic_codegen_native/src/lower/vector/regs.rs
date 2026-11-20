//! Registers for the virtual ones of a vector loop.

#[allow(clippy::wildcard_imports)]
use crate::lower::vector::*;

/// Registers for the virtual ones: `pre`'s and `pinned` for the whole
/// loop, the rest from their definition to their last use. `None`: more
/// than `n` at once.
pub(crate) fn allocate(
    pre: &mut [VOp],
    body: &mut [VOp],
    pinned: &[VReg],
    accs: &mut [Acc],
    n: usize,
) -> Option<u8> {
    let mut last: HashMap<VReg, usize> = HashMap::default();
    for (i, op) in body.iter().enumerate() {
        for r in uses(op) {
            last.insert(r, i);
        }
    }
    let mut map: HashMap<VReg, VReg> = HashMap::default();
    let mut free: Vec<VReg> = (0..n as VReg).rev().collect();
    for r in pinned {
        if !map.contains_key(r) {
            map.insert(*r, free.pop()?);
        }
    }
    let mut used = n - free.len();
    for (i, op) in body.iter().enumerate() {
        // Operands dying here free their registers for the result.
        for r in uses(op) {
            if last.get(&r) == Some(&i) && !pinned.contains(&r) {
                if let Some(p) = map.get(&r) {
                    if !free.contains(p) {
                        free.push(*p);
                    }
                }
            }
        }
        if let Some(d) = def(op) {
            if let std::collections::hash_map::Entry::Vacant(e) = map.entry(d) {
                let p = free.pop()?;
                e.insert(p);
                used = used.max(n - free.len());
                if !last.contains_key(&d) {
                    free.push(p);
                }
            }
        }
    }
    let m = |r: &mut VReg| *r = map[r];
    for op in pre.iter_mut().chain(body.iter_mut()) {
        map_regs(op, &mut |r| m(r));
    }
    for a in accs {
        for r in &mut a.regs {
            m(r);
        }
    }
    Some(used as u8)
}

pub(crate) fn uses(op: &VOp) -> Vec<VReg> {
    match *op {
        VOp::Store { src, .. } | VOp::StoreTrunc { src, .. } => vec![src],
        VOp::MaskStore { src, mask, .. } => vec![src, mask],
        VOp::Bin { a, b, .. } => vec![a, b],
        VOp::Un { a, .. } | VOp::Shift { a, .. } | VOp::Bail { a } => vec![a],
        VOp::Dot { acc, a, b, .. } => vec![acc, a, b],
        VOp::Fma { a, b, c, .. } => vec![a, b, c],
        VOp::Load { .. } | VOp::LoadExt { .. } | VOp::Splat { .. } | VOp::Const { .. } => {
            Vec::new()
        }
    }
}

pub(crate) const fn def(op: &VOp) -> Option<VReg> {
    match *op {
        VOp::Load { dst, .. }
        | VOp::LoadExt { dst, .. }
        | VOp::Splat { dst, .. }
        | VOp::Const { dst, .. }
        | VOp::Bin { dst, .. }
        | VOp::Un { dst, .. }
        | VOp::Shift { dst, .. }
        | VOp::Dot { acc: dst, .. }
        | VOp::Fma { dst, .. } => Some(dst),
        VOp::Store { .. } | VOp::StoreTrunc { .. } | VOp::MaskStore { .. } | VOp::Bail { .. } => {
            None
        }
    }
}

pub(crate) fn map_regs(op: &mut VOp, f: &mut impl FnMut(&mut VReg)) {
    match op {
        VOp::Load { dst, .. }
        | VOp::LoadExt { dst, .. }
        | VOp::Splat { dst, .. }
        | VOp::Const { dst, .. } => f(dst),
        VOp::Store { src, .. } | VOp::StoreTrunc { src, .. } => f(src),
        VOp::MaskStore { src, mask, .. } => {
            f(src);
            f(mask);
        }
        VOp::Bin { dst, a, b, .. } => {
            f(dst);
            f(a);
            f(b);
        }
        VOp::Un { dst, a, .. } | VOp::Shift { dst, a, .. } => {
            f(dst);
            f(a);
        }
        VOp::Bail { a } => f(a),
        VOp::Dot { acc, a, b, .. } => {
            f(acc);
            f(a);
            f(b);
        }
        VOp::Fma { dst, a, b, c } => {
            f(dst);
            f(a);
            f(b);
            f(c);
        }
    }
}
