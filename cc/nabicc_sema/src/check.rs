//! The checker's state, and the translation unit's objects and functions
//! merged from their declarations.

use crate::{Body, Def, FuncId, Function, Global, GlobalId, LabelId, LocalId, TLocal, TStmt};
use nabic_core::HashMap;
use nabicc_lex::Loc;
use nabicc_parse::Parser;
use nabicc_parse::tree::LocalKind;
use nabicc_parse::ty::{Type, TypeId, Types};
use nabicc_pp::{Error, File};
use nabicc_target::Prim;

pub(crate) type R<T> = Result<T, Error>;

/// What a parser local became.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Slot {
    Local(LocalId),
    /// A block-scope `static`.
    Global(GlobalId),
}

/// The `switch` being checked: its cases so far.
#[derive(Default)]
pub(crate) struct SwitchCx {
    pub cases: Vec<(i128, i128, LabelId)>,
    pub default: Option<LabelId>,
    /// The promoted type of its value.
    pub ty: Option<Prim>,
}

pub struct Cx<'a> {
    pub types: Types,
    pub(crate) files: &'a [File],
    pub(crate) globals: Vec<Global>,
    pub(crate) funcs: Vec<Function>,
    pub(crate) global_by_name: HashMap<String, GlobalId>,
    pub(crate) func_by_name: HashMap<String, FuncId>,
    /// Parser objects / functions to ours.
    pub(crate) var_map: Vec<GlobalId>,
    pub(crate) fn_map: Vec<FuncId>,
    pub(crate) strings: HashMap<(Vec<u32>, Prim), GlobalId>,
    /// The function being checked.
    pub(crate) locals: Vec<TLocal>,
    pub(crate) slots: Vec<Slot>,
    pub(crate) labels: HashMap<String, (LabelId, bool, Loc)>,
    pub(crate) label_count: u32,
    pub(crate) switches: Vec<SwitchCx>,
    pub(crate) loops: u32,
    pub(crate) ret: TypeId,
    pub(crate) fn_name: String,
    pub(crate) variadic: bool,
    /// At file scope (initializers of statics, sizes): constants only.
    pub(crate) file_scope: bool,
    pub(crate) hidden: u32,
}

/// Check what `p` read (with bodies) as one translation unit.
pub fn check(p: &mut Parser<'_, '_>) -> Result<crate::Unit, Error> {
    let t = p.types.t.clone();
    let types = std::mem::replace(&mut p.types, Types::new(t));
    let files = p.pp.files.clone();
    let mut cx = Cx {
        types,
        files: &files,
        globals: Vec::new(),
        funcs: Vec::new(),
        global_by_name: HashMap::default(),
        func_by_name: HashMap::default(),
        var_map: Vec::new(),
        fn_map: Vec::new(),
        strings: HashMap::default(),
        locals: Vec::new(),
        slots: Vec::new(),
        labels: HashMap::default(),
        label_count: 0,
        switches: Vec::new(),
        loops: 0,
        ret: 0,
        fn_name: String::new(),
        variadic: false,
        file_scope: true,
        hidden: 0,
    };
    cx.merge(p)?;
    // Initializers, then bodies: a body may use any object.
    let vars = std::mem::take(&mut p.vars);
    for (i, v) in vars.iter().enumerate() {
        let Some(init) = &v.init else { continue };
        let g = cx.var_map[i];
        cx.file_scope = true;
        let ty = cx.globals[g].ty;
        let (ty, init) = cx.init_object(ty, init, v.loc)?;
        cx.globals[g].ty = ty;
        cx.globals[g].def = Def::Init(init);
    }
    // Arrays of unknown size defined tentatively have one element.
    for g in &mut cx.globals {
        if g.def == Def::Tentative {
            let c = cx.types.canon(g.ty);
            if let Type::Array { elem, len: None } = cx.types.get(c).clone() {
                g.ty = cx.types.add(Type::Array { elem, len: Some(1) });
            }
        }
    }
    let bodies = std::mem::take(&mut p.bodies);
    for b in &bodies {
        let f = cx.fn_map[b.func];
        match cx.body(f, b) {
            Ok(body) => cx.funcs[f].body = Some(body),
            Err(e) if cx.funcs[f].inline || cx.funcs[f].internal => {
                cx.funcs[f].broken = Some(e.0);
                cx.funcs[f].body = Some(Body {
                    params: Vec::new(),
                    locals: Vec::new(),
                    stmts: Vec::new(),
                    labels: 0,
                    end: b.end,
                });
            }
            Err(e) => return Err(e),
        }
    }
    Ok(crate::Unit {
        types: cx.types,
        globals: cx.globals,
        funcs: cx.funcs,
    })
}

impl Cx<'_> {
    pub(crate) fn err(&self, loc: Loc, msg: &str) -> Error {
        Error::at(self.files, loc, msg)
    }

    /// The symbol of a declaration: its `asm` label (without the
    /// target's `_`, which the object writer adds), else its name.
    fn symbol(&self, name: &str, asm: Option<&String>) -> String {
        match asm {
            Some(a) if self.types.t.underscore => a.strip_prefix('_').unwrap_or(a).to_owned(),
            Some(a) => a.clone(),
            None => name.to_owned(),
        }
    }

    /// One object per name and one function per name, from every
    /// declaration of them.
    fn merge(&mut self, p: &Parser<'_, '_>) -> R<()> {
        for v in &p.vars {
            let id = if let Some(&g) = self.global_by_name.get(&v.name) {
                let complete = self.types.size_align(v.ty.ty).is_some();
                let sym = v
                    .asm
                    .is_some()
                    .then(|| self.symbol(&v.name, v.asm.as_ref()));
                let old = &mut self.globals[g];
                if complete && self.types.size_align(old.ty).is_none() {
                    old.ty = v.ty.ty;
                }
                old.internal |= v.is_static;
                if !v.is_extern && old.def == Def::Extern {
                    old.def = Def::Tentative;
                }
                if let Some(s) = sym {
                    old.symbol = s;
                }
                g
            } else {
                if v.tls && (!v.is_extern || v.init.is_some()) {
                    return Err(self.err(
                        v.loc,
                        "defining thread-local variables is not supported by `nabi cc` yet \
                         (declaring one `extern` is); keep the state in a struct passed to each \
                         thread",
                    ));
                }
                let g = self.globals.len();
                self.globals.push(Global {
                    name: v.name.clone(),
                    symbol: self.symbol(&v.name, v.asm.as_ref()),
                    ty: v.ty.ty,
                    internal: v.is_static,
                    konst: v.ty.konst,
                    def: if v.is_extern {
                        Def::Extern
                    } else {
                        Def::Tentative
                    },
                    align: 0,
                    tls: v.tls,
                    loc: v.loc,
                });
                self.global_by_name.insert(v.name.clone(), g);
                g
            };
            if v.init.is_some() {
                // Defined: whatever came before was a declaration.
                self.globals[id].def = Def::Tentative;
            }
            self.var_map.push(id);
        }
        for f in &p.fns {
            let id = if let Some(&g) = self.func_by_name.get(&f.name) {
                let sym = f
                    .asm
                    .is_some()
                    .then(|| self.symbol(&f.name, f.asm.as_ref()));
                let canon = self.types.canon(f.ty);
                let old = &mut self.funcs[g];
                old.internal |= f.is_static;
                // Inline only if every declaration says so (else C's
                // external definition is here).
                old.inline &= f.inline;
                if f.body.is_some() {
                    old.ty = canon;
                    old.loc = f.loc;
                }
                if let Some(s) = sym {
                    old.symbol = s;
                }
                g
            } else {
                let g = self.funcs.len();
                self.funcs.push(Function {
                    name: f.name.clone(),
                    symbol: self.symbol(&f.name, f.asm.as_ref()),
                    ty: self.types.canon(f.ty),
                    internal: f.is_static,
                    inline: f.inline,
                    body: None,
                    broken: None,
                    loc: f.loc,
                });
                self.func_by_name.insert(f.name.clone(), g);
                g
            };
            self.fn_map.push(id);
        }
        Ok(())
    }

    /// A hidden object of static storage: a string or compound literal,
    /// a block-scope `static`.
    pub(crate) fn hidden_global(
        &mut self,
        what: &str,
        ty: TypeId,
        konst: bool,
        loc: Loc,
    ) -> GlobalId {
        self.hidden += 1;
        let name = format!("__nabicc.{what}.{}", self.hidden);
        self.globals.push(Global {
            name: name.clone(),
            symbol: name,
            ty,
            internal: true,
            konst,
            def: Def::Tentative,
            align: 0,
            tls: false,
            loc,
        });
        self.globals.len() - 1
    }

    /// A new local of the function being checked.
    pub(crate) fn new_local(&mut self, name: &str, ty: TypeId, loc: Loc) -> LocalId {
        self.locals.push(TLocal {
            name: name.to_owned(),
            ty,
            addressed: false,
            align: 0,
            loc,
        });
        (self.locals.len() - 1) as LocalId
    }

    /// The label of `name` in the function being checked.
    pub(crate) fn label(&mut self, name: &str, loc: Loc) -> LabelId {
        if let Some(l) = self.labels.get(name) {
            return l.0;
        }
        let id = self.fresh_label();
        self.labels.insert(name.to_owned(), (id, false, loc));
        id
    }

    pub(crate) fn fresh_label(&mut self) -> LabelId {
        self.label_count += 1;
        self.label_count - 1
    }

    fn body(&mut self, f: FuncId, b: &nabicc_parse::Body) -> R<Body> {
        self.file_scope = false;
        self.locals.clear();
        self.slots.clear();
        self.labels.clear();
        self.label_count = 0;
        self.loops = 0;
        self.switches.clear();
        self.fn_name.clone_from(&self.funcs[f].name);
        let Type::Func(fty) = self.types.get(self.funcs[f].ty).clone() else {
            return Err(self.err(b.end, "a function definition without a function type"));
        };
        self.ret = self.unqual(fty.ret.ty);
        self.variadic = fty.variadic;
        if !self.types.is_void(self.ret) && self.types.size_align(self.ret).is_none() {
            return Err(self.err(
                self.funcs[f].loc,
                "the function returns an incomplete type; define the struct before the function",
            ));
        }
        let mut params = Vec::new();
        for l in &b.locals {
            let ty = self.unqual(l.ty.ty);
            if l.kind == LocalKind::Static {
                let g = self.hidden_global(
                    &format!("{}.{}", self.fn_name, l.name),
                    ty,
                    l.ty.konst,
                    l.loc,
                );
                self.globals[g].align = l.align;
                self.slots.push(Slot::Global(g));
            } else {
                let id = self.new_local(&l.name, ty, l.loc);
                self.locals[id as usize].align = l.align;
                if l.kind == LocalKind::Param {
                    params.push(id);
                }
                self.slots.push(Slot::Local(id));
            }
        }
        let mut stmts = Vec::new();
        for s in &b.stmts {
            stmts.push(self.stmt(s)?);
        }
        for (name, (_, defined, loc)) in &self.labels {
            if !defined {
                return Err(self.err(*loc, &format!("use of undeclared label '{name}'")));
            }
        }
        // Aggregates live in memory.
        for l in &mut self.locals {
            let c = self.types.canon(l.ty);
            if !matches!(
                self.types.get(c),
                Type::Prim(_) | Type::Pointer(_) | Type::Enum(_)
            ) {
                l.addressed = true;
            }
        }
        Ok(Body {
            params,
            locals: std::mem::take(&mut self.locals),
            stmts,
            labels: self.label_count,
            end: b.end,
        })
    }

    /// Check statement `s` of the function's top level (helper for
    /// statement expressions too).
    pub(crate) fn stmts(&mut self, ss: &[nabicc_parse::Stmt]) -> R<Vec<TStmt>> {
        let mut out = Vec::new();
        for s in ss {
            out.push(self.stmt(s)?);
        }
        Ok(out)
    }
}
