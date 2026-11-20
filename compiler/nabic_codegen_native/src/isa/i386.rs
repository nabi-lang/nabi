//! LIR -> i386 machine code (System V in ELF objects, MSVC in COFF ones).
//!
//! Frame: `ebp` pushed, `ebp` = it, `ebx esi edi` pushed, then the slots
//! and the outgoing arguments at `esp`, whose first 16 bytes are scratch
//! outside a call. Nothing moves `esp` in the body (a callee that pops is
//! undone at once), so `esp` + 16 is aligned where callers keep 16-byte
//! alignment. Each instruction loads its operands into fixed registers,
//! computes and stores: `eax edx` hold the first value (an 8-byte one as a
//! pair, low word first), `ecx esi` the second, `edi` addresses a store.
//! ELF code is position independent: `ebx` holds the GOT in a function
//! that reaches a symbol. 8-byte division and multiplication with an
//! overflow check call local helpers, 8-byte integer <-> float conversions
//! go through the x87 unit; other float arithmetic is SSE2.
//!
//! Above `-O0` the allocator (`regalloc`) puts 1..4-byte values in `ebx`
//! (where it holds the GOT, only between the instructions reading it,
//! which load it back from a copy at `[ebp - 16]`) and `esi edi`, all
//! saved by every prologue already, an often used 8-byte integer in two
//! of them (low word first), and floats not live across a call in `xmm3`..`xmm7`. No value
//! lives in `esi edi` across a sequence that takes them (8-byte
//! multiplication, division and popcount, atomics, a call storing three
//! results through pointers), nor in `ebx` across an atomic.
//! Scratch is then `eax ecx edx xmm0 xmm1 xmm2`: stores address through
//! `ecx edx eax`, a block copy keeps `esi edi` in the scratch area, and
//! add, subtract, logic and compares take a second operand behind a
//! pointer from memory. One in a frame slot is loaded into `ecx` first:
//! the slot was often stored just before, and a plain load of it is
//! renamed to the stored value where an ALU operand from memory waits
//! for store forwarding.

pub mod enc;

mod arith;
mod call;
mod frame;
mod mem;
mod select;
mod vector;

use crate::lir::{
    Bin, Callee, Cc, Conv, Func, Inst, K, Label, Loc, Mem, NO_SLOT, Order, Rmw, Un, V, scalar,
};
use crate::obj::{FnRange, Obj, RelKind, Reloc, Sect};
use crate::regalloc::{Alloc, Regs};
use enc::{Addr, Alu, Asm, Cond, Gpr, Rm, Seg, Shift, Sse, Sz, X87, Xmm, Xrm};

const EAX: u32 = 0;
const ECX: u32 = 1;
const EDX: u32 = 2;
const EBX: u32 = 3;
const ESP: u32 = 4;
const EBP: u32 = 5;
const ESI: u32 = 6;
const EDI: u32 = 7;
const XMM0: u32 = 0;
const XMM1: u32 = 1;
const XMM2: u32 = 2;
/// Integer result registers.
const RETS: [u32; 3] = [EAX, EDX, ECX];
/// Stack probe interval.
const PAGE: u32 = 4096;
/// Scratch bytes at `esp`.
const SCRATCH: u32 = 16;
/// Bytes pushed below the return address: `ebp ebx esi edi`.
const SAVED: u32 = 16;

/// The allocator's registers: `int` the callee-saved integer registers
/// (`ebx` but where it holds the GOT); no argument travels in a
/// register.
const fn regs(int: &'static [u32]) -> Regs {
    Regs {
        int_saved: int,
        int_free: &[],
        int_arg: &[],
        fp_saved: &[],
        fp_free: &[3, 4, 5, 6, 7],
        fp_arg: &[],
        int_args: &[],
        fp_args: &[],
        int_rets: &RETS,
        fp_rets: &[],
        sret: EAX,
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
    }
}

const REGS_ALL: Regs = regs(&[EBX, ESI, EDI]);
/// Where `ebx` holds the GOT: a value only between the instructions that
/// read it (`reads_got`), which then load it again (`Em::got_at`).
const REGS_GOT: Regs = regs(&[ESI, EDI, EBX]);

/// Whether `i` reads the GOT from `ebx`: a symbol's address, a thread
/// local, a call through the PLT (not of a Nabi function or a local
/// helper, which the link always holds).
fn reads_got(obj: &Obj, i: &Inst) -> bool {
    match i {
        Inst::Sym { .. } | Inst::Tls { .. } => true,
        Inst::Call {
            callee: Callee::Sym(s),
            ..
        } => {
            let n = &obj.syms[*s as usize].name;
            !(n.starts_with("_Y") || n.starts_with("nabi."))
        }
        _ => false,
    }
}

/// What `i`'s sequence takes of the allocator's registers.
fn clobber(i: &Inst) -> u64 {
    let atomic = matches!(
        i,
        Inst::AtomicLoad { .. }
            | Inst::AtomicStore { .. }
            | Inst::AtomicRmw { .. }
            | Inst::Cas { .. }
    );
    match (atomic, clobbers_sidi(i)) {
        (true, _) => (1 << EBX) | (1 << ESI) | (1 << EDI),
        (false, true) => (1 << ESI) | (1 << EDI),
        _ => 0,
    }
}

/// Whether `i`'s sequence takes `esi` / `edi` as scratch (or, an atomic,
/// `ebx` too: the 8-byte ones need it; a vector loop keeps `ebx`).
fn clobbers_sidi(i: &Inst) -> bool {
    match i {
        Inst::VLoop(_)
        | Inst::AtomicLoad { .. }
        | Inst::AtomicStore { .. }
        | Inst::AtomicRmw { .. }
        | Inst::Cas { .. } => true,
        Inst::Bin { op, k, .. } => {
            wide(*k) && !k.is_float() && matches!(op, Bin::Mul | Bin::Div | Bin::Rem)
        }
        Inst::Ovf { op, k, .. } => wide(*k) && !matches!(op, Bin::Add | Bin::Sub),
        Inst::Un { op, k, .. } => wide(*k) && *op == Un::Popcnt,
        Inst::Call { rets, .. } => {
            // Three result registers and a store through a pointer: no
            // scratch left to address it but `edi`.
            let n = rets
                .iter()
                .map(|(p, _)| match p.loc {
                    Loc::Int(r) => r as usize + if wide(p.k) { 2 } else { 1 },
                    _ => 0,
                })
                .max()
                .unwrap_or(0);
            n > 2 && rets.iter().any(|(_, m)| matches!(m, Mem::Ind(..)))
        }
        _ => false,
    }
}

/// The register file for a function (`got`: `ebx` holds the GOT).
const fn regs_of(got: bool) -> &'static Regs {
    if got { &REGS_GOT } else { &REGS_ALL }
}

/// Local helpers: unsigned 8-byte division and multiplication.
const UDIVMOD: &str = "nabi.i386.udivmod64";
const UMULO: &str = "nabi.i386.umulo64";

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

struct Em<'a> {
    obj: &'a mut Obj,
    start: u32,
    a: Asm,
    /// `(position of a rel32, or a rel8 if, label)`.
    fixes: Vec<(usize, Label, bool)>,
    labels: Vec<Option<usize>>,
    slot_off: Vec<u32>,
    /// Bytes below the saved registers.
    frame: u32,
    /// `ebx` holds the GOT.
    got: bool,
    /// Where values live in `ebx` too, the GOT kept at `[ebp - 16]`:
    /// whether `ebx` may hold a value instead.
    got_lost: Option<bool>,
    /// Bytes of arguments popped on return.
    pop: u32,
    lines: Vec<(u32, u32, u32, u32)>,
    /// Above `-O0`: where values live, and the sequences that change.
    ra: Alloc,
    opt: bool,
    /// Which label branches are rel8, in order (`relax::relax`).
    plan: Option<Vec<bool>>,
    /// Each label branch so far: `(start, length, label)`.
    sites: Vec<(usize, u8, Label)>,
    /// Above `-O0`: parts written once and read at most once
    /// (`lir::waitable`).
    waits: crate::lir::Waits,
    /// The flags of a comparison the next branch takes instead of its
    /// result, which then never goes out (`skipped`).
    flags: Option<Cond>,
    skipped: crate::lir::Waits,
}

/// `[base + disp]`.
const fn bd(base: u32, disp: i32) -> Addr {
    Addr::Bd(Gpr(base), disp)
}

/// Register `n` as an operand.
const fn gr(n: u32) -> Rm {
    Rm::R(Gpr(n))
}

/// `xmm` register `n` as an operand.
const fn xr(n: u32) -> Xrm {
    Xrm::X(Xmm(n))
}

/// Whether `f` reaches a symbol (the GOT in ELF).
fn reaches_sym(f: &Func) -> bool {
    f.insts.iter().any(|i| match i {
        Inst::Call { callee, .. } => matches!(callee, Callee::Sym(_)),
        Inst::Sym { .. } | Inst::Tls { .. } => true,
        _ => false,
    })
}

/// Emit `f` into `obj`'s text.
pub fn emit(obj: &mut Obj, f: &Func) {
    let pic = reaches_sym(f) && !obj.coff;
    let regs = regs_of(pic);
    let (owned, ra) = if f.opt {
        let got = |i: &Inst| if reads_got(obj, i) { 1 << EBX } else { 0 };
        let none = |_: &Inst| 0;
        let kill: &dyn Fn(&Inst) -> u64 = if pic { &got } else { &none };
        let (g, a) = crate::regalloc::prepare_with(f, regs, kill);
        (Some(g), a)
    } else {
        (None, Alloc::default())
    };
    let f = owned.as_ref().unwrap_or(f);
    let mut out = SCRATCH;
    let mut got = false;
    for i in &f.insts {
        match i {
            Inst::Call {
                callee, args, sret, ..
            } => {
                if sret.is_some() {
                    out = out.max(4);
                }
                for (p, _) in args {
                    if let Loc::Stack(o) = p.loc {
                        out = out.max(o + p.k.size().max(4));
                    }
                }
                got |= matches!(callee, Callee::Sym(_));
            }
            Inst::Sym { .. } | Inst::Tls { .. } => got = true,
            _ => {}
        }
    }
    // The GOT's copy at `[ebp - 16]`, below the saves, where values take
    // `ebx` too.
    let got_kept = got && !obj.coff && ra.saved.contains(&EBX);
    let crate::frame::Layout {
        off: slot_off,
        end: at,
        live,
    } = {
        let gone = |s| ra.gone(s);
        let base = (out.next_multiple_of(16), 16, 0);
        if f.opt {
            // Within reach of a disp8 from `esp` above the outgoing
            // arguments and from `ebp` below the saves.
            let low = 128u32.saturating_sub(base.0);
            let high = 128 - SAVED - 12 - if got_kept { 16 } else { 0 };
            let order = crate::frame::hot(f, 4, low, high);
            crate::frame::layout_in(f, &gone, base, 4, &order)
        } else {
            crate::frame::layout(f, &gone, base, 4)
        }
    };
    // ELF: a descriptor's address needs the GOT in a register of its own
    // (`desc_sym`), worth it from four entries.
    let min = if obj.coff { 2 } else { 4 };
    let runs = crate::roots::frame_from(obj, f, &slot_off, 4, &|s| ra.gone(s), min);
    // The return address and `SAVED` bytes above: 16-aligned `esp`.
    let frame = at.next_multiple_of(16) + 12 + if got_kept { 16 } else { 0 };
    let start = obj.sect(Sect::Text).align_to(16);
    let text = obj.asm.is_some();
    // Label branches as `x64::emit` lays them out.
    let mut plan: Option<Vec<bool>> = None;
    let mut tried = false;
    let waits = if f.opt {
        crate::lir::waitable(f).0
    } else {
        crate::lir::Waits::default()
    };
    loop {
        let mut e = Em {
            got: got && !obj.coff,
            got_lost: got_kept.then_some(false),
            obj: &mut *obj,
            start,
            a: Asm::new(text),
            fixes: Vec::new(),
            labels: vec![None; f.labels as usize],
            slot_off: slot_off.clone(),
            frame,
            pop: f.pop,
            lines: Vec::new(),
            ra: ra.clone(),
            opt: f.opt,
            plan: plan.take(),
            sites: Vec::new(),
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
                    match *i {
                        Inst::Sym {
                            dst,
                            sym,
                            got: false,
                        } if !e.obj.coff => e.desc_sym(dst, sym),
                        _ => e.inst(i, None),
                    }
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
        // `esp` is `ebp - 12 - frame`.
        let below = (frame + SAVED - 4) as i32;
        let off = |s: u32| e.slot_off[s as usize] as i32 - below;
        let locs = track
            .map(|t| {
                t.finish(
                    &e.ra,
                    (&e.skipped, &[]),
                    (e.a.pos() as u32, 4),
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
            let labels = self
                .labels
                .iter()
                .enumerate()
                .filter_map(|(l, at)| at.map(|at| (at as u32, l as u32)))
                .collect();
            list(self.obj, sym, &self.a, labels);
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
            saves: crate::obj::Saves::default(),
        });
        for (name, code) in [(UDIVMOD, udivmod as fn(&mut Asm)), (UMULO, umulo)] {
            if self.obj.named(name) {
                let s = self.obj.sym(name);
                if self.obj.syms[s as usize].def.is_none() {
                    let mut a = Asm::new(self.obj.asm.is_some());
                    code(&mut a);
                    let text = self.obj.sect(Sect::Text);
                    let at = text.align_to(16);
                    text.data.extend_from_slice(&a.code);
                    let y = &mut self.obj.syms[s as usize];
                    y.def = Some((Sect::Text, at, a.code.len() as u32));
                    y.global = false;
                    if self.obj.asm.is_some() {
                        list(self.obj, s, &a, Vec::new());
                    }
                }
            }
        }
        true
    }

    /// `mov r, imm32`, zero by `xor`.
    fn mov_imm(&mut self, r: u32, v: u32) {
        if v == 0 {
            self.a.alu(Alu::Xor, Sz::D, gr(r), Gpr(r));
        } else {
            self.a.mov_ri(Gpr(r), v);
        }
    }

    /// `mov rd, rs`.
    fn mov(&mut self, rd: u32, rs: u32) {
        if rd != rs {
            self.a.mov(Sz::D, Gpr(rd), gr(rs));
        }
    }

    /// `op rd, rs` (the `r/m, r` form).
    fn alu(&mut self, op: Alu, rd: u32, rs: u32) {
        self.a.alu(op, Sz::D, gr(rd), Gpr(rs));
    }

    /// `op rm, imm`: above `-O0` as `alu_imm`, else an imm32.
    fn alu_opt(&mut self, op: Alu, rm: Rm, imm: u32) {
        if self.opt {
            self.alu_imm(op, rm, imm);
        } else {
            self.a.alu_ri(op, rm, imm);
        }
    }

    /// `op rm, imm`: an imm8 where it fits, else an imm32.
    fn alu_imm(&mut self, op: Alu, rm: Rm, imm: u32) {
        let i = imm as i32;
        if (-128..128).contains(&i) {
            self.a.alu_ri8(op, rm, i as i8);
        } else {
            self.a.alu_ri(op, rm, imm);
        }
    }
}

/// Every prologue: `push ebp; mov ebp, esp; push ebx; push esi; push
/// edi`, as the unwind tables say.
fn prologue_regs(a: &mut Asm) {
    a.push(Gpr(EBP));
    a.mov_st(Sz::D, gr(EBP), Gpr(ESP));
    for r in [EBX, ESI, EDI] {
        a.push(Gpr(r));
    }
}

/// The listing of `a`, the code of `sym`, into `obj.asm`.
fn list(obj: &mut Obj, sym: u32, a: &Asm, labels: Vec<(u32, u32)>) {
    let name = crate::asm::sym_name(obj, &obj.syms[sym as usize].name);
    let l = crate::asm::Listing {
        name: &name,
        code: &a.code,
        labels,
        relocs: &a.relocs,
    };
    let lines = a.lines(&l);
    crate::asm::function(obj, &l, &lines, crate::asm::Bytes::Each);
}

/// The operand size of a `k` access.
const fn size(k: K) -> Sz {
    match k {
        K::I8 => Sz::B,
        K::I16 => Sz::W,
        _ => Sz::D,
    }
}

/// `movss` / `movsd` loading a `k` float.
const fn movs(k: K) -> Sse {
    if matches!(k, K::F32) {
        Sse::Movss
    } else {
        Sse::Movsd
    }
}

/// `movss` / `movsd` storing a `k` float.
const fn movs_st(k: K) -> Sse {
    if matches!(k, K::F32) {
        Sse::MovssSt
    } else {
        Sse::MovsdSt
    }
}

/// The scalar SSE arithmetic of `op` (add, subtract, multiply, else
/// divide) on `k`.
const fn farith(op: Bin, k: K) -> Sse {
    let s = matches!(k, K::F32);
    match op {
        Bin::Add if s => Sse::Addss,
        Bin::Add => Sse::Addsd,
        Bin::Sub => fsub(k),
        Bin::Mul if s => Sse::Mulss,
        Bin::Mul => Sse::Mulsd,
        _ if s => Sse::Divss,
        _ => Sse::Divsd,
    }
}

const fn fsub(k: K) -> Sse {
    if matches!(k, K::F32) {
        Sse::Subss
    } else {
        Sse::Subsd
    }
}

const fn fsqrt(k: K) -> Sse {
    if matches!(k, K::F32) {
        Sse::Sqrtss
    } else {
        Sse::Sqrtsd
    }
}

/// `cvtss2sd` / `cvtsd2ss` from a `from` float.
const fn fcvt(from: K) -> Sse {
    if matches!(from, K::F32) {
        Sse::Cvtss2sd
    } else {
        Sse::Cvtsd2ss
    }
}

const fn ucomis(k: K) -> Sse {
    if matches!(k, K::F64) {
        Sse::Ucomisd
    } else {
        Sse::Ucomiss
    }
}

/// x87 `fld` / `fstp` of a `k` float.
const fn fld(k: K) -> X87 {
    if matches!(k, K::F32) {
        X87::Fld32
    } else {
        X87::Fld64
    }
}

const fn fstp(k: K) -> X87 {
    if matches!(k, K::F32) {
        X87::Fstp32
    } else {
        X87::Fstp64
    }
}

/// The group 1 operation of add, subtract, `and`, `or`, else `xor`.
const fn alu_of(op: Bin) -> Alu {
    match op {
        Bin::Add => Alu::Add,
        Bin::Or => Alu::Or,
        Bin::And => Alu::And,
        Bin::Sub => Alu::Sub,
        _ => Alu::Xor,
    }
}

/// `alu_of` on an 8-byte value's high word: the carry taken.
const fn alu_hi(op: Bin) -> Alu {
    match op {
        Bin::Add => Alu::Adc,
        Bin::Sub => Alu::Sbb,
        _ => alu_of(op),
    }
}

/// The shift of `Shl` / `Shr` (`signed`: arithmetic) / `Rotl`.
const fn shift_of(op: Bin, signed: bool) -> Shift {
    match (op, signed) {
        (Bin::Shl, _) => Shift::Shl,
        (Bin::Rotl, _) => Shift::Rol,
        (_, true) => Shift::Sar,
        _ => Shift::Shr,
    }
}

/// `nabi.i386.udivmod64`: `(eax, edx)` / `(ecx, esi)` (non-zero) to the
/// quotient in `(eax, edx)`, the remainder in `(ecx, esi)`; keeps `ebx`
/// and `edi`.
fn udivmod(a: &mut Asm) {
    let (eax, ecx, edx, ebx, esp, esi, edi) = (
        Gpr(EAX),
        Gpr(ECX),
        Gpr(EDX),
        Gpr(EBX),
        Gpr(ESP),
        Gpr(ESI),
        Gpr(EDI),
    );
    let mov = |a: &mut Asm, d: Gpr, s: Gpr| a.mov_st(Sz::D, Rm::R(d), s);
    a.push(ebx);
    a.push(edi);
    a.test(Sz::D, Rm::R(esi), esi);
    let slow = a.jcc8(Cond::NE, None);
    // A divisor below 2^32: two 32-bit divisions.
    mov(a, ebx, eax);
    mov(a, eax, edx);
    a.alu(Alu::Xor, Sz::D, Rm::R(edx), edx);
    a.un(enc::Un::Div, Rm::R(ecx));
    mov(a, edi, eax);
    mov(a, eax, ebx);
    a.un(enc::Un::Div, Rm::R(ecx));
    mov(a, ecx, edx);
    mov(a, edx, edi);
    a.pop(edi);
    a.pop(ebx);
    a.ret();
    // slow: (edi, ebx) the remainder; 64 rounds shift it and the
    // dividend left together, subtracting the divisor where it fits; the
    // quotient's bits fill the dividend from below.
    let at = a.pos();
    a.patch_rel8(slow, at);
    a.alu(Alu::Xor, Sz::D, Rm::R(ebx), ebx);
    a.alu(Alu::Xor, Sz::D, Rm::R(edi), edi);
    a.push_i8(64);
    let top = a.pos();
    a.shift_1(Shift::Shl, eax);
    for r in [edx, ebx, edi] {
        a.shift_1(Shift::Rcl, r);
    }
    let sub = a.jcc8(Cond::B, None);
    a.alu(Alu::Cmp, Sz::D, Rm::R(edi), esi);
    let sub2 = a.jcc8(Cond::A, None);
    let next = a.jcc8(Cond::B, None);
    a.alu(Alu::Cmp, Sz::D, Rm::R(ebx), ecx);
    let next2 = a.jcc8(Cond::B, None);
    let at = a.pos();
    a.patch_rel8(sub, at);
    a.patch_rel8(sub2, at);
    a.alu(Alu::Sub, Sz::D, Rm::R(ebx), ecx);
    a.alu(Alu::Sbb, Sz::D, Rm::R(edi), esi);
    a.alu_ri8(Alu::Or, Rm::R(eax), 1);
    let at = a.pos();
    a.patch_rel8(next, at);
    a.patch_rel8(next2, at);
    a.dec(Rm::M(Addr::Bd(esp, 0)));
    a.jcc8(Cond::NE, Some(top));
    a.alu_ri8(Alu::Add, Rm::R(esp), 4);
    mov(a, ecx, ebx);
    mov(a, esi, edi);
    a.pop(edi);
    a.pop(ebx);
    a.ret();
}

/// `nabi.i386.umulo64`: `(eax, edx)` * `(ecx, esi)` to the low 8 bytes in
/// `(eax, edx)`, `edi` = 1 if the product overflows them, else 0;
/// clobbers `ecx` and `esi`, keeps `ebx`.
fn umulo(a: &mut Asm) {
    let (eax, ecx, edx, ebx, esp, esi, edi) = (
        Gpr(EAX),
        Gpr(ECX),
        Gpr(EDX),
        Gpr(EBX),
        Gpr(ESP),
        Gpr(ESI),
        Gpr(EDI),
    );
    let mov = |a: &mut Asm, d: Gpr, s: Gpr| a.mov_st(Sz::D, Rm::R(d), s);
    // `edi` = 1 unless `cc` (a short jump over the `mov`).
    let flag_unless = |a: &mut Asm, cc: Cond| {
        let skip = a.jcc8(cc, None);
        a.mov_ri(edi, 1);
        let at = a.pos();
        a.patch_rel8(skip, at);
    };
    a.push(ebx);
    a.alu(Alu::Xor, Sz::D, Rm::R(edi), edi);
    // Both high words: overflow.
    a.test(Sz::D, Rm::R(edx), edx);
    let one = a.jcc8(Cond::E, None);
    a.test(Sz::D, Rm::R(esi), esi);
    flag_unless(a, Cond::E);
    let at = a.pos();
    a.patch_rel8(one, at);
    // The cross products, each within 32 bits, their sum too.
    mov(a, ebx, eax);
    mov(a, eax, edx);
    a.un(enc::Un::Mul, Rm::R(ecx));
    a.test(Sz::D, Rm::R(edx), edx);
    flag_unless(a, Cond::E);
    a.push(eax);
    mov(a, eax, ebx);
    a.un(enc::Un::Mul, Rm::R(esi));
    a.test(Sz::D, Rm::R(edx), edx);
    flag_unless(a, Cond::E);
    a.alu(Alu::Add, Sz::D, Rm::M(Addr::Bd(esp, 0)), eax);
    flag_unless(a, Cond::AE);
    // The low product, the cross sum into its high word.
    mov(a, eax, ebx);
    a.un(enc::Un::Mul, Rm::R(ecx));
    a.pop(esi);
    a.alu(Alu::Add, Sz::D, Rm::R(edx), esi);
    flag_unless(a, Cond::AE);
    a.pop(ebx);
    a.ret();
}

/// A register's DWARF and `CodeView` numbers, the `CodeView` one the part
/// of the register a value of `size` bytes is in.
fn debug_reg(r: crate::regalloc::R, size: u32) -> Option<(u16, u16)> {
    let n = r.reg as u16;
    if r.fp {
        return (n < 8).then_some((21 + n, 154 + n));
    }
    let cv = match size {
        1 if n < 4 => 1 + n,
        2 => 9 + n,
        _ => 17 + n,
    };
    (n < 8).then_some((n, cv))
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
            plan: None,
            sites: Vec::new(),
            labels: Vec::new(),
            slot_off: Vec::new(),
            frame: 0,
            got: false,
            got_lost: None,
            pop: 0,
            lines: Vec::new(),
            ra: Alloc::default(),
            opt: false,
            waits: crate::lir::Waits::default(),
            flags: None,
            skipped: crate::lir::Waits::default(),
        }
    }

    fn bytes(f: impl FnOnce(&mut Em<'_>)) -> Vec<u8> {
        let mut obj = Obj::new(false, true, false);
        let mut e = em(&mut obj);
        f(&mut e);
        e.a.code
    }

    /// The emitter's own choices: an imm8 where it fits, `xor` for a zero.
    #[test]
    fn short_forms() {
        assert_eq!(bytes(|e| e.mov(EAX, ECX)), [0x8b, 0xc1]);
        assert!(bytes(|e| e.mov(EAX, EAX)).is_empty());
        assert_eq!(bytes(|e| e.mov_imm(ECX, 0)), [0x31, 0xc9]);
        assert_eq!(
            bytes(|e| e.alu_imm(Alu::Sub, gr(ESP), 20)),
            [0x83, 0xec, 20]
        );
        assert_eq!(
            bytes(|e| e.alu_imm(Alu::Sub, gr(ESP), 300)),
            [0x81, 0xec, 0x2c, 0x01, 0, 0]
        );
    }

    /// What the unwind tables describe.
    const PROLOGUE: [u8; 6] = [0x55, 0x89, 0xe5, 0x53, 0x56, 0x57];

    /// The unwind tables describe `PROLOGUE`, which saves every register
    /// a value may take but the caller-saved ones.
    #[test]
    fn prologue_saves_the_callee_saved_registers() {
        let code = bytes(|e| {
            e.frame = 3 * PAGE + 12;
            e.prologue();
        });
        assert_eq!(code[..6], PROLOGUE);
        // Three probed pages, then the rest.
        assert_eq!(code.len(), 6 + 3 * 10 + 6);
    }

    /// The helpers as first assembled by hand.
    const UDIVMOD_BYTES: [u8; 75] = [
        0x53, 0x57, 0x85, 0xf6, 0x75, 0x15, 0x89, 0xc3, 0x89, 0xd0, 0x31, 0xd2, 0xf7, 0xf1, 0x89,
        0xc7, 0x89, 0xd8, 0xf7, 0xf1, 0x89, 0xd1, 0x89, 0xfa, 0x5f, 0x5b, 0xc3, 0x31, 0xdb, 0x31,
        0xff, 0x6a, 0x40, 0xd1, 0xe0, 0xd1, 0xd2, 0xd1, 0xd3, 0xd1, 0xd7, 0x72, 0x0a, 0x39, 0xf7,
        0x77, 0x06, 0x72, 0x0b, 0x39, 0xcb, 0x72, 0x07, 0x29, 0xcb, 0x19, 0xf7, 0x83, 0xc8, 0x01,
        0xff, 0x0c, 0x24, 0x75, 0xe0, 0x83, 0xc4, 0x04, 0x89, 0xd9, 0x89, 0xfe, 0x5f, 0x5b, 0xc3,
    ];
    const UMULO_BYTES: [u8; 71] = [
        0x53, 0x31, 0xff, 0x85, 0xd2, 0x74, 0x09, 0x85, 0xf6, 0x74, 0x05, 0xbf, 0x01, 0x00, 0x00,
        0x00, 0x89, 0xc3, 0x89, 0xd0, 0xf7, 0xe1, 0x85, 0xd2, 0x74, 0x05, 0xbf, 0x01, 0x00, 0x00,
        0x00, 0x50, 0x89, 0xd8, 0xf7, 0xe6, 0x85, 0xd2, 0x74, 0x05, 0xbf, 0x01, 0x00, 0x00, 0x00,
        0x01, 0x04, 0x24, 0x73, 0x05, 0xbf, 0x01, 0x00, 0x00, 0x00, 0x89, 0xd8, 0xf7, 0xe1, 0x5e,
        0x01, 0xf2, 0x73, 0x05, 0xbf, 0x01, 0x00, 0x00, 0x00, 0x5b, 0xc3,
    ];

    /// The helpers' bytes, their jumps landing on instruction boundaries.
    #[test]
    fn helpers() {
        let mut a = Asm::new(false);
        udivmod(&mut a);
        assert_eq!(a.code, UDIVMOD_BYTES);
        let mut a = Asm::new(false);
        umulo(&mut a);
        assert_eq!(a.code, UMULO_BYTES);
    }
}
