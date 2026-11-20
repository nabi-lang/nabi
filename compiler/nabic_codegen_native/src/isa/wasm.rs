//! LIR -> wasm32 function bodies.
//!
//! Frame: slots in linear memory below `__stack_pointer` (the C ABI's
//! stack), the frame base in a local; a C variadic call's buffer at its
//! bottom. A scalar slot never addressed nor copied whole lives in locals
//! instead, one per part. Control flow is rebuilt as blocks and loops
//! (`cfg`). Narrow integers are held zero-extended in `i32`.

pub mod enc;
pub mod object;

mod arith;
mod call;
mod cfg;
mod locals;
mod mem;
mod select;
mod vector;

pub(crate) use crate::isa::wasm::call::*;
pub(crate) use crate::isa::wasm::locals::*;
pub use enc::{F32, F64, I32, I64, V128, uleb5};

use crate::debug::locs::{Place, Spans};
use crate::lir::{
    Bin, Callee, Cc, Conv, Func, Inst, K, Loc, Mem, NO_SLOT, Part, PartSrc, Rmw, Un, V, scalar,
};
use crate::obj::{Obj, Sect};
use enc::{Asm, Atomic, EMPTY, MemOp, Num, Sat, Width};
use nabic_codegen::abi::{ArgAbi, CType, RetAbi};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::PrimTy;
use nabic_mir::MirTy;

/// A function type: parameter and result value types.
pub type Ty = (Vec<u8>, Vec<u8>);

/// What a relocated operand names.
#[derive(Clone, Debug)]
pub enum Rel {
    /// `call`: a function index.
    Call(u32),
    /// `i32.const` of an address: a function's table index, else a data
    /// address (the writer tells them apart).
    Addr(u32),
    /// `i32.const` of a thread-local's offset from `__tls_base`.
    Tls(u32),
    /// `global.get` / `global.set`.
    Global(u32),
    /// `call_indirect`'s table.
    Table(u32),
    /// `call_indirect`'s type.
    Type(Ty),
}

/// A 5-byte LEB operand at `off` of the code.
#[derive(Clone, Debug)]
pub struct Reloc {
    pub off: u32,
    pub rel: Rel,
}

/// A function's code: its locals, instructions and `end`.
pub struct Code {
    pub sym: u32,
    pub ty: Ty,
    pub body: Vec<u8>,
    pub relocs: Vec<Reloc>,
    /// `--emit=asm`: each local's type past the parameters, and the
    /// instructions (offsets in `body`).
    pub text: Option<(Vec<u8>, Vec<enc::Line>)>,
}

/// The wasm side of an object.
#[derive(Default)]
pub struct Parts {
    pub code: Vec<Code>,
    /// Type of every function called or defined.
    pub sigs: HashMap<u32, Ty>,
    /// Symbols known to be functions.
    pub fns: HashSet<u32>,
    /// Exported under their names (a browser module's exports).
    pub exports: HashSet<u32>,
    /// Undefined functions imported as `(module, name)`.
    pub imports: HashMap<u32, (String, String)>,
    /// Thread-local data defined here.
    pub tls: HashSet<u32>,
    /// wasi-threads: shared memory, TLS through `__tls_base`.
    pub threads: bool,
    /// `-g`: each function's frame base local.
    pub fp: HashMap<u32, u32>,
}

pub const fn vt(k: K) -> u8 {
    match k {
        K::I8 | K::I16 | K::I32 => I32,
        K::I64 => I64,
        K::F32 => F32,
        K::F64 => F64,
    }
}

/// Where a slot part lives when not in memory: its local and type.
#[derive(Clone, Copy)]
pub(crate) struct Reg {
    local: u32,
    ty: u8,
}

/// A pending store: a local, or memory at an offset from the address
/// already pushed.
enum St {
    Local(Reg, K),
    Mem(u32, K),
}

struct Em<'a> {
    obj: &'a mut Obj,
    a: Asm,
    /// Types of the locals after the parameters.
    locals: Vec<u8>,
    nparams: u32,
    /// Scratch locals by type (`I32 I64 F32 F64`) and how many this
    /// instruction took.
    pool: [Vec<u32>; 4],
    used: [usize; 4],
    fp: u32,
    frame: u32,
    slot_off: Vec<u32>,
    regs: HashMap<(u32, u32), Reg>,
    /// The hidden result pointer first; the results.
    sret: bool,
    results: Vec<u8>,
    sp: u32,
    tls_base: u32,
    table: u32,
    /// The flag of the last `Ovf`.
    ovf: u32,
    /// `(code offset, line, column, scope)`.
    lines: Vec<(u32, u32, u32, u32)>,
    /// Optimized `-g`: `(code offset, instruction)` as each starts.
    marks: Option<Vec<(u32, usize)>>,
}

/// What `regalloc` needs of wasm32: no registers; parts live in locals.
const REGS: crate::regalloc::Regs = crate::regalloc::Regs {
    int_saved: &[],
    int_free: &[],
    int_arg: &[],
    fp_saved: &[],
    fp_free: &[],
    fp_arg: &[],
    int_args: &[],
    fp_args: &[],
    int_rets: &[],
    fp_rets: &[],
    sret: 0,
    word: 8,
    ptr: 4,
    fp_size: 8,
    vec: 0,
    vec_saved: false,
    costly_imm: None,
    pairs: false,
    saved_pairs: false,
    saves_all: false,
    clobber: None,
};

pub fn emit(obj: &mut Obj, f: &Func) {
    // Above `-O0` roots are cached in locals too.
    let owned = f.opt.then(|| crate::regalloc::prepare_locals(f, &REGS));
    let f = owned.as_ref().unwrap_or(f);
    let (params, ret) = signature(f);
    let ty = fn_ty(&params, ret.as_deref(), false);
    let sret = ret.is_none();
    let nparams = ty.0.len() as u32;
    // The variadic buffer at the bottom.
    let mut out = 0u32;
    for i in &f.insts {
        if let Inst::Call { args, .. } = i {
            for (p, _) in args {
                if let Loc::Stack(o) = p.loc {
                    out = out.max(o + p.k.size().max(4));
                }
            }
        }
    }
    let promo = promotable(f);
    let crate::frame::Layout {
        off: slot_off,
        end: at,
        live,
    } = crate::frame::layout(
        f,
        &|s| promo.keys().any(|k| k.0 == s),
        (out.next_multiple_of(16), 16, u32::MAX),
        4,
    );
    let frame = at.next_multiple_of(16);
    let wasm = obj.wasm.as_mut().expect("a wasm object");
    wasm.sigs.insert(f.sym, ty.clone());
    wasm.fns.insert(f.sym);
    let sp = obj.sym("__stack_pointer");
    let tls_base = obj.sym("__tls_base");
    let table = obj.sym("__indirect_function_table");
    let text = obj.asm.is_some();
    let mut e = Em {
        obj,
        a: Asm::new(text),
        locals: Vec::new(),
        nparams,
        pool: Default::default(),
        used: [0; 4],
        fp: 0,
        frame,
        slot_off,
        regs: HashMap::default(),
        sret,
        results: ty.1.clone(),
        sp,
        tls_base,
        table,
        ovf: 0,
        lines: Vec::new(),
        marks: (f.opt && f.debug.is_some()).then(Vec::new),
    };
    e.fp = e.new_local(I32);
    let sizes: HashMap<(u32, u32), u32> = promo.iter().map(|(&p, &(n, _))| (p, n)).collect();
    let mut keys: Vec<_> = promo
        .into_iter()
        .map(|((s, o), (size, k))| {
            let ty = match k {
                Some(K::F32) if size == 4 => F32,
                Some(K::F64) if size == 8 => F64,
                _ if size == 8 => I64,
                _ => I32,
            };
            ((s, o), ty)
        })
        .collect();
    keys.sort_unstable_by_key(|k| k.0);
    // Above `-O0` parts whose live ranges never meet share a local.
    let colors = if f.opt {
        crate::regalloc::share(f, &REGS, &keys)
    } else {
        (0..keys.len() as u32).collect()
    };
    let mut shared: HashMap<(u8, u32), u32> = HashMap::default();
    for (&(p, ty), c) in keys.iter().zip(colors) {
        let local = if let Some(&l) = shared.get(&(ty, c)) {
            l
        } else {
            let l = e.new_local(ty);
            shared.insert((ty, c), l);
            l
        };
        e.regs.insert(p, Reg { local, ty });
    }
    e.ovf = e.new_local(I32);
    if frame > 0 {
        e.a.global_get(sp);
        e.i32c(frame as i32);
        e.a.num(Num::I32Sub);
        e.a.local_tee(e.fp);
        e.a.global_set(sp);
    }
    // Locals start as zero, as the frame does not: nothing reads a part
    // before writing it.
    let g = cfg::Graph::new(&f.insts);
    e.structured(&g, &f.insts);
    e.a.unreachable();
    e.a.end();
    let mut body = enc::locals(&e.locals);
    let shift = body.len() as u32;
    body.extend_from_slice(&e.a.code);
    let text = e.a.take_text(shift).map(|t| (e.locals.clone(), t));
    let relocs = std::mem::take(&mut e.a.relocs)
        .into_iter()
        .map(|r| Reloc {
            off: r.off + shift,
            rel: r.rel,
        })
        .collect();
    if f.track {
        if let Some(m) = &e.marks {
            e.lines = rows_at(&f.rows, m);
        }
    }
    let sym = f.sym;
    let lines = e
        .lines
        .iter()
        .map(|l| (l.0 + shift, l.1, l.2, l.3))
        .collect();
    let debug = f.debug.as_ref().map(|d| {
        let offs = d
            .vars
            .iter()
            .map(|v| e.slot_off.get(v.slot as usize).map_or(0, |&o| o as i32))
            .collect();
        (d.clone(), offs)
    });
    let locs = e
        .marks
        .take()
        .map(|m| {
            var_locals(
                f,
                (&e.regs, &sizes),
                (&e.slot_off, &live),
                &m,
                (e.a.pos() as u32, shift),
            )
        })
        .unwrap_or_default();
    let fp = e.fp;
    let o = e.obj;
    o.fns.push(crate::obj::FnRange {
        locs,
        sym,
        start: 0,
        len: body.len() as u32,
        lines,
        debug,
        saves: crate::obj::Saves::default(),
    });
    o.wasm.as_mut().expect("a wasm object").fp.insert(sym, fp);
    o.syms[sym as usize].def = Some((
        Sect::Text,
        o.wasm.as_ref().map_or(0, |w| w.code.len()) as u32,
        0,
    ));
    o.wasm.as_mut().expect("a wasm object").code.push(Code {
        sym,
        ty,
        body,
        relocs,
        text,
    });
}

impl Em<'_> {
    /// Instruction `i` starts here (optimized `-g`).
    fn mark(&mut self, i: usize) {
        let at = self.a.pos() as u32;
        if let Some(m) = &mut self.marks {
            m.push((at, i));
        }
    }

    fn new_local(&mut self, ty: u8) -> u32 {
        self.locals.push(ty);
        self.nparams + self.locals.len() as u32 - 1
    }

    /// A scratch local of `ty`, free again at the next instruction.
    fn tmp(&mut self, ty: u8) -> u32 {
        let i = match ty {
            I32 => 0,
            I64 => 1,
            F32 => 2,
            _ => 3,
        };
        if self.used[i] == self.pool[i].len() {
            let l = self.new_local(ty);
            self.pool[i].push(l);
        }
        self.used[i] += 1;
        self.pool[i][self.used[i] - 1]
    }

    fn get(&mut self, l: u32) {
        self.a.local_get(l);
    }

    fn set(&mut self, l: u32) {
        self.a.local_set(l);
    }

    fn i32c(&mut self, v: i32) {
        self.a.i32_const(v);
    }

    fn i64c(&mut self, v: i64) {
        self.a.i64_const(v);
    }

    fn threads(&self) -> bool {
        self.obj.wasm.as_ref().is_some_and(|w| w.threads)
    }

    fn call(
        &mut self,
        callee: &Callee,
        args: &[(Part, V)],
        rets: &[(Part, Mem)],
        sret: Option<Mem>,
        variadic: bool,
    ) {
        // The variadic buffer, then the arguments in order.
        for (p, v) in args {
            if let Loc::Stack(o) = p.loc {
                self.get(self.fp);
                self.val_ext(*v, p.k, p.signed);
                let k = if p.k.size() < 4 { K::I32 } else { p.k };
                let (op, align) = match k {
                    K::I64 => (MemOp::I64Store, 3),
                    K::F32 => (MemOp::F32Store, 2),
                    K::F64 => (MemOp::F64Store, 3),
                    _ => (MemOp::I32Store, 2),
                };
                self.a.mem(op, align, o);
            }
        }
        if let Some(m) = sret {
            self.addr(m);
        }
        let mut ps: Vec<&(Part, V)> = args
            .iter()
            .filter(|a| matches!(a.0.loc, Loc::Int(_)))
            .collect();
        ps.sort_by_key(|a| match a.0.loc {
            Loc::Int(n) => n,
            _ => 0,
        });
        for (p, v) in &ps {
            self.val_ext(*v, p.k, p.signed);
        }
        let stacked = args.iter().any(|a| matches!(a.0.loc, Loc::Stack(_)));
        if variadic {
            if stacked {
                self.get(self.fp);
            } else {
                self.i32c(0);
            }
        }
        let params: Vec<Part> = ps.iter().map(|a| a.0).collect();
        let rparts: Vec<Part> = rets.iter().map(|r| r.0).collect();
        let ty = fn_ty(
            &params,
            if sret.is_some() { None } else { Some(&rparts) },
            variadic,
        );
        match callee {
            Callee::Sym(s) => {
                let w = self.obj.wasm.as_mut().expect("a wasm object");
                w.sigs.entry(*s).or_insert_with(|| ty.clone());
                w.fns.insert(*s);
                self.a.call(*s);
            }
            Callee::Ptr(m) => {
                self.load(*m, K::I32);
                self.a.call_indirect(ty, self.table);
            }
        }
        // One result at most.
        if let Some((p, m)) = rets.first() {
            let l = self.tmp(vt(p.k));
            self.set(l);
            self.store_local(*m, p.k, l);
        }
    }
}

/// The integer kind of `k`'s size.
const fn int_k(k: K) -> K {
    match k {
        K::F32 => K::I32,
        K::F64 => K::I64,
        k => k,
    }
}

/// `nabi_tls` in wasm TLS and `nabi_tls_addr`, its address in this
/// thread (`wasm32-wasip1-threads`: stable Rust cannot define wasm TLS).
#[must_use]
pub fn tls_object() -> Vec<u8> {
    let mut o = Obj::new(false, false, false);
    o.wasm = Some(Box::new(Parts {
        threads: true,
        ..Parts::default()
    }));
    let size = nabic_mir::tls_abi::SIZE.offset(4);
    let align = nabic_mir::tls_abi::ALIGN;
    let tls = o.sym("nabi_tls");
    o.define_data(tls, Sect::Data, &vec![0; size as usize], align, true);
    let f = o.sym("nabi_tls_addr");
    o.syms[f as usize].hidden = true;
    let base = o.sym("__tls_base");
    let mut a = Asm::new(false);
    a.global_get(base);
    a.i32_const_rel(Rel::Tls(tls));
    a.num(Num::I32Add);
    a.end();
    let mut code = enc::locals(&[]);
    code.extend_from_slice(&a.code);
    let relocs = a
        .relocs
        .into_iter()
        .map(|r| Reloc {
            off: r.off + 1,
            rel: r.rel,
        })
        .collect();
    o.syms[f as usize].def = Some((Sect::Text, 0, 0));
    let w = o.wasm.as_mut().expect("a wasm object");
    w.tls.insert(tls);
    w.fns.insert(f);
    w.code.push(Code {
        sym: f,
        ty: (Vec::new(), vec![I32]),
        body: code,
        relocs,
        text: None,
    });
    object::write(&o)
}
