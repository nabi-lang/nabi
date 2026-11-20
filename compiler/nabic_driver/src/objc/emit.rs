//! Writing the bindings of `extern "objc"` blocks as Nabi source: per
//! module, the `objc_msgSend` declarations, block trampolines and the
//! impls of the methods.

use crate::objc::decl::{Cross, Decl, Kind, Method, cross, err, last_name, obj_arg, raw_pointer};
use crate::objc::{OBJC, RAW};
use nabic_ast::{AttrBody, Attribute, Ty, TyKind};
use nabic_core::Visibility;
use nabic_diagnostic::{NabiDiagnostic, Span};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// `t` as source, `Self` written as `class`.
pub(crate) fn ty_text(t: &Ty, class: &str) -> String {
    let s = nabic_ast::print::print_ty(t);
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut i = 0;
    while i < bytes.len() {
        if s[i..].starts_with("Self")
            && (i == 0 || !ident(bytes[i - 1]))
            && bytes.get(i + 4).is_none_or(|&c| !ident(c))
        {
            out.push_str(class);
            i += 4;
        } else {
            out.push(char::from(bytes[i]));
            i += 1;
        }
    }
    out
}

/// FNV-1a of `s`, hex: names that differ by signature.
pub(crate) fn hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

const fn vis(v: Visibility) -> &'static str {
    match v {
        Visibility::Pub => "pub ",
        Visibility::Pkg => "pub(pkg) ",
        Visibility::Sub => "pub(sub) ",
        Visibility::Private => "",
    }
}

/// The type encoding of `t` in a block signature.
fn encoding(t: Option<&Ty>) -> &'static str {
    let Some(t) = t else { return "v" };
    if matches!(cross(t), Cross::Obj { .. }) {
        return "@";
    }
    if raw_pointer(t) {
        return "^v";
    }
    match last_name(t) {
        Some("bool") => "B",
        Some("i8" | "c_char") => "c",
        Some("u8") => "C",
        Some("i16") => "s",
        Some("u16") => "S",
        Some("i32") => "i",
        Some("u32") => "I",
        Some("i64" | "isize") => "q",
        Some("u64" | "usize") => "Q",
        Some("f32") => "f",
        Some("f64") => "d",
        Some("Sel") => ":",
        Some("Class") => "#",
        Some("str") => "*",
        _ => "?",
    }
}

/// `, a0: A, a1: B` of the types `raw`.
fn numbered(raw: &[String]) -> String {
    raw.iter().enumerate().fold(String::new(), |mut s, (i, t)| {
        let _ = write!(s, ", a{i}: {t}");
        s
    })
}

fn arrow(ret: Option<&str>) -> String {
    ret.map(|r| format!(" -> {r}")).unwrap_or_default()
}

/// The source of one module's bindings.
#[derive(Default)]
pub(crate) struct Out {
    /// `extern "C"` declarations, once each by name.
    externs: BTreeMap<String, String>,
    links: Vec<String>,
    items: String,
    /// Block trampolines and descriptors, outside the impls.
    glue: String,
    /// Method traits made per class, for their names.
    traits: BTreeMap<String, usize>,
}

impl Out {
    fn decl(&mut self, name: &str, text: String) {
        self.externs.entry(name.to_owned()).or_insert(text);
    }

    /// The `objc_msgSend` of C signature `params -> ret` in `module`.
    fn send(&mut self, module: &str, params: &[String], ret: Option<&str>) -> String {
        let key = format!("{module}|{}|{}", params.join(","), ret.unwrap_or(""));
        let name = format!("__nabi_objc_send_{}", hash(&key));
        let text = format!(
            "    #[link_name = \"objc_msgSend\"]\n    fn {name}(r: {RAW}, s: {OBJC}::Sel{}){};\n",
            numbered(params),
            arrow(ret)
        );
        self.decl(&name, text);
        name
    }

    /// `d`'s `#[link]` attributes, once each.
    pub(crate) fn links_of(&mut self, d: &Decl) {
        for l in &d.links {
            let t = link_text(l);
            if !self.links.contains(&t) {
                self.links.push(t);
            }
        }
    }

    /// The module's bindings.
    pub(crate) fn source(mut self) -> String {
        self.decl(
            "__nabi_objc_class",
            format!(
                "    #[link_name = \"objc_getRequiredClass\"]\n    \
                 fn __nabi_objc_class(name: str) -> {OBJC}::Class;\n"
            ),
        );
        self.decl(
            "__nabi_objc_sel",
            format!(
                "    #[link_name = \"sel_registerName\"]\n    \
                 fn __nabi_objc_sel(name: str) -> {OBJC}::Sel;\n"
            ),
        );
        let mut s = String::new();
        for l in &self.links {
            s.push_str(l);
            s.push('\n');
        }
        s.push_str("extern \"C\" {\n");
        for d in self.externs.values() {
            s.push_str(d);
        }
        s.push_str("}\n");
        s.push_str(&self.glue);
        s.push_str(&self.items);
        s
    }

    /// The type of class (or protocol) `d` declares: the marker struct,
    /// its `Object` impl and `as_super`. `sup` is the superclass as a
    /// path from `d`'s module.
    /// `main`: its objects are released on the main thread.
    pub(crate) fn class(&mut self, d: &Decl, sup: Option<&str>, isa: &[String], main: bool) {
        let c = &d.class;
        let (head, class_fn) = if d.kind == Kind::Protocol {
            (
                d.sup.as_ref().map_or_else(
                    || format!("@protocol {c}"),
                    |s| format!("@protocol {c} <{s}>"),
                ),
                format!(
                    "panic(\"`{c}` is a protocol: it has no class object; its objects come \
                     from the methods that return them\")"
                ),
            )
        } else {
            (
                format!(
                    "@interface {c} : {}",
                    d.sup.as_deref().unwrap_or("NSObject")
                ),
                format!("unsafe {{ __nabi_objc_class(\"{c}\") }}"),
            )
        };
        let _ = write!(
            self.items,
            "/// `{head}`\npub struct {c} {{}}\n\
             impl {OBJC}::Object for {c} {{\n    \
             fn name() -> str {{ \"{c}\" }}\n    \
             fn class() -> {OBJC}::Class {{ {class_fn} }}\n{}}}\n",
            if main {
                "    fn main_thread() -> bool { true }\n"
            } else {
                ""
            }
        );
        for a in isa {
            let _ = writeln!(self.items, "impl {OBJC}::IsA<{a}> for {c} {{}}");
        }
        if let (Some(s), Some(sup)) = (&d.sup, sup) {
            let _ = write!(
                self.items,
                "impl {OBJC}::Id<{c}> {{\n    \
                 /// The object as `{s}`.\n    \
                 pub fn as_super(self) -> {OBJC}::Id<{sup}> {{ unsafe {{ self.cast::<{sup}>() }} }}\n}}\n"
            );
        }
    }

    /// `methods` (declared in `module`) as methods of class `class`, the
    /// type `ty` there, and of its subclasses: a trait of the instance
    /// methods for every `Id<T>` of a `T: IsA<ty>`, one of the class
    /// methods for every such `T`.
    pub(crate) fn methods(
        &mut self,
        module: &str,
        methods: &[&Method],
        class: &str,
        ty: &str,
        diags: &mut Vec<NabiDiagnostic>,
    ) {
        let n = self.traits.entry(class.to_owned()).or_insert(0);
        *n += 1;
        let base = if *n == 1 {
            format!("{class}Methods")
        } else {
            format!("{class}Methods{n}")
        };
        for instance in [true, false] {
            let ms: Vec<&&Method> = methods.iter().filter(|m| m.instance == instance).collect();
            if ms.is_empty() {
                continue;
            }
            let mut decls = String::new();
            let mut defs = String::new();
            let mut public = Visibility::Private;
            for m in ms {
                if let Some((sig, body)) = self.wrapper(module, m, class, ty, diags) {
                    // Availability reaches the call's check.
                    let pre = m
                        .def
                        .attrs
                        .iter()
                        .filter(|a| a.path == ["available"])
                        .filter_map(nabic_hir::marks::availability)
                        .fold(String::new(), |mut s, (os, v)| {
                            let _ = writeln!(s, "    #[available({os} = \"{v}\")]");
                            s
                        });
                    let _ = writeln!(decls, "{pre}    {sig};");
                    let _ = write!(defs, "{pre}    {sig} {{\n{body}    }}\n");
                    if m.def.vis == Visibility::Pub || public == Visibility::Private {
                        public = m.def.vis;
                    }
                }
            }
            if decls.is_empty() {
                continue;
            }
            let (name, on) = if instance {
                (base.clone(), format!("{OBJC}::Id<__T>"))
            } else {
                (base.replace("Methods", "ClassMethods"), "__T".to_owned())
            };
            let what = if instance { "instance" } else { "class" };
            let _ = write!(
                self.items,
                "/// The {what} methods of `{class}` and its subclasses.\n\
                 {}trait {name}<__T> {{\n{decls}}}\n\
                 impl<__T: {OBJC}::Object + {OBJC}::IsA<{ty}>> {name}<__T> for {on} {{\n{defs}}}\n",
                vis(public)
            );
        }
    }

    /// `to_str` for a subclass `ty` of `bare::ffi::objc`'s `NSString`.
    pub(crate) fn string_text(&mut self, ty: &str) {
        let _ = write!(
            self.items,
            "impl {OBJC}::Id<{ty}> {{\n    \
             /// The text as a `str`, copied.\n    \
             pub fn to_str(self) -> str {{ unsafe {{ self.cast::<{OBJC}::NSString>() }}.to_str() }}\n}}\n"
        );
    }

    /// A block's invoke trampoline and descriptor for closure type
    /// `fn(params) -> ret`: their names and the trampoline's C type.
    fn block_glue(
        &mut self,
        module: &str,
        params: &[Ty],
        ret: Option<&Ty>,
        class: &str,
        span: Span,
        diags: &mut Vec<NabiDiagnostic>,
    ) -> Option<(String, String, String)> {
        let mut raw = Vec::new();
        let mut args = Vec::new();
        for (i, p) in params.iter().enumerate() {
            match cross(p) {
                Cross::Obj { nullable, .. } => {
                    let t = ty_text(obj_arg(p).unwrap_or(p), class);
                    let take = format!("unsafe {{ {OBJC}::Id::<{t}>::retain(a{i}) }}");
                    raw.push(RAW.to_owned());
                    args.push(if nullable {
                        take
                    } else {
                        format!("{take} ?? {OBJC}::nil_result(\"a block argument\")")
                    });
                }
                Cross::Block(..) => {
                    err(
                        diags,
                        span,
                        "a block that takes a block is not supported; take that one as a raw \
                         pointer"
                            .to_owned(),
                    );
                    return None;
                }
                Cross::Plain => {
                    raw.push(ty_text(p, class));
                    args.push(format!("a{i}"));
                }
            }
        }
        let fn_ty = format!(
            "fn({}){}",
            params
                .iter()
                .map(|p| ty_text(p, class))
                .collect::<Vec<_>>()
                .join(", "),
            arrow(ret.map(|r| ty_text(r, class)).as_deref())
        );
        let call = format!("__f({})", args.join(", "));
        let (ret_raw, body) = match ret.map(|r| (r, cross(r))) {
            None => (None, format!("{call};")),
            Some((
                _,
                Cross::Obj {
                    nullable: false, ..
                },
            )) => (Some(RAW.to_owned()), format!("{call}.autoreleased_raw()")),
            Some((_, Cross::Obj { nullable: true, .. })) => {
                (Some(RAW.to_owned()), format!("{OBJC}::raw_or_nil({call})"))
            }
            Some((_, Cross::Block(..))) => {
                err(
                    diags,
                    span,
                    "a block that returns a block is not supported".to_owned(),
                );
                return None;
            }
            Some((r, Cross::Plain)) => (Some(ty_text(r, class)), call),
        };
        let h = hash(&format!("{module}|{fn_ty}"));
        let invoke = format!("__nabi_objc_invoke_{h}");
        let desc = format!("__NABI_OBJC_BLOCK_{h}");
        // Taken as a C function pointer: its C entry.
        let c_ty = format!(
            "extern \"C\" fn({RAW}{}){}",
            raw.iter().fold(String::new(), |mut s, t| {
                let _ = write!(s, ", {t}");
                s
            }),
            arrow(ret_raw.as_deref())
        );
        if self.externs.contains_key(&invoke) {
            return Some((invoke, desc, c_ty));
        }
        // Not an extern; the key keeps it to one per module.
        self.decl(&invoke, String::new());
        let sig: String = std::iter::once(encoding(ret))
            .chain(std::iter::once("@?"))
            .chain(params.iter().map(|p| encoding(Some(p))))
            .collect();
        let _ = write!(
            self.glue,
            "static {desc}: {OBJC}::block::Descriptor = {OBJC}::block::Descriptor::new(\"{sig}\");\n\
             extern \"C\" fn {invoke}(__blk: {RAW}{}){} {{\n    \
             let __f = unsafe {{ {OBJC}::block::closure::<{fn_ty}>(__blk) }};\n    {body}\n}}\n",
            numbered(&raw),
            arrow(ret_raw.as_deref())
        );
        Some((invoke, desc, c_ty))
    }

    /// The signature and body of method `m`'s wrapper for class `class`
    /// (Nabi path `ty`), in a trait over `__T: IsA<ty>`: `Self` is `__T`.
    fn wrapper(
        &mut self,
        module: &str,
        m: &Method,
        class: &str,
        ty: &str,
        diags: &mut Vec<NabiDiagnostic>,
    ) -> Option<(String, String)> {
        let mut w = self.params(module, m, ty, diags)?;
        let f = &m.def;
        let ret = f
            .ret_ty
            .as_ref()
            .filter(|t| !matches!(&t.kind, TyKind::Tuple(xs) if xs.is_empty()));
        let ret_cross = ret.map(cross);
        if m.throws {
            w.raw.push("bare::ptr::Ptr<bare::ptr::CPtr<u8>>".to_owned());
            w.pre.push_str(
                "    let __err: bare::ptr::CPtr<u8> = bare::ptr::CPtr::null();\n    \
                 let __errp = bare::ptr::addr_of(__err);\n",
            );
            w.args.push("__errp".to_owned());
        }
        let ret_raw = match &ret_cross {
            Some(Cross::Obj { .. }) => Some(RAW.to_owned()),
            Some(Cross::Block(..)) => {
                err(
                    diags,
                    f.span,
                    format!(
                        "`{}`: a block result is not supported; declare it as a raw pointer",
                        f.name
                    ),
                );
                return None;
            }
            Some(Cross::Plain) => ret.map(|r| ty_text(r, "__T")),
            None => m.throws.then(|| "bool".to_owned()),
        };
        let send = self.send(module, &w.raw, ret_raw.as_deref());
        let recv = if !m.instance {
            "__T::class().as_raw()".to_owned()
        } else if m.consumes_self() {
            "self.take_raw()".to_owned()
        } else {
            "self.as_raw()".to_owned()
        };
        let call = format!(
            "unsafe {{ {send}({recv}, __nabi_objc_sel(\"{}\"){}) }}",
            m.sel,
            w.args.iter().fold(String::new(), |mut s, a| {
                let _ = write!(s, ", {a}");
                s
            })
        );
        let mut body = w.pre;
        if ret_raw.is_none() {
            let _ = writeln!(body, "    {call};");
            body.push_str(&w.post);
        } else {
            let _ = writeln!(body, "    let __r = {call};");
            body.push_str(&w.post);
            let thrown = format!("throw unsafe {{ {OBJC}::Error::from_out(__err) }};");
            match &ret_cross {
                Some(Cross::Obj { nullable, .. }) => {
                    if m.throws {
                        let _ = writeln!(body, "    if __r.is_null() {{ {thrown} }}");
                    }
                    let inner = ret
                        .and_then(obj_arg)
                        .map(|t| ty_text(t, "__T"))
                        .unwrap_or_default();
                    let take = if m.retained {
                        "from_retained"
                    } else {
                        "from_autoreleased"
                    };
                    let conv = format!("unsafe {{ {OBJC}::Id::<{inner}>::{take}(__r) }}");
                    if *nullable {
                        let _ = writeln!(body, "    {conv}");
                    } else {
                        let what =
                            format!("{}[{class} {}]", if m.instance { '-' } else { '+' }, m.sel);
                        let _ = writeln!(body, "    {conv} ?? {OBJC}::nil_result(\"{what}\")");
                    }
                }
                None => {
                    let _ = writeln!(body, "    if !__r {{ {thrown} }}");
                }
                _ => body.push_str("    __r\n"),
            }
        }
        let tparams = if w.generics.is_empty() {
            String::new()
        } else {
            format!("<{}>", w.generics.join(", "))
        };
        let mut params = if m.instance {
            vec!["self".to_owned()]
        } else {
            Vec::new()
        };
        params.extend(w.params);
        let sig = format!(
            "{}fn {}{tparams}({}){}{}",
            if w.unsafe_fn { "unsafe " } else { "" },
            f.name,
            params.join(", "),
            if m.throws {
                format!(" throws ({OBJC}::Error)")
            } else {
                String::new()
            },
            arrow(ret.map(|r| ty_text(r, "__T")).as_deref()),
        );
        Some((sig, body))
    }

    /// The parameters of `m`'s wrapper and what passes them.
    fn params(
        &mut self,
        module: &str,
        m: &Method,
        ty: &str,
        diags: &mut Vec<NabiDiagnostic>,
    ) -> Option<Params> {
        let mut w = Params {
            unsafe_fn: m.def.is_unsafe,
            ..Params::default()
        };
        for (i, p) in m
            .def
            .params
            .iter()
            .skip(usize::from(m.instance))
            .enumerate()
        {
            let t = p.ty.as_ref()?;
            let name = &p.name;
            match cross(t) {
                Cross::Obj { nullable, any } => {
                    // With `none` a type parameter would stay open:
                    // `x.as_object()` there.
                    let shown = if any && !nullable {
                        w.generics.push(format!("__O{i}: {OBJC}::Object"));
                        format!("{OBJC}::Id<__O{i}>")
                    } else if let (false, Some(c)) = (nullable, obj_arg(t)) {
                        // Any subclass's object: `IsA`.
                        let c = ty_text(c, "__T");
                        w.generics
                            .push(format!("__O{i}: {OBJC}::Object + {OBJC}::IsA<{c}>"));
                        format!("{OBJC}::Id<__O{i}>")
                    } else {
                        ty_text(t, "__T")
                    };
                    w.params.push(format!("{name}: {shown}"));
                    w.raw.push(RAW.to_owned());
                    w.args.push(if nullable {
                        format!("{OBJC}::raw_or_nil({name})")
                    } else {
                        format!("{name}.as_raw()")
                    });
                }
                Cross::Block(ps, r) => {
                    let (invoke, desc, c_ty) =
                        self.block_glue(module, &ps, r.as_ref(), ty, p.span, diags)?;
                    w.params.push(format!("{name}: {}", ty_text(t, "__T")));
                    w.raw.push(RAW.to_owned());
                    let _ = write!(
                        w.pre,
                        "    let __c{i}: {c_ty} = {invoke};\n    \
                         let __i{i}: usize = unsafe {{ bare::mem::transmute_unchecked(__c{i}) }};\n    \
                         let __b{i} = {OBJC}::block::new({name}, __i{i}, bare::ptr::addr_of({desc}));\n"
                    );
                    let _ = writeln!(w.post, "    {OBJC}::block::release(__b{i});");
                    w.args.push(format!("__b{i}"));
                }
                Cross::Plain => {
                    w.unsafe_fn |= raw_pointer(t);
                    let shown = ty_text(t, "__T");
                    w.params.push(format!("{name}: {shown}"));
                    w.raw.push(shown);
                    w.args.push(name.clone());
                }
            }
        }
        Some(w)
    }
}

/// A wrapper's parameters: as declared, as C passes them, and the code
/// before and after the send.
#[derive(Default)]
struct Params {
    generics: Vec<String>,
    params: Vec<String>,
    raw: Vec<String>,
    args: Vec<String>,
    pre: String,
    post: String,
    unsafe_fn: bool,
}

fn link_text(a: &Attribute) -> String {
    let AttrBody::Args(xs) = &a.body else {
        return String::new();
    };
    let args: Vec<String> = xs.iter().map(nabic_ast::print::print_attr_arg).collect();
    format!("#[link({})]", args.join(", "))
}

/// The path to `class`, declared in module `at`, from `module`:
/// `pkg::`-relative within one package of `packages`, `None` across
/// packages.
pub(crate) fn path_to(packages: &[String], module: &str, at: &str, class: &str) -> Option<String> {
    if module == at {
        return Some(class.to_owned());
    }
    let pkg_of = |m: &str| {
        packages
            .iter()
            .filter(|p| !p.is_empty() && (m == p.as_str() || m.starts_with(&format!("{p}::"))))
            .max_by_key(|p| p.len())
            .cloned()
            .unwrap_or_default()
    };
    let p = pkg_of(at);
    if pkg_of(module) != p {
        return None;
    }
    let rel = if p.is_empty() {
        at
    } else {
        at.strip_prefix(p.as_str())?.trim_start_matches("::")
    };
    Some(if rel.is_empty() {
        format!("pkg::{class}")
    } else {
        format!("pkg::{rel}::{class}")
    })
}
