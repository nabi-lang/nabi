//! The component-level view of WIT the componentizer works on: worlds
//! of named interfaces, their types and functions, as both the WIT
//! directory (through [`Resolve`]) and the `component-type:*` custom
//! sections (`decode`) give them. Interfaces of one package and name
//! whose versions are semver-compatible are one group, imported or
//! exported once under the newest version.

use crate::abi::MAX_FLAT_PARAMS;
use crate::component::bin::Res;
use crate::parse::Prim;
use crate::resolve::{self, Owner, Resolve, TypeKind, WorldEntry, WorldId};
use resolve::Type as T;
use std::collections::{BTreeMap, HashMap};

pub type Tid = usize;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Val {
    Prim(Prim),
    ErrorContext,
    Id(Tid),
}

#[derive(Clone, Debug)]
pub enum Def {
    /// `type n = v`, a `use`, or an export of another type.
    Alias(Val),
    Record(Vec<(String, Val)>),
    Variant(Vec<(String, Option<Val>)>),
    Enum(Vec<String>),
    Flags(Vec<String>),
    List(Val),
    FixedList(Val, u32),
    Tuple(Vec<Val>),
    Option(Val),
    Result(Option<Val>, Option<Val>),
    Own(Tid),
    Borrow(Tid),
    Future(Option<Val>),
    Stream(Option<Val>),
    Resource,
}

#[derive(Clone, Debug)]
pub struct TyDef {
    pub def: Def,
    /// The interface (index into `Model::ifaces`) declaring it and its
    /// name there; `None` for an anonymous type.
    pub name: Option<(usize, String)>,
}

#[derive(Clone, Debug)]
pub struct Func {
    /// The canonical ABI name (`[method]r.f`).
    pub name: String,
    pub is_async: bool,
    pub params: Vec<(String, Val)>,
    pub result: Option<Val>,
}

#[derive(Clone, Debug, Default)]
pub struct Iface {
    /// `ns:pkg/name@version`, a plain name when inline in a world, or
    /// `$root` for a world's own types and functions.
    pub key: String,
    pub types: Vec<(String, Tid)>,
    pub funcs: Vec<Func>,
}

/// One world: the interfaces it imports and exports (indices into
/// `Model::ifaces`) and the functions it exports itself (those it
/// imports, and its types, are in its `$root` interface).
#[derive(Clone, Debug, Default)]
pub struct World {
    pub imports: Vec<usize>,
    pub exports: Vec<usize>,
    pub root_exports: Vec<Func>,
}

#[derive(Debug, Default)]
pub struct Model {
    pub tys: Vec<TyDef>,
    pub ifaces: Vec<Iface>,
}

pub const ROOT: &str = "$root";

impl Model {
    pub fn ty(&mut self, def: Def) -> Tid {
        self.tys.push(TyDef { def, name: None });
        self.tys.len() - 1
    }

    pub fn iface(&mut self, key: &str) -> usize {
        self.ifaces.push(Iface {
            key: key.to_owned(),
            ..Iface::default()
        });
        self.ifaces.len() - 1
    }

    /// `v` through aliases.
    pub fn dealias<'a>(&'a self, mut v: &'a Val) -> &'a Val {
        while let Val::Id(t) = v {
            match &self.tys[*t].def {
                Def::Alias(a) => v = a,
                _ => break,
            }
        }
        v
    }

    fn def(&self, v: &Val) -> Option<&Def> {
        match self.dealias(v) {
            Val::Id(t) => Some(&self.tys[*t].def),
            _ => None,
        }
    }

    /// Number of core values `v` flattens to (enough to compare with the
    /// limits).
    pub fn flat(&self, v: &Val) -> usize {
        let cap = |n: usize| n.min(MAX_FLAT_PARAMS + 1);
        let Some(d) = self.def(v) else {
            return match self.dealias(v) {
                Val::Prim(Prim::String) => 2,
                _ => 1,
            };
        };
        cap(match d {
            Def::Alias(a) => self.flat(a),
            Def::Record(fs) => fs.iter().map(|(_, t)| self.flat(t)).sum(),
            Def::Tuple(ts) => ts.iter().map(|t| self.flat(t)).sum(),
            Def::Variant(cs) => {
                1 + cs
                    .iter()
                    .filter_map(|(_, t)| t.as_ref())
                    .map(|t| self.flat(t))
                    .max()
                    .unwrap_or(0)
            }
            Def::Enum(_)
            | Def::Own(_)
            | Def::Borrow(_)
            | Def::Future(_)
            | Def::Stream(_)
            | Def::Resource => 1,
            Def::Flags(f) => f.len().div_ceil(32),
            Def::List(_) => 2,
            Def::FixedList(e, n) => self.flat(e) * (*n as usize).min(MAX_FLAT_PARAMS + 1),
            Def::Option(t) => 1 + self.flat(t),
            Def::Result(a, b) => 1 + a.iter().chain(b).map(|t| self.flat(t)).max().unwrap_or(0),
        })
    }

    pub fn flat_all<'a>(&self, vs: impl IntoIterator<Item = &'a Val>) -> usize {
        vs.into_iter().map(|v| self.flat(v)).sum()
    }

    /// `v` holds a list or string: lifting it reads memory, lowering it
    /// allocates.
    pub fn has_ptr(&self, v: &Val) -> bool {
        self.any(v, &|v| matches!(v, Val::Prim(Prim::String)), true)
    }

    pub fn has_string(&self, v: &Val) -> bool {
        self.any(v, &|v| matches!(v, Val::Prim(Prim::String)), false)
    }

    /// Some value inside `v` is `leaf` (lists count when `lists`).
    fn any(&self, v: &Val, leaf: &dyn Fn(&Val) -> bool, lists: bool) -> bool {
        if leaf(v) {
            return true;
        }
        let Val::Id(t) = v else { return false };
        let one = |t: &Val| self.any(t, leaf, lists);
        let opt = |t: &Option<Val>| t.as_ref().is_some_and(one);
        match &self.tys[*t].def {
            Def::Alias(a) | Def::Option(a) | Def::FixedList(a, _) => one(a),
            Def::List(a) => lists || one(a),
            Def::Record(fs) => fs.iter().any(|(_, t)| one(t)),
            Def::Tuple(ts) => ts.iter().any(one),
            Def::Variant(cs) => cs.iter().any(|(_, t)| opt(t)),
            Def::Result(a, b) => opt(a) || opt(b),
            _ => false,
        }
    }

    /// The futures and streams of `f`, in `wit-parser`'s order (each
    /// after those in its payload): what `[stream-new-N]f` counts.
    pub fn payloads(&self, f: &Func) -> Vec<Tid> {
        let mut out = Vec::new();
        for (_, t) in &f.params {
            self.find_payloads(t, &mut out);
        }
        if let Some(t) = &f.result {
            self.find_payloads(t, &mut out);
        }
        out
    }

    fn find_payloads(&self, v: &Val, out: &mut Vec<Tid>) {
        let Val::Id(t) = v else { return };
        let opt = |t: &Option<Val>, out: &mut Vec<Tid>| {
            if let Some(t) = t {
                self.find_payloads(t, out);
            }
        };
        match &self.tys[*t].def {
            Def::Alias(a) | Def::Option(a) | Def::List(a) | Def::FixedList(a, _) => {
                self.find_payloads(a, out);
            }
            Def::Record(fs) => fs.iter().for_each(|(_, t)| self.find_payloads(t, out)),
            Def::Tuple(ts) => ts.iter().for_each(|t| self.find_payloads(t, out)),
            Def::Variant(cs) => cs.iter().for_each(|(_, t)| opt(t, out)),
            Def::Result(a, b) => {
                opt(a, out);
                opt(b, out);
            }
            Def::Future(p) | Def::Stream(p) => {
                opt(p, out);
                out.push(*t);
            }
            Def::Enum(_) | Def::Flags(_) | Def::Own(_) | Def::Borrow(_) | Def::Resource => {}
        }
    }
}

/// The group key of interface key `k`: `ns:pkg/name@1` for `1.x.y`,
/// `@0.2` for `0.2.x`, the key itself when no version (or `0.0.x`, a
/// prerelease) makes others compatible.
pub fn group_key(k: &str) -> String {
    let Some((name, v)) = k.split_once('@') else {
        return k.to_owned();
    };
    match version(v) {
        Some((0, 0, _)) | None => k.to_owned(),
        Some((0, minor, _)) => format!("{name}@0.{minor}"),
        Some((major, ..)) => format!("{name}@{major}"),
    }
}

/// `major.minor.patch` of a release version.
fn version(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.split('.').map(str::parse::<u64>);
    let r = (it.next()?.ok()?, it.next()?.ok()?, it.next()?.ok()?);
    it.next().is_none().then_some(r)
}

fn version_of(k: &str) -> (u64, u64, u64) {
    k.split_once('@')
        .and_then(|(_, v)| version(v))
        .unwrap_or_default()
}

/// Interfaces by group.
#[derive(Debug, Default)]
pub struct Groups {
    /// Name (the newest key) and members (newest first) of each group.
    pub groups: Vec<(String, Vec<usize>)>,
    pub of: Vec<usize>,
    pub by_key: HashMap<String, usize>,
}

impl Groups {
    pub fn new(m: &Model) -> Self {
        let mut by: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, f) in m.ifaces.iter().enumerate() {
            by.entry(group_key(&f.key)).or_default().push(i);
        }
        let mut g = Self {
            of: vec![0; m.ifaces.len()],
            ..Self::default()
        };
        for (key, mut members) in by {
            members.sort_by_key(|&i| std::cmp::Reverse(version_of(&m.ifaces[i].key)));
            let id = g.groups.len();
            for &i in &members {
                g.of[i] = id;
            }
            g.by_key.insert(key, id);
            g.groups.push((m.ifaces[members[0]].key.clone(), members));
        }
        g
    }

    /// The group of interface key `k`.
    pub fn find(&self, k: &str) -> Option<usize> {
        self.by_key.get(&group_key(k)).copied()
    }

    pub fn name(&self, g: usize) -> &str {
        &self.groups[g].0
    }

    /// Function `name` of group `g`, from its newest member having it.
    pub fn func<'m>(&self, m: &'m Model, g: usize, name: &str) -> Option<&'m Func> {
        self.groups[g]
            .1
            .iter()
            .find_map(|&i| m.ifaces[i].funcs.iter().find(|f| f.name == name))
    }

    /// Type `name` of group `g`, from its newest member having it.
    pub fn ty(&self, m: &Model, g: usize, name: &str) -> Option<Tid> {
        self.groups[g].1.iter().find_map(|&i| {
            m.ifaces[i]
                .types
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, t)| *t)
        })
    }

    /// The group and name of a named type.
    pub fn named(&self, m: &Model, t: Tid) -> Option<(usize, String)> {
        m.tys[t]
            .name
            .as_ref()
            .map(|(i, n)| (self.of[*i], n.clone()))
    }

    /// `t`, or the group's own type of that name: one definition for all
    /// members.
    pub fn canon(&self, m: &Model, t: Tid) -> Tid {
        self.named(m, t)
            .and_then(|(g, n)| self.ty(m, g, &n))
            .unwrap_or(t)
    }
}

/// The world of a WIT directory (`component.wit` and `deps/`), into `m`.
pub fn from_resolve(m: &mut Model, r: &Resolve, w: WorldId) -> Res<World> {
    let mut cx = Conv {
        r,
        ifaces: HashMap::default(),
        types: HashMap::default(),
        roots: HashMap::default(),
    };
    let root = cx.root(m, w);
    let wd = &r.worlds[w];
    let mut world = World::default();
    for (_, t) in &wd.types {
        cx.named(m, *t)?;
    }
    for e in &wd.imports {
        match e {
            WorldEntry::Iface(i) => {
                let id = cx.iface(m, *i)?;
                world.imports.push(id);
            }
            WorldEntry::Func(f) => {
                let f = cx.func(m, f)?;
                m.ifaces[root].funcs.push(f);
            }
        }
    }
    for e in &wd.exports {
        match e {
            WorldEntry::Iface(i) => {
                let id = cx.iface(m, *i)?;
                world.exports.push(id);
            }
            WorldEntry::Func(f) => {
                let f = cx.func(m, f)?;
                world.root_exports.push(f);
            }
        }
    }
    Ok(world)
}

struct Conv<'a> {
    r: &'a Resolve,
    ifaces: HashMap<usize, usize>,
    types: HashMap<usize, Tid>,
    roots: HashMap<WorldId, usize>,
}

impl Conv<'_> {
    fn root(&mut self, m: &mut Model, w: WorldId) -> usize {
        *self.roots.entry(w).or_insert_with(|| m.iface(ROOT))
    }

    fn iface(&mut self, m: &mut Model, i: usize) -> Res<usize> {
        if let Some(&id) = self.ifaces.get(&i) {
            return Ok(id);
        }
        let id = m.iface(&self.r.iface_key(i));
        self.ifaces.insert(i, id);
        for (_, t) in &self.r.interfaces[i].types {
            self.named(m, *t)?;
        }
        for f in &self.r.interfaces[i].funcs {
            let f = self.func(m, f)?;
            m.ifaces[id].funcs.push(f);
        }
        Ok(id)
    }

    fn func(&mut self, m: &mut Model, f: &resolve::Function) -> Res<Func> {
        Ok(Func {
            name: f.wit_name(self.r),
            is_async: f.is_async,
            params: f
                .params
                .iter()
                .map(|(n, t)| Ok((n.clone(), self.val(m, t)?)))
                .collect::<Res<_>>()?,
            result: f.result.as_ref().map(|t| self.val(m, t)).transpose()?,
        })
    }

    fn named(&mut self, m: &mut Model, t: usize) -> Res<Tid> {
        if let Some(&id) = self.types.get(&t) {
            return Ok(id);
        }
        let td = &self.r.types[t];
        let owner = match td.owner {
            Owner::Iface(i) => self.iface(m, i)?,
            Owner::World(w) => self.root(m, w),
        };
        if let Some(&id) = self.types.get(&t) {
            return Ok(id);
        }
        let id = m.ty(Def::Resource);
        m.tys[id].name = Some((owner, td.name.clone()));
        m.ifaces[owner].types.push((td.name.clone(), id));
        self.types.insert(t, id);
        let def = match &td.kind {
            // A `use`: the type itself, not a handle to it.
            TypeKind::Alias(resolve::Type::Id(a)) => Def::Alias(Val::Id(self.named(m, *a)?)),
            TypeKind::Alias(a) => Def::Alias(self.val(m, a)?),
            TypeKind::Record(fs) => Def::Record(
                fs.iter()
                    .map(|(n, t)| Ok((n.clone(), self.val(m, t)?)))
                    .collect::<Res<_>>()?,
            ),
            TypeKind::Variant(cs) => Def::Variant(
                cs.iter()
                    .map(|(n, t)| Ok((n.clone(), t.as_ref().map(|t| self.val(m, t)).transpose()?)))
                    .collect::<Res<_>>()?,
            ),
            TypeKind::Enum(cs) => Def::Enum(cs.clone()),
            TypeKind::Flags(fs) => Def::Flags(fs.clone()),
            TypeKind::Resource => Def::Resource,
            TypeKind::Unresolved => return Err(format!("WIT type `{}` is unresolved", td.name)),
        };
        m.tys[id].def = def;
        Ok(id)
    }

    fn val(&mut self, m: &mut Model, t: &resolve::Type) -> Res<Val> {
        let opt = |s: &mut Self, m: &mut Model, t: &Option<Box<T>>| {
            t.as_deref().map(|t| s.val(m, t)).transpose()
        };
        let def = match t {
            T::Prim(p) => return Ok(Val::Prim(*p)),
            T::ErrorContext => return Ok(Val::ErrorContext),
            // A resource by name is an owned handle.
            T::Id(t) if matches!(self.r.types[self.r.dealias(*t)].kind, TypeKind::Resource) => {
                Def::Own(self.named(m, *t)?)
            }
            T::Id(t) => return Ok(Val::Id(self.named(m, *t)?)),
            T::List(e) => Def::List(self.val(m, e)?),
            T::FixedList(e, n) => Def::FixedList(self.val(m, e)?, *n),
            T::Option(e) => Def::Option(self.val(m, e)?),
            T::Result(a, b) => Def::Result(opt(self, m, a)?, opt(self, m, b)?),
            T::Tuple(ts) => Def::Tuple(ts.iter().map(|t| self.val(m, t)).collect::<Res<_>>()?),
            T::Own(t) => Def::Own(self.named(m, *t)?),
            T::Borrow(t) => Def::Borrow(self.named(m, *t)?),
            T::Future(e) => Def::Future(opt(self, m, e)?),
            T::Stream(e) => Def::Stream(opt(self, m, e)?),
        };
        Ok(Val::Id(m.ty(def)))
    }
}
