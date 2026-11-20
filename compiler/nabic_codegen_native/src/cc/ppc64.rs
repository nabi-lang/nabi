//! The powerpc64 `ELFv2` convention.

#[allow(clippy::wildcard_imports)]
use crate::cc::*;

/// powerpc64 `ELFv2`, as LLVM's `LowerCall_64SVR4` places arguments: each
/// scalar takes the next doubleword of the parameter save area (an `f32`
/// of a float array 4 bytes, the array then rounded to a doubleword; a
/// 16-aligned aggregate from an even one);
/// one of the first eight is in its GPR `r3 + n` (a float there too when
/// variadic, or out of FPRs), the stack otherwise; fixed floats in the
/// next of `f1`-`f13`. A result in `r3`-`r10` / `f1`-`f8`, else through
/// the hidden pointer, the first argument.
pub(crate) fn assign_ppc64(
    params: &[Shape],
    ret: Option<&Shape>,
    sret: bool,
    variadic_from: Option<usize>,
) -> SigLocs {
    let ret = match ret {
        None if sret => None,
        None => Some(Vec::new()),
        Some(r) => {
            let ints = r.leaves.iter().filter(|l| !l.k.is_float()).count();
            let fps = r.leaves.len() - ints;
            (ints <= 8 && fps <= 8).then(|| {
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
    let mut at = if ret.is_none() { 8u32 } else { 0 };
    let mut fi = 0u8;
    let mut out = Vec::new();
    for (i, p) in params.iter().enumerate() {
        let fixed = variadic_from.is_none_or(|v| i < v);
        let packed = p.array && p.leaves.first().is_some_and(|l| l.k == K::F32);
        if p.array && p.align >= 16 {
            at = at.next_multiple_of(16);
        }
        let mut parts = Vec::new();
        for l in &p.leaves {
            let loc = if l.k.is_float() && fixed && fi < 13 {
                fi += 1;
                Loc::Fp(fi - 1)
            } else if at < 64 {
                Loc::Int((at / 8) as u8)
            } else {
                Loc::Stack(at)
            };
            parts.push(part(loc, *l));
            at += if packed { 4 } else { 8 };
        }
        at = at.next_multiple_of(8);
        out.push(parts);
    }
    SigLocs {
        params: out,
        ret,
        stack: at,
        pop: 0,
    }
}
