//! A checked C translation unit (`nabicc_sema`) as Nabi MIR: each C
//! function a `FnRole::C` MIR function under its C name, each object a
//! static with its C bytes. Scalar locals whose address is never taken
//! are MIR locals (registers); the rest live in slots reached through
//! their address. Every target the native backend has takes the result,
//! with the C ABI of `nabic_codegen::abi`.

// The checker and the lowering follow C's grammar (long matches) and
// name values as C does (`a`, `b`, `l`, `r`).
#![allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    clippy::many_single_char_names,
    clippy::unnecessary_box_returns
)]

mod body;
mod data;
mod expr;
mod layout;
mod va;

use nabic_core::HashMap;
use nabic_diagnostic::SourceMap;
use nabic_ir_common::{Abi, FnFacts, FnRole, FunctionId, Inline, Linkage};
use nabic_mir::{MirFunction, MirModule, MirTy, Target};
use nabicc_lex::Loc;
use nabicc_pp::{Error, File};
use nabicc_sema::{Def, TK, TS, TStmt, Unit};

pub(crate) type R<T> = Result<T, Error>;

/// What one translation unit lowers with.
pub struct Options<'a> {
    pub target: &'a nabic_target::TargetSpec,
    /// The preprocessor's files, by `Loc::file`.
    pub files: &'a [File],
    /// `-g`: statement positions for the line table.
    pub debug: bool,
}

/// The module-wide state.
pub(crate) struct Lx<'a> {
    pub u: &'a Unit,
    pub t: Target,
    pub files: &'a [File],
    pub debug: bool,
    /// C file index -> source map id.
    pub file_ids: Vec<u32>,
    /// Functions kept (reached), by `FuncId`: their MIR id.
    pub fn_ids: HashMap<usize, FunctionId>,
    /// Declarations codegen intercepts or the lowering calls itself.
    pub helpers: HashMap<&'static str, FunctionId>,
    pub extra: Vec<MirFunction>,
    pub records: HashMap<u32, (u32, MirTy)>,
    pub reprs: std::collections::BTreeMap<u32, nabic_hir::Repr>,
    pub struct_names: HashMap<u32, (String, Vec<String>)>,
    pub next_fn: u32,
}

impl Lx<'_> {
    pub(crate) fn err(&self, loc: Loc, msg: &str) -> Error {
        Error::at(self.files, loc, msg)
    }

    /// A declaration of C function `name` (`__builtin_va_start`, the C
    /// library's), made once.
    pub(crate) fn helper(
        &mut self,
        name: &'static str,
        params: Vec<MirTy>,
        ret: MirTy,
        variadic: bool,
    ) -> FunctionId {
        if let Some(f) = self.helpers.get(name) {
            return *f;
        }
        let id = FunctionId(self.next_fn);
        self.next_fn += 1;
        self.extra.push(decl(id, name, params, ret, variadic));
        self.helpers.insert(name, id);
        id
    }
}

/// An external C function declaration.
fn decl(id: FunctionId, name: &str, params: Vec<MirTy>, ret: MirTy, variadic: bool) -> MirFunction {
    MirFunction {
        id,
        def_id: None,
        name: name.to_owned(),
        params: Vec::new(),
        locals: Vec::new(),
        blocks: Vec::new(),
        param_tys: params,
        ret_ty: ret,
        span: nabic_diagnostic::Span::DUMMY,
        facts: FnFacts {
            abi: Abi::C {
                variadic,
                system: false,
            },
            linkage: Linkage::Extern,
            ..FnFacts::default()
        },
        strips: Vec::new(),
        pins: Vec::new(),
        dbg: nabic_mir::FnDbg::default(),
    }
}

/// Lower `u`; `sources` gets the files the line table names.
pub fn lower(u: &Unit, opts: &Options<'_>, sources: &mut SourceMap) -> R<MirModule> {
    let file_ids = opts
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            if i == 0 || f.builtin {
                0
            } else {
                sources.add(f.path.display().to_string(), String::new())
            }
        })
        .collect();
    let mut lx = Lx {
        u,
        t: Target::new(opts.target.clone()),
        files: opts.files,
        debug: opts.debug,
        file_ids,
        fn_ids: HashMap::default(),
        helpers: HashMap::default(),
        extra: Vec::new(),
        records: HashMap::default(),
        reprs: std::collections::BTreeMap::new(),
        struct_names: HashMap::default(),
        next_fn: 0,
    };
    let (fns, globals) = reached(u);
    for &f in &fns {
        lx.fn_ids.insert(f, FunctionId(lx.next_fn));
        lx.next_fn += 1;
    }
    // Record layouts first: every type below needs them.
    for g in &globals {
        lx.mir_ty(u.globals[*g].ty, u.globals[*g].loc)?;
    }
    let mut functions = Vec::new();
    for &f in &fns {
        let func = &u.funcs[f];
        let id = lx.fn_ids[&f];
        let Some(ft) = lx.func_type(func.ty) else {
            return Err(lx.err(func.loc, "a function without a function type"));
        };
        let params = ft
            .params
            .iter()
            .map(|p| lx.param_ty(p.ty, func.loc))
            .collect::<R<Vec<_>>>()?;
        let ret = lx.param_ty(ft.ret.ty, func.loc)?;
        // wasi-libc's start calls `main` by these names (clang's).
        let sym = if func.symbol == "main" && lx.t.arch == "wasm32" {
            if params.is_empty() {
                "__main_void"
            } else {
                "__main_argc_argv"
            }
        } else {
            &func.symbol
        };
        let sym = sym.to_owned();
        let mut m = decl(id, &sym, params, ret, ft.variadic);
        if let Some(e) = &func.broken {
            return Err(nabicc_pp::Error(e.clone()));
        }
        if let Some(b) = &func.body {
            // An inline definition is this unit's own copy.
            let internal = func.internal || func.inline;
            m.facts = FnFacts {
                abi: Abi::C {
                    variadic: ft.variadic,
                    system: matches!(ft.cc, nabicc_parse::ty::CallConv::Stdcall),
                },
                linkage: Linkage::Defined,
                export_name: (!internal).then(|| sym.clone()),
                role: FnRole::C,
                inline: if func.inline {
                    Inline::Hint
                } else {
                    Inline::Auto
                },
                ..FnFacts::default()
            };
            if internal {
                // Unique in the object, out of the program's names' way.
                m.name = format!("{}.{}", func.symbol, id.0);
            }
            m.span = lx.span(func.loc);
            body::lower(&mut lx, &mut m, b, &ft)?;
        }
        functions.push(m);
    }
    let mut statics = Vec::new();
    for &g in &globals {
        statics.push(data::global(&mut lx, g)?);
    }
    functions.append(&mut lx.extra);
    functions.sort_by_key(|f| f.id);
    let mut target = lx.t.clone();
    target.reprs = std::sync::Arc::new(std::mem::take(&mut lx.reprs));
    Ok(MirModule {
        functions,
        statics,
        struct_names: std::mem::take(&mut lx.struct_names),
        target,
        link: nabic_mir::Link {
            c: true,
            ..nabic_mir::Link::default()
        },
        ..MirModule::default()
    })
}

impl Lx<'_> {
    pub(crate) fn span(&self, l: Loc) -> nabic_diagnostic::Span {
        nabic_diagnostic::Span {
            start: 0,
            end: 0,
            line: l.line,
            col: l.col,
            file: self.file_ids.get(l.file as usize).copied().unwrap_or(0),
        }
    }
}

/// The functions and objects the unit's code reaches: every external
/// definition, and what they (and the reached data) use. Inline and
/// `static` functions nobody calls are not compiled.
fn reached(u: &Unit) -> (Vec<usize>, Vec<usize>) {
    let mut fns = vec![false; u.funcs.len()];
    let mut gs = vec![false; u.globals.len()];
    let mut work: Vec<Ref> = Vec::new();
    for (i, f) in u.funcs.iter().enumerate() {
        if f.body.is_some() && !f.internal && !f.inline {
            work.push(Ref::Fn(i));
        }
    }
    for (i, g) in u.globals.iter().enumerate() {
        if !g.internal && g.def != Def::Extern {
            work.push(Ref::Global(i));
        }
    }
    while let Some(r) = work.pop() {
        match r {
            Ref::Fn(i) => {
                if std::mem::replace(&mut fns[i], true) {
                    continue;
                }
                if let Some(b) = &u.funcs[i].body {
                    for s in &b.stmts {
                        refs_stmt(s, &mut work);
                    }
                }
            }
            Ref::Global(i) => {
                if std::mem::replace(&mut gs[i], true) {
                    continue;
                }
                if let Def::Init(init) = &u.globals[i].def {
                    for it in &init.items {
                        if let nabicc_sema::InitKind::Expr(e) | nabicc_sema::InitKind::Bits(e, _) =
                            &it.kind
                        {
                            refs_expr(e, &mut work);
                        }
                    }
                }
            }
        }
    }
    let fns = (0..u.funcs.len()).filter(|i| fns[*i]).collect();
    let gs = (0..u.globals.len()).filter(|i| gs[*i]).collect();
    (fns, gs)
}

enum Ref {
    Fn(usize),
    Global(usize),
}

fn refs_stmt(s: &TStmt, w: &mut Vec<Ref>) {
    match &s.kind {
        TS::Expr(e) | TS::Do(_, e) => {
            refs_expr(e, w);
            if let TS::Do(b, _) = &s.kind {
                refs_stmt(b, w);
            }
        }
        TS::Init(_, init) => {
            for it in &init.items {
                if let nabicc_sema::InitKind::Expr(e) | nabicc_sema::InitKind::Bits(e, _) = &it.kind
                {
                    refs_expr(e, w);
                }
            }
        }
        TS::Block(ss) => ss.iter().for_each(|s| refs_stmt(s, w)),
        TS::If(c, a, b) => {
            refs_expr(c, w);
            refs_stmt(a, w);
            if let Some(b) = b {
                refs_stmt(b, w);
            }
        }
        TS::While(c, b) => {
            refs_expr(c, w);
            refs_stmt(b, w);
        }
        TS::For(i, c, s2, b) => {
            if let Some(i) = i {
                refs_stmt(i, w);
            }
            for e in [c, s2].into_iter().flatten() {
                refs_expr(e, w);
            }
            refs_stmt(b, w);
        }
        TS::Switch(e, b, ..) => {
            refs_expr(e, w);
            refs_stmt(b, w);
        }
        TS::Return(Some(e)) => refs_expr(e, w),
        TS::Label(_, b) => refs_stmt(b, w),
        TS::Return(None) | TS::Break | TS::Continue | TS::Goto(_) | TS::Null | TS::Asm => {}
    }
}

fn refs_expr(e: &nabicc_sema::TExpr, w: &mut Vec<Ref>) {
    match &e.kind {
        TK::Func(f) => w.push(Ref::Fn(*f)),
        TK::Global(g) => w.push(Ref::Global(*g)),
        TK::Int(_) | TK::Float(_) | TK::Local(_) => {}
        TK::Deref(a)
        | TK::Member(a, ..)
        | TK::AddrOf(a)
        | TK::Decay(a)
        | TK::Conv(a)
        | TK::Neg(a)
        | TK::BitNot(a)
        | TK::Not(a)
        | TK::VaStart(a)
        | TK::VaEnd(a)
        | TK::VaArg(a)
        | TK::IncDec(a, ..) => refs_expr(a, w),
        TK::Bin(_, a, b)
        | TK::Cmp(_, a, b)
        | TK::PtrAdd(a, b, _)
        | TK::PtrDiff(a, b, _)
        | TK::And(a, b)
        | TK::Or(a, b)
        | TK::Comma(a, b)
        | TK::Assign(a, b)
        | TK::OpAssign(_, a, b, _)
        | TK::PtrAssign(a, b, ..)
        | TK::VaCopy(a, b) => {
            refs_expr(a, w);
            refs_expr(b, w);
        }
        TK::Cond(a, b, c) => {
            refs_expr(a, w);
            refs_expr(b, w);
            refs_expr(c, w);
        }
        TK::Call(f, args, _) => {
            refs_expr(f, w);
            args.iter().for_each(|a| refs_expr(a, w));
        }
        TK::Compound(o, init) => {
            refs_expr(o, w);
            for it in &init.items {
                if let nabicc_sema::InitKind::Expr(e) | nabicc_sema::InitKind::Bits(e, _) = &it.kind
                {
                    refs_expr(e, w);
                }
            }
        }
        TK::StmtExpr(ss, last) => {
            ss.iter().for_each(|s| refs_stmt(s, w));
            if let Some(l) = last {
                refs_expr(l, w);
            }
        }
        TK::Builtin(_, args) => args.iter().for_each(|a| refs_expr(a, w)),
    }
}
