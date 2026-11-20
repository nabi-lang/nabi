//! The wasm32 convention: a parameter per leaf.

#[allow(clippy::wildcard_imports)]
use crate::cc::*;

/// wasm32: a parameter per leaf, a single result leaf or the hidden
/// pointer; variadic arguments at their alignment (4 at least) in the
/// buffer, as clang lays out a `va_list`.
pub(crate) fn assign_wasm(
    params: &[Shape],
    ret: Option<&Shape>,
    sret: bool,
    variadic_from: Option<usize>,
) -> SigLocs {
    let ret = match ret {
        None if sret => None,
        None => Some(Vec::new()),
        Some(r) if r.leaves.len() <= 1 => {
            Some(r.leaves.iter().map(|l| part(Loc::Int(0), *l)).collect())
        }
        Some(_) => None,
    };
    let (mut n, mut stack) = (0usize, 0u32);
    let mut out = Vec::new();
    for (i, p) in params.iter().enumerate() {
        let va = variadic_from.is_some_and(|v| i >= v);
        let mut parts = Vec::new();
        for l in &p.leaves {
            let loc = if va {
                let size = l.k.size().max(4);
                stack = stack.next_multiple_of(size) + size;
                Loc::Stack(stack - size)
            } else {
                n += 1;
                Loc::Int(u8::try_from(n - 1).expect("wasm32: at most 256 parameter scalars"))
            };
            parts.push(part(loc, *l));
        }
        out.push(parts);
    }
    SigLocs {
        params: out,
        ret,
        stack: stack.next_multiple_of(8),
        pop: 0,
    }
}
