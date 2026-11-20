//! AST -> HIR lowering.

pub mod bounds;
pub mod collect;
pub mod consteval;
pub mod expr;
pub mod incr;
pub mod infer;
pub mod mono;
pub mod reflect;
pub mod resolve;
pub mod throws;

mod affine;
mod bignum;
mod broadcast;
mod caller;
mod checks;
mod compat;
mod coro;
mod desugar;
mod diag;
mod env;
mod fstring;
mod habits;
mod half;
mod hot;
mod lookup;
mod methodpath;
mod names;
mod objc;
mod pattern;
mod plugin;
mod scope;
mod suggest;
mod traits;
mod typedefs;
mod typing;

pub(crate) use crate::lower::compat::*;
pub(crate) use crate::lower::diag::*;
pub use crate::lower::env::*;
pub use crate::lower::names::type_visible;
pub(crate) use crate::lower::names::*;
pub(crate) use crate::lower::resolve::shadowing;
pub(crate) use crate::lower::typedefs::*;
pub use collect::{KeptLibrary, LibraryStage, keep_library, kept_library, lower_full, stage};
pub use coro::FN_BLOCK_LABEL;
pub use incr::{Bodies, BodyCache, Unit};
pub use resolve::{
    SHADOW, hidden_module, hidden_module_msg, implicit_use, is_dependency, module_visibility,
    root_path, set_packages, set_shadows, unshadowed, up_path, visibility_rank, visible_from,
    visible_paths,
};

use crate::ty::resolve_ty;
use crate::{
    EnumId, HirBlock, HirEnumDef, HirEnumVariant, HirExpr, HirExprKind, HirField, HirFunction,
    HirLocal, HirMatchArm, HirModule, HirParam, HirPattern, HirStaticDef, HirStmt, HirStructDef,
    HirTy, HirUnionKind, NicheArg, NicheVal, StructId, TraitRef, UnionStorage,
};
use collect::collect_unknown_paths;
use desugar::{
    desugar_nullish_coalesce, desugar_question_dot, desugar_try_q, lower_closure_expr, lower_for,
};
use expr::{
    build_any_dispatchers, enum_base_name, field_access, has_int_suffix, hir_binop, hir_int,
    hir_local, lower_block, lower_cond, lower_expr, lower_loop_body, lower_sibling_pair,
    method_call_on, parse_char_literal, parse_int_literal, resolve_enum_variant, variant_arg_tys,
};
use mono::{
    bind_from_arg, bind_self, generic_enum_variants, generic_struct_fields, generic_subst,
    lower_fn, monomorphize, monomorphize_on, resolve_in_mono, unify_param_ty_with,
};
use nabic_ast::{
    Block, CallArg, EnumDef as AstEnumDef, EnumVariantKind as AstEnumVariantKind, Expr, ExprKind,
    FnDef, ImplItem as AstImplItem, Item, Literal as AstLit, MatchArm as AstMatchArm, Param,
    Path as AstPath, Pattern as AstPattern, PatternKind as AstPatternKind, Program, Stmt, StmtKind,
    StructDef as AstStructDef, StructKind as AstStructKind, Ty, TyKind as AstTyKind,
};
use nabic_core::{HashMap, Visibility};
use nabic_diagnostic::{Code, Span};
use nabic_ir_common::{ConstVal, FunctionId, LocalId, PrimOp, PrimTy};
use nabic_parser::{Fixity, PrecTable};
use pattern::{int_fits, lower_match, lower_pattern};
use resolve::{
    COPY_TRAIT, DEFAULT_TRAIT, EQ_TRAIT, GC_BUF, impl_module, instance_mangle, is_toolchain_module,
    item_module, leaf_name, mangle_inst, method_target_for, module_display, module_of, package_of,
    resolve_name, resolve_ty_with_structs, substitute_params, ty_mangle,
};
use throws::{
    ErrFrame, adapt_fn_value, adapt_never_fn, erase_unbound_errors, exit_through,
    finish_throwing_body, jump_index, lower_catch, lower_return, lower_throw, lower_try,
    main_frame, rethrows_value, sig_ret_ast, uncaught_in_main, unpack_call,
};

/// `LowerCtx::access_module`: the current module, or a definition's.
enum AccessModule<'a> {
    Here(&'a str),
    Def(std::rc::Rc<str>),
}

impl std::ops::Deref for AccessModule<'_> {
    type Target = str;
    fn deref(&self) -> &str {
        match self {
            Self::Here(m) => m,
            Self::Def(m) => m,
        }
    }
}

/// Lower `program` alone: no `use` tables, default driver input.
#[must_use]
pub fn lower(program: &Program, prec: &PrecTable) -> HirModule {
    let empty_uses: crate::uses::UseTable = HashMap::default();
    let empty_reexports: Vec<crate::uses::ReExport> = Vec::new();
    lower_full(
        program.clone(),
        prec,
        &[],
        &empty_uses,
        &empty_reexports,
        &crate::HirInput::default(),
        &nabic_diagnostic::SourceMap::new(),
    )
}

#[must_use]
pub fn lower_with_uses(
    program: &Program,
    prec: &PrecTable,
    uses: &crate::uses::UseTable,
) -> HirModule {
    let empty_reexports: Vec<crate::uses::ReExport> = Vec::new();
    lower_full(
        program.clone(),
        prec,
        &[],
        uses,
        &empty_reexports,
        &crate::HirInput::default(),
        &nabic_diagnostic::SourceMap::new(),
    )
}

impl ClosureSink {
    fn new() -> Self {
        Self {
            extras: std::cell::RefCell::new(Vec::new()),
            next_id: std::cell::Cell::new(0),
            mono_cache: std::cell::RefCell::new(HashMap::default()),
            mono_sigs: std::cell::RefCell::new(HashMap::default()),
            mono_depth: std::cell::Cell::new(0),
            uninferred: std::cell::Cell::new(0),
            any_tags: std::cell::RefCell::new(Vec::new()),
            ref_out: std::cell::RefCell::new(HashMap::default()),
            any_dispatch: std::cell::RefCell::new(Vec::new()),
            projecting: std::cell::RefCell::new(Vec::new()),
            thunks: std::cell::RefCell::new(HashMap::default()),
            instances: std::cell::RefCell::new(Vec::new()),
            instance_warns: std::cell::RefCell::new(InstanceWarns::default()),
            diagnostics: std::cell::RefCell::new(Vec::new()),
            infer_vars: std::cell::RefCell::new(Vec::new()),
            infer_depth: std::cell::Cell::new(0),
            infer_floor: std::cell::Cell::new(0),
            fn_origin: std::cell::RefCell::new(HashMap::default()),
            track_caller: std::cell::RefCell::new(nabic_core::HashSet::default()),
            const_lowering: std::cell::RefCell::new(nabic_core::HashSet::default()),
            const_stack: std::cell::RefCell::new(Vec::new()),
            inferred_ret: std::cell::RefCell::default(),
            inferred_fns: std::cell::RefCell::default(),
            bodies_done: std::cell::Cell::new(false),
            sig_pass: std::cell::Cell::new(None),
            recursive_ret: std::cell::RefCell::default(),
            call_sites: std::cell::RefCell::default(),
            tentative_ret: std::cell::RefCell::default(),
            params: std::cell::RefCell::default(),
            alias_names: std::cell::RefCell::default(),
            failed: std::cell::RefCell::default(),
            unit_ret_errs: std::cell::RefCell::default(),
            opaque_called: std::cell::Cell::default(),
            opaque_check: std::cell::Cell::default(),
            opaque_params: std::cell::RefCell::default(),
            some_view: std::cell::RefCell::default(),
            some_params: std::cell::RefCell::default(),
            default_lits: std::cell::RefCell::default(),
            lit_open: std::cell::Cell::default(),
            implicit_depth: std::cell::Cell::default(),
            lit_vars: std::cell::RefCell::default(),
            open_insts: std::cell::RefCell::default(),
            bodies: std::cell::RefCell::default(),
            assumed: std::cell::RefCell::default(),
            const_names: std::cell::RefCell::default(),
            dropped_at: std::cell::Cell::new(None),
        }
    }
}

/// A handler registered with a built-in shape's pattern (`[..]` etc.).
#[derive(Clone)]
pub(crate) struct ShapeHandler {
    shape: nabic_parser::Shape,
    /// The handler's full name.
    name: String,
    priority: i64,
    /// The `#[literal]` attribute.
    span: nabic_diagnostic::Span,
}

/// What `any Tr` needs from a trait declaration.
#[derive(Clone)]
pub(crate) struct TraitInfo {
    methods: Vec<String>,
    supers: Vec<String>,
    /// Per method, what its signature past `self` depends on (`Self`,
    /// `Self::X`, trait parameters); `any Tr` dispatch needs the bound to
    /// fix each.
    needs: HashMap<String, Vec<String>>,
    /// Method declarations (signature when no concrete impl exists).
    decls: HashMap<String, nabic_ast::FnDef>,
    /// The trait's module (to resolve `decls`).
    module: String,
    /// The trait's type parameters, in order (`Index<K>` -> `[K]`).
    params: Vec<String>,
    /// Its generic parameters as declared (defaults included).
    generics: nabic_ast::Generics,
    /// Its supertraits as written.
    super_tys: Vec<Ty>,
    /// Its associated types and their declared bounds.
    assoc: Vec<(String, Vec<Ty>)>,
    /// Its associated consts and their declared types.
    consts: Vec<(String, Ty)>,
}

#[derive(Clone, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub enum MethodTarget {
    Struct(StructId),
    Enum(EnumId),
    /// Impl targets that aren't one struct or enum, keyed by
    /// `ty_mangle` so impls on one shape share a target.
    Mangled(String),
}

#[derive(Clone)]
pub(crate) enum MethodOrFn<'a> {
    Fn(&'a FnDef),
    /// `module` is where the body's names resolve: the impl's module
    /// for explicit methods, the trait's module for default bodies.
    Method {
        /// Borrowed from the program, or a trait default with the
        /// impl's trait arguments substituted into its signature.
        fn_def: std::borrow::Cow<'a, FnDef>,
        self_ty: HirTy,
        module: String,
        /// `fn_table` key, also the function's name.
        key: String,
        /// A trait default body's trait parameters -> this impl's
        /// arguments (`trait Dim<const D: usize>` + `impl Dim<3>`).
        subst: HashMap<String, HirTy>,
        /// The trait's default body, not a method the impl wrote.
        default: bool,
    },
}

/// Where a free function's names resolve: its module, or for an
/// `extern` declaration (not a module item) the module declaring it.
pub fn fn_module(env: &Env<'_>, f: &FnDef) -> String {
    env.extern_fn(&f.name)
        .cloned()
        .unwrap_or_else(|| module_of(&f.name))
}

impl MethodOrFn<'_> {
    fn def(&self) -> &FnDef {
        match self {
            Self::Fn(f) => f,
            Self::Method { fn_def, .. } => fn_def,
        }
    }
}

/// A function with `ref` parameters, copy-in / copy-out: it
/// returns `ret` packed with each `ref` parameter's final value
/// (`ref_packed`), which the call site writes back.
#[derive(Clone)]
pub struct RefOut {
    /// The declared return (a `Throws` carrier for a throwing fn).
    pub ret: HirTy,
    /// `(parameter index, type)` of each `ref` parameter.
    pub refs: Vec<(usize, HirTy)>,
    /// An `async` method's `ref self`: `refs` hold the parameters' cells
    /// (`GcBuf<T>`), read back once the future completes (`ref_unpack`).
    pub deferred: bool,
}

/// A projection being resolved: base type, associated name, trait filter.
type ProjectionGoal = (HirTy, String, Option<Vec<String>>);

/// Per-function signature for lowering; `param_defaults[i]` is the
/// parameter's default expression, if any.
#[derive(Clone)]
pub struct FnSig {
    pub id: FunctionId,
    pub ret_ty: HirTy,
    pub param_defaults: Vec<Option<Expr>>,
    /// Resolved parameter types: the expected types of the arguments, so
    /// an untyped closure argument takes its parameter types from them.
    pub param_tys: Vec<HirTy>,
    /// Parameter names, to place named arguments.
    pub param_names: Vec<String>,
    /// Which parameters are `ref` (empty = none).
    pub param_refs: Vec<bool>,
    /// The last parameter is a variadic `...T` (a `Slice<T>` the call
    /// builds from its trailing arguments).
    pub variadic: bool,
}

/// Outcome of resolving `[..enum path.., Variant]`.
pub enum EnumRes {
    /// The prefix doesn't name an enum.
    NotEnum,
    Found(EnumId, usize),
    /// An enum was named but the variant / instantiation is wrong
    /// (already reported).
    Error,
}

/// Where call arguments come from: still-AST expressions (lowered
/// against the parameter type) or values already lowered (spread).
enum ArgSrc<'e> {
    Ast(&'e Expr),
    Lowered(HirExpr),
    /// `ref x`.
    Ref(&'e Expr),
    /// A dict spread's read of this parameter: the index of
    /// its built expression.
    Dict(usize),
}

type TySubst = HashMap<String, HirTy>;

#[derive(Clone, Default)]
struct InstanceWarns {
    instances: HashMap<nabic_diagnostic::Span, nabic_core::HashSet<FunctionId>>,
    warns: Vec<(
        nabic_diagnostic::Span,
        nabic_diagnostic::NabiDiagnostic,
        nabic_core::HashSet<FunctionId>,
    )>,
}

impl InstanceWarns {
    /// Instance `id` of the template at `template` is lowered.
    fn instance(&mut self, template: nabic_diagnostic::Span, id: FunctionId) {
        incr::log_warn(|| incr::WarnEvent::Instance(template, id));
        self.instances.entry(template).or_default().insert(id);
    }

    /// Instance `id` of the template at `template` raised `d`.
    fn warn(
        &mut self,
        template: nabic_diagnostic::Span,
        d: nabic_diagnostic::NabiDiagnostic,
        id: FunctionId,
    ) {
        incr::log_warn(|| incr::WarnEvent::Warn(template, d.clone(), id));
        let same = |(t, w, _): &&mut (_, nabic_diagnostic::NabiDiagnostic, _)| {
            *t == template && w.message == d.message && w.primary == d.primary
        };
        if let Some((_, _, by)) = self.warns.iter_mut().find(same) {
            by.insert(id);
        } else {
            self.warns
                .push((template, d, std::iter::once(id).collect()));
        }
    }

    /// The warnings every instance of their template raised.
    fn agreed(self) -> impl Iterator<Item = nabic_diagnostic::NabiDiagnostic> {
        let instances = self.instances;
        self.warns
            .into_iter()
            .filter(move |(t, _, by)| instances.get(t).is_some_and(|all| all.is_subset(by)))
            .map(|(_, d, _)| d)
    }
}

#[derive(Clone)]
pub struct ClosureSink {
    pub extras: std::cell::RefCell<Vec<HirFunction>>,
    /// Functions with `ref` parameters (`ref self` included).
    pub ref_out: std::cell::RefCell<HashMap<FunctionId, RefOut>>,
    pub next_id: std::cell::Cell<u32>,
    /// Cached monomorphizations: `(generic_fn_name, concrete_type_args)`
    /// -> specialized `FunctionId`.
    pub mono_cache: std::cell::RefCell<HashMap<(String, Vec<String>), FunctionId>>,
    /// Specialized `FnSig` per monomorphization for call-site lookup.
    pub mono_sigs: std::cell::RefCell<HashMap<FunctionId, FnSig>>,
    /// Current monomorphization nesting (bounded by `MONO_DEPTH_LIMIT`).
    pub mono_depth: std::cell::Cell<u32>,
    /// Bumped by each "type annotations needed" failure, so an unannotated
    /// `let` knows to look at later uses.
    uninferred: std::cell::Cell<u32>,
    /// `any` type tags of the concrete types stored so far (`any_tag`).
    any_tags: std::cell::RefCell<Vec<(HirTy, u32)>>,
    /// `any Tr` method dispatchers (bodies built after all lowering).
    any_dispatch: std::cell::RefCell<Vec<expr::AnyDispatch>>,
    /// `T::Item` projections being resolved (cuts `impl<I: Iterator>
    /// IntoIterator for I { type Item = I::Item; }` cycles).
    projecting: std::cell::RefCell<Vec<ProjectionGoal>>,
    /// Adapter thunks (`adapt_fn_value` / `adapt_never_fn`) by
    /// `kind|callee|slot type|callee type`.
    thunks: std::cell::RefCell<HashMap<String, FunctionId>>,
    /// Generic instance bodies being lowered: `(template span, instance
    /// id)`, `None` for a non-instance body nested in one.
    instances: std::cell::RefCell<Vec<Option<(nabic_diagnostic::Span, FunctionId)>>>,
    /// Instances per template and the warnings each raised: a warning is
    /// reported only if every instance raised it.
    instance_warns: std::cell::RefCell<InstanceWarns>,
    /// Diagnostics emitted during lowering (`HirModule.diagnostics`).
    diagnostics: std::cell::RefCell<Vec<nabic_diagnostic::NabiDiagnostic>>,
    /// Inference variables (`infer`): one per open type parameter of an
    /// un-annotated `let` being inferred from its later uses.
    infer_vars: std::cell::RefCell<Vec<infer::Var>>,
    /// `let` inferences in progress: unification only runs when > 0.
    infer_depth: std::cell::Cell<u32>,
    /// Variables before the outermost inference in progress.
    infer_floor: std::cell::Cell<usize>,
    /// Every lowered function's `(const fn, standard library)`.
    fn_origin: std::cell::RefCell<HashMap<FunctionId, (bool, bool)>>,
    /// Lowered `#[track_caller]` functions (`caller`).
    track_caller: std::cell::RefCell<nabic_core::HashSet<FunctionId>>,
    /// Declarations being lowered for a compile-time evaluation.
    const_lowering: std::cell::RefCell<nabic_core::HashSet<FunctionId>>,
    /// Const items being lowered, innermost last (cycle check).
    const_stack: std::cell::RefCell<Vec<String>>,
    /// Return types inferred from bodies, by function;
    /// `None` while the body is being inferred.
    inferred_ret: std::cell::RefCell<HashMap<FunctionId, Option<HirTy>>>,
    /// Declarations whose inference lowering was final, with the first
    /// function id they could have made (for `rollback`); the decl pass
    /// reuses them.
    inferred_fns: std::cell::RefCell<HashMap<FunctionId, (u32, HirFunction)>>,
    /// Every reachable body is lowered: a later inference (a signature
    /// asked for by a check) must leave no functions behind.
    bodies_done: std::cell::Cell<bool>,
    /// The first round of signature types (`Some`): whether a type
    /// waited for the full const evaluator.
    sig_pass: std::cell::Cell<Option<bool>>,
    /// Call sites of the generic instances being lowered, outermost
    /// first, and whether the callee is an implicit generic (its body
    /// errors point at the call too).
    pub call_sites: std::cell::RefCell<Vec<(String, nabic_diagnostic::Span, bool)>>,
    /// Calls met of functions whose return type is being inferred.
    pub recursive_ret: std::cell::RefCell<Vec<FunctionId>>,
    /// Inferred return types that depend on a fn still being inferred:
    /// dropped once it is done.
    tentative_ret: std::cell::RefCell<Vec<FunctionId>>,
    /// Type parameters of the generic bodies being checked
    /// (`HirTy::Param`, `bounds`).
    params: std::cell::RefCell<Vec<bounds::ParamInfo>>,
    /// Generic types met through an alias outside the standard library,
    /// `(type text, alias as written, package)`: diagnostics name the
    /// alias (`Velocity`, not `Quantity<f64, 1, 0, -1, 0, 0, 0, 0>`).
    alias_names: std::cell::RefCell<Vec<(String, String, String)>>,
    /// Templates whose definition check failed, by span: their
    /// instances (and a trait default's copies) report nothing more.
    failed: std::cell::RefCell<nabic_core::HashSet<Span>>,
    /// `pub` fns whose body gave a value without `->`, by span: one
    /// error for all their instances.
    unit_ret_errs: std::cell::RefCell<nabic_core::HashSet<Span>>,
    /// A body called a `-> some ..` fn (`mono::check_opaque_uses`).
    opaque_called: std::cell::Cell<bool>,
    /// Checking a body with each `-> some Tr` call's result an opaque
    /// type parameter bounded by `Tr`.
    opaque_check: std::cell::Cell<bool>,
    opaque_params: std::cell::RefCell<HashMap<FunctionId, HirTy>>,
    /// The module whose body `opaque_check` checks: named `some` types of
    /// other modules are opaque there.
    some_view: std::cell::RefCell<Option<String>>,
    /// A named `some` type as an opaque parameter, by full name.
    some_params: std::cell::RefCell<HashMap<String, HirTy>>,
    /// Unsuffixed number literals typed by default (nothing expected one):
    /// a slot of the same kind retypes them (`coerce`).
    pub default_lits: std::cell::RefCell<nabic_core::HashSet<Span>>,
    /// The body whose unsuffixed literals are inference variables
    /// (`infer::open_scope`); each is recorded in `lit_vars`.
    lit_open: std::cell::Cell<Option<Span>>,
    /// Lowering an implicit generic's body for its open arguments
    /// (`mono::open_body`): not again inside it.
    pub implicit_depth: std::cell::Cell<u32>,
    lit_vars: std::cell::RefCell<Vec<(Span, u32)>>,
    /// Instances asked for over open literal types, made once solved.
    pub open_insts: std::cell::RefCell<Vec<mono::OpenInst>>,
    /// The declarations whose bodies are being lowered, innermost last.
    bodies: std::cell::RefCell<Vec<Span>>,
    /// The `where` clauses on other types than a parameter of the body
    /// being checked.
    assumed: std::cell::RefCell<Vec<(HirTy, TraitRef)>>,
    /// The `const` parameters of the body being checked, by the value
    /// standing for each (`bounds`).
    const_names: std::cell::RefCell<Vec<(i128, String)>>,
    /// `diagnostics.len()` when `emit_diag` last dropped one: the
    /// `*_last` helpers then leave the earlier diagnostic alone.
    dropped_at: std::cell::Cell<Option<usize>>,
}

/// Per-function lowering state: scoped name -> local-id map plus the
/// running list of local declarations, on top of the shared `Env`.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)] // independent per-body modes
pub struct LowerCtx<'a> {
    pub env: &'a Env<'a>,
    locals: Vec<HirLocal>,
    scopes: Vec<HashMap<String, LocalId>>,
    /// This instance's type-parameter substitution (empty if not generic);
    /// overrides same-named aliases. Owned while an associated const lowers
    /// under its impl's.
    ty_subst: std::borrow::Cow<'a, TySubst>,
    self_ty: Option<HirTy>,
    /// Enclosing function's (or closure's) return type. `return v;`
    /// checks against it and wraps `v` into a matching sum type.
    fn_ret_ty: HirTy,
    /// Closure without a declared / expected return type: the first
    /// `return v;` fixes `fn_ret_ty`.
    infer_ret: bool,
    /// Inferring a function's return type: the types of
    /// its `return` values and trailing value.
    ret_tys: Option<Vec<HirTy>>,
    /// Lowering the implicit `main`, where `return` is banned;
    /// closures get a fresh context.
    implicit_main: bool,
    /// Lowering the test harness `main` (`RootMain::Harness`).
    harness_main: bool,
    /// The `ref` parameters' locals and types (`ref self` included);
    /// every return also hands their values back (copy-out).
    ref_params: Vec<(LocalId, HirTy)>,
    /// `ref_params` are cells handed back as they are (an `async` method's
    /// `ref self`, `mono::defers_refs`).
    ref_cells: bool,
    /// `await e`: the span of `e`, whose deferred `ref` write-back
    /// (`mono::ref_unpack`) waits for the future, and that write-back.
    awaited: Option<nabic_diagnostic::Span>,
    await_write_back: Vec<HirStmt>,
    /// Bindings some closure captures as `[ref x]`, by name and
    /// span; they live in a one-element `GcBuf` cell.
    ref_captured: nabic_core::HashSet<(String, nabic_diagnostic::Span)>,
    /// Pattern bindings (`declare_bound`) still to be moved into their
    /// cells (`take_cells`) before the code in their scope runs.
    pending_cells: Vec<(String, LocalId, HirTy)>,
    /// Sub-sum type-pattern binders (`v: A | B` against `A | B | C`):
    /// `(binder, whole-scrutinee local)`, converted by `take_cells`.
    pending_resums: Vec<(LocalId, LocalId)>,
    /// In a closure: locals holding a value capture (assigning one is an
    /// error).
    value_captured: nabic_core::HashSet<LocalId>,
    /// Set by a path call whose first argument fills `self`: whether it was
    /// `ref`. Taken by that call's argument lowering.
    explicit_recv: Option<bool>,
    /// The slice expression being lowered fills a generic `SliceMut<T>`
    /// parameter: a list's range is a writable view.
    want_view: bool,
    /// A dot tree typed through its traits only (`broadcast`): its
    /// subtrees are not fused.
    dot_types_only: bool,
    /// Spans of calls whose `ref self` receiver is no place to write
    /// back to (an element, a static, a const): the write is dropped.
    dropped_ref_recv: Vec<Span>,
    /// Enclosing `unsafe fn` body / `unsafe { }` blocks:
    /// unsafe calls are allowed when > 0.
    unsafe_depth: u32,
    /// The macOS versions `if objc::available(..)` blocks around the
    /// code being lowered check.
    available: Vec<(u32, u32, u32)>,
    /// Set by a call through a lang slot (`expr::lang_fn`): lowering
    /// made it, so the hook's own marks do not warn. Taken by the call.
    hook_call: std::cell::Cell<bool>,
    /// Spans of calls to `#[discardable]` functions (`unused_result`).
    discardable_calls: std::cell::RefCell<Vec<Span>>,
    /// Lowering `addr_of`'s place: taking an extern static's address
    /// reads nothing.
    addr_place: bool,
    /// Enclosing breakable constructs, innermost last: `(label, is_loop)`.
    /// A labelled block takes `break 'l` but not `continue`.
    breakables: Vec<(Option<String>, bool)>,
    /// `breakables` index -> the type its `break` values must have:
    /// the annotated slot (`true` = inferred from the first `break`).
    break_tys: HashMap<usize, (HirTy, bool)>,
    /// `breakables` index -> the sum a later `break` value joined the
    /// inferred type into: its earlier `break`s are lowered again into it.
    break_joined: HashMap<usize, HirTy>,
    /// The type `open_break_target` gives that index instead of the slot's.
    break_preset: Option<(usize, HirTy)>,
    /// `breakables` indices of `while` / `for` loops: their value is `()`.
    unit_loops: nabic_core::HashSet<usize>,
    /// Expected types. A sub-expression with its own slot pushes it (or
    /// `Unknown`), so leaves only see the slot they flow into.
    expected_ty_stack: Vec<HirTy>,
    /// Module path of the function being lowered (`""` at the root).
    pub current_module: String,
    /// While a name a meta template or the compiler wrote is resolved:
    /// the module of its definition, whose scope and access it has.
    access: std::cell::RefCell<Option<std::rc::Rc<str>>>,
    /// Block-scoped `use`s, one level per `push_scope`, searched before
    /// module uses.
    local_uses: Vec<Vec<crate::uses::UseEntry>>,
    /// Per scope: nested fns by name -> lifted fn key.
    nested_fns: Vec<HashMap<String, String>>,
    /// Or-pattern binders: name -> preset local, so every alternative binds
    /// `x` once with one type.
    or_pattern_binders: Vec<HashMap<String, LocalId>>,
    /// `f::<T, 3>(..)` arguments for the generic call being lowered,
    /// read by `monomorphize_on` (empty otherwise).
    pub explicit_generics: HashMap<String, HirTy>,
    /// In a trait's default body: the trait and the impl's arguments,
    /// so `self.m(..)` is that impl's `m`, not an inherent one.
    default_of: Option<(String, Vec<HirTy>)>,
    /// `x.m::<T>(..)`: the method call's explicit type arguments, taken
    /// by `method_call_on`.
    method_type_args: Vec<Ty>,
    /// `x.m(..)` as written: only the traits in scope give `m`, as
    /// taken by `method_call_on` (a call the compiler makes sees all).
    user_method: bool,
    /// `<T as Tr<A>>::m(..)` / `Tr::<A>::m(..)`: the impl of those trait
    /// arguments among several of `Tr`.
    pub(crate) want_trait: Option<TraitRef>,
    /// Error type of the enclosing function's / closure's `throws`
    /// (`None`: no error may leave it).
    fn_throws: Option<HirTy>,
    /// Enclosing prefix `try`s; a throwing call must sit under one.
    try_depth: u32,
    /// `try for`: the `try_depth` outside the `try`, set for the `for`
    /// it marks, which takes it (the `try` covers the iteration only).
    try_for: Option<u32>,
    /// That `for`'s body block and the `try_depth` it is lowered at.
    try_for_body: Option<(Span, u32)>,
    /// Inside the body of a `try for` (a throwing call there gets a hint).
    in_try_for_body: u32,
    /// Enclosing `catch` / `try?` / `try!` / `finally` frames, innermost
    /// last (see `throws`).
    err_frames: Vec<ErrFrame>,
    /// By-value parameters whose release the body owes (`affine`).
    owe_params: Vec<(LocalId, HirTy)>,
    /// Where each local with an inserted release was bound: what an error
    /// in that release (it has no span) points at.
    owed_at: HashMap<LocalId, nabic_diagnostic::Span>,
    /// Probe pass over a closure body: collects the error types that
    /// escape it instead of reporting them.
    throws_probe: Option<Vec<HirTy>>,
    /// Lowering the body of `fn main() throws (E)`: an escaping error
    /// ends the process.
    uncaught_in_main: bool,
    /// Lowering a `catch` dispatch: the local holding the site of the
    /// error it passes on when no arm takes it.
    rethrow_site: Option<LocalId>,
    /// Throwing calls lowered so far (a `try` covering none is
    /// pointless).
    throwing_calls: u32,
    /// Counter for synthesized labels and local names.
    next_synth: u32,
    /// Narrowed rebinding -> (original local, its type);
    /// assignments to the rebinding also store into the original.
    narrowed: HashMap<LocalId, (LocalId, HirTy)>,
    /// A `match` arm's binding of the whole matched place -> that place:
    /// a loop stepping the binding in place writes it back there.
    arm_bound: HashMap<LocalId, HirExpr>,
    /// Narrowing of a `[ref]` cell -> the read of its rebinding: each read
    /// tests the cell again, as a capture may have changed it.
    cell_views: HashMap<LocalId, HirExpr>,
    /// A `for ref` binding -> the element place it names (`xs[i]`). Reads
    /// and writes of the binding lower that place instead.
    places: HashMap<LocalId, Expr>,
    /// Narrowings ended by assignment: (name, rebinding, scope
    /// index). Branches undo and rejoin them.
    unnarrowed: Vec<(String, LocalId, usize)>,
    /// Lock guards bound but not yet released by `using` (local, depth,
    /// span, binder, type); an error when their scope ends.
    guard_lets: Vec<(LocalId, usize, Span, String, &'static str)>,
    /// Locals bound by an unannotated `let` to an unsuffixed number
    /// literal: its text and span. A type error on one points at the
    /// declaration.
    lit_lets: HashMap<LocalId, (String, Span)>,
    /// Expressions (by address) where a guard may stay unreleased:
    /// initializers, assigned values, `return`, the body's value
    /// (`expr::guard_sink`). Elsewhere a produced guard is never released
    /// and a read guard variable is moved.
    guard_sinks: nabic_core::HashSet<usize>,
    /// Names the function body binds once and names once after
    /// (`sole_names`): a fresh future in one is its only handle.
    sole_names: nabic_core::HashSet<String>,
    /// Locals holding a fresh future nothing else names
    /// (`expr::plain_fresh_future`), with the loop depth they were made
    /// at.
    sole_fresh: HashMap<LocalId, usize>,
    /// Type parameter -> its trait bounds and their supertraits (trait
    /// keys): `T::Item` resolves through them (`set_bounds`).
    ty_bounds: HashMap<String, Vec<String>>,
    /// Type parameter -> its bounds as written, so `T::Error` takes the
    /// `TryFrom<S>` impl, not any `TryFrom`.
    ty_bound_args: HashMap<String, Vec<AstPath>>,
    /// Lowering a coroutine body (`yield` / `await` are allowed).
    coro: Option<coro::CoroCtx>,
    /// Associated types of the enclosing trait / impl
    /// (`ASSOC_TYPES_ATTR`): a bare `X` is `Self::X`.
    assoc_names: Vec<String>,
    /// Lowering an unannotated `let`'s initializer or what follows for
    /// inference: open type parameters become inference variables
    /// (`infer`), not errors.
    pub infer_vars: bool,
    /// Already lowered expressions an AST desugar refers to by a `#sN`
    /// path (`expr::splice`).
    splices: HashMap<String, HirExpr>,
    /// What is being lowered (the expression, statement or item): where
    /// diagnostics without a location of their own go.
    pub at: std::cell::Cell<Span>,
    /// Temporaries named so far (`expr::tmp_path`): their names count
    /// per body, so a body lowers the same whatever was lowered before.
    temps: std::cell::Cell<u32>,
    /// The `: T` of the `let` whose value the next `coerce` checks.
    let_ann: std::cell::Cell<Option<Span>>,
    /// Inside a branch a constant condition never takes (`if N == 0 {
    /// .. } else { N - 1 }` at `N = 0`): it never runs, so its constant
    /// overflow is no error.
    dead: u32,
}

/// Where a speculative lowering started (`probe_mark`); `rollback`
/// undoes everything since.
pub struct ProbeMark {
    diagnostics: usize,
    uninferred: u32,
    next_id: u32,
    any_tags: usize,
    locals: usize,
}

impl<'a> LowerCtx<'a> {
    pub fn probe_mark(&self) -> ProbeMark {
        ProbeMark {
            diagnostics: self.env.sink.diagnostics.borrow().len(),
            uninferred: self.env.sink.uninferred.get(),
            next_id: self.env.sink.next_id.get(),
            any_tags: self.env.sink.any_tags.borrow().len(),
            locals: self.locals.len(),
        }
    }

    /// Whether nothing was reported since `mark`.
    fn probe_clean(&self, mark: &ProbeMark) -> bool {
        self.env.sink.diagnostics.borrow().len() == mark.diagnostics
    }

    /// Undo everything since `mark`: locals, diagnostics (returned), and
    /// every function made since (closures, thunks, instances and their
    /// cache entries). Struct / enum instances stay;
    /// `infer::drop_var_instances` drops the open ones.
    pub fn rollback(&mut self, mark: &ProbeMark) -> Vec<nabic_diagnostic::NabiDiagnostic> {
        self.locals.truncate(mark.locals);
        let kept = |l: &LocalId| (l.0 as usize) < mark.locals;
        for scope in &mut self.scopes {
            scope.retain(|_, l| kept(l));
        }
        self.value_captured.retain(kept);
        self.narrowed.retain(|l, _| kept(l));
        self.arm_bound.retain(|l, _| kept(l));
        self.cell_views.retain(|l, _| kept(l));
        self.places.retain(|l, _| kept(l));
        self.guard_lets.retain(|g| kept(&g.0));
        self.lit_lets.retain(|l, _| kept(l));
        self.unnarrowed.retain(|u| kept(&u.1));
        let sink = &self.env.sink;
        let old = |id: &FunctionId| id.0 < mark.next_id;
        let dropped = sink.diagnostics.borrow_mut().split_off(mark.diagnostics);
        sink.uninferred.set(mark.uninferred);
        sink.extras.borrow_mut().retain(|f| old(&f.id));
        sink.mono_cache.borrow_mut().retain(|_, id| old(id));
        sink.mono_sigs.borrow_mut().retain(|id, _| old(id));
        sink.ref_out.borrow_mut().retain(|id, _| old(id));
        sink.inferred_fns
            .borrow_mut()
            .retain(|_, (start, _)| *start < mark.next_id);
        sink.thunks.borrow_mut().retain(|_, id| old(id));
        sink.any_dispatch.borrow_mut().retain(|d| old(&d.id));
        sink.any_tags.borrow_mut().truncate(mark.any_tags);
        let mut iw = sink.instance_warns.borrow_mut();
        for set in iw.instances.values_mut() {
            set.retain(old);
        }
        iw.warns.retain(|(_, _, by)| by.iter().any(old));
        dropped
    }

    /// A "type annotations needed" error (see `ClosureSink::uninferred`).
    pub fn emit_uninferred(&self, msg: String) {
        self.env
            .sink
            .uninferred
            .set(self.env.sink.uninferred.get() + 1);
        self.emit_error(
            Code::TYPE_ANNOTATIONS_NEEDED,
            format!("type annotations needed: {msg}"),
        );
    }

    /// A context lowering at `at` (see `LowerCtx::at`).
    pub fn new(
        env: &'a Env<'a>,
        ty_subst: &'a HashMap<String, HirTy>,
        self_ty: Option<HirTy>,
        at: Span,
    ) -> Self {
        Self::with_subst(env, std::borrow::Cow::Borrowed(ty_subst), self_ty, at)
    }

    fn with_subst(
        env: &'a Env<'a>,
        ty_subst: std::borrow::Cow<'a, TySubst>,
        self_ty: Option<HirTy>,
        at: Span,
    ) -> Self {
        Self {
            env,
            locals: Vec::new(),
            scopes: vec![HashMap::default()],
            ty_subst,
            self_ty,
            fn_ret_ty: HirTy::Unit,
            infer_ret: false,
            ret_tys: None,
            implicit_main: false,
            harness_main: false,
            ref_params: Vec::new(),
            ref_cells: false,
            awaited: None,
            await_write_back: Vec::new(),
            ref_captured: nabic_core::HashSet::default(),
            pending_cells: Vec::new(),
            pending_resums: Vec::new(),
            value_captured: nabic_core::HashSet::default(),
            unsafe_depth: 0,
            available: Vec::new(),
            hook_call: std::cell::Cell::new(false),
            discardable_calls: std::cell::RefCell::new(Vec::new()),
            addr_place: false,
            explicit_recv: None,
            want_view: false,
            dot_types_only: false,
            dropped_ref_recv: Vec::new(),
            breakables: Vec::new(),
            break_tys: HashMap::default(),
            break_joined: HashMap::default(),
            break_preset: None,
            unit_loops: nabic_core::HashSet::default(),
            expected_ty_stack: Vec::new(),
            current_module: String::new(),
            access: std::cell::RefCell::new(None),
            local_uses: vec![Vec::new()],
            nested_fns: vec![HashMap::default()],
            or_pattern_binders: Vec::new(),
            explicit_generics: HashMap::default(),
            default_of: None,
            method_type_args: Vec::new(),
            user_method: false,
            want_trait: None,
            fn_throws: None,
            try_depth: 0,
            try_for: None,
            try_for_body: None,
            in_try_for_body: 0,
            err_frames: Vec::new(),
            owe_params: Vec::new(),
            owed_at: HashMap::default(),
            throws_probe: None,
            uncaught_in_main: false,
            rethrow_site: None,
            throwing_calls: 0,
            next_synth: 0,
            narrowed: HashMap::default(),
            arm_bound: HashMap::default(),
            cell_views: HashMap::default(),
            places: HashMap::default(),
            unnarrowed: Vec::new(),
            guard_lets: Vec::new(),
            lit_lets: HashMap::default(),
            guard_sinks: nabic_core::HashSet::default(),
            sole_names: nabic_core::HashSet::default(),
            sole_fresh: HashMap::default(),
            ty_bounds: HashMap::default(),
            ty_bound_args: HashMap::default(),
            coro: None,
            assoc_names: Vec::new(),
            infer_vars: false,
            splices: HashMap::default(),
            at: std::cell::Cell::new(at),
            temps: std::cell::Cell::new(0),
            let_ann: std::cell::Cell::new(None),
            dead: 0,
        }
    }

    /// The location being lowered.
    pub fn here(&self) -> Span {
        self.at.get()
    }

    /// Lower at `span`: `f`'s diagnostics without a location of their
    /// own go there.
    fn at_span<R>(&mut self, span: Span, f: impl FnOnce(&mut Self) -> R) -> R {
        let outer = self.at.replace(span);
        let r = f(self);
        self.at.set(outer);
        r
    }

    /// The module whose scope and access the name being resolved has: its
    /// definition's for a name from a meta template, else the current one.
    fn access_module(&self) -> AccessModule<'_> {
        match self.access.borrow().as_ref() {
            Some(m) => AccessModule::Def(std::rc::Rc::clone(m)),
            None => AccessModule::Here(&self.current_module),
        }
    }

    /// Block `use`s in reach: none for a name from a definition.
    fn reach_uses(&self) -> &[Vec<crate::uses::UseEntry>] {
        if self.access.borrow().is_some() {
            &[]
        } else {
            &self.local_uses
        }
    }

    /// Resolve names from `origin` (a path's definition module; `None`:
    /// the current one) until the returned value is put back.
    fn set_access(&self, origin: Option<&str>) -> Option<std::rc::Rc<str>> {
        self.access.replace(origin.map(std::rc::Rc::from))
    }

    /// The source text under `span`, for help that quotes the user's code.
    pub fn src(&self, span: Span) -> Option<&str> {
        let (_, text) = self.env.sources.file(span.file)?;
        text.get(span.start as usize..span.end as usize)
    }

    /// Bytes `from..to` of `outer`'s text as a span of their own.
    fn sub_span(&self, outer: Span, from: usize, to: usize) -> Option<Span> {
        let text = self.src(outer)?;
        let before = text.get(..from)?;
        text.get(from..to)?;
        let (line, col) = match before.rfind('\n') {
            Some(nl) => (
                outer.line + before.matches('\n').count() as u32,
                before[nl + 1..].chars().count() as u32 + 1,
            ),
            None => (outer.line, outer.col + before.chars().count() as u32),
        };
        Some(Span {
            start: outer.start + from as u32,
            end: outer.start + to as u32,
            line,
            col,
            file: outer.file,
        })
    }
}

/// Placeholder for a failed expression (already reported). Never reaches
/// MIR: the driver stops on errors and `check_unresolved_types` refuses
/// `Unknown`.
const fn poison(span: nabic_diagnostic::Span) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Const(ConstVal::Unit),
        ty: HirTy::Unknown,
        span,
    }
}

/// `cell[0] = value` on a one-element `GcBuf` cell.
fn buf_store(cell: HirExpr, value: HirExpr) -> HirExpr {
    let span = value.span;
    let elem = match &cell.ty {
        HirTy::GcBuf(t) => (**t).clone(),
        _ => value.ty.clone(),
    };
    HirExpr {
        kind: HirExprKind::BufSet {
            buf: Box::new(cell),
            index: Box::new(hir_int(0, PrimTy::Usize, span)),
            value: Box::new(value),
            elem,
        },
        ty: HirTy::Unit,
        span,
    }
}

/// Bindings captured as `[ref x]` by closures in `body` (nested ones too);
/// each names the innermost `x` in scope.
fn ref_captured_bindings(
    params: &[Param],
    body: &Block,
) -> nabic_core::HashSet<(String, nabic_diagnostic::Span)> {
    type Binding = (String, nabic_diagnostic::Span);
    struct V {
        scopes: Vec<Vec<Binding>>,
        out: nabic_core::HashSet<Binding>,
    }
    impl crate::walk::AstVisitor for V {
        fn expr(&mut self, e: &Expr) {
            let ExprKind::Closure { captures, .. } = &e.kind else {
                return;
            };
            for c in captures {
                let nabic_ast::CaptureSpec::Ref(n) = c else {
                    continue;
                };
                let found = self
                    .scopes
                    .iter()
                    .rev()
                    .flat_map(|s| s.iter().rev())
                    .find(|(b, _)| b == n);
                if let Some(b) = found {
                    self.out.insert(b.clone());
                }
            }
        }
        fn bind_at(&mut self, name: &str, at: nabic_diagnostic::Span) {
            if let Some(s) = self.scopes.last_mut() {
                s.push((name.to_owned(), at));
            }
        }
        fn push_scope(&mut self) {
            self.scopes.push(Vec::new());
        }
        fn pop_scope(&mut self) {
            self.scopes.pop();
        }
    }
    let mut v = V {
        scopes: vec![params.iter().map(|p| (p.name.clone(), p.span)).collect()],
        out: nabic_core::HashSet::default(),
    };
    crate::walk::walk_block(&mut v, body);
    v.out
}

/// A marked item's name as the user knows it: a bound's method
/// (`#Tr::m`) as `Tr::m`, a generic impl's (`::impl1c8e475a_m`) as `m`.
fn shown_mark_name(name: &str) -> String {
    let (ty, m) = name.rsplit_once("::").unwrap_or(("", name));
    let m = match m.strip_prefix("impl").and_then(|r| r.split_once('_')) {
        Some((hex, rest)) if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) => rest,
        _ => m,
    };
    let ty = ty.trim_start_matches('#');
    if ty.is_empty() {
        m.to_owned()
    } else {
        format!("{ty}::{m}")
    }
}

/// A const argument's arithmetic has no comparisons and no
/// conditionals.
fn const_arg_shape(e: &Expr) -> Result<(), String> {
    match &e.kind {
        ExprKind::Block(b) if b.stmts.is_empty() => {
            b.trailing.as_deref().map_or(Ok(()), const_arg_shape)
        }
        ExprKind::OpInfix { op, lhs, rhs } => {
            if !matches!(op.as_str(), "+" | "-" | "*" | "/" | "%") {
                return Err(format!(
                    "`{op}` is not allowed: only `+`, `-`, `*`, `/`, `%` and unary `-`"
                ));
            }
            const_arg_shape(lhs)?;
            const_arg_shape(rhs)
        }
        ExprKind::OpPrefix { op, operand } => {
            if op != "-" {
                return Err(format!("`{op}` is not allowed: only unary `-`"));
            }
            const_arg_shape(operand)
        }
        ExprKind::If { .. } | ExprKind::Match { .. } => {
            Err("conditionals are not allowed; compute the value in a `const fn`".to_owned())
        }
        _ => Ok(()),
    }
}
