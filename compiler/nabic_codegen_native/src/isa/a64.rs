//! LIR -> arm64 machine code.
//!
//! Frame: `fp` / `lr` pushed, `fp` = that pair, the callee-saved pairs
//! used just below it (as compact unwind lays them out), then the slots,
//! outgoing arguments at `sp`. Scratch registers are caller-saved
//! (`x9`..`x17`, `v16`..`v18`). Above `-O0` the allocator (`regalloc`)
//! puts values in `x19`..`x28`, `v8`..`v15` and `v19`..`v31`; every other
//! LIR value lives in its slot. In straight-line code the emitter tracks
//! which register still holds a slot (`Held`) and delays the store of a
//! slot written and read once (`Pend`). Branches, labels and calls see
//! every slot in memory.

pub mod enc;

mod arith;
mod frame;
mod mem;
mod select;
mod vector;

use crate::lir::NO_SLOT;
use crate::lir::{
    Bin, Callee, Cc, Conv, Func, Inst, K, Label, Loc, Mem, Order, Rmw, Un, V, Waits, scalar,
    waitable,
};
use crate::obj::{FnRange, Obj, RelKind, Reloc, Saves, Sect};
use crate::regalloc::{Alloc, Regs};
use enc::{
    AddOp, Asm, Bitmask, Cond, Dp1, Dp2, Dp3, Excl, Fp, Index, Ins, LDRB, LDRD, LDRH, LDRS, LDRSB,
    LDRSH, LDRSW, LDRW, LDRX, Log, Ls, Lse, Pair, R, STRB, STRD, STRH, STRS, STRW, STRX, Shift,
    V as Vr,
};

/// Stack probe interval.
const PAGE: u32 = 4096;
const SP: u32 = 31;
const ZR: u32 = 31;
const FP: u32 = 29;
/// Data scratch.
const X9: u32 = 9;
const X10: u32 = 10;
const X11: u32 = 11;
const X12: u32 = 12;
const X13: u32 = 13;
/// Pointer bases of indirect source / destination memory.
const X14: u32 = 14;
const X15: u32 = 15;
/// Call target; address legalization.
const X16: u32 = 16;
const X17: u32 = 17;
const V16: u32 = 16;
const V17: u32 = 17;
const V18: u32 = 18;

enum Fix {
    /// `b` (26-bit).
    B(Label),
    /// `b.cond`, `cbz`, `cbnz` (19-bit).
    C(Label),
}

/// A register that holds the `size` bytes at `off` of frame slot `slot`. `ext`:
/// its upper bits are those bytes extended (`Some(signed)`), or unknown.
#[derive(Clone, Copy)]
struct Held {
    reg: u32,
    fp: bool,
    slot: u32,
    off: u32,
    size: u32,
    ext: Option<bool>,
}

/// A store not yet emitted: `reg` into byte `off` of `slot`, at `sp + at`.
#[derive(Clone, Copy)]
struct Pend {
    reg: u32,
    op: Ls,
    slot: u32,
    off: u32,
    at: i64,
}

/// Value registers, picked by `pick`: those no instruction uses for a
/// fixed purpose across its operand loads.
const POOL: [u32; 5] = [X9, X10, X11, X12, X13];
const FP_POOL: [u32; 3] = [V16, V17, V18];

/// What the allocator may use: `x18` is the platform's, `x9`..`x17` and
/// `v16`..`v18` scratch; `x0`..`x8` / `v0`..`v7` carry arguments and
/// results.
/// An immediate `cmp`, `add` / `sub` take no 12 bits of, and any
/// multiplier but a power of two (a shift).
fn costly_imm(op: Option<Bin>, v: i64) -> bool {
    match op {
        None | Some(Bin::Add | Bin::Sub) => !(-4095..4096).contains(&v),
        Some(Bin::Mul) => !(v > 0 && (v as u64).is_power_of_two()),
        _ => false,
    }
}

const REGS: Regs = Regs {
    int_saved: &[19, 20, 21, 22, 23, 24, 25, 26, 27, 28],
    int_free: &[],
    int_arg: &[0, 1, 2, 3, 4, 5, 6, 7, 8],
    fp_saved: &[8, 9, 10, 11, 12, 13, 14, 15],
    fp_free: &[19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31],
    fp_arg: &[0, 1, 2, 3, 4, 5, 6, 7],
    int_args: &[0, 1, 2, 3, 4, 5, 6, 7],
    fp_args: &[0, 1, 2, 3, 4, 5, 6, 7],
    int_rets: &[0, 1, 2, 3, 4, 5, 6, 7],
    fp_rets: &[0, 1, 2, 3, 4, 5, 6, 7],
    sret: 8,
    word: 8,
    ptr: 8,
    fp_size: 8,
    vec: 16,
    vec_saved: false,
    costly_imm: Some(costly_imm),
    pairs: false,
    saved_pairs: true,
    saves_all: false,
    clobber: None,
};

/// Every prologue (before the frame's `sub sp`, undone via `fp`): `stp
/// x29, x30, [sp, #-16]!; mov x29, sp`, as the unwind encodings
/// (`coff::A64_CODES`, Mach-O frame mode) say.
pub const PROLOGUE: [u32; 2] = [PRO[0].word(), PRO[1].word()];
const PRO: [Ins; 2] = [
    enc::pair(Pair::StpX, Index::Pre, FP, 30, enc::SP, -2),
    enc::add_imm(AddOp::Add, true, R(FP), enc::SP, 0, false),
];
/// Every epilogue: `mov sp, x29; ldp x29, x30, [sp], #16; ret`.
pub const EPILOGUE: [u32; 3] = [EPI[0].word(), EPI[1].word(), EPI[2].word()];
const EPI: [Ins; 3] = [
    enc::add_imm(AddOp::Add, true, enc::SP, R(FP), 0, false),
    enc::pair(Pair::LdpX, Index::Post, FP, 30, enc::SP, 2),
    enc::ret(),
];

struct Em<'a> {
    obj: &'a mut Obj,
    start: u32,
    a: Asm,
    relocs: Vec<(u32, RelKind, u32)>,
    fixes: Vec<(usize, Fix)>,
    labels: Vec<Option<usize>>,
    slot_off: Vec<u32>,
    frame: u32,
    lines: Vec<(u32, u32, u32, u32)>,
    held: Vec<Held>,
    /// Slots whose address is taken: a store through a pointer may write them.
    escaped: Vec<bool>,
    pending: Vec<Pend>,
    /// `waitable`: read at most once, the store may wait; never read, it is
    /// dead.
    waits: Waits,
    /// The relocation of the next instruction.
    next_rel: Option<(RelKind, u32)>,
    /// A comparison left its result in the flags for the next `Br`.
    flags: Option<Cond>,
    /// An `ands` set the flags by this part for the `Cmp` with zero next.
    tested: Option<Mem>,
    ra: Alloc,
    /// Callee-saved pairs stored below `fp`, the first highest: `(first,
    /// second, fp)`.
    pairs: Vec<(u32, u32, bool)>,
    saves: Saves,
    /// Above `-O0`: conditional branches go straight to their target
    /// (19-bit), unless `far`.
    short: bool,
    opt: bool,
    /// An index scaling `lir::scaled` left to the add after it.
    scaled: Option<(Mem, V, u32)>,
    /// `-g` above `-O0`: the slot parts whose waiting store went out, and
    /// the code offset past it (`locs`).
    stored: Vec<((u32, u32), u32)>,
    /// Above `-O0`, by label: a loop head, aligned to 16 bytes so a loop's
    /// speed does not hang on where unrelated code put it.
    heads: Vec<bool>,
}

/// A 64-bit operation for `k`.
const fn sf(k: K) -> bool {
    matches!(k, K::I64 | K::F64)
}

/// Emit `f` into `obj`'s text.
pub fn emit(obj: &mut Obj, f: &Func) {
    // The outgoing argument area: the most any call puts on the stack.
    let mut out = 0u32;
    for i in &f.insts {
        if let Inst::Call { args, .. } = i {
            for (p, _) in args {
                match p.loc {
                    Loc::Stack(o) => out = out.max(o + p.k.size().max(8)),
                    Loc::VecStack(o) => out = out.max(o + 16),
                    _ => {}
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
    } = crate::frame::layout(f, &|s| ra.gone(s), (out.next_multiple_of(16), 16, 0), 8);
    let runs = crate::roots::frame(obj, f, &slot_off, 8, &|s| ra.gone(s));
    // Whole pairs: compact unwind restores both.
    let mut pairs = Vec::new();
    for (regs, base, fp) in [(&ra.saved, 19, false), (&ra.saved_fp, 8, true)] {
        let mut ps: Vec<u32> = regs.iter().map(|r| (r - base) / 2).collect();
        ps.dedup();
        pairs.extend(ps.into_iter().map(|p| (base + 2 * p, base + 2 * p + 1, fp)));
    }
    let frame = (at.next_multiple_of(16)) + 16 * pairs.len() as u32;
    let (waits, escaped) = waitable(f);
    // Functions and loop heads 16-byte aligned above `-O0`, as fetch
    // works in aligned blocks.
    let start = obj.sect(Sect::Text).align_to(if f.opt { 16 } else { 4 });
    let heads = if f.opt {
        crate::regalloc::loop_heads(f)
    } else {
        Vec::new()
    };
    // Short conditional branches first; a target out of their reach
    // means the long forms throughout.
    let text = obj.asm.is_some();
    for short in [f.opt, false] {
        let mut e = Em {
            obj: &mut *obj,
            start,
            a: Asm::new(text),
            relocs: Vec::new(),
            fixes: Vec::new(),
            labels: vec![None; f.labels as usize],
            slot_off: slot_off.clone(),
            frame,
            lines: Vec::new(),
            held: Vec::new(),
            escaped: escaped.clone(),
            pending: Vec::new(),
            waits: waits.clone(),
            next_rel: None,
            flags: None,
            tested: None,
            ra: ra.clone(),
            pairs: pairs.clone(),
            saves: Saves::default(),
            short,
            opt: f.opt,
            scaled: None,
            stored: Vec::new(),
            heads: heads.clone(),
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
        let off = |s: u32| e.slot_off[s as usize] as i32 - frame as i32;
        let end = 4 * e.a.pos() as u32;
        let locs = track
            .map(|t| t.finish(&e.ra, (&e.waits, &e.stored), (end, 8), &off, &debug_reg))
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
    /// Append `w`. Every instruction written names its destination in bits
    /// 0..5 (write-back bases and `cas`'s `rs` aside), so a store pending on
    /// that register goes out first and its `Held` is dropped.
    fn ins(&mut self, i: Ins) {
        self.flush_reg(i.dst());
        if let Some((kind, sym)) = self.next_rel.take() {
            let off = self.start + 4 * self.a.pos() as u32;
            self.relocs.push((off, kind, sym));
        }
        self.a.push(i);
        self.clobber(i.dst());
    }

    fn emit_pend(&mut self, p: Pend) {
        let idx = (p.at >> p.op.size()) as u32;
        self.a.push(enc::ldst(p.op, p.reg, enc::SP, idx));
        self.stored.push(((p.slot, p.off), 4 * self.a.pos() as u32));
    }

    /// Emit the stores waiting on register `r`.
    fn flush_reg(&mut self, r: u32) {
        while let Some(i) = self.pending.iter().position(|p| p.reg == r) {
            let p = self.pending.remove(i);
            self.emit_pend(p);
        }
    }

    /// Emit the stores waiting on slot `slot` (`None`: all).
    fn flush(&mut self, slot: Option<u32>) {
        while let Some(i) = self
            .pending
            .iter()
            .position(|p| slot.is_none_or(|s| p.slot == s))
        {
            let p = self.pending.remove(i);
            self.emit_pend(p);
        }
    }

    /// Slot `m`'s one read takes the register: its store is dead.
    fn consume(&mut self, m: Mem) {
        if let Mem::Frame(s, o) = m {
            self.pending.retain(|p| p.slot != s || p.off != o);
        }
    }

    /// The first value register not in `avoid`, preferring one that no
    /// store waits on and holds nothing.
    fn pick(&self, pool: &[u32], avoid: &[u32]) -> u32 {
        let ok = |r: &&u32| !avoid.contains(r);
        let free = |r: &&u32| ok(r) && !self.pending.iter().any(|p| p.reg == **r);
        pool.iter()
            .filter(free)
            .find(|r| !self.held.iter().any(|h| h.reg == **r))
            .or_else(|| pool.iter().find(free))
            .or_else(|| pool.iter().find(ok))
            .copied()
            .unwrap_or(pool[0])
    }

    fn clobber(&mut self, r: u32) {
        self.held.retain(|h| h.reg != r);
    }

    /// A call or a join: no register is known.
    fn forget(&mut self) {
        self.held.clear();
    }

    /// `size` bytes at `m` were written.
    fn wrote(&mut self, m: Mem, size: u32) {
        match m {
            Mem::Frame(s, o) => self
                .held
                .retain(|h| h.slot != s || h.off >= o + size || o >= h.off + h.size),
            Mem::Ind(..) => {
                let esc = &self.escaped;
                self.held.retain(|h| !esc[h.slot as usize]);
            }
        }
    }

    fn find(&self, m: Mem, size: u32, fp: bool) -> Option<Held> {
        let Mem::Frame(s, o) = m else { return None };
        self.held
            .iter()
            .find(|h| h.slot == s && h.off == o && h.size == size && h.fp == fp)
            .copied()
    }

    fn hold(&mut self, reg: u32, fp: bool, m: Mem, size: u32, ext: Option<bool>) {
        if self.ra.get(m).is_some() {
            return;
        }
        if let Mem::Frame(slot, off) = m {
            self.held.push(Held {
                reg,
                fp,
                slot,
                off,
                size,
                ext,
            });
        }
    }

    /// The integer register `m` is allocated to, if any.
    fn dst_x(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| !a.fp).map(|a| a.reg)
    }

    /// The vector register `m` is allocated to, if any.
    fn dst_v(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| a.fp).map(|a| a.reg)
    }

    fn pos(&self) -> usize {
        self.a.pos()
    }

    /// Relocate the next instruction.
    const fn reloc(&mut self, kind: RelKind, sym: u32) {
        self.next_rel = Some((kind, sym));
    }

    /// Patch the branches and write the code out; `false` (nothing
    /// written) when a short branch cannot reach.
    fn finish(mut self, sym: u32, debug: Option<(crate::lir::FnDebug, Vec<i32>)>) -> bool {
        for (at, fix) in std::mem::take(&mut self.fixes) {
            let (Fix::B(l) | Fix::C(l)) = fix;
            let target = self.labels[l as usize].expect("bound label");
            let d = target as i64 - at as i64;
            if let Fix::C(_) = fix {
                if !enc::reaches19(d) {
                    return false;
                }
                self.a.patch19(at, d);
            } else {
                self.a.patch26(at, d);
            }
        }
        let len = 4 * self.a.pos() as u32;
        let start = self.start;
        let bytes: Vec<u8> = self.a.code.iter().flat_map(|w| w.to_le_bytes()).collect();
        if self.obj.asm.is_some() {
            self.list(sym, &bytes);
        }
        let text = self.obj.sect(Sect::Text);
        text.data.extend_from_slice(&bytes);
        for (off, kind, s) in self.relocs {
            text.relocs.push(Reloc { off, kind, sym: s });
        }
        let lines = std::mem::take(&mut self.lines);
        let s = &mut self.obj.syms[sym as usize];
        s.def = Some((Sect::Text, start, len));
        self.obj.fns.push(FnRange {
            locs: Vec::new(),
            sym,
            start,
            len,
            lines,
            debug,
            saves: self.saves,
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
        let apple = !self.obj.elf && !self.obj.coff;
        let lines = self.a.lines(&l, apple);
        crate::asm::function(self.obj, &l, &lines, crate::asm::Bytes::Words);
    }

    fn movz(&mut self, rd: u32, imm: u32, hw: u32) {
        self.ins(enc::movz(R(rd), imm, hw));
    }

    fn movk(&mut self, rd: u32, imm: u32, hw: u32) {
        self.ins(enc::movk(R(rd), imm, hw));
    }

    fn movn(&mut self, rd: u32, imm: u32, hw: u32) {
        self.ins(enc::movn(R(rd), imm, hw));
    }

    fn mov_imm(&mut self, rd: u32, v: u64) {
        let halves = |x: u64| (0..4).filter(|h| (x >> (16 * h)) & 0xffff != 0).count();
        if self.opt && halves(v).min(halves(!v)) > 1 && self.mov_logical(rd, v) {
            return;
        }
        if halves(!v) < halves(v) {
            // `movn` then `movk` the halfwords that are not all ones.
            let mut first = true;
            for h in 0..4u32 {
                let part = ((v >> (16 * h)) & 0xffff) as u32;
                if first {
                    if part != 0xffff || h == 3 {
                        self.movn(rd, !part & 0xffff, h);
                        first = false;
                    }
                } else if part != 0xffff {
                    self.movk(rd, part, h);
                }
            }
            if first {
                self.movn(rd, 0, 0);
            }
            return;
        }
        let mut first = true;
        for h in 0..4u32 {
            let part = ((v >> (16 * h)) & 0xffff) as u32;
            if part != 0 {
                if first {
                    self.movz(rd, part, h);
                    first = false;
                } else {
                    self.movk(rd, part, h);
                }
            }
        }
        if first {
            self.movz(rd, 0, 0);
        }
    }

    /// `v` as `orr rd, xzr, #b` for a logical immediate `b`, then a
    /// `movk` of the one halfword where they differ, if shorter than the
    /// moves (a division's magic number: `0xcccc_cccc_cccc_cccd`).
    fn mov_logical(&mut self, rd: u32, v: u64) -> bool {
        let orr = |e: &mut Self, b: Bitmask| e.ins(enc::log_imm(Log::Orr, true, R(rd), R(31), b));
        if let Some(b) = enc::bitmask(v, 64) {
            orr(self, b);
            return true;
        }
        let halves = |x: u64| (0..4).filter(|h| (x >> (16 * h)) & 0xffff != 0).count();
        if halves(v).min(halves(!v)) < 3 {
            return false;
        }
        let part = |x: u64, h: u32| (x >> (16 * h)) & 0xffff;
        for h in 0..4u32 {
            for n in [1, 3, 2] {
                let m = 0xffffu64 << (16 * h);
                let b = (v & !m) | (part(v, (h + n) % 4) << (16 * h));
                if let Some(b) = enc::bitmask(b, 64) {
                    orr(self, b);
                    self.movk(rd, part(v, h) as u32, h);
                    return true;
                }
            }
        }
        false
    }

    /// `rd = rn + imm` (64-bit; `sp` allowed for both).
    fn add_imm(&mut self, rd: u32, rn: u32, imm: i64) {
        let (op, a) = if imm < 0 {
            (AddOp::Sub, imm.unsigned_abs())
        } else {
            (AddOp::Add, imm as u64)
        };
        if a == 0 {
            if rd != rn {
                self.ins(enc::add_imm(AddOp::Add, true, R(rd), R(rn), 0, false));
            }
            return;
        }
        if a < 1 << 24 {
            let (hi, lo) = ((a >> 12) as u32, (a & 0xfff) as u32);
            let mut src = rn;
            if hi != 0 {
                self.ins(enc::add_imm(op, true, R(rd), R(src), hi, true));
                src = rd;
            }
            if lo != 0 {
                self.ins(enc::add_imm(op, true, R(rd), R(src), lo, false));
            }
            return;
        }
        // Far: through x17, extended-register form (takes `sp`).
        self.mov_imm(X17, imm as u64);
        let x = enc::add_ext(AddOp::Add, true, R(rd), R(rn), R(X17), enc::Ext::Uxtx);
        self.ins(x);
    }
}

/// The overflow condition of an `adds` / `subs` for `op`.
const fn ovf_cond(op: Bin, signed: bool) -> Cond {
    match (op, signed) {
        (_, true) => Cond::VS,
        (Bin::Add, false) => Cond::HS,
        _ => Cond::LO,
    }
}

/// `fmov` between two `d` (`d`) or `s` registers.
const fn fmov(d: bool, vd: u32, vn: u32) -> Ins {
    enc::fp(Fp::Fmov, d, Vr(vd), Vr(vn), Vr(0))
}

/// A register's DWARF and `CodeView` numbers (`x0`.. / `v0`..; `CodeView`
/// only the integer registers, `X0` = 50).
fn debug_reg(r: crate::regalloc::R, size: u32) -> Option<(u16, u16)> {
    let n = r.reg as u16;
    // `CodeView`: `CV_ARM64_S0` / `_D0` / `_Q0` by size, `CV_ARM64_X0`
    // (x29 `_FP`, x30 `_LR` follow).
    if r.fp {
        let cv = match size {
            4 => 100,
            8 => 140,
            _ => 180,
        };
        return (n < 32).then_some((64 + n, cv + n));
    }
    (n <= 30).then_some((n, 50 + n))
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
            frame: 0,
            lines: Vec::new(),
            held: Vec::new(),
            escaped: Vec::new(),
            pending: Vec::new(),
            waits: Waits::default(),
            next_rel: None,
            flags: None,
            tested: None,
            ra: Alloc::default(),
            pairs: Vec::new(),
            saves: Saves::default(),
            short: false,
            opt: false,
            scaled: None,
            stored: Vec::new(),
            heads: Vec::new(),
        }
    }

    /// Scratch registers are caller-saved (`x19`..`x28`, `v8`..`v15` are
    /// not): the unwind encodings describe only the saves the allocator asks
    /// for.
    #[test]
    fn values_stay_in_caller_saved_registers() {
        for r in POOL.iter().chain(&[X14, X15, X16, X17]) {
            assert!(!(19..=28).contains(r), "x{r}");
        }
        for v in FP_POOL {
            assert!(!(8..=15).contains(&v), "v{v}");
        }
        let mut obj = Obj::new(false, false, false);
        let mut e = em(&mut obj);
        e.frame = 3 * PAGE + 32;
        e.prologue();
        assert_eq!(e.a.code[..2], PROLOGUE);
    }

    /// The prologue and epilogue the unwind encodings describe.
    #[test]
    fn prologue_and_epilogue_words() {
        assert_eq!(PROLOGUE, [0xa9bf_7bfd, 0x9100_03fd]);
        assert_eq!(EPILOGUE, [0x9100_03bf, 0xa8c1_7bfd, 0xd65f_03c0]);
    }

    /// Atomics without LSE (ARMv8.0): `ldaxr` / `stlxr` loops as the
    /// reference assembler encodes them.
    #[test]
    fn llsc_atomics_encode() {
        let mut obj = Obj::new(false, true, false);
        let mut e = em(&mut obj);
        e.llsc_rmw(Rmw::Add, 3, Order::SeqCst);
        assert_eq!(
            e.a.code,
            [
                0xc85f_fdc9, // ldaxr x9, [x14]
                0x8b0a_012b, // add x11, x9, x10
                0xc80c_fdcb, // stlxr w12, x11, [x14]
                0x35ff_ffac, // cbnz w12, the ldaxr
            ]
        );
        e.a.code.clear();
        e.llsc_rmw(Rmw::Xchg, 1, Order::SeqCst);
        assert_eq!(
            e.a.code,
            [
                0x485f_fdc9, // ldaxrh w9, [x14]
                0x480c_fdca, // stlxrh w12, w10, [x14]
                0x35ff_ffcc, // cbnz w12, the ldaxrh
            ]
        );
        e.a.code.clear();
        e.llsc_rmw(Rmw::And, 2, Order::SeqCst);
        assert_eq!(e.a.code[0], 0x885f_fdc9); // ldaxr w9, [x14]
        assert_eq!(e.a.code[1], 0x8a0a_012b); // and x11, x9, x10
        e.a.code.clear();
        e.llsc_cas(0, Order::SeqCst);
        assert_eq!(
            e.a.code,
            [
                0x085f_fdcb, // ldaxrb w11, [x14]
                0x6b29_017f, // cmp w11, w9, uxtb
                0x5400_0081, // b.ne the clrex
                0x080c_fdca, // stlxrb w12, w10, [x14]
                0x35ff_ff8c, // cbnz w12, the ldaxrb
                0x1400_0002, // b past the clrex
                0xd503_3f5f, // clrex
            ]
        );
        e.a.code.clear();
        e.llsc_cas(1, Order::SeqCst);
        assert_eq!(e.a.code[1], 0x6b29_217f); // cmp w11, w9, uxth
        e.a.code.clear();
        e.llsc_cas(3, Order::SeqCst);
        assert_eq!(e.a.code[1], 0xeb09_017f); // cmp x11, x9
        // No LSE word (`ldadd` .. `swp`, `cas`) in either.
        for w in &e.a.code {
            assert_ne!(w & 0x3f20_0c00, 0x3820_0000, "{w:#x}");
            assert_ne!(w & 0x3fa0_7c00, 0x08a0_7c00, "{w:#x}");
        }
    }

    /// Windows with `x19 x20` and `d8 d9` saved: the pairs stored from `sp`
    /// before `x29` is set, the codes (`add_fp`, `save_fregp`, `save_regp`,
    /// `alloc_s`, `save_fplr_x`, `end`) for each instruction.
    #[test]
    fn windows_prologue_unwind_codes() {
        let mut obj = Obj::new(false, false, true);
        let mut e = em(&mut obj);
        e.pairs = vec![(19, 20, false), (8, 9, true)];
        e.frame = 48;
        e.prologue();
        assert_eq!(
            e.a.code,
            [
                0xa9bf_7bfd, // stp x29, x30, [sp, #-16]!
                0xd100_83ff, // sub sp, sp, #32
                0xa901_53f3, // stp x19, x20, [sp, #16]
                0x6d00_27e8, // stp d8, d9, [sp]
                0x9100_83fd, // add x29, sp, #32
                0xd100_43ff, // sub sp, sp, #16
            ]
        );
        assert_eq!(
            e.saves.win,
            [0xe2, 4, 0xd8, 0x00, 0xc8, 0x02, 0x02, 0x81, 0xe4]
        );
    }

    /// Logical immediates as the reference assembler encodes them (`and` /
    /// `orr` / `eor` x0 or w0, x1 or w1), and values that do not encode.
    #[test]
    fn logical_immediates_encode() {
        for (op, v, bits, word) in [
            (Log::And, 0x3ffu64, 64, 0x9240_2420u32),
            (Log::And, 0xc0, 32, 0x121a_0420),
            (Log::Orr, 0xffff_0000_ffff_0000, 64, 0xb210_3c20),
            (Log::Eor, 0x5555_5555_5555_5555, 64, 0xd200_f020),
            (Log::And, 0xffff_ffff_ffff_fff0, 64, 0x927c_ec20),
            (Log::And, 0x8000_0001, 32, 0x1201_0420),
        ] {
            let m = enc::bitmask(v, bits).expect("encodes");
            let w = enc::log_imm(op, bits == 64, R(0), R(1), m).word();
            assert_eq!(w, word, "{v:#x}");
        }
        for v in [0, u64::MAX, 0x1234, 0x5] {
            assert_eq!(enc::bitmask(v, 64), None, "{v:#x}");
        }
    }

    /// Run the move-wide and 64-bit add / sub (immediate, extended
    /// register) words of `code` on `x` (31 is `sp`).
    fn run(code: &[u32], x: &mut [u64; 32]) {
        for &w in code {
            let (rd, rn) = ((w & 31) as usize, ((w >> 5) & 31) as usize);
            let hw = (w >> 21) & 3;
            let imm16 = u64::from((w >> 5) & 0xffff) << (16 * hw);
            match w & 0xff80_0000 {
                0xd280_0000 => x[rd] = imm16,
                0x9280_0000 => x[rd] = !imm16,
                0xf280_0000 => x[rd] = (x[rd] & !(0xffff << (16 * hw))) | imm16,
                op @ (0x9100_0000 | 0xd100_0000) => {
                    let imm = u64::from((w >> 10) & 0xfff) << (12 * ((w >> 22) & 1));
                    x[rd] = if op == 0x9100_0000 {
                        x[rn].wrapping_add(imm)
                    } else {
                        x[rn].wrapping_sub(imm)
                    };
                }
                // orr xd, xzr, #imm
                0xb200_0000 if rn == 31 => {
                    x[rd] = enc::decode_bitmask(
                        (w >> 22) & 1,
                        (w >> 16) & 0x3f,
                        (w >> 10) & 0x3f,
                        true,
                    );
                }
                _ if w & 0xffe0_fc00 == 0x8b20_6000 => {
                    x[rd] = x[rn].wrapping_add(x[((w >> 16) & 31) as usize]);
                }
                _ => panic!("unexpected word {w:#010x}"),
            }
        }
    }

    /// Halfword patterns: zeros, ones, and mixed values.
    fn values() -> Vec<u64> {
        let mut out = vec![0, 1, 0xffff, 0x1_0000, u64::MAX, u64::MAX - 1, 1 << 63];
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for _ in 0..2000 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let mut v = 0u64;
            for h in 0..4 {
                let part = match (seed >> (h * 8)) & 3 {
                    0 => 0,
                    1 => 0xffff,
                    _ => (seed >> (16 + h * 11)) & 0xffff,
                };
                v |= part << (16 * h);
            }
            out.push(v);
        }
        out
    }

    #[test]
    fn mov_imm_builds_every_value_in_fewest_words() {
        let mut obj = Obj::new(false, false, false);
        for v in values() {
            let mut e = em(&mut obj);
            e.mov_imm(X9, v);
            let mut x = [0xdead_beef_u64; 32];
            run(&e.a.code, &mut x);
            assert_eq!(x[9], v, "{v:#x}: {:x?}", e.a.code);
            let count = |y: u64| (0..4).filter(|h| (y >> (16 * h)) & 0xffff != 0).count();
            let best = count(v).min(count(!v)).max(1);
            assert_eq!(e.a.code.len(), best, "{v:#x}: {:x?}", e.a.code);
        }
    }

    #[test]
    fn mov_imm_takes_logical_immediates_above_o0() {
        let mut obj = Obj::new(false, false, false);
        let mut vs = values();
        vs.extend([
            0xcccc_cccc_cccc_cccd,
            0xaaaa_aaaa_aaaa_aaab,
            0x5555_5555_5555_5555,
            0x0f0f_0f0f_0f0f_0f0f,
            0x8000_0000_0000_0001,
        ]);
        for v in vs {
            let mut e = em(&mut obj);
            e.opt = true;
            e.mov_imm(X9, v);
            let mut x = [0xdead_beef_u64; 32];
            run(&e.a.code, &mut x);
            assert_eq!(x[9], v, "{v:#x}: {:x?}", e.a.code);
            let count = |y: u64| (0..4).filter(|h| (y >> (16 * h)) & 0xffff != 0).count();
            assert!(e.a.code.len() <= count(v).min(count(!v)).max(1), "{v:#x}");
        }
        let mut e = em(&mut obj);
        e.opt = true;
        e.mov_imm(X9, 0xcccc_cccc_cccc_cccd);
        assert_eq!(e.a.code.len(), 2);
    }

    #[test]
    fn add_imm_reaches_far_offsets() {
        let mut obj = Obj::new(false, false, false);
        let imms = [
            0,
            1,
            -1,
            8,
            4095,
            4096,
            4097,
            -4096,
            -4097,
            0xff_ffff,
            -0xff_ffff,
            1 << 24,
            -(1 << 24) - 8,
            i64::from(i32::MAX),
            -(1 << 40),
        ];
        for imm in imms {
            for (rd, rn) in [(X9, X9), (X9, SP), (SP, SP), (X10, X9)] {
                let mut e = em(&mut obj);
                e.add_imm(rd, rn, imm);
                let mut x = [0u64; 32];
                x[rn as usize] = 0x1000_0000_0000;
                run(&e.a.code, &mut x);
                let want = 0x1000_0000_0000u64.wrapping_add(imm as u64);
                assert_eq!(x[rd as usize], want, "{imm} x{rd} x{rn}: {:x?}", e.a.code);
                assert!(
                    e.a.code.len() <= 2 || imm.unsigned_abs() >= 1 << 24,
                    "{imm}"
                );
            }
        }
    }

    /// A load's address: the scaled 12-bit form, the unscaled 9-bit form
    /// or `x17` from `add_imm`.
    #[test]
    fn ldst_addresses_every_offset() {
        let mut obj = Obj::new(false, false, false);
        for op in [LDRX, LDRW, LDRH, LDRB, LDRD] {
            let size = 1i64 << op.size();
            for off in [
                0,
                size,
                -size,
                255,
                -256,
                -257,
                4095 * size,
                4096 * size,
                12,
                301,
                1 << 20,
            ] {
                let mut e = em(&mut obj);
                e.ldst(op, X9, X10, off);
                let (last, setup) = e.a.code.split_last().unwrap();
                let mut x = [0u64; 32];
                x[10] = 0x10_0000;
                run(setup, &mut x);
                let base = x[((last >> 5) & 31) as usize];
                let addr = if last & (1 << 24) != 0 {
                    base + (u64::from((last >> 10) & 0xfff) << op.size())
                } else {
                    assert_eq!(last & ((1 << 21) | (3 << 10)), 0, "{last:#x}");
                    let imm9 = i64::from(((last >> 12) & 0x1ff) as i32) << 55 >> 55;
                    base.wrapping_add(imm9 as u64)
                };
                assert_eq!(
                    addr,
                    0x10_0000u64.wrapping_add(off as u64),
                    "{size} {off}: {:x?}",
                    e.a.code
                );
                assert_eq!(last & 31, X9, "{last:#x}");
                let opcode = |w: u32| w & 0xffc0_0000 & !(1 << 24);
                let want = enc::ldst(op, 0, R(0), 0).word();
                assert_eq!(opcode(*last), opcode(want), "{last:#x}");
            }
        }
    }
}
