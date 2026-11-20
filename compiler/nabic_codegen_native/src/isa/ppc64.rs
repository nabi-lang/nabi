//! LIR -> powerpc64 machine code: `ELFv2` (little endian), POWER8, in ELF
//! objects.
//!
//! Every function has the `ELFv2` global entry (`r2` = the TOC from the
//! entry address in `r12`), its local entry 8 bytes in. Frame: `r31`
//! saved just below the incoming `r1`, the return address in the caller's
//! LR save word, `r31` = the incoming `r1` (the CFA); below it the slots,
//! 16 scratch bytes, the parameter save area (at least the 64 bytes of
//! the register arguments) and the 32-byte header at the new `r1`, whose
//! TOC word holds `r2` across indirect calls (PLT stubs store it there
//! too). Nothing moves `r1` in the body. Each instruction loads its
//! operands into fixed volatile registers, computes and stores: `r11`
//! holds the first value, `r12` the second, `r9` / `r10` pointers, `r0`
//! the index of a far offset; `r3`-`r10` are scratch inside one
//! instruction, `f12` and `f13` compute floats. Symbols are TOC relative;
//! other images' through the GOT.
//!
//! Above `-O0` the shared allocator keeps values in registers the
//! sequences above never touch: the callee-saved `r14`-`r30` and
//! `f14`-`f31` (stored below the saved `r31` after the frame allocation,
//! reloaded by every epilogue), the volatile `f0`, and the argument
//! registers `r3`-`r7` / `f1`-`f11`, which only the call, return and
//! parameter moves write. Operations then read their operands from, and
//! compute into, those registers; an integer register holds its part
//! zero-extended.

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
use crate::obj::{FnRange, Obj, RelKind, Reloc, Saves, Sect};
use crate::regalloc::{Alloc, Regs};
use enc::{Alu, Asm, Cond, EQ, F, Fp, GT, LT, LogImm, Logic, Ls, R, UN};

const R0: u32 = 0;
const SP: u32 = 1;
const TOC: u32 = 2;
const R3: u32 = 3;
const R8: u32 = 8;
const R9: u32 = 9;
const R10: u32 = 10;
const R11: u32 = 11;
const R12: u32 = 12;
const TP: u32 = 13;
const FP: u32 = 31;
/// `f1`, the first float argument / result; `f12` / `f13` compute.
const F1: u32 = 1;
const FA: u32 = 12;
const FB: u32 = 13;
/// What the allocator may use: `r0`, `r8`-`r12`, `f12` / `f13` are
/// scratch; `r3`-`r10` / `f1`-`f13` carry arguments and results.
const REGS: Regs = Regs {
    int_saved: &[
        14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30,
    ],
    int_free: &[],
    int_arg: &[3, 4, 5, 6, 7],
    fp_saved: &[
        14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
    ],
    fp_free: &[0],
    fp_arg: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
    int_args: &[3, 4, 5, 6, 7, 8, 9, 10],
    fp_args: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13],
    int_rets: &[3, 4, 5, 6, 7, 8, 9, 10],
    fp_rets: &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13],
    sret: 3,
    word: 8,
    ptr: 8,
    fp_size: 8,
    vec: 0,
    vec_saved: false,
    costly_imm: None,
    pairs: false,
    saved_pairs: false,
    saves_all: false,
    clobber: None,
};
/// Stack probe interval.
const PAGE: u32 = 4096;
/// The frame header: back chain, CR, LR and TOC save words.
const HEADER: u32 = 32;
/// The register arguments' part of a parameter save area.
const PSA: u32 = 64;
/// The TOC save word.
const TOC_SAVE: i64 = 24;

/// Every prologue up to the frame allocation, as the unwind tables say:
/// the global entry, `mflr r0; std r31, -8(r1); std r0, 16(r1); mr r31,
/// r1`.
#[cfg(test)]
const PROLOGUE: [u32; 6] = [
    0x3c4c_0000,
    0x3842_0000,
    0x7c08_02a6,
    0xfbe1_fff8,
    0xf801_0010,
    0x7c3f_0b78,
];

const fn fits16(v: i64) -> bool {
    v >= -0x8000 && v < 0x8000
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

/// The float kind of `k`'s size.
const fn float_k(k: K) -> K {
    if k.size() == 4 { K::F32 } else { K::F64 }
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
    start: u32,
    a: Asm,
    relocs: Vec<(u32, RelKind, u32)>,
    /// `(index of a b, label)`.
    /// `(index of a b, or a bc if, label)`.
    fixes: Vec<(usize, Label, bool)>,
    labels: Vec<Option<usize>>,
    slot_off: Vec<u32>,
    /// The 16 scratch bytes, from `r1`.
    scratch: i64,
    /// Bytes from the new `r1` to the incoming one.
    frame: u32,
    lines: Vec<(u32, u32, u32, u32)>,
    ra: Alloc,
    opt: bool,
    /// Callee-saved registers stored below the saved `r31`, the first
    /// highest: `(register, float)`.
    saved: Vec<(u32, bool)>,
    saves: Saves,
    /// Conditional jumps as one `bc` (`br`).
    short: bool,
    /// Above `-O0`: parts written once and read at most once
    /// (`lir::waitable`).
    waits: crate::lir::Waits,
    /// The condition of a comparison the next branch takes instead of its
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
    let mut out = PSA;
    for i in &f.insts {
        if let Inst::Call { args, .. } = i {
            // With stack arguments the save area holds every parameter,
            // FPR ones after the last stack word too: a gcc callee stores
            // its FPR arguments there. Each FPR part takes a doubleword
            // after the part before it (a packed `f32` only 4: over).
            let (mut end, mut stack) = (0, false);
            for (p, _) in args {
                end = match p.loc {
                    Loc::Int(n) => end.max(8 * u32::from(n) + 8),
                    Loc::Stack(o) => {
                        stack = true;
                        end.max(o + 8)
                    }
                    _ => end + 8,
                };
            }
            if stack {
                out = out.max(end);
            }
        }
    }
    let scratch = HEADER + out.next_multiple_of(16);
    let crate::frame::Layout {
        off: slot_off,
        end: at,
        live,
    } = crate::frame::layout(f, &|s| ra.gone(s), (scratch + 16, 16, 0), 8);
    let runs = crate::roots::frame(obj, f, &slot_off, 8, &|s| ra.gone(s));
    let saved: Vec<(u32, bool)> = ra
        .saved
        .iter()
        .map(|&r| (r, false))
        .chain(ra.saved_fp.iter().map(|&r| (r, true)))
        .collect();
    // The saved `r31` at the top, the callee-saved registers below it.
    let frame = (at + 8 + 8 * saved.len() as u32).next_multiple_of(16);
    let start = obj.sect(Sect::Text).align_to(16);
    let obj_text = obj.asm.is_some();
    // Above `-O0` conditional jumps are one `bc` (16-bit); a target out
    // of its reach means the long forms throughout.
    let waits = if f.opt {
        crate::lir::waitable(f).0
    } else {
        crate::lir::Waits::default()
    };
    for short in [f.opt, false] {
        let mut e = Em {
            obj: &mut *obj,
            start,
            a: Asm::new(obj_text),
            relocs: Vec::new(),
            fixes: Vec::new(),
            labels: vec![None; f.labels as usize],
            slot_off: slot_off.clone(),
            scratch: i64::from(scratch),
            frame,
            lines: Vec::new(),
            ra: ra.clone(),
            opt: f.opt,
            saved: saved.clone(),
            saves: Saves::default(),
            short,
            waits: waits.clone(),
            flags: None,
            skipped: crate::lir::Waits::default(),
        };
        e.prologue();
        let mut track = crate::debug::locs::Track::new(f, live.clone());
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
        // `r31` is `r1 + frame`.
        let off = |s: u32| e.slot_off[s as usize] as i32 - frame as i32;
        let locs = track
            .map(|t| {
                t.finish(
                    &e.ra,
                    (&e.skipped, &[]),
                    (4 * e.pos() as u32, 8),
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
    }
}

impl Em<'_> {
    fn ins(&mut self, i: enc::Ins) {
        self.a.push(i);
    }

    fn pos(&self) -> usize {
        self.a.pos()
    }

    fn here_off(&self) -> u32 {
        self.start + 4 * self.pos() as u32
    }

    /// A relocation of the next instruction.
    fn reloc(&mut self, kind: RelKind, sym: u32) {
        let off = self.here_off();
        self.relocs.push((off, kind, sym));
    }

    /// Into `obj`, unless a short `bc`'s target is out of its reach.
    fn finish(mut self, sym: u32, debug: Option<(crate::lir::FnDebug, Vec<i32>)>) -> bool {
        for (at, l, bc) in std::mem::take(&mut self.fixes) {
            let target = self.labels[l as usize].expect("bound label");
            let d = (target as i64 - at as i64) * 4;
            if bc {
                if !(-0x8000..0x8000).contains(&d) {
                    return false;
                }
                self.a.patch_bc(at, d as u32);
                continue;
            }
            assert!(
                enc::reaches_b(d),
                "powerpc64: a function's jump past 32 MiB"
            );
            self.a.patch_b(at, d);
        }
        let len = 4 * self.pos() as u32;
        let start = self.start;
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
        crate::asm::function(self.obj, &l, &lines, crate::asm::Bytes::EachWord);
    }

    // Encodings.

    /// `rt = ra <op> rb` (`add`, `mulld`, ..).
    fn alu(&mut self, op: Alu, rt: u32, ra: u32, rb: u32) {
        self.ins(enc::alu(op, R(rt), R(ra), R(rb)));
    }

    /// `ra = rs <op> rb` of the logical / shift group.
    fn logic(&mut self, op: Logic, ra: u32, rs: u32, rb: u32) {
        self.ins(enc::logic(op, R(ra), R(rs), R(rb)));
    }

    /// `rt = rb - ra`.
    fn subf(&mut self, rt: u32, ra: u32, rb: u32) {
        self.alu(Alu::Subf, rt, ra, rb);
    }

    fn addi(&mut self, rt: u32, ra: u32, imm: i64) {
        self.ins(enc::addi(R(rt), R(ra), imm));
    }

    fn mr(&mut self, ra: u32, rs: u32) {
        if ra != rs {
            self.ins(enc::mr(R(ra), R(rs)));
        }
    }

    /// `ori` / `oris` / `xori` / `andi.` `ra, rs, ui`.
    fn logic_imm(&mut self, op: LogImm, ra: u32, rs: u32, ui: i64) {
        self.ins(enc::log_imm(op, R(ra), R(rs), ui));
    }

    fn sldi(&mut self, ra: u32, rs: u32, n: u32) {
        self.ins(enc::rldicr(R(ra), R(rs), n, 63 - n));
    }

    fn srdi(&mut self, ra: u32, rs: u32, n: u32) {
        if n == 0 {
            self.mr(ra, rs);
        } else {
            self.ins(enc::rldicl(R(ra), R(rs), 64 - n, n));
        }
    }

    fn sradi(&mut self, ra: u32, rs: u32, n: u32) {
        self.ins(enc::sradi(R(ra), R(rs), n));
    }

    /// `ra` = the low `bits` of `rs`, zero-extended.
    fn clrldi(&mut self, ra: u32, rs: u32, bits: u32) {
        self.ins(enc::rldicl(R(ra), R(rs), 0, 64 - bits));
    }

    /// `rd` = `v`.
    fn li(&mut self, rd: u32, v: i64) {
        if fits16(v) {
            self.addi(rd, 0, v);
            return;
        }
        if i64::from(v as i32) == v {
            self.ins(enc::addis(R(rd), R(0), v >> 16));
            if v & 0xffff != 0 {
                self.logic_imm(LogImm::Ori, rd, rd, v & 0xffff);
            }
            return;
        }
        if v >> 32 == 0 {
            // 2^31..2^32: the sign-extended word, the high half cleared.
            self.li(rd, i64::from(v as i32));
            self.clrldi(rd, rd, 32);
            return;
        }
        self.li(rd, v >> 32);
        self.sldi(rd, rd, 32);
        if (v >> 16) & 0xffff != 0 {
            self.logic_imm(LogImm::Oris, rd, rd, (v >> 16) & 0xffff);
        }
        if v & 0xffff != 0 {
            self.logic_imm(LogImm::Ori, rd, rd, v & 0xffff);
        }
    }

    /// `rd = rs + imm`, a far `imm` through `r0`.
    fn add_imm(&mut self, rd: u32, rs: u32, imm: i64) {
        if fits16(imm) {
            if imm != 0 || rd != rs {
                self.addi(rd, rs, imm);
            }
            return;
        }
        self.li(R0, imm);
        self.alu(Alu::Add, rd, rs, R0);
    }
}

/// A register's DWARF number (`r0`.. 0, `f0`.. 32).
fn debug_reg(r: crate::regalloc::R, _size: u32) -> Option<(u16, u16)> {
    (r.reg < 32).then_some((r.reg as u16 + if r.fp { 32 } else { 0 }, 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn em(obj: &mut Obj) -> Em<'_> {
        Em {
            obj,
            start: 0,
            a: Asm::new(false),
            relocs: Vec::new(),
            fixes: Vec::new(),
            labels: Vec::new(),
            slot_off: Vec::new(),
            scratch: 0,
            frame: 0,
            lines: Vec::new(),
            ra: Alloc::default(),
            opt: false,
            saved: Vec::new(),
            saves: Saves::default(),
            short: false,
            waits: crate::lir::Waits::default(),
            flags: None,
            skipped: crate::lir::Waits::default(),
        }
    }

    /// The value `li` leaves in its register: `addi`, `addis`, `ori`,
    /// `oris`, `rldicr` (`sldi`), `rldicl` (`clrldi`).
    fn run(code: &[u32]) -> i64 {
        let mut x = 0i64;
        for &w in code {
            let ra = (w >> 16) & 31;
            let imm = i64::from(w as u16 as i16);
            let ui = i64::from(w & 0xffff);
            match w >> 26 {
                14 => x = if ra == 0 { imm } else { x.wrapping_add(imm) },
                15 => {
                    x = if ra == 0 {
                        imm << 16
                    } else {
                        x.wrapping_add(imm << 16)
                    }
                }
                24 => x |= ui,
                25 => x |= ui << 16,
                30 => {
                    let sh = ((w >> 11) & 31) | (((w >> 1) & 1) << 5);
                    let m = ((w >> 6) & 31) | (((w >> 5) & 1) << 5);
                    let r = (x as u64).rotate_left(sh);
                    x = match (w >> 2) & 7 {
                        0 => (r & (u64::MAX >> m)) as i64,
                        1 => (r & (u64::MAX << (63 - m))) as i64,
                        _ => panic!("{w:#x}"),
                    };
                }
                _ => panic!("{w:#x}"),
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
            0x7fff,
            -0x8000,
            0x8000,
            -0x8001,
            0xffff,
            0x1_0000,
            0x7fff_ffff,
            -0x8000_0000,
            0x8000_0000,
            0xffff_ffff,
            0x1_0000_0000,
            0x1234_5678_9abc_def0,
            i64::MAX,
            i64::MIN,
            0x5555_5555_5555_5555,
            0x43e0_0000_0000_0000,
            -0x1_0000_0001,
        ];
        let mut x = 0x9e37_79b9_7f4a_7c15_u64;
        for _ in 0..4000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            vs.push(x as i64);
            vs.push((x as i64) >> (x % 60));
        }
        for v in vs {
            let mut e = em(&mut obj);
            e.li(R11, v);
            assert!(e.a.code.len() <= 5, "{v:#x}");
            assert_eq!(run(&e.a.code), v, "{v:#x}");
        }
    }

    /// The prologue the unwind tables describe, and known encodings
    /// (`llvm-mc --triple=powerpc64le`).
    #[test]
    fn encodings() {
        let mut obj = Obj::new(false, true, false);
        let mut e = em(&mut obj);
        e.prologue();
        assert_eq!(e.a.code, PROLOGUE);
        let mut e = em(&mut obj);
        e.call_sym(0);
        e.epilogue();
        assert_eq!(
            e.a.code,
            [
                0x4800_0001, // bl 0
                enc::nop().word(),
                0xe81f_0010, // ld r0, 16(r31)
                0x7c08_03a6, // mtlr r0
                0x7fe1_fb78, // mr r1, r31
                0xebe1_fff8, // ld r31, -8(r1)
                enc::blr().word(),
            ]
        );
        let mut e = em(&mut obj);
        e.alu(Alu::Mulhdu, R8, R11, R12); // mulhdu r8, r11, r12
        e.subf(R11, R12, R11); // sub r11, r11, r12
        e.logic(Logic::Srad, R11, R11, R12); // srad r11, r11, r12
        e.sradi(R9, R10, 63); // sradi r9, r10, 63
        e.srdi(R11, R11, 3); // srdi r11, r11, 3
        e.larx(K::I64, R11, R9); // ldarx r11, 0, r9
        e.stcx(K::I32, R10, R9); // stwcx. r10, 0, r9
        e.fop(K::F32, Fp::Fadd, FA, FA, FB); // fadds f12, f12, f13
        e.fop(K::F64, Fp::Fctidz, FB, 0, FA); // fctidz f13, f12
        e.cr_bit(R11, GT); // mfcr r11; rlwinm r11, r11, 2, 31, 31
        e.ls(true, K::I32, true, false, R11, SP, 6); // li r0, 6; lwax r11, r1, r0
        e.ins(enc::stdux(R(SP), R(SP), R(R0))); // stdux r1, r1, r0
        assert_eq!(e.a.code, ENCODED);
    }

    const ENCODED: [u32; 14] = [
        0x7d0b_6012,
        0x7d6c_5850,
        0x7d6b_6634,
        0x7d49_fe76,
        0x796b_e8c2,
        0x7d60_48a8,
        0x7d40_492d,
        0xed8c_682a,
        0xfda0_665e,
        0x7d60_0026,
        0x556b_17fe,
        0x3800_0006,
        0x7d61_02aa,
        0x7c21_016a,
    ];
}
