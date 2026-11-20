//! LIR -> RISC-V machine code: RV64GC (LP64D) and RV32IMAC (ILP32, soft
//! float), in ELF objects. Above -O0 every instruction with a 16-bit (`C`)
//! form takes it (`compact`).
//!
//! Frame: `ra` and `s0` saved just below the incoming `sp`, `s0` = the
//! incoming `sp` (the CFA), then the slots and the outgoing arguments at
//! `sp`, whose first 16 bytes are scratch outside a call. Nothing moves
//! `sp` in the body. Each instruction loads its operands into fixed
//! caller-saved registers, computes and stores: `t0` holds the first
//! value (rv32: an 8-byte one in `t0 t1`, low word first), `t2` (`t2 t3`)
//! the second, `t4` a pointer, `t5` a store's base, `t6` the address of a
//! far offset; `a0`-`a7` are scratch inside one instruction, `ft0`-`ft2`
//! compute floats. Above -O0 values live in registers no sequence
//! touches: `s1`-`s11` and `fs0`-`fs11` (callee-saved, stored below the
//! saved `s0` when used; on rv32 an often used 8-byte integer in two),
//! `ft3`-`ft11`, `fa0`-`fa7` and on riscv64 `a0`-`a7` (written by argument
//! and result moves, done in order, and `a0`-`a5` inside the few
//! sequences `clobber64` names).
//! Code is position independent (`auipc` pairs; riscv64
//! reaches symbols of other images through the GOT). rv32 has no FPU and
//! no 8-byte atomics: floats, 8-byte division and atomics call the
//! compiler-rt / libatomic helpers.

pub mod enc;

mod arith;
mod call;
mod frame;
mod mem;
mod select;
mod vector;

use crate::lir::{
    Bin, Callee, Cc, Conv, Func, Inst, K, Label, Loc, Mem, NO_SLOT, Order, Part, Rmw, Un, V, scalar,
};
use crate::obj::{FnRange, Obj, RelKind, Reloc, Saves, Sect};
use crate::regalloc::{Alloc, Regs};
use enc::{Amo, Cond, FArith, FCmp, IntTy, Ls, Op, Rm, X};

const ZERO: u32 = 0;
const RA: u32 = 1;
const SP: u32 = 2;
const TP: u32 = 4;
const T0: u32 = 5;
const T1: u32 = 6;
const T2: u32 = 7;
const S0: u32 = 8;
const A0: u32 = 10;
const A1: u32 = 11;
const A2: u32 = 12;
const A3: u32 = 13;
const A4: u32 = 14;
const A5: u32 = 15;
const A6: u32 = 16;
const A7: u32 = 17;
const T3: u32 = 28;
const T4: u32 = 29;
const T5: u32 = 30;
const T6: u32 = 31;
/// `ft0`-`ft2`, `fa0`.
const F0: u32 = 0;
const F1: u32 = 1;
const FA0: u32 = 10;
/// Value registers: `a0`-`a7` only on riscv64 (rv32 pairs and helpers use
/// them as scratch); rv32 has no float registers.
const REGS64: Regs = Regs {
    int_saved: &[9, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27],
    int_free: &[],
    int_arg: &[16, 17, 15, 14, 13, 12, 11, 10],
    fp_saved: &[8, 9, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27],
    fp_free: &[3, 4, 5, 6, 7, 28, 29, 30, 31],
    fp_arg: &[10, 11, 12, 13, 14, 15, 16, 17],
    int_args: &[10, 11, 12, 13, 14, 15, 16, 17],
    fp_args: &[10, 11, 12, 13, 14, 15, 16, 17],
    int_rets: &[10, 11],
    fp_rets: &[10, 11],
    sret: 10,
    word: 8,
    ptr: 8,
    fp_size: 8,
    vec: 0,
    vec_saved: false,
    costly_imm: None,
    pairs: false,
    saved_pairs: false,
    saves_all: false,
    clobber: Some(clobber64),
};

/// What `i`'s sequence takes of `a0`-`a5` on riscv64 (beyond the moves of
/// calls, returns and parameters): overflow checks, bit counts and byte
/// swaps, conversions, atomics.
const fn clobber64(i: &Inst) -> u64 {
    match i {
        Inst::Ovf { .. } | Inst::Un { .. } | Inst::Conv { .. } => 0xf << A0,
        Inst::AtomicLoad { .. }
        | Inst::AtomicStore { .. }
        | Inst::AtomicRmw { .. }
        | Inst::Cas { .. } => 0x3f << A0,
        _ => 0,
    }
}
const REGS32: Regs = Regs {
    int_saved: &[9, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27],
    int_free: &[],
    int_arg: &[],
    fp_saved: &[],
    fp_free: &[],
    fp_arg: &[],
    int_args: &[10, 11, 12, 13, 14, 15, 16, 17],
    fp_args: &[],
    int_rets: &[10, 11],
    fp_rets: &[],
    sret: 10,
    word: 4,
    ptr: 4,
    fp_size: 0,
    vec: 0,
    vec_saved: false,
    costly_imm: None,
    pairs: true,
    saved_pairs: false,
    saves_all: false,
    clobber: None,
};
/// Stack probe interval.
const PAGE: u32 = 4096;
/// Scratch bytes at `sp`.
const SCRATCH: u32 = 16;

// `OP` operations.
const ADD: Op = Op::Add;
const SUB: Op = Op::Sub;
const SLL: Op = Op::Sll;
const SLT: Op = Op::Slt;
const SLTU: Op = Op::Sltu;
const XOR: Op = Op::Xor;
const SRL: Op = Op::Srl;
const SRA: Op = Op::Sra;
const OR: Op = Op::Or;
const AND: Op = Op::And;
const MUL: Op = Op::Mul;
const MULH: Op = Op::Mulh;
const MULHU: Op = Op::Mulhu;
const DIV: Op = Op::Div;
const DIVU: Op = Op::Divu;
const REM: Op = Op::Rem;
const REMU: Op = Op::Remu;

// Branch conditions.
const BEQ: Cond = Cond::EQ;
const BNE: Cond = Cond::NE;
const BLT: Cond = Cond::LT;
const BGE: Cond = Cond::GE;
const BLTU: Cond = Cond::LTU;
const BGEU: Cond = Cond::GEU;

/// `libatomic`'s `__ATOMIC_SEQ_CST`.
const SEQ_CST: i64 = 5;

/// Every prologue (rv64): `addi sp, sp, -16; sd ra, 8(sp); sd s0, 0(sp);
/// addi s0, sp, 16`, as the unwind tables say.
#[cfg(test)]
const PROLOGUE64: [u32; 4] = [0xff01_0113, 0x0011_3423, 0x0081_3023, 0x0101_0413];

const fn fits12(v: i64) -> bool {
    v >= -2048 && v < 2048
}

/// `i` as a `k` value extended by `signed` to 64 bits.
const fn ext(i: i64, k: K, signed: bool) -> i64 {
    let bits = k.bits();
    if bits >= 64 {
        i
    } else if signed {
        (i << (64 - bits)) >> (64 - bits)
    } else {
        i & ((1i64 << bits) - 1)
    }
}

/// The integer kind of `k`'s size (a float's bits).
const fn int_k(k: K) -> K {
    match k {
        K::F32 => K::I32,
        K::F64 => K::I64,
        k => k,
    }
}

struct Em<'a> {
    obj: &'a mut Obj,
    rv32: bool,
    start: u32,
    a: enc::Asm,
    relocs: Vec<(u32, RelKind, u32)>,
    /// `(index of a jal, or a branch if, label)`.
    fixes: Vec<(usize, Label, bool)>,
    labels: Vec<Option<usize>>,
    slot_off: Vec<u32>,
    /// Bytes below the saved `ra` / `s0`.
    frame: u32,
    /// `Func::va_save`.
    va: u32,
    lines: Vec<(u32, u32, u32, u32)>,
    ra: Alloc,
    opt: bool,
    /// Callee-saved registers stored below the saved `s0`, `(register,
    /// float)`, each in 8 bytes, the first highest.
    saved: Vec<(u32, bool)>,
    saves: Saves,
    /// Which conditional jumps are one branch, in order (`br`).
    plan: Option<Vec<bool>>,
    /// Each conditional jump so far: `(index, words, label)`.
    sites: Vec<(usize, u8, Label)>,
    /// Above `-O0`: parts written once and read at most once
    /// (`lir::waitable`).
    waits: crate::lir::Waits,
    /// A comparison the next branch makes itself in place of reading its
    /// result, which then never goes out (`skipped`).
    flags: Option<(Cond, u32, u32)>,
    skipped: crate::lir::Waits,
    /// Once compacted (`compact`): each word's byte offset, and the end.
    off: Option<Vec<u32>>,
}

/// Emit `f` into `obj`'s text.
pub fn emit(obj: &mut Obj, f: &Func) {
    let rv32 = obj.rv32;
    let mut out = SCRATCH;
    for i in &f.insts {
        if let Inst::Call { args, .. } = i {
            for (p, _) in args {
                if let Loc::Stack(o) = p.loc {
                    out = out.max(o + p.k.size().max(if rv32 { 4 } else { 8 }));
                }
            }
        }
    }
    let (owned, ra) = if f.opt {
        let regs = if rv32 { &REGS32 } else { &REGS64 };
        let (g, a) = crate::regalloc::prepare(f, regs);
        (Some(g), a)
    } else {
        (None, Alloc::default())
    };
    let f = owned.as_ref().unwrap_or(f);
    let crate::frame::Layout {
        off: slot_off,
        end: at,
        live,
    } = crate::frame::layout(
        f,
        &|s| ra.gone(s),
        (out.next_multiple_of(16), 16, 0),
        if rv32 { 4 } else { 8 },
    );
    let runs = crate::roots::frame(obj, f, &slot_off, if rv32 { 4 } else { 8 }, &|s| ra.gone(s));
    let saved: Vec<(u32, bool)> = (ra.saved.iter().map(|&r| (r, false)))
        .chain(ra.saved_fp.iter().map(|&r| (r, true)))
        .collect();
    let frame = at.next_multiple_of(16) + (8 * saved.len() as u32).next_multiple_of(16);
    // Compacted code (above -O0) needs only 2-byte alignment.
    let start = obj.sect(Sect::Text).align_to(if f.opt { 2 } else { 4 });
    let text = obj.asm.is_some();
    // Above `-O0` conditional jumps to labels are laid out twice: first
    // all an inverted branch over a `jal`, then one branch wherever
    // `relax::relax` finds it reaches (all long again should a target
    // move out of reach).
    let mut plan: Option<Vec<bool>> = None;
    let mut tried = false;
    let waits = if f.opt {
        crate::lir::waitable(f).0
    } else {
        crate::lir::Waits::default()
    };
    loop {
        let mut e = Em {
            obj: &mut *obj,
            rv32,
            start,
            a: enc::Asm::new(text),
            relocs: Vec::new(),
            fixes: Vec::new(),
            labels: vec![None; f.labels as usize],
            slot_off: slot_off.clone(),
            frame,
            va: f.va_save,
            lines: Vec::new(),
            ra: ra.clone(),
            opt: f.opt,
            saved: saved.clone(),
            saves: Saves::default(),
            plan: plan.take(),
            sites: Vec::new(),
            waits: waits.clone(),
            flags: None,
            skipped: crate::lir::Waits::default(),
            off: None,
        };
        e.prologue();
        let mut track = crate::debug::locs::Track::new(f, live.clone());
        for (n, i) in f.insts.iter().enumerate() {
            if let Some(t) = &mut track {
                t.at(n, 4 * e.a.pos() as u32, &mut e.lines);
            }
            if let Some(rep) = runs.as_ref().and_then(|r| r.at(n)) {
                for i in rep {
                    e.inst(i, None);
                }
                continue;
            }
            e.inst(i, f.insts.get(n + 1));
        }
        // `s0` is `sp + frame + 16`.
        if f.opt && !tried {
            tried = true;
            let p = crate::relax::relax(&e.sites, &e.labels, 1, &|at, to| {
                (-4096..4096).contains(&((to as i64 - at as i64) * 4))
            });
            if p.contains(&true) {
                plan = Some(p);
                continue;
            }
        }
        if f.opt && !e.compact() {
            plan = None;
            continue;
        }
        if let (Some(t), Some(o)) = (&mut track, &e.off) {
            t.remap(&|x| o[x as usize / 4]);
        }
        let off = |s: u32| e.slot_off[s as usize] as i32 - frame as i32 - 16;
        let end = e.byte(e.a.pos());
        let locs = track
            .map(|t| {
                t.finish(
                    &e.ra,
                    (&e.skipped, &[]),
                    (end, if rv32 { 4 } else { 8 }),
                    &off,
                    &debug_reg,
                )
            })
            .unwrap_or_default();
        let debug = f.debug.as_ref().map(|d| {
            let offs = d
                .vars
                .iter()
                .map(|v| if v.slot == NO_SLOT { 0 } else { off(v.slot) })
                .collect();
            (d.clone(), offs)
        });
        if e.finish(f.sym, debug) {
            if let Some(r) = obj.fns.last_mut() {
                r.locs = locs;
            }
            return;
        }
        plan = None;
    }
}

impl Em<'_> {
    fn ins(&mut self, i: enc::Ins) {
        self.a.push(i);
    }

    fn here_off(&self) -> u32 {
        self.start + 4 * self.a.pos() as u32
    }

    /// The byte offset of word `n` (`compact` moves them).
    fn byte(&self, n: usize) -> u32 {
        self.off.as_ref().map_or(4 * n as u32, |o| o[n])
    }

    /// Each instruction with a 16-bit form (`enc::compress`) in it: the
    /// branches and jumps to labels and the short branches inside
    /// sequences where theirs reaches (`relax::relax_each`, from all
    /// short), the rest wherever a relocation does not patch them. The
    /// branches and jumps are patched to the code as laid out, the
    /// offsets recorded so far moved with it. False: a planned branch is
    /// out of reach.
    fn compact(&mut self) -> bool {
        let n = self.a.code.len();
        let rv64 = !self.rv32;
        let mut fixed = vec![false; n];
        for r in &self.relocs {
            let i = ((r.0 - self.start) / 4) as usize;
            fixed[i] = true;
            // `call`: the `auipc` and its `jalr`.
            if r.1 == RelKind::RvCall {
                fixed[i + 1] = true;
            }
        }
        // `(word, target word)` of each branch and jump, in order.
        let mut fix = vec![None; n];
        for &(at, l, _) in &self.fixes {
            fix[at] = Some(self.labels[l as usize].expect("bound label"));
        }
        let mut sites: Vec<(usize, usize)> = Vec::new();
        let mut small = vec![false; n];
        for i in 0..n {
            let w = self.a.code[i];
            if let Some(t) = fix[i] {
                sites.push((i, t));
            } else if !fixed[i] && matches!(w & 0x7f, 0x63 | 0x6f) {
                // A branch or `jal` patched already (sequences, loops).
                let t = i as i64 + i64::from(self.a.target(i)) / 4;
                sites.push((i, t as usize));
            } else {
                small[i] = !fixed[i] && enc::compress(w, rv64).is_some();
            }
        }
        // Halfwords before each word, every site long.
        let mut h = vec![0usize; n + 1];
        for i in 0..n {
            let site =
                fix[i].is_some() || (!fixed[i] && matches!(self.a.code[i] & 0x7f, 0x63 | 0x6f));
            h[i + 1] = h[i] + if small[i] && !site { 1 } else { 2 };
        }
        let reach: Vec<Option<(i32, i32)>> = sites
            .iter()
            .map(|&(i, _)| enc::c_reach(self.a.code[i], rv64))
            .collect();
        let rs: Vec<(usize, u8, Label)> = sites
            .iter()
            .enumerate()
            .map(|(k, &(i, _))| (h[i], 2, k as Label))
            .collect();
        let tl: Vec<Option<usize>> = sites.iter().map(|&(_, t)| Some(h[t])).collect();
        let ok = crate::relax::relax_each(&rs, &tl, 1, &|k, at, to| {
            let d = (to as i64 - at as i64) * 2;
            reach[k].is_some_and(|(lo, hi)| i64::from(lo) <= d && d < i64::from(hi))
        });
        for (k, &(i, _)) in sites.iter().enumerate() {
            small[i] = ok[k];
        }
        let mut off = vec![0u32; n + 1];
        for i in 0..n {
            off[i + 1] = off[i] + if small[i] { 2 } else { 4 };
        }
        for &(i, t) in &sites {
            let d = off[t] as i64 - off[i] as i64;
            let jal = self.a.code[i] & 0x7f == 0x6f;
            if jal {
                assert!(enc::reaches_jal(d), "RISC-V: a function's jump past 1 MiB");
            } else if !(-4096..4096).contains(&d) {
                return false;
            }
            self.a.repoint(i, d as i32);
            debug_assert!(!small[i] || enc::compress(self.a.code[i], rv64).is_some());
        }
        self.fixes.clear();
        // The prologue: four instructions, all compressed or none.
        self.saves.short = small.len() >= 4 && small[..4].iter().all(|&b| b);
        let start = self.start;
        for r in &mut self.relocs {
            r.0 = start + off[((r.0 - start) / 4) as usize];
        }
        for l in &mut self.lines {
            l.0 = off[l.0 as usize / 4];
        }
        self.saves.at = off[self.saves.at as usize / 4];
        self.a.compact(small, rv64);
        self.off = Some(off);
        true
    }

    /// A relocation of the next instruction.
    fn reloc(&mut self, kind: RelKind, sym: u32) {
        let off = self.here_off();
        self.relocs.push((off, kind, sym));
    }

    /// Into `obj`, unless a short branch's target is out of its reach.
    fn finish(mut self, sym: u32, debug: Option<(crate::lir::FnDebug, Vec<i32>)>) -> bool {
        // Compacted code is patched already.
        for (at, l, short) in std::mem::take(&mut self.fixes) {
            let target = self.labels[l as usize].expect("bound label");
            let d = (target as i64 - at as i64) * 4;
            if short {
                if !(-4096..4096).contains(&d) {
                    return false;
                }
                self.a.patch_branch(at, d as i32);
                continue;
            }
            assert!(enc::reaches_jal(d), "RISC-V: a function's jump past 1 MiB");
            self.a.patch_jal(at, d as i32);
        }
        let len = self.byte(self.a.pos());
        let start = self.start;
        let bytes = self.a.bytes();
        if self.obj.asm.is_some() {
            self.list(sym, &bytes);
        }
        let text = self.obj.sect(Sect::Text);
        text.data.extend_from_slice(&bytes);
        for (off, kind, s) in std::mem::take(&mut self.relocs) {
            text.relocs.push(Reloc { off, kind, sym: s });
        }
        let lines = std::mem::take(&mut self.lines);
        self.obj.syms[sym as usize].def = Some((Sect::Text, start, len));
        self.obj.fns.push(FnRange {
            locs: Vec::new(),
            sym,
            start,
            len,
            lines,
            debug,
            saves: std::mem::take(&mut self.saves),
        });
        true
    }

    /// The listing of this function into `obj.asm`.
    fn list(&mut self, sym: u32, bytes: &[u8]) {
        let name = crate::asm::sym_name(self.obj, &self.obj.syms[sym as usize].name);
        let labels = self
            .labels
            .iter()
            .enumerate()
            .filter_map(|(l, at)| at.map(|at| (self.byte(at), l as u32)))
            .collect();
        let relocs: Vec<_> = self
            .relocs
            .iter()
            .map(|&(off, kind, s)| (off - self.start, kind, s))
            .collect();
        let l = crate::asm::Listing {
            name: &name,
            code: bytes,
            labels,
            relocs: &relocs,
        };
        // The ELF writer's labels at the `auipc` of each `pcrel_lo`,
        // numbered in the text section's order.
        let before = self.obj.sects.get(&Sect::Text).map_or(0, |t| {
            t.relocs
                .iter()
                .filter(|r| r.kind == RelKind::RvPcLoI)
                .count()
        });
        let mut los: Vec<u32> = relocs
            .iter()
            .filter(|r| r.1 == RelKind::RvPcLoI)
            .map(|r| r.0 - 4)
            .collect();
        los.sort_unstable();
        let syms: Vec<(u32, String)> = los
            .into_iter()
            .enumerate()
            .map(|(n, at)| (at, format!(".Lpcrel_hi{}", before + n)))
            .collect();
        let lines = self.a.lines_as(&l, &syms, self.obj.src.is_some());
        crate::asm::function(self.obj, &l, &lines, crate::asm::Bytes::Words);
    }

    const fn xlen(&self) -> u32 {
        if self.rv32 { 32 } else { 64 }
    }

    /// The word kind (a pointer).
    const fn word(&self) -> K {
        if self.rv32 { K::I32 } else { K::I64 }
    }

    /// An 8-byte value in a register pair (rv32).
    const fn pair(&self, k: K) -> bool {
        self.rv32 && k.size() == 8
    }

    // Encodings.

    fn alu(&mut self, o: Op, rd: u32, rs1: u32, rs2: u32) {
        self.ins(enc::op(o, X(rd), X(rs1), X(rs2)));
    }

    fn addi(&mut self, rd: u32, rs: u32, imm: i32) {
        self.ins(enc::addi(X(rd), X(rs), imm));
    }

    fn mv(&mut self, rd: u32, rs: u32) {
        if rd != rs {
            self.addi(rd, rs, 0);
        }
    }

    fn andi(&mut self, rd: u32, rs: u32, imm: i32) {
        self.ins(enc::andi(X(rd), X(rs), imm));
    }

    fn xori(&mut self, rd: u32, rs: u32, imm: i32) {
        self.ins(enc::xori(X(rd), X(rs), imm));
    }

    fn slli(&mut self, rd: u32, rs: u32, sh: u32) {
        self.ins(enc::slli(X(rd), X(rs), sh));
    }

    fn srli(&mut self, rd: u32, rs: u32, sh: u32) {
        self.ins(enc::srli(X(rd), X(rs), sh));
    }

    fn srai(&mut self, rd: u32, rs: u32, sh: u32) {
        self.ins(enc::srai(X(rd), X(rs), sh));
    }

    /// `sltiu rd, rs, 1`.
    fn seqz(&mut self, rd: u32, rs: u32) {
        self.ins(enc::sltiu(X(rd), X(rs), 1));
    }

    /// `sltu rd, zero, rs`.
    fn snez(&mut self, rd: u32, rs: u32) {
        self.alu(SLTU, rd, ZERO, rs);
    }

    fn lui(&mut self, rd: u32, imm20: u32) {
        self.ins(enc::lui(X(rd), imm20));
    }

    /// `rd` = `v` (rv32: its low 32 bits).
    fn li(&mut self, rd: u32, v: i64) {
        let v = if self.rv32 { i64::from(v as i32) } else { v };
        if fits12(v) {
            self.addi(rd, ZERO, v as i32);
            return;
        }
        if i64::from(v as i32) == v {
            let (hi, lo) = enc::hi_lo(v);
            self.lui(rd, hi);
            if lo != 0 {
                // `addiw` on rv64: a 32-bit result, sign-extended.
                if self.rv32 {
                    self.addi(rd, rd, lo);
                } else {
                    self.ins(enc::addiw(X(rd), X(rd), lo));
                }
            }
            return;
        }
        // rv64: the upper bits, shifted, plus the low 12.
        let lo = (v << 52) >> 52;
        let rest = v.wrapping_sub(lo);
        let sh = rest.trailing_zeros();
        self.li(rd, rest >> sh);
        self.slli(rd, rd, sh);
        if lo != 0 {
            self.addi(rd, rd, lo as i32);
        }
    }

    /// `rd = rs + imm`, `tmp` (not `rs`) holding a far `imm`.
    fn add_imm(&mut self, rd: u32, rs: u32, imm: i64, tmp: u32) {
        if fits12(imm) {
            if imm != 0 || rd != rs {
                self.addi(rd, rs, imm as i32);
            }
            return;
        }
        self.li(tmp, imm);
        self.alu(ADD, rd, rs, tmp);
    }
}

/// A register's DWARF number (`x0`.. 0, `f0`.. 32).
fn debug_reg(r: crate::regalloc::R, _size: u32) -> Option<(u16, u16)> {
    (r.reg < 32).then_some((r.reg as u16 + if r.fp { 32 } else { 0 }, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn em(obj: &mut Obj, rv32: bool) -> Em<'_> {
        Em {
            obj,
            rv32,
            start: 0,
            a: enc::Asm::new(false),
            relocs: Vec::new(),
            fixes: Vec::new(),
            labels: Vec::new(),
            slot_off: Vec::new(),
            frame: 0,
            va: 0,
            lines: Vec::new(),
            ra: Alloc::default(),
            opt: false,
            saved: Vec::new(),
            saves: Saves::default(),
            plan: None,
            sites: Vec::new(),
            waits: crate::lir::Waits::default(),
            flags: None,
            skipped: crate::lir::Waits::default(),
            off: None,
        }
    }

    /// The value `li` leaves in its register: `lui`, `addi(w)`, `slli`.
    fn run(code: &[u32], rv32: bool) -> i64 {
        const OP_IMM: u32 = 0x13;
        const OP_IMM_32: u32 = 0x1b;
        const LUI: u32 = 0x37;
        let mut x = 0i64;
        for &w in code {
            let imm = i64::from((w as i32) >> 20);
            let op = w & 0x7f;
            let f3 = (w >> 12) & 7;
            match (op, f3) {
                (LUI, _) => x = i64::from((w & 0xffff_f000) as i32),
                (OP_IMM, 0) => {
                    let rs1 = (w >> 15) & 31;
                    let src = if rs1 == ZERO { 0 } else { x };
                    x = src.wrapping_add(imm);
                }
                (OP_IMM, 1) => x <<= imm & 63,
                (OP_IMM_32, 0) => x = i64::from((x.wrapping_add(imm)) as i32),
                _ => panic!("{w:#x}"),
            }
            if rv32 {
                x = i64::from(x as i32);
            }
        }
        x
    }

    #[test]
    fn li_builds_every_value() {
        let mut obj = Obj::new(false, true, false);
        let mut vs = vec![
            0,
            1,
            -1,
            2047,
            -2048,
            2048,
            -2049,
            0x7fff_f800,
            0x7fff_ffff,
            -0x8000_0000,
            0x8000_0000,
            0xffff_ffff,
            0x1234_5678_9abc_def0,
            i64::MAX,
            i64::MIN,
            0x5555_5555_5555_5555,
            0x0101_0101_0101_0101,
            0x43e0_0000_0000_0000,
        ];
        let mut x = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..2000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            vs.push(x as i64);
            vs.push((x as i64) >> (x % 60));
        }
        for v in vs {
            for rv32 in [false, true] {
                let mut e = em(&mut obj, rv32);
                e.li(T0, v);
                assert!(e.a.pos() <= 8, "{v:#x}");
                let want = if rv32 { i64::from(v as i32) } else { v };
                assert_eq!(run(&e.a.code, rv32), want, "{v:#x} rv32 {rv32}");
            }
        }
    }

    /// The prologue the unwind tables describe, and known encodings.
    #[test]
    fn encodings() {
        let mut obj = Obj::new(false, true, false);
        let mut e = em(&mut obj, false);
        e.prologue();
        assert_eq!(e.a.code, PROLOGUE64);
        let mut e = em(&mut obj, false);
        e.call_sym(0);
        e.epilogue();
        assert_eq!(
            e.a.code,
            [
                0x0000_0097, // auipc ra, 0
                0x0000_80e7, // jalr ra
                0xff04_0113, // addi sp, s0, -16
                0x0081_3083, // ld ra, 8(sp)
                0x0001_3403, // ld s0, 0(sp)
                0x0101_0113, // addi sp, sp, 16
                0x0000_8067, // ret
            ]
        );
        let mut e = em(&mut obj, true);
        e.prologue();
        // addi sp, sp, -16; sw ra, 12(sp); sw s0, 8(sp); addi s0, sp, 16
        assert_eq!(
            e.a.code,
            [0xff01_0113, 0x0011_2623, 0x0081_2423, 0x0101_0413]
        );
        // Callee-saved registers below the saved `s0`, restored first.
        let mut e = em(&mut obj, false);
        e.saved = vec![(9, false), (8, true)];
        e.prologue();
        e.epilogue();
        assert_eq!(e.saves.at, 24);
        assert_eq!(e.saves.regs, [(9, -24), (40, -32)]);
        assert_eq!(
            e.a.code[4..8],
            [
                0xfe94_3423, // sd s1, -24(s0)
                0xfe84_3027, // fsd fs0, -32(s0)
                0xfe84_3483, // ld s1, -24(s0)
                0xfe04_3407, // fld fs0, -32(s0)
            ]
        );
        let mut e = em(&mut obj, false);
        e.alu(MULHU, A0, T0, T2);
        e.amo(Amo::Lr, true, T0, T4, ZERO, Order::SeqCst); // lr.d.aqrl t0, (t4)
        e.ins(enc::fcvt_to_int(IntTy::L, true, X(T0), enc::F(F0), Rm::Rtz));
        e.branch(BNE, T0, ZERO, 8);
        assert_eq!(
            e.a.code,
            [0x0272_b533, 0x160e_b2af, 0xc220_12d3, 0x0002_9463]
        );
    }
}
