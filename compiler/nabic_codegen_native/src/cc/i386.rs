//! The i386 convention: everything on the stack.

#[allow(clippy::wildcard_imports)]
use crate::cc::*;

/// i386: every parameter in 4-byte stack slots (8-byte scalars in two),
/// a `byval` copy at its words; the hidden result pointer first. A
/// result in `eax edx ecx` (an 8-byte integer in two neighbours) and one
/// float in `st0`, else through the hidden pointer.
pub(crate) fn assign_i386(
    conv: Conv,
    params: &[Shape],
    ret: Option<&Shape>,
    sret: bool,
) -> SigLocs {
    let ret = match ret {
        None if sret => None,
        None => Some(Vec::new()),
        Some(r) => {
            let words: u32 = r
                .leaves
                .iter()
                .filter(|l| !l.k.is_float())
                .map(|l| l.k.size().div_ceil(4))
                .sum();
            let fps = r.leaves.iter().filter(|l| l.k.is_float()).count();
            (words <= 3 && fps <= 1).then(|| {
                let mut gi = 0u8;
                r.leaves
                    .iter()
                    .map(|l| {
                        let loc = if l.k.is_float() {
                            Loc::Fp(0)
                        } else {
                            gi += l.k.size().div_ceil(4) as u8;
                            Loc::Int(gi - l.k.size().div_ceil(4) as u8)
                        };
                        part(loc, *l)
                    })
                    .collect()
            })
        }
    };
    let mut stack = if ret.is_none() { 4 } else { 0 };
    let mut out = Vec::new();
    for p in params {
        let base = stack;
        let mut parts = Vec::new();
        for l in &p.leaves {
            let at = if p.byval {
                base + l.off
            } else {
                stack.next_multiple_of(4)
            };
            parts.push(part(Loc::Stack(at), *l));
            stack = stack.max(at + l.k.size().max(4));
        }
        if p.byval {
            stack = base + p.leaves.last().map_or(0, |l| l.off + 4);
        }
        out.push(parts);
    }
    let pop = if conv == Conv::X86 && ret.is_none() {
        4
    } else {
        0
    };
    SigLocs {
        params: out,
        ret,
        stack: stack.next_multiple_of(4),
        pop,
    }
}
