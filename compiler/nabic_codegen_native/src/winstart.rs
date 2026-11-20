//! nabi's CRT startup for Windows (`*-pc-windows-msvc`) instead of MSVC's
//! static parts, as one COFF object: the entry (UCRT setup, `.CRT$XC*`
//! initializers, `nabi_main`, `exit`), the loader's TLS directory
//! (`_tls_used`), `_fltused`, `__chkstk` (the probe the Rust-built runtime
//! calls for frames of a page or more), and `printf` / `fprintf` /
//! `sprintf` / `snprintf`, which UCRT exports none of (its headers inline
//! them over `__stdio_common_v*printf`), so C variadic calls reach these.
//!
//! Assembled by the backend's encoders for `x86_64`, arm64 and x86. The
//! functions keep the backend's frame pointer prolog and epilog, so their
//! unwind info is the usual one (x86 has none). x86 names carry the C
//! prefix `_`.

use crate::isa::a64::enc::{self as a64, AddOp};
use crate::isa::i386::enc as i386;
use crate::isa::x64::enc::{self as x64, Addr, Alu, Cond, Gpr, Rm, Sz};
use crate::lir::SymId;
use crate::obj::RelKind;

const RAX: u32 = 0;
const RCX: u32 = 1;
const RDX: u32 = 2;
const RSP: u32 = 4;
const RBP: u32 = 5;
const R8: u32 = 8;
const R9: u32 = 9;

const MACHINE_AMD64: u16 = 0x8664;
const MACHINE_ARM64: u16 = 0xaa64;
const MACHINE_I386: u16 = 0x14c;

const CODE: u32 = 0x20 | 0x2000_0000 | 0x4000_0000;
const RDATA: u32 = 0x40 | 0x4000_0000;
const DATA: u32 = RDATA | 0x8000_0000;
const LNK_COMDAT: u32 = 0x1000;
const SELECT_ANY: u8 = 2;
const SELECT_ASSOCIATIVE: u8 = 5;
const CLASS_EXTERNAL: u8 = 2;
const CLASS_STATIC: u8 = 3;
const DTYPE_FUNCTION: u16 = 0x20;

const AMD64_ADDR64: u16 = 1;
const AMD64_ADDR32NB: u16 = 3;
const AMD64_REL32: u16 = 4;
const ARM64_ADDR32NB: u16 = 2;
const ARM64_BRANCH26: u16 = 3;
const ARM64_PAGEBASE_REL21: u16 = 4;
const ARM64_PAGEOFFSET_12A: u16 = 6;
const ARM64_ADDR64: u16 = 0xe;
const I386_DIR32: u16 = 6;
const I386_REL32: u16 = 0x14;

/// x64 `UNWIND_INFO` of a `push rbp; mov rbp, rsp` prolog (`coff`'s).
const X64_UNWIND: [u8; 8] = [0x01, 0x04, 0x02, 0x05, 0x04, 0x03, 0x01, 0x50];
/// arm64 unwind codes of `crate::isa::a64::PROLOGUE`: `set_fp`,
/// `save_fplr_x` 16, `end`, a `nop` pad.
const A64_CODES: [u8; 4] = [0xe1, 0x81, 0xe4, 0xe3];
/// Of a variadic function's prolog, `sub sp, sp, #64` first: `set_fp`,
/// `save_fplr_x` 16, `alloc_s` 64, `end`.
const A64_CODES_VARARGS: [u8; 4] = [0xe1, 0x81, 0x04, 0xe4];

/// A function: its code, relocations `(offset, type, symbol)`, and the
/// offsets of its epilogs (arm64 unwind info).
struct Fn {
    name: &'static str,
    code: Vec<u8>,
    relocs: Vec<(u32, u16, &'static str)>,
    epilogs: Vec<u32>,
    /// The arm64 prolog allocates the register save area of `...`.
    varargs: bool,
    /// An any-copy COMDAT of its own (`printf`: a C object may bring one).
    comdat: bool,
    /// Has unwind info (not the leaf `__chkstk`).
    unwind: bool,
}

/// The code of a function in the making, by the arch's encoder.
enum Code {
    X64(x64::Asm),
    A64(a64::Asm),
    I386(i386::Asm),
}

/// Machine code under construction: the code, the symbols its
/// relocations name (by `SymId`), the epilogs.
struct Asm {
    code: Code,
    names: Vec<&'static str>,
    /// arm64 relocations `(offset, type, symbol)` (its encoder keeps none).
    relocs: Vec<(u32, u16, &'static str)>,
    epilogs: Vec<u32>,
}

impl Asm {
    const fn new(code: Code) -> Self {
        Self {
            code,
            names: Vec::new(),
            relocs: Vec::new(),
            epilogs: Vec::new(),
        }
    }

    /// `x86_64` (`x86`) or arm64 code.
    fn arch(x86: bool) -> Self {
        Self::new(if x86 {
            Code::X64(x64::Asm::new(false))
        } else {
            Code::A64(a64::Asm::new(false))
        })
    }

    fn i386() -> Self {
        Self::new(Code::I386(i386::Asm::new(false)))
    }

    const fn x86(&self) -> bool {
        matches!(self.code, Code::X64(_))
    }

    /// The `SymId` of `name` in this function's relocations.
    fn sym(&mut self, name: &'static str) -> SymId {
        if let Some(i) = self.names.iter().position(|n| *n == name) {
            return i as SymId;
        }
        self.names.push(name);
        (self.names.len() - 1) as SymId
    }

    fn x(&mut self) -> &mut x64::Asm {
        match &mut self.code {
            Code::X64(x) => x,
            _ => unreachable!("x86_64 code"),
        }
    }

    fn i(&mut self) -> &mut i386::Asm {
        match &mut self.code {
            Code::I386(i) => i,
            _ => unreachable!("x86 code"),
        }
    }

    /// The offset of the next instruction.
    fn pos(&self) -> u32 {
        match &self.code {
            Code::X64(x) => x.pos() as u32,
            Code::A64(a) => 4 * a.pos() as u32,
            Code::I386(i) => i.pos() as u32,
        }
    }

    /// arm64 instructions.
    fn w(&mut self, ins: &[a64::Ins]) {
        let Code::A64(a) = &mut self.code else {
            unreachable!("arm64 code")
        };
        for i in ins {
            a.push(*i);
        }
    }

    /// arm64 instructions given as their words (`a64::PROLOGUE`).
    fn words(&mut self, words: &[u32]) {
        let Code::A64(a) = &mut self.code else {
            unreachable!("arm64 code")
        };
        a.code.extend_from_slice(words);
    }

    /// x64 `lea r, [rip + sym]`.
    fn lea_rip(&mut self, r: u32, sym: &'static str) {
        let s = self.sym(sym);
        self.x().lea(Gpr(r), Addr::Rip(RelKind::X86Pc32, s));
    }

    /// x86 `call sym`.
    fn call32(&mut self, sym: &'static str) {
        let s = self.sym(sym);
        self.i().call(RelKind::I386Call, s, 0);
    }

    /// x86 `push offset sym`.
    fn push32(&mut self, sym: &'static str) {
        let s = self.sym(sym);
        self.i().push_sym(RelKind::I386Abs32, s);
    }

    /// `call sym` / `bl sym`.
    fn call(&mut self, sym: &'static str) {
        if self.x86() {
            let s = self.sym(sym);
            self.x().call(RelKind::X86Call, s);
        } else {
            let at = self.pos();
            self.relocs.push((at, ARM64_BRANCH26, sym));
            self.w(&[a64::b(true, 0)]);
        }
    }

    /// arm64 `x<rd> = &sym` (`adrp`, `add`).
    fn adr(&mut self, rd: u32, sym: &'static str) {
        let at = self.pos();
        self.relocs.push((at, ARM64_PAGEBASE_REL21, sym));
        self.relocs.push((at + 4, ARM64_PAGEOFFSET_12A, sym));
        let r = a64::R(rd);
        self.w(&[a64::adrp(r), a64::add_imm(AddOp::Add, true, r, r, 0, false)]);
    }

    /// arm64 `mov w<rd>, #imm`.
    fn movz(&mut self, rd: u32, imm: u32) {
        self.w(&[a64::movz_w(a64::R(rd), imm)]);
    }

    fn prologue(&mut self, frame: u8) {
        if self.x86() {
            let x = self.x();
            x.push_rbp();
            x.mov_st(Sz::Q, Rm::r(RBP), Gpr(RSP));
            x.alu_ri8(Alu::Sub, Sz::Q, Rm::r(RSP), frame as i8);
        } else {
            self.words(&crate::isa::a64::PROLOGUE);
        }
    }

    fn epilogue(&mut self) {
        let at = self.pos();
        self.epilogs.push(at);
        if self.x86() {
            let x = self.x();
            x.mov_st(Sz::Q, Rm::r(RSP), Gpr(RBP));
            x.pop_rbp();
            x.ret();
        } else {
            self.words(&crate::isa::a64::EPILOGUE);
        }
    }

    fn done(self, name: &'static str) -> Fn {
        // A relocation's COFF type by its kind.
        let ty = |k: RelKind| match k {
            RelKind::X86Call | RelKind::X86Pc32 => AMD64_REL32,
            RelKind::I386Call => I386_REL32,
            RelKind::I386Abs32 => I386_DIR32,
            k => unreachable!("{k:?} in startup code"),
        };
        let names = self.names;
        let enc = |rs: &[(u32, RelKind, SymId)]| -> Vec<(u32, u16, &'static str)> {
            rs.iter()
                .map(|&(o, k, s)| (o, ty(k), names[s as usize]))
                .collect()
        };
        let (code, relocs) = match self.code {
            Code::X64(x) => (x.code.clone(), enc(&x.relocs)),
            Code::I386(i) => (i.code.clone(), enc(&i.relocs)),
            Code::A64(a) => (
                a.code.iter().flat_map(|w| w.to_le_bytes()).collect(),
                self.relocs,
            ),
        };
        Fn {
            name,
            code,
            relocs,
            epilogs: self.epilogs,
            varargs: false,
            comdat: false,
            unwind: true,
        }
    }
}

/// `_set_app_type(app)`, the narrow argv and environment, the C++-style
/// initializers.
fn crt_setup(a: &mut Asm, app: u32) {
    if a.x86() {
        a.x().mov_ri32(Gpr(RCX), app);
        a.call("_set_app_type");
        a.x().mov_ri32(Gpr(RCX), 1);
        a.call("_configure_narrow_argv");
        a.call("_initialize_narrow_environment");
        a.lea_rip(RCX, "__xc_a");
        a.lea_rip(RDX, "__xc_z");
    } else {
        a.movz(0, app);
        a.call("_set_app_type");
        a.movz(0, 1);
        a.call("_configure_narrow_argv");
        a.call("_initialize_narrow_environment");
        a.adr(0, "__xc_a");
        a.adr(1, "__xc_z");
    }
    a.call("_initterm");
}

/// `exit(<the result register>)`, not returning.
fn exit(a: &mut Asm) {
    if a.x86() {
        a.x().mov_st(Sz::D, Rm::r(RCX), Gpr(RAX));
        a.call("exit");
        a.x().int3();
    } else {
        a.call("exit");
        a.w(&[a64::brk(1)]);
    }
}

/// `mainCRTStartup`; with `gui`, `WinMainCRTStartup` and `WinMain` (which
/// ignores lpCmdLine: the runtime reads the command line itself, so it
/// gets "").
fn entry(x86: bool, gui: bool) -> Vec<Fn> {
    let mut a = Asm::arch(x86);
    a.prologue(32);
    if !gui {
        crt_setup(&mut a, 1);
        a.call("nabi_main");
        exit(&mut a);
        return vec![a.done("mainCRTStartup")];
    }
    crt_setup(&mut a, 2);
    if x86 {
        a.lea_rip(RCX, "__ImageBase");
        a.x().alu_rr(Alu::Xor, Sz::D, Gpr(RDX), Gpr(RDX));
        a.lea_rip(R8, ".nabi_empty");
        a.x().mov_ri32(Gpr(R9), 10); // SW_SHOWDEFAULT
    } else {
        a.adr(0, "__ImageBase");
        a.w(&[a64::movz(a64::R(1), 0, 0)]);
        a.adr(2, ".nabi_empty");
        a.movz(3, 10);
    }
    a.call("WinMain");
    exit(&mut a);
    let start = a.done("WinMainCRTStartup");
    let mut a = Asm::arch(x86);
    a.prologue(32);
    a.call("nabi_main");
    a.epilogue();
    vec![start, a.done("WinMain")]
}

/// `__chkstk`. x64: rax holds the frame size; touches each page below the
/// caller's rsp, keeping every register. arm64: x15 holds the frame size /
/// 16; touches each page below sp, keeping every register but x16 / x17.
fn chkstk(x86: bool) -> Fn {
    let mut a = Asm::arch(x86);
    if x86 {
        let x = a.x();
        let (rax, rcx) = (Gpr(RAX), Gpr(RCX));
        x.push(rcx);
        x.push(rax);
        x.alu_rax(Alu::Cmp, 0x1000);
        x.lea(rcx, Addr::Bd(Gpr(RSP), 24));
        let small = x.jcc8(Cond::B, None);
        let top = x.pos();
        x.alu_ri(Alu::Sub, Sz::Q, Rm::R(rcx), 0x1000);
        x.test_mr(Sz::Q, Addr::Bd(rcx, 0), rcx);
        x.alu_rax(Alu::Sub, 0x1000);
        x.alu_rax(Alu::Cmp, 0x1000);
        x.jcc8(Cond::A, Some(top));
        let at = x.pos();
        x.patch_rel8(small, at);
        x.alu_rr(Alu::Sub, Sz::Q, rcx, rax);
        x.test_mr(Sz::Q, Addr::Bd(rcx, 0), rcx);
        x.pop(rax);
        x.pop(rcx);
        x.ret();
    } else {
        let (x15, x16, x17) = (a64::R(15), a64::R(16), a64::R(17));
        a.w(&[
            a64::lsl(true, x16, x15, 4),
            a64::add_imm(AddOp::Add, true, x17, a64::SP, 0, false),
            // 1:
            a64::add_imm(AddOp::Sub, true, x17, x17, 1, true),
            a64::add_imm(AddOp::Subs, true, x16, x16, 1, true),
            a64::ldst(a64::LDRX, 31, x17, 0),
            a64::b_cond(a64::Cond::GT, -3),
            a64::ret(),
        ]);
    }
    Fn {
        unwind: false,
        ..a.done("__chkstk")
    }
}

/// How a `printf` wrapper calls UCRT's `__stdio_common_v*printf`.
#[derive(Clone, Copy)]
enum Printf {
    /// `printf(fmt, ...)`: `vfprintf(0, stdout, fmt, NULL, va)`.
    Stdout,
    /// `fprintf(f, fmt, ...)`: `vfprintf(0, f, fmt, NULL, va)`.
    File,
    /// `sprintf(b, fmt, ...)`: `vsprintf(1, b, -1, fmt, NULL, va)`
    /// (`_CRT_INTERNAL_PRINTF_LEGACY_VSPRINTF_NULL_TERMINATION`).
    Buffer,
    /// `snprintf(b, n, fmt, ...)`: `vsprintf(2, b, n, fmt, NULL, va)`
    /// (`_CRT_INTERNAL_PRINTF_STANDARD_SNPRINTF_BEHAVIOR`).
    Bounded,
}

/// A `printf` wrapper: the argument registers spilled where the stack
/// arguments continue them (`va_list` is a pointer on every Windows
/// arch), the call, a negative result as -1.
fn printf(x86: bool, kind: Printf) -> Fn {
    let (name, fixed): (_, u8) = match kind {
        Printf::Stdout => ("printf", 1),
        Printf::File => ("fprintf", 2),
        Printf::Buffer => ("sprintf", 2),
        Printf::Bounded => ("snprintf", 3),
    };
    let mut a = Asm::arch(x86);
    if x86 {
        // Shadow space, two stack arguments; the home area at rbp + 16.
        a.prologue(48);
        // `[rbp + 16 + 8 i]`, argument i's home.
        let home = |i: i32| Rm::m(RBP, 16 + 8 * i);
        let x = a.x();
        for (i, r) in [RCX, RDX, R8, R9].into_iter().enumerate() {
            x.mov_st(Sz::Q, home(i as i32), Gpr(r));
        }
        match kind {
            Printf::Stdout => {
                a.x().mov_ri32(Gpr(RCX), 1);
                a.call("__acrt_iob_func");
                let x = a.x();
                x.mov_st(Sz::Q, Rm::r(RDX), Gpr(RAX));
                x.mov(Sz::Q, Gpr(R8), home(0));
            }
            Printf::File => {
                x.mov(Sz::Q, Gpr(RDX), home(0));
                x.mov(Sz::Q, Gpr(R8), home(1));
            }
            Printf::Buffer => {
                x.mov(Sz::Q, Gpr(RDX), home(0));
                x.mov_ri_sx(Gpr(R8), -1);
                x.mov(Sz::Q, Gpr(R9), home(1));
            }
            Printf::Bounded => {
                x.mov(Sz::Q, Gpr(RDX), home(0));
                x.mov(Sz::Q, Gpr(R8), home(1));
                x.mov(Sz::Q, Gpr(R9), home(2));
            }
        }
        let va = home(i32::from(fixed));
        let Rm::M(va) = va else { unreachable!() };
        let x = a.x();
        x.lea(Gpr(RAX), va);
        match kind {
            Printf::Stdout | Printf::File => {
                x.alu_rr(Alu::Xor, Sz::D, Gpr(RCX), Gpr(RCX));
                x.alu_rr(Alu::Xor, Sz::D, Gpr(R9), Gpr(R9));
                x.mov_st(Sz::Q, Rm::m(RSP, 32), Gpr(RAX));
                a.call("__stdio_common_vfprintf");
            }
            Printf::Buffer | Printf::Bounded => {
                let opts = if matches!(kind, Printf::Buffer) { 1 } else { 2 };
                x.mov_ri32(Gpr(RCX), opts);
                x.mov_mi(Sz::Q, Rm::m(RSP, 32), 0);
                x.mov_st(Sz::Q, Rm::m(RSP, 40), Gpr(RAX));
                a.call("__stdio_common_vsprintf");
            }
        }
        let x = a.x();
        x.mov_ri32(Gpr(RCX), u32::MAX);
        x.test_rr(Sz::D, Gpr(RAX), Gpr(RAX));
        x.cmov(Cond::S, Sz::D, Gpr(RAX), Rm::r(RCX));
    } else {
        // The save area of x0..x7 just below the stack arguments, above
        // the frame record: slot i at x29 + 16 + 8i.
        let (sp, fp) = (a64::SP, a64::R(29));
        a.w(&[a64::add_imm(AddOp::Sub, true, sp, sp, 64, false)]);
        a.words(&crate::isa::a64::PROLOGUE);
        for i in 0..4 {
            let p = a64::pair(
                a64::Pair::StpX,
                a64::Index::Offset,
                2 * i,
                2 * i + 1,
                fp,
                2 + 2 * i as i32,
            );
            a.w(&[p]);
        }
        // `ldr x<rt>, [x29, #16 + 8 * slot]`.
        let slot = |rt: u32, slot: u32| a64::ldst(a64::LDRX, rt, fp, 2 + slot);
        let x = |n: u32| a64::R(n);
        match kind {
            Printf::Stdout => {
                a.movz(0, 1);
                a.call("__acrt_iob_func");
                a.w(&[
                    a64::mov(true, x(1), x(0)),
                    a64::movz(x(0), 0, 0),
                    slot(2, 0),
                    a64::movz(x(3), 0, 0),
                ]);
            }
            Printf::File => {
                a.w(&[
                    a64::movz(x(0), 0, 0),
                    slot(1, 0),
                    slot(2, 1),
                    a64::movz(x(3), 0, 0),
                ]);
            }
            Printf::Buffer => {
                a.w(&[
                    a64::movz(x(0), 1, 0),
                    slot(1, 0),
                    a64::movn(x(2), 0, 0),
                    slot(3, 1),
                    a64::movz(x(4), 0, 0),
                ]);
            }
            Printf::Bounded => {
                a.w(&[
                    a64::movz(x(0), 2, 0),
                    slot(1, 0),
                    slot(2, 1),
                    slot(3, 2),
                    a64::movz(x(4), 0, 0),
                ]);
            }
        }
        // `add x<rd>, x29, #va`.
        let va = 16 + 8 * u32::from(fixed);
        let add = |rd: u32| a64::add_imm(AddOp::Add, true, x(rd), fp, va, false);
        match kind {
            Printf::Stdout | Printf::File => {
                a.w(&[add(4)]);
                a.call("__stdio_common_vfprintf");
            }
            Printf::Buffer | Printf::Bounded => {
                a.w(&[add(5)]);
                a.call("__stdio_common_vsprintf");
            }
        }
        a.w(&[
            a64::cmp_imm(false, x(0), 0),
            a64::csinv(false, x(0), x(0), a64::ZR, a64::Cond::GE),
        ]);
        let at = a.pos();
        a.epilogs.push(at);
        a.w(&[
            a64::add_imm(AddOp::Add, true, sp, fp, 0, false),
            a64::pair(a64::Pair::LdpX, a64::Index::Post, 29, 30, sp, 2),
            a64::add_imm(AddOp::Add, true, sp, sp, 64, false),
            a64::ret(),
        ]);
        return Fn {
            varargs: true,
            comdat: true,
            ..a.done(name)
        };
    }
    a.epilogue();
    Fn {
        comdat: true,
        ..a.done(name)
    }
}

/// x86 `mainCRTStartup`, or `WinMainCRTStartup` and `WinMain`: as
/// [`entry`], cdecl, every argument on the stack.
fn entry_i386(gui: bool) -> Vec<Fn> {
    let mut a = Asm::i386();
    let frame = |a: &mut Asm| {
        let i = a.i();
        i.push(i386::EBP);
        i.mov_st(i386::Sz::D, i386::Rm::R(i386::EBP), i386::ESP);
    };
    frame(&mut a);
    a.i().push_i8(if gui { 2 } else { 1 });
    a.call32("__set_app_type");
    a.i().push_i8(1);
    a.call32("__configure_narrow_argv");
    a.call32("__initialize_narrow_environment");
    a.push32("___xc_z");
    a.push32("___xc_a");
    a.call32("__initterm");
    a.i().alu_ri8(i386::Alu::Add, i386::Rm::R(i386::ESP), 16);
    let exit = |a: &mut Asm| {
        a.i().push(i386::EAX);
        a.call32("_exit");
        a.i().int3();
    };
    if !gui {
        a.call32("_nabi_main");
        exit(&mut a);
        return vec![a.done("_mainCRTStartup")];
    }
    // WinMain(__ImageBase, NULL, "", SW_SHOWDEFAULT), stdcall.
    a.i().push_i8(10);
    a.push32(".nabi_empty");
    a.i().push_i8(0);
    a.push32("___ImageBase");
    a.call32("_WinMain@16");
    exit(&mut a);
    let start = a.done("_WinMainCRTStartup");
    let mut a = Asm::i386();
    frame(&mut a);
    a.call32("_nabi_main");
    let i = a.i();
    i.mov_st(i386::Sz::D, i386::Rm::R(i386::ESP), i386::EBP);
    i.pop(i386::EBP);
    i.ret_n(16);
    vec![start, a.done("_WinMain@16")]
}

/// x86 `__chkstk` (also `__alloca_probe`), MSVC's: eax holds the frame
/// size; touches each page from the caller's esp down to esp - eax, then
/// moves esp there. Keeps every register but eax.
fn chkstk_i386() -> Fn {
    let mut a = Asm::i386();
    let i = a.i();
    i.push(i386::ECX);
    i.lea(i386::ECX, i386::Addr::Bd(i386::ESP, 4));
    i.alu(
        i386::Alu::Sub,
        i386::Sz::D,
        i386::Rm::R(i386::ECX),
        i386::EAX,
    );
    i.alu(
        i386::Alu::Sbb,
        i386::Sz::D,
        i386::Rm::R(i386::EAX),
        i386::EAX,
    );
    i.un(i386::Un::Not, i386::Rm::R(i386::EAX));
    // 0 on wrap: faults on the guard page.
    i.alu(
        i386::Alu::And,
        i386::Sz::D,
        i386::Rm::R(i386::ECX),
        i386::EAX,
    );
    i.mov_st(i386::Sz::D, i386::Rm::R(i386::EAX), i386::ESP);
    i.alu_eax(i386::Alu::And, 0xffff_f000);
    let top = i.pos();
    i.alu(
        i386::Alu::Cmp,
        i386::Sz::D,
        i386::Rm::R(i386::ECX),
        i386::EAX,
    );
    let next = i.jcc8(i386::Cond::B, None);
    i.mov_st(i386::Sz::D, i386::Rm::R(i386::EAX), i386::ECX);
    i.pop(i386::ECX);
    i.xchg_eax(i386::ESP);
    i.mov(
        i386::Sz::D,
        i386::EAX,
        i386::Rm::M(i386::Addr::Bd(i386::EAX, 0)),
    );
    i.mov_st(
        i386::Sz::D,
        i386::Rm::M(i386::Addr::Bd(i386::ESP, 0)),
        i386::EAX,
    );
    i.ret();
    let at = i.pos();
    i.patch_rel8(next, at);
    i.alu_eax(i386::Alu::Sub, 0x1000);
    i.test(
        i386::Sz::D,
        i386::Rm::M(i386::Addr::Bd(i386::EAX, 0)),
        i386::EAX,
    );
    i.jmp8(Some(top));
    Fn {
        unwind: false,
        ..a.done("__chkstk")
    }
}

/// An x86 `printf` wrapper (see [`printf`]): the stack arguments after
/// the fixed ones are the `va_list`; the 64-bit options take two words.
fn printf_i386(kind: Printf) -> Fn {
    let (name, fixed) = match kind {
        Printf::Stdout => ("_printf", 1),
        Printf::File => ("_fprintf", 2),
        Printf::Buffer => ("_sprintf", 2),
        Printf::Bounded => ("_snprintf", 3),
    };
    // `[ebp + 8 + 4 * i]`, argument i.
    let arg = |i: i32| i386::Addr::Bd(i386::EBP, 8 + 4 * i);
    let mut a = Asm::i386();
    a.i().push(i386::EBP);
    a.i().mov_st(i386::Sz::D, i386::Rm::R(i386::EBP), i386::ESP);
    if matches!(kind, Printf::Stdout) {
        a.i().push_i8(1);
        a.call32("___acrt_iob_func");
        a.i().alu_ri8(i386::Alu::Add, i386::Rm::R(i386::ESP), 4);
    }
    let i = a.i();
    i.lea(i386::ECX, arg(fixed));
    i.push(i386::ECX);
    i.push_i8(0); // locale
    match kind {
        Printf::Stdout => {
            i.push_m(arg(0));
            i.push(i386::EAX);
        }
        Printf::File => {
            i.push_m(arg(1));
            i.push_m(arg(0));
        }
        Printf::Buffer => {
            i.push_m(arg(1));
            i.push_i8(-1);
            i.push_m(arg(0));
        }
        Printf::Bounded => {
            i.push_m(arg(2));
            i.push_m(arg(1));
            i.push_m(arg(0));
        }
    }
    let opts = match kind {
        Printf::Stdout | Printf::File => 0,
        Printf::Buffer => 1,
        Printf::Bounded => 2,
    };
    i.push_i8(0);
    i.push_i8(opts);
    a.call32(if opts == 0 {
        "___stdio_common_vfprintf"
    } else {
        "___stdio_common_vsprintf"
    });
    let i = a.i();
    i.mov_ri(i386::ECX, u32::MAX);
    i.test(i386::Sz::D, i386::Rm::R(i386::EAX), i386::EAX);
    i.cmov(i386::Cond::S, i386::EAX, i386::Rm::R(i386::ECX));
    i.mov_st(i386::Sz::D, i386::Rm::R(i386::ESP), i386::EBP);
    i.pop(i386::EBP);
    i.ret();
    Fn {
        comdat: true,
        ..a.done(name)
    }
}

/// A section of the object.
struct Section {
    name: &'static str,
    flags: u32,
    /// log2 of the alignment.
    align: u32,
    data: Vec<u8>,
    relocs: Vec<(u32, u16, String)>,
    /// `(selection, associated section index)` of a COMDAT.
    comdat: Option<(u8, usize)>,
}

impl Section {
    const fn new(name: &'static str, flags: u32, align: u32, data: Vec<u8>) -> Self {
        Self {
            name,
            flags,
            align,
            data,
            relocs: Vec::new(),
            comdat: None,
        }
    }
}

/// A symbol: name, section index (`None`: undefined), value, function,
/// external.
type Sym = (String, Option<usize>, u32, bool, bool);

/// The startup object of a Windows `arch` (`x86_64`, `aarch64`, `x86`)
/// program; `gui`: entered through `WinMainCRTStartup` and `WinMain` (`-C
/// subsystem=windows`), else `mainCRTStartup`.
pub fn startup(arch: &str, gui: bool) -> Result<Vec<u8>, String> {
    object(arch, Some(gui))
}

/// The startup object of a DLL: the TLS directory, `_fltused`,
/// `__chkstk` and the `printf`s, no program entry (the runtime's
/// `nabi_dll_main` is the DLL's).
pub fn dll_startup(arch: &str) -> Result<Vec<u8>, String> {
    object(arch, None)
}

/// `entry`: the program entry, `Some(gui)`; `None` for a DLL.
#[allow(clippy::too_many_lines)]
fn object(arch: &str, entry_gui: Option<bool>) -> Result<Vec<u8>, String> {
    let gui = entry_gui.unwrap_or(false);
    let (x86, i386) = match arch {
        "x86_64" => (true, false),
        "aarch64" => (false, false),
        "x86" => (true, true),
        _ => return Err(format!("no Windows startup code for {arch}")),
    };
    let (addr64, addr32nb) = if i386 {
        (I386_DIR32, 0)
    } else if x86 {
        (AMD64_ADDR64, AMD64_ADDR32NB)
    } else {
        (ARM64_ADDR64, ARM64_ADDR32NB)
    };
    let kinds = [
        Printf::Stdout,
        Printf::File,
        Printf::Buffer,
        Printf::Bounded,
    ];
    let fns = if i386 {
        let mut fns = if entry_gui.is_some() {
            entry_i386(gui)
        } else {
            Vec::new()
        };
        fns.push(chkstk_i386());
        fns.extend(kinds.map(printf_i386));
        fns
    } else {
        let mut fns = if entry_gui.is_some() {
            entry(x86, gui)
        } else {
            Vec::new()
        };
        fns.push(chkstk(x86));
        fns.extend(kinds.map(|k| printf(x86, k)));
        fns
    };
    // A pointer: log2 size; C names: x86's `_`.
    let ptr = if i386 { 2 } else { 3 };
    let c = |n: &str| if i386 { format!("_{n}") } else { n.to_owned() };

    let mut sects: Vec<Section> = Vec::new();
    let mut syms: Vec<Sym> = Vec::new();
    let mut data = |sects: &mut Vec<Section>, name, flags, align, bytes: Vec<u8>, sym: &str| {
        sects.push(Section::new(name, flags, align, bytes));
        syms.push((c(sym), Some(sects.len() - 1), 0, false, true));
        sects.len() - 1
    };
    data(&mut sects, ".data", DATA, 2, vec![0; 4], "_tls_index");
    // Tells the linker the program uses floating point.
    data(
        &mut sects,
        ".data",
        DATA,
        2,
        39029u32.to_le_bytes().to_vec(),
        "_fltused",
    );
    let p = 1usize << ptr;
    data(&mut sects, ".tls", DATA, 0, vec![0], "_tls_start");
    data(&mut sects, ".tls$ZZZ", DATA, 0, vec![0], "_tls_end");
    data(&mut sects, ".CRT$XLA", DATA, ptr, vec![0; p], "__xl_a");
    data(&mut sects, ".CRT$XLZ", DATA, ptr, vec![0; p], "__xl_z");
    data(&mut sects, ".CRT$XCA", RDATA, ptr, vec![0; p], "__xc_a");
    data(&mut sects, ".CRT$XCZ", RDATA, ptr, vec![0; p], "__xc_z");
    // IMAGE_TLS_DIRECTORY64 / 32: the template's start and end, the
    // index's address, the callbacks (after `__xl_a`), zero fill,
    // characteristics.
    let mut dir = vec![0u8; 4 * p + 8];
    dir[3 * p] = p as u8;
    let tls_used = data(&mut sects, ".rdata$T", RDATA, ptr, dir, "_tls_used");
    sects[tls_used].relocs = ["_tls_start", "_tls_end", "_tls_index", "__xl_a"]
        .into_iter()
        .enumerate()
        .map(|(k, s)| ((k * p) as u32, addr64, c(s)))
        .collect();
    if gui {
        sects.push(Section::new(".rdata", RDATA, 0, vec![0]));
        syms.push((
            ".nabi_empty".to_owned(),
            Some(sects.len() - 1),
            0,
            false,
            false,
        ));
    }

    // Code: the entry and `__chkstk` in one section, each `printf` in a
    // COMDAT of its own; unwind info beside it, associated with a COMDAT.
    let text = sects.len();
    sects.push(Section::new(".text", CODE, 4, Vec::new()));
    let mut xdata_shared = None;
    let mut pdata = Section::new(".pdata", RDATA, 2, Vec::new());
    for f in &fns {
        let sect = if f.comdat {
            let mut s = Section::new(".text", CODE | LNK_COMDAT, 4, Vec::new());
            s.comdat = Some((SELECT_ANY, 0));
            sects.push(s);
            sects.len() - 1
        } else {
            text
        };
        let s = &mut sects[sect];
        while s.data.len() % 16 != 0 {
            s.data.push(if x86 { 0xcc } else { 0 });
        }
        let at = s.data.len() as u32;
        s.data.extend_from_slice(&f.code);
        s.relocs
            .extend(f.relocs.iter().map(|&(o, t, y)| (at + o, t, y.to_owned())));
        syms.push((f.name.to_owned(), Some(sect), at, true, true));
        if f.name == "__chkstk" && i386 {
            syms.push(("__alloca_probe".to_owned(), Some(sect), at, true, true));
        }
        if !f.unwind || i386 {
            continue;
        }
        // The unwind record this function's `.pdata` entry names.
        let xdata_sym = format!(".nabi_xdata.{}", f.name);
        if x86 {
            if xdata_shared.is_none() {
                sects.push(Section::new(".xdata", RDATA, 2, X64_UNWIND.to_vec()));
                xdata_shared = Some(sects.len() - 1);
                syms.push((".nabi_xdata".to_owned(), xdata_shared, 0, false, false));
            }
        } else {
            let mut x = Section::new(".xdata", RDATA, 2, xdata(f));
            if f.comdat {
                x.flags |= LNK_COMDAT;
                x.comdat = Some((SELECT_ASSOCIATIVE, sect));
            }
            sects.push(x);
            syms.push((xdata_sym.clone(), Some(sects.len() - 1), 0, false, false));
        }
        let xdata_sym = if x86 {
            ".nabi_xdata".to_owned()
        } else {
            xdata_sym
        };
        let len = f.code.len() as u32;
        let entry: Vec<(u32, String)> = if x86 {
            vec![
                (at, f.name.to_owned()),
                (at + len, f.name.to_owned()),
                (0, xdata_sym),
            ]
        } else {
            vec![(at, f.name.to_owned()), (0, xdata_sym)]
        };
        // A COMDAT's entry goes with it; the rest share one section.
        let p = if f.comdat {
            let mut p = Section::new(".pdata", RDATA | LNK_COMDAT, 2, Vec::new());
            p.comdat = Some((SELECT_ASSOCIATIVE, sect));
            sects.push(p);
            sects.len() - 1
        } else {
            usize::MAX
        };
        let target = if p == usize::MAX {
            &mut pdata
        } else {
            &mut sects[p]
        };
        for (value, sym) in entry {
            // Relative to the symbol: the function's offset is in the
            // symbol, not the place.
            let value = if sym == f.name { value - at } else { value };
            let off = target.data.len() as u32;
            target.data.extend_from_slice(&value.to_le_bytes());
            target.relocs.push((off, addr32nb, sym));
        }
    }
    if !pdata.data.is_empty() {
        sects.push(pdata);
    }
    let machine = if i386 {
        MACHINE_I386
    } else if x86 {
        MACHINE_AMD64
    } else {
        MACHINE_ARM64
    };
    Ok(write(machine, &sects, &syms))
}

/// arm64 `.xdata` of `f`: an epilog scope per epilog, the codes.
fn xdata(f: &Fn) -> Vec<u8> {
    let words = f.code.len() as u32 / 4;
    let n = f.epilogs.len() as u32;
    let mut x = Vec::new();
    x.extend_from_slice(&(words | (n << 22) | (1 << 27)).to_le_bytes());
    for e in &f.epilogs {
        x.extend_from_slice(&(e / 4).to_le_bytes());
    }
    x.extend_from_slice(if f.varargs {
        &A64_CODES_VARARGS
    } else {
        &A64_CODES
    });
    x
}

fn put16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

/// Symbol records and the string table of the longer names.
#[derive(Default)]
struct SymTab {
    recs: Vec<u8>,
    strs: Vec<u8>,
}

impl SymTab {
    /// A record (`aux` auxiliary ones follow); its index.
    fn add(&mut self, name: &str, value: u32, sect: u16, ty: u16, class: u8, aux: u8) -> u32 {
        let at = self.len();
        let b = &mut self.recs;
        if name.len() <= 8 {
            let mut n = [0u8; 8];
            n[..name.len()].copy_from_slice(name.as_bytes());
            b.extend_from_slice(&n);
        } else {
            put32(b, 0);
            put32(b, self.strs.len() as u32 + 4);
            self.strs.extend_from_slice(name.as_bytes());
            self.strs.push(0);
        }
        put32(b, value);
        put16(b, sect);
        put16(b, ty);
        b.push(class);
        b.push(aux);
        at
    }

    fn len(&self) -> u32 {
        (self.recs.len() / 18) as u32
    }
}

/// Serialize: header, section headers, data and relocations, symbols
/// (x86's absolute ones, each section's, then the named ones), strings.
fn write(machine: u16, sects: &[Section], syms: &[Sym]) -> Vec<u8> {
    let mut tab = SymTab::default();
    if machine == MACHINE_I386 {
        // Bit 0: safe for `/safeseh` (no handlers here).
        tab.add("@feat.00", 1, 0xffff, 0, CLASS_STATIC, 0);
        // The TEB's TLS array pointer, at fs:[__tls_array] (C compilers'
        // implicit TLS).
        tab.add("__tls_array", 0x2c, 0xffff, 0, CLASS_EXTERNAL, 0);
    }
    // Each section's symbol, then an any-copy COMDAT's leader.
    let mut leaders: Vec<Option<usize>> = vec![None; sects.len()];
    for (i, (_, sect, ..)) in syms.iter().enumerate() {
        if let Some(s) = *sect {
            if sects[s].comdat.is_some_and(|c| c.0 == SELECT_ANY) && leaders[s].is_none() {
                leaders[s] = Some(i);
            }
        }
    }
    let ty = |func: bool| if func { DTYPE_FUNCTION } else { 0 };
    let mut index = vec![0u32; syms.len()];
    for (k, s) in sects.iter().enumerate() {
        let n = k as u16 + 1;
        tab.add(s.name, 0, n, 0, CLASS_STATIC, 1);
        let mut aux = [0u8; 18];
        aux[..4].copy_from_slice(&(s.data.len() as u32).to_le_bytes());
        aux[4..6].copy_from_slice(&(s.relocs.len() as u16).to_le_bytes());
        if let Some((sel, assoc)) = s.comdat {
            if sel == SELECT_ASSOCIATIVE {
                aux[12..14].copy_from_slice(&(assoc as u16 + 1).to_le_bytes());
            }
            aux[14] = sel;
        }
        tab.recs.extend_from_slice(&aux);
        if let Some(l) = leaders[k] {
            let (name, _, value, func, _) = &syms[l];
            index[l] = tab.add(name, *value, n, ty(*func), CLASS_EXTERNAL, 0);
        }
    }
    for (i, (name, sect, value, func, external)) in syms.iter().enumerate() {
        if leaders.contains(&Some(i)) {
            continue;
        }
        let class = if *external {
            CLASS_EXTERNAL
        } else {
            CLASS_STATIC
        };
        let sect = sect.map_or(0, |s| s as u16 + 1);
        index[i] = tab.add(name, *value, sect, ty(*func), class, 0);
    }
    // Relocations; the symbols they name that are not here, undefined.
    let mut undefined: Vec<(String, u32)> = Vec::new();
    let mut relocs: Vec<Vec<(u32, u16, u32)>> = Vec::new();
    for s in sects {
        let mut rs = Vec::new();
        for (off, kind, sym) in &s.relocs {
            let i = if let Some(i) = syms.iter().position(|y| y.0 == *sym) {
                index[i]
            } else if let Some((_, i)) = undefined.iter().find(|u| u.0 == *sym) {
                *i
            } else {
                let i = tab.add(sym, 0, 0, 0, CLASS_EXTERNAL, 0);
                undefined.push((sym.clone(), i));
                i
            };
            rs.push((*off, *kind, i));
        }
        relocs.push(rs);
    }

    let mut b = Vec::new();
    put16(&mut b, machine);
    put16(&mut b, sects.len() as u16);
    put32(&mut b, 0);
    let symoff_at = b.len();
    put32(&mut b, 0);
    put32(&mut b, tab.len());
    put16(&mut b, 0);
    put16(&mut b, 0);
    let headers = b.len();
    b.resize(headers + 40 * sects.len(), 0);
    for (k, s) in sects.iter().enumerate() {
        b.resize(b.len().next_multiple_of(1 << s.align.min(4)), 0);
        let data = b.len() as u32;
        b.extend_from_slice(&s.data);
        let rel = b.len() as u32;
        for &(off, ty, sym) in &relocs[k] {
            put32(&mut b, off);
            put32(&mut b, sym);
            put16(&mut b, ty);
        }
        let mut h = Vec::with_capacity(40);
        let mut name = [0u8; 8];
        name[..s.name.len()].copy_from_slice(s.name.as_bytes());
        h.extend_from_slice(&name);
        put32(&mut h, 0);
        put32(&mut h, 0);
        put32(&mut h, s.data.len() as u32);
        put32(&mut h, data);
        put32(&mut h, if relocs[k].is_empty() { 0 } else { rel });
        put32(&mut h, 0);
        put16(&mut h, relocs[k].len() as u16);
        put16(&mut h, 0);
        // `IMAGE_SCN_ALIGN_<n>BYTES`: log2 + 1 in bits 20..24.
        put32(&mut h, s.flags | ((s.align + 1) << 20));
        b[headers + 40 * k..headers + 40 * (k + 1)].copy_from_slice(&h);
    }
    let symoff = b.len() as u32;
    b[symoff_at..symoff_at + 4].copy_from_slice(&symoff.to_le_bytes());
    b.extend_from_slice(&tab.recs);
    put32(&mut b, tab.strs.len() as u32 + 4);
    b.extend_from_slice(&tab.strs);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The symbol names of COFF object `b`, as `(name, section)`.
    fn symbols(b: &[u8]) -> Vec<(String, i16)> {
        let u32_at = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        let (symoff, n) = (u32_at(8), u32_at(12));
        let strs = symoff + 18 * n;
        let mut out = Vec::new();
        let mut i = 0;
        while i < n {
            let r = &b[symoff + 18 * i..symoff + 18 * (i + 1)];
            let name = if r[..4] == [0; 4] {
                let at = strs + u32::from_le_bytes(r[4..8].try_into().unwrap()) as usize;
                let end = b[at..].iter().position(|&c| c == 0).unwrap();
                String::from_utf8_lossy(&b[at..at + end]).into_owned()
            } else {
                let end = r[..8].iter().position(|&c| c == 0).unwrap_or(8);
                String::from_utf8_lossy(&r[..end]).into_owned()
            };
            out.push((name, i16::from_le_bytes([r[12], r[13]])));
            i += 1 + r[17] as usize;
        }
        out
    }

    /// Each arch and subsystem: the entry, the TLS directory, `__chkstk`
    /// and the `printf`s defined; the CRT's functions undefined.
    #[test]
    fn startup_defines_the_crt_parts() {
        for (arch, machine) in [
            ("x86_64", MACHINE_AMD64),
            ("aarch64", MACHINE_ARM64),
            ("x86", MACHINE_I386),
        ] {
            let c = |n: &str| {
                if arch == "x86" && n != "__chkstk" {
                    format!("_{n}")
                } else {
                    n.to_owned()
                }
            };
            for gui in [false, true] {
                let b = startup(arch, gui).unwrap();
                if let Some(dir) = std::env::var_os("NABI_DUMP_WINSTART") {
                    let p = std::path::Path::new(&dir).join(format!("{arch}-{gui}.obj"));
                    std::fs::write(p, &b).unwrap();
                }
                assert_eq!(u16::from_le_bytes([b[0], b[1]]), machine);
                let syms = symbols(&b);
                let defined = |n: &str| syms.iter().any(|(s, k)| s == n && *k > 0);
                let undefined = |n: &str| syms.iter().any(|(s, k)| s == n && *k == 0);
                let entry = if gui {
                    "WinMainCRTStartup"
                } else {
                    "mainCRTStartup"
                };
                for n in [
                    entry,
                    "_tls_used",
                    "_tls_index",
                    "_fltused",
                    "__chkstk",
                    "printf",
                    "snprintf",
                ] {
                    assert!(defined(&c(n)), "{arch} {gui}: {n} undefined");
                }
                for n in ["nabi_main", "_initterm", "exit", "__stdio_common_vsprintf"] {
                    assert!(undefined(&c(n)), "{arch} {gui}: {n} not undefined");
                }
                let win_main = if arch == "x86" {
                    "_WinMain@16"
                } else {
                    "WinMain"
                };
                assert_eq!(gui, defined(win_main));
                assert_eq!(arch == "x86", defined("__alloca_probe"));
                assert_eq!(
                    arch == "x86",
                    syms.iter().any(|(s, k)| s == "__tls_array" && *k == -1)
                );
            }
        }
        assert!(startup("riscv64", false).is_err());
    }
}
