//! What a hot-reload build (`--hot`) needs beyond a plugin's tables.
//!
//! The signature key of each hot-package function, and every hot struct
//! and enum the module uses (in the types of its locals, parameters,
//! results and expressions), described in full with a function giving
//! each field default: for codegen's slots, frames and GC tables.

use crate::lower::LowerCtx;
use crate::lower::env::Env;
use crate::lower::reflect::desc::{describe, is_hot, key_of};
use crate::plugin::{Hot, HotType};
use crate::{HirBlock, HirFunction, HirStaticDef, HirTy};
use nabic_core::{HashMap, HashSet};
use nabic_ir_common::FunctionId;

/// The hot tables of a `--hot` build (`packages`: the hot ones), or
/// `None`.
pub(crate) fn run(
    env: &Env<'_>,
    packages: Option<&[String]>,
    functions: &[HirFunction],
    statics: &[HirStaticDef],
    exports: &mut Vec<crate::plugin::Export>,
) -> Option<Hot> {
    let packages = packages?;
    let empty = HashMap::default();
    let mut ctx = LowerCtx::new(env, &empty, None, nabic_diagnostic::Span::DUMMY);
    let extras: Vec<HirFunction> = env.sink.extras.borrow().clone();
    let all = || functions.iter().chain(extras.iter());
    let mut sigs = Vec::new();
    let main = packages.first().map(|p| format!("{p}::main"));
    for f in all().filter(|f| f.body.is_some() && is_hot(&f.name)) {
        let params = f.params.iter().map(|p| p.ty.clone()).collect();
        let fn_ty = HirTy::Fn(
            params,
            Box::new(f.ret_ty.clone()),
            crate::FnBounds::default(),
        );
        let key = key_of(&ctx, &fn_ty);
        // A program's `main`, for the dev host.
        if main.as_ref() == Some(&f.name) && !exports.iter().any(|e| e.id == f.id) {
            exports.push(crate::plugin::Export {
                id: f.id,
                path: f.name.clone(),
                sig: key.clone(),
                shown: ctx.ty_name(&fn_ty),
            });
        }
        sigs.push((f.id, key));
    }
    let mut seen = HashSet::default();
    let mut found = Vec::new();
    for f in all() {
        let tys = f.locals.iter().map(|l| &l.ty);
        let tys = tys.chain(f.params.iter().map(|p| &p.ty)).chain([&f.ret_ty]);
        for t in tys {
            gather(&ctx, t, &mut seen, &mut found);
        }
        // Values no local holds: a matched temporary, a call's result.
        if let Some(b) = &f.body {
            crate::each_block(b, &mut |e| gather(&ctx, &e.ty, &mut seen, &mut found));
        }
    }
    let mut keys = Vec::new();
    for s in statics {
        gather(&ctx, &s.ty, &mut seen, &mut found);
        keys.push((s.name.clone(), key_of(&ctx, &s.ty)));
    }
    let mut types = Vec::new();
    for ty in found {
        let desc = describe(&mut ctx, &ty, nabic_diagnostic::Span::DUMMY);
        let (path, span) = match &ty {
            HirTy::Struct(id) => {
                let d = ctx.struct_def(*id);
                (d.name, d.span)
            }
            HirTy::Enum(id) => {
                let d = ctx.enum_def(*id);
                (d.name, d.span)
            }
            _ => continue,
        };
        let path = path.split("__").next().unwrap_or(&path);
        types.push(HotType {
            key: key_of(&ctx, &ty),
            path: nabic_core::local::unmangle(path).into_owned(),
            name: ctx.ty_name(&ty),
            span,
            desc,
            ty,
        });
    }
    Some(Hot {
        packages: packages.to_vec(),
        sigs,
        types,
        statics: keys,
    })
}

/// The structs and enums of hot packages in `ty`, by value or as type
/// arguments (`List<Enemy>` names `Enemy`), and in their fields.
fn gather(ctx: &LowerCtx<'_>, ty: &HirTy, seen: &mut HashSet<HirTy>, out: &mut Vec<HirTy>) {
    match ty {
        HirTy::Struct(id) => {
            if !seen.insert(ty.clone()) {
                return;
            }
            let d = ctx.struct_def(*id);
            if is_hot(d.name.split("__").next().unwrap_or(&d.name)) {
                out.push(ty.clone());
            }
            for t in d.generic_args.iter().chain(d.fields.iter().map(|f| &f.ty)) {
                gather(ctx, t, seen, out);
            }
        }
        HirTy::Enum(id) => {
            if !seen.insert(ty.clone()) {
                return;
            }
            let d = ctx.enum_def(*id);
            if is_hot(d.name.split("__").next().unwrap_or(&d.name)) {
                out.push(ty.clone());
            }
            let payloads = d.variants.iter().filter_map(|v| v.payload_ty.as_ref());
            for t in d.generic_args.iter().chain(payloads) {
                gather(ctx, t, seen, out);
            }
        }
        HirTy::Tuple(ts) | HirTy::Sum(ts) => {
            for t in ts {
                gather(ctx, t, seen, out);
            }
        }
        HirTy::Array(t, _) | HirTy::GcBuf(t) => gather(ctx, t, seen, out),
        HirTy::Throws { ok, err } => {
            gather(ctx, ok, seen, out);
            gather(ctx, err, seen, out);
        }
        _ => {}
    }
}

/// `fn() -> T` giving the default of field `field` of struct `owner`:
/// what a migration writes into a field the old version lacked.
pub(crate) fn default_fn(
    ctx: &LowerCtx<'_>,
    owner: &str,
    field: &str,
    ty: &HirTy,
    value: &nabic_ast::Expr,
) -> FunctionId {
    let id = FunctionId(ctx.env.sink.next_id.get());
    ctx.env.sink.next_id.set(id.0 + 1);
    let mut c = LowerCtx::with_subst(ctx.env, ctx.ty_subst.clone(), None, value.span);
    c.fn_ret_ty = ty.clone();
    let v = crate::lower::expr::field_default(&mut c, owner, field, ty, value);
    ctx.env.sink.extras.borrow_mut().push(HirFunction {
        id,
        def_id: None,
        name: format!("__hot_default_{}", id.0),
        params: Vec::new(),
        locals: c.into_locals(),
        ret_ty: ty.clone(),
        body: Some(HirBlock {
            stmts: Vec::new(),
            trailing: Some(v),
            label: None,
        }),
        is_async: false,
        is_generator: false,
        span: value.span,
        facts: nabic_ir_common::FnFacts::default(),
    });
    crate::lower::incr::touch(id);
    id
}
