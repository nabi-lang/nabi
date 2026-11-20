//! Mid-level IR. SSA basic blocks; the last representation before
//! native code generation. Looks at HIR only.

pub mod plugin;
pub mod summary;

mod codec;
mod coro;
mod inplace;
mod liveness;
mod lower;
mod names;
mod opt;
mod partial;
mod print;
mod rotate;
mod specialize;
mod strip;
mod target;
mod ty;
mod version;

pub use liveness::{
    Deaths, addressed, addressed_by, block_local, deaths, live_across, live_out_of,
    maybe_unassigned, stmt_reads, stmt_uses, term_reads, term_uses,
};
pub use lower::lower;
pub use nabir_abi::{gc as gc_abi, tls as tls_abi, typedesc as typedesc_abi};
pub use opt::{Memo, Opt, optimize, optimize_with};
pub use partial::{PartStore, part_stores};
pub use print::print_mir;
pub use rotate::{back_edges, noreturn_call, short_loops, succs};
pub use target::{CAbi, CKind, GcSlot, ObjectFormat, StackProbes, Target, TargetSpec};
pub use ty::{ANY_STRUCT, any_ty};

use nabic_core::DefId;
use nabic_diagnostic::Span;
use nabic_hir::StructId;
use nabic_ir_common::{Abi, BlockId, ConstVal, FnFacts, FunctionId, LocalId, PrimOp, PrimTy};

#[derive(Clone, Debug, Default)]
pub struct MirModule {
    pub functions: Vec<MirFunction>,
    pub statics: Vec<MirStaticDef>,
    /// Struct name and field names by `StructId` (`-g` DWARF types).
    pub struct_names: nabic_core::HashMap<u32, (String, Vec<String>)>,
    /// Each struct's build-independent name by `StructId` (the ids are
    /// handed out in lowering order): what incremental keys name it by.
    pub struct_keys: nabic_core::HashMap<u32, String>,
    /// `-g` types the `MirTy`s do not tell.
    pub debug: DebugTypes,
    pub target: Target,
    /// The packages of the program and how they link (filled by the
    /// driver).
    pub link: Link,
    /// The types reflection describes; each one's address is the
    /// extern static `symbol`, which the glue defines.
    pub type_descs: Vec<nabic_ir_common::typedesc::TypeDesc<MirTy>>,
    /// A plugin's or plugin host's tables.
    pub plugin: plugin::Plugin,
}

/// Whether `-g` describes the local named `name`: not one lowering made
/// (`#..`) or a desugaring's temporary (`__..`, as `__env`,
/// `__nabi_slice_target`).
#[must_use]
pub fn debug_visible(name: &str) -> bool {
    !name.starts_with('#') && !name.starts_with("__")
}

/// What `-g` shows of a value beyond its `MirTy`.
#[derive(Clone, Debug, PartialEq)]
pub enum DebugTy {
    /// As its `MirTy` says.
    Mir(MirTy),
    /// A sum or enum laid out `(i32 tag, payload)` (`.1`), or a `throws`
    /// carrier `(is_err, payload)`: its name and its variants, `(stored
    /// tag, name, payload)` (`()`: no data).
    Sum(String, MirTy, Vec<(u32, String, DebugTy)>),
    /// A reference to a `GcBuf` object (its name) of these elements.
    Buf(String, Box<DebugTy>),
    /// `any Tr` (`any_ty`): a variant part over [`DebugTypes::any`].
    Any,
    /// A reference to a GC object holding one value after its header (an
    /// `any`'s boxed copy, a closure's environment), named for the
    /// debugger (which takes same-named types of a size for one).
    Boxed(String, Box<DebugTy>),
    /// A `[ref]`-captured variable's cell (a local only): shown as the
    /// value in it.
    Cell(Box<DebugTy>),
    /// A function value (this fat `fn` type), named for the debugger,
    /// with its environment: the object (a `Boxed`) of a closure whose
    /// type is known, else `GcRef`.
    Closure(String, MirTy, Box<DebugTy>),
}

impl DebugTy {
    /// The `MirTy` of the values it describes.
    #[must_use]
    pub fn mir(&self) -> MirTy {
        match self {
            Self::Mir(m) | Self::Sum(_, m, _) | Self::Closure(_, m, _) => m.clone(),
            Self::Buf(..) | Self::Boxed(..) | Self::Cell(_) => MirTy::GcRef,
            Self::Any => crate::any_ty(),
        }
    }

    /// Whether it describes a `ty` value.
    #[must_use]
    pub fn fits(&self, ty: &MirTy) -> bool {
        self.mir() == *ty
    }
}

impl MirTy {
    /// The discriminant type of a [`DebugTy::Sum`] laid out as `self`:
    /// a carrier's `bool` tag is a byte, any other tag the `i32` it is
    /// (gdb sign-extends a 4-byte `DW_AT_discr_value`, so an `any` tag
    /// with the high bit set matches only a signed discriminant).
    #[must_use]
    pub fn debug_tag(&self) -> PrimTy {
        match self {
            Self::Tuple(ps) if ps.first() == Some(&Self::Prim(PrimTy::Bool)) => PrimTy::U8,
            _ => PrimTy::I32,
        }
    }
}

/// The `DebugTy`s of named locals and struct fields whose `MirTy`
/// loses something (a sum's variants, a GC reference's pointee).
#[derive(Clone, Debug, Default)]
pub struct DebugTypes {
    pub locals: nabic_core::HashMap<(FunctionId, LocalId), DebugTy>,
    /// Every field's, by `StructId`.
    pub fields: nabic_core::HashMap<u32, Vec<DebugTy>>,
    /// The types stored into `any` slots, `(stored tag, name, payload)`:
    /// the payload word is the value, or [`DebugTy::Boxed`] (the tag's
    /// `ANY_BOXED` bit set).
    pub any: Vec<(u32, String, DebugTy)>,
}

/// The packages items belong to, and how this module links with
/// prebuilt dependencies (their `lib<name>.a` + `<name>.nabihir`).
#[derive(Clone, Debug, Default)]
pub struct Link {
    /// Package hash per item-path prefix (`core`, ..., `""` for the
    /// user package), longest prefix first. Empty: one hash
    /// over the printed MIR.
    pub packages: Vec<(String, String)>,
    /// This module is package `lib`'s prebuilt library: only that
    /// package's functions and statics are exported, the rest of the
    /// code it uses (library, instances) is a private copy.
    pub lib: Option<String>,
    /// Packages loaded from NABIHIR: their code and statics live in their
    /// `.a`; bodies compiled here (generics, kept bodies) are private.
    pub prebuilt: Vec<String>,
    /// Added to every GC type id: the ids below belong to the prebuilt
    /// dependencies.
    pub type_offset: u32,
    /// `#[link(..)]` libraries of `extern` blocks:
    /// `(name, static)`, in first-seen order.
    pub native_libs: Vec<(String, bool)>,
    /// `#[link(name, kind = "framework")]`: Apple frameworks, in
    /// first-seen order.
    pub frameworks: Vec<String>,
    /// `ffi::wit`: what the component's link needs.
    pub wit: Option<WitLink>,
    /// A C translation unit (`nabi cc`): its functions are `FnRole::C`,
    /// its statics C objects, and it needs no runtime glue.
    pub c: bool,
}

/// The WIT of a component (`wasm32-wasip2`): the linker gets a directory
/// holding `world` and, under `deps/`, the WIT sources it refers to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WitLink {
    /// A WIT package with the one world the component has.
    pub world: String,
    /// The package sources: `.wit` files or directories.
    pub sources: Vec<WitSource>,
    /// Symbols the core module exports (functions, post-returns,
    /// destructors, `cabi_realloc`).
    pub exports: Vec<String>,
    /// A reactor: no `main`, so no `wasi:cli/run` (only the exports).
    pub reactor: bool,
}

/// One WIT package source, read: a `.wit` file or a directory of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WitSource {
    /// The package whose `ffi::wit` read it first.
    pub pkg: String,
    pub path: std::path::PathBuf,
    pub dir: bool,
    /// Its files: `(path, text)`.
    pub files: Vec<(std::path::PathBuf, String)>,
}

impl Link {
    /// Item `path` belongs to package `pkg` (a `::`-path prefix).
    #[must_use]
    pub fn owns(pkg: &str, path: &str) -> bool {
        path.strip_prefix(pkg).is_some_and(|r| r.starts_with("::"))
    }

    /// The hash of the package item `path` belongs to.
    #[must_use]
    pub fn hash_of(&self, path: &str) -> &str {
        self.packages
            .iter()
            .find(|(p, _)| p.is_empty() || Self::owns(p, path))
            .map_or("", |(_, h)| h.as_str())
    }

    /// Item `path` belongs to a prebuilt dependency.
    #[must_use]
    pub fn in_prebuilt(&self, path: &str) -> bool {
        self.prebuilt.iter().any(|d| Self::owns(d, path))
    }

    /// Static `name` is the standard library's, in a library: defined weak
    /// under its own symbol, so the program and every library linked
    /// with it share one.
    #[must_use]
    pub fn shared_static(&self, name: &str) -> bool {
        self.lib.is_some() && nabic_core::in_toolchain(name)
    }
}

#[derive(Clone, Debug)]
pub struct MirStaticDef {
    pub name: String,
    pub ty: MirTy,
    pub init: ConstVal,
    pub is_mut: bool,
    /// `extern "C" { static .. }`: defined outside, keeps its C name.
    pub is_extern: bool,
    /// `HirStaticDef::dll_import`.
    pub dll_import: bool,
    /// Lazily initialized by this function on first access.
    pub init_fn: Option<FunctionId>,
    /// A C object (`nabi cc`): defined under its own name with these
    /// bytes; `init` is not read.
    pub c: Option<CData>,
}

/// The bytes of a C object, with the addresses in them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CData {
    /// Seen by other objects (not `static`).
    pub global: bool,
    pub bytes: Vec<u8>,
    pub relocs: Vec<CReloc>,
    /// Thread-local (`_Thread_local`): declared only, reached through
    /// the thread's block.
    pub tls: bool,
}

/// An address at byte `at` of a [`CData`]: `to`'s plus `addend`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CReloc {
    pub at: u32,
    pub to: CRef,
    pub addend: i64,
}

/// What a [`CReloc`] points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CRef {
    Static(String),
    Fn(FunctionId),
}

#[derive(Clone, Debug)]
pub struct MirFunction {
    pub id: FunctionId,
    pub def_id: Option<DefId>,
    pub name: String,
    pub params: Vec<LocalId>,
    pub locals: Vec<LocalDecl>,
    pub blocks: Vec<BasicBlock>,
    pub param_tys: Vec<MirTy>,
    pub ret_ty: MirTy,
    pub span: Span,
    pub facts: FnFacts,
    /// Loop headers whose loop `strip` bounded to `SHORT_TRIPS`
    /// iterations a time (`short_loops`).
    pub strips: Vec<u32>,
    /// In a function where a `bare::ptr` value can appear: its locals
    /// holding GC references. Each stays rooted (so in place) until its
    /// scope ends, not only until its last use, and a coroutine keeps it
    /// in its frame as a pinning word (`str::as_ptr`).
    pub pins: Vec<LocalId>,
    /// What `-g` keeps of it through optimization.
    pub dbg: FnDbg,
}

/// What optimization leaves `-g` of a function: the named locals whose
/// assignments went and the bodies inlined into it. Empty below `-O1`.
#[derive(Clone, Debug, Default)]
pub struct FnDbg {
    pub binds: Vec<Bind>,
    pub inlined: Vec<Inlined>,
    /// Locals an inlined body's result is assigned to: where the call
    /// is (line, scope), not the body.
    pub decls: Vec<(LocalId, u32, Scope)>,
}

/// A named local no assignment of which is left.
#[derive(Clone, Debug)]
pub struct Bind {
    pub local: LocalId,
    /// What it holds wherever it would: a constant, or a local assigned
    /// once (or a parameter never assigned). `None`: optimized out.
    pub value: Option<Operand>,
    /// Else, an aggregate built of these (by field, `None`: gone).
    pub parts: Vec<Option<Operand>>,
    /// Where it was assigned.
    pub line: u32,
    pub scope: Scope,
}

/// A body inlined: the positions of the copy are in scope `scope` (its
/// body's scope 0) or in scopes within it.
#[derive(Clone, Debug)]
pub struct Inlined {
    pub scope: u32,
    /// The scope of the call.
    pub parent: u32,
    /// The callee's item path and declaration.
    pub name: String,
    pub decl: Span,
    /// The call's position.
    pub call: Span,
    /// The copies of the callee's parameters, in order.
    pub params: Vec<LocalId>,
}

impl MirFunction {
    /// Its item path (a generic instance's: the generic's).
    #[must_use]
    pub fn path(&self) -> &str {
        self.facts.path(&self.name)
    }

    /// The return type `-g` gives it: a scalar's, which every
    /// convention returns in a register, so a debugger reads it (`finish`).
    #[must_use]
    pub fn debug_ret(&self) -> Option<PrimTy> {
        match self.ret_ty {
            MirTy::Prim(p) if p != PrimTy::Str => Some(p),
            _ => None,
        }
    }
}

/// A lexical scope of a function (a block with its own `let`s), for
/// debug info: `id` 0 is the function body; `parent` encloses it. The
/// first `Loc` of a scope is at its block's start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scope {
    pub id: u32,
    pub parent: u32,
}

#[derive(Clone, Debug)]
pub struct LocalDecl {
    pub id: LocalId,
    pub name: Option<String>,
    pub ty: MirTy,
}

#[derive(Clone, Debug)]
pub struct BasicBlock {
    pub id: BlockId,
    /// Block parameters (in place of phi nodes).
    /// Predecessors pass values by terminator args.
    pub params: Vec<LocalId>,
    pub stmts: Vec<Statement>,
    pub terminator: Terminator,
}

#[derive(Clone, Debug)]
pub enum Statement {
    Assign {
        target: LocalId,
        rvalue: RValue,
    },
    // `arr[index] = value` into an array local's slot, or into the array
    // `offset` bytes into one (`s.buf[i] = v` in place).
    ArrayWrite {
        array: LocalId,
        offset: u32,
        index: Operand,
        value: Operand,
        elem_ty: MirTy,
        len: u32,
    },
    // The local's scope ended (only emitted for GC-holding locals):
    // codegen clears its shadow-stack root so it stops retaining.
    StorageDead(LocalId),
    // `buf[index] = value` into a `BufAlloc` object (unchecked).
    BufStore {
        buf: Operand,
        index: Operand,
        value: Operand,
        elem_ty: MirTy,
    },
    /// The following statements come from source position `Span` (a
    /// HIR statement or block value) in lexical `Scope`: debug info only.
    Loc(Span, Scope),
    // Store `value` (a `ty`) at raw address `addr` (`usize`).
    RawStore {
        addr: Operand,
        value: Operand,
        ty: MirTy,
        volatile: bool,
    },
    // Field `index` of the `ty` (a tuple) payload of the GC object
    // `ptr` = `value`, in place (coroutine frames).
    HeapStore {
        ptr: Operand,
        ty: MirTy,
        index: u32,
        value: Operand,
    },
    // `STATIC_NAME = value` - store into a module-level global.
    StaticStore {
        name: String,
        value: Operand,
        ty: MirTy,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum RValue {
    Use(Operand),
    /// `op_ty` is the operand type - the result type lives on the
    /// destination `LocalDecl` (comparisons widen to `bool`).
    BinOp {
        op: PrimOp,
        op_ty: MirTy,
        lhs: Operand,
        rhs: Operand,
    },
    UnaryOp {
        op: PrimOp,
        op_ty: MirTy,
        operand: Operand,
    },
    // Direct call to a function in the same compilation unit.
    Call {
        callee: FunctionId,
        args: Vec<Operand>,
        ret_ty: MirTy,
    },
    // Build a struct value from its fields, in declaration order.
    StructInit {
        ty: MirTy,
        fields: Vec<Operand>,
    },
    // Read field `index` of a struct value.
    FieldRead {
        target: Operand,
        index: u32,
        field_ty: MirTy,
    },
    // Value conversion from `from_ty` to `to_ty`; an integer is zero- or
    // sign-extended when widening, truncated when narrowing.
    Cast {
        source: Operand,
        from_ty: MirTy,
        to_ty: MirTy,
    },
    // Bit-preserving move between a variant's payload type and the
    // shared payload slot of its enum / sum (zero-extend / truncate,
    // float bits reinterpreted). Not a value conversion like `Cast`.
    Bitcast {
        source: Operand,
        from_ty: MirTy,
        to_ty: MirTy,
    },
    // Functional field replace: same struct as `source` but with the
    // given index updated.
    WithField {
        source: Operand,
        struct_ty: MirTy,
        index: u32,
        value: Operand,
    },
    // Build a fixed-size array literal. Codegen stores each element
    // into the destination local's slot.
    ArrayInit {
        elem_ty: MirTy,
        elements: Vec<Operand>,
    },
    // Read `array[index]` from an array local's slot, or from the
    // array `offset` bytes into one (`s.buf[i]` in place, no copy).
    ArrayIndex {
        array: LocalId,
        offset: u32,
        index: Operand,
        elem_ty: MirTy,
        len: u32,
    },
    // Materialise a function-pointer SSA value from a known callee id.
    FnAddr {
        callee: FunctionId,
        fn_ty: MirTy,
    },
    // Read a module-level `static` global by name: its address, then a
    // load.
    StaticLoad {
        name: String,
        ty: MirTy,
    },
    // Copy `value` (of type `ty`) into a fresh GC object; yields its
    // `GcRef`. The payload of an `any Tr` value. Codegen keeps a numeric /
    // bool / char primitive no wider than a pointer inline (the word is
    // the value's bits, not a reference).
    HeapBox {
        value: Operand,
        ty: MirTy,
    },
    // A zeroed GC object holding `len` `elem_ty`s; yields its `GcRef`.
    // Its type's ScanTable traces the payload when `elem_ty` holds GC
    // references.
    BufAlloc {
        elem_ty: MirTy,
        len: Operand,
    },
    // Load a `ty` from raw address `addr` (`usize`).
    RawLoad {
        addr: Operand,
        ty: MirTy,
        volatile: bool,
    },
    // The address (`usize`) of the local's stack slot. The local stays in
    // that slot (never an SSA value), rooted for its whole scope when it
    // holds GC references (`liveness::addressed`).
    AddrOf(LocalId),
    // The address (`usize`) of `static` `name` (initialized first if lazy).
    StaticAddr(String),
    // Size (or alignment) of `ty` in bytes, as `usize`.
    SizeOf {
        ty: MirTy,
        align: bool,
    },
    // Element `index` of a `BufAlloc` object (unchecked).
    BufLoad {
        buf: Operand,
        index: Operand,
        elem_ty: MirTy,
    },
    // Atomic `op` on element `index` of a `BufAlloc` object (see
    // `HirExprKind::BufAtomic`); yields the previous value.
    BufAtomic {
        op: &'static str,
        buf: Operand,
        index: Operand,
        args: Vec<Operand>,
        elem_ty: MirTy,
        order: [&'static str; 2],
    },
    // Load the `ty` payload of the `GcRef` `ptr` (a `HeapBox` object),
    // or unpack an inline payload.
    HeapLoad {
        ptr: Operand,
        ty: MirTy,
    },
    // A zeroed GC object with a `ty` payload (traced by `ty`'s
    // ScanTable); yields its `GcRef`. A coroutine frame.
    HeapAlloc {
        ty: MirTy,
    },
    // Field `index` of the `ty` (a tuple) payload of GC object `ptr`.
    HeapField {
        ptr: Operand,
        ty: MirTy,
        index: u32,
    },
    // Indirect call through a function-typed operand: a nabi `fn`
    // value's code, or an `extern "C" fn` pointer.
    CallIndirect {
        callee: Operand,
        args: Vec<Operand>,
        arg_tys: Vec<MirTy>,
        ret_ty: MirTy,
        abi: Abi,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operand {
    Const(ConstVal),
    Copy(LocalId),
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Goto {
        target: BlockId,
        args: Vec<Operand>,
    },
    If {
        cond: Operand,
        then_bb: BlockId,
        then_args: Vec<Operand>,
        else_bb: BlockId,
        else_args: Vec<Operand>,
    },
    Return(Option<Operand>),
    Unreachable,
    /// Coroutine suspend point (only before `coro::transform`): hand
    /// `value` to the resumer; the next resume continues at `resume`.
    /// `is_final` marks the point a finished coroutine parks at.
    Suspend {
        value: Option<Operand>,
        resume: BlockId,
        is_final: bool,
    },
}

/// What a [`MirTy::Union`] is: only `Sum` marks a sum's `(i32 tag,
/// payload)` layout; the shape alone never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnionKind {
    /// The payload slot right after a sum's `i32` tag (`sum_layout`).
    Sum,
    /// Anything else: a `union`'s storage, a pinning frame slot.
    Plain,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MirTy {
    Unit,
    Prim(PrimTy),
    Any,
    Tuple(Vec<MirTy>),
    // Concrete struct, fields stored in declaration order.
    Struct(StructId, Vec<MirTy>),
    // Fixed-size array of `MirTy` repeated `len` times.
    Array(Box<MirTy>, u32),
    // First-class function pointer.
    Fn(Vec<MirTy>, Box<MirTy>),
    /// Word that may hold a GC reference (an object's header
    /// address), lowered as a pointer-width integer: the payload of an `any`
    /// value, which holds small primitives inline instead. Codegen roots
    /// and traces exactly these words; the collector ignores a word that
    /// is not an object address.
    GcRef,
    /// Overlaid values holding GC references: `words` pointer-width
    /// words, laid out like `[GcRef; words]`, keeping each variant's
    /// `(stored tag, type)`. As aligned as its most aligned variant (an
    /// explicit layout past a word). A sum's payload slot
    /// ([`UnionKind::Sum`]) is scanned per the `i32` tag before it
    /// (`SUM_PAYLOAD`); any other one as ambiguous words.
    Union(Vec<(u32, MirTy)>, u32, UnionKind),
}
