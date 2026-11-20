//! LIR -> 32-bit ARM machine code (ARMv7-A, A32 instructions, `VFPv3` and
//! NEON), AAPCS-VFP in ELF objects.
//!
//! Frame: `r4`-`r11` and `lr` pushed, `r11` = `sp` there, then the slots
//! and the outgoing arguments at `sp`, whose first 16 bytes are scratch
//! outside a call. Nothing moves `sp` in the body, which stays 8-byte
//! aligned. Each instruction loads its operands into fixed registers,
//! computes and stores: `r4 r5` hold the first value (an 8-byte one as a
//! pair, low word first), `r6 r7` the second, `r8` addresses a store,
//! `ip` and `lr` move floats (every float load and store goes through
//! them: `vldr` / `vstr` need aligned addresses), `d0`-`d2` (`s0 s2 s4`)
//! compute floats. Code is position independent: addresses come from
//! `movw` / `movt` of the distance, GOT slots from a literal word skipped
//! over. Division, 8-byte integer <-> float conversions and signed 8-byte
//! multiplication with an overflow check call the run-time ABI helpers.
//!
//! Above `-O0` the first value is `r0 r1` and the second `r2 r3`, and the
//! allocator gets `r4`-`r7`, `r9 r10` (pushed by every prologue anyway: no
//! extra saves, the unwind tables stay as they are; the block copy then
//! uses `r2 r3` and `ip lr`, the unsigned 8-byte overflow multiply `ip
//! lr`; an atomic keeps the `-O0` registers, a vector loop its bases from
//! `r3` up, and no value lives there across them; an often used 8-byte
//! integer may take two) and `d16`-`d31`, caller-saved and touched by
//! nothing else (`d8`-`d15` are not saved by the prologue, so they stay
//! unused).
//! An `f32` in a `d` register is its low half: `d16`-`d31` have no `s`
//! names, so it moves through `d0`-`d2` to compute. Operands are read in
//! place and results computed straight into their registers where the
//! instruction allows.

mod arith;
mod call;
mod enc;
mod frame;
mod mem;
mod select;
mod vector;

use crate::lir::{
    Bin, Callee, Cc, Conv, Func, Inst, K, Label, Loc, Mem, NO_SLOT, Order, Part, Rmw, Un, V, scalar,
};
use crate::obj::{FnRange, Obj, RelKind, Reloc, Sect};
use crate::regalloc::{Alloc, Regs};
use enc::{Acc, Cond, D, Imm, Op, R, S, Shift, Vop, modimm};

const R0: u32 = 0;
const R1: u32 = 1;
const R2: u32 = 2;
const R3: u32 = 3;
const R4: u32 = 4;
const R5: u32 = 5;
const R6: u32 = 6;
const R7: u32 = 7;
const R8: u32 = 8;
const R9: u32 = 9;
const R10: u32 = 10;
const FP: u32 = 11;
const IP: u32 = 12;
const SP: u32 = 13;
const LR: u32 = 14;
const PC: u32 = 15;
/// Stack probe interval.
const PAGE: u32 = 4096;
/// Scratch bytes at `sp`.
const SCRATCH: u32 = 16;
/// Bytes pushed: `r4`-`r11`, `lr`.
const SAVED: u32 = 36;

/// Every prologue: `push {r4-r11, lr}; mov r11, sp`, as the unwind
/// tables say.
const PROLOGUE: [enc::Ins; 2] = [enc::push(0x4ff0), enc::mov(R(FP), enc::SP)];
/// The `.ARM.exidx` entry of every function (compact model 0): `vsp =
/// r11; pop {r4-r11, r14}`.
pub const EXIDX: u32 = 0x809b_84ff;

/// What the allocator may use: `r4`-`r7`, `r9 r10` (the prologue saves
/// them) and `d16`-`d31` (caller-saved, no `s` names); `r0`-`r3`, `r8`,
/// `ip`, `lr` and `d0`-`d7` are scratch or carry arguments.
const REGS: Regs = Regs {
    int_saved: &[R4, R5, R6, R7, R9, R10],
    int_free: &[],
    int_arg: &[],
    fp_saved: &[],
    fp_free: &[
        16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
    ],
    fp_arg: &[],
    int_args: &[R0, R1, R2, R3],
    fp_args: &[0, 1, 2, 3, 4, 5, 6, 7],
    int_rets: &[R0, R1, R2, R3],
    fp_rets: &[0, 1, 2, 3],
    sret: R0,
    word: 4,
    ptr: 4,
    fp_size: 8,
    vec: 0,
    vec_saved: false,
    costly_imm: None,
    pairs: true,
    saved_pairs: false,
    saves_all: true,
    clobber: Some(clobber),
};

/// What `i`'s sequence takes of the allocator's registers: an atomic
/// keeps the `-O0` registers `r4`-`r7`, a vector loop its bases from `r3`
/// up.
fn clobber(i: &Inst) -> u64 {
    match i {
        Inst::AtomicLoad { .. }
        | Inst::AtomicStore { .. }
        | Inst::AtomicRmw { .. }
        | Inst::Cas { .. } => 0xf0,
        Inst::VLoop(v) => ((1u64 << v.bases.len()) - 1) << 3,
        _ => 0,
    }
}

/// `i` as a `k` immediate extended by `signed` to 32 bits.
const fn ext32(i: i64, k: K, signed: bool) -> u32 {
    let bits = k.bits();
    if bits >= 32 {
        i as u32
    } else if signed {
        ((i << (64 - bits)) >> (64 - bits)) as u32
    } else {
        (i as u32) & ((1u32 << bits) - 1)
    }
}

const fn wide(k: K) -> bool {
    k.size() == 8
}

/// The loads and stores of `K`s.
impl Acc {
    const fn load(k: K, signed: bool) -> Self {
        match (k, signed) {
            (K::I8, true) => Self::Ldrsb,
            (K::I8, false) => Self::Ldrb,
            (K::I16, true) => Self::Ldrsh,
            (K::I16, false) => Self::Ldrh,
            _ => Self::Ldr,
        }
    }

    const fn store(k: K) -> Self {
        match k {
            K::I8 => Self::Strb,
            K::I16 => Self::Strh,
            _ => Self::Str,
        }
    }
}

struct Em<'a> {
    obj: &'a mut Obj,
    start: u32,
    a: enc::Asm,
    relocs: Vec<(u32, RelKind, u32)>,
    /// `(index of a branch, label)`.
    fixes: Vec<(usize, Label)>,
    labels: Vec<Option<usize>>,
    slot_off: Vec<u32>,
    /// Bytes below the saved registers.
    frame: u32,
    /// `Func::va_save`.
    va: u32,
    lines: Vec<(u32, u32, u32, u32)>,
    /// Literal words: their offsets in the function.
    pools: Vec<u32>,
    ra: Alloc,
    opt: bool,
    /// The first value (an 8-byte one in `x xh`) and the second: `r4`-`r7`
    /// at `-O0`, `r0`-`r3` above, where `r4`-`r7` hold values.
    x: u32,
    xh: u32,
    y: u32,
    yh: u32,
    /// Above `-O0`: parts written once and read at most once
    /// (`lir::waitable`).
    waits: crate::lir::Waits,
    /// The flags of a comparison the next branch takes instead of its
    /// result, which then never goes out (`skipped`).
    flags: Option<Cond>,
    skipped: crate::lir::Waits,
}

/// Emit `f` into `obj`'s text.
pub fn emit(obj: &mut Obj, f: &Func) {
    let (owned, ra) = if f.opt {
        let (g, a) = crate::regalloc::prepare(f, &REGS);
        (Some(g), a)
    } else {
        (None, Alloc::default())
    };
    let f = owned.as_ref().unwrap_or(f);
    let mut out = SCRATCH;
    for i in &f.insts {
        if let Inst::Call { args, .. } = i {
            for (p, _) in args {
                if let Loc::Stack(o) = p.loc {
                    out = out.max(o + p.k.size().max(4));
                }
            }
        }
    }
    let crate::frame::Layout {
        off: slot_off,
        end: at,
        live,
    } = crate::frame::layout(f, &|s| ra.gone(s), (out.next_multiple_of(8), 8, 0), 4);
    let runs = crate::roots::frame(obj, f, &slot_off, 4, &|s| ra.gone(s));
    // With the 36 bytes pushed: an 8-byte aligned `sp`.
    let frame = at.next_multiple_of(8) + 4;
    let start = obj.sect(Sect::Text).align_to(4);
    let text = obj.asm.is_some();
    let mut e = Em {
        obj: &mut *obj,
        start,
        a: enc::Asm::new(text),
        relocs: Vec::new(),
        fixes: Vec::new(),
        labels: vec![None; f.labels as usize],
        slot_off,
        frame,
        va: f.va_save,
        lines: Vec::new(),
        pools: Vec::new(),
        ra,
        opt: f.opt,
        x: if f.opt { R0 } else { R4 },
        xh: if f.opt { R1 } else { R5 },
        y: if f.opt { R2 } else { R6 },
        yh: if f.opt { R3 } else { R7 },
        waits: if f.opt {
            crate::lir::waitable(f).0
        } else {
            crate::lir::Waits::default()
        },
        flags: None,
        skipped: crate::lir::Waits::default(),
    };
    e.prologue();
    let mut track = crate::debug::locs::Track::new(f, live);
    for (n, i) in f.insts.iter().enumerate() {
        if let Some(t) = &mut track {
            t.at(n, 4 * e.pos() as u32, &mut e.lines);
        }
        if let Some(rep) = runs.as_ref().and_then(|r| r.at(n)) {
            for i in rep {
                e.inst(i, None);
            }
            continue;
        }
        e.inst(i, f.insts.get(n + 1));
    }
    // `sp` is `r11 - frame`.
    let off = |s: u32| e.slot_off[s as usize] as i32 - frame as i32;
    let locs = track
        .map(|t| {
            t.finish(
                &e.ra,
                (&e.skipped, &[]),
                (4 * e.pos() as u32, 4),
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
    e.finish(f.sym, debug);
    if let Some(r) = obj.fns.last_mut() {
        r.locs = locs;
    }
}

impl Em<'_> {
    fn ins(&mut self, i: enc::Ins) {
        self.a.push(i);
    }

    /// Index of the next instruction.
    fn pos(&self) -> usize {
        self.a.pos()
    }

    /// Byte offset of the next instruction in the section.
    fn here_off(&self) -> u32 {
        self.start + 4 * self.pos() as u32
    }

    /// A relocation of the next instruction (or word).
    fn reloc(&mut self, kind: RelKind, sym: u32) {
        let off = self.here_off();
        self.relocs.push((off, kind, sym));
    }

    fn finish(mut self, sym: u32, debug: Option<(crate::lir::FnDebug, Vec<i32>)>) {
        for (at, l) in std::mem::take(&mut self.fixes) {
            let target = self.labels[l as usize].expect("bound label");
            self.a.patch24(at, target as i64 - (at as i64 + 2));
        }
        let len = 4 * self.pos() as u32;
        let start = self.start;
        for &p in &self.pools {
            self.obj.data_in_code.push((start + p, start + p + 4));
        }
        let bytes: Vec<u8> = self.a.code.iter().flat_map(|w| w.to_le_bytes()).collect();
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
            saves: crate::obj::Saves::default(),
        });
    }

    /// The listing of this function into `obj.asm`.
    fn list(&mut self, sym: u32, bytes: &[u8]) {
        let name = crate::asm::sym_name(self.obj, &self.obj.syms[sym as usize].name);
        let labels = self
            .labels
            .iter()
            .enumerate()
            .filter_map(|(l, at)| at.map(|at| (4 * at as u32, l as u32)))
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
        let lines = self.a.lines(&l);
        crate::asm::function(self.obj, &l, &lines, crate::asm::Bytes::Words);
    }

    /// `op rd, rn, rm` under `cond`, the flags set by `s`.
    fn dp(&mut self, cond: Cond, op: Op, s: bool, rd: u32, rn: u32, rm: u32) {
        self.ins(enc::dp(cond, op, s, R(rd), R(rn), R(rm)));
    }

    /// `op rd, rn, #imm`.
    fn dpi(&mut self, cond: Cond, op: Op, s: bool, rd: u32, rn: u32, imm: Imm) {
        self.ins(enc::dp_imm(cond, op, s, R(rd), R(rn), imm));
    }

    /// `op rd, rn, rm, <ty> rs` (a register-specified shift of `rm`).
    #[allow(clippy::too_many_arguments)]
    fn dps(&mut self, op: Op, s: bool, rd: u32, rn: u32, rm: u32, ty: Shift, rs: u32) {
        let i = enc::dp_rsr(Cond::AL, op, s, R(rd), R(rn), R(rm), ty, R(rs));
        self.ins(i);
    }

    /// `op rd, rn, rm, <ty> #n`.
    #[allow(clippy::too_many_arguments)]
    fn dpsi(&mut self, cond: Cond, op: Op, s: bool, rd: u32, rn: u32, rm: u32, ty: Shift, n: u32) {
        let i = enc::dp_shift(cond, op, s, R(rd), R(rn), R(rm), ty, n);
        self.ins(i);
    }

    fn mov(&mut self, rd: u32, rm: u32) {
        if rd != rm {
            self.dp(Cond::AL, Op::Mov, false, rd, 0, rm);
        }
    }

    /// `(lo, hi)` = `(l, h)`, whichever registers they share (`ip`
    /// scratch).
    fn mov2(&mut self, lo: u32, hi: u32, l: u32, h: u32) {
        if lo == h && hi == l {
            self.mov(IP, l);
            self.mov(lo, h);
            self.mov(hi, IP);
        } else if lo == h {
            self.mov(hi, h);
            self.mov(lo, l);
        } else {
            self.mov(lo, l);
            self.mov(hi, h);
        }
    }

    /// `rd` = `v`.
    fn mov_imm(&mut self, rd: u32, v: u32) {
        self.mov_imm_cc(Cond::AL, rd, v);
    }

    /// `rd` = `v` under `cond` (one instruction where it can be).
    fn mov_imm_cc(&mut self, cond: Cond, rd: u32, v: u32) {
        if let Some(e) = modimm(v) {
            self.dpi(cond, Op::Mov, false, rd, 0, e);
        } else if let Some(e) = modimm(!v) {
            self.dpi(cond, Op::Mvn, false, rd, 0, e);
        } else {
            self.ins(enc::movw(cond, R(rd), v & 0xffff));
            let hi = v >> 16;
            if hi != 0 {
                self.ins(enc::movt(cond, R(rd), hi));
            }
        }
    }

    /// `rd = rn + imm`, in modified-immediate chunks (no scratch).
    fn add_imm(&mut self, rd: u32, rn: u32, imm: i32) {
        if imm == 0 {
            self.mov(rd, rn);
            return;
        }
        let (op, mut v) = if imm < 0 {
            (Op::Sub, imm.unsigned_abs())
        } else {
            (Op::Add, imm as u32)
        };
        let mut src = rn;
        while v != 0 {
            let p = v.trailing_zeros() & !1;
            let chunk = v & (0xff << p);
            self.dpi(
                Cond::AL,
                op,
                false,
                rd,
                src,
                modimm(chunk).unwrap_or(Imm::small(0)),
            );
            src = rd;
            v -= chunk;
        }
    }
}

/// A register's DWARF number (`r0`..; floats not described).
fn debug_reg(r: crate::regalloc::R, size: u32) -> Option<(u16, u16)> {
    let n = r.reg as u16;
    // `CodeView`: `CV_ARM_R0`.. (`_SP` 23, `_LR` 24, `_PC` 25); a double
    // in `dN` DWARF 256 + N, `CV_ARM_ND0`.. (a single there has no name).
    if r.fp {
        return (size == 8 && n < 32).then_some((256 + n, 151 + n));
    }
    (n < 16).then_some((n, 10 + n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn em(obj: &mut Obj) -> Em<'_> {
        Em {
            obj,
            start: 0,
            a: enc::Asm::new(false),
            relocs: Vec::new(),
            fixes: Vec::new(),
            labels: Vec::new(),
            slot_off: Vec::new(),
            frame: 0,
            va: 0,
            lines: Vec::new(),
            pools: Vec::new(),
            ra: Alloc::default(),
            opt: false,
            x: R4,
            xh: R5,
            y: R6,
            yh: R7,
            waits: crate::lir::Waits::default(),
            flags: None,
            skipped: crate::lir::Waits::default(),
        }
    }

    /// The value of a modified immediate's encoding.
    const fn unrot(e: u32) -> u32 {
        (e & 0xff).rotate_right(2 * (e >> 8))
    }

    #[test]
    fn modified_immediates() {
        for v in [
            0,
            0xff,
            0x3fc,
            0xff00_0000,
            0xf000_000f,
            0x1000,
            0x8000_0000,
        ] {
            assert_eq!(modimm(v).map(|e| unrot(e.0)), Some(v), "{v:#x}");
        }
        for v in [0x101, 0x1234, 0xffff, 0x8000_0101] {
            assert_eq!(modimm(v), None, "{v:#x}");
        }
    }

    /// `add_imm` reaches any offset in at most four `add` / `sub` of
    /// modified immediates, no scratch register.
    #[test]
    fn add_imm_reaches_every_offset() {
        let mut obj = Obj::new(false, true, false);
        for imm in [
            1,
            255,
            256,
            4095,
            4096,
            0x1_2345,
            -0x1_2345,
            0x7fff_ffff,
            -0x7fff_ffff,
        ] {
            let mut e = em(&mut obj);
            e.add_imm(R8, SP, imm);
            assert!(e.pos() <= 4, "{imm:#x}");
            let mut v = 0i64;
            for (k, w) in e.a.code.iter().enumerate() {
                let (op, rn, rd) = ((w >> 21) & 15, (w >> 16) & 15, (w >> 12) & 15);
                assert_eq!((rd, rn), (R8, if k == 0 { SP } else { R8 }));
                let x = i64::from(unrot(w & 0xfff));
                v += if op == Op::Add as u32 { x } else { -x };
            }
            assert_eq!(v, i64::from(imm));
        }
    }

    #[test]
    fn mov_imm_uses_movw_movt_past_modified_immediates() {
        let mut obj = Obj::new(false, true, false);
        let mut e = em(&mut obj);
        e.mov_imm(R4, 0x1234_5678);
        assert_eq!(e.a.code, [0xe305_4678, 0xe341_4234]);
        let mut e = em(&mut obj);
        e.mov_imm(R4, 0xffff_ff00);
        assert_eq!(e.a.code, [0xe3e0_40ff]); // mvn r4, #0xff
    }
}
