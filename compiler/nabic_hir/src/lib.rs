//! High-level IR: the AST with names resolved, types checked and
//! generics instantiated. The data, its NABIHIR codec and printer, and
//! the AST visitor; `nabic_typeck` makes it.

pub mod marks;
pub mod plugin;
pub mod walk;

mod codec;
mod dropped;
mod print;

pub use dropped::{CfgDropped, cfg_dropped, cfg_preds, note_dropped};
pub use print::{instance, print_hir};

use nabic_core::DefId;
use nabic_diagnostic::{NabiDiagnostic, Span};
use nabic_ir_common::{ConstVal, FnFacts, FunctionId, LocalId, PrimOp, PrimTy};

/// How many bodies were lowered and how many of them replayed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BodyReuse {
    pub replayed: usize,
    pub lowered: usize,
}

/// Marker on a trait / impl method, `[ASSOC_TYPES_ATTR, X, ..]`: bare `X`
/// in the body means `Self::X`.
pub const ASSOC_TYPES_ATTR: &str = "#assoc";

#[derive(Clone, Debug, Default)]
pub struct HirModule {
    pub functions: Vec<HirFunction>,
    pub structs: Vec<HirStructDef>,
    pub enums: Vec<HirEnumDef>,
    /// Module-level `static` items with stable storage (one global per
    /// entry). Differs from `const` (which inlines at each use site).
    pub statics: Vec<HirStaticDef>,
    /// Lowering diagnostics, appended by the driver.
    pub diagnostics: Vec<NabiDiagnostic>,
    /// Codegen roots: `main`, exports and lazy static inits (a prebuilt
    /// library: also all its functions and the `any` dispatchers). `None`:
    /// keep everything (a library without `main`).
    pub roots: Option<Vec<FunctionId>>,
    /// Library functions no lowered body reaches: body-less stubs in
    /// `functions` (signatures for tools), not code to emit.
    pub unlowered: Vec<FunctionId>,
    /// Every type-like item (struct, enum, union, alias, trait) by full
    /// name: its visibility (`type_visible`).
    pub type_visibility: nabic_core::HashMap<String, nabic_core::Visibility>,
    /// A prebuilt-library build's `any` seeds for its NABIHIR: `(module,
    /// Nabi source)` (see `lower::expr::any_link`).
    pub any_seeds: Vec<(String, String)>,
    /// The types this program stores into `any` slots that source can
    /// name, by full path (the REPL seeds later cells with them).
    pub any_types: Vec<String>,
    /// Every concrete type stored into an `any` slot, with its type tag
    /// (`-g` describes an `any` by them).
    pub any_tags: Vec<(HirTy, u32)>,
    /// Bodies replayed from `HirInput::bodies`, of those lowered.
    pub reuse: BodyReuse,
    /// A prebuilt-library build's use-after-hand-off summaries of its
    /// bodies, for its NABIHIR (`HirInput::summaries`).
    pub summaries: Vec<(String, String)>,
    /// The types reflection describes (`nabi.type_desc`).
    pub type_descs: Vec<nabic_ir_common::typedesc::TypeDesc<HirTy>>,
    /// A plugin's or a plugin host's tables (`plugin::Mode`).
    pub plugin: plugin::Info,
}

#[derive(Clone, Debug)]
pub struct HirStaticDef {
    pub name: String,
    pub ty: HirTy,
    /// Initial value: a literal or const-folded primitive.
    pub init: ConstVal,
    pub is_mut: bool,
    /// `extern "C" { static .. }`: no initializer here.
    pub is_extern: bool,
    /// An extern static of a block not linked `kind = "static"`: on
    /// Windows a DLL's data, reached through its import address.
    pub dll_import: bool,
    /// Non-literal initializer: this function computes the value on
    /// first access (lazy, thread-safe); `init` is unused.
    pub init_fn: Option<FunctionId>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub struct EnumId(pub u32);

#[derive(Clone, Debug)]
pub struct HirEnumDef {
    pub id: EnumId,
    /// `#[repr(..)]`: an integer tag type.
    pub repr: Repr,
    pub def_id: Option<DefId>,
    pub name: String,
    pub variants: Vec<HirEnumVariant>,
    pub span: Span,
    /// Concrete type args if this enum is an instantiation of a generic
    /// enum (`Res<i32, bool>`); empty otherwise. Mirrors `HirStructDef`.
    pub generic_args: Vec<HirTy>,
}

#[derive(Clone, Debug)]
pub struct HirEnumVariant {
    pub name: String,
    /// Stored tag: the variant index, or a value variant's value.
    pub tag: i128,
    pub payload_ty: Option<HirTy>,
    /// Field names of a named-form variant (`Move { x: i32; y: i32; }`),
    /// by payload tuple index. Empty for unit and tuple-form variants.
    pub named_fields: Vec<String>,
    /// Constructor argument count (0 = unit). `V(T)` and `V(A, B)` both
    /// store one `payload_ty`, so a tuple payload is otherwise ambiguous.
    pub arity: usize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub struct StructId(pub u32);

#[derive(Clone, Debug)]
pub struct HirStructDef {
    pub id: StructId,
    /// `#[repr(..)]`.
    pub repr: Repr,
    pub def_id: Option<DefId>,
    pub name: String,
    pub fields: Vec<HirField>,
    pub span: Span,
    /// Type arguments of a generic struct instance; empty otherwise.
    pub generic_args: Vec<HirTy>,
    /// `Some` for a `union`: one synthetic `__data` field typed by its
    /// `UnionStorage`; declared fields live in `aliases`.
    pub union_kind: Option<HirUnionKind>,
    /// `#[niche(..)]`: bit patterns the single field never
    /// holds. Empty without the attribute.
    pub niche: Vec<NicheArg>,
}

/// `#[repr(..)]`. The default layout is already C's, so `C`
/// changes nothing there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Repr {
    pub c: bool,
    /// One field, passed and laid out as that field.
    pub transparent: bool,
    /// No padding, alignment 1.
    pub packed: bool,
    /// `align(N)`: at least `N`-aligned; 0 without.
    pub align: u32,
    /// An enum's tag type (`u8` .. `i64`).
    pub int: Option<PrimTy>,
    /// A library struct the C ABI passes as a C type of its own.
    pub c_kind: CKind,
}

/// The C type a compiler-known library struct stands for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CKind {
    #[default]
    Plain,
    /// `f16`: `_Float16`.
    F16,
    /// `bf16`: `__bf16`.
    BF16,
    /// `Simd<T, N>`: a vector (`float32x4_t`, `__m128`).
    Simd,
    /// `Mask<T, N>`: no C type.
    Mask,
}

/// One `#[niche(..)]` value, read against the field's primitive type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NicheVal {
    Int(i128),
    /// The field type's `MIN` / `MAX`.
    Min,
    Max,
    /// The canonical quiet NaN / infinities of the field's float type.
    Nan,
    Inf,
    NegInf,
}

/// One `#[niche(..)]` argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NicheArg {
    Val(NicheVal),
    /// Inclusive bounds; an open end is `Min` / `Max`.
    Range(NicheVal, NicheVal),
}

/// A `union`'s storage. `#[repr(C)]` or a non-primitive field forces
/// the byte array, so the layout matches C.
#[derive(Clone, Debug)]
pub enum UnionStorage {
    /// One primitive wide enough for every field, accessed by cast;
    /// stays in registers.
    WidestPrim(PrimTy),
    /// Bytes as large and aligned as the largest field
    /// (`nabic_mir::ty::union_storage`); possible reference words are
    /// scanned as ambiguous.
    ByteArray,
}

#[derive(Clone, Debug)]
pub struct HirUnionKind {
    pub storage: UnionStorage,
    /// Declared fields and their types: the cast targets at access sites.
    pub aliases: Vec<(String, HirTy)>,
    pub is_repr_c: bool,
}

#[derive(Clone, Debug)]
pub struct HirField {
    pub name: String,
    pub ty: HirTy,
    /// Default for an omitted field: kept as AST and lowered at each
    /// literal, which is fine since it is const.
    pub default: Option<nabic_ast::Expr>,
}

#[derive(Clone, Debug)]
pub struct HirFunction {
    pub id: FunctionId,
    pub def_id: Option<DefId>,
    pub name: String,
    pub params: Vec<HirParam>,
    pub locals: Vec<HirLocal>,
    pub ret_ty: HirTy,
    pub body: Option<HirBlock>,
    pub is_async: bool,
    pub is_generator: bool,
    pub span: Span,
    pub facts: FnFacts,
}

#[derive(Clone, Debug)]
pub struct HirParam {
    pub name: String,
    pub local: LocalId,
    pub ty: HirTy,
    pub by_ref: bool,
}

#[derive(Clone, Debug)]
pub struct HirLocal {
    pub id: LocalId,
    pub name: Option<String>,
    pub ty: HirTy,
    /// A `[ref]`-captured variable: the local holds its one-element
    /// `GcBuf` cell (`ty`), shared with the closures.
    pub boxed: bool,
}

#[derive(Clone, Debug)]
pub struct HirBlock {
    pub stmts: Vec<HirStmt>,
    /// Trailing expression: the block's value.
    pub trailing: Option<HirExpr>,
    /// `'label:` on the block: `break 'label value` exits it with `value`.
    pub label: Option<String>,
}

#[derive(Clone, Debug)]
pub enum HirStmt {
    Let {
        local: LocalId,
        init: HirExpr,
    },
    Assign {
        local: LocalId,
        value: HirExpr,
    },
    /// `arr[index] = value;`; with a `path` of field indices, into the
    /// array field `array_local.f.g` in place.
    ArrayAssign {
        array_local: LocalId,
        path: Vec<u32>,
        index: HirExpr,
        value: HirExpr,
        elem_ty: HirTy,
    },
    /// Write to a `mut` module-level `static`: global address + store.
    StaticAssign {
        name: String,
        value: HirExpr,
    },
    Expr(HirExpr),
    Return(Option<HirExpr>),
    Break {
        label: Option<String>,
        value: Option<HirExpr>,
    },
    Continue {
        label: Option<String>,
    },
    /// Call of a never-returning runtime hook (`nabi_panic_at` for `try!`,
    /// `nabi_uncaught_error_at` for an error escaping `main`), followed by
    /// `unreachable` in MIR. `throw` itself is returns / jumps
    /// (`lower::throws`).
    Panic(HirExpr),
}

#[derive(Clone, Debug)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub ty: HirTy,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum HirExprKind {
    Const(ConstVal),
    Local(LocalId),
    Block(Box<HirBlock>),
    /// Read of a module-level `static`: global address + load.
    StaticRef(String),
    /// Pre-resolution placeholder for unresolved paths.
    UnresolvedPath(Vec<String>),
    /// Binary op on two operands of one primitive type, bypassing trait
    /// dispatch (same result).
    PrimBinOp {
        op: PrimOp,
        lhs: Box<HirExpr>,
        rhs: Box<HirExpr>,
    },
    PrimUnaryOp {
        op: PrimOp,
        operand: Box<HirExpr>,
    },
    /// Direct call to a function by id (resolved within this module).
    Call {
        callee: FunctionId,
        args: Vec<HirExpr>,
    },
    /// A function as a value (also lifted closure bodies).
    FnRef(FunctionId),
    /// Indirect call through a function-typed expression.
    CallIndirect {
        callee: Box<HirExpr>,
        args: Vec<HirExpr>,
        ret_ty: HirTy,
    },
    /// `expr as Type`; codegen picks the op from `from_ty` / `to_ty`.
    Cast {
        operand: Box<HirExpr>,
        from_ty: HirTy,
        to_ty: HirTy,
    },
    /// Closure value `(fn_addr, env)`; the lifted function takes `env`
    /// first, the visible signature is `(arg_tys) -> ret_ty`.
    ClosureValue {
        fn_id: FunctionId,
        env: Box<HirExpr>,
        env_ty: HirTy,
        arg_tys: Vec<HirTy>,
        ret_ty: HirTy,
    },
    /// Call a closure value: `fn_addr(env, args..)`.
    CallClosure {
        closure: Box<HirExpr>,
        args: Vec<HirExpr>,
        arg_tys: Vec<HirTy>,
        ret_ty: HirTy,
        env_ty: HirTy,
    },
    /// `if cond { then } else { else_branch }`; the else is an expression
    /// so `else if` chains need nothing special.
    If {
        cond: Box<HirExpr>,
        then_block: Box<HirBlock>,
        else_branch: Option<Box<HirExpr>>,
    },
    /// `while cond { body }`; unit.
    While {
        label: Option<String>,
        cond: Box<HirExpr>,
        body: Box<HirBlock>,
    },
    /// `loop { body }`; its value is the `break` operand (or unit).
    Loop {
        label: Option<String>,
        body: Box<HirBlock>,
        result_ty: HirTy,
    },
    /// `match scrut { pat => body, ... }`.
    Match {
        scrutinee: Box<HirExpr>,
        arms: Vec<HirMatchArm>,
    },
    /// `Foo { field: value, ... }`; fields in declaration order.
    StructInit {
        struct_id: StructId,
        fields: Vec<HirExpr>,
    },
    /// `expr.field` on a struct value.
    FieldRead {
        target: Box<HirExpr>,
        struct_id: StructId,
        field_index: u32,
        field_ty: HirTy,
    },
    /// `target` with one field replaced (lowered `p.x = value`).
    WithField {
        target: Box<HirExpr>,
        struct_id: StructId,
        field_index: u32,
        value: Box<HirExpr>,
    },
    /// `EnumName::Variant(args)`. `args` is empty for unit variants.
    EnumVariantInit {
        enum_id: EnumId,
        variant_index: u32,
        args: Vec<HirExpr>,
    },
    /// Wrap a value into a sum-type slot: `let x: i32 | none = 42`.
    /// `value` is `None` for the `none` variant.
    SumWrap {
        sum_ty: HirTy,
        variant_index: u32,
        value: Option<Box<HirExpr>>,
    },
    /// `(a, b, c)` tuple literal.
    Tuple {
        fields: Vec<HirExpr>,
    },
    /// `t.0`, `t.1`: tuple field by index.
    TupleField {
        target: Box<HirExpr>,
        index: u32,
        field_ty: HirTy,
    },
    /// `[a, b, c]` fixed-size array literal.
    ArrayInit {
        elem_ty: HirTy,
        elements: Vec<HirExpr>,
    },
    /// `[v; len]` with a large `len`: `value` stored into every element
    /// by a loop.
    ArrayRepeat {
        elem_ty: HirTy,
        value: Box<HirExpr>,
        len: u32,
    },
    /// `buf_new::<T>(len)`: a zeroed `GcBuf<T>` of `len` elements.
    BufNew {
        elem: HirTy,
        len: Box<HirExpr>,
    },
    /// `ptr_read::<T>(addr)` (`bare`): load a `T` at a raw address.
    RawRead {
        addr: Box<HirExpr>,
        ty: HirTy,
        volatile: bool,
    },
    /// `ptr_write(addr, v)`: store `v` at a raw address; unit.
    RawWrite {
        addr: Box<HirExpr>,
        value: Box<HirExpr>,
        ty: HirTy,
        volatile: bool,
    },
    /// `addr_of(place)` (`bare`): the address of a local or a part of
    /// one, which then lives in memory.
    AddrOf {
        place: Box<HirExpr>,
    },
    /// `size_of::<T>()` / `align_of::<T>()` (`usize`).
    SizeOf {
        ty: HirTy,
        align: bool,
    },
    /// The address (`usize`) of entry `n` of `HirModule::type_descs`.
    TypeDesc(u32),
    /// `buf_get(buf, i)`: element `i`, unchecked.
    BufGet {
        buf: Box<HirExpr>,
        index: Box<HirExpr>,
        elem: HirTy,
    },
    /// `buf_set(buf, i, v)`: store element `i`, unchecked; unit.
    BufSet {
        buf: Box<HirExpr>,
        index: Box<HirExpr>,
        value: Box<HirExpr>,
        elem: HirTy,
    },
    /// `nabi.atomic.OP` on element `i` of an integer / `bool` buffer; yields
    /// the previous value. `args`: the operand (`cas`: expected, new).
    BufAtomic {
        op: &'static str,
        buf: Box<HirExpr>,
        index: Box<HirExpr>,
        args: Vec<HirExpr>,
        elem: HirTy,
        /// Memory ordering(s) (`relaxed` .. `seq_cst`); `cas` uses both
        /// (success, failure), the others the first.
        order: [&'static str; 2],
    },
    /// `arr[idx]`.
    ArrayIndex {
        array: Box<HirExpr>,
        index: Box<HirExpr>,
        elem_ty: HirTy,
    },
    /// Cast a value into a union's `__data` storage: a numeric cast for
    /// `WidestPrim`, a store through memory for `ByteArray`.
    UnionWrite {
        struct_id: StructId,
        from_ty: HirTy,
        storage_ty: HirTy,
        value: Box<HirExpr>,
    },
    /// Read a declared field back out of a union's `__data` storage.
    UnionRead {
        struct_id: StructId,
        target: Box<HirExpr>,
        field_name: String,
        storage_ty: HirTy,
        field_ty: HirTy,
    },
    /// Coroutine suspend (`yield`, pending `await`): hand `value` to the
    /// resumer and continue here on the next resume. `is_final` is where a
    /// finished coroutine parks, returning the same value forever.
    Suspend {
        value: Box<HirExpr>,
        is_final: bool,
    },
    /// A fresh coroutine of `body_fn` with initial `args`: a function value
    /// `fn(resume args..) -> step` whose env is the frame.
    CoroNew {
        body_fn: FunctionId,
        args: Vec<HirExpr>,
    },
}

#[derive(Clone, Debug)]
pub struct HirMatchArm {
    pub pat: HirPattern,
    pub guard: Option<HirExpr>,
    pub body: HirExpr,
}

/// A lowered pattern.
#[derive(Clone, Debug)]
pub enum HirPattern {
    /// `_`: matches anything, binds nothing.
    Wildcard,
    /// `name`: matches anything, binds a fresh local.
    Binding { local: LocalId, ty: HirTy },
    /// `name @ sub`: matches like `sub`, binds the whole value.
    At {
        local: LocalId,
        ty: HirTy,
        sub: Box<HirPattern>,
    },
    /// `42i32`, `"foo"`, `true`: equality with the constant.
    Literal { value: ConstVal, prim: PrimTy },
    /// `a | b | c`: any sub-pattern matches.
    Or(Vec<HirPattern>),
    /// `(p0, p1, ..)`: sub-patterns aligned to elements.
    Tuple(Vec<HirPattern>),
    /// `Foo { field: pat, .. }`: fields by index into the definition;
    /// unlisted fields are wildcards.
    Struct {
        struct_id: StructId,
        fields: Vec<(u32, HirPattern)>,
    },
    /// `lo..hi` or `lo..=hi`, either bound optional, compared as the
    /// scrutinee's primitive.
    Range {
        start: Option<ConstVal>,
        end: Option<ConstVal>,
        inclusive: bool,
        prim: PrimTy,
    },
    /// `EnumName::Variant(sub_pat...)` pattern.
    EnumVariant {
        enum_id: EnumId,
        variant_index: u32,
        args: Vec<HirPattern>,
    },
    /// `name: T`: variant `variant_index` of a sum, optionally binding the
    /// unwrapped payload to `binder`.
    SumVariant {
        sum_ty: HirTy,
        variant_index: u32,
        binder: Option<LocalId>,
        payload_ty: Option<HirTy>,
    },
    /// A pattern on one member of a sum or `any` (`'a'` on `char | none`):
    /// the tag test, then `sub` on the payload.
    SumMember {
        sum_ty: HirTy,
        variant_index: u32,
        payload_ty: HirTy,
        sub: Box<HirPattern>,
    },
    /// `[a, ..rest, z]` on a list, slice or fixed array.
    Slice(Box<SlicePattern>),
}

/// A slice pattern.
///
/// `whole` takes the scrutinee and `len` its length (`len_expr`, over
/// `whole`); it matches when `len` is the number of items, or at least
/// that with a `..`. Each item's `read` (over `whole` and `len`) is then
/// matched with its pattern.
#[derive(Clone, Debug)]
pub struct SlicePattern {
    pub whole: LocalId,
    pub len: LocalId,
    pub len_expr: HirExpr,
    pub head: Vec<(HirExpr, HirPattern)>,
    /// `..`: `Some`, with the view `..name` binds.
    pub rest: Option<Option<(HirExpr, HirPattern)>>,
    pub tail: Vec<(HirExpr, HirPattern)>,
    /// A fixed array's length: the only one a value can have.
    pub fixed: Option<u32>,
}

impl SlicePattern {
    /// The items before and after the rest, in order.
    pub fn items(&self) -> impl Iterator<Item = &(HirExpr, HirPattern)> {
        self.head
            .iter()
            .chain(self.rest.iter().flatten())
            .chain(self.tail.iter())
    }

    /// The fewest elements a match needs.
    #[must_use]
    pub fn min_len(&self) -> u32 {
        u32::try_from(self.head.len() + self.tail.len()).expect("slice pattern too long")
    }
}

impl HirPattern {
    /// Matches every value of its type (no refutable sub-pattern).
    #[must_use]
    pub fn is_irrefutable(&self) -> bool {
        match self {
            Self::Wildcard | Self::Binding { .. } => true,
            Self::At { sub, .. } => sub.is_irrefutable(),
            Self::Tuple(parts) => parts.iter().all(Self::is_irrefutable),
            Self::Struct { fields, .. } => fields.iter().all(|(_, p)| p.is_irrefutable()),
            Self::Or(alts) => alts.iter().any(Self::is_irrefutable),
            Self::Slice(s) => {
                let fits = match s.fixed {
                    Some(n) => s.rest.is_some() || n == s.min_len(),
                    None => s.rest.is_some() && s.min_len() == 0,
                };
                fits && s.items().all(|(_, p)| p.is_irrefutable())
            }
            Self::Literal { .. }
            | Self::Range { .. }
            | Self::EnumVariant { .. }
            | Self::SumVariant { .. }
            | Self::SumMember { .. } => false,
        }
    }

    /// The expressions a slice pattern reads with, in order.
    pub fn each_expr<'a>(&'a self, f: &mut impl FnMut(&'a HirExpr)) {
        match self {
            Self::Slice(s) => {
                f(&s.len_expr);
                for (e, p) in s.items() {
                    f(e);
                    p.each_expr(f);
                }
            }
            Self::At { sub, .. } | Self::SumMember { sub, .. } => sub.each_expr(f),
            Self::Or(ps) | Self::Tuple(ps) | Self::EnumVariant { args: ps, .. } => {
                ps.iter().for_each(|p| p.each_expr(f));
            }
            Self::Struct { fields, .. } => fields.iter().for_each(|(_, p)| p.each_expr(f)),
            _ => {}
        }
    }

    /// `each_expr`, mutably.
    pub fn each_expr_mut(&mut self, f: &mut impl FnMut(&mut HirExpr)) {
        match self {
            Self::Slice(s) => {
                f(&mut s.len_expr);
                let rest = s.rest.iter_mut().flatten();
                for (e, p) in s.head.iter_mut().chain(rest).chain(s.tail.iter_mut()) {
                    f(e);
                    p.each_expr_mut(f);
                }
            }
            Self::At { sub, .. } | Self::SumMember { sub, .. } => sub.each_expr_mut(f),
            Self::Or(ps) | Self::Tuple(ps) | Self::EnumVariant { args: ps, .. } => {
                ps.iter_mut().for_each(|p| p.each_expr_mut(f));
            }
            Self::Struct { fields, .. } => {
                fields.iter_mut().for_each(|(_, p)| p.each_expr_mut(f));
            }
            _ => {}
        }
    }

    /// Each local the pattern binds.
    pub fn each_binding(&self, out: &mut dyn FnMut(LocalId)) {
        match self {
            Self::Binding { local, .. }
            | Self::SumVariant {
                binder: Some(local),
                ..
            } => out(*local),
            Self::At { local, sub, .. } => {
                out(*local);
                sub.each_binding(out);
            }
            Self::Or(ps) | Self::Tuple(ps) => ps.iter().for_each(|p| p.each_binding(out)),
            Self::Struct { fields, .. } => fields.iter().for_each(|(_, p)| p.each_binding(out)),
            Self::EnumVariant { args, .. } => args.iter().for_each(|p| p.each_binding(out)),
            Self::SumMember { sub, .. } => sub.each_binding(out),
            Self::Slice(s) => {
                out(s.whole);
                out(s.len);
                s.items().for_each(|(_, p)| p.each_binding(out));
            }
            _ => {}
        }
    }
}

thread_local! {
    /// Each struct's and enum's build-independent name, by id
    /// (`[structs, enums]`), for `HirTy::stable_key`.
    static TYPE_NAMES: std::cell::RefCell<[Vec<String>; 2]> =
        const { std::cell::RefCell::new([Vec::new(), Vec::new()]) };
}

/// Forget the type names of an earlier compile.
pub fn clear_type_names() {
    TYPE_NAMES.with(|n| *n.borrow_mut() = [Vec::new(), Vec::new()]);
}

/// The type names given so far.
pub fn type_names() -> [Vec<String>; 2] {
    TYPE_NAMES.with(|n| n.borrow().clone())
}

/// Take up the type names of `type_names`.
pub fn set_type_names(names: [Vec<String>; 2]) {
    TYPE_NAMES.with(|n| *n.borrow_mut() = names);
}

/// Forget the names of the structs and enums from `structs` / `enums`
/// on (their ids are handed out again).
pub fn truncate_type_names(structs: usize, enums: usize) {
    TYPE_NAMES.with(|n| {
        let mut n = n.borrow_mut();
        n[0].truncate(structs);
        n[1].truncate(enums);
    });
}

/// Name struct (or enum) `id` by full path, instances by their arguments'
/// stable keys; called when the id is handed out.
pub fn name_type(enum_: bool, id: u32, base: &str, args: &[HirTy]) {
    let name = if args.is_empty() {
        base.to_owned()
    } else {
        let args: Vec<String> = args.iter().map(HirTy::canon_key).collect();
        format!("{base}<{}>", args.join(", "))
    };
    set_type_name(enum_, id, name);
}

/// Set the `name_type` of struct (or `enum_`) `id`.
pub fn set_type_name(enum_: bool, id: u32, name: String) {
    TYPE_NAMES.with(|n| {
        let v = &mut n.borrow_mut()[usize::from(enum_)];
        let id = id as usize;
        if v.len() <= id {
            v.resize(id + 1, String::new());
        }
        v[id] = name;
    });
}

/// The build-independent name of struct (or `enum_`) `id` (`name_type`),
/// while its compile's types are named.
#[must_use]
pub fn type_name(enum_: bool, id: u32) -> Option<String> {
    TYPE_NAMES.with(|n| {
        n.borrow()[usize::from(enum_)]
            .get(id as usize)
            .filter(|s| !s.is_empty())
            .cloned()
    })
}

/// A type name without its instance suffix (from the first `__` on).
#[must_use]
pub fn enum_base_name(name: &str) -> &str {
    name.split("__").next().unwrap_or(name)
}

/// A trait as bounded or provided (`Get<i32, Item = u8>`):
/// full path, positional arguments, associated bindings sorted by name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TraitRef {
    pub path: String,
    pub args: Vec<HirTy>,
    pub binds: Vec<(String, HirTy)>,
}

impl TraitRef {
    #[must_use]
    pub fn new(path: String, args: Vec<HirTy>, mut binds: Vec<(String, HirTy)>) -> Self {
        binds.sort_by(|a, b| a.0.cmp(&b.0));
        Self { path, args, binds }
    }

    /// The trait alone, no arguments.
    #[must_use]
    pub fn plain(path: &str) -> Self {
        Self::new(path.to_owned(), Vec::new(), Vec::new())
    }

    /// No arguments and no bindings.
    #[must_use]
    pub fn is_plain(&self) -> bool {
        self.args.is_empty() && self.binds.is_empty()
    }

    /// Does an impl providing `self` (fully given) meet `want`, which may
    /// omit arguments and bindings?
    #[must_use]
    pub fn satisfies(&self, want: &Self) -> bool {
        // A const argument written bare (`Dim<3>`) is the impl's typed one
        // (`Dim<N>` with `N: usize`).
        let same = |w: &HirTy, h: &HirTy| match (w.as_const(), h.as_const()) {
            (Some((a, pa)), Some((b, pb))) => a == b && (pa.is_none() || pb.is_none() || pa == pb),
            _ => w == h,
        };
        self.path == want.path
            && (want.args.is_empty()
                || (want.args.len() == self.args.len()
                    && want.args.iter().zip(&self.args).all(|(w, h)| same(w, h))))
            && want.binds.iter().all(|b| self.binds.contains(b))
    }

    /// `Path<A, Name = B>`, each type rendered by `name`.
    pub fn render(&self, path: &str, name: impl Fn(&HirTy) -> String) -> String {
        if self.is_plain() {
            return path.to_owned();
        }
        let parts: Vec<String> = self
            .args
            .iter()
            .map(&name)
            .chain(self.binds.iter().map(|(n, t)| format!("{n} = {}", name(t))))
            .collect();
        format!("{path}<{}>", parts.join(", "))
    }
}

/// What a `fn` type promises beyond its signature.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct FnBounds {
    /// `+ Send + Sync`: sorted auto-trait keys.
    pub auto: Vec<String>,
    /// `unsafe fn(..)`: calling it needs an unsafe scope.
    pub unsafe_fn: bool,
    /// `extern "C" fn(..)`: a thin C function pointer.
    pub c_abi: bool,
    /// `extern "system" fn(..)` where it is `stdcall` (32-bit x86
    /// Windows; elsewhere it is `extern "C" fn`), with `c_abi`.
    pub stdcall: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HirTy {
    /// Not yet inferred.
    Unknown,
    /// `()`: the return type when none is written.
    Unit,
    Prim(PrimTy),
    /// Untyped wildcard (error recovery, untyped parameters); compatible
    /// with everything. The user's `any` is `AnyTrait(vec![])`.
    Any,
    /// `any` / `any Tr` / `any (A + B)`, bounds in canonical order. A tag
    /// (program-wide type number) and a heap payload; built with `SumWrap`,
    /// narrowed with `SumVariant`.
    AnyTrait(Vec<TraitRef>),
    /// A type name not resolved (yet): a type parameter, `Self`, or an
    /// unknown name reported later.
    Path(Vec<String>),
    /// The type of `none`.
    None,
    /// A const generic argument in a type substitution: its
    /// value, and the parameter's declared type once known.
    Const(i128, Option<PrimTy>),
    /// A const parameter not bound yet (argument hints before inference).
    OpenConst(String),
    /// What a `rethrows` error parameter binds to for a non-throwing
    /// argument.
    NoThrow,
    /// An inference variable of an un-annotated `let` (`lower::infer`).
    Var(u32),
    /// A type parameter while its generic body is checked:
    /// opaque, with only what its bounds give.
    Param(u32),
    Sum(Vec<HirTy>),
    Tuple(Vec<HirTy>),
    /// A resolved struct.
    Struct(StructId),
    /// Resolved enum.
    Enum(EnumId),
    /// Fixed-size array `[T; N]`.
    Array(Box<HirTy>, u32),
    /// `fn(A, B) -> R`: a function pointer, with its bounds.
    Fn(Vec<HirTy>, Box<HirTy>, FnBounds),
    /// `{fn_ptr, env}`; the env type is part of the closure's identity.
    Closure {
        args: Vec<HirTy>,
        ret: Box<HirTy>,
        env: Box<HirTy>,
        /// It captures a `[ref x]` cell, which the creator keeps using:
        /// never `Send` / `Sync`.
        shares: bool,
        /// Bit `i`: env field `i` is a `[move x]` capture, which the
        /// creator no longer uses, so it needs only `Send`.
        moved: u64,
    },
    /// `throws (err) -> ok` carrier `{ is_err, ok, err }` (no unwinding);
    /// only as a return type, unpacked by `try`.
    Throws {
        ok: Box<HirTy>,
        err: Box<HirTy>,
    },
    /// `bare::mem::GcBuf<T>`: a GC object holding a run of `T`s (the
    /// storage of `List` / `Dict` / `Set`), one word; the owner keeps the
    /// length.
    GcBuf(Box<HirTy>),
    /// `!`: the type of a diverging expression (`panic(..)`, `exit(..)`,
    /// `loop {}` without `break`). Flows into any slot.
    Never,
}

impl HirTy {
    /// An `extern "C" fn(..)` type: a thin C function pointer, called
    /// with the C ABI, rather than a `{code, env}` value.
    #[must_use]
    pub const fn is_c_fn(&self) -> bool {
        matches!(self, Self::Fn(_, _, b) if b.c_abi)
    }

    /// `any (A + B)`: the bounds deduplicated in one canonical order
    /// (by `stable_key` rendering), so `any (A + B)` is `any (B + A)`.
    #[must_use]
    pub fn any(mut bounds: Vec<TraitRef>) -> Self {
        bounds.sort_by_cached_key(|b| b.render(&b.path, Self::stable_key));
        bounds.dedup();
        Self::AnyTrait(bounds)
    }

    /// A sum is a set. Nested sums flatten, duplicates drop and
    /// members take one canonical order, so `A | B` and `B | A` are one
    /// `HirTy`; a single member is that type. Build every `HirTy::Sum` here
    /// or with `layered` (or `canonicalize` after editing members).
    ///
    /// Each member sum is taken as a generic term and each other member
    /// as a written one (see `layered`): `item | none` with
    /// `item = i32 | none` is `(i32 | none)?`. Written sums are `spread`
    /// first. A canonical sum maps to itself.
    ///
    /// Order: `none` last, the rest by `canon_key`, so tags
    /// agree across separately compiled packages.
    #[must_use]
    pub fn sum(parts: impl IntoIterator<Item = Self>) -> Self {
        let (generic, written): (Vec<Self>, Vec<Self>) =
            parts.into_iter().partition(|p| matches!(p, Self::Sum(_)));
        Self::layered(written, generic)
    }

    /// Layers of generic sums: `written` are the terms written
    /// in the sum (written sums already `spread`; a sum among them is a
    /// nested layer kept as is), `generic` what its generic terms (type
    /// parameters, `Self`, projections, generic alias parameters) stand
    /// for. A generic term that is a sum sharing a term with `written`
    /// stays one nested layer; any other spreads. Overlap between generic
    /// terms does not nest: `T | U` with `T = i32 | none`, `U = none` is
    /// `i32 | none`.
    #[must_use]
    pub fn layered(written: Vec<Self>, generic: Vec<Self>) -> Self {
        let mut out = written;
        let own = out.clone();
        for t in generic {
            match t {
                Self::Sum(ps) if !ps.iter().any(|m| own.contains(m)) => out.extend(ps),
                t => out.push(t),
            }
        }
        out.sort_by_cached_key(|t| (t.is_none(), t.canon_key()));
        out.dedup();
        if out.len() == 1 {
            out.pop().expect("one member")
        } else {
            Self::Sum(out)
        }
    }

    /// A sum term's key in the canonical order: a primitive
    /// by its name, a struct or enum by its full path (an instance as
    /// `path<arg keys>`), a layer as `(` + its terms' keys joined by `|`
    /// + `)`. Other terms fall back to `stable_key`.
    #[must_use]
    pub fn canon_key(&self) -> String {
        match self {
            Self::Prim(p) => p.name().to_owned(),
            Self::None => "none".to_owned(),
            Self::Unit => "()".to_owned(),
            Self::Struct(id) => type_name(false, id.0).unwrap_or_else(|| self.stable_key()),
            Self::Enum(id) => type_name(true, id.0).unwrap_or_else(|| self.stable_key()),
            Self::Sum(ps) => {
                let keys: Vec<String> = ps.iter().map(Self::canon_key).collect();
                format!("({})", keys.join("|"))
            }
            _ => self.stable_key(),
        }
    }

    /// The structural rendering with each struct / enum id replaced by
    /// its name (`name_type`): the same in every build.
    #[must_use]
    pub fn stable_key(&self) -> String {
        let text = format!("{self:?}");
        TYPE_NAMES.with(|names| {
            let names = names.borrow();
            let mut out = String::with_capacity(text.len());
            let mut rest = text.as_str();
            loop {
                let next = [("Struct(StructId(", 0), ("Enum(EnumId(", 1)]
                    .into_iter()
                    .filter_map(|(p, k)| rest.find(p).map(|at| (at, p, k)))
                    .min();
                let Some((at, p, k)) = next else { break };
                let tail = &rest[at + p.len()..];
                let n = tail.bytes().take_while(u8::is_ascii_digit).count();
                let name = tail[..n]
                    .parse::<usize>()
                    .ok()
                    .and_then(|id| names[k].get(id))
                    .filter(|s| !s.is_empty() && tail[n..].starts_with("))"));
                out.push_str(&rest[..at]);
                if let Some(name) = name {
                    let kind = if k == 0 { "Struct" } else { "Enum" };
                    out.push_str(&format!("{kind}({name})"));
                    rest = &tail[n + 2..];
                } else {
                    out.push_str(&rest[at..at + p.len() + n]);
                    rest = &tail[n..];
                }
            }
            out.push_str(rest);
            out
        })
    }

    /// A written member of a written sum: its members join the outer set
    /// (`(A | B) | C` is `A | B | C`, flattening even `none` layers).
    pub fn spread(self, out: &mut Vec<Self>) {
        match self {
            Self::Sum(ps) => out.extend(ps),
            t => out.push(t),
        }
    }

    /// `A | B`, with a nested layer in parentheses: `(i32 | E) | E`,
    /// and `(i32 | none)?` for one beside just the outer `none`.
    pub fn sum_name(&self, name: impl Fn(&Self) -> String) -> String {
        let Self::Sum(ps) = self else {
            return name(self);
        };
        let member = |t: &Self| match t {
            Self::Sum(_) => format!("({})", name(t)),
            t => name(t),
        };
        match ps.as_slice() {
            [inner @ Self::Sum(_), n] if n.is_none() => format!("{}?", member(inner)),
            _ => ps.iter().map(member).collect::<Vec<_>>().join(" | "),
        }
    }

    /// The `none` type.
    #[must_use]
    pub const fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    /// A const argument's value and declared type.
    #[must_use]
    pub const fn as_const(&self) -> Option<(i128, Option<PrimTy>)> {
        match self {
            Self::Const(v, p) => Some((*v, *p)),
            _ => None,
        }
    }

    /// Re-establish the `sum` canon everywhere inside `self` (after
    /// members were rewritten in place: alias / generic substitution).
    pub fn canonicalize(&mut self) {
        match self {
            Self::Sum(ps) => {
                ps.iter_mut().for_each(Self::canonicalize);
                *self = Self::sum(std::mem::take(ps));
            }
            Self::Tuple(ps) => ps.iter_mut().for_each(Self::canonicalize),
            Self::Array(e, _) | Self::GcBuf(e) => e.canonicalize(),
            Self::Fn(a, r, _)
            | Self::Closure {
                args: a, ret: r, ..
            } => {
                a.iter_mut().for_each(Self::canonicalize);
                r.canonicalize();
            }
            Self::Throws { ok, err } => {
                ok.canonicalize();
                err.canonicalize();
            }
            _ => {}
        }
    }
}

/// Pre-order visit of every expression in `b`.
pub fn each_block<'a>(b: &'a HirBlock, f: &mut impl FnMut(&'a HirExpr)) {
    for s in &b.stmts {
        match s {
            HirStmt::Let { init: e, .. }
            | HirStmt::Assign { value: e, .. }
            | HirStmt::StaticAssign { value: e, .. }
            | HirStmt::Expr(e)
            | HirStmt::Panic(e)
            | HirStmt::Return(Some(e))
            | HirStmt::Break { value: Some(e), .. } => each_expr(e, f),
            HirStmt::ArrayAssign { index, value, .. } => {
                each_expr(index, f);
                each_expr(value, f);
            }
            HirStmt::Return(None) | HirStmt::Break { .. } | HirStmt::Continue { .. } => {}
        }
    }
    if let Some(t) = &b.trailing {
        each_expr(t, f);
    }
}

/// Pre-order visit of `e` and its subexpressions.
pub fn each_expr<'a>(e: &'a HirExpr, f: &mut impl FnMut(&'a HirExpr)) {
    f(e);
    let mut go = |x: &'a HirExpr| each_expr(x, f);
    match &e.kind {
        HirExprKind::Const(_)
        | HirExprKind::Local(_)
        | HirExprKind::StaticRef(_)
        | HirExprKind::UnresolvedPath(_)
        | HirExprKind::FnRef(_)
        | HirExprKind::SizeOf { .. }
        | HirExprKind::TypeDesc(_) => {}
        HirExprKind::Block(b) => each_block(b, f),
        HirExprKind::While { cond, body, .. } => {
            go(cond);
            each_block(body, f);
        }
        HirExprKind::Loop { body, .. } => each_block(body, f),
        HirExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            go(cond);
            each_block(then_block, f);
            if let Some(e) = else_branch {
                each_expr(e, f);
            }
        }
        HirExprKind::Match { scrutinee, arms } => {
            go(scrutinee);
            for a in arms {
                a.pat.each_expr(&mut |x| each_expr(x, f));
                if let Some(g) = &a.guard {
                    each_expr(g, f);
                }
                each_expr(&a.body, f);
            }
        }
        HirExprKind::PrimBinOp { lhs, rhs, .. } => {
            go(lhs);
            go(rhs);
        }
        HirExprKind::PrimUnaryOp { operand: x, .. }
        | HirExprKind::Cast { operand: x, .. }
        | HirExprKind::ClosureValue { env: x, .. }
        | HirExprKind::FieldRead { target: x, .. }
        | HirExprKind::TupleField { target: x, .. }
        | HirExprKind::ArrayRepeat { value: x, .. }
        | HirExprKind::BufNew { len: x, .. }
        | HirExprKind::RawRead { addr: x, .. }
        | HirExprKind::AddrOf { place: x }
        | HirExprKind::UnionWrite { value: x, .. }
        | HirExprKind::UnionRead { target: x, .. }
        | HirExprKind::Suspend { value: x, .. } => go(x),
        HirExprKind::SumWrap { value, .. } => {
            if let Some(v) = value {
                go(v);
            }
        }
        HirExprKind::Call { args: xs, .. }
        | HirExprKind::StructInit { fields: xs, .. }
        | HirExprKind::EnumVariantInit { args: xs, .. }
        | HirExprKind::Tuple { fields: xs }
        | HirExprKind::ArrayInit { elements: xs, .. }
        | HirExprKind::CoroNew { args: xs, .. } => xs.iter().for_each(go),
        HirExprKind::CallIndirect {
            callee: c, args, ..
        }
        | HirExprKind::CallClosure {
            closure: c, args, ..
        } => {
            go(c);
            args.iter().for_each(go);
        }
        HirExprKind::WithField { target, value, .. } => {
            go(target);
            go(value);
        }
        HirExprKind::RawWrite { addr, value, .. } => {
            go(addr);
            go(value);
        }
        HirExprKind::BufGet { buf, index, .. }
        | HirExprKind::ArrayIndex {
            array: buf, index, ..
        } => {
            go(buf);
            go(index);
        }
        HirExprKind::BufSet {
            buf, index, value, ..
        } => {
            go(buf);
            go(index);
            go(value);
        }
        HirExprKind::BufAtomic {
            buf, index, args, ..
        } => {
            go(buf);
            go(index);
            args.iter().for_each(go);
        }
    }
}

/// Pre-order visit of every expression in `b`, mutably.
pub fn each_block_mut(b: &mut HirBlock, f: &mut impl FnMut(&mut HirExpr)) {
    for s in &mut b.stmts {
        match s {
            HirStmt::Let { init: e, .. }
            | HirStmt::Assign { value: e, .. }
            | HirStmt::StaticAssign { value: e, .. }
            | HirStmt::Expr(e)
            | HirStmt::Panic(e)
            | HirStmt::Return(Some(e))
            | HirStmt::Break { value: Some(e), .. } => each_expr_mut(e, f),
            HirStmt::ArrayAssign { index, value, .. } => {
                each_expr_mut(index, f);
                each_expr_mut(value, f);
            }
            HirStmt::Return(None) | HirStmt::Break { .. } | HirStmt::Continue { .. } => {}
        }
    }
    if let Some(t) = &mut b.trailing {
        each_expr_mut(t, f);
    }
}

/// Pre-order visit of `e` and its subexpressions, mutably.
pub fn each_expr_mut(e: &mut HirExpr, f: &mut impl FnMut(&mut HirExpr)) {
    f(e);
    let mut go = |x: &mut HirExpr| each_expr_mut(x, f);
    match &mut e.kind {
        HirExprKind::Const(_)
        | HirExprKind::Local(_)
        | HirExprKind::StaticRef(_)
        | HirExprKind::UnresolvedPath(_)
        | HirExprKind::FnRef(_)
        | HirExprKind::SizeOf { .. }
        | HirExprKind::TypeDesc(_) => {}
        HirExprKind::Block(b) => each_block_mut(b, f),
        HirExprKind::Loop { body, .. } => each_block_mut(body, f),
        HirExprKind::While { cond, body, .. } => {
            go(cond);
            each_block_mut(body, f);
        }
        HirExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            go(cond);
            each_block_mut(then_block, f);
            if let Some(e) = else_branch {
                each_expr_mut(e, f);
            }
        }
        HirExprKind::Match { scrutinee, arms } => {
            go(scrutinee);
            for a in arms {
                a.pat.each_expr_mut(&mut |x| each_expr_mut(x, f));
                if let Some(g) = &mut a.guard {
                    each_expr_mut(g, f);
                }
                each_expr_mut(&mut a.body, f);
            }
        }
        HirExprKind::PrimBinOp { lhs: a, rhs: b, .. }
        | HirExprKind::WithField {
            target: a,
            value: b,
            ..
        }
        | HirExprKind::RawWrite {
            addr: a, value: b, ..
        }
        | HirExprKind::BufGet {
            buf: a, index: b, ..
        }
        | HirExprKind::ArrayIndex {
            array: a, index: b, ..
        } => {
            go(a);
            go(b);
        }
        HirExprKind::PrimUnaryOp { operand: x, .. }
        | HirExprKind::Cast { operand: x, .. }
        | HirExprKind::ClosureValue { env: x, .. }
        | HirExprKind::FieldRead { target: x, .. }
        | HirExprKind::TupleField { target: x, .. }
        | HirExprKind::ArrayRepeat { value: x, .. }
        | HirExprKind::BufNew { len: x, .. }
        | HirExprKind::RawRead { addr: x, .. }
        | HirExprKind::AddrOf { place: x }
        | HirExprKind::UnionWrite { value: x, .. }
        | HirExprKind::UnionRead { target: x, .. }
        | HirExprKind::Suspend { value: x, .. } => go(x),
        HirExprKind::SumWrap { value, .. } => {
            if let Some(v) = value {
                go(v);
            }
        }
        HirExprKind::Call { args: xs, .. }
        | HirExprKind::StructInit { fields: xs, .. }
        | HirExprKind::EnumVariantInit { args: xs, .. }
        | HirExprKind::Tuple { fields: xs }
        | HirExprKind::ArrayInit { elements: xs, .. }
        | HirExprKind::CoroNew { args: xs, .. } => xs.iter_mut().for_each(go),
        HirExprKind::CallIndirect {
            callee: c, args, ..
        }
        | HirExprKind::CallClosure {
            closure: c, args, ..
        } => {
            go(c);
            args.iter_mut().for_each(go);
        }
        HirExprKind::BufSet {
            buf, index, value, ..
        } => {
            go(buf);
            go(index);
            go(value);
        }
        HirExprKind::BufAtomic {
            buf, index, args, ..
        } => {
            go(buf);
            go(index);
            args.iter_mut().for_each(go);
        }
    }
}
