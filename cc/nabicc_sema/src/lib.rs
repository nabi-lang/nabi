//! The meaning of a C translation unit: every expression typed and its
//! implicit conversions made explicit (integer promotions, the usual
//! arithmetic conversions, array and function decay, assignment
//! conversions), initializers flattened to offsets, objects and
//! functions merged across their declarations, and C's constraints
//! checked. `nabicc_lower` turns the result into MIR.

// The checker and the lowering follow C's grammar (long matches) and
// name values as C does (`a`, `b`, `l`, `r`).
#![allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::unnecessary_box_returns
)]

pub mod check;

mod builtin;
mod ctype;
mod expr;
mod fold;
mod init;
mod stmt;

pub use crate::check::check;
pub use crate::fold::{Const, Sym, fold, wrap};

use nabicc_lex::Loc;
use nabicc_parse::ty::{TypeId, Types};

pub type GlobalId = usize;
pub type FuncId = usize;
pub type LocalId = u32;
pub type LabelId = u32;

/// A checked translation unit.
pub struct Unit {
    pub types: Types,
    pub globals: Vec<Global>,
    pub funcs: Vec<Function>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Def {
    /// Declared only (`extern`, or a function without a body).
    Extern,
    /// A tentative definition: zero-initialized here.
    Tentative,
    /// Defined with this initializer.
    Init(TInit),
}

/// An object with static storage duration.
#[derive(Clone, Debug, PartialEq)]
pub struct Global {
    pub name: String,
    /// Its symbol (an `asm` label, else the name).
    pub symbol: String,
    pub ty: TypeId,
    /// `static` (or a block-scope static, string or compound literal).
    pub internal: bool,
    pub konst: bool,
    pub def: Def,
    /// `aligned` / `_Alignas`, bytes (0: the type's).
    pub align: u64,
    /// `_Thread_local` (declared only: definitions are refused).
    pub tls: bool,
    pub loc: Loc,
}

/// A function: declared, and defined here with a body.
#[derive(Clone, Debug)]
pub struct Function {
    pub name: String,
    pub symbol: String,
    /// Its function type (canonical).
    pub ty: TypeId,
    pub internal: bool,
    pub inline: bool,
    pub body: Option<Body>,
    /// An inline or `static` function whose body does not check: an
    /// error only if the unit's code calls it (a header's intrinsics).
    pub broken: Option<String>,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct Body {
    pub params: Vec<LocalId>,
    pub locals: Vec<TLocal>,
    pub stmts: Vec<TStmt>,
    /// Labels by id, for `goto`.
    pub labels: u32,
    pub end: Loc,
}

#[derive(Clone, Debug)]
pub struct TLocal {
    pub name: String,
    pub ty: TypeId,
    /// Its address is taken (or it is an aggregate): it lives in memory.
    pub addressed: bool,
    pub align: u64,
    pub loc: Loc,
}

/// A bitfield: where its bits are in the storage unit at the member's
/// offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bits {
    /// Bit offset from the start of the unit (little-endian numbering).
    pub at: u32,
    pub width: u32,
    pub signed: bool,
    /// Bytes of the unit the bits are read from and written to.
    pub unit: u32,
}

/// A typed expression: `ty` is canonical and unqualified.
#[derive(Clone, Debug, PartialEq)]
pub struct TExpr {
    pub kind: TK,
    pub ty: TypeId,
    pub loc: Loc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    And,
    Or,
    Xor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// A compiler builtin, its operands already converted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Clz,
    Ctz,
    Popcount,
    Bswap,
    Unreachable,
    Trap,
    /// `__builtin_*_overflow(a, b, &r)`: the result's place last.
    AddOverflow,
    SubOverflow,
    MulOverflow,
    /// `__atomic_*` / `__sync_*` on an address.
    AtomicLoad,
    AtomicStore,
    AtomicExchange,
    /// `(ptr, &expected, desired)`: whether it stored.
    AtomicCas,
    /// `__sync_val_compare_and_swap(ptr, old, new)`: the old value.
    AtomicCasVal,
    AtomicFetch(Arith),
    AtomicFetchNand,
    /// `op_fetch`: the new value.
    AtomicOpFetch(Arith),
    Fence,
    /// `__builtin_alloca(n)`.
    Alloca,
    FrameAddress,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TK {
    Int(i128),
    Float(f64),
    /// Lvalues.
    Local(LocalId),
    Global(GlobalId),
    Deref(Box<TExpr>),
    /// A member of a record lvalue at this byte offset.
    Member(Box<TExpr>, u64, Option<Bits>),
    /// A function designator (only under `AddrOf` or as a callee).
    Func(FuncId),
    /// The address of an lvalue or function.
    AddrOf(Box<TExpr>),
    /// An array lvalue's first element's address.
    Decay(Box<TExpr>),
    /// The scalar converted to `ty` (`void`: the value dropped).
    Conv(Box<TExpr>),
    Neg(Box<TExpr>),
    BitNot(Box<TExpr>),
    /// `!x`: an `int`.
    Not(Box<TExpr>),
    /// Arithmetic in `ty`, both operands of `ty` (a shift's right: of
    /// its own promoted type).
    Bin(Arith, Box<TExpr>, Box<TExpr>),
    /// Comparison of operands of one type: an `int`.
    Cmp(Cmp, Box<TExpr>, Box<TExpr>),
    /// `p + i * size` (`i` a `ptrdiff_t`).
    PtrAdd(Box<TExpr>, Box<TExpr>, u64),
    /// `(a - b) / size`.
    PtrDiff(Box<TExpr>, Box<TExpr>, u64),
    And(Box<TExpr>, Box<TExpr>),
    Or(Box<TExpr>, Box<TExpr>),
    Cond(Box<TExpr>, Box<TExpr>, Box<TExpr>),
    Comma(Box<TExpr>, Box<TExpr>),
    /// `place = value`: the value of `ty`.
    Assign(Box<TExpr>, Box<TExpr>),
    /// `place op= rhs` computed in type `op_ty` (both converted to it,
    /// the result back to the place's type).
    OpAssign(Arith, Box<TExpr>, Box<TExpr>, TypeId),
    /// `p += i` / `p -= i` (`sub`) on a pointer place, element `size`.
    PtrAssign(Box<TExpr>, Box<TExpr>, u64, bool),
    /// `++` / `--`: `pre`, `inc`; `step` is 1 or a pointer's element
    /// size.
    IncDec(Box<TExpr>, bool, bool, u64),
    /// A call: the callee (a function designator or a pointer to a
    /// function), the arguments converted, the function type.
    Call(Box<TExpr>, Vec<TExpr>, TypeId),
    /// A compound literal: its object and initializer.
    Compound(Box<TExpr>, TInit),
    StmtExpr(Vec<TStmt>, Option<Box<TExpr>>),
    /// `va_start(ap)`, `va_end(ap)`, `va_copy(dst, src)` on the lists'
    /// addresses; `va_arg(ap, T)`.
    VaStart(Box<TExpr>),
    VaEnd(Box<TExpr>),
    VaCopy(Box<TExpr>, Box<TExpr>),
    VaArg(Box<TExpr>),
    Builtin(Builtin, Vec<TExpr>),
}

impl TExpr {
    #[must_use]
    pub const fn new(kind: TK, ty: TypeId, loc: Loc) -> Self {
        Self { kind, ty, loc }
    }

    /// Whether it designates an object.
    #[must_use]
    pub const fn is_lvalue(&self) -> bool {
        matches!(
            self.kind,
            TK::Local(_) | TK::Global(_) | TK::Deref(_) | TK::Member(..) | TK::Compound(..)
        )
    }
}

/// An initializer as the stores that make it, onto a zeroed object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TInit {
    pub items: Vec<InitItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InitItem {
    pub off: u64,
    pub kind: InitKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum InitKind {
    /// A scalar (or a whole record value) of the expression's type.
    Expr(TExpr),
    Bits(TExpr, Bits),
    /// A character array's bytes from a string literal.
    Bytes(Vec<u8>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TStmt {
    pub kind: TS,
    pub loc: Loc,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TS {
    Expr(TExpr),
    /// A local initialized: zeroed, then the stores.
    Init(LocalId, TInit),
    Block(Vec<TStmt>),
    If(TExpr, Box<TStmt>, Option<Box<TStmt>>),
    While(TExpr, Box<TStmt>),
    Do(Box<TStmt>, TExpr),
    For(Option<Box<TStmt>>, Option<TExpr>, Option<TExpr>, Box<TStmt>),
    /// The value (promoted), the body, its cases `(low, high, label)`
    /// and its default.
    Switch(
        TExpr,
        Box<TStmt>,
        Vec<(i128, i128, LabelId)>,
        Option<LabelId>,
    ),
    Break,
    Continue,
    Return(Option<TExpr>),
    Goto(LabelId),
    /// A label (also each `case` and `default`).
    Label(LabelId, Box<TStmt>),
    Null,
    /// Inline assembly (refused where its function is compiled).
    Asm,
}

impl TStmt {
    #[must_use]
    pub const fn new(kind: TS, loc: Loc) -> Self {
        Self { kind, loc }
    }
}
