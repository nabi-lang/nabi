//! The s390x convention.

#[allow(clippy::wildcard_imports)]
use crate::cc::*;

/// s390x: integers in `r2`-`r6`, floats in `f0 f2 f4 f6`, the rest in
/// 8-byte stack slots (narrow values right-justified). A result in `r2`-`r5`
/// / `f0 f2 f4 f6`, else through the hidden pointer in `r2`.
pub(crate) fn assign_s390x(params: &[Shape], ret: Option<&Shape>, sret: bool) -> SigLocs {
    let ret = match ret {
        None if sret => None,
        None => Some(Vec::new()),
        Some(r) => {
            let ints = r.leaves.iter().filter(|l| !l.k.is_float()).count();
            let fps = r.leaves.len() - ints;
            (ints <= 4 && fps <= 4).then(|| {
                let (mut gi, mut fi) = (0u8, 0u8);
                r.leaves
                    .iter()
                    .map(|l| {
                        let loc = if l.k.is_float() {
                            fi += 1;
                            Loc::Fp(fi - 1)
                        } else {
                            gi += 1;
                            Loc::Int(gi - 1)
                        };
                        part(loc, *l)
                    })
                    .collect()
            })
        }
    };
    let (mut gi, mut fi, mut stack) = (u8::from(ret.is_none()), 0u8, 0u32);
    let mut out = Vec::new();
    for p in params {
        let mut parts = Vec::new();
        for l in &p.leaves {
            let loc = if l.k.is_float() && fi < 4 {
                fi += 1;
                Loc::Fp(fi - 1)
            } else if !l.k.is_float() && gi < 5 {
                gi += 1;
                Loc::Int(gi - 1)
            } else {
                stack += 8;
                Loc::Stack(stack - 8)
            };
            parts.push(part(loc, *l));
        }
        out.push(parts);
    }
    SigLocs {
        params: out,
        ret,
        stack,
        pop: 0,
    }
}
