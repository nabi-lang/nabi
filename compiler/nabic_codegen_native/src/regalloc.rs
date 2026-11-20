//! Register allocation above `-O0`, shared by every ISA.
//!
//! A slot part is a candidate when it is accessed only as itself: one
//! size, never addressed, never copied whole, not described to the
//! debugger. GC roots are always addressed (the shadow-stack frame holds
//! their address), so no root ever lives in a register: what a collection
//! reads and rewrites stays in memory. Candidates get liveness over the
//! LIR's control flow, one interval each (first to last point live), and
//! a linear scan over the ISA's register file: an interval live across a
//! call takes a callee-saved register, any other prefers a caller-saved
//! one the emitter never touches. Uses weigh by how often their block
//! runs (`Cfg::freqs`: static branch probabilities, loops). When
//! registers run out, the candidate of least weighted use stays in its
//! slot, or is split (`split_ranges`): a temporary carries it around the
//! calls it lives across or through a loop that hardly uses it, and the
//! value itself takes a register where it is used. The emitter reads and
//! writes an allocated part in its register instead of the frame. On a
//! 32-bit target an 8-byte integer part may take two registers, low word
//! first, when it is used often enough to pay for both ([`PAIR_MIN`]).

mod anchor;
mod assign;
mod cfg;
mod live;
mod rewrite;
mod spill;

pub(crate) use crate::regalloc::anchor::*;
pub(crate) use crate::regalloc::assign::*;
pub(crate) use crate::regalloc::cfg::*;
pub(crate) use crate::regalloc::live::*;
pub(crate) use crate::regalloc::rewrite::*;
pub(crate) use crate::regalloc::spill::*;

use crate::lir::{Bin, Callee, Func, Inst, K, Label, Loc, Mem, Slot, V, Vx, VxOp, scalar};
use nabic_core::HashMap;

/// An ISA's registers for values (the emitter's own numbering).
#[allow(clippy::struct_excessive_bools)]
pub struct Regs {
    /// Integer registers calls preserve (saved in the prologue when used).
    pub int_saved: &'static [u32],
    /// Integer registers calls clobber that no instruction sequence of the
    /// emitter, argument or result uses.
    pub int_free: &'static [u32],
    /// Caller-saved registers calls, returns and parameters move values
    /// through: a value may sit in one where no such move writes it before
    /// reading the value.
    pub int_arg: &'static [u32],
    pub fp_saved: &'static [u32],
    pub fp_free: &'static [u32],
    pub fp_arg: &'static [u32],
    /// The registers of `Loc::Int(n)` / `Loc::Fp(n)` for arguments and
    /// for results, and of `Loc::Sret`.
    pub int_args: &'static [u32],
    pub fp_args: &'static [u32],
    pub int_rets: &'static [u32],
    pub fp_rets: &'static [u32],
    pub sret: u32,
    /// The widest integer part a register holds.
    pub word: u32,
    /// The size of a pointer.
    pub ptr: u32,
    /// Floats of this size and below go in float registers (0: none).
    pub fp_size: u32,
    /// Vectors (`Inst::V`) of this size and below go in float registers,
    /// which the emitter's vector code reads and writes (0: none).
    pub vec: u32,
    /// Calls preserve 16 bytes of each of `fp_saved`, not only a float's.
    pub vec_saved: bool,
    /// Whether an integer immediate costs instructions of its own as the
    /// second operand of this `Bin` (`None`: a `Cmp`): loops then read it
    /// from a register set on entry.
    pub costly_imm: Option<fn(Option<Bin>, i64) -> bool>,
    /// An integer part of two words may live in two registers (a 32-bit
    /// target's 8-byte integers).
    pub pairs: bool,
    /// The prologue saves callee-saved registers two at a time, entries
    /// `2k` and `2k + 1` of `int_saved` / `fp_saved` together: the second
    /// of a pair costs nothing more.
    pub saved_pairs: bool,
    /// Every prologue saves all of `int_saved`: using one costs nothing.
    pub saves_all: bool,
    /// The integer registers (bit `n`: register `n`) an instruction's
    /// sequence takes as scratch: no value lives in one where it runs.
    pub clobber: Option<fn(&Inst) -> u64>,
}

/// Where an allocated part lives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct R {
    pub reg: u32,
    pub fp: bool,
}

#[derive(Clone, Default)]
pub struct Alloc {
    map: HashMap<(Slot, u32), R>,
    /// Two-word parts: the low word's register, the high word's.
    pairs: HashMap<(Slot, u32), (u32, u32)>,
    /// Slots no instruction reaches in memory: every part is in a register.
    gone: Vec<bool>,
    /// Callee-saved registers used, in the order of the register file.
    pub saved: Vec<u32>,
    pub saved_fp: Vec<u32>,
    /// With `Func::track`: each allocated part's size and the
    /// instructions its register holds it over.
    pub live: HashMap<(Slot, u32), (u32, Ranges)>,
    /// With `Func::track`: parts whose value a temporary carries for a
    /// while (`split_cold`, `split_ranges`), the temporary, and the
    /// instructions it holds the value over.
    pub copies: Vec<(Carry, Ranges)>,
}

impl Alloc {
    /// The register holding `m`, if it is an allocated part.
    #[must_use]
    pub fn get(&self, m: Mem) -> Option<R> {
        match m {
            Mem::Frame(s, o) => self.map.get(&(s, o)).copied(),
            Mem::Ind(..) => None,
        }
    }

    /// The register holding operand `v`, if it is an allocated part.
    #[must_use]
    pub fn get_v(&self, v: V) -> Option<R> {
        match v {
            V::M(m) => self.get(m),
            V::I(_) => None,
        }
    }

    /// The registers holding the two-word part `m`, low word first.
    #[must_use]
    pub fn pair(&self, m: Mem) -> Option<(u32, u32)> {
        match m {
            Mem::Frame(s, o) => self.pairs.get(&(s, o)).copied(),
            Mem::Ind(..) => None,
        }
    }

    /// The registers holding operand `v`, a two-word part.
    #[must_use]
    pub fn pair_v(&self, v: V) -> Option<(u32, u32)> {
        match v {
            V::M(m) => self.pair(m),
            V::I(_) => None,
        }
    }

    /// Whether `m` is a part in registers, one or two.
    #[must_use]
    pub fn held(&self, m: Mem) -> bool {
        self.get(m).is_some() || self.pair(m).is_some()
    }

    /// The register holding the pointer `m` goes through.
    #[must_use]
    pub fn base(&self, m: Mem) -> Option<u32> {
        match m {
            Mem::Ind(s, _) => self.map.get(&(s, 0)).filter(|r| !r.fp).map(|r| r.reg),
            Mem::Frame(..) => None,
        }
    }

    /// `slot` needs no frame space.
    #[must_use]
    pub fn gone(&self, slot: Slot) -> bool {
        self.gone.get(slot as usize).copied().unwrap_or(false)
    }
}

/// What an access says of the part's register class.
#[derive(Clone, Copy)]
pub(crate) enum Class {
    Int,
    Fp,
    /// A whole vector (`Inst::V`).
    Vec,
    /// Untyped bytes (a scalar copy).
    Any,
}

const fn class(k: K) -> Class {
    if k.is_float() { Class::Fp } else { Class::Int }
}

/// How an instruction accesses a memory operand.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) enum Acc {
    Read,
    Write,
    /// As raw memory (addressed, or copied whole).
    Whole,
}

/// Every memory operand of `i`, with its size, class and access.
fn operands(i: &Inst, ptr: u32, out: &mut Vec<(Mem, u32, Class, Acc)>) {
    let v = |out: &mut Vec<_>, x: &V, k: K| {
        if let V::M(m) = x {
            out.push((*m, k.size(), class(k), Acc::Read));
        }
    };
    let w = |out: &mut Vec<_>, m: &Mem, size: u32, c: Class| out.push((*m, size, c, Acc::Write));
    let whole = |out: &mut Vec<_>, m: &Mem| out.push((*m, 0, Class::Any, Acc::Whole));
    match i {
        Inst::Label(_) | Inst::Jmp(_) | Inst::Trap | Inst::Line(..) => {}
        Inst::VLoop(l) => whole(out, &Mem::Frame(l.io, 0)),
        Inst::V(x) => {
            for m in x.srcs() {
                out.push((m, x.vb, Class::Vec, Acc::Read));
            }
            if let Some((m, k)) = x.scalar() {
                v(out, &V::M(m), k);
            }
            match x.scalar_dst() {
                Some(k) => w(out, &x.dst, k.size(), class(k)),
                None => w(out, &x.dst, x.vb, Class::Vec),
            }
        }
        Inst::Br { c, .. } => v(out, c, K::I8),
        Inst::Mov { k, dst, src } => {
            v(out, src, *k);
            w(out, dst, k.size(), class(*k));
        }
        Inst::Copy { dst, src, size } => {
            if scalar(*size).is_some() {
                out.push((*src, *size, Class::Any, Acc::Read));
                w(out, dst, *size, Class::Any);
            } else {
                whole(out, src);
                whole(out, dst);
            }
        }
        Inst::Zero { dst, .. } => whole(out, dst),
        Inst::Addr { dst, src } => {
            whole(out, src);
            w(out, dst, ptr, Class::Int);
        }
        Inst::Sym { dst, .. } | Inst::Tls { dst, .. } => w(out, dst, ptr, Class::Int),
        Inst::Param { part, dst } if part.vector() => w(out, dst, 16, Class::Vec),
        Inst::Param { part, dst } => w(out, dst, part.k.size(), class(part.k)),
        Inst::Bin { k, dst, a, b, .. } | Inst::Ovf { k, dst, a, b, .. } => {
            v(out, a, *k);
            v(out, b, *k);
            w(out, dst, k.size(), class(*k));
        }
        Inst::Cmp { k, dst, a, b, .. } => {
            v(out, a, *k);
            v(out, b, *k);
            w(out, dst, 1, Class::Int);
        }
        Inst::Un { k, dst, a, .. } => {
            v(out, a, *k);
            w(out, dst, k.size(), class(*k));
        }
        Inst::Conv {
            from, to, dst, a, ..
        } => {
            v(out, a, *from);
            w(out, dst, to.size(), class(*to));
        }
        Inst::Fma { k, dst, a, b, c } => {
            v(out, a, *k);
            v(out, b, *k);
            v(out, c, *k);
            w(out, dst, k.size(), class(*k));
        }
        Inst::AtomicLoad { k, dst, ptr, .. } => {
            whole(out, ptr);
            w(out, dst, k.size(), class(*k));
        }
        Inst::AtomicStore { k, ptr, src, .. } => {
            whole(out, ptr);
            v(out, src, *k);
        }
        Inst::AtomicRmw {
            k, dst, ptr, val, ..
        } => {
            whole(out, ptr);
            v(out, val, *k);
            w(out, dst, k.size(), class(*k));
        }
        Inst::Cas {
            k,
            dst,
            ptr,
            expected,
            new,
            ..
        } => {
            whole(out, ptr);
            v(out, expected, *k);
            v(out, new, *k);
            w(out, dst, k.size(), class(*k));
        }
        Inst::Call {
            callee,
            args,
            rets,
            sret,
            ..
        } => {
            if let Callee::Ptr(m) = callee {
                out.push((*m, ptr, Class::Int, Acc::Read));
            }
            for (p, x) in args {
                match x {
                    V::M(m) if p.vector() => out.push((*m, 16, Class::Vec, Acc::Read)),
                    x => v(out, x, p.k),
                }
            }
            for (p, m) in rets {
                if p.vector() {
                    w(out, m, 16, Class::Vec);
                } else {
                    w(out, m, p.k.size(), class(p.k));
                }
            }
            if let Some(m) = sret {
                whole(out, m);
            }
        }
        Inst::Ret { vals } => {
            for (p, x) in vals {
                match x {
                    V::M(m) if p.vector() => out.push((*m, 16, Class::Vec, Acc::Read)),
                    x => v(out, x, p.k),
                }
            }
        }
    }
}

/// Whether `i` clobbers the registers calls clobber.
const fn clobbers(i: &Inst) -> bool {
    matches!(i, Inst::Call { .. } | Inst::Tls { .. } | Inst::VLoop(_))
}

/// The register `loc` names (`ret`: a result's), with its class.
fn loc_reg(regs: &Regs, loc: Loc, ret: bool) -> Option<(u32, bool)> {
    let (ints, fps) = if ret {
        (regs.int_rets, regs.fp_rets)
    } else {
        (regs.int_args, regs.fp_args)
    };
    match loc {
        Loc::Int(n) => ints.get(n as usize).map(|r| (*r, false)),
        Loc::Fp(n) | Loc::Vec(n) => fps.get(n as usize).map(|r| (*r, true)),
        Loc::Sret => Some((regs.sret, false)),
        Loc::Stack(_) | Loc::VecStack(_) | Loc::Area(_) => None,
    }
}

/// For each candidate, the argument / result registers (by class) some
/// move writes while the candidate still has to be read there, or that
/// the candidate, written by such a move, would clobber before it is
/// read: argument registers load in order, results store in order,
/// parameters store in order, a code pointer loads after the arguments.
fn forbidden(
    f: &Func,
    regs: &Regs,
    vreg: &dyn Fn(Mem) -> Option<usize>,
    nv: usize,
) -> Vec<[u64; 2]> {
    let mut out = vec![[0u64; 2]; nv];
    let bit = |r: (u32, bool)| (usize::from(r.1), 1u64 << r.0);
    let add = |out: &mut Vec<[u64; 2]>, v: Option<usize>, rs: &[(u32, bool)]| {
        if let Some(v) = v {
            for &r in rs {
                let (c, b) = bit(r);
                out[v][c] |= b;
            }
        }
    };
    // A value read through a pointer: the pointer's register.
    let of = |x: &V| match x {
        V::M(Mem::Ind(s, _)) => vreg(Mem::Frame(*s, 0)),
        V::M(m) => vreg(*m),
        V::I(_) => None,
    };
    let params: Vec<(Option<(u32, bool)>, Mem)> = f
        .insts
        .iter()
        .filter_map(|i| match i {
            Inst::Param { part, dst } => Some((loc_reg(regs, part.loc, false), *dst)),
            _ => None,
        })
        .collect();
    for (n, (_, dst)) in params.iter().enumerate() {
        let later: Vec<_> = params[n + 1..].iter().filter_map(|p| p.0).collect();
        add(&mut out, vreg(*dst), &later);
    }
    for i in &f.insts {
        match i {
            Inst::Call {
                callee, args, rets, ..
            } => {
                let mut written = Vec::new();
                for (p, x) in args {
                    add(&mut out, of(x), &written);
                    written.extend(loc_reg(regs, p.loc, false));
                }
                written.push((regs.sret, false));
                if let Callee::Ptr(m) = callee {
                    add(&mut out, of(&V::M(*m)), &written);
                }
                let srcs: Vec<_> = rets
                    .iter()
                    .map(|(p, _)| loc_reg(regs, p.loc, true))
                    .collect();
                for (n, (_, m)) in rets.iter().enumerate() {
                    let later: Vec<_> = srcs[n + 1..].iter().flatten().copied().collect();
                    add(&mut out, vreg(*m), &later);
                }
            }
            Inst::Ret { vals } => {
                let mut written = Vec::new();
                for (p, x) in vals {
                    add(&mut out, of(x), &written);
                    written.extend(loc_reg(regs, p.loc, true));
                }
            }
            _ => {}
        }
    }
    out
}

/// By label: the target of a jump from below it (a loop head), unless
/// it starts a cold block.
#[must_use]
pub fn loop_heads(f: &Func) -> Vec<bool> {
    let mut at = vec![usize::MAX; f.labels as usize];
    let mut heads = vec![false; f.labels as usize];
    for (n, i) in f.insts.iter().enumerate() {
        if let Inst::Label(l) = i {
            at[*l as usize] = n;
        }
        let (ls, k, _) = succs(i);
        for l in &ls[..k] {
            if at[*l as usize] < n {
                heads[*l as usize] = true;
            }
        }
    }
    for l in &f.cold {
        heads[*l as usize] = false;
    }
    heads
}

/// The labels `i` may jump to (the first `n`), and whether it falls
/// through.
pub const fn succs(i: &Inst) -> ([Label; 2], usize, bool) {
    match i {
        Inst::Jmp(l) => ([*l, 0], 1, false),
        Inst::Br { t, f, .. } => ([*t, *f], 2, false),
        Inst::Ovf { fail, .. } => ([*fail, 0], 1, true),
        Inst::Ret { .. } | Inst::Trap => ([0; 2], 0, false),
        _ => ([0; 2], 0, true),
    }
}

/// Apply `g` to every memory operand of `i` with its access (a pointer
/// read through `Ind` is the operand's own).
fn map_mems(i: &mut Inst, g: &mut dyn FnMut(&mut Mem, Acc)) {
    let v = |x: &mut V, g: &mut dyn FnMut(&mut Mem, Acc)| {
        if let V::M(m) = x {
            g(m, Acc::Read);
        }
    };
    match i {
        Inst::Label(_) | Inst::Jmp(_) | Inst::Trap | Inst::Line(..) => {}
        Inst::VLoop(l) => {
            let mut m = Mem::Frame(l.io, 0);
            g(&mut m, Acc::Whole);
            if let Mem::Frame(s, _) = m {
                l.io = s;
            }
        }
        Inst::V(x) => {
            let (a, b) = x.reads();
            if a {
                g(&mut x.a, Acc::Read);
            }
            if b {
                g(&mut x.b, Acc::Read);
            }
            g(&mut x.dst, Acc::Write);
        }
        Inst::Br { c, .. } => v(c, g),
        Inst::Mov { dst, src, .. } => {
            v(src, g);
            g(dst, Acc::Write);
        }
        Inst::Copy { dst, src, size } => {
            let a = if scalar(*size).is_some() {
                (Acc::Read, Acc::Write)
            } else {
                (Acc::Whole, Acc::Whole)
            };
            g(src, a.0);
            g(dst, a.1);
        }
        Inst::Zero { dst, .. } => g(dst, Acc::Whole),
        Inst::Addr { dst, src } => {
            g(src, Acc::Whole);
            g(dst, Acc::Write);
        }
        Inst::Sym { dst, .. } | Inst::Tls { dst, .. } | Inst::Param { dst, .. } => {
            g(dst, Acc::Write);
        }
        Inst::Bin { dst, a, b, .. } | Inst::Ovf { dst, a, b, .. } | Inst::Cmp { dst, a, b, .. } => {
            v(a, g);
            v(b, g);
            g(dst, Acc::Write);
        }
        Inst::Un { dst, a, .. } | Inst::Conv { dst, a, .. } => {
            v(a, g);
            g(dst, Acc::Write);
        }
        Inst::Fma { dst, a, b, c, .. } => {
            v(a, g);
            v(b, g);
            v(c, g);
            g(dst, Acc::Write);
        }
        Inst::AtomicLoad { dst, ptr, .. } => {
            g(ptr, Acc::Whole);
            g(dst, Acc::Write);
        }
        Inst::AtomicStore { ptr, src, .. } => {
            g(ptr, Acc::Whole);
            v(src, g);
        }
        Inst::AtomicRmw { dst, ptr, val, .. } => {
            g(ptr, Acc::Whole);
            v(val, g);
            g(dst, Acc::Write);
        }
        Inst::Cas {
            dst,
            ptr,
            expected,
            new,
            ..
        } => {
            g(ptr, Acc::Whole);
            v(expected, g);
            v(new, g);
            g(dst, Acc::Write);
        }
        Inst::Call {
            callee,
            args,
            rets,
            sret,
            ..
        } => {
            if let Callee::Ptr(m) = callee {
                g(m, Acc::Read);
            }
            for (_, x) in args {
                v(x, g);
            }
            for (_, m) in rets {
                g(m, Acc::Write);
            }
            if let Some(m) = sret {
                g(m, Acc::Whole);
            }
        }
        Inst::Ret { vals } => {
            for (_, x) in vals {
                v(x, g);
            }
        }
    }
}

/// `f` as the emitter sees it above `-O0`: loop constants hoisted, roots
/// cached in temporaries where every temporary gets a register, dead
/// computations dropped.
///
/// Positions go first: none may sit between the instructions the passes
/// and the emitter pair up, and `-g` must not change the code. With
/// `Func::track` they come back as `rows`, each before the instruction
/// that followed it (`anchor`).
#[must_use]
pub fn prepare(f: &Func, regs: &Regs) -> (Func, Alloc) {
    prepare_with(f, regs, &|_| 0)
}

/// [`prepare`], `kill` naming more registers instructions take
/// (`Regs::clobber`).
#[must_use]
pub fn prepare_with(f: &Func, regs: &Regs, kill: &dyn Fn(&Inst) -> u64) -> (Func, Alloc) {
    let lineless = f
        .insts
        .iter()
        .any(|i| matches!(i, Inst::Line(..)))
        .then(|| {
            f.with_insts(
                f.insts
                    .iter()
                    .filter(|i| !matches!(i, Inst::Line(..)))
                    .cloned()
                    .collect(),
            )
        });
    let (mut g, a) = prepare_code(lineless.as_ref().unwrap_or(f), regs, kill);
    if f.track {
        g.rows = anchor(f, &g);
    }
    (g, a)
}

fn prepare_code(f: &Func, regs: &Regs, kill: &dyn Fn(&Inst) -> u64) -> (Func, Alloc) {
    let stored = crate::frame::dse(f, regs.ptr);
    let f = stored.as_ref().unwrap_or(f);
    let split = if has_file(regs, false) {
        split_words(f, regs)
    } else {
        None
    };
    let f = split.as_ref().unwrap_or(f);
    let forwarded = forward_moves(f);
    let f = forwarded.as_ref().unwrap_or(f);
    // Temporaries of a class with no register would stay in memory.
    let fq = Freq::of(f);
    let hoisted = hoist_consts(f, regs, &fq);
    let f = hoisted.as_ref().unwrap_or(f);
    let (mut cache, mut done) = if has_file(regs, false) {
        worth_caching(f, regs, &fq)
    } else {
        (Vec::new(), None)
    };
    let base = f.slots.len() as Slot;
    loop {
        // The liveness of `g` while nothing changed it.
        let (mut g, lv) = if let Some(done) = done.take() {
            done
        } else {
            let mut g = if cache.is_empty() {
                f.clone()
            } else {
                cache_roots(f, regs, &cache)
            };
            let mut same = None;
            if let Some(lv) = dce(&mut g, regs) {
                let a = own_conds(&mut g, &lv);
                let b = coalesce(&mut g, &lv);
                same = (!a && !b).then_some(lv);
            }
            (g, same)
        };
        let mut copies = Vec::new();
        let lv = split_cold(&mut g, regs, lv, &mut copies);
        let lv = lv.unwrap_or_else(|| live(&g, regs));
        let (mut a, sp) = allocate_with(&g, regs, &lv, &fq, &copies, kill);
        if sp.spill.iter().any(|&x| x) && split_ranges(&mut g, &lv, &sp, &mut copies) {
            a = allocate_with(&g, regs, &live(&g, regs), &fq, &copies, kill).0;
        }
        if cache.is_empty() {
            return (g, a);
        }
        // A cached root left in memory costs a store per write and a
        // reload per call: no cache for it.
        let mut miss = Vec::new();
        for (n, c) in cache.iter().enumerate() {
            let t = base + n as Slot;
            if a.get(Mem::Frame(t, 0)).is_none() && g.slots.len() > t as usize {
                miss.push(c.0);
            }
        }
        if miss.is_empty() {
            return (g, a);
        }
        cache.retain(|c| !miss.contains(&c.0));
    }
}

/// Whether `regs` has any register of the class (`fp`) to allocate.
const fn has_file(regs: &Regs, fp: bool) -> bool {
    if fp {
        regs.fp_size != 0
            && !(regs.fp_saved.is_empty() && regs.fp_free.is_empty() && regs.fp_arg.is_empty())
    } else {
        !(regs.int_saved.is_empty() && regs.int_free.is_empty() && regs.int_arg.is_empty())
    }
}

/// `f` as wasm32 sees it above `-O0`: roots cached in temporaries (which
/// become locals), dead computations dropped. Positions go first and
/// come back as `rows`, as in `prepare`.
#[must_use]
pub fn prepare_locals(f: &Func, regs: &Regs) -> Func {
    let lineless = f
        .insts
        .iter()
        .any(|i| matches!(i, Inst::Line(..)))
        .then(|| {
            f.with_insts(
                f.insts
                    .iter()
                    .filter(|i| !matches!(i, Inst::Line(..)))
                    .cloned()
                    .collect(),
            )
        });
    let mut g = prepare_locals_code(lineless.as_ref().unwrap_or(f), regs);
    if f.track {
        g.rows = anchor(f, &g);
    }
    g
}

fn prepare_locals_code(f: &Func, regs: &Regs) -> Func {
    let stored = crate::frame::dse(f, regs.ptr);
    let f = stored.as_ref().unwrap_or(f);
    let (cache, done) = worth_caching(f, regs, &Freq::of(f));
    if let Some((g, _)) = done {
        return g;
    }
    let mut g = if cache.is_empty() {
        f.clone()
    } else {
        cache_roots(f, regs, &cache)
    };
    dce(&mut g, regs);
    g
}

/// Every memory operand of `i` (`locs`).
pub fn mems(i: &Inst, ptr: u32, out: &mut Vec<Mem>) {
    let mut ops = Vec::new();
    operands(i, ptr, &mut ops);
    out.extend(ops.iter().map(|o| o.0));
}

/// The memory operands `i` writes.
pub fn written(i: &Inst, ptr: u32, out: &mut Vec<Mem>) {
    let mut ops = Vec::new();
    operands(i, ptr, &mut ops);
    out.extend(ops.iter().filter(|o| o.3 == Acc::Write).map(|o| o.0));
}

/// Whether `i` writes part `p` (`locs`).
pub fn writes(i: &Inst, p: (Slot, u32)) -> bool {
    let mut ops = Vec::new();
    operands(i, 8, &mut ops);
    ops.iter()
        .any(|o| o.3 == Acc::Write && o.0 == Mem::Frame(p.0, p.1))
}

/// Whether `i` writes any part of slot `s` (`locs`).
pub fn writes_slot(i: &Inst, s: Slot) -> bool {
    let mut ops = Vec::new();
    operands(i, 8, &mut ops);
    ops.iter()
        .any(|o| o.3 == Acc::Write && matches!(o.0, Mem::Frame(x, _) if x == s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lir::{Bin, Part, VBin, Vx, VxOp};

    /// An arm64-like file: two callee-saved, two caller-saved, argument
    /// registers 0..2.
    const T: Regs = Regs {
        int_saved: &[19, 20],
        int_free: &[9],
        int_arg: &[0, 1],
        fp_saved: &[8],
        fp_free: &[16],
        fp_arg: &[],
        int_args: &[0, 1],
        fp_args: &[0, 1],
        int_rets: &[0, 1],
        fp_rets: &[0, 1],
        sret: 8,
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

    const fn int(n: u8) -> Part {
        Part {
            loc: Loc::Int(n),
            k: K::I64,
            signed: false,
        }
    }

    const fn m(s: Slot) -> Mem {
        Mem::Frame(s, 0)
    }

    fn func(slots: usize, insts: Vec<Inst>) -> Func {
        Func {
            slots: vec![(8, 8); slots],
            insts,
            labels: 4,
            opt: true,
            ..Func::default()
        }
    }

    const fn add(dst: Slot, a: V, b: V) -> Inst {
        Inst::Bin {
            op: Bin::Add,
            k: K::I64,
            signed: false,
            dst: m(dst),
            a,
            b,
        }
    }

    /// `dst = a < 100`.
    const fn lt(dst: Slot, a: Mem) -> Inst {
        Inst::Cmp {
            cc: crate::lir::Cc::Lt,
            k: K::I64,
            signed: true,
            dst: m(dst),
            a: V::M(a),
            b: V::I(100),
        }
    }

    const fn call(args: Vec<(Part, V)>, rets: Vec<(Part, Mem)>) -> Inst {
        Inst::Call {
            callee: Callee::Sym(0),
            args,
            rets,
            sret: None,
            variadic: false,
            pop: 0,
        }
    }

    /// A value live across a call (and used enough to pay for the save)
    /// takes a callee-saved register; one read by the call only does not.
    /// Vectors take float registers, never one a call keeps only the low
    /// half of: across a call one stays in memory.
    #[test]
    fn vectors_in_float_registers() {
        let regs = Regs { vec: 16, ..T };
        let vx = |op, dst, a, b| {
            Inst::V(Box::new(Vx {
                op,
                w: 4,
                vb: 16,
                dst: m(dst),
                a: m(a),
                b: m(b),
            }))
        };
        let mut f = func(
            4,
            vec![
                vx(VxOp::Const(0), 0, 0, 0),
                vx(VxOp::Const(1), 1, 1, 1),
                vx(VxOp::Bin(VBin::FAdd), 2, 0, 1),
                call(Vec::new(), Vec::new()),
                vx(VxOp::Bin(VBin::FAdd), 3, 2, 0),
                vx(VxOp::Mov, 3, 3, 3),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        f.slots = vec![(16, 16); 4];
        let a = allocate(&f, &regs);
        assert!(a.get(m(1)).is_some_and(|r| r.fp && r.reg == 16));
        assert!(a.get(m(0)).is_none() && a.get(m(2)).is_none());
        assert!(a.saved_fp.is_empty());
    }

    #[test]
    fn across_calls_callee_saved() {
        let f = func(
            3,
            vec![
                Inst::Param {
                    part: int(0),
                    dst: m(0),
                },
                add(1, V::M(m(0)), V::I(1)),
                call(vec![(int(0), V::M(m(1)))], vec![(int(0), m(2))]),
                add(2, V::M(m(2)), V::M(m(0))),
                add(2, V::M(m(2)), V::M(m(0))),
                add(2, V::M(m(2)), V::M(m(0))),
                add(2, V::M(m(2)), V::M(m(0))),
                add(2, V::M(m(2)), V::M(m(0))),
                Inst::Ret {
                    vals: vec![(int(0), V::M(m(2)))],
                },
            ],
        );
        let a = allocate(&f, &T);
        assert_eq!(a.get(m(0)).map(|r| r.reg), Some(19));
        assert!(a.get(m(1)).is_some_and(|r| !T.int_saved.contains(&r.reg)));
        assert_eq!(a.saved, [19]);
        assert!(a.gone(0) && a.gone(1));
    }

    /// With callee-saved registers saved in pairs, a value used too little
    /// to open one of its own takes the partner of an open one.
    #[test]
    fn partner_of_saved_register() {
        let f = func(
            4,
            vec![
                Inst::Param {
                    part: int(0),
                    dst: m(0),
                },
                Inst::Param {
                    part: int(1),
                    dst: m(3),
                },
                add(1, V::M(m(0)), V::I(1)),
                call(vec![(int(0), V::M(m(1)))], vec![(int(0), m(2))]),
                add(2, V::M(m(2)), V::M(m(0))),
                add(2, V::M(m(2)), V::M(m(0))),
                add(2, V::M(m(2)), V::M(m(0))),
                add(2, V::M(m(2)), V::M(m(0))),
                add(2, V::M(m(2)), V::M(m(0))),
                add(2, V::M(m(2)), V::M(m(3))),
                Inst::Ret {
                    vals: vec![(int(0), V::M(m(2)))],
                },
            ],
        );
        let a = allocate(&f, &T);
        assert_eq!(a.get(m(0)).map(|r| r.reg), Some(19));
        assert!(a.get(m(3)).is_none());
        let paired = Regs {
            saved_pairs: true,
            ..T
        };
        let a = allocate(&f, &paired);
        assert_eq!(a.get(m(0)).map(|r| r.reg), Some(19));
        assert_eq!(a.get(m(3)).map(|r| r.reg), Some(20));
        assert_eq!(a.saved, [19, 20]);
    }

    /// A 32-bit file of three callee-saved registers.
    const T32: Regs = Regs {
        int_saved: &[3, 6, 7],
        int_free: &[],
        int_arg: &[],
        fp_saved: &[],
        fp_free: &[],
        fp_arg: &[],
        int_args: &[],
        fp_args: &[],
        int_rets: &[0, 2],
        fp_rets: &[],
        sret: 0,
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

    /// On a 32-bit target an 8-byte integer a loop works on takes two
    /// registers; one used a few times stays in memory.
    #[test]
    fn pairs_for_hot_wide_values() {
        let f = func(
            3,
            vec![
                Inst::Param {
                    part: int(0),
                    dst: m(0),
                },
                Inst::Mov {
                    k: K::I64,
                    dst: m(1),
                    src: V::I(0),
                },
                Inst::Label(0),
                add(1, V::M(m(1)), V::M(m(0))),
                Inst::Cmp {
                    cc: crate::lir::Cc::Lt,
                    k: K::I64,
                    signed: true,
                    dst: Mem::Frame(2, 0),
                    a: V::M(m(1)),
                    b: V::I(100),
                },
                Inst::Br {
                    c: V::M(Mem::Frame(2, 0)),
                    t: 0,
                    f: 1,
                },
                Inst::Label(1),
                Inst::Ret {
                    vals: vec![(int(0), V::M(m(1)))],
                },
            ],
        );
        let a = allocate(&f, &T32);
        let (lo, hi) = a.pair(m(1)).expect("a pair");
        assert_ne!(lo, hi);
        assert!(a.get(m(1)).is_none());
        assert!(a.gone(1));
        // Read once in the loop, written once before it: not worth two.
        assert!(a.pair(m(0)).is_none() && !a.gone(0));
    }

    /// An argument register some earlier argument writes cannot hold a
    /// later argument's value.
    #[test]
    fn argument_order() {
        let f = func(
            3,
            vec![
                add(0, V::I(1), V::I(2)),
                add(1, V::I(3), V::I(4)),
                call(vec![(int(0), V::M(m(0))), (int(1), V::M(m(1)))], Vec::new()),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        let no = forbidden(
            &f,
            &T,
            &|x| match x {
                Mem::Frame(s, 0) if s < 2 => Some(s as usize),
                _ => None,
            },
            2,
        );
        assert_eq!(no[0][0], 0);
        assert_eq!(no[1][0], 1 << 0);
    }

    /// A later argument (or return value, or code pointer) read through
    /// a pointer: the pointer cannot sit in a register an earlier move
    /// writes.
    #[test]
    fn argument_through_pointer() {
        let only = |x: Mem| match x {
            Mem::Frame(s, 0) if s < 2 => Some(s as usize),
            _ => None,
        };
        let f = func(
            3,
            vec![
                add(0, V::I(1), V::I(2)),
                add(1, V::I(3), V::I(4)),
                call(
                    vec![(int(0), V::M(m(0))), (int(1), V::M(Mem::Ind(1, 8)))],
                    Vec::new(),
                ),
                Inst::Ret {
                    vals: vec![(int(0), V::M(m(0))), (int(1), V::M(Mem::Ind(1, 16)))],
                },
            ],
        );
        let no = forbidden(&f, &T, &only, 2);
        assert_eq!(no[0][0], 0);
        assert_eq!(no[1][0], 1 << 0);
        let g = func(
            2,
            vec![
                add(0, V::I(1), V::I(2)),
                Inst::Call {
                    callee: Callee::Ptr(Mem::Ind(1, 0)),
                    args: vec![(int(0), V::M(m(0))), (int(1), V::I(7))],
                    rets: Vec::new(),
                    sret: None,
                    variadic: false,
                    pop: 0,
                },
                Inst::Ret { vals: Vec::new() },
            ],
        );
        let no = forbidden(&g, &T, &only, 2);
        assert_eq!(no[1][0] & 0b11, 0b11);
        let a = allocate(&f, &T);
        assert_ne!(a.get(m(1)).map(|r| r.reg), Some(0));
    }

    /// Values whose ranges never meet share a register.
    #[test]
    fn holes_share() {
        let f = func(
            2,
            vec![
                add(0, V::I(1), V::I(2)),
                add(0, V::M(m(0)), V::I(1)),
                Inst::Ret {
                    vals: vec![(int(0), V::M(m(0)))],
                },
                Inst::Label(0),
                add(1, V::I(1), V::I(2)),
                Inst::Ret {
                    vals: vec![(int(0), V::M(m(1)))],
                },
            ],
        );
        let parts = [((0, 0), 0u8), ((1, 0), 0u8)];
        assert_eq!(share(&f, &T, &parts), [0, 0]);
    }

    /// A loop's body runs more often than its entry; a path ending in a
    /// trap hardly ever; a call on one side of a branch makes it the
    /// unlikely one.
    #[test]
    fn block_frequencies() {
        let mut f = func(
            3,
            vec![
                Inst::Param {
                    part: int(0),
                    dst: m(0),
                },
                Inst::Label(0),
                Inst::Br {
                    c: V::M(m(0)),
                    t: 1,
                    f: 2,
                },
                Inst::Label(1),
                call(Vec::new(), Vec::new()),
                Inst::Jmp(2),
                Inst::Label(2),
                Inst::Br {
                    c: V::M(m(0)),
                    t: 3,
                    f: 0,
                },
                Inst::Label(3),
                Inst::Br {
                    c: V::M(m(0)),
                    t: 4,
                    f: 5,
                },
                Inst::Label(4),
                Inst::Trap,
                Inst::Label(5),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        f.labels = 6;
        let fr = Cfg::new(&f).freqs(&f);
        let at = |l: Label| {
            f.insts
                .iter()
                .position(|i| matches!(i, Inst::Label(x) if *x == l))
                .unwrap()
        };
        assert_eq!(fr[0], UNIT);
        // The loop: about eight runs per entry.
        assert!(fr[at(0)] > 6 * UNIT && fr[at(0)] < 10 * UNIT, "{fr:?}");
        // The call's side: less than half.
        assert!(2 * fr[at(1)] < fr[at(0)], "{fr:?}");
        // The trap: next to never; the exit: as often as the entry.
        assert!(fr[at(4)] < UNIT / 64, "{fr:?}");
        assert!(fr[at(5)] > UNIT / 2, "{fr:?}");
    }

    /// Values a loop works on that live across a call on its rare path,
    /// more than the callee-saved registers: the ones left over are split
    /// around the call and take caller-saved registers.
    #[test]
    fn split_around_rare_call() {
        let mut insts = vec![
            Inst::Param {
                part: int(0),
                dst: m(0),
            },
            Inst::Param {
                part: int(1),
                dst: m(1),
            },
            Inst::Mov {
                k: K::I64,
                dst: m(2),
                src: V::I(3),
            },
            Inst::Label(0),
        ];
        for _ in 0..4 {
            for s in 0..3 {
                insts.push(add(s, V::M(m(s)), V::M(m((s + 1) % 3))));
            }
        }
        insts.extend([
            lt(3, m(0)),
            Inst::Br {
                c: V::M(m(3)),
                t: 1,
                f: 2,
            },
            Inst::Label(1),
            call(Vec::new(), Vec::new()),
            Inst::Jmp(2),
            Inst::Label(2),
            lt(4, m(1)),
            Inst::Br {
                c: V::M(m(4)),
                t: 0,
                f: 3,
            },
            Inst::Label(3),
            add(0, V::M(m(0)), V::M(m(1))),
            add(0, V::M(m(0)), V::M(m(2))),
            Inst::Ret {
                vals: vec![(int(0), V::M(m(0)))],
            },
        ]);
        let f = func(5, insts);
        let (g, a) = prepare(&f, &T);
        for s in 0..3 {
            assert!(a.get(m(s)).is_some(), "{s}: {:?}", g.insts);
        }
        // One copied out and back around the call.
        let n = g
            .insts
            .iter()
            .position(|i| matches!(i, Inst::Call { .. }))
            .unwrap();
        assert!(matches!(g.insts[n - 1], Inst::Mov { .. }), "{:?}", g.insts);
        assert!(matches!(g.insts[n + 1], Inst::Mov { .. }), "{:?}", g.insts);
    }

    /// A value used around an inner loop that leaves it alone, with the
    /// registers all taken inside: copied out on the way in and back on
    /// the way out, a register outside.
    #[test]
    fn split_out_of_loop() {
        let mut insts = vec![
            Inst::Param {
                part: int(0),
                dst: m(0),
            },
            Inst::Label(0),
        ];
        // Slot 0: used in the outer loop only.
        for _ in 0..6 {
            insts.push(add(0, V::M(m(0)), V::I(1)));
        }
        for s in 1..5 {
            insts.push(Inst::Mov {
                k: K::I64,
                dst: m(s),
                src: V::I(1),
            });
        }
        insts.push(Inst::Label(1));
        // Slots 1..5: the inner loop's own.
        for s in 1..5 {
            insts.push(add(s, V::M(m(s)), V::M(m(s % 4 + 1))));
            insts.push(add(s, V::M(m(s)), V::M(m(s))));
        }
        insts.extend([
            lt(6, m(1)),
            Inst::Br {
                c: V::M(m(6)),
                t: 1,
                f: 2,
            },
            Inst::Label(2),
            lt(7, m(0)),
            Inst::Br {
                c: V::M(m(7)),
                t: 0,
                f: 3,
            },
            Inst::Label(3),
            Inst::Ret {
                vals: vec![(int(0), V::M(m(0)))],
            },
        ]);
        let mut f = func(8, insts);
        f.labels = 4;
        let (g, a) = prepare(&f, &T);
        assert!(a.get(m(0)).is_some(), "{:?}", g.insts);
        let out = g.insts.iter().any(
            |i| matches!(i, Inst::Mov { dst: Mem::Frame(t, 0), src: V::M(x), .. } if *t >= 8 && *x == m(0)),
        );
        assert!(out, "{:?}", g.insts);
    }

    /// A rooted slot read in a loop is cached: written through, reloaded
    /// after the call in the loop, read from its temporary.
    #[test]
    fn roots_cached() {
        let mut f = func(
            3,
            vec![
                Inst::Param {
                    part: int(0),
                    dst: m(0),
                },
                Inst::Addr {
                    dst: m(2),
                    src: m(0),
                },
                Inst::Label(0),
                add(1, V::M(Mem::Ind(0, 8)), V::M(Mem::Ind(0, 16))),
                add(1, V::M(m(1)), V::M(Mem::Ind(0, 24))),
                call(vec![(int(1), V::M(m(1)))], Vec::new()),
                Inst::Br {
                    c: V::M(m(1)),
                    t: 0,
                    f: 1,
                },
                Inst::Label(1),
                Inst::Ret { vals: Vec::new() },
            ],
        );
        f.gc_frame = Some((2, vec![0]));
        assert_eq!(cacheable(&f, &T), [((0, 0), 8)]);
        let g = cache_roots(&f, &T, &[((0, 0), 8)]);
        // The parameter written through, reloaded after the call.
        let t = m(3);
        assert!(
            g.insts.iter().any(
                |i| matches!(i, Inst::Mov { dst, src: V::M(s), .. } if *dst == m(0) && *s == t)
            )
        );
        assert!(
            g.insts.iter().any(
                |i| matches!(i, Inst::Mov { dst, src: V::M(s), .. } if *dst == t && *s == m(0))
            )
        );
        assert!(!g.insts.iter().any(|i| matches!(
            i,
            Inst::Bin {
                a: V::M(Mem::Ind(0, _)),
                ..
            }
        )));
        let (h, a) = prepare(&f, &T);
        assert!(a.get(m(3)).is_some(), "{:?}", h.insts);
    }
}
