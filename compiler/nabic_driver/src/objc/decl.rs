//! Reading `extern "objc"` blocks: the class, its methods and their
//! selectors, checked.

use nabic_ast::{
    AttrArg, AttrBody, Attribute, ExternBlock, ExternItem, FnDef, Literal, ThrowsKind, Ty, TyKind,
};
use nabic_diagnostic::{Code, NabiDiagnostic, Span};

/// What a block declares.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `#[objc::class(C)]`: the class and its type.
    Class,
    /// `#[objc::protocol(P)]`: a type for objects of unnamed classes that
    /// conform to `P`.
    Protocol,
    /// `#[objc::category(C)]`: methods of a class declared elsewhere.
    Category,
}

/// One method of a block.
pub(crate) struct Method {
    pub(crate) def: FnDef,
    pub(crate) sel: String,
    pub(crate) instance: bool,
    /// The result is owned (`+1`): the `alloc`, `new`, `copy`,
    /// `mutableCopy` and `init` families, or `#[returns_retained]`.
    pub(crate) retained: bool,
    /// Its own `throws (objc::Error)`: an `NSError **` last.
    pub(crate) throws: bool,
    /// ARC's method family of the selector (`""`: none).
    pub(crate) family: &'static str,
}

impl Method {
    /// `init`: the receiver's retain goes to the result.
    pub(crate) fn consumes_self(&self) -> bool {
        self.instance && self.family == "init"
    }
}

/// One `extern "objc"` block, read.
pub(crate) struct Decl {
    pub(crate) module: String,
    pub(crate) class: String,
    pub(crate) kind: Kind,
    pub(crate) sup: Option<String>,
    /// `conforms(P, Q)`: the protocols a class adopts, or more parents
    /// of a protocol.
    pub(crate) conforms: Vec<String>,
    pub(crate) links: Vec<Attribute>,
    pub(crate) methods: Vec<Method>,
    pub(crate) span: Span,
}

/// How a parameter or result crosses.
pub(crate) enum Cross {
    /// `Id<T>` (`nullable`: `Id<T> | none`); `any`: `Id<NSObject>`, which
    /// takes any object.
    Obj { nullable: bool, any: bool },
    /// A closure, as a block.
    Block(Vec<Ty>, Option<Ty>),
    /// As is, under the C ABI.
    Plain,
}

pub(crate) fn err(diags: &mut Vec<NabiDiagnostic>, span: Span, msg: String) {
    diags.push(NabiDiagnostic::error(Code::FFI, span, msg));
}

/// `T` of `Id<T>`.
pub(crate) fn id_arg(t: &Ty) -> Option<&Ty> {
    match &t.kind {
        TyKind::Path(p) => {
            let last = p.segments.last()?;
            (last.name == "Id" && last.generics.len() == 1).then(|| &last.generics[0])
        }
        _ => None,
    }
}

/// `T` of `Id<T>` or `Id<T> | none`.
pub(crate) fn obj_arg(t: &Ty) -> Option<&Ty> {
    match &t.kind {
        TyKind::Sum(members) => members.iter().find_map(id_arg),
        _ => id_arg(t),
    }
}

pub(crate) fn last_name(t: &Ty) -> Option<&str> {
    match &t.kind {
        TyKind::Path(p) => p.segments.last().map(|s| s.name.as_str()),
        _ => None,
    }
}

pub(crate) fn cross(t: &Ty) -> Cross {
    let any = |x: &Ty| id_arg(x).and_then(last_name) == Some("NSObject");
    if id_arg(t).is_some() {
        return Cross::Obj {
            nullable: false,
            any: any(t),
        };
    }
    match &t.kind {
        TyKind::Sum(members) if members.len() == 2 => {
            let none = |x: &Ty| matches!(x.kind, TyKind::None);
            let obj = members.iter().find(|x| !none(x));
            match obj {
                Some(o) if id_arg(o).is_some() && members.iter().any(none) => Cross::Obj {
                    nullable: true,
                    any: any(o),
                },
                _ => Cross::Plain,
            }
        }
        TyKind::Fn(params, ret, _) => Cross::Block(
            params.clone(),
            (!matches!(&ret.kind, TyKind::Tuple(xs) if xs.is_empty())).then(|| (**ret).clone()),
        ),
        _ => Cross::Plain,
    }
}

pub(crate) fn raw_pointer(t: &Ty) -> bool {
    matches!(last_name(t), Some("Ptr" | "ConstPtr" | "CPtr" | "ManyPtr"))
}

/// ARC's method family of `sel`: `alloc`, `copy`, ..; `""` for none.
pub(crate) fn family(sel: &str) -> &'static str {
    let s = sel.trim_start_matches('_');
    for f in ["alloc", "copy", "mutableCopy", "new", "init"] {
        if let Some(rest) = s.strip_prefix(f) {
            if !rest.starts_with(|c: char| c.is_ascii_lowercase()) {
                return f;
            }
        }
    }
    ""
}

fn has_attr(f: &FnDef, name: &str) -> bool {
    f.attrs.iter().any(|a| a.path == [name])
}

/// What `#[objc::class(..)]` names.
pub(crate) struct ClassAttr {
    pub(crate) name: String,
    /// `extends(S)`.
    pub(crate) sup: Option<String>,
    /// `conforms(P, Q)`.
    pub(crate) conforms: Vec<String>,
}

/// The names of `#[objc::class(C, extends(S), conforms(P, Q))]`.
pub(crate) fn parse_class_attr(a: &Attribute) -> Option<ClassAttr> {
    let AttrBody::Args(args) = &a.body else {
        return None;
    };
    let AttrArg::Word(name) = args.first()? else {
        return None;
    };
    let words = |body: &AttrBody| match body {
        AttrBody::Args(xs) => xs
            .iter()
            .map(|x| match x {
                AttrArg::Word(w) => Some(w.clone()),
                _ => None,
            })
            .collect::<Option<Vec<String>>>(),
        _ => None,
    };
    let mut c = ClassAttr {
        name: name.clone(),
        sup: None,
        conforms: Vec::new(),
    };
    for arg in &args[1..] {
        match arg {
            AttrArg::Nested { path, body } if path == &["extends"] && c.sup.is_none() => {
                match words(body)?.as_slice() {
                    [w] => c.sup = Some(w.clone()),
                    _ => return None,
                }
            }
            AttrArg::Nested { path, body } if path == &["conforms"] && c.conforms.is_empty() => {
                c.conforms = words(body)?;
                if c.conforms.is_empty() {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(c)
}

/// The selector of `f`, which passes `pieces` arguments.
fn selector(
    f: &FnDef,
    pieces: usize,
    throws: bool,
    diags: &mut Vec<NabiDiagnostic>,
) -> Option<String> {
    if let Some(a) = f.attrs.iter().find(|a| a.path == ["selector"]) {
        let given = match &a.body {
            AttrBody::Args(xs) => match xs.as_slice() {
                [AttrArg::Lit(Literal::Str(s))] => Some(s.clone()),
                _ => None,
            },
            _ => None,
        };
        // `attrs` reported a malformed one.
        let s = given?;
        let n = s.matches(':').count();
        if n != pieces {
            let error = if throws {
                " (the `NSError **` included)"
            } else {
                ""
            };
            err(
                diags,
                f.span,
                format!(
                    "`#[selector(\"{s}\")]` has {n} argument(s), but `{}` passes {pieces}{error}",
                    f.name
                ),
            );
            return None;
        }
        return Some(s);
    }
    if pieces == 0 {
        return Some(f.name.clone());
    }
    let parts: Vec<&str> = f.name.split('_').collect();
    if parts.len() != pieces || parts.iter().any(|p| p.is_empty()) {
        let have = if throws {
            format!("{} parameter(s) and the `NSError **`", pieces - 1)
        } else {
            format!("{pieces} parameter(s)")
        };
        err(
            diags,
            f.span,
            format!(
                "`{}` has {have}, so its selector has {pieces} part(s): name it after the \
                 selector with `_` between the parts (`initWithBytes_length` for \
                 `initWithBytes:length:`), or give it with `#[selector(\"..\")]`",
                f.name
            ),
        );
        return None;
    }
    Some(parts.iter().fold(String::new(), |mut s, p| {
        s.push_str(p);
        s.push(':');
        s
    }))
}

/// Whether `f` throws, checked: `throws (objc::Error)` after nothing or
/// an object.
fn throws(f: &FnDef, diags: &mut Vec<NabiDiagnostic>) -> Option<bool> {
    let throws = match &f.throws {
        ThrowsKind::None => false,
        ThrowsKind::Typed(ts) if ts.len() == 1 && last_name(&ts[0]) == Some("Error") => true,
        _ => {
            err(
                diags,
                f.span,
                format!(
                    "`{}`: an Objective-C method reports errors through its last `NSError **`: \
                     write `throws (objc::Error)` and leave that parameter out",
                    f.name
                ),
            );
            return None;
        }
    };
    if throws
        && !f
            .ret_ty
            .as_ref()
            .is_none_or(|t| matches!(cross(t), Cross::Obj { .. }))
    {
        err(
            diags,
            f.span,
            format!(
                "`{}`: a throwing method returns nothing (a `BOOL` result) or an `Id<T>` \
                 (nil on failure): the error is the `throws`",
                f.name
            ),
        );
        return None;
    }
    Some(throws)
}

/// The method `f` of class `class` declares, checked.
fn method(f: FnDef, class: &str, diags: &mut Vec<NabiDiagnostic>) -> Option<Method> {
    let instance = f.params.first().is_some_and(|p| p.name == "self");
    let problem = if instance && f.params[0].by_ref {
        Some(
            "an Objective-C method takes `self`, not `ref self`: the receiver is an object, shared already",
        )
    } else if !f.generics.params.is_empty() || f.is_async || f.is_generator {
        Some("an `extern \"objc\"` method is a plain `fn`: no generics, `async` or generators")
    } else {
        None
    };
    if let Some(p) = problem {
        err(diags, f.span, format!("`{}`: {p}", f.name));
        return None;
    }
    let args: Vec<_> = f.params.iter().skip(usize::from(instance)).collect();
    if let Some(p) = args.iter().find(|p| p.variadic || p.default.is_some()) {
        err(
            diags,
            p.span,
            format!(
                "`{}`: variadic Objective-C methods and default arguments are not supported; \
                 declare one method per argument list",
                f.name
            ),
        );
        return None;
    }
    if let Some(p) = args.iter().find(|p| p.ty.is_none()) {
        err(
            diags,
            p.span,
            format!("`{}`: parameter `{}` needs a type", f.name, p.name),
        );
        return None;
    }
    let throws = throws(&f, diags)?;
    let sel = selector(&f, args.len() + usize::from(throws), throws, diags)?;
    let family = family(&sel);
    if !instance && family == "init" {
        err(
            diags,
            f.span,
            format!(
                "`{}` is an `init` method: it takes `self`, the object `{class}::alloc()` returns",
                f.name
            ),
        );
        return None;
    }
    let retained = (has_attr(&f, "returns_retained") || !family.is_empty())
        && !has_attr(&f, "returns_not_retained");
    Some(Method {
        def: f,
        sel,
        instance,
        retained,
        throws,
        family,
    })
}

/// Read one block; `None` (reported) when it is malformed.
pub(crate) fn decl(
    module: String,
    b: ExternBlock,
    diags: &mut Vec<NabiDiagnostic>,
) -> Option<Decl> {
    let mut named = None;
    for a in &b.attrs {
        let kind = match a.path.as_slice() {
            [o, k] if o == "objc" && k == "class" => Kind::Class,
            [o, k] if o == "objc" && k == "protocol" => Kind::Protocol,
            [o, k] if o == "objc" && k == "category" => Kind::Category,
            _ => continue,
        };
        if named.is_some() {
            err(
                diags,
                a.span,
                "an `extern \"objc\"` block is for one class: write one block per class".to_owned(),
            );
            return None;
        }
        // `attrs` reported a malformed one.
        named = Some((parse_class_attr(a)?, kind));
    }
    let Some((
        ClassAttr {
            name: class,
            sup,
            conforms,
        },
        kind,
    )) = named
    else {
        err(
            diags,
            b.span,
            "an `extern \"objc\"` block names its class: `#[objc::class(NSArray)]` declares one, \
             `#[objc::category(NSString)]` adds methods to one declared elsewhere"
                .to_owned(),
        );
        return None;
    };
    let mut methods = Vec::new();
    for item in b.items {
        match item {
            ExternItem::Fn(f)
                if kind == Kind::Protocol && f.params.first().is_none_or(|p| p.name != "self") =>
            {
                err(
                    diags,
                    f.span,
                    format!(
                        "`{}`: protocol `{class}` has no class object to send a class method to; \
                         declare it on a class that conforms",
                        f.name
                    ),
                );
            }
            ExternItem::Fn(f) => methods.extend(method(f, &class, diags)),
            ExternItem::Static(s) => err(
                diags,
                s.span,
                format!(
                    "`static {}` in an `extern \"objc\"` block: it holds methods; declare a C \
                     global in an `extern \"C\"` block",
                    s.name
                ),
            ),
        }
    }
    let links = b.attrs.into_iter().filter(|a| a.path == ["link"]).collect();
    let sup = match kind {
        Kind::Class => Some(sup.unwrap_or_else(|| "NSObject".to_owned())),
        Kind::Protocol => sup,
        Kind::Category => None,
    };
    Some(Decl {
        module,
        class,
        kind,
        sup,
        conforms,
        links,
        methods,
        span: b.span,
    })
}
