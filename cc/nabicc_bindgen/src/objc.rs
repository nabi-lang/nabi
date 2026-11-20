//! Objective-C bindings (`-x objective-c`): each class, protocol and
//! category of the bound headers as an `extern "objc"` block, which the
//! compiler turns into methods (`bare::ffi::objc`).
//!
//! A class's block holds its methods and its properties' accessors from
//! every bound `@interface` of it (categories and extensions merged);
//! `NSObject`, `NSString` and `NSError` are `bare::ffi::objc`'s, so theirs
//! are categories. Classes and protocols the bound declarations use but
//! do not declare get a block without methods. Types map as Swift reads
//! them: nullability to `Id<T>` or `Id<T> | none`, `instancetype` to
//! `Id<Self>`, blocks to closures, a last `NSError **` to `throws`, and
//! generic arguments are dropped (`ObjectType` is `id`).

use crate::{Gen, Skip, ident};
use nabic_core::{HashMap, HashSet};
use nabicc_parse::objc::{Avail, EnumKind, Interface, Method, Null, ObjKind, Property};
use nabicc_parse::ty::{QualType, Type, TypeId};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::path::Path;

const OBJC: &str = "bare::ffi::objc";

/// Classes `bare::ffi::objc` declares.
const BUILTIN: [&str; 3] = ["NSObject", "NSString", "NSError"];

/// Methods a program never sends itself under ARC's rules.
const NEVER: &[&str] = &[
    "dealloc",
    "retain",
    "release",
    "autorelease",
    "retainCount",
    "finalize",
    "allocWithZone:",
    "zone",
];

/// What an Objective-C type is.
enum Obj {
    Instancetype,
    /// `id`.
    Any,
    Class(String),
    Protocol(String),
    /// `Class`.
    ClassObject,
    Sel,
}

/// The emitter's state.
#[derive(Default)]
pub(crate) struct State {
    /// Classes and protocols queued, by Objective-C name.
    classes: HashSet<String>,
    protocols: HashSet<String>,
    queue: VecDeque<(bool, String)>,
    /// Class names, for protocols named like one (`NSObject`).
    class_names: HashSet<String>,
    /// Frameworks the bound declarations are in.
    frameworks: BTreeSet<String>,
    out: String,
    /// The deployment version: older availability is no news.
    deployment: (u32, u32, u32),
    /// iOS (else macOS) availability.
    ios: bool,
}

/// The framework holding the file `path` (the outermost: a
/// sub-framework links through its umbrella).
fn framework(path: &Path) -> Option<String> {
    path.components().find_map(|c| {
        c.as_os_str()
            .to_string_lossy()
            .strip_suffix(".framework")
            .map(str::to_owned)
    })
}

/// `initWithBytes:length:` as `initWithBytes_length`, and whether that
/// name gives the selector back (else `#[selector]` says it).
fn method_name(sel: &str) -> (String, bool) {
    let pieces: Vec<&str> = sel.trim_end_matches(':').split(':').collect();
    let args = sel.matches(':').count();
    let name = pieces.join("_");
    let plain = pieces.iter().all(|p| !p.is_empty() && !p.contains('_'))
        && (args == 0 || args == pieces.len());
    let id = ident(&name);
    (id.clone(), plain && id == name)
}

impl Gen<'_> {
    /// The Objective-C type `t` is, through typedefs.
    fn obj_of(&self, mut t: TypeId) -> Option<Obj> {
        let oc = self.oc?;
        loop {
            match self.t.get(t) {
                Type::Typedef(d) => {
                    let td = &self.t.typedefs[*d as usize];
                    match td.name.as_str() {
                        "instancetype" if *d == oc.instancetype => return Some(Obj::Instancetype),
                        "id" => return Some(Obj::Any),
                        "Class" => return Some(Obj::ClassObject),
                        "SEL" => return Some(Obj::Sel),
                        _ => t = td.ty.ty,
                    }
                }
                Type::TypeOf(q) => t = q.ty,
                Type::Pointer(q) => {
                    let Type::Record(r) = self.t.get(self.t.canon(q.ty)) else {
                        return None;
                    };
                    return match oc.kind(*r) {
                        Some(ObjKind::Class(n)) => Some(Obj::Class(n.clone())),
                        Some(ObjKind::Protocol(n)) => Some(Obj::Protocol(n.clone())),
                        None => match self.t.records[*r as usize].name.as_deref() {
                            Some("objc_object") => Some(Obj::Any),
                            Some("objc_class") => Some(Obj::ClassObject),
                            Some("objc_selector") => Some(Obj::Sel),
                            _ => None,
                        },
                    };
                }
                _ => return None,
            }
        }
    }

    /// Is `t` an object pointer (`id`, a class's, a protocol's)?
    fn is_object(&self, t: TypeId) -> bool {
        matches!(
            self.obj_of(t),
            Some(Obj::Any | Obj::Class(_) | Obj::Protocol(_) | Obj::Instancetype)
        )
    }

    /// A C context's object pointer (`CPtr<u8>`), Objective-C's
    /// `Class` or `SEL`; `None` for anything else.
    pub(crate) fn c_obj(&mut self, t: TypeId) -> Option<String> {
        let o = self.obj_of(t)?;
        Some(match o {
            Obj::ClassObject => format!("{OBJC}::Class"),
            Obj::Sel => format!("{OBJC}::Sel"),
            _ => {
                self.cptr = true;
                "CPtr<u8>".to_owned()
            }
        })
    }

    /// Whether `t`'s outermost pointer may be nil: as written, else
    /// non-nil inside `NS_ASSUME_NONNULL` for a single pointer.
    fn nullable(&self, t: TypeId, written: Option<Null>, assume: bool) -> bool {
        let oc = self.oc.expect("Objective-C mode");
        let mut at = t;
        let mut n = written;
        while n.is_none() {
            n = oc
                .null
                .get(&at)
                .or_else(|| self.extra_null.get(&at))
                .copied();
            match self.t.get(at) {
                Type::Typedef(d) => at = self.t.typedefs[*d as usize].ty.ty,
                Type::TypeOf(q) => at = q.ty,
                _ => break,
            }
        }
        if let Some(n) = n {
            return n != Null::Nonnull;
        }
        let single = match self.t.get(self.t.canon(t)) {
            Type::Pointer(q) => !matches!(
                self.t.get(self.t.canon(q.ty)),
                Type::Pointer(_) | Type::Block(_)
            ),
            Type::Block(_) => true,
            _ => false,
        };
        !(assume && single)
    }

    /// The Nabi path of class `c` from the bindings.
    fn class_path(c: &str) -> String {
        if BUILTIN.contains(&c) {
            format!("{OBJC}::{c}")
        } else {
            c.to_owned()
        }
    }

    /// The Nabi name of protocol `p`: `PProtocol` when a class is `P`.
    fn protocol_name(&self, p: &str) -> String {
        if self.objc.class_names.contains(p) || BUILTIN.contains(&p) {
            format!("{p}Protocol")
        } else {
            p.to_owned()
        }
    }

    fn need(&mut self, protocol: bool, name: &str) {
        let set = if protocol {
            &mut self.objc.protocols
        } else {
            &mut self.objc.classes
        };
        if set.insert(name.to_owned()) {
            self.objc.queue.push_back((protocol, name.to_owned()));
        }
    }

    /// A method's (or block's) parameter or result type; `None` for
    /// `void`.
    fn mty(&mut self, q: QualType, nullable: bool) -> Result<Option<String>, Skip> {
        if self.t.is_void(q.ty) {
            return Ok(None);
        }
        let opt = |s: String| if nullable { format!("{s} | none") } else { s };
        if let Some(o) = self.obj_of(q.ty) {
            return Ok(Some(match o {
                Obj::Instancetype => opt(format!("{OBJC}::Id<Self>")),
                Obj::Any => opt(format!("{OBJC}::Id<{OBJC}::NSObject>")),
                Obj::Class(c) => {
                    self.need(false, &c);
                    opt(format!("{OBJC}::Id<{}>", Self::class_path(&c)))
                }
                Obj::Protocol(p) => {
                    self.need(true, &p);
                    opt(format!("{OBJC}::Id<{}>", self.protocol_name(&p)))
                }
                Obj::ClassObject => format!("{OBJC}::Class"),
                Obj::Sel => format!("{OBJC}::Sel"),
            }));
        }
        let canon = self.t.canon(q.ty);
        if let Type::Block(f) = self.t.get(canon).clone() {
            let Type::Func(func) = self.t.get(self.t.canon(f.ty)).clone() else {
                return Err("a block of no function type".to_owned());
            };
            if func.variadic {
                return Err("a variadic block".to_owned());
            }
            let mut ps = Vec::new();
            for p in &func.params {
                if matches!(self.t.get(self.t.canon(p.ty)), Type::Block(_)) {
                    return Err("a block taking a block".to_owned());
                }
                let n = self.nullable(p.ty, None, false);
                ps.push(self.mty(*p, n)?.ok_or("a `void` block parameter")?);
            }
            if matches!(self.t.get(self.t.canon(func.ret.ty)), Type::Block(_)) {
                return Err("a block returning a block".to_owned());
            }
            let n = self.nullable(func.ret.ty, None, false);
            let r = self.mty(func.ret, n)?;
            return Ok(Some(format!(
                "fn({}){}",
                ps.join(", "),
                r.map(|r| format!(" -> {r}")).unwrap_or_default()
            )));
        }
        let s = self.ty(q.ty)?;
        self.by_value(q.ty)?;
        Ok(Some(s))
    }

    /// Is `t` `NSError **`?
    fn error_out(&self, t: TypeId) -> bool {
        let Type::Pointer(q) = self.t.get(self.t.canon(t)) else {
            return false;
        };
        matches!(self.obj_of(q.ty), Some(Obj::Class(c)) if c == "NSError")
    }

    fn avail_attr(&self, avail: &Avail) -> String {
        match avail.introduced {
            Some(v)
                if v > self.objc.deployment
                    && v > if self.objc.ios {
                        (13, 4, 0)
                    } else {
                        (11, 0, 0)
                    } =>
            {
                let (major, minor, patch) = v;
                let shown = if patch == 0 {
                    format!("{major}.{minor}")
                } else {
                    format!("{major}.{minor}.{patch}")
                };
                let os = if self.objc.ios { "ios" } else { "macos" };
                format!("    #[available({os} = \"{shown}\")]\n")
            }
            _ => String::new(),
        }
    }

    /// One method's declaration in its block.
    fn method_decl(
        &mut self,
        m: &Method,
        names: &mut HashSet<(bool, String)>,
    ) -> Result<String, Skip> {
        if m.variadic {
            return Err("variadic".to_owned());
        }
        let mut params = m.params.iter().collect::<Vec<_>>();
        // The root class's (unaudited) `alloc`, `new`, `init`, `copy`
        // return an object, as Swift reads them.
        let root = matches!(
            m.sel.as_str(),
            "alloc" | "new" | "init" | "copy" | "mutableCopy"
        );
        let rnull = self.nullable(m.ret.ty, None, m.assume_nonnull || root);
        let throws = params.last().is_some_and(|p| self.error_out(p.ty.ty))
            && (self.t.prim_of(m.ret.ty) == Some(nabicc_target::Prim::Bool)
                || self.t.prim_of(m.ret.ty) == Some(nabicc_target::Prim::SChar)
                || self.obj_of(m.ret.ty).is_some());
        if throws {
            params.pop();
        }
        if matches!(self.t.get(self.t.canon(m.ret.ty)), Type::Block(_)) {
            return Err("a block result (a closure cannot be read back)".to_owned());
        }
        let ret = if throws && self.obj_of(m.ret.ty).is_none() {
            None
        } else {
            self.mty(m.ret, rnull && !throws)?
        };
        let mut seen = HashSet::default();
        let mut ps = if m.instance {
            vec!["self".to_owned()]
        } else {
            Vec::new()
        };
        for (i, p) in params.iter().enumerate() {
            let mut name = ident(&p.name);
            if name == "self" || !seen.insert(name.clone()) {
                name = format!("a{i}");
            }
            let n = self.nullable(p.ty.ty, None, m.assume_nonnull);
            let t = self
                .mty(p.ty, n)?
                .ok_or_else(|| format!("parameter `{}` is `void`", p.name))?;
            ps.push(format!("{name}: {t}"));
        }
        let (mut name, plain) = method_name(&m.sel);
        let mut explicit = !plain;
        while !names.insert((m.instance, name.clone())) {
            name.push('_');
            explicit = true;
        }
        let mut s = self.avail_attr(&m.avail);
        if explicit {
            let _ = writeln!(s, "    #[selector(\"{}\")]", m.sel);
        }
        match m.retained {
            Some(true) => s.push_str("    #[returns_retained]\n"),
            Some(false) => s.push_str("    #[returns_not_retained]\n"),
            None => {}
        }
        let _ = write!(
            s,
            "    pub fn {name}({}){}{};",
            ps.join(", "),
            if throws {
                format!(" throws ({OBJC}::Error)")
            } else {
                String::new()
            },
            ret.map(|r| format!(" -> {r}")).unwrap_or_default()
        );
        Ok(s)
    }

    /// A property's getter and setter as methods (`void` the setter's
    /// result).
    fn property_methods(p: &Property, void: TypeId) -> Vec<Method> {
        let getter = p.getter.clone().unwrap_or_else(|| p.name.clone());
        let base = Method {
            instance: !p.class,
            sel: getter,
            ret: p.ty,
            params: Vec::new(),
            variadic: false,
            optional: p.optional,
            avail: p.avail,
            retained: None,
            consumes_self: false,
            assume_nonnull: p.assume_nonnull,
            loc: p.loc,
        };
        let mut out = vec![base.clone()];
        if !p.readonly {
            let mut cap = p.name.clone();
            if let Some(f) = cap.get(..1) {
                let up = f.to_ascii_uppercase();
                cap.replace_range(..1, &up);
            }
            let sel = p.setter.clone().unwrap_or_else(|| format!("set{cap}:"));
            out.push(Method {
                sel,
                ret: QualType::new(void),
                params: vec![nabicc_parse::objc::Param {
                    name: p.name.clone(),
                    ty: p.ty,
                    consumed: false,
                }],
                ..base
            });
        }
        out
    }

    /// The methods of `ms` (and properties `ps`) as declarations.
    fn members(&mut self, ms: &[&Method], ps: &[&Property], body: &mut String) {
        let oc = self.oc.expect("Objective-C mode");
        let mut names = HashSet::default();
        let mut sels = HashSet::default();
        let void = self.void_type();
        let mut all: Vec<Method> = ms.iter().map(|m| (*m).clone()).collect();
        for p in ps {
            for m in Self::property_methods(p, void) {
                // Nullability from the attributes reaches the type.
                if let Some(n) = p.null {
                    let t = if m.params.is_empty() {
                        m.ret.ty
                    } else {
                        m.params[0].ty.ty
                    };
                    if !oc.null.contains_key(&t) {
                        self.extra_null.insert(t, n);
                    }
                }
                all.push(m);
            }
        }
        for m in &all {
            if m.optional || m.avail.unavailable || NEVER.contains(&m.sel.as_str()) {
                continue;
            }
            if !sels.insert((m.instance, m.sel.clone())) {
                continue;
            }
            match self.method_decl(m, &mut names) {
                Ok(d) => {
                    body.push_str(&d);
                    body.push('\n');
                }
                Err(why) => {
                    let _ = writeln!(
                        body,
                        "    // skipped {}[{}]: {why}",
                        if m.instance { '-' } else { '+' },
                        m.sel
                    );
                }
            }
        }
    }

    fn void_type(&self) -> TypeId {
        self.t.prim(nabicc_target::Prim::Void)
    }

    /// The Objective-C part: every class and protocol of the roots,
    /// then what they use.
    pub(crate) fn objc_items(&mut self) {
        let Some(oc) = self.oc else { return };
        self.objc.class_names = oc.interfaces.iter().map(|i| i.name.clone()).collect();
        let mut ifaces: BTreeMap<&str, Vec<&Interface>> = BTreeMap::new();
        for i in &oc.interfaces {
            ifaces.entry(i.name.as_str()).or_default().push(i);
        }
        let protos: HashMap<&str, &nabicc_parse::objc::Protocol> =
            oc.protocols.iter().map(|p| (p.name.as_str(), p)).collect();
        // Bound: those with a declaration in the roots; the root class
        // always (its `alloc`, `new`, `init`).
        let bound: Vec<&str> = ifaces
            .iter()
            .filter(|(n, is)| **n == "NSObject" || is.iter().any(|i| self.has(i.loc)))
            .map(|(n, _)| *n)
            .collect();
        let bound_p: Vec<&str> = oc
            .protocols
            .iter()
            .filter(|p| self.has(p.loc))
            .map(|p| p.name.as_str())
            .collect();
        for c in &bound {
            self.objc.classes.insert((*c).to_owned());
        }
        for p in &bound_p {
            self.objc.protocols.insert((*p).to_owned());
        }
        // The C declarations' uses so far: those bound here get blocks.
        self.objc
            .queue
            .retain(|(p, n)| !if *p { &bound_p } else { &bound }.contains(&n.as_str()));
        for c in bound {
            let is = &ifaces[c];
            let root =
                |i: &&Interface| (c == "NSObject" && i.category.is_none()) || self.has(i.loc);
            let mine: Vec<&Interface> = is.iter().copied().filter(root).collect();
            self.class_block(c, &mine, is, true);
        }
        for p in bound_p {
            self.protocol_block(protos[p], true);
        }
        // What they use, without methods.
        while let Some((protocol, n)) = self.objc.queue.pop_front() {
            if protocol {
                match protos.get(n.as_str()) {
                    Some(p) => self.protocol_block(p, false),
                    None => {
                        let _ = writeln!(
                            self.objc.out,
                            "#[objc::protocol({})]\nextern \"objc\" {{}}",
                            self.protocol_name(&n)
                        );
                    }
                }
            } else if !BUILTIN.contains(&n.as_str()) {
                let is = ifaces.get(n.as_str()).cloned().unwrap_or_default();
                self.class_block(&n, &[], &is, false);
            }
        }
    }

    /// The block of class `c`: `mine` are the interfaces whose methods
    /// it binds, `all` every one of it (for the superclass).
    fn class_block(&mut self, c: &str, mine: &[&Interface], all: &[&Interface], full: bool) {
        let main = all.iter().find(|i| i.category.is_none());
        let sup = main.and_then(|i| i.sup.clone());
        if let Some(s) = &sup {
            self.need(false, s);
        }
        let mut conforms = Vec::new();
        if full {
            for i in mine {
                for p in &i.protocols {
                    if !conforms.contains(p) {
                        conforms.push(p.clone());
                    }
                }
            }
        }
        for p in &conforms {
            self.need(true, p);
        }
        let fw = main
            .or_else(|| all.first())
            .and_then(|i| self.files.get(i.loc.file as usize))
            .and_then(|f| framework(&f.path));
        let mut body = String::new();
        let ms: Vec<&Method> = mine.iter().flat_map(|i| &i.methods).collect();
        let ps: Vec<&Property> = mine.iter().flat_map(|i| &i.props).collect();
        self.members(&ms, &ps, &mut body);
        let head = match (&sup, main) {
            (Some(s), _) => format!("@interface {c} : {s}"),
            (None, Some(_)) => format!("@interface {c}"),
            (None, None) => format!("@class {c}"),
        };
        let mut s = format!("/// `{head}`\n");
        if let Some(f) = &fw {
            self.objc.frameworks.insert(f.clone());
            let _ = writeln!(s, "#[link(name = \"{f}\", kind = \"framework\")]");
        }
        if BUILTIN.contains(&c) {
            let _ = writeln!(s, "#[objc::category({c})]");
        } else {
            let mut args = vec![c.to_owned()];
            // A class without a superclass here is a root class
            // (`NSProxy`): `NSObject` stands in.
            args.push(format!("extends({})", sup.as_deref().unwrap_or("NSObject")));
            if !conforms.is_empty() {
                let names: Vec<String> = conforms.iter().map(|p| self.protocol_name(p)).collect();
                args.push(format!("conforms({})", names.join(", ")));
            }
            let _ = writeln!(s, "#[objc::class({})]", args.join(", "));
        }
        let _ = writeln!(s, "extern \"objc\" {{\n{body}}}");
        self.objc.out.push_str(&s);
    }

    fn protocol_block(&mut self, p: &nabicc_parse::objc::Protocol, full: bool) {
        for q in &p.protocols {
            self.need(true, q);
        }
        let mut body = String::new();
        if full {
            let ms: Vec<&Method> = p.methods.iter().filter(|m| m.instance).collect();
            let ps: Vec<&Property> = p.props.iter().filter(|x| !x.class).collect();
            self.members(&ms, &ps, &mut body);
        }
        let name = self.protocol_name(&p.name);
        let mut args = vec![name];
        let parents: Vec<String> = p.protocols.iter().map(|q| self.protocol_name(q)).collect();
        if let Some((first, rest)) = parents.split_first() {
            args.push(format!("extends({first})"));
            if !rest.is_empty() {
                args.push(format!("conforms({})", rest.join(", ")));
            }
        }
        let _ = write!(
            self.objc.out,
            "/// `@protocol {}`\n#[objc::protocol({})]\nextern \"objc\" {{\n{body}}}\n",
            p.name,
            args.join(", ")
        );
    }

    /// Does C function `f`'s type involve Objective-C objects or blocks?
    pub(crate) fn objc_fn(&self, f: usize) -> bool {
        let Some(t) = self.function_type(self.fns[f].ty) else {
            return false;
        };
        let func = self.func(t);
        std::iter::once(func.ret)
            .chain(func.params.iter().copied())
            .any(|q| {
                matches!(
                    self.obj_of(q.ty),
                    Some(Obj::Any | Obj::Class(_) | Obj::Protocol(_) | Obj::Instancetype)
                ) || matches!(self.t.get(self.t.canon(q.ty)), Type::Block(_))
            })
    }

    /// C function `f` with object parameters or result: its raw
    /// declaration and a wrapper holding `Id`s.
    pub(crate) fn objc_function(&mut self, f: usize) {
        let c_name = self.fns[f].name.clone();
        let id = ident(&c_name);
        let r = self.objc_function_text(f, &c_name, &id);
        match r {
            Ok(()) => {
                self.values.insert(id);
            }
            Err(why) => self.skip("fn", &c_name, &why),
        }
    }

    fn objc_function_text(&mut self, f: usize, c_name: &str, id: &str) -> Result<(), Skip> {
        let t = self
            .function_type(self.fns[f].ty)
            .ok_or("not a function type")?;
        let func = self.func(t);
        if func.variadic || !func.proto {
            return Err("variadic, with Objective-C objects".to_owned());
        }
        let info = self
            .oc
            .and_then(|o| o.fns.get(&f).copied())
            .unwrap_or_default();
        let raw_name = format!("__nabi_objc_fn_{c_name}");
        let mut raw = Vec::new();
        let mut params = Vec::new();
        let mut args = Vec::new();
        let names = self.fns[f].params.clone();
        for (i, q) in func.params.iter().enumerate() {
            let mut name = ident(names.get(i).and_then(|p| p.0.as_deref()).unwrap_or(""));
            if name.is_empty()
                || params
                    .iter()
                    .any(|p: &String| p.starts_with(&format!("{name}:")))
            {
                name = format!("arg{i}");
            }
            if matches!(self.t.get(self.t.canon(q.ty)), Type::Block(_)) {
                return Err("a block parameter".to_owned());
            }
            if self.is_object(q.ty) {
                let n = self.nullable(q.ty, None, info.assume_nonnull);
                let shown = self.mty(*q, n)?.unwrap_or_default();
                raw.push(format!("{name}: CPtr<u8>"));
                args.push(if n {
                    format!("{OBJC}::raw_or_nil({name})")
                } else {
                    format!("{name}.as_raw()")
                });
                params.push(format!("{name}: {shown}"));
            } else {
                let s = self.ty(q.ty)?;
                self.by_value(q.ty)?;
                raw.push(format!("{name}: {s}"));
                args.push(name.clone());
                params.push(format!("{name}: {s}"));
            }
        }
        self.cptr = true;
        let call = format!("unsafe {{ {raw_name}({}) }}", args.join(", "));
        let (raw_ret, ret, body) = if self.is_object(func.ret.ty) {
            let n = self.nullable(func.ret.ty, None, info.assume_nonnull);
            let shown = self.mty(func.ret, false)?.unwrap_or_default();
            let inner = shown
                .strip_prefix(&format!("{OBJC}::Id<"))
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or("")
                .to_owned();
            let take = if info.retained == Some(true) {
                "from_retained"
            } else {
                "retain"
            };
            let conv = format!("unsafe {{ {OBJC}::Id::<{inner}>::{take}({call}) }}");
            if n {
                (
                    " -> CPtr<u8>".to_owned(),
                    format!(" -> {shown} | none"),
                    conv,
                )
            } else {
                (
                    " -> CPtr<u8>".to_owned(),
                    format!(" -> {shown}"),
                    format!("{conv} ?? {OBJC}::nil_result(\"{c_name}\")"),
                )
            }
        } else {
            let r = self.ret(func.ret.ty)?;
            self.by_value(func.ret.ty)?;
            (r.clone(), r, call)
        };
        let sym = self.symbol(c_name, self.fns[f].asm.as_deref());
        let _ = writeln!(
            self.blocks[0],
            "    #[link_name = \"{sym}\"]\n    fn {raw_name}({}){raw_ret};",
            raw.join(", ")
        );
        let avail = self.avail_attr(&info.avail);
        let _ = writeln!(
            self.objc.out,
            "{}pub fn {id}({}){ret} {{ {body} }}",
            avail.trim_start(),
            params.join(", ")
        );
        Ok(())
    }

    /// An extern global holding an object: its raw static and a getter.
    pub(crate) fn objc_var(&mut self, v: usize) -> bool {
        let t = self.vars[v].ty.ty;
        let Some(Obj::Any | Obj::Class(_) | Obj::Protocol(_)) = self.obj_of(t) else {
            return false;
        };
        let c_name = self.vars[v].name.clone();
        let id = ident(&c_name);
        let Ok(Some(shown)) = self.mty(QualType::new(t), false) else {
            return false;
        };
        let inner = shown
            .strip_prefix(&format!("{OBJC}::Id<"))
            .and_then(|s| s.strip_suffix('>'))
            .unwrap_or("")
            .to_owned();
        // A static cannot take another name: the global is reached as
        // a function's address, as `_NSConcreteStackBlock` is.
        let sym = self.symbol(&c_name, self.vars[v].asm.as_deref());
        let _ = writeln!(
            self.blocks[0],
            "    #[link_name = \"{sym}\"]\n    fn __nabi_objc_var_{c_name}();"
        );
        let _ = writeln!(
            self.objc.out,
            "/// The `{c_name}` constant.\npub fn {id}() -> {shown} {{ unsafe {{ \
             {OBJC}::Id::<{inner}>::retain({OBJC}::global(__nabi_objc_var_{c_name})) }} ?? \
             {OBJC}::nil_result(\"{c_name}\") }}"
        );
        self.values.insert(id);
        true
    }

    /// `NS_ENUM` / `NS_OPTIONS` / `NS_CLOSED_ENUM` `name` over `int`: a
    /// transparent struct, options with `|`, `&` and `contains`.
    pub(crate) fn objc_enum(&mut self, kind: EnumKind, name: &str, int: &str) {
        let _ = write!(
            self.items,
            "#[repr(transparent)]\npub struct {name}(pub {int});\n\
             impl Clone for {name} {{ const fn clone(self) -> {name} {{ self }} }}\n\
             impl Copy for {name} {{}}\n\
             impl PartialEq for {name} {{ fn eq(self, rhs: {name}) -> bool {{ self.0 == rhs.0 }} }}\n\
             impl Eq for {name} {{}}\n"
        );
        if kind == EnumKind::Options {
            let _ = write!(
                self.items,
                "impl core::ops::BitOr for {name} {{ type Output = {name}; fn bitor(self, rhs: {name}) -> {name} {{ {name}(self.0 | rhs.0) }} }}\n\
                 impl core::ops::BitAnd for {name} {{ type Output = {name}; fn bitand(self, rhs: {name}) -> {name} {{ {name}(self.0 & rhs.0) }} }}\n\
                 impl {name} {{\n    /// Every flag of `o` is set.\n    \
                 pub fn contains(self, o: {name}) -> bool {{ self.0 & o.0 == o.0 }}\n}}\n"
            );
        }
    }

    /// The `#[link]` blocks of the frameworks and the Objective-C part.
    pub(crate) fn objc_finish(&self, out: &mut String) {
        if self.oc.is_none() {
            return;
        }
        // The classes `bare::ffi::objc` declares, and `Id`, come with a
        // glob import of the bindings.
        let _ = write!(
            out,
            "\npub use {OBJC}::{{Id, NSError, NSObject, NSString}};\n\n"
        );
        out.push_str(&self.objc.out);
    }

    pub(crate) fn set_deployment(&mut self, v: (u32, u32, u32), ios: bool) {
        self.objc.deployment = v;
        self.objc.ios = ios;
    }
}
