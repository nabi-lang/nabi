//! Function bodies and initializers as read: names resolved to their
//! declarations, types not yet checked (`nabicc_sema` does that).

use crate::ty::QualType;
use nabicc_lex::Loc;
use nabicc_target::Prim;

/// A local of the function being read (parameters first).
pub type LocalId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    Param,
    Auto,
    /// `register`: its address may not be taken.
    Register,
    /// `static` in a block: one object for the whole program run.
    Static,
}

#[derive(Clone, Debug)]
pub struct Local {
    pub name: String,
    pub ty: QualType,
    pub kind: LocalKind,
    /// `_Alignas` / `aligned`, bytes (0: none).
    pub align: u64,
    pub loc: Loc,
}

/// A function definition's body.
#[derive(Clone, Debug)]
pub struct Body {
    /// Its declaration (`Parser::fns`).
    pub func: usize,
    pub params: Vec<LocalId>,
    pub locals: Vec<Local>,
    pub stmts: Vec<Stmt>,
    /// Where the closing brace is.
    pub end: Loc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    BitNot,
    Not,
    Deref,
    Addr,
    PreInc,
    PreDec,
    PostInc,
    PostDec,
    Real,
    Imag,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Mul,
    Div,
    Rem,
    Add,
    Sub,
    Shl,
    Shr,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    BitAnd,
    BitXor,
    BitOr,
    And,
    Or,
    Comma,
}

impl BinOp {
    /// The operator of `p` and its precedence (higher binds tighter).
    #[must_use]
    pub fn of(p: &str) -> Option<(Self, u8)> {
        Some(match p {
            "*" => (Self::Mul, 10),
            "/" => (Self::Div, 10),
            "%" => (Self::Rem, 10),
            "+" => (Self::Add, 9),
            "-" => (Self::Sub, 9),
            "<<" => (Self::Shl, 8),
            ">>" => (Self::Shr, 8),
            "<" => (Self::Lt, 7),
            ">" => (Self::Gt, 7),
            "<=" => (Self::Le, 7),
            ">=" => (Self::Ge, 7),
            "==" => (Self::Eq, 6),
            "!=" => (Self::Ne, 6),
            "&" => (Self::BitAnd, 5),
            "^" => (Self::BitXor, 4),
            "|" => (Self::BitOr, 3),
            "&&" => (Self::And, 2),
            "||" => (Self::Or, 1),
            _ => return None,
        })
    }

    /// The operator of compound assignment `p` (`+=` ...).
    #[must_use]
    pub fn assign(p: &str) -> Option<Option<Self>> {
        Some(match p {
            "=" => None,
            "*=" => Some(Self::Mul),
            "/=" => Some(Self::Div),
            "%=" => Some(Self::Rem),
            "+=" => Some(Self::Add),
            "-=" => Some(Self::Sub),
            "<<=" => Some(Self::Shl),
            ">>=" => Some(Self::Shr),
            "&=" => Some(Self::BitAnd),
            "^=" => Some(Self::BitXor),
            "|=" => Some(Self::BitOr),
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    /// An integer constant (literal, character, enumerator) of type `ty`.
    Int(i128, Prim),
    Float(f64, Prim),
    /// A string literal: its code units without the terminating zero, of
    /// element type `elem`.
    Str(Vec<u32>, Prim),
    Local(LocalId),
    /// A file-scope object (`Parser::vars`).
    Global(usize),
    /// A function (`Parser::fns`).
    Func(usize),
    /// A compiler builtin called by name (`__builtin_*`, `__sync_*`,
    /// `__atomic_*`).
    Builtin(String),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `a = b` (`None`) or `a op= b`.
    Assign(Option<BinOp>, Box<Expr>, Box<Expr>),
    /// `c ? a : b`; GNU `c ?: b` has no `a`.
    Cond(Box<Expr>, Option<Box<Expr>>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    Index(Box<Expr>, Box<Expr>),
    /// `e.name` / `e->name` (`true`).
    Member(Box<Expr>, String, bool),
    Cast(QualType, Box<Expr>),
    SizeofExpr(Box<Expr>),
    SizeofType(QualType),
    AlignofExpr(Box<Expr>),
    AlignofType(QualType),
    /// `(T){ .. }`.
    Compound(QualType, Box<Init>),
    /// GNU `({ .. })`: its value is the last expression statement's.
    StmtExpr(Vec<Stmt>),
    VaArg(Box<Expr>, QualType),
    /// `_Generic(e, T: a, default: b)`.
    Generic(Box<Expr>, Vec<(Option<QualType>, Expr)>),
}

impl Expr {
    #[must_use]
    pub const fn new(kind: ExprKind, loc: Loc) -> Self {
        Self { kind, loc }
    }
}

/// An initializer.
#[derive(Clone, Debug)]
pub enum Init {
    Expr(Expr),
    /// `{ .. }`: each item with its designators.
    List(Vec<(Vec<Desig>, Init)>, Loc),
}

#[derive(Clone, Debug)]
pub enum Desig {
    Field(String, Loc),
    Index(Expr),
    /// GNU `[a ... b]`.
    Range(Expr, Expr),
}

#[derive(Clone, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub enum StmtKind {
    Expr(Expr),
    /// Block-scope objects declared here, with their initializers.
    Decl(Vec<(LocalId, Option<Init>)>),
    Block(Vec<Stmt>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    While(Expr, Box<Stmt>),
    Do(Box<Stmt>, Expr),
    /// `for (init; cond; step) body`.
    For(Option<Box<Stmt>>, Option<Expr>, Option<Expr>, Box<Stmt>),
    Switch(Expr, Box<Stmt>),
    /// `case a:` or GNU `case a ... b:`.
    Case(Expr, Option<Expr>, Box<Stmt>),
    Default(Box<Stmt>),
    Break,
    Continue,
    Return(Option<Expr>),
    Goto(String),
    Label(String, Box<Stmt>),
    Null,
    /// Inline assembly: an error only if its function is compiled.
    Asm,
}

impl Stmt {
    #[must_use]
    pub const fn new(kind: StmtKind, loc: Loc) -> Self {
        Self { kind, loc }
    }
}
