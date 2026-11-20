//! Instruction text, in Intel syntax as llvm-objdump prints it.

#[allow(clippy::wildcard_imports)]
use crate::isa::x64::enc::*;

/// A recorded imm32 of an `sz` operation: sign-extended to 64 bits, else
/// shown unsigned (as objdump does).
pub(crate) fn imm32(sz: Sz, imm: i32) -> Op {
    if sz.w() {
        Op::I(i64::from(imm))
    } else {
        Op::I(i64::from(imm as u32))
    }
}

pub(crate) const fn rm_op(rm: Rm, sz: Sz) -> Op {
    match rm {
        Rm::R(g) => Op::G(g, sz),
        Rm::M(m) => Op::M(m, sz.ptr()),
    }
}

pub(crate) const fn xrm_op(rm: Xrm, ptr: Ptr) -> Op {
    match rm {
        Xrm::X(x) => Op::X(x, false),
        Xrm::M(m) => Op::M(m, ptr),
    }
}

pub(crate) fn hex(v: i64) -> String {
    if v < 0 {
        format!("-{:#x}", v.unsigned_abs())
    } else {
        format!("{v:#x}")
    }
}

pub(crate) fn gpr_name(g: Gpr, sz: Sz) -> String {
    const Q: [&str; 8] = ["rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi"];
    const B: [&str; 8] = ["al", "cl", "dl", "bl", "spl", "bpl", "sil", "dil"];
    let n = g.0 as usize;
    if n >= 8 {
        let s = match sz {
            Sz::B => "b",
            Sz::W => "w",
            Sz::D => "d",
            Sz::Q => "",
        };
        return format!("r{n}{s}");
    }
    match sz {
        Sz::Q => Q[n].to_owned(),
        Sz::D => format!("e{}", &Q[n][1..]),
        Sz::W => Q[n][1..].to_owned(),
        Sz::B => B[n].to_owned(),
    }
}

pub(crate) fn op_text(o: Op) -> String {
    match o {
        Op::G(g, sz) => gpr_name(g, sz),
        Op::X(x, false) => format!("xmm{}", x.0),
        Op::X(x, true) => format!("ymm{}", x.0),
        Op::I(v) => hex(v),
        Op::M(m, ptr) => {
            let p = match ptr {
                Ptr::None => "",
                Ptr::B => "byte ptr ",
                Ptr::W => "word ptr ",
                Ptr::D => "dword ptr ",
                Ptr::Q => "qword ptr ",
                Ptr::X => "xmmword ptr ",
                Ptr::Y => "ymmword ptr ",
            };
            let a = match m {
                Addr::Bd(b, 0) | Addr::BdRel(b, ..) => format!("[{}]", gpr_name(b, Sz::Q)),
                Addr::Bd(b, d) if d < 0 => {
                    format!(
                        "[{} - {:#x}]",
                        gpr_name(b, Sz::Q),
                        i64::from(d).unsigned_abs()
                    )
                }
                Addr::Bd(b, d) => format!("[{} + {d:#x}]", gpr_name(b, Sz::Q)),
                Addr::Bi(b, i, 0) => {
                    format!("[{} + {}]", gpr_name(b, Sz::Q), gpr_name(i, Sz::Q))
                }
                Addr::Bi(b, i, sc) => format!(
                    "[{} + {}*{}]",
                    gpr_name(b, Sz::Q),
                    1 << sc,
                    gpr_name(i, Sz::Q)
                ),
                Addr::Rip(..) => "[rip]".to_owned(),
                Addr::Abs(seg, d) => {
                    let s = if seg == Seg::Fs { "fs" } else { "gs" };
                    format!("{s}:[{d:#x}]")
                }
            };
            format!("{p}{a}")
        }
        Op::No | Op::Rel | Op::Rel8 => String::new(),
    }
}
