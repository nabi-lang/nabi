//! Lowering environment: interface tables and function sets.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

/// Read-only lowering environment shared by every body (plus the
/// interior-mutable `sink`). Inputs live for `'s`, item tables for `'a`.
pub struct EnvIn<'a, 's> {
    /// The compile's source files (`#[track_caller]` / `throw` sites).
    pub(crate) sources: &'s nabic_diagnostic::SourceMap,
    pub(crate) prec: &'s PrecTable,
    pub(crate) t: Tables<'a>,
}

pub type Env<'a> = EnvIn<'a, 'a>;

impl<'a> std::ops::Deref for EnvIn<'a, '_> {
    type Target = Tables<'a>;
    fn deref(&self) -> &Tables<'a> {
        &self.t
    }
}

impl std::ops::DerefMut for EnvIn<'_, '_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.t
    }
}

/// What collecting the items makes (`Env`'s tables).
#[derive(Clone)]
pub struct Tables<'a> {
    /// Shared: the tables of a kept library stage are cloned per compile.
    pub(crate) fn_table: HashMap<String, std::sync::Arc<FnSig>>,
    /// `fn_table` and `generic_fns` keys by last segment (both are
    /// complete before any body is lowered).
    pub(crate) fn_leaves: std::cell::OnceCell<Leaves>,
    /// `extern` fns: one global C namespace callable by bare
    /// name; name -> declaring module (where its signature resolves).
    pub(crate) extern_fns: HashMap<String, String>,
    /// Concrete methods by `(type, name)`: several when traits share a
    /// method name (`pick_method` chooses).
    pub(crate) method_table: HashMap<(MethodTarget, String), Vec<FunctionId>>,
    /// `fn_table` key of every function id (method signatures).
    pub(crate) key_of: HashMap<FunctionId, String>,
    /// Declarations whose return type is inferred from the body.
    pub(crate) infer_ret: nabic_core::HashSet<FunctionId>,
    /// `RootMain::Implicit`.
    pub(crate) implicit_main: bool,
    /// The implicit `main`'s top-level `let`s: name -> the `const` that
    /// would make it visible to a `fn` (a hint for a `fn` that names one).
    pub(crate) main_lets: HashMap<String, String>,
    /// `RootMain::Harness`.
    pub(crate) harness_main: bool,
    /// Bodies without `-> T` that may diverge on every path
    /// (`collect::may_diverge`), by span.
    pub(crate) may_diverge: nabic_core::HashSet<nabic_diagnostic::Span>,
    /// Definitions a later item of the same name repeats (reported).
    pub(crate) duplicated: std::cell::RefCell<nabic_core::HashSet<nabic_diagnostic::Span>>,
    pub(crate) method_info: HashMap<FunctionId, MethodInfo>,
    /// Every struct / union / enum, declared or instantiated.
    pub defs: TypeDefs<'a>,
    /// Module-level `const NAME: T = expr;` items. A bare `Path(NAME)`
    /// resolves to the const's value expression inlined at the use site.
    pub(crate) consts: HashMap<String, (Ty, Expr)>,
    pub(crate) const_leaves: std::cell::RefCell<(usize, Leaves)>,
    /// Generic function templates keyed by name. `lower_call` queries
    /// this when the callee path matches a generic function.
    pub(crate) generic_fns: HashMap<String, &'a FnDef>,
    /// Generic-impl and generic methods by (impl target's base, name),
    /// called via `monomorphize` when `method_table` has none.
    pub(crate) generic_methods: HashMap<(String, String), Vec<GenericMethod>>,
    /// `#[intrinsic("nabi.OP.TYPE")]` methods (`Prim` / `Convert`): calls
    /// lower to the op.
    pub(crate) intrinsics: HashMap<FunctionId, collect::Intrinsic>,
    /// Generic `#[intrinsic]` functions by name.
    pub(crate) generic_intrinsics: HashMap<String, collect::Intrinsic>,
    /// `extern "C"` functions: a `fn` argument must be a named fn (a C
    /// function pointer), not a closure or `fn` variable.
    pub(crate) extern_fn_ids: nabic_core::HashSet<FunctionId>,
    /// `extern "nabi"` declarations: the runtime's own functions.
    pub(crate) nabi_externs: nabic_core::HashSet<FunctionId>,
    /// C variadic `extern` declarations (`fn printf(fmt: str, ...)`):
    /// extra arguments take the C default argument promotions.
    pub(crate) c_variadic: nabic_core::HashSet<FunctionId>,
    /// `extern "C" fn` definitions: with `extern_fn_ids`,
    /// the named functions an `extern "C" fn(..)` value can point to.
    pub(crate) c_defs: nabic_core::HashSet<FunctionId>,
    /// `extern "system"` declarations and definitions where `"system"`
    /// is `stdcall` (else empty): `extern "system" fn(..)` values.
    pub(crate) stdcall_fns: nabic_core::HashSet<FunctionId>,
    pub sink: ClosureSink,
    /// Visibility of every value-namespace item, keyed by full name.
    pub(crate) item_visibility: HashMap<String, Visibility>,
    /// Struct name -> field name -> visibility.
    pub(crate) field_vis: HashMap<String, HashMap<String, Visibility>>,
    /// Every impl (bounds, dispatch, projections, associated consts).
    pub(crate) impls: Impls,
    /// `unsafe fn`s and C functions: calls need an unsafe scope.
    pub(crate) unsafe_fns: nabic_core::HashSet<FunctionId>,
    /// `impl !Send for S {}` opt-outs: `(generic base of S, trait key)`.
    pub(crate) neg_auto: nabic_core::HashSet<(String, String)>,
    /// `auto_trait` answers by type and trait, while the program no
    /// longer changes (`lower::send`); `None` otherwise.
    pub(crate) auto_memo: std::cell::RefCell<Option<HashMap<String, HashMap<HirTy, bool>>>>,
    /// `affine::affine` answers by type.
    pub(crate) affine_memo: std::cell::RefCell<HashMap<HirTy, Option<affine::Kind>>>,
    /// Trait key -> declared methods + supertraits (`any Tr` calls).
    pub(crate) trait_info: HashMap<String, TraitInfo>,
    /// Module-level `static` items by name -> declared type.
    pub(crate) statics: HashMap<String, HirTy>,
    pub(crate) static_leaves: std::cell::RefCell<(usize, Leaves)>,
    /// Statics that may be assigned (`extern` `static mut`).
    pub(crate) mut_statics: nabic_core::HashSet<String>,
    /// Statics of `extern` blocks, C globals.
    pub(crate) extern_statics: nabic_core::HashSet<String>,
    /// `#[literal]` handlers of the built-in literal shapes.
    pub(crate) literal_shapes: Vec<ShapeHandler>,
    pub names: TypeNames,
    /// Every non-generic function by `FunctionId` (the ids below
    /// `sink.next_id`'s start).
    pub(crate) decls: Vec<MethodOrFn<'a>>,
    /// `#[non_exhaustive]` structs and enums, by name.
    pub(crate) non_exhaustive: nabic_core::HashSet<String>,
    /// `#[sealed]` traits, by name.
    pub(crate) sealed: nabic_core::HashSet<String>,
    /// Marked functions and methods: the name a warning shows (`P::new`),
    /// the marks.
    pub(crate) fn_marks: HashMap<FunctionId, (String, crate::marks::Mark)>,
    /// Marked fields (`m::P.x`), consts and statics, by key.
    pub(crate) marks: HashMap<String, crate::marks::Mark>,
    /// The package whose uses of marked items warn: the one built.
    pub(crate) mark_pkg: Option<String>,
    /// `--check-library`: `unused_result` also checks the library's code.
    pub(crate) lint_library: bool,
    /// Per `(trait, method)`: the template a call on a type parameter
    /// bounded by the trait goes through (`bounds::templates`).
    pub(crate) bound_methods: HashMap<(String, String), GenericMethod>,
    /// A `bound_methods` template's name -> its trait.
    pub(crate) bound_traits: HashMap<String, String>,
}

/// One of `Env`'s sets of functions.
#[derive(Clone, Copy)]
pub(crate) enum FnSet {
    ExternC,
    NabiExtern,
    CVariadic,
    CDef,
    Stdcall,
    Unsafe,
}

impl FnSet {
    pub(crate) const fn of(n: u8) -> Option<Self> {
        Some(match n {
            0 => Self::ExternC,
            1 => Self::NabiExtern,
            2 => Self::CVariadic,
            3 => Self::CDef,
            4 => Self::Stdcall,
            5 => Self::Unsafe,
            _ => return None,
        })
    }

    pub(crate) fn contains(self, env: &Env<'_>, id: FunctionId) -> bool {
        match self {
            Self::ExternC => env.extern_fn_ids.contains(&id),
            Self::NabiExtern => env.nabi_externs.contains(&id),
            Self::CVariadic => env.c_variadic.contains(&id),
            Self::CDef => env.c_defs.contains(&id),
            Self::Stdcall => env.stdcall_fns.contains(&id),
            Self::Unsafe => env.unsafe_fns.contains(&id),
        }
    }
}

/// Body lowering reads the interface tables through these: each read
/// is logged for the incremental store (`incr::Read`).
impl<'a> Env<'a> {
    pub(crate) fn sig(&self, key: &str) -> Option<&FnSig> {
        incr::read(|| incr::Read::Sig(key.to_owned()));
        self.fn_table.get(key).map(|s| &**s)
    }

    pub(crate) fn generic_fn(&self, key: &str) -> Option<&'a FnDef> {
        incr::read(|| incr::Read::Generic(key.to_owned()));
        self.generic_fns.get(key).copied()
    }

    /// Function and generic function keys whose last segment is `leaf`.
    pub(crate) fn fn_leaf(&self, leaf: &str) -> Vec<String> {
        incr::read(|| incr::Read::FnLeaf(leaf.to_owned()));
        self.fn_leaves
            .get_or_init(|| leaves(self.fn_table.keys().chain(self.generic_fns.keys())))
            .get(leaf)
            .cloned()
            .unwrap_or_default()
    }

    /// The module declaring `extern` function `name`.
    pub(crate) fn extern_fn(&self, name: &str) -> Option<&String> {
        incr::read(|| incr::Read::ExternFn(name.to_owned()));
        self.extern_fns.get(name)
    }

    /// The concrete methods `method` of `ty`.
    pub(crate) fn methods_on(&self, ty: &HirTy, method: &str) -> Option<&Vec<FunctionId>> {
        let t = method_target_for(ty)?;
        incr::read(|| incr::Read::Methods(ty.clone(), method.to_owned()));
        self.method_table.get(&(t, method.to_owned()))
    }

    /// Every concrete method.
    pub(crate) fn all_methods(&self) -> &HashMap<(MethodTarget, String), Vec<FunctionId>> {
        incr::read(|| incr::Read::AllMethods);
        &self.method_table
    }

    /// Every generic method template.
    pub(crate) fn all_generic_methods(&self) -> &HashMap<(String, String), Vec<GenericMethod>> {
        incr::read(|| incr::Read::AllGenericMethods);
        &self.generic_methods
    }

    pub(crate) fn method_info(&self, id: FunctionId) -> Option<&MethodInfo> {
        incr::read(|| incr::Read::MethodInfo(id));
        self.method_info.get(&id)
    }

    pub(crate) fn generic_methods(&self, base: &str, method: &str) -> Option<&Vec<GenericMethod>> {
        incr::read(|| incr::Read::GenericMethods(base.to_owned(), method.to_owned()));
        self.generic_methods
            .get(&(base.to_owned(), method.to_owned()))
    }

    /// The `fn_table` key of function `id`.
    pub(crate) fn fn_key(&self, id: FunctionId) -> Option<&String> {
        incr::read(|| incr::Read::KeyOf(id));
        self.key_of.get(&id)
    }

    /// Declaration `id`'s return type is inferred from its body.
    pub(crate) fn infers_ret(&self, id: FunctionId) -> bool {
        incr::read(|| incr::Read::InferRet(id));
        self.infer_ret.contains(&id)
    }

    /// The body at `at` may diverge on every path (`collect::may_diverge`).
    pub fn diverges(&self, at: Span) -> bool {
        incr::read(|| incr::Read::MayDiverge(at));
        self.may_diverge.contains(&at)
    }

    /// The definition at `at` is repeated later: its calls report no
    /// argument errors. Not a logged read: no unit is made while any exist.
    pub(crate) fn duplicated(&self, at: Span) -> bool {
        self.duplicated.borrow().contains(&at)
    }

    pub(crate) fn intrinsic(&self, id: FunctionId) -> Option<&collect::Intrinsic> {
        incr::read(|| incr::Read::Intrinsic(id));
        self.intrinsics.get(&id)
    }

    pub(crate) fn generic_intrinsic(&self, name: &str) -> Option<&collect::Intrinsic> {
        incr::read(|| incr::Read::GenericIntrinsic(name.to_owned()));
        self.generic_intrinsics.get(name)
    }

    pub(crate) fn is(&self, set: FnSet, id: FunctionId) -> bool {
        incr::read(|| incr::Read::FnIn(set as u8, id));
        set.contains(self, id)
    }

    /// The visibility of value item `key`.
    pub(crate) fn item_vis(&self, key: &str) -> Option<Visibility> {
        incr::read(|| incr::Read::ItemVis(key.to_owned()));
        self.item_visibility.get(key).copied()
    }

    pub(crate) fn fields_vis(&self, ty: &str) -> Option<&HashMap<String, Visibility>> {
        incr::read(|| incr::Read::FieldVis(ty.to_owned()));
        self.field_vis.get(ty)
    }

    pub(crate) fn konst(&self, key: &str) -> Option<&(Ty, Expr)> {
        incr::read(|| incr::Read::Const(key.to_owned()));
        self.consts.get(key)
    }

    pub(crate) fn static_ty(&self, key: &str) -> Option<&HirTy> {
        incr::read(|| incr::Read::Static(key.to_owned()));
        self.statics.get(key)
    }

    pub(crate) fn is_mut_static(&self, key: &str) -> bool {
        incr::read(|| incr::Read::MutStatic(key.to_owned()));
        self.mut_statics.contains(key)
    }

    pub(crate) fn is_extern_static(&self, key: &str) -> bool {
        incr::read(|| incr::Read::ExternStatic(key.to_owned()));
        self.extern_statics.contains(key)
    }

    pub(crate) fn trait_of(&self, key: &str) -> Option<&TraitInfo> {
        incr::read(|| incr::Read::TraitInfo(key.to_owned()));
        self.trait_info.get(key)
    }

    /// The keys of the statics (else consts) whose last segment is `leaf`.
    pub(crate) fn value_leaf(&self, statics: bool, leaf: &str) -> Vec<String> {
        let (index, len) = if statics {
            (&self.static_leaves, self.statics.len())
        } else {
            (&self.const_leaves, self.consts.len())
        };
        let mut index = index.borrow_mut();
        if index.0 != len {
            let keys = if statics {
                leaves(self.statics.keys())
            } else {
                leaves(self.consts.keys())
            };
            *index = (len, keys);
        }
        index.1.get(leaf).cloned().unwrap_or_default()
    }

    /// `impl !key for base {}`.
    pub(crate) fn opts_out(&self, base: &str, key: &str) -> bool {
        incr::read(|| incr::Read::NegAuto(base.to_owned(), key.to_owned()));
        self.neg_auto.contains(&(base.to_owned(), key.to_owned()))
    }

    pub(crate) fn is_non_exhaustive(&self, name: &str) -> bool {
        incr::read(|| incr::Read::NonExhaustive(name.to_owned()));
        self.non_exhaustive.contains(name)
    }

    pub(crate) fn is_sealed(&self, name: &str) -> bool {
        incr::read(|| incr::Read::Sealed(name.to_owned()));
        self.sealed.contains(name)
    }

    /// Function `id`'s marks, with the name a warning shows.
    pub(crate) fn fn_mark(&self, id: FunctionId) -> Option<&(String, crate::marks::Mark)> {
        incr::read(|| incr::Read::FnMark(id));
        self.fn_marks.get(&id)
    }

    /// The marks of field, const or static `key`.
    pub(crate) fn mark(&self, key: &str) -> Option<&crate::marks::Mark> {
        incr::read(|| incr::Read::Mark(key.to_owned()));
        self.marks.get(key)
    }

    pub(crate) fn bound_method(&self, tr: &str, method: &str) -> Option<&GenericMethod> {
        incr::read(|| incr::Read::BoundMethod(tr.to_owned(), method.to_owned()));
        self.bound_methods.get(&(tr.to_owned(), method.to_owned()))
    }

    /// The trait of `bound_methods` template `name`.
    pub(crate) fn bound_trait(&self, name: &str) -> Option<&String> {
        incr::read(|| incr::Read::BoundTrait(name.to_owned()));
        self.bound_traits.get(name)
    }

    /// Module `module`'s `use` entries.
    pub(crate) fn uses_of(&self, module: &str) -> Option<&Vec<crate::uses::UseEntry>> {
        incr::read(|| incr::Read::Uses(module.to_owned()));
        self.names.uses.get(module)
    }

    pub(crate) fn decl(&self, id: FunctionId) -> Option<&MethodOrFn<'a>> {
        incr::read(|| incr::Read::Decl(id));
        self.decls.get(id.0 as usize)
    }
}

impl Tables<'_> {
    /// No item collected yet.
    pub(crate) fn empty() -> Self {
        Self {
            fn_table: HashMap::default(),
            fn_leaves: std::cell::OnceCell::new(),
            extern_fns: HashMap::default(),
            method_table: HashMap::default(),
            key_of: HashMap::default(),
            infer_ret: nabic_core::HashSet::default(),
            implicit_main: false,
            main_lets: HashMap::default(),
            harness_main: false,
            may_diverge: nabic_core::HashSet::default(),
            duplicated: std::cell::RefCell::default(),
            method_info: HashMap::default(),
            defs: TypeDefs::default(),
            consts: HashMap::default(),
            const_leaves: std::cell::RefCell::default(),
            generic_fns: HashMap::default(),
            generic_methods: HashMap::default(),
            intrinsics: HashMap::default(),
            generic_intrinsics: HashMap::default(),
            extern_fn_ids: nabic_core::HashSet::default(),
            nabi_externs: nabic_core::HashSet::default(),
            c_variadic: nabic_core::HashSet::default(),
            c_defs: nabic_core::HashSet::default(),
            stdcall_fns: nabic_core::HashSet::default(),
            sink: ClosureSink::new(),
            item_visibility: HashMap::default(),
            field_vis: HashMap::default(),
            impls: Impls::default(),
            unsafe_fns: nabic_core::HashSet::default(),
            neg_auto: nabic_core::HashSet::default(),
            auto_memo: std::cell::RefCell::default(),
            affine_memo: std::cell::RefCell::default(),
            trait_info: HashMap::default(),
            statics: HashMap::default(),
            static_leaves: std::cell::RefCell::default(),
            mut_statics: nabic_core::HashSet::default(),
            extern_statics: nabic_core::HashSet::default(),
            literal_shapes: Vec::new(),
            names: TypeNames::empty(),
            decls: Vec::new(),
            non_exhaustive: nabic_core::HashSet::default(),
            sealed: nabic_core::HashSet::default(),
            fn_marks: HashMap::default(),
            marks: HashMap::default(),
            mark_pkg: None,
            lint_library: false,
            bound_methods: HashMap::default(),
            bound_traits: HashMap::default(),
        }
    }
}
