//! AAPCS32, with VFP registers for floats.

#[allow(clippy::wildcard_imports)]
use crate::cc::*;

/// AAPCS32 register and stack state, as LLVM's `CC_ARM_AAPCS_VFP` and
/// `CC_ARM_AAPCS_Custom_Aggregate` allocate.
pub(crate) struct Arm {
    /// Next core register (`r0`-`r3`, never back-filled).
    ncrn: u8,
    /// `s0`-`s15` taken (a `d` register is its two halves).
    s: [bool; 16],
    stack: u32,
    /// Floats go to the VFP registers (else as integers).
    vfp: bool,
}

impl Arm {
    /// The first `n` free consecutive `s` registers (`w` = 2: `d`
    /// registers) from 0.
    pub(crate) fn block(&self, n: usize, w: usize) -> Option<usize> {
        (0..=16 / w - n).find(|&i| self.s[i * w..(i + n) * w].iter().all(|t| !t))
    }

    pub(crate) fn take_s(&mut self, i: usize, n: usize, w: usize) {
        self.s[i * w..(i + n) * w].fill(true);
    }

    pub(crate) fn stack(&mut self, size: u32, align: u32) -> u32 {
        self.stack = self.stack.next_multiple_of(align);
        self.stack += size;
        self.stack - size
    }

    /// A scalar of its own: floats in a VFP register, integers in core
    /// ones (8 bytes in an even pair), else the stack.
    pub(crate) fn one(&mut self, k: K) -> Loc {
        if self.vfp && k.is_float() {
            let w = k.size() as usize / 4;
            if let Some(i) = self.block(1, w) {
                self.take_s(i, 1, w);
                return Loc::Fp(i as u8);
            }
            self.s = [true; 16];
            return Loc::Stack(self.stack(k.size(), k.size()));
        }
        if k.size() == 8 {
            self.ncrn = self.ncrn.next_multiple_of(2);
            if self.ncrn < 4 {
                self.ncrn += 2;
                return Loc::Int(self.ncrn - 2);
            }
            return Loc::Stack(self.stack(8, 8));
        }
        if self.ncrn < 4 {
            self.ncrn += 1;
            return Loc::Int(self.ncrn - 1);
        }
        Loc::Stack(self.stack(4, 4))
    }

    /// A block: an HFA in consecutive VFP registers, integer words in
    /// consecutive core registers (split with the stack while it is
    /// empty), else all on the stack, its first member aligned to 4 or 8.
    pub(crate) fn block_of(&mut self, p: &Shape) -> Vec<Part> {
        let k0 = p.leaves.first().map_or(K::I32, |l| l.k);
        let fp = self.vfp && k0.is_float() && p.leaves.iter().all(|l| l.k == k0);
        let mut out = Vec::new();
        if fp {
            let (n, w) = (p.leaves.len(), k0.size() as usize / 4);
            if let Some(i) = self.block(n, w) {
                self.take_s(i, n, w);
                for (j, l) in p.leaves.iter().enumerate() {
                    out.push(part(Loc::Fp((i + j) as u8), *l));
                }
                return out;
            }
            self.s = [true; 16];
            let mut align = k0.size();
            for l in &p.leaves {
                out.push(part(Loc::Stack(self.stack(l.k.size(), align)), *l));
                align = 1;
            }
            return out;
        }
        // Integer words (narrow members promoted, floats as their bits).
        let words: u32 = p.leaves.iter().map(|l| l.k.size().div_ceil(4)).sum();
        let first8 = if p.byval {
            p.align >= 8
        } else {
            k0.size() == 8
        };
        if first8 {
            self.ncrn = (self.ncrn + 1).min(4) & !1;
        }
        let free = 4 - u32::from(self.ncrn);
        if words <= free || self.stack == 0 {
            for l in &p.leaves {
                let n = l.k.size().div_ceil(4) as u8;
                let loc = if self.ncrn + n <= 4 {
                    self.ncrn += n;
                    Loc::Int(self.ncrn - n)
                } else {
                    self.ncrn = 4;
                    Loc::Stack(self.stack(4 * u32::from(n), 4))
                };
                out.push(part(loc, *l));
            }
            return out;
        }
        self.ncrn = 4;
        let mut align = if first8 { 8 } else { 4 };
        for l in &p.leaves {
            let size = l.k.size().max(4);
            out.push(part(Loc::Stack(self.stack(size, align)), *l));
            align = 1;
        }
        out
    }
}

/// AAPCS32: `vfp` floats in `s` / `d` registers (`Loc::Fp` numbers the
/// `s` register of an `f32`, the `d` register of an `f64`), else in
/// integer registers. A result in `r0`-`r3` (an 8-byte integer in an
/// even pair) and VFP registers, else through the hidden pointer, the
/// first integer argument.
pub(crate) fn assign_arm(params: &[Shape], ret: Option<&Shape>, sret: bool, vfp: bool) -> SigLocs {
    let ret = match ret {
        None if sret => None,
        None => Some(Vec::new()),
        Some(r) => {
            let mut st = Arm {
                ncrn: 0,
                s: [false; 16],
                stack: 0,
                vfp,
            };
            let parts: Vec<Part> = r.leaves.iter().map(|l| part(st.one(l.k), *l)).collect();
            parts
                .iter()
                .all(|p| !matches!(p.loc, Loc::Stack(_)))
                .then_some(parts)
        }
    };
    let mut st = Arm {
        ncrn: u8::from(ret.is_none()),
        s: [false; 16],
        stack: 0,
        vfp,
    };
    let mut out = Vec::new();
    for p in params {
        if p.byval || p.block {
            out.push(st.block_of(p));
        } else {
            out.push(p.leaves.iter().map(|l| part(st.one(l.k), *l)).collect());
        }
    }
    SigLocs {
        params: out,
        ret,
        stack: st.stack.next_multiple_of(4),
        pop: 0,
    }
}
