//! Objective-C declarations as headers have them.
//!
//! `@class`, `@protocol`, `@interface` (categories and extensions too),
//! methods and properties, plus what the C declarations around them
//! carry for Objective-C: nullability, availability, ownership
//! conventions and enum kinds.
//!
//! A class is an opaque record a typedef of its name refers to, so
//! `NSString *` reads as a pointer to it; a protocol is an opaque record
//! no name refers to, and `id<P>` is a pointer to it. Lightweight
//! generic arguments are read and dropped; a class's type parameters are
//! `id` inside its declarations.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// A pointer's nullability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Null {
    Nonnull,
    Nullable,
    Unspecified,
}

impl Null {
    /// The nullability keyword `s` (`_Nonnull`, ..).
    pub(crate) const fn of(s: Sym) -> Option<Self> {
        match s {
            kw::NONNULL => Some(Self::Nonnull),
            kw::NULLABLE | kw::NULLABLE_RESULT => Some(Self::Nullable),
            kw::NULL_UNSPECIFIED => Some(Self::Unspecified),
            _ => None,
        }
    }
}

/// macOS availability of a declaration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Avail {
    /// `introduced=`, as `(major, minor, patch)`.
    pub introduced: Option<(u32, u32, u32)>,
    /// Not on macOS: `unavailable`, `API_UNAVAILABLE(macos)`,
    /// `NS_UNAVAILABLE`.
    pub unavailable: bool,
}

impl Avail {
    pub(crate) fn merge(&mut self, o: &Self) {
        if o.introduced > self.introduced {
            self.introduced = o.introduced;
        }
        self.unavailable |= o.unavailable;
    }
}

/// `NS_ENUM` (open `enum_extensibility`), `NS_OPTIONS` (`flag_enum`) or
/// `NS_CLOSED_ENUM`; plain C enums have no entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnumKind {
    Open,
    Options,
    Closed,
}

/// What a class or protocol record stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObjKind {
    Class(String),
    Protocol(String),
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: QualType,
    /// `NS_CONSUMED`.
    pub consumed: bool,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // independent facts of a declaration
pub struct Method {
    /// `-` (not `+`).
    pub instance: bool,
    /// `initWithBytes:length:`.
    pub sel: String,
    pub ret: QualType,
    pub params: Vec<Param>,
    pub variadic: bool,
    /// In a protocol's `@optional` part.
    pub optional: bool,
    pub avail: Avail,
    /// `NS_RETURNS_RETAINED` (`Some(true)`) or `NS_RETURNS_NOT_RETAINED`.
    pub retained: Option<bool>,
    pub consumes_self: bool,
    /// Declared inside `NS_ASSUME_NONNULL_BEGIN/END`.
    pub assume_nonnull: bool,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // independent facts of a declaration
pub struct Property {
    pub name: String,
    pub ty: QualType,
    /// From the attribute list (`nullable`, `nonnull`, ..).
    pub null: Option<Null>,
    pub readonly: bool,
    /// `class`: a property of the class object.
    pub class: bool,
    pub getter: Option<String>,
    pub setter: Option<String>,
    pub optional: bool,
    pub avail: Avail,
    pub assume_nonnull: bool,
    pub loc: Loc,
}

/// `@interface`: a class, or (`category`) more of one.
#[derive(Clone, Debug)]
pub struct Interface {
    pub name: String,
    /// `Some("")` for a class extension `@interface C ()`.
    pub category: Option<String>,
    pub sup: Option<String>,
    pub protocols: Vec<String>,
    /// Lightweight generic parameters (`ObjectType`).
    pub params: Vec<String>,
    pub methods: Vec<Method>,
    pub props: Vec<Property>,
    pub avail: Avail,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct Protocol {
    pub name: String,
    pub protocols: Vec<String>,
    pub methods: Vec<Method>,
    pub props: Vec<Property>,
    pub avail: Avail,
    pub loc: Loc,
}

/// What a C function declaration carries for Objective-C.
#[derive(Clone, Copy, Debug, Default)]
pub struct FnInfo {
    pub avail: Avail,
    pub retained: Option<bool>,
    pub assume_nonnull: bool,
}

#[derive(Default)]
pub struct Objc {
    /// Objective-C is being read (`Parser::objc_mode`).
    pub on: bool,
    pub interfaces: Vec<Interface>,
    pub protocols: Vec<Protocol>,
    /// The records standing for classes and protocols.
    pub kinds: HashMap<RecordId, ObjKind>,
    /// Protocol records by name.
    pub protocol_records: HashMap<String, RecordId>,
    /// Nullability written on pointer and typedef uses.
    pub null: HashMap<TypeId, Null>,
    /// Inside `NS_ASSUME_NONNULL_BEGIN/END`.
    pub nonnull: bool,
    pub enums: HashMap<EnumId, EnumKind>,
    /// By function index.
    pub fns: HashMap<usize, FnInfo>,
    /// `const` integer variables' values, by variable index.
    pub consts: HashMap<usize, i128>,
    /// The `instancetype` typedef.
    pub instancetype: TypedefId,
    pub(crate) kindof: Sym,
    covariant: Sym,
    contravariant: Sym,
}

impl Objc {
    pub(crate) fn fn_info(&mut self, id: usize, a: &Attrs) {
        self.fns.insert(
            id,
            FnInfo {
                avail: a.avail,
                retained: a.retained,
                assume_nonnull: self.nonnull,
            },
        );
    }

    /// The class or protocol a record stands for.
    pub fn kind(&self, r: RecordId) -> Option<&ObjKind> {
        self.kinds.get(&r)
    }
}

/// Context words in a method's or property's type: they qualify, they
/// are not types.
const CONTEXT: &[&str] = &[
    "in",
    "out",
    "inout",
    "bycopy",
    "byref",
    "oneway",
    "nullable",
    "nonnull",
    "null_unspecified",
    "null_resettable",
];

impl Parser<'_, '_> {
    /// Read Objective-C from here on: `@` declarations, `id<P>`,
    /// `instancetype` and generic arguments.
    pub fn objc_mode(&mut self) {
        self.objc.on = true;
        self.objc.kindof = self.pp.names.intern("__kindof");
        self.objc.covariant = self.pp.names.intern("__covariant");
        self.objc.contravariant = self.pp.names.intern("__contravariant");
        // `instancetype` is `id` (the `struct objc_object *` objc.h
        // declares) that names the receiver's class.
        let tag = self.pp.names.intern("objc_object");
        let rid = if let Some(Tag::Record(r)) = self.tags.get(&tag) {
            *r
        } else {
            let r = self.new_record(Some("objc_object".to_owned()), false);
            self.bind_tag(tag, Tag::Record(r));
            r
        };
        let rec = self.types.add(Type::Record(rid));
        let ptr = self.types.pointer(QualType::new(rec));
        self.types.typedefs.push(Typedef {
            name: "instancetype".to_owned(),
            ty: QualType::new(ptr),
            align: 0,
            loc: Loc::default(),
        });
        let id = (self.types.typedefs.len() - 1) as TypedefId;
        self.objc.instancetype = id;
        let s = self.pp.names.intern("instancetype");
        self.ords.insert(s, Ord::Typedef(id));
    }

    /// At `@`, the start of an Objective-C declaration?
    pub(crate) fn at_objc(&mut self) -> R<bool> {
        let t = self.peek()?;
        Ok(t.kind == Kind::Other && self.name(t.sym) == "@")
    }

    /// The `@` keyword after `@`.
    fn at_word(&mut self) -> R<Option<String>> {
        if !self.at_objc()? {
            return Ok(None);
        }
        let t = self.peek_at(1)?;
        Ok((t.kind == Kind::Ident).then(|| self.name(t.sym).to_owned()))
    }

    fn ident(&mut self, what: &str) -> R<(String, Sym, Loc)> {
        let t = self.peek()?;
        if t.kind != Kind::Ident {
            return Err(self.err(
                t.loc,
                &format!("expected {what}, found {}", self.describe(t)),
            ));
        }
        self.bump()?;
        Ok((self.name(t.sym).to_owned(), t.sym, t.loc))
    }

    /// `@class`, `@protocol`, `@interface`, .. at file scope; `attrs`
    /// came before it.
    pub(crate) fn objc_decl(&mut self, attrs: &Attrs) -> R<()> {
        let at = self.bump()?;
        let (word, _, loc) = self.ident("an Objective-C keyword after '@'")?;
        match word.as_str() {
            "class" => loop {
                let (name, sym, loc) = self.ident("a class name")?;
                if self.at("<")? {
                    self.skip_angles()?;
                }
                self.class_type(&name, sym, loc);
                if !self.eat(",")? {
                    self.expect(";")?;
                    return Ok(());
                }
            },
            "protocol" => self.protocol(attrs, loc),
            "interface" => self.interface(attrs, loc),
            "compatibility_alias" => {
                let (_, alias, _) = self.ident("an alias name")?;
                let (_, class, _) = self.ident("a class name")?;
                self.expect(";")?;
                if let Some(o) = self.ords.get(&class).copied() {
                    self.bind(alias, o);
                }
                Ok(())
            }
            "import" => {
                while !self.eat(";")? {
                    if self.peek()?.kind == Kind::Eof {
                        return Err(self.err(at.loc, "expected ';'"));
                    }
                    self.bump()?;
                }
                Ok(())
            }
            "implementation" => Err(self.err(
                loc,
                "`@implementation` in a header: headers only declare, and the Objective-C \
                 front end reads declarations; move the implementation to a `.m` file",
            )),
            w => Err(self.err(
                loc,
                &format!(
                    "`@{w}` is not an Objective-C declaration this front end reads (`@class`, \
                     `@protocol`, `@interface`)"
                ),
            )),
        }
    }

    /// The type of class `name`: an opaque record its typedef names.
    fn class_type(&mut self, name: &str, sym: Sym, loc: Loc) {
        if self.is_typedef_name(sym) {
            return;
        }
        let rid = self.new_record(None, false);
        self.objc.kinds.insert(rid, ObjKind::Class(name.to_owned()));
        let rec = self.types.add(Type::Record(rid));
        self.types.typedefs.push(Typedef {
            name: name.to_owned(),
            ty: QualType::new(rec),
            align: 0,
            loc,
        });
        let id = (self.types.typedefs.len() - 1) as TypedefId;
        self.bind(sym, Ord::Typedef(id));
    }

    fn protocol_record(&mut self, name: &str) -> RecordId {
        if let Some(r) = self.objc.protocol_records.get(name) {
            return *r;
        }
        let rid = self.new_record(None, false);
        self.objc
            .kinds
            .insert(rid, ObjKind::Protocol(name.to_owned()));
        self.objc.protocol_records.insert(name.to_owned(), rid);
        rid
    }

    /// Skip `<..>` from its `<`, `>>` closing two.
    fn skip_angles(&mut self) -> R<()> {
        let open = self.bump()?;
        let mut depth = 1i32;
        loop {
            let t = self.bump()?;
            if t.kind == Kind::Eof {
                return Err(self.err(open.loc, "this '<' is never closed"));
            }
            if t.kind != Kind::Punct {
                continue;
            }
            match self.name(t.sym) {
                "<" => depth += 1,
                ">" => depth -= 1,
                ">>" if depth == 1 => {
                    // The second `>` closes an outer list.
                    let mut gt = t;
                    gt.sym = self.pp.names.intern(">");
                    gt.loc.col += 1;
                    self.buf.push_front(gt);
                    return Ok(());
                }
                ">>" => depth -= 2,
                "(" | "[" | "{" => {
                    self.buf.push_front(t);
                    self.skip_balanced()?;
                }
                _ => {}
            }
            if depth <= 0 {
                return Ok(());
            }
        }
    }

    /// Eat a `>`, splitting `>>` (`id<NSCopying>>` closes two lists).
    fn eat_close(&mut self) -> R<bool> {
        if self.eat(">")? {
            return Ok(true);
        }
        if !self.at(">>")? {
            return Ok(false);
        }
        let gt = self.pp.names.intern(">");
        if let Some(t) = self.buf.iter_mut().find(|t| t.kind != Kind::Pragma) {
            t.sym = gt;
            t.loc.col += 1;
        }
        Ok(true)
    }

    /// `<P, Q>`: names, from the `<`.
    fn name_list(&mut self) -> R<Vec<String>> {
        self.expect("<")?;
        let mut names = Vec::new();
        loop {
            let (n, _, _) = self.ident("a protocol name")?;
            names.push(n);
            if self.eat_close()? {
                return Ok(names);
            }
            self.expect(",")?;
        }
    }

    /// `<..>` after typedef name `d` (whose use is `base`): `id<P>` is
    /// a pointer to protocol `P`, other arguments are dropped.
    pub(crate) fn objc_angles(&mut self, d: TypedefId, base: TypeId) -> R<TypeId> {
        let is_id = self.types.typedefs[d as usize].name == "id";
        let first = self.peek_at(1)?;
        let named = first.kind == Kind::Ident && !self.is_typedef_name(first.sym);
        if is_id && named {
            let protocols = self.name_list()?;
            let rid = self.protocol_record(&protocols[0]);
            let rec = self.types.add(Type::Record(rid));
            return Ok(self.types.pointer(QualType::new(rec)));
        }
        self.skip_angles()?;
        Ok(base)
    }

    /// `availability(macos, introduced=11.0, ..)` from its `(`: the
    /// target's platform's (`ios` for iOS targets).
    pub(crate) fn availability(&mut self, a: &mut objc::Avail) -> R<()> {
        self.expect("(")?;
        let p = self.bump()?;
        let platform = self.name(p.sym).to_owned();
        let mac = if self.types.t.ios {
            matches!(platform.as_str(), "ios" | "iOS")
        } else {
            matches!(platform.as_str(), "macos" | "macosx" | "macOS")
        };
        loop {
            let t = self.peek()?;
            if self.is_p(t, ")") {
                self.bump()?;
                return Ok(());
            }
            if t.kind == Kind::Eof {
                return Err(self.err(p.loc, "expected ')' after the availability"));
            }
            self.bump()?;
            if t.kind != Kind::Ident {
                if self.is_p(t, "(") {
                    self.buf.push_front(t);
                    self.skip_balanced()?;
                }
                continue;
            }
            let key = self.name(t.sym).to_owned();
            if !mac {
                continue;
            }
            if key == "unavailable" {
                a.unavailable = true;
            } else if key == "introduced" && self.eat("=")? {
                let v = self.bump()?;
                a.introduced = Some(version(self.name(v.sym)));
            }
        }
    }

    /// Nullability of pointer `ty` made by a declarator chunk: its own,
    /// else the specifiers' when it is the outermost.
    pub(crate) fn objc_null(&mut self, ty: TypeId, own: Option<Null>, specs: &Specs, outer: bool) {
        if !self.objc.on {
            return;
        }
        if let Some(n) = own.or(if outer { specs.null } else { None }) {
            self.objc.null.insert(ty, n);
        }
    }

    /// `@protocol P;`, `@protocol P, Q;` or `@protocol P <Q> .. @end`.
    fn protocol(&mut self, attrs: &Attrs, loc: Loc) -> R<()> {
        let (name, _, _) = self.ident("a protocol name")?;
        self.protocol_record(&name);
        if self.at(";")? || self.at(",")? {
            while self.eat(",")? {
                let (n, _, _) = self.ident("a protocol name")?;
                self.protocol_record(&n);
            }
            self.expect(";")?;
            return Ok(());
        }
        let protocols = if self.at("<")? {
            self.name_list()?
        } else {
            Vec::new()
        };
        let mut a = attrs.clone();
        self.attributes(&mut a)?;
        let (methods, props) = self.members()?;
        self.objc.protocols.push(Protocol {
            name,
            protocols,
            methods,
            props,
            avail: a.avail,
            loc,
        });
        Ok(())
    }

    /// `@interface C<T> : S <P> { ivars } .. @end`, or a category
    /// `@interface C (Name) <P> .. @end`.
    fn interface(&mut self, attrs: &Attrs, loc: Loc) -> R<()> {
        let (name, sym, nloc) = self.ident("a class name")?;
        self.class_type(&name, sym, nloc);
        // `<..>` after the name: type parameters when `:` or `(` follows
        // (or variance or a bound says so), else a root class's protocols.
        let mut params = Vec::new();
        let mut protocols = Vec::new();
        if self.at("<")? {
            let (list, typed) = self.type_params()?;
            if typed || self.at(":")? || self.at("(")? {
                params = list;
            } else {
                protocols = list;
            }
        }
        let mut category = None;
        let mut sup = None;
        if self.eat("(")? {
            let t = self.peek()?;
            if t.kind == Kind::Ident {
                self.bump()?;
                category = Some(self.name(t.sym).to_owned());
            } else {
                category = Some(String::new());
            }
            self.expect(")")?;
        } else if self.eat(":")? {
            let (s, _, _) = self.ident("a superclass name")?;
            sup = Some(s);
            if self.at("<")? && !self.angles_are_protocols()? {
                self.skip_angles()?;
            }
        }
        if self.at("<")? {
            protocols.extend(self.name_list()?);
        }
        if self.at("{")? {
            self.skip_balanced()?;
        }
        // The type parameters are `id` inside.
        let id = self.pp.names.intern("id");
        let id_def = match self.ords.get(&id) {
            Some(Ord::Typedef(d)) => Some(*d),
            _ => None,
        };
        let mut saved = Vec::new();
        if let Some(d) = id_def {
            for p in &params {
                let s = self.pp.names.intern(p);
                saved.push((s, self.ords.insert(s, Ord::Typedef(d))));
            }
        }
        let r = self.members();
        for (s, old) in saved {
            match old {
                Some(o) => self.ords.insert(s, o),
                None => self.ords.remove(&s),
            };
        }
        let (methods, props) = r?;
        self.objc.interfaces.push(Interface {
            name,
            category,
            sup,
            protocols,
            params,
            methods,
            props,
            avail: attrs.avail,
            loc,
        });
        Ok(())
    }

    /// Are the `<..>` ahead protocol names (`: NSObject <NSCopying>`)
    /// rather than type arguments (`: NSArray<ObjectType>`)?
    fn angles_are_protocols(&mut self) -> R<bool> {
        let t = self.peek_at(1)?;
        Ok(t.kind == Kind::Ident
            && !self.is_typedef_name(t.sym)
            && self.objc.protocol_records.contains_key(self.name(t.sym)))
    }

    /// `<__covariant T : Bound, U>` from its `<`: the names, and whether
    /// variance or a bound marked them as type parameters.
    fn type_params(&mut self) -> R<(Vec<String>, bool)> {
        self.expect("<")?;
        let mut names = Vec::new();
        let mut typed = false;
        loop {
            let mut t = self.peek()?;
            if t.kind == Kind::Ident
                && (t.sym == self.objc.covariant || t.sym == self.objc.contravariant)
            {
                typed = true;
                self.bump()?;
                t = self.peek()?;
            }
            if t.kind != Kind::Ident {
                return Err(self.err(
                    t.loc,
                    &format!("expected a name, found {}", self.describe(t)),
                ));
            }
            self.bump()?;
            names.push(self.name(t.sym).to_owned());
            if self.eat(":")? {
                typed = true;
                self.type_name()?;
            }
            if self.eat_close()? {
                return Ok((names, typed));
            }
            self.expect(",")?;
        }
    }

    /// Methods and properties up to `@end` (C declarations among them
    /// are read as at file scope).
    fn members(&mut self) -> R<(Vec<Method>, Vec<Property>)> {
        let mut methods = Vec::new();
        let mut props = Vec::new();
        let mut optional = false;
        loop {
            let t = self.peek()?;
            if t.kind == Kind::Eof {
                return Err(self.err(t.loc, "expected '@end' before the end of the input"));
            }
            if self.is_p(t, "-") || self.is_p(t, "+") {
                methods.push(self.method(optional)?);
                continue;
            }
            if self.is_p(t, ";") {
                self.bump()?;
                continue;
            }
            match self.at_word()?.as_deref() {
                Some("end") => {
                    self.bump()?;
                    self.bump()?;
                    return Ok((methods, props));
                }
                Some("optional") => {
                    self.bump()?;
                    self.bump()?;
                    optional = true;
                }
                Some("required") => {
                    self.bump()?;
                    self.bump()?;
                    optional = false;
                }
                Some("property") => {
                    self.bump()?;
                    self.bump()?;
                    self.property(optional, &mut props)?;
                }
                Some("public" | "private" | "protected" | "package") => {
                    self.bump()?;
                    self.bump()?;
                }
                _ => self.external()?,
            }
        }
    }

    /// `(nullable T *)`: the type in a method's parentheses.
    fn method_type(&mut self) -> R<QualType> {
        self.expect("(")?;
        let mut null = None;
        loop {
            let t = self.peek()?;
            if t.kind != Kind::Ident || !CONTEXT.contains(&self.name(t.sym)) {
                break;
            }
            // `in` and its kin are words only before a type.
            if self.second_is(")")? || self.second_is("*")? {
                break;
            }
            null = match self.name(t.sym) {
                "nullable" | "null_resettable" => Some(Null::Nullable),
                "nonnull" => Some(Null::Nonnull),
                "null_unspecified" => Some(Null::Unspecified),
                _ => null,
            };
            self.bump()?;
        }
        let q = self.type_name()?;
        self.expect(")")?;
        if let Some(n) = null {
            self.objc.null.insert(q.ty, n);
        }
        Ok(q)
    }

    /// `id`, for a method without a written type.
    fn id_type(&mut self) -> QualType {
        let id = self.pp.names.intern("id");
        match self.ords.get(&id) {
            Some(Ord::Typedef(d)) => QualType::new(self.types.add(Type::Typedef(*d))),
            _ => QualType::new(self.types.add(Type::Typedef(self.objc.instancetype))),
        }
    }

    /// `- (R)a:(A)x b:(B)y attrs;`.
    fn method(&mut self, optional: bool) -> R<Method> {
        let sign = self.bump()?;
        let instance = self.is_p(sign, "-");
        let ret = if self.at("(")? {
            self.method_type()?
        } else {
            self.id_type()
        };
        let mut sel = String::new();
        let mut params = Vec::new();
        let mut variadic = false;
        // Attributes after a parameter's name may be the method's.
        let mut trailing = Attrs::default();
        loop {
            let t = self.peek()?;
            let piece = if t.kind == Kind::Ident {
                if !sel.is_empty() && !self.second_is(":")? {
                    break;
                }
                self.bump()?;
                self.name(t.sym).to_owned()
            } else if self.is_p(t, ":") {
                String::new()
            } else {
                break;
            };
            if !self.eat(":")? {
                if params.is_empty() && sel.is_empty() {
                    sel = piece;
                }
                break;
            }
            sel.push_str(&piece);
            sel.push(':');
            let ty = if self.at("(")? {
                self.method_type()?
            } else {
                self.id_type()
            };
            // Parameters decay as in C: `id const []` is a pointer.
            let ty = self.decay(ty);
            let mut a = Attrs::default();
            self.attributes(&mut a)?;
            let (name, _, _) = self.ident("a parameter name")?;
            let mut b = Attrs::default();
            self.attributes(&mut b)?;
            params.push(Param {
                name,
                ty,
                consumed: a.consumed || b.consumed,
            });
            trailing.merge(&b);
            if self.at(",")? && self.second_is("...")? {
                self.bump()?;
                self.bump()?;
                variadic = true;
                break;
            }
        }
        if sel.is_empty() {
            let t = self.peek()?;
            return Err(self.err(
                t.loc,
                &format!("expected a selector, found {}", self.describe(t)),
            ));
        }
        let mut a = trailing;
        self.attributes(&mut a)?;
        if self.at("{")? {
            return Err(self.err(
                sign.loc,
                "a method body in a header: headers only declare, and the Objective-C front end \
                 reads declarations; move the body to a `.m` file",
            ));
        }
        self.expect(";")?;
        Ok(Method {
            instance,
            sel,
            ret,
            params,
            variadic,
            optional,
            avail: a.avail,
            retained: a.retained,
            consumes_self: a.consumes_self,
            assume_nonnull: self.objc.nonnull,
            loc: sign.loc,
        })
    }

    /// `@property (attrs) T a, *b attrs;` after `@property`.
    fn property(&mut self, optional: bool, out: &mut Vec<Property>) -> R<()> {
        let loc = self.peek()?.loc;
        let mut null = None;
        let (mut readonly, mut class) = (false, false);
        let (mut getter, mut setter) = (None, None);
        if self.eat("(")? {
            loop {
                if self.eat(")")? {
                    break;
                }
                if self.eat(",")? {
                    continue;
                }
                let (w, _, _) = self.ident("a property attribute")?;
                match w.as_str() {
                    "readonly" => readonly = true,
                    "readwrite" => readonly = false,
                    "class" => class = true,
                    "nullable" | "null_resettable" => null = Some(Null::Nullable),
                    "nonnull" => null = Some(Null::Nonnull),
                    "null_unspecified" => null = Some(Null::Unspecified),
                    "getter" => {
                        self.expect("=")?;
                        getter = Some(self.ident("a getter name")?.0);
                    }
                    "setter" => {
                        self.expect("=")?;
                        let (s, _, _) = self.ident("a setter name")?;
                        self.expect(":")?;
                        setter = Some(format!("{s}:"));
                    }
                    _ => {}
                }
            }
        }
        let specs = self.specs(false)?;
        let base = self.base_type(&specs)?;
        loop {
            let d = self.declarator()?;
            let mut attrs = specs.attrs.clone();
            attrs.merge(&d.attrs);
            self.attributes(&mut attrs)?;
            let ty = self.apply(base, &specs, &d.chunks, &attrs);
            let Some((name, _, _)) = d.name else {
                let t = self.peek()?;
                return Err(self.err(t.loc, "expected a property name"));
            };
            out.push(Property {
                name,
                ty,
                null,
                readonly,
                class,
                getter: getter.clone(),
                setter: setter.clone(),
                optional,
                avail: attrs.avail,
                assume_nonnull: self.objc.nonnull,
                loc,
            });
            if !self.eat(",")? {
                break;
            }
        }
        self.expect(";")?;
        Ok(())
    }
}

/// `"10.15.4"` (or `10_15_4`) as `(10, 15, 4)`.
fn version(s: &str) -> (u32, u32, u32) {
    let mut it = s
        .split(['.', '_'])
        .map(|p| p.trim().parse::<u32>().unwrap_or(0));
    (
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
        it.next().unwrap_or(0),
    )
}
