//! LIR -> s390x (z/Architecture) machine code: big endian, z10 and older
//! instructions but for vector loops (z13), in ELF objects.
//!
//! Frame: `stmg r6, r15` into the caller's register save area, `r11` = the
//! incoming `r15` (the CFA - 160), then `r15` lowered by the frame: the
//! register save area its callees use at `r15`, the outgoing stack
//! arguments at `r15 + 160`, the slots above them, the callee-saved float
//! registers used (`std`, above -O0) at the top. Nothing moves `r15` in
//! the body. Each instruction loads its operands into fixed registers,
//! computes and stores: `r7` holds the first value, `r8` the second, `r9` a
//! pointer, `r10` a store's base, `r14` a float access's base, `r1` the
//! index of a far displacement, `r0` an immediate; `r2`-`r6` and the
//! even / odd pair `r12 r13` (multiply, divide, `flogr`) are scratch inside
//! one instruction, `f1 f3` compute floats. Code is position independent
//! (`larl`, `lgrl` of GOT slots).
//!
//! Above -O0 the first and second value are `r4 r5` (an atomic keeps `r7
//! r8`), the pair is `r2 r3` and a byte / halfword compare-and-swap uses
//! `r14 r1 r10` for `r6 r13 r12`, so values live in `r7 r8 r12 r13`
//! (callee-saved, stored by every prologue anyway; `r7 r8` not across an
//! atomic), `r6` (the fifth argument register, between its moves), `f5
//! f7`, the float argument registers and `f8`-`f15`; an operation reads
//! them in place and computes into its destination's register where it
//! can.

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
use crate::regalloc::{Alloc, R, Regs};
use enc::{Asm, Pcrel, Ri, Ril, Rr, Rre, Rrf, Rsy, Rxy, Ss};

const R0: u8 = 0;
const R1: u8 = 1;
const R2: u8 = 2;
const R3: u8 = 3;
const R4: u8 = 4;
const R5: u8 = 5;
const R6: u8 = 6;
const R11: u8 = 11;
const R12: u8 = 12;
const R13: u8 = 13;
const R14: u8 = 14;
const R15: u8 = 15;
/// The first and second value, a pointer, a store's base, a float
/// access's base.
const X: u8 = 7;
const Y: u8 = 8;
const P: u8 = 9;
const SB: u8 = 10;
const FB: u8 = R14;
const F1: u8 = 1;
const F3: u8 = 3;
/// The register save area at `r15`.
const SAVE: u32 = 160;
/// Stack probe interval.
const PAGE: u32 = 4096;

// Condition code masks.
const CC0: u8 = 8;
const CC1: u8 = 4;
const CC2: u8 = 2;
const CC3: u8 = 1;
const ALWAYS: u8 = 15;
const NE: u8 = CC1 | CC2;

/// Rounding mode of a conversion: toward zero.
const RTZ: u8 = 5;

/// What the allocator may use: `r7 r8 r12 r13` (saved by every prologue;
/// above -O0 the first and second value move to `r4 r5`, the even / odd
/// pair to `r2 r3`), `r6` (the fifth argument, scratch nowhere else above
/// -O0), `f5 f7` (no instruction touches them), `f0 f2 f4 f6` (arguments
/// and results), `f8`-`f15` (saved when used).
const REGS: Regs = Regs {
    int_saved: &[7, 8, 12, 13],
    int_free: &[],
    int_arg: &[6],
    fp_saved: &[8, 9, 10, 11, 12, 13, 14, 15],
    fp_free: &[5, 7],
    fp_arg: &[0, 2, 4, 6],
    int_args: &[2, 3, 4, 5, 6],
    fp_args: &[0, 2, 4, 6],
    int_rets: &[2, 3, 4, 5, 6],
    fp_rets: &[0, 2, 4, 6],
    sret: 2,
    word: 8,
    ptr: 8,
    fp_size: 8,
    vec: 0,
    vec_saved: false,
    costly_imm: None,
    pairs: false,
    saved_pairs: false,
    saves_all: true,
    clobber: Some(clobber),
};

/// What `i`'s sequence takes of the allocator's registers: an atomic
/// keeps the first and second value in `r7 r8`.
const fn clobber(i: &Inst) -> u64 {
    match i {
        Inst::AtomicLoad { .. }
        | Inst::AtomicStore { .. }
        | Inst::AtomicRmw { .. }
        | Inst::Cas { .. } => (1 << X) | (1 << Y),
        _ => 0,
    }
}

/// The DWARF number of float register `f`.
const fn dwarf_f(f: u8) -> u16 {
    const N: [u16; 16] = [
        16, 20, 17, 21, 18, 22, 19, 23, 24, 28, 25, 29, 26, 30, 27, 31,
    ];
    N[f as usize]
}

/// The condition codes of `cc` after a compare. Floats: unordered is CC
/// 3, true for `Ne` alone.
const fn cc_mask(cc: Cc) -> u8 {
    match cc {
        Cc::Eq => CC0,
        Cc::Ne => CC1 | CC2 | CC3,
        Cc::Lt => CC1,
        Cc::Le => CC0 | CC1,
        Cc::Gt => CC2,
        Cc::Ge => CC0 | CC2,
    }
}

const fn fits16(v: i64) -> bool {
    v >= -0x8000 && v < 0x8000
}

const fn fits20(v: i64) -> bool {
    v >= -0x8_0000 && v < 0x8_0000
}

const fn fits32(v: i64) -> bool {
    v >= i32::MIN as i64 && v <= i32::MAX as i64
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

/// The load of `k` extended by `signed` to 64 bits.
const fn load_op(k: K, signed: bool) -> Rxy {
    match (int_k(k), signed) {
        (K::I8, true) => Rxy::Lgb,
        (K::I8, false) => Rxy::Llgc,
        (K::I16, true) => Rxy::Lgh,
        (K::I16, false) => Rxy::Llgh,
        (K::I32, true) => Rxy::Lgf,
        (K::I32, false) => Rxy::Llgf,
        _ => Rxy::Lg,
    }
}

/// The store of `k`'s low bytes.
const fn store_op(k: K) -> Rxy {
    match int_k(k) {
        K::I8 => Rxy::Stcy,
        K::I16 => Rxy::Sthy,
        K::I32 => Rxy::Sty,
        _ => Rxy::Stg,
    }
}

struct Em<'a> {
    obj: &'a mut Obj,
    start: u32,
    a: Asm,
    /// `(offset of a brcl, or a brc if `short`, label)`.
    fixes: Vec<(usize, Label)>,
    labels: Vec<Option<usize>>,
    slot_off: Vec<u32>,
    /// Bytes below the incoming `r15`.
    frame: u32,
    lines: Vec<(u32, u32, u32, u32)>,
    ra: Alloc,
    opt: bool,
    /// Callee-saved float registers stored at `r11 - 8 (i + 1)`.
    fsaved: Vec<u8>,
    saves: Saves,
    /// Jumps as `brc` (`jcc`).
    short: bool,
    /// The first and second value: `r7 r8` at -O0, `r4 r5` above, where
    /// `r7 r8` hold values.
    x: u8,
    y: u8,
    /// Above `-O0`: parts written once and read at most once
    /// (`lir::waitable`).
    waits: crate::lir::Waits,
    /// The condition-code mask of a comparison the next branch takes
    /// instead of its result, which then never goes out (`skipped`).
    flags: Option<u8>,
    skipped: crate::lir::Waits,
}

/// Emit `f` into `obj`'s text.
pub fn emit(obj: &mut Obj, f: &Func) {
    let mut out = 0;
    for i in &f.insts {
        if let Inst::Call { args, .. } = i {
            for (p, _) in args {
                if let Loc::Stack(o) = p.loc {
                    out = out.max(o + 8);
                }
            }
        }
    }
    let (owned, ra) = if f.opt {
        let (g, a) = crate::regalloc::prepare(f, &REGS);
        (Some(g), a)
    } else {
        (None, Alloc::default())
    };
    let f = owned.as_ref().unwrap_or(f);
    let crate::frame::Layout {
        off: slot_off,
        end: at,
        live,
    } = crate::frame::layout(f, &|s| ra.gone(s), (SAVE + out, 8, 0), 8);
    let runs = crate::roots::frame(obj, f, &slot_off, 8, &|s| ra.gone(s));
    // The saved float registers at the top, below the incoming `r15`.
    let fsaved: Vec<u8> = ra.saved_fp.iter().map(|&r| r as u8).collect();
    let frame = at.next_multiple_of(8) + 8 * fsaved.len() as u32;
    let start = obj.sect(Sect::Text).align_to(8);
    let text = obj.asm.is_some();
    // Above `-O0` jumps are `brc` (16-bit); a target out of its reach
    // means `brcl` throughout.
    let waits = if f.opt {
        crate::lir::waitable(f).0
    } else {
        crate::lir::Waits::default()
    };
    for short in [f.opt, false] {
        let mut e = Em {
            obj: &mut *obj,
            start,
            a: Asm::new(text),
            fixes: Vec::new(),
            labels: vec![None; f.labels as usize],
            slot_off: slot_off.clone(),
            frame,
            lines: Vec::new(),
            ra: ra.clone(),
            opt: f.opt,
            fsaved: fsaved.clone(),
            saves: Saves::default(),
            short,
            x: if f.opt { R4 } else { X },
            y: if f.opt { R5 } else { Y },
            waits: waits.clone(),
            flags: None,
            skipped: crate::lir::Waits::default(),
        };
        e.prologue();
        let mut track = crate::debug::locs::Track::new(f, live.clone());
        for (n, i) in f.insts.iter().enumerate() {
            if let Some(t) = &mut track {
                t.at(n, e.a.pos() as u32, &mut e.lines);
            }
            if let Some(rep) = runs.as_ref().and_then(|r| r.at(n)) {
                for i in rep {
                    e.inst(i, None);
                }
                continue;
            }
            e.inst(i, f.insts.get(n + 1));
        }
        // `r11` is `r15 + frame`.
        let off = |s: u32| e.slot_off[s as usize] as i32 - frame as i32;
        let locs = track
            .map(|t| {
                t.finish(
                    &e.ra,
                    (&e.skipped, &[]),
                    (e.a.pos() as u32, 8),
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
    /// Into `obj`, unless a `brc`'s target is out of its reach.
    fn finish(mut self, sym: u32, debug: Option<(crate::lir::FnDebug, Vec<i32>)>) -> bool {
        for (at, l) in std::mem::take(&mut self.fixes) {
            let target = self.labels[l as usize].expect("bound label");
            if !self.short {
                self.a.patch_brcl(at, target);
            } else if !self.a.patch_brc(at, target) {
                return false;
            }
        }
        if self.obj.asm.is_some() {
            self.list(sym);
        }
        let len = self.a.pos() as u32;
        let start = self.start;
        let text = self.obj.sect(Sect::Text);
        text.data.extend_from_slice(&self.a.code);
        for &(off, kind, s) in &self.a.relocs {
            text.relocs.push(Reloc {
                off: start + off,
                kind,
                sym: s,
            });
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
    fn list(&mut self, sym: u32) {
        let name = crate::asm::sym_name(self.obj, &self.obj.syms[sym as usize].name);
        let labels = self
            .labels
            .iter()
            .enumerate()
            .filter_map(|(l, at)| at.map(|at| (at as u32, l as u32)))
            .collect();
        let l = crate::asm::Listing {
            name: &name,
            code: &self.a.code,
            labels,
            relocs: &self.a.relocs,
        };
        let lines = self.a.lines(&l);
        crate::asm::function(self.obj, &l, &lines, crate::asm::Bytes::Short);
    }

    fn lgr(&mut self, rd: u8, rs: u8) {
        if rd != rs {
            self.a.rre(Rre::Lgr, rd, rs);
        }
    }

    /// `r` = `v`.
    fn li(&mut self, r: u8, v: i64) {
        if fits16(v) {
            self.a.ri(Ri::Lghi, r, v as u16);
        } else if fits32(v) {
            self.a.ril(Ril::Lgfi, r, v as u32);
        } else if u32::try_from(v).is_ok() {
            self.a.ril(Ril::Llilf, r, v as u32);
        } else {
            self.a.ril(Ril::Llihf, r, (v >> 32) as u32);
            if v as u32 != 0 {
                self.a.ril(Ril::Iilf, r, v as u32);
            }
        }
    }

    /// `r += imm` (`r0` holds a far `imm`; `r` not `r0`).
    fn add_imm(&mut self, r: u8, imm: i64) {
        if imm == 0 {
            return;
        }
        if fits16(imm) {
            self.a.ri(Ri::Aghi, r, imm as u16);
        } else if fits32(imm) {
            self.a.ril(Ril::Agfi, r, imm as u32);
        } else {
            self.li(R0, imm);
            self.a.rre(Rre::Agr, r, R0);
        }
    }

    /// `brasl %r14, sym`.
    fn call_sym(&mut self, sym: u32) {
        self.a.pcrel(Pcrel::Brasl, R14, RelKind::S390Call, sym);
    }
}

/// A register's DWARF number (`r0`.. 0, floats by `dwarf_f`).
fn debug_reg(r: crate::regalloc::R, _size: u32) -> Option<(u16, u16)> {
    if r.fp {
        return (r.reg < 16).then(|| (dwarf_f(r.reg as u8), 0));
    }
    (r.reg < 16).then_some((r.reg as u16, 0))
}

/// The rotate of a `k`: `rll` reads and writes the low word only.
const fn rotl(k: K) -> Rsy {
    if matches!(k, K::I64) {
        Rsy::Rllg
    } else {
        Rsy::Rll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn em(obj: &mut Obj) -> Em<'_> {
        Em {
            obj,
            start: 0,
            a: Asm::new(false),
            fixes: Vec::new(),
            labels: Vec::new(),
            slot_off: Vec::new(),
            frame: 0,
            lines: Vec::new(),
            ra: Alloc::default(),
            opt: false,
            fsaved: Vec::new(),
            saves: Saves::default(),
            short: false,
            x: X,
            y: Y,
            waits: crate::lir::Waits::default(),
            flags: None,
            skipped: crate::lir::Waits::default(),
        }
    }

    /// The value `li` leaves in its register, from the listing of
    /// `lghi`, `lgfi`, `llilf`, `llihf`, `iilf`.
    fn run(a: &Asm) -> i64 {
        let l = crate::asm::Listing {
            name: "f",
            code: &a.code,
            labels: Vec::new(),
            relocs: &[],
        };
        let mut x = 0u64;
        for (_, t) in a.lines(&l) {
            let (mn, ops) = t.split_once('\t').expect("operands");
            let i: i64 = ops
                .rsplit(", ")
                .next()
                .and_then(|v| v.parse().ok())
                .expect(&t);
            match mn {
                "lghi" | "lgfi" | "llilf" => x = i as u64,
                "llihf" => x = (i as u64) << 32,
                "iilf" => x = (x & !0xffff_ffff) | i as u64,
                _ => panic!("{t}"),
            }
        }
        x as i64
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
            0x7fff_ffff,
            -0x8000_0000,
            0x8000_0000,
            0xffff_ffff,
            0x1_0000_0000,
            -0x8000_0001,
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
            vs.push((x as i64) >> (x % 64));
        }
        for v in vs {
            let mut e = em(&mut obj);
            e.a = Asm::new(true);
            e.li(X, v);
            assert!(e.a.code.len() <= 12, "{v:#x}");
            assert_eq!(run(&e.a), v, "{v:#x}");
        }
    }

    /// The prologue the unwind tables describe (from `llvm-mc
    /// -triple=s390x -mcpu=z10 -show-encoding`).
    #[test]
    fn prologue_and_epilogue() {
        // stmg %r6, %r15, 48(%r15); lgr %r11, %r15
        const PROLOGUE: [u8; 10] = [0xeb, 0x6f, 0xf0, 0x30, 0x00, 0x24, 0xb9, 0x04, 0x00, 0xbf];
        let mut obj = Obj::new(false, true, false);
        let mut e = em(&mut obj);
        e.frame = 160;
        e.prologue();
        assert_eq!(e.a.code[..10], PROLOGUE);
        // aghi %r15, -160
        assert_eq!(e.a.code[10..], [0xa7, 0xfb, 0xff, 0x60]);
        // Callee-saved float registers below the incoming `r15`.
        let mut e = em(&mut obj);
        e.frame = 176;
        e.fsaved = vec![8, 15];
        e.prologue();
        assert_eq!(
            e.a.code[14..],
            [
                0xed, 0x80, 0xbf, 0xf8, 0xff, 0x67, // stdy %f8, -8(%r11)
                0xed, 0xf0, 0xbf, 0xf0, 0xff, 0x67, // stdy %f15, -16(%r11)
            ]
        );
        assert_eq!(e.saves.at, 26);
        assert_eq!(e.saves.regs, [(24, -168), (31, -176)]);
        let mut e = em(&mut obj);
        e.epilogue();
        e.call_sym(0);
        assert_eq!(
            e.a.code,
            [
                0xeb, 0x6f, 0xb0, 0x30, 0x00, 0x04, // lmg %r6, %r15, 48(%r11)
                0x07, 0xfe, // br %r14
                0xc0, 0xe5, 0, 0, 0, 0, // brasl %r14, sym
            ]
        );
        assert_eq!(e.a.relocs, [(10, RelKind::S390Call, 0)]);
    }
}
