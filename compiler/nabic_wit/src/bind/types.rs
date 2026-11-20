//! Type mapping: views, type paths, declarations, resource glue.

#[allow(clippy::wildcard_imports)]
use crate::bind::*;

impl Gen<'_> {
    pub(crate) fn view(&self, t: &Type) -> View {
        match t {
            Type::Prim(p) => View::Prim(*p),
            Type::List(e) => View::List((**e).clone()),
            Type::Option(e) => View::Option((**e).clone()),
            Type::Result(a, b) => View::Result(a.as_deref().cloned(), b.as_deref().cloned()),
            Type::Tuple(ts) => View::Tuple(ts.clone()),
            Type::FixedList(e, n) => View::FixedList((**e).clone(), *n),
            Type::Future(_) => View::Future,
            Type::Stream(_) => View::Stream,
            Type::ErrorContext => View::ErrorContext,
            Type::Own(id) => View::Own(self.r.dealias(*id)),
            Type::Borrow(id) => View::Borrow(self.r.dealias(*id)),
            Type::Id(id) => {
                let d = self.r.dealias(*id);
                match &self.r.types[d].kind {
                    TypeKind::Alias(t) => self.view(t),
                    TypeKind::Record(fs) => View::Record(d, fs.clone()),
                    TypeKind::Variant(cs) => View::Variant(d, cs.clone()),
                    TypeKind::Enum(cs) => View::Enum(d, cs.len()),
                    TypeKind::Flags(fs) => View::Flags(d, fs.clone()),
                    TypeKind::Resource | TypeKind::Unresolved => View::Own(d),
                }
            }
        }
    }

    /// Module path (below `base`) of what `owner` defines.
    pub(crate) fn owner_mod(&self, owner: Owner) -> Vec<String> {
        let (pkg, mut tail) = match owner {
            Owner::Iface(i) => {
                let f = &self.r.interfaces[i];
                let mut tail = Vec::new();
                if let Some(w) = f.inline {
                    tail.push(snake(&self.r.worlds[w].name));
                }
                tail.push(snake(&f.name));
                (f.pkg, tail)
            }
            Owner::World(w) => (self.r.worlds[w].pkg, vec![snake(&self.r.worlds[w].name)]),
        };
        let name = &self.r.packages[pkg].name;
        let mut path: Vec<String> = name.ns.split(':').map(snake).collect();
        path.push(snake(&name.name));
        path.append(&mut tail);
        path
    }

    pub(crate) fn abs(&self, m: &[String]) -> String {
        let mut s = self.base.clone();
        for p in m {
            s.push_str("::");
            s.push_str(p);
        }
        s
    }

    pub(crate) fn type_path(&self, id: TypeId) -> String {
        let t = &self.r.types[id];
        format!(
            "{}::{}",
            self.abs(&self.owner_mod(t.owner)),
            pascal(&t.name)
        )
    }

    pub(crate) fn ours(&self, res: TypeId) -> bool {
        matches!(self.r.types[res].owner, Owner::Iface(i) if self.exported.contains(&i))
    }

    pub(crate) fn rty(&self, t: &Type) -> Result<String, String> {
        Ok(match t {
            Type::Prim(p) => match p {
                Prim::Bool => "bool",
                Prim::U8 => "u8",
                Prim::U16 => "u16",
                Prim::U32 => "u32",
                Prim::U64 => "u64",
                Prim::S8 => "i8",
                Prim::S16 => "i16",
                Prim::S32 => "i32",
                Prim::S64 => "i64",
                Prim::F32 => "f32",
                Prim::F64 => "f64",
                Prim::Char => "char",
                Prim::String => "str",
            }
            .to_owned(),
            Type::List(e) => format!("{LIST}<{}>", self.rty(e)?),
            // `T?` keeps a layer for a `T` that is optional itself.
            Type::Option(e) => format!("{}?", self.rty(e)?),
            Type::FixedList(e, n) => format!("[{}; {n}]", self.rty(e)?),
            Type::Future(e) => format!("{FUTURE}<{}>", self.payload_ty(e.as_deref())?),
            Type::Stream(e) => format!("{RECEIVER}<{}>", self.payload_ty(e.as_deref())?),
            Type::ErrorContext => format!("{W}::ErrorContext"),
            Type::Result(a, b) => format!(
                "{RESULT}<{}, {}>",
                a.as_deref()
                    .map_or_else(|| Ok("()".into()), |t| self.rty(t))?,
                b.as_deref()
                    .map_or_else(|| Ok("()".into()), |t| self.rty(t))?
            ),
            Type::Tuple(ts) => {
                let parts = ts
                    .iter()
                    .map(|t| self.rty(t))
                    .collect::<Result<Vec<_>, _>>()?;
                match parts.len() {
                    0 => "()".into(),
                    1 => format!("({},)", parts[0]),
                    _ => format!("({})", parts.join(", ")),
                }
            }
            Type::Own(id) | Type::Borrow(id) | Type::Id(id) => self.type_path(*id),
        })
    }

    /// A future's or stream's value type (`()` without one).
    pub(crate) fn payload_ty(&self, t: Option<&Type>) -> Result<String, String> {
        t.map_or_else(|| Ok("()".into()), |t| self.rty(t))
    }

    /// `t` holds a resource (no derives then: the user's types need not
    /// have them).
    pub(crate) fn has_resource(&self, t: &Type, depth: usize) -> bool {
        if depth > 64 {
            return false;
        }
        match self.view(t) {
            // No derives either for what holds a future or stream.
            View::Own(_) | View::Borrow(_) | View::Future | View::Stream | View::ErrorContext => {
                true
            }
            View::Prim(_) | View::Enum(..) | View::Flags(..) => false,
            View::List(e) | View::Option(e) | View::FixedList(e, _) => {
                self.has_resource(&e, depth + 1)
            }
            View::Result(a, b) => [a, b]
                .iter()
                .flatten()
                .any(|t| self.has_resource(t, depth + 1)),
            View::Tuple(ts) => ts.iter().any(|t| self.has_resource(t, depth + 1)),
            View::Record(_, fs) => fs.iter().any(|(_, t)| self.has_resource(t, depth + 1)),
            View::Variant(_, cs) => cs
                .iter()
                .filter_map(|(_, t)| t.as_ref())
                .any(|t| self.has_resource(t, depth + 1)),
        }
    }

    /// `t` lowers into memory blocks (strings, lists) that a
    /// post-return function frees.
    pub(crate) fn has_heap(&self, t: &Type) -> bool {
        match self.view(t) {
            View::Prim(Prim::String) | View::List(_) => true,
            View::Prim(_)
            | View::Enum(..)
            | View::Flags(..)
            | View::Own(_)
            | View::Borrow(_)
            | View::Future
            | View::Stream
            | View::ErrorContext => false,
            View::Option(e) | View::FixedList(e, _) => self.has_heap(&e),
            View::Result(a, b) => [a, b].iter().flatten().any(|t| self.has_heap(t)),
            View::Tuple(ts) => ts.iter().any(|t| self.has_heap(t)),
            View::Record(_, fs) => fs.iter().any(|(_, t)| self.has_heap(t)),
            View::Variant(_, cs) => cs
                .iter()
                .filter_map(|(_, t)| t.as_ref())
                .any(|t| self.has_heap(t)),
        }
    }

    pub(crate) fn push(&mut self, m: &[String], text: &str) {
        self.mods.entry(m.to_vec()).or_default().push_str(text);
    }

    /// The `extern "C"` function for core import `name` of `module`.
    pub(crate) fn extern_fn(
        &mut self,
        m: &[String],
        module: &str,
        name: &str,
        params: &[Flat],
        ret: Option<Flat>,
    ) -> String {
        let id = ident("wit", module, name);
        let ps: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(i, f)| format!("a{i}: {}", f.nabi()))
            .collect();
        let ret = ret.map(|f| format!(" -> {}", f.nabi())).unwrap_or_default();
        self.externs
            .entry(m.to_vec())
            .or_default()
            .entry(module.to_owned())
            .or_default()
            .insert(format!(
                "    #[link_name = \"{name}\"]\n    fn {id}({}){ret};\n",
                ps.join(", ")
            ));
        id
    }

    /// The `[resource-new]` / `[resource-rep]` / `[resource-drop]`
    /// imports of resource `res`, and its table (exported ones).
    pub(crate) fn resource_fns(&mut self, res: TypeId) -> (String, String, String, String) {
        let Owner::Iface(i) = self.r.types[res].owner else {
            unreachable!("resources live in interfaces")
        };
        let m = self.owner_mod(Owner::Iface(i));
        let name = &self.r.types[res].name;
        let key = self.r.iface_key(i);
        if self.ours(res) {
            let module = format!("[export]{key}");
            let new = self.extern_fn(
                &m,
                &module,
                &format!("[resource-new]{name}"),
                &[Flat::I32],
                Some(Flat::I32),
            );
            let rep = self.extern_fn(
                &m,
                &module,
                &format!("[resource-rep]{name}"),
                &[Flat::I32],
                Some(Flat::I32),
            );
            let drop = self.extern_fn(
                &m,
                &module,
                &format!("[resource-drop]{name}"),
                &[Flat::I32],
                None,
            );
            let table = format!(
                "{}::WIT_TABLE_{}",
                self.abs(&m),
                snake(name).to_ascii_uppercase()
            );
            (new, rep, drop, table)
        } else {
            let drop = self.extern_fn(
                &m,
                &key,
                &format!("[resource-drop]{name}"),
                &[Flat::I32],
                None,
            );
            (String::new(), String::new(), drop, String::new())
        }
    }

    /// The nabi declaration of type `id`.
    pub(crate) fn type_decl(&mut self, id: TypeId) -> Result<(), String> {
        let t = &self.r.types[id];
        let m = self.owner_mod(t.owner);
        let name = pascal(&t.name);
        let me = Type::Id(id);
        let derive = if self.has_resource(&me, 0) {
            ""
        } else {
            "#[derive(Debug, Clone, PartialEq)]\n"
        };
        let text = match t.kind.clone() {
            TypeKind::Alias(a) => format!("pub type {name} = {};\n", self.rty(&a)?),
            TypeKind::Record(fs) => {
                let mut s = format!("{derive}pub struct {name} {{\n");
                for (f, ft) in &fs {
                    let _ = writeln!(s, "    pub {}: {};", snake(f), self.rty(ft)?);
                }
                s + "}\n"
            }
            TypeKind::Variant(cs) => {
                let mut s = format!("{derive}pub enum {name} {{\n");
                for (c, ct) in &cs {
                    match ct {
                        Some(ct) => {
                            let _ = writeln!(s, "    {}({}),", pascal(c), self.rty(ct)?);
                        }
                        None => {
                            let _ = writeln!(s, "    {},", pascal(c));
                        }
                    }
                }
                s + "}\n"
            }
            TypeKind::Enum(cs) => {
                let mut s = format!("{derive}pub enum {name} {{\n");
                for c in &cs {
                    let _ = writeln!(s, "    {},", pascal(c));
                }
                s + "}\n"
            }
            TypeKind::Flags(fs) => {
                let mut s =
                    format!("#[derive(Debug, Clone, PartialEq, Default)]\npub struct {name} {{\n");
                for f in &fs {
                    let _ = writeln!(s, "    pub {}: bool;", snake(f));
                }
                s + "}\n"
            }
            TypeKind::Resource if self.ours(id) => {
                let (.., table) = self.resource_fns(id);
                let user = format!("{}::{name}", self.base);
                let table = table.rsplit("::").next().unwrap_or_default().to_owned();
                format!(
                    "pub type {name} = {user};\npub(pkg) static {table}: {W}::Table<{user}> = unsafe {{ {W}::Table::new() }};\n"
                )
            }
            TypeKind::Resource => {
                let (_, _, drop, _) = self.resource_fns(id);
                format!(
                    "pub struct {name} {{\n    pub(pkg) handle: {W}::Handle;\n}}\n\
                     impl ::core::marker::Drop for {name} {{\n    fn drop(ref self) {{\n        \
                     match self.handle.release() {{\n            h: i32 => {{ unsafe {{ {drop}(h); }} }}\n            \
                     none => {{}}\n        }}\n    }}\n}}\n"
                )
            }
            TypeKind::Unresolved => return Err(format!("type `{}` is unresolved", t.name)),
        };
        self.push(&m, &text);
        Ok(())
    }
}
