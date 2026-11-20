//! The logged reads of a lowering and their encoding in a unit.

#[allow(clippy::wildcard_imports)]
use crate::lower::incr::*;

/// The counters of the shared state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) next_id: u32,
    pub(crate) structs: u32,
    pub(crate) enums: u32,
    pub(crate) any_tags: u32,
    pub(crate) any_dispatch: u32,
    pub(crate) extras: u32,
    pub(crate) infer_vars: u32,
}

/// A read of the shared state, with its answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Lookup {
    Mono(String, Vec<HirTy>, Option<FunctionId>),
    /// A type by its path (not an instance's key).
    Struct(String, Option<StructId>),
    Enum(String, Option<EnumId>),
    /// The instance of a generic type over these arguments.
    StructInst(String, Vec<HirTy>, Option<StructId>),
    EnumInst(String, Vec<HirTy>, Option<EnumId>),
    /// Every instance of a generic type.
    StructInsts(String, Vec<StructId>),
    EnumInsts(String, Vec<EnumId>),
    Thunk(Key, Option<FunctionId>),
    AnyTag(HirTy, Option<u32>),
    /// `(recv, trait, method, parameter and return types, answer)`.
    AnyDispatch(HirTy, String, String, Vec<HirTy>, Option<FunctionId>),
    /// `inferred_ret`'s entry (`Some(None)`: being inferred).
    #[allow(clippy::option_option)]
    InferredRet(FunctionId, Option<Option<HirTy>>),
}

/// A read of the program's interface (the tables collected from its
/// items), answered by `Incr::answer`'s fingerprint.
#[derive(Clone, Debug)]
pub(crate) enum Read {
    /// `fn_table`: a declaration's signature, or none.
    Sig(String),
    /// `generic_fns`: a generic function's template.
    Generic(String),
    /// Function and generic function keys by last segment.
    FnLeaf(String),
    ExternFn(String),
    /// The concrete methods of a name on a type.
    Methods(HirTy, String),
    /// Every concrete method.
    AllMethods,
    /// Every generic method template.
    AllGenericMethods,
    MethodInfo(FunctionId),
    GenericMethods(String, String),
    KeyOf(FunctionId),
    InferRet(FunctionId),
    MayDiverge(Span),
    Intrinsic(FunctionId),
    GenericIntrinsic(String),
    /// Membership in one of the sets of functions (`FnSet`).
    FnIn(u8, FunctionId),
    ItemVis(String),
    FieldVis(String),
    Const(String),
    ConstLeaf(String),
    Static(String),
    StaticLeaf(String),
    MutStatic(String),
    ExternStatic(String),
    TraitInfo(String),
    NegAuto(String, String),
    NonExhaustive(String),
    Sealed(String),
    /// A function's `#[deprecated]` / `#[deprecated_safe]`.
    FnMark(FunctionId),
    /// A field's, const's or static's.
    Mark(String),
    BoundMethod(String, String),
    BoundTrait(String),
    /// A declaration (its signature, body and impl).
    Decl(FunctionId),
    /// The generic body at a span failed its check (`bounds::in_failed`).
    CheckFailed(Span),
    /// A module's `use` entries.
    Uses(String),
    /// Every generic impl.
    GenericImpls,
    /// The generic impls of a trait.
    GenericImplsOf(String),
    /// The generic impls with a method.
    GenericImplsWith(String),
    /// The impls with an associated const.
    ImplConsts(String),
    /// The trait refs of the concrete impls on a type.
    ImplsOn(HirTy),
    /// Those of one trait.
    ImplsOnOf(HirTy, String),
    /// The targets of a trait's concrete impls.
    ImplsOf(String),
    /// The type key a path names from a module (`TypeNames::canon`).
    Canon(Vec<String>, String),
    /// A type-namespace key exists (an item or a re-export).
    Type(String),
    TypeLeaf(String),
    TypeVis(String),
    ModVis(String),
    Visible(String),
    Reexport(String),
    Trait(String),
    Traits,
    Variant(String),
    VariantLeaf(String),
    NameConst(String),
    NameConstLeaf(String),
    ValueReexport(String),
    GenericAlias(String),
    AnyGenericAlias,
    PlainAlias(String),
    DefaultedLeaf(String),
    StructDef(StructId),
    EnumDef(EnumId),
    GenericStruct(String),
    GenericEnum(String),
    GenericUnion(String),
}

/// An `InstanceWarns` change.
#[derive(Clone, Debug)]
pub(crate) enum WarnEvent {
    Instance(Span, FunctionId),
    Warn(Span, NabiDiagnostic, FunctionId),
}

/// A key of `mono_cache`, `thunks` or `unknown_spans`.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Mono(String, Vec<HirTy>),
    /// `throws::thunk_key`'s parts.
    Thunk(&'static str, Option<FunctionId>, HirTy, HirTy),
    Unknown(String),
}

impl Key {
    /// The table's key: the one `mono::instantiate` files the instance
    /// under, fn bounds included.
    pub(crate) fn mono(name: &str, args: &[HirTy]) -> (String, Vec<String>) {
        crate::lower::mono::cache_key(name, args)
    }
}

pub(crate) struct Head {
    pub(crate) start: Counts,
    pub(crate) end: Counts,
    pub(crate) lookups: Vec<Lookup>,
    /// Its reads of the interface, each with its answer's fingerprint.
    pub(crate) reads: Vec<(Read, u64)>,
    /// Files its spans point into outside its body: path, the end of
    /// the line its furthest span there ends on, and the hash of the
    /// text up to it (the spans' positions, and what they show).
    pub(crate) deps: Vec<(String, u32, u64)>,
    /// The generic type each new struct, then enum, instantiates.
    pub(crate) bases: Vec<String>,
    /// The functions and types made before it that it names: kind, id,
    /// build-independent name (a function's name, a type's
    /// `crate::name_type`).
    pub(crate) named: Vec<(u8, u32, String)>,
}

pub(crate) struct Body {
    pub(crate) func: HirFunction,
    pub(crate) extras: Vec<HirFunction>,
    pub(crate) structs: Vec<HirStructDef>,
    pub(crate) enums: Vec<HirEnumDef>,
    /// The final entries of each function it touched.
    pub(crate) entries: Vec<Entries>,
    /// The final `mono_cache` / `thunks` entry at each key it set.
    pub(crate) keyed: Vec<(Key, Option<FunctionId>)>,
    pub(crate) unknown_spans: Vec<(String, Span)>,
    pub(crate) any_tags: Vec<(HirTy, u32)>,
    pub(crate) any_dispatch: Vec<AnyDispatch>,
    /// `let` inference variables (their numbers name type instances).
    pub(crate) infer_vars: Vec<crate::lower::infer::Var>,
    pub(crate) warns: Vec<WarnEvent>,
    pub(crate) diagnostics: Vec<NabiDiagnostic>,
    pub(crate) name_diags: Vec<NabiDiagnostic>,
    pub(crate) alias_uses: Vec<AliasUse>,
}

/// The entries of one function in the tables by function (`None`: none).
#[derive(Clone)]
pub(crate) struct Entries {
    pub(crate) id: FunctionId,
    pub(crate) ref_out: Option<RefOut>,
    pub(crate) mono_sig: Option<FnSig>,
    pub(crate) fn_origin: Option<(bool, bool)>,
    pub(crate) track_caller: bool,
    #[allow(clippy::option_option)] // an entry of a table of options
    pub(crate) inferred_ret: Option<Option<HirTy>>,
    /// How far past the unit's first function id its lowering started.
    pub(crate) inferred_fn: Option<(u32, HirFunction)>,
}

codec_struct!(Counts {
    next_id,
    structs,
    enums,
    any_tags,
    any_dispatch,
    extras,
    infer_vars
});

codec_enum!(Lookup {
    Mono(x0, x1, x2),
    Struct(x0, x1),
    Enum(x0, x1),
    StructInst(x0, x1, x2),
    EnumInst(x0, x1, x2),
    StructInsts(x0, x1),
    EnumInsts(x0, x1),
    Thunk(x0, x1),
    AnyTag(x0, x1),
    AnyDispatch(x0, x1, x2, x3, x4),
    InferredRet(x0, x1),
});

codec_enum!(Read {
    Sig(x0),
    Generic(x0),
    FnLeaf(x0),
    ExternFn(x0),
    Methods(x0, x1),
    AllMethods,
    AllGenericMethods,
    MethodInfo(x0),
    GenericMethods(x0, x1),
    KeyOf(x0),
    InferRet(x0),
    MayDiverge(x0),
    Intrinsic(x0),
    GenericIntrinsic(x0),
    FnIn(x0, x1),
    ItemVis(x0),
    FieldVis(x0),
    Const(x0),
    ConstLeaf(x0),
    Static(x0),
    StaticLeaf(x0),
    MutStatic(x0),
    ExternStatic(x0),
    TraitInfo(x0),
    NegAuto(x0, x1),
    NonExhaustive(x0),
    Sealed(x0),
    FnMark(x0),
    Mark(x0),
    BoundMethod(x0, x1),
    BoundTrait(x0),
    Decl(x0),
    CheckFailed(x0),
    Uses(x0),
    GenericImpls,
    GenericImplsOf(x0),
    GenericImplsWith(x0),
    ImplConsts(x0),
    ImplsOn(x0),
    ImplsOnOf(x0, x1),
    ImplsOf(x0),
    Canon(x0, x1),
    Type(x0),
    TypeLeaf(x0),
    TypeVis(x0),
    ModVis(x0),
    Visible(x0),
    Reexport(x0),
    Trait(x0),
    Traits,
    Variant(x0),
    VariantLeaf(x0),
    NameConst(x0),
    NameConstLeaf(x0),
    ValueReexport(x0),
    GenericAlias(x0),
    AnyGenericAlias,
    PlainAlias(x0),
    DefaultedLeaf(x0),
    StructDef(x0),
    EnumDef(x0),
    GenericStruct(x0),
    GenericEnum(x0),
    GenericUnion(x0),
});

codec_enum!(WarnEvent {
    Instance(x0, x1),
    Warn(x0, x1, x2)
});

codec_enum!(Key {
    Mono(x0, x1),
    Thunk(x0, x1, x2, x3),
    Unknown(x0)
});

codec_struct!(FnSig {
    id,
    ret_ty,
    param_defaults,
    param_tys,
    param_names,
    param_refs,
    variadic
});

codec_struct!(RefOut {
    ret,
    refs,
    deferred
});

codec_struct!(AnyDispatch {
    id,
    recv_ty,
    trait_key,
    method,
    params,
    ret,
    span
});

codec_struct!(Entries {
    id,
    ref_out,
    mono_sig,
    fn_origin,
    track_caller,
    inferred_ret,
    inferred_fn
});

codec_struct!(Head {
    start,
    end,
    lookups,
    reads,
    deps,
    bases,
    named
});

codec_struct!(Body {
    func,
    extras,
    structs,
    enums,
    entries,
    keyed,
    unknown_spans,
    any_tags,
    any_dispatch,
    infer_vars,
    warns,
    diagnostics,
    name_diags,
    alias_uses,
});

/// A lowering being recorded.
pub(crate) struct Rec {
    pub(crate) lookups: Vec<Lookup>,
    /// Keys looked up so far (the first answer is the one that counts).
    pub(crate) seen: HashSet<String>,
    pub(crate) reads: Vec<Read>,
    /// The reads so far, encoded.
    pub(crate) read_seen: HashSet<Vec<u8>>,
    pub(crate) warns: Vec<WarnEvent>,
    /// Functions whose entries in the tables by function this lowering
    /// may have set or removed, and keys of the tables by key.
    pub(crate) touched: Vec<FunctionId>,
    pub(crate) keys: Vec<Key>,
}

/// Log a read of the shared state (while a unit is recorded): `key`
/// names what was read, `answer` makes the log entry.
pub fn log(key: impl FnOnce() -> String, answer: impl FnOnce() -> Lookup) {
    REC.with(|r| {
        if let Some(rec) = r.borrow_mut().as_mut() {
            if rec.seen.insert(key()) {
                rec.lookups.push(answer());
            }
        }
    });
}

/// Log a read of the interface (while a unit is recorded).
pub(crate) fn read(r: impl FnOnce() -> Read) {
    REC.with(|rec| {
        if let Some(rec) = rec.borrow_mut().as_mut() {
            let r = r();
            let mut e = Enc::new(true);
            r.enc(&mut e);
            if rec.read_seen.insert(e.buf) {
                rec.reads.push(r);
            }
        }
    });
}

/// `log` of a struct (`enum_`: enum) id lookup by path.
pub(crate) fn log_type(enum_: bool, key: &str, found: Option<u32>) {
    REC.with(|r| {
        if let Some(rec) = r.borrow_mut().as_mut() {
            if !rec.seen.insert(format!("t{enum_}|{key}")) {
                return;
            }
            rec.lookups.push(if enum_ {
                Lookup::Enum(key.to_owned(), found.map(EnumId))
            } else {
                Lookup::Struct(key.to_owned(), found.map(StructId))
            });
        }
    });
}

/// Record that the entries of function `id` may change.
pub fn touch(id: FunctionId) {
    REC.with(|r| {
        if let Some(rec) = r.borrow_mut().as_mut() {
            rec.touched.push(id);
        }
    });
}

/// Record that the entry at a key may change.
pub fn touch_key(k: impl FnOnce() -> Key) {
    REC.with(|r| {
        if let Some(rec) = r.borrow_mut().as_mut() {
            rec.keys.push(k());
        }
    });
}

/// Log an `InstanceWarns` change.
pub(crate) fn log_warn(e: impl FnOnce() -> WarnEvent) {
    REC.with(|r| {
        if let Some(rec) = r.borrow_mut().as_mut() {
            rec.warns.push(e());
        }
    });
}

codec_struct!(MethodInfo {
    module,
    vis,
    trait_name,
    trait_args
});

codec_struct!(GenericMethod {
    def,
    self_ty,
    target,
    info
});

codec_struct!(TraitInfo {
    methods,
    supers,
    needs,
    decls,
    module,
    params,
    generics,
    super_tys,
    assoc,
    consts
});
