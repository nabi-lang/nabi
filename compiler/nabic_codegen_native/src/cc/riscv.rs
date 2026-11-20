//! The RISC-V convention.

#[allow(clippy::wildcard_imports)]
use crate::cc::*;

/// RISC-V register and stack state, as LLVM's `CC_RISCV` allocates.
pub(crate) struct Riscv {
    /// Next `a` / `fa` register.
    gi: u8,
    fi: u8,
    stack: u32,
    rv32: bool,
}

impl Riscv {
    pub(crate) fn stack(&mut self, size: u32, align: u32) -> u32 {
        self.stack = self.stack.next_multiple_of(align);
        self.stack += size;
        self.stack - size
    }

    /// One scalar. LP64D: a fixed float in an `fa` register while one is
    /// left, else as an integer. Integers in an `a` register, else an
    /// XLEN stack slot. rv32: an 8-byte scalar takes two `a` registers,
    /// the last one and stack offset 0, or 8 aligned stack bytes; as a
    /// variadic, an even register first.
    pub(crate) fn one(&mut self, k: K, fixed: bool) -> Loc {
        if !self.rv32 && k.is_float() && fixed && self.fi < 8 {
            self.fi += 1;
            return Loc::Fp(self.fi - 1);
        }
        if self.rv32 && k.size() == 8 {
            if !fixed && self.gi % 2 == 1 {
                self.gi += 1;
            }
            if self.gi < 8 {
                let r = self.gi;
                self.gi += 1;
                if self.gi < 8 {
                    self.gi += 1;
                } else {
                    // The high word: nothing is on the stack yet.
                    self.stack = 4;
                }
                return Loc::Int(r);
            }
            return Loc::Stack(self.stack(8, 8));
        }
        if self.gi < 8 {
            self.gi += 1;
            return Loc::Int(self.gi - 1);
        }
        let w = if self.rv32 { 4 } else { 8 };
        Loc::Stack(self.stack(w, w))
    }
}

/// RISC-V: every leaf an argument of its own (a first-class aggregate),
/// `Riscv::one`. A result of at most two XLEN values (`a0 a1`, `fa0
/// fa1`), else through the hidden pointer in `a0`.
pub(crate) fn assign_riscv(
    params: &[Shape],
    ret: Option<&Shape>,
    sret: bool,
    variadic_from: Option<usize>,
    rv32: bool,
) -> SigLocs {
    let ret = match ret {
        None if sret => None,
        None => Some(Vec::new()),
        Some(r) => {
            let words: u32 = r
                .leaves
                .iter()
                .map(|l| if rv32 && l.k.size() == 8 { 2 } else { 1 })
                .sum();
            (words <= 2).then(|| {
                let mut st = Riscv {
                    gi: 0,
                    fi: 0,
                    stack: 0,
                    rv32,
                };
                r.leaves
                    .iter()
                    .map(|l| part(st.one(l.k, true), *l))
                    .collect()
            })
        }
    };
    let mut st = Riscv {
        gi: u8::from(ret.is_none()),
        fi: 0,
        stack: 0,
        rv32,
    };
    let mut out = Vec::new();
    for (i, p) in params.iter().enumerate() {
        let fixed = variadic_from.is_none_or(|v| i < v);
        // rv64 `i128`: a variadic one in an even register pair, one
        // wholly on the stack 16-aligned (`CC_RISCVAssign2XLen`).
        if !rv32 && !p.aggregate && p.align == 16 && p.leaves.len() == 2 {
            if !fixed && st.gi % 2 == 1 {
                st.gi += 1;
            }
            if st.gi >= 8 {
                st.stack = st.stack.next_multiple_of(16);
            }
        }
        out.push(
            p.leaves
                .iter()
                .map(|l| part(st.one(l.k, fixed), *l))
                .collect(),
        );
    }
    SigLocs {
        params: out,
        ret,
        stack: st.stack.next_multiple_of(if rv32 { 4 } else { 8 }),
        pop: 0,
    }
}
