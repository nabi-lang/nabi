//! Impl tables and struct / union / enum definitions by id.

#[allow(clippy::wildcard_imports)]
use crate::lower::*;

/// Per-method facts for call sites: privacy module, visibility, and the
/// implemented trait (operators need a trait impl).
#[derive(Clone)]
pub(crate) struct MethodInfo {
    pub(crate) module: String,
    pub(crate) vis: Visibility,
    pub(crate) trait_name: Option<String>,
    /// The trait's arguments of a concrete impl (`Convert<f64>`: `[f64]`),
    /// defaults filled; empty for a template.
    pub(crate) trait_args: Vec<HirTy>,
}

/// A method template monomorphized per call: `def` has the impl's
/// generics then the method's, named `Base__method` in the impl's module.
#[derive(Clone)]
pub(crate) struct GenericMethod {
    /// Shared: the tables of a kept library stage are cloned per compile.
    pub(crate) def: std::sync::Arc<FnDef>,
    /// The impl target (`Box<T>`): the type of `self` and of `Self`.
    pub(crate) self_ty: Ty,
    /// A concrete impl's resolved target (`Range<f64>`): impls of other
    /// arguments of one base keep their templates apart by it.
    pub(crate) target: Option<HirTy>,
    pub(crate) info: MethodInfo,
}

/// An associated const `(NAME, type, value)`.
pub(crate) type AssocConst = (String, Ty, Expr);

/// One `impl`, inherent or trait, concrete or generic (a bare
/// `T` target is a blanket impl).
#[derive(Clone)]
pub(crate) struct Impl {
    /// Empty for an inherent impl.
    pub(crate) trait_key: String,
    /// `impl<T> Get<T> for Box<T> { type Item = T; }`: `[T]` and
    /// `[(Item, T)]`, resolved per instantiation.
    pub(crate) trait_args: Vec<Ty>,
    pub(crate) assoc: Vec<(String, Ty)>,
    pub(crate) generics: nabic_ast::Generics,
    pub(crate) where_clause: Vec<nabic_ast::WherePred>,
    pub(crate) self_ty: Ty,
    pub(crate) module: String,
    /// A generic trait impl's (or structural inherent impl's) method
    /// templates by name, trait default bodies included.
    pub(crate) methods: HashMap<String, GenericMethod>,
    /// Associated consts, trait defaults included.
    pub(crate) consts: Vec<AssocConst>,
    /// A concrete impl's resolved target, and the trait ref it provides
    /// (arguments with defaults filled, associated types as bindings).
    pub(crate) concrete: Option<(HirTy, Option<TraitRef>)>,
}

impl Impl {
    pub(crate) fn is_generic(&self) -> bool {
        !self.generics.params.is_empty()
    }
}

/// Every impl, in source order, with indexes for the concrete ones.
#[derive(Clone, Default)]
pub(crate) struct Impls {
    pub(crate) all: Vec<std::sync::Arc<Impl>>,
    /// Concrete trait impls by target.
    pub(crate) by_target: HashMap<MethodTarget, Vec<usize>>,
    /// Concrete trait impls by trait key, in resolution order.
    pub(crate) by_trait: HashMap<String, Vec<usize>>,
    /// Generic impls by trait key (`""`: inherent), in source order.
    pub(crate) generic_of: HashMap<String, Vec<usize>>,
    /// Generic impls by method name, in source order.
    pub(crate) generic_with: HashMap<String, Vec<usize>>,
}

impl Impls {
    /// Every impl, in source order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Impl> {
        self.all.iter().map(|i| &**i)
    }

    pub(crate) fn push(&mut self, i: Impl) {
        if i.is_generic() {
            self.generic_of
                .entry(i.trait_key.clone())
                .or_default()
                .push(self.all.len());
            for m in i.methods.keys() {
                self.generic_with
                    .entry(m.clone())
                    .or_default()
                    .push(self.all.len());
            }
        }
        self.all.push(std::sync::Arc::new(i));
    }

    /// Concrete impl `i`'s target and trait ref, once resolved.
    pub(crate) fn resolve(&mut self, i: usize, ty: HirTy, provides: Option<TraitRef>) {
        if let Some(r) = &provides {
            if let Some(t) = method_target_for(&ty) {
                self.by_target.entry(t).or_default().push(i);
            }
            self.by_trait.entry(r.path.clone()).or_default().push(i);
        }
        std::sync::Arc::make_mut(&mut self.all[i]).concrete = Some((ty, provides));
    }

    pub(crate) fn generic(&self) -> impl Iterator<Item = &Impl> {
        incr::read(|| incr::Read::GenericImpls);
        self.iter().filter(|i| i.is_generic())
    }

    /// The generic impls `pick` names.
    pub(crate) fn generic_by<'i: 'p, 'p>(
        &'i self,
        pick: Pick<'p>,
    ) -> Box<dyn Iterator<Item = &'i Impl> + 'p> {
        match pick {
            Pick::Trait(key) => {
                incr::read(|| incr::Read::GenericImplsOf(key.to_owned()));
                Box::new(
                    self.generic_of
                        .get(key)
                        .map_or(&[][..], Vec::as_slice)
                        .iter()
                        .map(|&i| &*self.all[i]),
                )
            }
            Pick::Method(m) => {
                incr::read(|| incr::Read::GenericImplsWith(m.to_owned()));
                Box::new(
                    self.generic_with
                        .get(m)
                        .map_or(&[][..], Vec::as_slice)
                        .iter()
                        .map(|&i| &*self.all[i]),
                )
            }
            Pick::Where(keep) => Box::new(self.generic().filter(move |i| keep(i))),
        }
    }

    /// The impls with associated const `name`.
    pub(crate) fn with_const<'i: 'n, 'n>(
        &'i self,
        name: &'n str,
    ) -> impl Iterator<Item = &'i Impl> + 'n {
        incr::read(|| incr::Read::ImplConsts(name.to_owned()));
        self.iter()
            .filter(move |i| i.consts.iter().any(|(n, ..)| n == name))
    }

    /// The trait refs of the concrete impls of trait `key` on `ty`.
    pub(crate) fn refs_of<'s>(
        &'s self,
        ty: &HirTy,
        key: &'s str,
    ) -> impl Iterator<Item = &'s TraitRef> + 's {
        incr::read(|| incr::Read::ImplsOnOf(ty.clone(), key.to_owned()));
        method_target_for(ty)
            .and_then(|t| self.by_target.get(&t))
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|&i| self.all[i].concrete.as_ref()?.1.as_ref())
            .filter(move |r| r.path == key)
    }

    /// The trait refs of the concrete impls on `ty`.
    pub(crate) fn refs_on<'s>(
        &'s self,
        ty: &HirTy,
    ) -> impl Iterator<Item = &'s TraitRef> + use<'s> {
        incr::read(|| incr::Read::ImplsOn(ty.clone()));
        method_target_for(ty)
            .and_then(|t| self.by_target.get(&t))
            .map_or(&[][..], Vec::as_slice)
            .iter()
            .filter_map(|&i| self.all[i].concrete.as_ref()?.1.as_ref())
    }

    /// Trait `key`'s concrete impls: target and trait ref, in resolution
    /// order.
    pub(crate) fn concrete_of(&self, key: &str) -> impl Iterator<Item = (&HirTy, &TraitRef)> {
        incr::read(|| incr::Read::ImplsOf(key.to_owned()));
        self.by_trait
            .get(key)
            .into_iter()
            .flatten()
            .filter_map(|&i| {
                let (t, r) = self.all[i].concrete.as_ref()?;
                Some((t, r.as_ref()?))
            })
    }

    /// The targets of trait `key`'s concrete impls, in resolution order.
    pub(crate) fn tys_of(&self, key: &str) -> impl Iterator<Item = &HirTy> {
        incr::read(|| incr::Read::ImplsOf(key.to_owned()));
        self.by_trait
            .get(key)
            .into_iter()
            .flatten()
            .filter_map(|&i| Some(&self.all[i].concrete.as_ref()?.0))
    }
}

/// The types `auto_trait` is inside of, outermost first, and the
/// outermost one a `true` below assumed (`usize::MAX`: none).
pub(crate) struct AutoSeen {
    pub(crate) open: Vec<HirTy>,
    pub(crate) low: usize,
}

impl Default for AutoSeen {
    fn default() -> Self {
        Self {
            open: Vec::new(),
            low: usize::MAX,
        }
    }
}

/// Which generic impls a lookup wants: of a trait, with a method, or
/// any `keep` takes.
#[derive(Clone, Copy)]
pub(crate) enum Pick<'p> {
    Trait(&'p str),
    Method(&'p str),
    Where(&'p dyn Fn(&Impl) -> bool),
}

/// Every struct / union / enum by id: declared ones and generic instances,
/// made when first named (`instantiate_struct` / `instantiate_enum`).
#[derive(Clone, Default)]
pub struct TypeDefs<'a> {
    pub(crate) structs: std::cell::RefCell<Vec<HirStructDef>>,
    pub(crate) struct_table: std::cell::RefCell<HashMap<String, StructId>>,
    pub(crate) enums: std::cell::RefCell<Vec<HirEnumDef>>,
    pub(crate) enum_table: std::cell::RefCell<HashMap<String, EnumId>>,
    /// Generic templates by full name.
    pub(crate) generic_structs: HashMap<String, &'a AstStructDef>,
    pub(crate) generic_unions: HashMap<String, &'a nabic_ast::UnionDef>,
    pub(crate) generic_enums: HashMap<String, &'a AstEnumDef>,
    /// Leaf names of templates whose parameters all have defaults: their
    /// bare name is an instance.
    pub(crate) defaulted_leaves: nabic_core::HashSet<String>,
}

impl<'a> TypeDefs<'a> {
    pub(crate) fn generics_of(&self, base: &str) -> Option<&nabic_ast::Generics> {
        self.generic_struct(base)
            .map(|d| &d.generics)
            .or_else(|| self.generic_union(base).map(|d| &d.generics))
            .or_else(|| self.generic_enum(base).map(|d| &d.generics))
    }

    /// `base` is a generic struct or union template (their instances are
    /// structs).
    pub(crate) fn is_generic_struct(&self, base: &str) -> bool {
        self.generic_struct(base).is_some() || self.generic_union(base).is_some()
    }

    pub(crate) fn generic_union(&self, base: &str) -> Option<&'a nabic_ast::UnionDef> {
        incr::read(|| incr::Read::GenericUnion(base.to_owned()));
        self.generic_unions.get(base).copied()
    }

    /// Generic struct template `base`.
    pub fn generic_struct(&self, base: &str) -> Option<&'a AstStructDef> {
        incr::read(|| incr::Read::GenericStruct(base.to_owned()));
        self.generic_structs.get(base).copied()
    }

    pub fn generic_enum(&self, base: &str) -> Option<&'a AstEnumDef> {
        incr::read(|| incr::Read::GenericEnum(base.to_owned()));
        self.generic_enums.get(base).copied()
    }

    /// A template with leaf name `leaf` has every parameter defaulted.
    pub(crate) fn defaulted(&self, leaf: &str) -> bool {
        incr::read(|| incr::Read::DefaultedLeaf(leaf.to_owned()));
        self.defaulted_leaves.contains(leaf)
    }

    pub(crate) fn struct_at(&self, id: StructId) -> HirStructDef {
        incr::read(|| incr::Read::StructDef(id));
        self.structs.borrow()[id.0 as usize].clone()
    }

    pub(crate) fn enum_at(&self, id: EnumId) -> HirEnumDef {
        incr::read(|| incr::Read::EnumDef(id));
        self.enums.borrow()[id.0 as usize].clone()
    }

    /// `base` written without arguments names its all-defaults instance.
    pub(crate) fn bare_instance(&self, base: &str) -> bool {
        self.generics_of(base).is_some_and(mono::all_defaulted)
    }

    /// Drop the structs and enums from `structs` / `enums` on (instances
    /// a rolled-back lowering made).
    pub(crate) fn truncate(&self, structs: usize, enums: usize) {
        self.structs.borrow_mut().truncate(structs);
        self.struct_table
            .borrow_mut()
            .retain(|_, id| (id.0 as usize) < structs);
        self.enums.borrow_mut().truncate(enums);
        self.enum_table
            .borrow_mut()
            .retain(|_, id| (id.0 as usize) < enums);
        crate::truncate_type_names(structs, enums);
    }

    pub(crate) fn struct_id(&self, key: &str) -> Option<StructId> {
        let id = self.struct_table.borrow().get(key).copied();
        incr::log_type(false, key, id.map(|i| i.0));
        id
    }

    pub(crate) fn enum_id(&self, key: &str) -> Option<EnumId> {
        let id = self.enum_table.borrow().get(key).copied();
        incr::log_type(true, key, id.map(|i| i.0));
        id
    }

    /// Instance `base<args>` of a generic struct or union, existing or new
    /// (at `span`). The id is taken before field types resolve, so a field
    /// naming the instance finds it.
    pub(crate) fn instantiate_struct(
        &self,
        base: &str,
        args: Vec<HirTy>,
        span: nabic_diagnostic::Span,
        res: impl Fn(&TySubst, &Ty) -> HirTy,
    ) -> Option<StructId> {
        let key = mangle_inst(base, &args);
        let found = self.struct_table.borrow().get(&key).copied();
        incr::log(
            || format!("ts|{key}"),
            || incr::Lookup::StructInst(base.to_owned(), args.clone(), found),
        );
        if let Some(id) = found {
            return Some(id);
        }
        let tmpl = match self.generic_struct(base) {
            Some(d) => Ok(d),
            None => Err(self.generic_union(base)?),
        };
        let (generics, attrs) = match tmpl {
            Ok(d) => (&d.generics, &d.attrs),
            Err(u) => (&u.generics, &u.attrs),
        };
        let id = StructId(u32::try_from(self.structs.borrow().len()).expect("StructId overflow"));
        crate::name_type(false, id.0, base, &args);
        self.struct_table.borrow_mut().insert(key.clone(), id);
        self.structs.borrow_mut().push(HirStructDef {
            id,
            repr: collect::repr_of(attrs).unwrap_or_default(),
            def_id: None,
            name: key.clone(),
            fields: Vec::new(),
            span,
            generic_args: args.clone(),
            union_kind: None,
            niche: collect::niche_args(attrs).unwrap_or_default(),
        });
        let subst = generic_subst(generics, &args);
        let res = |t: &Ty| res(&subst, t);
        match tmpl {
            Ok(d) => {
                let fields = generic_struct_fields(d, &res);
                self.structs.borrow_mut()[id.0 as usize].fields = fields;
            }
            Err(u) => {
                // Declaration errors were reported at the template.
                let inst = collect::union_def(u, id, key, &res, args, &mut Vec::new());
                self.structs.borrow_mut()[id.0 as usize] = HirStructDef { span, ..inst };
            }
        }
        Some(id)
    }

    /// `instantiate_struct` for a generic enum.
    pub(crate) fn instantiate_enum(
        &self,
        base: &str,
        args: Vec<HirTy>,
        span: nabic_diagnostic::Span,
        variants: impl FnOnce(&AstEnumDef, &TySubst) -> Vec<HirEnumVariant>,
    ) -> Option<EnumId> {
        let key = mangle_inst(base, &args);
        let found = self.enum_table.borrow().get(&key).copied();
        incr::log(
            || format!("te|{key}"),
            || incr::Lookup::EnumInst(base.to_owned(), args.clone(), found),
        );
        if let Some(id) = found {
            return Some(id);
        }
        let def = self.generic_enum(base)?;
        let id = EnumId(u32::try_from(self.enums.borrow().len()).expect("EnumId overflow"));
        crate::name_type(true, id.0, base, &args);
        self.enum_table.borrow_mut().insert(key.clone(), id);
        self.enums.borrow_mut().push(HirEnumDef {
            id,
            repr: collect::repr_of(&def.attrs).unwrap_or_default(),
            def_id: None,
            name: key,
            variants: Vec::new(),
            span,
            generic_args: args.clone(),
        });
        let variants = variants(def, &generic_subst(&def.generics, &args));
        self.enums.borrow_mut()[id.0 as usize].variants = variants;
        Some(id)
    }
}
