//! LIR -> `x86_64` machine code (System V; Windows x64 in COFF objects).
//!
//! Frame: `rbp` pushed, `rbp` = it, the callee-saved registers used just
//! below it, then the slots, outgoing arguments at `rsp`. Scratch: `rax
//! rcx rdx r8 r9`, `xmm0`..`xmm3`; pointer bases `r10 r11`, all
//! caller-saved in both conventions. Above `-O0` the allocator
//! (`regalloc`) puts values in `rbx r12`..`r15` (Windows: `rsi rdi` too),
//! System V's `rsi rdi` and `xmm4`..`xmm15`, Windows' `xmm4`..`xmm15`
//! (`xmm6`.. callee-saved, stored whole below the integer registers).
//! `Held` / `Pend` as on arm64. Fixed-register instructions (division,
//! conversions, atomics) load operands first, then `scratch`. The
//! baseline's instructions, but `popcnt`, `lzcnt` and `tzcnt` where the
//! CPU has them (`Obj::popcnt` ..), and AVX2 in vector loops.

pub mod enc;

mod arith;
mod call;
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
use enc::{Addr, Alu, Asm, Avx, Bits, Cond, Gpr, Rm, Seg, Shift, Sse, Sz, Xmm, Xrm};

const RAX: u32 = 0;
const RCX: u32 = 1;
const RDX: u32 = 2;
const RSP: u32 = 4;
const RBP: u32 = 5;
const RSI: u32 = 6;
const RDI: u32 = 7;
const R10: u32 = 10;
const R11: u32 = 11;
const R8: u32 = 8;
const R9: u32 = 9;
const XMM0: u32 = 0;
const XMM1: u32 = 1;
/// Value registers, picked by `pick`.
const POOL: [u32; 5] = [RAX, RCX, RDX, R8, R9];

const FP_POOL: [u32; 4] = [XMM0, XMM1, 2, 3];
/// Integer argument registers.
const ARGS: [u32; 6] = [RDI, RSI, RDX, RCX, 8, 9];
/// Windows x64: by register position.
const WIN_ARGS: [u32; 4] = [RCX, RDX, 8, 9];
/// Integer result registers.
const RETS: [u32; 3] = [RAX, RDX, RCX];
/// Stack probe interval.
const PAGE: u32 = 4096;
/// Windows x64 `UNWIND_CODE` operations.
const UWOP_PUSH_NONVOL: u8 = 0;
const UWOP_ALLOC_LARGE: u8 = 1;
const UWOP_ALLOC_SMALL: u8 = 2;
const UWOP_SET_FPREG: u8 = 3;
const UWOP_SAVE_NONVOL: u8 = 4;
const UWOP_SAVE_XMM128: u8 = 8;

/// Windows x64: bytes below `rbp` for `n` integer and `m` `xmm`
/// callee-saved registers (the `xmm` 16-byte aligned below the others).
const fn win_saves(n: usize, m: usize) -> u32 {
    (16 * n.div_ceil(2) + 16 * m) as u32
}

/// What the allocator may use (System V): `rsi rdi` and `xmm4`..`xmm7`
/// carry arguments; no `xmm` survives a call.
/// An immediate no instruction takes as 32 bits sign-extended.
fn costly_imm(op: Option<Bin>, v: i64) -> bool {
    !matches!(
        op,
        Some(Bin::Shl | Bin::Shr | Bin::Rotl | Bin::Div | Bin::Rem)
    ) && i32::try_from(v).is_err()
}

const SYSV: Regs = Regs {
    int_saved: &[3, 12, 13, 14, 15],
    int_free: &[],
    int_arg: &[RSI, RDI],
    fp_saved: &[],
    fp_free: &[8, 9, 10, 11, 12, 13, 14, 15],
    fp_arg: &[4, 5, 6, 7],
    int_args: &ARGS,
    fp_args: &[0, 1, 2, 3, 4, 5, 6, 7],
    int_rets: &RETS,
    fp_rets: &[0, 1],
    sret: RDI,
    word: 8,
    ptr: 8,
    fp_size: 8,
    vec: 16,
    vec_saved: false,
    costly_imm: Some(costly_imm),
    pairs: false,
    saved_pairs: false,
    saves_all: false,
    clobber: None,
};

/// Windows x64: `rsi rdi` callee-saved too; `xmm6`..`xmm15` are
/// callee-saved whole (128 bits): the prologue stores them with `movaps`.
const WIN: Regs = Regs {
    int_saved: &[3, RSI, RDI, 12, 13, 14, 15],
    int_free: &[],
    int_arg: &[],
    fp_saved: &[6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    fp_free: &[4, 5],
    fp_arg: &[],
    int_args: &WIN_ARGS,
    fp_args: &[0, 1, 2, 3],
    int_rets: &RETS,
    fp_rets: &[0, 1],
    sret: RCX,
    word: 8,
    ptr: 8,
    fp_size: 8,
    vec: 16,
    vec_saved: true,
    costly_imm: Some(costly_imm),
    pairs: false,
    saved_pairs: false,
    saves_all: false,
    clobber: None,
};

/// `i` as a `k` immediate extended by `signed` (as `Em::load` does).
const fn ext(i: i64, k: K, signed: bool) -> i64 {
    let bits = k.bits();
    if bits == 64 {
        i
    } else if signed {
        (i << (64 - bits)) >> (64 - bits)
    } else {
        i & ((1i64 << bits) - 1)
    }
}

/// A register (`fp`: `xmm`) holding `size` bytes at `off` of `slot`;
/// `ext`: upper bits are those bytes extended (`Some(signed)`) or unknown.
#[derive(Clone, Copy)]
struct Held {
    reg: u32,
    fp: bool,
    slot: u32,
    off: u32,
    size: u32,
    ext: Option<bool>,
}

/// A store not yet emitted: `reg` (`k`) into byte `off` of `slot`, at
/// `at`.
#[derive(Clone, Copy)]
struct Pend {
    reg: u32,
    fp: bool,
    k: K,
    slot: u32,
    off: u32,
    at: Addr,
}

struct Em<'a> {
    obj: &'a mut Obj,
    start: u32,
    a: Asm,
    /// `(position of a rel32, or a rel8 if, label)`.
    fixes: Vec<(usize, Label, bool)>,
    labels: Vec<Option<usize>>,
    slot_off: Vec<u32>,
    frame: u32,
    lines: Vec<(u32, u32, u32, u32)>,
    held: Vec<Held>,
    /// Slots whose address is taken: a store through a pointer may write them.
    escaped: Vec<bool>,
    pending: Vec<Pend>,
    waits: Waits,
    /// A comparison left its result in the flags for the next `Br`.
    flags: Option<Cond>,
    /// An `and` / `test` set the flags by this part for the `Cmp` with
    /// zero next.
    tested: Option<Mem>,
    ra: Alloc,
    /// Callee-saved registers stored below `rbp`, the first highest.
    saved: Vec<u32>,
    /// Windows: callee-saved `xmm` registers stored (128 bits) below them.
    saved_xmm: Vec<u32>,
    saves: Saves,
    /// Above `-O0`: branch layout may invert conditions.
    opt: bool,
    /// An index scaling `lir::scaled` left to the add after it.
    scaled: Option<(Mem, V, u32)>,
    /// `-g` above `-O0`: the slot parts whose waiting store went out, and
    /// the code offset past it (`locs`).
    stored: Vec<((u32, u32), u32)>,
    /// Which label branches are rel8, in order (`relax::relax`).
    plan: Option<Vec<bool>>,
    /// Each label branch so far: `(start, length, label)`.
    sites: Vec<(usize, u8, Label)>,
}

/// Emit `f` into `obj`'s text.
pub fn emit(obj: &mut Obj, f: &Func) {
    let mut out = 0u32;
    for i in &f.insts {
        if let Inst::Call { args, .. } = i {
            // Windows: the callee's 32-byte home area.
            if obj.coff {
                out = out.max(32);
            }
            for (p, _) in args {
                match p.loc {
                    Loc::Stack(o) => out = out.max(o + 8),
                    Loc::VecStack(o) => out = out.max(o + 16),
                    _ => {}
                }
            }
        }
    }
    let (owned, ra) = if f.opt {
        let regs = if obj.coff { &WIN } else { &SYSV };
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
    } = {
        let gone = |s| ra.gone(s);
        let base = (out.next_multiple_of(16), 16, 0);
        if f.opt {
            // Within reach of a disp8 from `rsp` above the outgoing
            // arguments and from `rbp` below the saves.
            let saves = if obj.coff {
                win_saves(ra.saved.len(), ra.saved_fp.len())
            } else {
                8 * ra.saved.len() as u32
            };
            let low = 128u32.saturating_sub(base.0);
            let high = 128u32.saturating_sub(saves + 8);
            let order = crate::frame::hot(f, 8, low, high);
            crate::frame::layout_in(f, &gone, base, 8, &order)
        } else {
            crate::frame::layout(f, &gone, base, 8)
        }
    };
    let runs = crate::roots::frame(obj, f, &slot_off, 8, &|s| ra.gone(s));
    let (saved, saved_xmm) = (&ra.saved, &ra.saved_fp);
    let frame = if obj.coff {
        (at + win_saves(saved.len(), saved_xmm.len())).next_multiple_of(16)
    } else {
        (at + 8 * saved.len() as u32).next_multiple_of(16)
    };
    let (waits, escaped) = waitable(f);
    let start = obj.sect(Sect::Text).align_to(16);
    let text = obj.asm.is_some();
    // Above `-O0` label branches are laid out twice: first all rel32,
    // then with rel8 wherever `relax::relax` finds it reaches (all rel32
    // again should a target move out of reach).
    let mut plan: Option<Vec<bool>> = None;
    let mut tried = false;
    loop {
        let mut e = Em {
            obj: &mut *obj,
            start,
            a: Asm::new(text),
            fixes: Vec::new(),
            labels: vec![None; f.labels as usize],
            slot_off: slot_off.clone(),
            frame,
            lines: Vec::new(),
            held: Vec::new(),
            escaped: escaped.clone(),
            pending: Vec::new(),
            waits: waits.clone(),
            flags: None,
            tested: None,
            ra: ra.clone(),
            saved: ra.saved.clone(),
            saved_xmm: ra.saved_fp.clone(),
            saves: Saves::default(),
            opt: f.opt,
            scaled: None,
            stored: Vec::new(),
            plan: plan.take(),
            sites: Vec::new(),
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
        if f.opt && !tried {
            tried = true;
            let p = crate::relax::relax(&e.sites, &e.labels, 2, &|at, to| {
                (-128..128).contains(&(to as i64 - at as i64 - 2))
            });
            if p.contains(&true) {
                plan = Some(p);
                continue;
            }
        }
        let off = |s: u32| e.slot_off[s as usize] as i32 - frame as i32;
        let locs = track
            .map(|t| {
                t.finish(
                    &e.ra,
                    (&e.waits, &e.stored),
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
        plan = None;
    }
}

impl Em<'_> {
    /// Into `obj`, unless a rel8 branch's target is out of its reach.
    fn finish(mut self, sym: u32, debug: Option<(crate::lir::FnDebug, Vec<i32>)>) -> bool {
        for (at, l, short) in std::mem::take(&mut self.fixes) {
            let target = self.labels[l as usize].expect("bound label");
            if !short {
                self.a.patch_rel32(at, target);
            } else if (-128..128).contains(&(target as i64 - at as i64 - 1)) {
                self.a.patch_rel8(at, target);
            } else {
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
            saves: self.saves,
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
        crate::asm::function(self.obj, &l, &lines, crate::asm::Bytes::Each);
    }

    /// The integer argument register at position `n`.
    fn arg_reg(&self, n: u8) -> u32 {
        if self.obj.coff {
            WIN_ARGS[n as usize]
        } else {
            ARGS[n as usize]
        }
    }

    /// The hidden result pointer's register.
    fn sret_reg(&self) -> u32 {
        if self.obj.coff { RCX } else { RDI }
    }

    /// Register `r` (`fp`: an `xmm`) is about to be written: the stores
    /// waiting on it go out first, and what it held is gone.
    fn w(&mut self, r: u32, fp: bool) {
        while let Some(i) = self.pending.iter().position(|p| p.reg == r && p.fp == fp) {
            let p = self.pending.remove(i);
            self.emit_pend(p);
        }
        self.held.retain(|h| h.reg != r || h.fp != fp);
    }

    /// Before code that writes scratch registers on its own: every store
    /// out, no register known.
    fn scratch(&mut self) {
        self.flush(None);
        self.held.clear();
    }

    /// `[rsp + d]`, or above `-O0` the same bytes from `rbp` (`rsp +
    /// frame` past the prologue) when that encodes shorter.
    fn sp(&self, d: i32) -> Addr {
        let fp = d - self.frame as i32;
        let short = |x: i32| (-128..128).contains(&x);
        if self.opt && (short(fp) || (d != 0 && !short(d))) {
            Addr::Bd(Gpr(RBP), fp)
        } else {
            Addr::Bd(Gpr(RSP), d)
        }
    }

    fn emit_pend(&mut self, p: Pend) {
        let m = p.at;
        if p.fp {
            self.a.sse_st(Self::movs_st(p.k), m, Xmm(p.reg));
        } else {
            self.store_raw(p.reg, Rm::M(m), p.k);
        }
        self.stored.push(((p.slot, p.off), self.a.pos() as u32));
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

    /// `size` bytes at `m` are read from memory: what they hold must be there.
    fn rd(&mut self, m: Mem, size: u32) {
        if let Mem::Frame(s, o) = m {
            while let Some(i) = self
                .pending
                .iter()
                .position(|p| p.slot == s && p.off < o + size && o < p.off + p.k.size())
            {
                let p = self.pending.remove(i);
                self.emit_pend(p);
            }
        }
    }

    /// Slot `m`'s one read takes the register: its store is dead.
    fn consume(&mut self, m: Mem) {
        if let Mem::Frame(s, o) = m {
            self.pending.retain(|p| p.slot != s || p.off != o);
        }
    }

    /// The first register of `pool` not in `avoid`, preferring one that
    /// no store waits on and holds nothing.
    fn pick(&self, pool: &[u32], fp: bool, avoid: &[u32]) -> u32 {
        let ok = |r: &&u32| !avoid.contains(r);
        let free = |r: &&u32| ok(r) && !self.pending.iter().any(|p| p.reg == **r && p.fp == fp);
        pool.iter()
            .filter(free)
            .find(|r| !self.held.iter().any(|h| h.reg == **r && h.fp == fp))
            .or_else(|| pool.iter().find(free))
            .or_else(|| pool.iter().find(ok))
            .copied()
            .unwrap_or(pool[0])
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

    /// `op dst, src`, 64-bit (`w`) or 32-bit.
    fn alu(&mut self, op: Alu, w: bool, dst: u32, src: u32) {
        self.a.alu_rr(op, Sz::dq(w), Gpr(dst), Gpr(src));
    }

    /// The integer register `m` is allocated to, if any.
    fn dst_r(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| !a.fp).map(|a| a.reg)
    }

    /// The `xmm` `m` is allocated to, if any.
    fn dst_x(&self, m: Mem) -> Option<u32> {
        self.ra.get(m).filter(|a| a.fp).map(|a| a.reg)
    }

    /// `r` = the `k` at `rm`, zero-extended.
    fn sized_load(&mut self, k: K, r: u32, rm: Rm) {
        match k {
            K::I8 => self.a.movzx(Sz::D, Gpr(r), Sz::B, rm),
            K::I16 => self.a.movzx(Sz::D, Gpr(r), Sz::W, rm),
            K::I32 | K::F32 => self.a.mov(Sz::D, Gpr(r), rm),
            K::I64 | K::F64 => self.a.mov(Sz::Q, Gpr(r), rm),
        }
    }

    /// `xchg [r10], rax` (sequentially consistent store); the old value in rax.
    fn xchg(&mut self, k: K) {
        self.a.xchg(sz(k), Addr::Bd(Gpr(R10), 0), Gpr(RAX));
    }
}

/// The operand size of `k`.
const fn sz(k: K) -> Sz {
    match k {
        K::I8 => Sz::B,
        K::I16 => Sz::W,
        K::I32 | K::F32 => Sz::D,
        K::I64 | K::F64 => Sz::Q,
    }
}

/// The memory operand `Em::mem` made.
fn addr(rm: Rm) -> Addr {
    match rm {
        Rm::M(a) => a,
        Rm::R(_) => unreachable!("memory operand"),
    }
}

/// The group 1 operation of `op` (`Add`..`Xor`).
fn alu(op: Bin) -> Alu {
    match op {
        Bin::Add => Alu::Add,
        Bin::Or => Alu::Or,
        Bin::And => Alu::And,
        Bin::Sub => Alu::Sub,
        Bin::Xor => Alu::Xor,
        _ => unreachable!("{op:?} is no group 1 operation"),
    }
}

/// The shift of `op` (`Shl`, `Shr`, `Rotl`).
const fn shift(op: Bin, signed: bool) -> Shift {
    match (op, signed) {
        (Bin::Shl, _) => Shift::Shl,
        (Bin::Rotl, _) => Shift::Rol,
        (_, true) => Shift::Sar,
        _ => Shift::Shr,
    }
}

/// A register's DWARF and `CodeView` numbers, the `CodeView` one the part
/// of the register a value of `size` bytes is in.
fn debug_reg(r: crate::regalloc::R, size: u32) -> Option<(u16, u16)> {
    // Hardware order (`rax rcx rdx rbx rsp rbp rsi rdi`) to DWARF's.
    const DWARF: [u16; 8] = [0, 2, 1, 3, 7, 6, 4, 5];
    let n = r.reg as u16;
    if r.fp {
        return (n < 16).then_some((17 + n, 154 + n));
    }
    let dw = if n < 8 { DWARF[n as usize] } else { n };
    let cv = match (size, n) {
        (8, 0..8) => [328, 330, 331, 329, 335, 334, 332, 333][n as usize],
        (8, _) => 336 + n - 8,
        (4, 0..8) => 17 + n,
        (4, _) => 360 + n - 8,
        (2, 0..8) => 9 + n,
        (2, _) => 352 + n - 8,
        (_, 0..4) => 1 + n,
        (_, 4..8) => [327, 326, 324, 325][n as usize - 4],
        (_, _) => 344 + n - 8,
    };
    (n < 16).then_some((dw, cv))
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
            held: Vec::new(),
            escaped: Vec::new(),
            pending: Vec::new(),
            waits: Waits::default(),
            flags: None,
            tested: None,
            ra: Alloc::default(),
            saved: Vec::new(),
            saved_xmm: Vec::new(),
            saves: Saves::default(),
            opt: false,
            scaled: None,
            stored: Vec::new(),
            plan: None,
            sites: Vec::new(),
        }
    }

    /// Scratch registers are caller-saved in both conventions (`rbx`, `rsi`
    /// / `rdi` on Windows, `r12`..`r15`, `xmm6`.. are not): the unwind
    /// encodings describe only the saves the allocator asks for.
    #[test]
    fn values_stay_in_caller_saved_registers() {
        for r in POOL.iter().chain(&[R10, R11]) {
            assert!(![3, RSI, RDI, 12, 13, 14, 15].contains(r), "r{r}");
        }
        assert!(FP_POOL.iter().all(|x| *x < 6));
        let code = bytes(|e| {
            e.frame = 3 * PAGE + 32;
            e.prologue();
        });
        // push rbp; mov rbp, rsp
        assert_eq!(code[..4], [0x55, 0x48, 0x89, 0xe5]);
    }

    /// Windows with `rbx r12` and `xmm6` saved: their area first, `rbp` the
    /// frame register 48 bytes up, the codes last first as `dumpbin
    /// /unwindinfo` reads them.
    #[test]
    fn windows_prologue_unwind_info() {
        let mut obj = Obj::new(true, false, true);
        let mut e = em(&mut obj);
        e.saved = vec![3, 12];
        e.saved_xmm = vec![6];
        e.frame = 64;
        e.prologue();
        assert_eq!(
            e.a.code[..25],
            [
                0x55, // push rbp
                0x48, 0x83, 0xec, 0x20, // sub rsp, 32
                0x48, 0x8d, 0x6c, 0x24, 0x20, // lea rbp, [rsp + 32]
                0x48, 0x89, 0x5d, 0xf8, // mov [rbp - 8], rbx
                0x4c, 0x89, 0x65, 0xf0, // mov [rbp - 16], r12
                0x0f, 0x29, 0x75, 0xe0, // movaps [rbp - 32], xmm6
                0x48, 0x81, 0xec, // sub rsp, 32
            ]
        );
        assert_eq!(
            e.saves.win,
            [
                1, 22, 9, 0x25, // version, prolog, slots, rbp at 32
                22, 0x68, 0, 0, // SAVE_XMM128 xmm6 at 0
                18, 0xc4, 2, 0, // SAVE_NONVOL r12 at 16
                14, 0x34, 3, 0, // SAVE_NONVOL rbx at 24
                10, 0x03, // SET_FPREG
                5, 0x32, // ALLOC_SMALL 32
                1, 0x50, // PUSH_NONVOL rbp
                0, 0, // even count
            ]
        );
    }

    /// The bytes `f` writes, checked against the reference assembler's
    /// encodings (REX, `ModRM`, SIB, displacement and immediate forms).
    fn bytes(f: impl FnOnce(&mut Em<'_>)) -> Vec<u8> {
        let mut obj = Obj::new(true, true, false);
        let mut e = em(&mut obj);
        f(&mut e);
        e.a.code
    }

    #[test]
    fn mov_rr_skips_a_move_to_itself() {
        // mov rax, rcx / mov r9, r10
        assert_eq!(bytes(|e| e.mov_rr(RAX, RCX)), [0x48, 0x8b, 0xc1]);
        assert_eq!(bytes(|e| e.mov_rr(R9, R10)), [0x4d, 0x8b, 0xca]);
        assert_eq!(bytes(|e| e.mov_rr(RDX, RDX)), []);
    }

    #[test]
    fn mov_imm_picks_the_shortest_form() {
        // mov r32, imm32 (zero-extends)
        assert_eq!(bytes(|e| e.mov_imm(RAX, 1)), [0xb8, 1, 0, 0, 0]);
        assert_eq!(
            bytes(|e| e.mov_imm(R10, 0xffff_ffff)),
            [0x41, 0xba, 0xff, 0xff, 0xff, 0xff]
        );
        // mov r64, simm32
        assert_eq!(
            bytes(|e| e.mov_imm(RCX, u64::MAX)),
            [0x48, 0xc7, 0xc1, 0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(
            bytes(|e| e.mov_imm(R9, i64::from(i32::MIN) as u64)),
            [0x49, 0xc7, 0xc1, 0x00, 0x00, 0x00, 0x80]
        );
        // movabs
        assert_eq!(
            bytes(|e| e.mov_imm(RDX, 1 << 32)),
            [0x48, 0xba, 0, 0, 0, 0, 1, 0, 0, 0]
        );
        assert_eq!(
            bytes(|e| e.mov_imm(R11, 1 << 63)),
            [0x49, 0xbb, 0, 0, 0, 0, 0, 0, 0, 0x80]
        );
        // Just below `i32::MIN`: not a simm32.
        assert_eq!(
            bytes(|e| e.mov_imm(RAX, (i64::from(i32::MIN) - 1) as u64)),
            [0x48, 0xb8, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff, 0xff]
        );
    }

    #[test]
    fn forward_jumps_patch_their_rel32() {
        let code = bytes(|e| {
            let at = e.jcc_fwd(Cond::NE);
            e.mov_imm(RAX, 1);
            e.here(at);
        });
        assert_eq!(code, [0x0f, 0x85, 5, 0, 0, 0, 0xb8, 1, 0, 0, 0]);
        let code = bytes(|e| {
            let at = e.jmp_fwd();
            e.here(at);
        });
        assert_eq!(code, [0xe9, 0, 0, 0, 0]);
    }
}
