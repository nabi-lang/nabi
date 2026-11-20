//! Extern functions, C types and link names.

#[allow(clippy::wildcard_imports)]
use crate::lower::collect::*;

/// An `extern "C" fn` definition is called from C: no
/// captures, generics or `throws` (errors can't cross into C), FFI types
/// only.
pub(crate) fn check_extern_fn(
    f: &FnDef,
    abi: &str,
    diags: &mut Vec<nabic_diagnostic::NabiDiagnostic>,
) {
    let mut bad = |msg: String| {
        diags.push(nabic_diagnostic::NabiDiagnostic::error(
            Code::FFI,
            f.span,
            format!("`extern {abi:?} fn {}`: {msg}", leaf_name(&f.name)),
        ));
    };
    if abi != "C" && abi != "system" {
        bad("unsupported ABI (`\"C\"` or `\"system\"`)".to_owned());
    }
    if !matches!(f.throws, nabic_ast::ThrowsKind::None) {
        bad("cannot `throws` - an error cannot cross into C".to_owned());
    }
    if !f.generics.params.is_empty() {
        bad("cannot be generic".to_owned());
    }
    if f.is_async || f.is_generator {
        bad("cannot be `async` / a generator".to_owned());
    }
}

/// What of `ty` C cannot hold, a GC reference the collector
/// may move or free (`List<i64>`, a closure), else `None`. `str` is a
/// `char*` in an extern declaration; a bare `str` is checked elsewhere.
/// A pointer or C function pointer `| none` is a nullable C pointer.
pub(crate) fn gc_part(env: &Env<'_>, ty: &HirTy, depth: u32) -> Option<HirTy> {
    if depth > 8 {
        return None;
    }
    match ty {
        HirTy::Prim(PrimTy::Str) => (depth > 0).then(|| ty.clone()),
        HirTy::Sum(parts) => match parts.as_slice() {
            [m, HirTy::None] | [HirTy::None, m] if null_niche(env, m) => None,
            _ => Some(ty.clone()),
        },
        HirTy::GcBuf(_) | HirTy::Any | HirTy::AnyTrait(_) | HirTy::Closure { .. } => {
            Some(ty.clone())
        }
        HirTy::Fn(..) if !ty.is_c_fn() => Some(ty.clone()),
        HirTy::Tuple(ts) => ts.iter().find_map(|t| gc_part(env, t, depth + 1)),
        HirTy::Array(t, _) => gc_part(env, t, depth + 1),
        HirTy::Struct(id) => {
            let def = env.defs.struct_at(*id);
            def.fields
                .iter()
                .any(|f| gc_part(env, &f.ty, depth + 1).is_some())
                .then(|| ty.clone())
        }
        _ => None,
    }
}

/// `m | none` has C's pointer layout, `none` being null:
/// `m` is a C function pointer or a `#[niche(0)]` pointer struct.
pub(crate) fn null_niche(env: &Env<'_>, m: &HirTy) -> bool {
    match m {
        HirTy::Fn(..) => m.is_c_fn(),
        HirTy::Struct(id) => {
            let def = env.defs.struct_at(*id);
            !def.niche.is_empty()
                && def.fields.len() == 1
                && gc_part(env, &def.fields[0].ty, 1).is_none()
        }
        _ => false,
    }
}

/// `nabi_*` (the runtime's ABI, which takes GC references) and `__nabi_*`
/// (the REPL's cells): the toolchain's own symbols.
pub(crate) fn toolchain_symbol(name: &str) -> bool {
    let n = leaf_name(name);
    n.starts_with("nabi_") || n.starts_with("__nabi_")
}

/// Where a type meets C.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CPos {
    /// A parameter of a C declaration: a `str` is a `char*`, a `fn(..)`
    /// a C function pointer.
    DeclParam,
    DeclResult,
    /// A parameter or the result of an `extern "C" fn` definition.
    Def,
    /// An extern `static`: memory, never a register.
    Static,
}

/// Why C cannot take `ty` at `pos` (`top`: the parameter, result or
/// static itself, not a field of it): the part at fault, whether it is
/// a GC reference, and what to write instead. `None`: C can.
fn c_refusal(
    env: &Env<'_>,
    ty: &HirTy,
    pos: CPos,
    top: bool,
    depth: u32,
) -> Option<(HirTy, bool, String)> {
    if depth > 8 {
        return None;
    }
    let refuse = |help: &str| Some((ty.clone(), false, help.to_owned()));
    let gc = || Some((ty.clone(), true, GC_HELP.to_owned()));
    let result = matches!(pos, CPos::DeclResult | CPos::Def);
    match ty {
        HirTy::Prim(PrimTy::Str) => match (pos, top) {
            (CPos::DeclParam, true) => None,
            (CPos::DeclResult, true) => refuse(
                "C keeps the bytes of a returned `char*`: declare the result \
                 `ConstPtr<c_char> | none` and copy the string with \
                 `unsafe { CStr::from_ptr(p) }` (`bare::ffi`)",
            ),
            (CPos::Def, true) => refuse(
                "C passes a `char*`: take `s: ConstPtr<c_char>` and copy it with \
                 `unsafe { CStr::from_ptr(s) }` (`bare::ffi`); return a `ConstPtr<c_char>` to \
                 bytes that outlive the call",
            ),
            _ => gc(),
        },
        HirTy::Prim(PrimTy::Char) => {
            refuse("C has no `char` of four bytes: use `u32` and `char::from_u32(x)`")
        }
        HirTy::Unit if top && result => None,
        HirTy::Unit => refuse("C has no empty type: leave the parameter out"),
        HirTy::Struct(id) => {
            let def = env.defs.struct_at(*id);
            match def.repr.c_kind {
                crate::CKind::F16 | crate::CKind::BF16 => return None,
                crate::CKind::Simd if top || pos == CPos::Static => return None,
                crate::CKind::Simd => {
                    return refuse(
                        "a struct holding a `Simd` cannot cross by value: pass a pointer to it",
                    );
                }
                crate::CKind::Mask => {
                    return refuse(
                        "a `Mask` has no C type: pass a `Simd` of integers \
                         (`mask.select(Simd::splat(1), Simd::splat(0))`)",
                    );
                }
                crate::CKind::Plain => {}
            }
            let fields: Vec<HirTy> = match &def.union_kind {
                Some(u) => u.aliases.iter().map(|(_, t)| t.clone()).collect(),
                None => def.fields.iter().map(|f| f.ty.clone()).collect(),
            };
            let part = fields
                .iter()
                .find_map(|t| gc_part(env, t, depth + 1))
                .filter(|p| has_gc(env, p));
            if let Some(part) = part {
                // A library collection (`List`) is named, not its buffer.
                let shown = if matches!(part, HirTy::Struct(_)) {
                    part
                } else {
                    ty.clone()
                };
                return Some((shown, true, GC_HELP.to_owned()));
            }
            fields
                .iter()
                .find_map(|t| c_refusal(env, t, pos, false, depth + 1))
        }
        HirTy::Enum(id) => {
            let def = env.defs.enum_at(*id);
            if def.variants.iter().any(|v| v.payload_ty.is_some()) {
                refuse("C has no enum with data: pass a struct of a tag and a `union`")
            } else if def.repr.int.is_none() {
                refuse(
                    "the layout of a plain enum is the compiler's: add `#[repr(i32)]` (a C \
                     `enum` is an `int`), or take an `i32` and match it",
                )
            } else {
                None
            }
        }
        HirTy::Sum(parts) => match parts.as_slice() {
            [m, HirTy::None] | [HirTy::None, m] if null_niche(env, m) => None,
            _ if has_gc(env, ty) => gc(),
            _ => refuse(
                "only a pointer `| none` has a C form (null): pass a `ConstPtr<T> | none`, or the \
                 value and a flag",
            ),
        },
        HirTy::Tuple(_) => refuse("C has no tuples: declare a `struct`"),
        HirTy::Array(..) if top && pos != CPos::Static => refuse(
            "C passes an array as a pointer: take a `ConstPtr<T>` (`addr_of(xs)`), or put the \
             array in a struct",
        ),
        HirTy::Array(t, _) => c_refusal(env, t, pos, false, depth + 1),
        HirTy::Fn(..) if ty.is_c_fn() => None,
        // A declaration's `fn(..)` parameter is an `extern "C" fn`.
        HirTy::Fn(..) if top && pos == CPos::DeclParam => None,
        HirTy::Fn(..) => refuse(
            "a nabi `fn` value is a function and its captures: write `extern \"C\" fn(..)`, \
             made from a named `extern \"C\" fn`",
        ),
        HirTy::GcBuf(_) | HirTy::Any | HirTy::AnyTrait(_) | HirTy::Closure { .. } => gc(),
        _ => None,
    }
}

const GC_HELP: &str = "help: use a primitive, a `CPtr<T>` (`bare::ptr`) and a length, or a \
                       `#[repr(C)]` struct of those; for a `List<T>`, pin it with \
                       `let p = gc::pin(xs);` and pass `p.as_data_ptr()` and `xs.len()`";

/// `ty` reaches a GC reference (the collector may move or free it).
fn has_gc(env: &Env<'_>, ty: &HirTy) -> bool {
    match ty {
        HirTy::Sum(parts) => parts.iter().any(|p| has_gc(env, p)),
        HirTy::None | HirTy::Prim(_) | HirTy::Unit => false,
        t => gc_part(env, t, 1).is_some(),
    }
}

/// The types of the package's `extern "C"` blocks and `extern "C" fn`
/// definitions: FFI types only, checked as resolved (through aliases
/// and fields).
pub(crate) fn check_extern_ffi(env: &Env<'_>, programs: &[&Program]) {
    let no_subst = HashMap::default();
    let report = |what: String, bad: (HirTy, bool, String), at: Span| {
        let cx = LowerCtx::new(env, &no_subst, None, at);
        let (ty, gc, help) = bad;
        let shown = cx.ty_name(&ty);
        let why = if gc {
            ": the collector may move or free it"
        } else {
            ""
        };
        let help = match (
            gc,
            shown
                .strip_prefix("List<")
                .and_then(|t| t.strip_suffix('>')),
        ) {
            (true, Some(elem)) => format!(
                "help: C takes a list as a pointer and a length: declare `ptr: ConstPtr<{elem}>, \
                 len: usize`, and pass `p.as_data_ptr()` and `xs.len()` from \
                 `let p = gc::pin(xs);` (`bare::gc`)"
            ),
            (true, None) if shown == "str" => "help: C takes text as a `char*`: declare \
                 `ConstPtr<c_char>` and pass `CString::new(s)?.as_ptr()` (`bare::ffi`)"
                .to_owned(),
            (true, None) => help,
            (false, _) => format!("help: {help}"),
        };
        env.sink.diagnostics.borrow_mut().push(
            nabic_diagnostic::NabiDiagnostic::error(
                Code::FFI,
                at,
                format!("{what} has type `{shown}`, which C cannot hold{why}"),
            )
            .with_note(help),
        );
    };
    let fn_types = |f: &FnDef, kind: &str, decl: bool| {
        if toolchain_symbol(&f.name) {
            return;
        }
        let Some(sig) = env.fn_table.get(&f.name) else {
            return;
        };
        let name = leaf_name(&f.name);
        let (param, result) = if decl {
            (CPos::DeclParam, CPos::DeclResult)
        } else {
            (CPos::Def, CPos::Def)
        };
        for (p, ty) in f.params.iter().zip(&sig.param_tys) {
            if let Some(bad) = c_refusal(env, ty, param, true, 0) {
                let at = p.ty.as_ref().map_or(p.span, |t| t.span);
                report(
                    format!("parameter `{}` of {kind} `{name}`", p.name),
                    bad,
                    at,
                );
            }
        }
        if let Some(bad) = c_refusal(env, &sig.ret_ty, result, true, 0) {
            let at = f.ret_ty.as_ref().map_or(f.span, |t| t.span);
            report(format!("the result of {kind} `{name}`"), bad, at);
        }
    };
    for p in programs {
        for item in &p.items {
            match item {
                Item::Extern(blk) if blk.abi != "nabi" => {
                    for ei in &blk.items {
                        match ei {
                            nabic_ast::ExternItem::Fn(f) => fn_types(f, "extern function", true),
                            nabic_ast::ExternItem::Static(s) if !toolchain_symbol(&s.name) => {
                                let bad = env
                                    .statics
                                    .get(&s.name)
                                    .and_then(|t| c_refusal(env, t, CPos::Static, true, 0));
                                if let Some(bad) = bad {
                                    let name = leaf_name(&s.name);
                                    report(format!("extern static `{name}`"), bad, s.ty.span);
                                }
                            }
                            nabic_ast::ExternItem::Static(_) => {}
                        }
                    }
                }
                Item::Fn(f) if nabic_ast::extern_fn_abi(&f.attrs).is_some() => {
                    fn_types(f, "`extern \"C\" fn`", false);
                }
                _ => {}
            }
        }
    }
}

/// The `#[on_panic]` values of the `extern "C" fn`s in `programs`, checked
/// against their resolved result types.
pub(crate) fn on_panic_values(
    env: &Env<'_>,
    programs: &[&Program],
) -> HashMap<FunctionId, ConstVal> {
    let mut out = HashMap::default();
    for p in programs {
        for item in &p.items {
            let Item::Fn(f) = item else { continue };
            if nabic_ast::extern_fn_abi(&f.attrs).is_none() {
                continue;
            }
            let Some(sig) = env.fn_table.get(&f.name) else {
                continue;
            };
            match on_panic_value(f, &sig.ret_ty) {
                Some(Ok(v)) => {
                    out.insert(sig.id, v);
                }
                Some(Err(msg)) => {
                    env.sink.diagnostics.borrow_mut().push(
                        nabic_diagnostic::NabiDiagnostic::error(
                            Code::INVALID_ATTRIBUTE,
                            f.span,
                            msg,
                        ),
                    );
                }
                None => {}
            }
        }
    }
    out
}

/// `#[link(wasm_import_module = "m")]` on an `extern` block:
/// the core module its functions are imported from on wasm32.
pub(crate) fn wasm_import_module(attrs: &[nabic_ast::Attribute]) -> Option<String> {
    attrs
        .iter()
        .filter(|a| a.path == ["link"])
        .find_map(|a| match &a.body {
            nabic_ast::AttrBody::Args(args) => args.iter().find_map(|x| match x {
                nabic_ast::AttrArg::KeyValue(k, nabic_ast::Literal::Str(m))
                    if k == "wasm_import_module" =>
                {
                    Some(m.clone())
                }
                _ => None,
            }),
            _ => None,
        })
}

/// `#[link_name = "sym"]` on an extern function: the symbol
/// it links to; in a `wasm_import_module` block, its import name.
pub(crate) fn link_name_of(f: &FnDef) -> Option<String> {
    let a = f.attrs.iter().find(|a| a.path == ["link_name"])?;
    match &a.body {
        nabic_ast::AttrBody::Value(nabic_ast::Literal::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

/// `#[export_name("sym")]`: the symbol it names;
/// `#[export]`: the function's own name.
pub(crate) fn export_name(f: &FnDef) -> Option<String> {
    let Some(a) = f.attrs.iter().find(|a| a.name() == "export_name") else {
        return bare_export(f);
    };
    match &a.body {
        nabic_ast::AttrBody::Args(args) => args.iter().find_map(|x| match x {
            nabic_ast::AttrArg::Lit(nabic_ast::Literal::Str(s)) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    }
}

/// `#[export]`: the function's own name as its symbol.
fn bare_export(f: &FnDef) -> Option<String> {
    f.attrs
        .iter()
        .any(|a| a.path == ["export"])
        .then(|| leaf_name(&f.name).to_owned())
}

/// Bits of `ffi_flags`: `#[errno]`, `#[leaf]`.
pub(crate) const FFI_ERRNO: u8 = 1;
pub(crate) const FFI_LEAF: u8 = 2;

/// `#[errno]` / `#[leaf]` on a C declaration or its block, as
/// `FFI_*` bits.
pub(crate) fn ffi_flags(blk: &[nabic_ast::Attribute], f: &FnDef) -> u8 {
    let on = |n: &str| blk.iter().chain(&f.attrs).any(|a| a.path == [n]);
    (if on("errno") { FFI_ERRNO } else { 0 }) | (if on("leaf") { FFI_LEAF } else { 0 })
}

/// `#[on_panic(return = v)]` of an `extern "C" fn` returning `ret`: `v`
/// as a constant (`Unit` for a function without a result), or what to
/// write instead. `None`: no such attribute.
pub(crate) fn on_panic_value(def: &FnDef, ret: &HirTy) -> Option<Result<ConstVal, String>> {
    let attr = def.attrs.iter().find(|a| a.path == ["on_panic"])?;
    let name = leaf_name(&def.name);
    let lit = match &attr.body {
        AttrBody::Empty => None,
        AttrBody::Args(xs) => match xs.as_slice() {
            [AttrArg::KeyValue(k, l)] if k == "return" => Some(l),
            _ => {
                return Some(Err(format!(
                    "write `#[on_panic(return = -1)]`: the value `{name}` returns to C when it panics"
                )));
            }
        },
        AttrBody::Value(_) => {
            return Some(Err(format!(
                "write `#[on_panic(return = -1)]`: the value `{name}` returns to C when it panics"
            )));
        }
    };
    let bad = |what: &str| {
        Err(format!(
            "`#[on_panic]` on `{name}`: {what}; write `#[on_panic(return = -1)]` with a value of \
             the result type"
        ))
    };
    Some(match (ret, lit) {
        (HirTy::Unit, None) => Ok(ConstVal::Unit),
        (HirTy::Unit, Some(_)) => Err(format!(
            "`#[on_panic]` on `{name}`: it returns nothing, so write `#[on_panic]` alone"
        )),
        (_, None) => bad("a function with a result needs the value it returns"),
        (HirTy::Prim(PrimTy::Bool), Some(Literal::Bool(b))) => Ok(ConstVal::Bool(*b)),
        (HirTy::Prim(prim), Some(Literal::Int(text)))
            if prim.is_int() || *prim == PrimTy::CChar =>
        {
            match crate::lower::expr::parse_int_literal(text) {
                Some((value, _)) if prim.is_signed_int() || *prim == PrimTy::CChar => {
                    Ok(ConstVal::Int { value, ty: *prim })
                }
                Some((value, _)) if value >= 0 => Ok(ConstVal::UInt {
                    value: value.unsigned_abs(),
                    ty: *prim,
                }),
                _ => bad("the value is not a number of the result type"),
            }
        }
        (HirTy::Prim(prim), Some(Literal::Float(text) | Literal::Int(text))) if prim.is_float() => {
            let digits: String = text
                .trim_end_matches("f64")
                .trim_end_matches("f32")
                .chars()
                .filter(|c| *c != '_')
                .collect();
            match digits.parse::<f64>() {
                Ok(x) if *prim == PrimTy::F32 => Ok(ConstVal::Float {
                    bits: u64::from((x as f32).to_bits()),
                    ty: *prim,
                }),
                Ok(x) => Ok(ConstVal::Float {
                    bits: x.to_bits(),
                    ty: *prim,
                }),
                Err(_) => bad("the value is not a number"),
            }
        }
        // A pointer result: `0` is null.
        (HirTy::Struct(_) | HirTy::Sum(_), Some(Literal::Int(text)))
            if crate::lower::expr::parse_int_literal(text).is_some_and(|(v, _)| v == 0) =>
        {
            Ok(ConstVal::UInt {
                value: 0,
                ty: PrimTy::Usize,
            })
        }
        _ => bad("only a number, a `bool` or `0` (a null pointer) can be returned"),
    })
}
