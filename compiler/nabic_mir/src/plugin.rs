//! A plugin's or plugin host's tables, as codegen lays them out in its
//! module descriptor (`nabir_abi::module`), and the MIR it adds: the
//! loaded tags, the dispatchers' fallback to other modules.

pub use nabic_hir::plugin::Mode;

use crate::{
    BasicBlock, LocalDecl, MirFunction, MirModule, MirStaticDef, MirTy, Operand, RValue, Statement,
    Terminator,
};
use nabic_ir_common::{Abi, ConstVal, FnFacts, FunctionId, Linkage, LocalId, PrimOp, PrimTy};

/// A plugin's `u32` tag table (the descriptor's `tags.tags`); its code
/// loads each tag from it.
pub const TAGS: &str = "nabi.module.tags";
/// What a dispatcher asks for a tag it does not know.
pub const ANY_DISPATCH: &str = "nabi_any_dispatch";

#[derive(Clone, Debug, Default)]
pub struct Plugin {
    pub mode: Mode,
    /// Each `any` tag the code uses, with its type key (braced paths).
    pub tags: Vec<(u32, String)>,
    pub exports: Vec<Export>,
    pub dispatchers: Vec<Dispatcher>,
    /// `Plugin::get` signature statics: `(static, key)`.
    pub sigs: Vec<(String, String)>,
    /// The compiler id: modules of one process match exactly (driver).
    pub toolchain: String,
    /// A host's program name (driver).
    pub name: String,
    /// The dependencies a plugin links in (`bundle: true`) (driver).
    pub bundle: Vec<String>,
    /// Package versions by name (driver).
    pub versions: Vec<(String, String)>,
    /// `--hot`.
    pub hot: Option<Hot>,
}

/// A hot-reload build's tables (`nabic_hir::plugin::Hot`, in MIR).
#[derive(Clone, Debug, Default)]
pub struct Hot {
    /// The hot packages: the plugin's own first.
    pub packages: Vec<String>,
    /// The type key of each hot-package function's type.
    pub sigs: Vec<(FunctionId, String)>,
    pub types: Vec<HotTy>,
    /// The type key of each static's type, by static.
    pub statics: Vec<(String, String)>,
    /// This version, and the one it was checked against (driver).
    pub version: u64,
    pub base_version: u64,
}

/// One struct or enum of a hot package.
#[derive(Clone, Debug)]
pub struct HotTy {
    /// Its key, the path in braces.
    pub key: String,
    /// `game::Enemy`.
    pub path: String,
    pub name: String,
    pub span: nabic_diagnostic::Span,
    /// What it lowers to.
    pub mir: MirTy,
    pub is_enum: bool,
}

impl Hot {
    /// Item `path` belongs to a hot package.
    #[must_use]
    pub fn owns(&self, path: &str) -> bool {
        self.packages.iter().any(|p| crate::Link::owns(p, path))
    }
}

#[derive(Clone, Debug)]
pub struct Export {
    pub id: FunctionId,
    /// The `(env, args..)` code of its function value.
    pub value: FunctionId,
    pub path: String,
    pub sig: String,
    pub shown: String,
}

#[derive(Clone, Debug)]
pub struct Dispatcher {
    pub id: FunctionId,
    pub value: FunctionId,
    pub method: String,
    pub tags: Vec<u32>,
}

impl Plugin {
    /// Index of tag `tag` in `tags`.
    #[must_use]
    pub fn tag_index(&self, tag: u32) -> Option<usize> {
        self.tags.iter().position(|(t, _)| *t == tag)
    }
}

/// The FNV-1a 64 hash of a dispatcher's method key, what
/// `nabi_any_dispatch` is asked with.
#[must_use]
pub const fn method_hash(method: &str) -> u64 {
    nabir_abi::typedesc::fnv64(method.as_bytes())
}

/// After lowering: the tables, the statics the code reads them from,
/// and each dispatcher's last arm asking the process.
pub(crate) fn finish(
    m: &mut MirModule,
    hir: &nabic_hir::HirModule,
    wrapper: &dyn Fn(FunctionId) -> Option<FunctionId>,
) {
    let info = &hir.plugin;
    if info.mode == Mode::None {
        return;
    }
    let tags = hir
        .any_tags
        .iter()
        .zip(&info.tag_keys)
        .map(|((_, t), k)| (*t, k.clone()))
        .collect::<Vec<_>>();
    let exports = info
        .exports
        .iter()
        .filter(|e| m.functions.iter().any(|f| f.id == e.id))
        .filter_map(|e| {
            Some(Export {
                id: e.id,
                value: wrapper(e.id)?,
                path: e.path.clone(),
                sig: e.sig.clone(),
                shown: e.shown.clone(),
            })
        })
        .collect();
    let dispatchers: Vec<Dispatcher> = info
        .dispatchers
        .iter()
        .filter(|d| m.functions.iter().any(|f| f.id == d.id))
        .filter_map(|d| {
            Some(Dispatcher {
                id: d.id,
                value: wrapper(d.id)?,
                method: d.method.clone(),
                tags: d.tags.clone(),
            })
        })
        .collect();
    let mut sigs = Vec::new();
    for f in &m.functions {
        for s in f.blocks.iter().flat_map(|b| &b.stmts) {
            if let Statement::Assign {
                rvalue: RValue::StaticLoad { name, .. },
                ..
            } = s
            {
                if let Some(k) = nabic_hir::plugin::sig_key(name) {
                    if !sigs.iter().any(|(n, _)| n == name) {
                        sigs.push((name.clone(), k));
                    }
                }
            }
        }
    }
    for (name, _) in &sigs {
        m.statics
            .push(extern_static(name, MirTy::Prim(PrimTy::U64)));
    }
    if info.mode == Mode::Plugin {
        let n = u32::try_from(tags.len().max(1)).expect("tag count");
        m.statics.push(extern_static(
            TAGS,
            MirTy::Array(Box::new(MirTy::Prim(PrimTy::U32)), n),
        ));
    }
    let next = m.functions.iter().map(|f| f.id.0 + 1).max().unwrap_or(0);
    let ask = FunctionId(next);
    let mut asked = false;
    for d in &dispatchers {
        if let Some(f) = m.functions.iter_mut().find(|f| f.id == d.id) {
            asked |= fall_back(f, ask, method_hash(&d.method));
        }
    }
    if asked {
        m.functions.push(any_dispatch_decl(ask));
    }
    let hot = info.hot.as_ref().map(|h| Hot {
        packages: h.packages.clone(),
        sigs: h.sigs.clone(),
        statics: h.statics.clone(),
        types: h
            .types
            .iter()
            .map(|t| HotTy {
                key: t.key.clone(),
                path: t.path.clone(),
                name: t.name.clone(),
                span: t.span,
                mir: crate::ty::lower_ty(&t.ty, hir, &m.target),
                is_enum: matches!(t.ty, nabic_hir::HirTy::Enum(_)),
            })
            .collect(),
        ..Hot::default()
    });
    m.plugin = Plugin {
        mode: info.mode,
        tags,
        exports,
        dispatchers,
        sigs,
        hot,
        ..Plugin::default()
    };
}

fn extern_static(name: &str, ty: MirTy) -> MirStaticDef {
    MirStaticDef {
        name: name.to_owned(),
        ty,
        init: ConstVal::Unit,
        is_mut: true,
        is_extern: true,
        dll_import: false,
        init_fn: None,
        c: None,
    }
}

/// `extern "C" fn nabi_any_dispatch(method: u64, tag: u32) -> usize`.
fn any_dispatch_decl(id: FunctionId) -> MirFunction {
    MirFunction {
        id,
        def_id: None,
        name: ANY_DISPATCH.to_owned(),
        params: vec![LocalId(0), LocalId(1)],
        locals: vec![
            LocalDecl {
                id: LocalId(0),
                name: None,
                ty: MirTy::Prim(PrimTy::U64),
            },
            LocalDecl {
                id: LocalId(1),
                name: None,
                ty: MirTy::Prim(PrimTy::U32),
            },
        ],
        blocks: Vec::new(),
        param_tys: vec![MirTy::Prim(PrimTy::U64), MirTy::Prim(PrimTy::U32)],
        ret_ty: MirTy::Prim(PrimTy::Usize),
        span: nabic_diagnostic::Span::DUMMY,
        facts: FnFacts {
            linkage: Linkage::Extern,
            abi: Abi::C {
                variadic: false,
                system: false,
            },
            ..FnFacts::default()
        },
        strips: Vec::new(),
        pins: Vec::new(),
        dbg: crate::FnDbg::default(),
    }
}

/// Dispatcher `f`'s no-arm-matched blocks (empty, `Unreachable`) call
/// the code another module registered for the tag:
/// `nabi_any_dispatch(method, tag)` as a function value of `f`'s own
/// signature. Whether any changed.
fn fall_back(f: &mut MirFunction, ask: FunctionId, method: u64) -> bool {
    let holes: Vec<usize> = f
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| {
            b.stmts.is_empty()
                && b.params.is_empty()
                && matches!(b.terminator, Terminator::Unreachable)
        })
        .map(|(i, _)| i)
        .collect();
    if holes.is_empty() || f.params.is_empty() {
        return false;
    }
    let local = |f: &mut MirFunction, ty: MirTy| {
        let id = LocalId(u32::try_from(f.locals.len()).expect("LocalId overflow"));
        f.locals.push(LocalDecl { id, name: None, ty });
        id
    };
    let i32_ = MirTy::Prim(PrimTy::I32);
    let usize_ = MirTy::Prim(PrimTy::Usize);
    let code_ty = crate::ty::code_ty(f.param_tys.clone(), f.ret_ty.clone());
    let recv = f.params[0];
    for h in holes {
        let (raw, tag, tag32, at, code) = (
            local(f, i32_.clone()),
            local(f, i32_.clone()),
            local(f, MirTy::Prim(PrimTy::U32)),
            local(f, usize_.clone()),
            local(f, code_ty.clone()),
        );
        let ret = (f.ret_ty != MirTy::Unit).then(|| local(f, f.ret_ty.clone()));
        let mut stmts = vec![
            Statement::Assign {
                target: raw,
                rvalue: RValue::FieldRead {
                    target: Operand::Copy(recv),
                    index: 0,
                    field_ty: i32_.clone(),
                },
            },
            Statement::Assign {
                target: tag,
                rvalue: RValue::BinOp {
                    op: PrimOp::BitAnd,
                    op_ty: i32_.clone(),
                    lhs: Operand::Copy(raw),
                    rhs: Operand::Const(ConstVal::Int {
                        value: i128::from(!crate::gc_abi::ANY_BOXED),
                        ty: PrimTy::I32,
                    }),
                },
            },
            Statement::Assign {
                target: tag32,
                rvalue: RValue::Cast {
                    source: Operand::Copy(tag),
                    from_ty: i32_.clone(),
                    to_ty: MirTy::Prim(PrimTy::U32),
                },
            },
            Statement::Assign {
                target: at,
                rvalue: RValue::Call {
                    callee: ask,
                    args: vec![
                        Operand::Const(ConstVal::UInt {
                            value: u128::from(method),
                            ty: PrimTy::U64,
                        }),
                        Operand::Copy(tag32),
                    ],
                    ret_ty: usize_.clone(),
                },
            },
            Statement::Assign {
                target: code,
                rvalue: RValue::Bitcast {
                    source: Operand::Copy(at),
                    from_ty: usize_.clone(),
                    to_ty: code_ty.clone(),
                },
            },
        ];
        let mut args = vec![Operand::Const(ConstVal::Int {
            value: 0,
            ty: PrimTy::Usize,
        })];
        args.extend(f.params.iter().map(|p| Operand::Copy(*p)));
        let mut arg_tys = vec![MirTy::GcRef];
        arg_tys.extend(f.param_tys.iter().cloned());
        let call = RValue::CallIndirect {
            callee: Operand::Copy(code),
            args,
            arg_tys,
            ret_ty: f.ret_ty.clone(),
            abi: Abi::Nabi,
        };
        let target = ret.unwrap_or_else(|| local(f, MirTy::Unit));
        stmts.push(Statement::Assign {
            target,
            rvalue: call,
        });
        f.blocks[h] = BasicBlock {
            id: f.blocks[h].id,
            params: Vec::new(),
            stmts,
            terminator: Terminator::Return(ret.map(Operand::Copy)),
        };
    }
    true
}
