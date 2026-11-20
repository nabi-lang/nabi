//! C declarations: a translation unit's file-scope declarations, and
//! with `Parser::bodies` set its function bodies and initializers as
//! trees (`tree`).
//!
//! Types, record layouts and enum values are kept. Constant expressions
//! are evaluated where declarations need them (array sizes, enum values,
//! bit widths, alignments, static assertions) and for macro constants.

// Expression and layout code names values as C does (`a`, `b`, `l`, `r`).
#![allow(clippy::many_single_char_names, clippy::cognitive_complexity)]

pub mod objc;
pub mod read;
pub mod tree;
pub mod ty;

mod declarator;
mod expr;
mod init;
mod record;
mod specs;
mod stmt;
mod syntax;

pub use crate::expr::str_bytes;
pub use crate::syntax::is_builtin;
pub use crate::tree::*;

use crate::declarator::is_reserved;
use crate::ty::{
    CallConv, Enum, EnumId, Enumerator, Field, Func, QualType, Record, RecordDef, RecordId, Type,
    TypeId, Typedef, TypedefId, Types,
};
use nabic_core::HashMap;
use nabicc_lex::{Kind, Loc, Sym, Tok, kw};
use nabicc_pp::{Error, Pp, char_value, parse_int};
use nabicc_target::{Arch, Prim};
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tag {
    Record(RecordId),
    Enum(EnumId),
}

#[derive(Clone, Copy, Debug)]
pub enum DeclKind {
    Tag(Tag),
    Typedef(TypedefId),
    Function(usize),
    Var(usize),
}

#[derive(Clone, Copy, Debug)]
pub struct Decl {
    pub kind: DeclKind,
    pub loc: Loc,
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    pub name: String,
    pub ty: TypeId,
    /// Parameters as declared (arrays and functions decayed).
    pub params: Vec<(Option<String>, QualType)>,
    pub is_static: bool,
    pub inline: bool,
    pub asm: Option<String>,
    pub loc: Loc,
    /// Its definition (`Parser::bodies`).
    pub body: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct VarDecl {
    pub name: String,
    pub ty: QualType,
    pub is_static: bool,
    pub tls: bool,
    pub asm: Option<String>,
    pub loc: Loc,
    /// Declared `extern`: a definition elsewhere, unless initialized.
    pub is_extern: bool,
    /// Its initializer (with `Parser::bodies`).
    pub init: Option<Init>,
}

#[derive(Clone, Copy)]
pub(crate) enum Ord {
    Typedef(TypedefId),
    Enumerator(EnumId, u32),
    Var(usize),
    Func(usize),
    /// A local of the function being read (`Parser::locals`).
    Local(LocalId),
}

/// A constant expression's value.
#[derive(Clone, Debug)]
pub enum Val {
    Int(i128),
    Float(f64),
    Str(Vec<u8>),
    /// An address (null-based: `&((T *)0)->m`).
    Ptr(i128),
    /// Not a constant.
    None,
}

/// An expression's type and value.
#[derive(Clone, Debug)]
pub struct Typed {
    pub ty: QualType,
    pub v: Val,
    /// An lvalue; `Ptr(a)` is then its address.
    pub lvalue: bool,
}

#[derive(Default, Clone)]
#[allow(clippy::struct_excessive_bools)] // independent attributes
pub(crate) struct Attrs {
    packed: bool,
    aligned: u64,
    mode: Option<String>,
    /// A vector attribute: its argument and name.
    vector: Option<(u64, &'static str)>,
    cc: Option<CallConv>,
    asm: Option<String>,
    tls: bool,
    /// Objective-C headers: availability, ownership conventions and
    /// enum kinds.
    avail: objc::Avail,
    retained: Option<bool>,
    consumed: bool,
    consumes_self: bool,
    flag_enum: bool,
    closed_enum: bool,
    open_enum: bool,
}

impl Attrs {
    fn merge(&mut self, o: &Self) {
        self.packed |= o.packed;
        self.aligned = self.aligned.max(o.aligned);
        if o.mode.is_some() {
            self.mode.clone_from(&o.mode);
        }
        if o.vector.is_some() {
            self.vector = o.vector;
        }
        if o.cc.is_some() {
            self.cc = o.cc;
        }
        if o.asm.is_some() {
            self.asm.clone_from(&o.asm);
        }
        self.tls |= o.tls;
        self.avail.merge(&o.avail);
        if o.retained.is_some() {
            self.retained = o.retained;
        }
        self.consumed |= o.consumed;
        self.consumes_self |= o.consumes_self;
        self.flag_enum |= o.flag_enum;
        self.closed_enum |= o.closed_enum;
        self.open_enum |= o.open_enum;
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Storage {
    #[default]
    None,
    Typedef,
    Extern,
    Static,
    Auto,
}

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct Specs {
    storage: Storage,
    tls: bool,
    inline: bool,
    base: Option<TypeId>,
    konst: bool,
    void: bool,
    bool_: bool,
    char_: bool,
    int: bool,
    short: bool,
    long: u8,
    signed: bool,
    unsigned: bool,
    float: bool,
    double: bool,
    complex: bool,
    int128: bool,
    ms_int: Option<Prim>,
    float16: bool,
    fp16: bool,
    bf16: bool,
    float128: bool,
    ibm128: bool,
    attrs: Attrs,
    /// `_Nullable` and its kin among the specifiers.
    null: Option<objc::Null>,
    /// A tag declared or defined by these specifiers.
    owned: Option<(Tag, Loc)>,
    loc: Loc,
}

impl Specs {
    const fn has_type(&self) -> bool {
        self.base.is_some()
            || self.void
            || self.bool_
            || self.char_
            || self.int
            || self.short
            || self.long > 0
            || self.signed
            || self.unsigned
            || self.float
            || self.double
            || self.complex
            || self.int128
            || self.ms_int.is_some()
            || self.float16
            || self.fp16
            || self.bf16
            || self.float128
            || self.ibm128
    }
}

/// A piece of a declarator, innermost (nearest the name) first.
#[derive(Clone)]
pub(crate) enum Chunk {
    Pointer {
        konst: bool,
        cc: Option<CallConv>,
        /// Microsoft's `__ptr32` / `__ptr64`: a pointer of this many bytes.
        bytes: Option<u64>,
        null: Option<objc::Null>,
    },
    Block {
        konst: bool,
        null: Option<objc::Null>,
    },
    Array(Option<u64>),
    Func {
        params: Vec<(Option<String>, QualType)>,
        variadic: bool,
        proto: bool,
    },
}

pub(crate) struct Declarator {
    name: Option<(String, Sym, Loc)>,
    chunks: Vec<Chunk>,
    attrs: Attrs,
}

pub struct Parser<'p, 't> {
    pub pp: &'p mut Pp<'t>,
    pub types: Types,
    pub decls: Vec<Decl>,
    pub fns: Vec<FnDecl>,
    pub vars: Vec<VarDecl>,
    buf: VecDeque<Tok>,
    pack: u64,
    ords: HashMap<Sym, Ord>,
    tags: HashMap<Sym, Tag>,
    /// The parameters of the prototype being read: they hide typedef
    /// names, and array sizes may name them.
    hidden: Vec<(Sym, QualType)>,
    /// Parameter lists being read: only there may an array size vary.
    params_depth: u32,
    last: Loc,
    /// Read function bodies and initializers (`nabi cc`); bindgen
    /// skips them.
    pub bodies_on: bool,
    pub bodies: Vec<Body>,
    /// The locals of the function being read.
    locals: Vec<Local>,
    /// Block scopes being read, innermost last.
    scopes: Vec<stmt::Scope>,
    /// Ordinary names at file scope (block-scope `extern` finds them).
    file_scope: HashMap<Sym, Ord>,
    /// The name of the function being read (`__func__`).
    current_fn: Option<String>,
    /// Objective-C declarations (`objc_mode`).
    pub objc: objc::Objc,
}

type R<T> = Result<T, Error>;

/// A tag a declaration declares or defines, and where.
type Owned = Option<(Tag, Loc)>;

impl<'p, 't> Parser<'p, 't> {
    pub fn new(pp: &'p mut Pp<'t>, types: Types) -> Self {
        let mut p = Self {
            pp,
            types,
            decls: Vec::new(),
            fns: Vec::new(),
            vars: Vec::new(),
            buf: VecDeque::new(),
            pack: 0,
            ords: HashMap::default(),
            tags: HashMap::default(),
            hidden: Vec::new(),
            params_depth: 0,
            last: Loc::default(),
            bodies_on: false,
            bodies: Vec::new(),
            locals: Vec::new(),
            scopes: Vec::new(),
            file_scope: HashMap::default(),
            current_fn: None,
            objc: objc::Objc::default(),
        };
        let builtin = [
            (kw::BUILTIN_VA_LIST, Some(p.types.va_list)),
            (kw::BUILTIN_MS_VA_LIST, p.types.ms_va_list),
            (kw::INT128_T, p.types.int128_t),
            (kw::UINT128_T, p.types.uint128_t),
        ];
        for (s, d) in builtin {
            if let Some(d) = d {
                p.ords.insert(s, Ord::Typedef(d));
            }
        }
        // The SVE types are clang's implicit typedefs.
        for (n, t) in p.types.sizeless.clone() {
            p.types.typedefs.push(Typedef {
                name: n.to_owned(),
                ty: QualType::new(t),
                align: 0,
                loc: Loc::default(),
            });
            let id = (p.types.typedefs.len() - 1) as TypedefId;
            let s = p.pp.names.intern(n);
            p.ords.insert(s, Ord::Typedef(id));
        }
        p
    }

    fn err(&self, loc: Loc, msg: &str) -> Error {
        self.pp.err(loc, msg)
    }

    fn name(&self, s: Sym) -> &str {
        self.pp.names.get(s)
    }

    // Tokens.

    fn fill(&mut self, n: usize) -> R<()> {
        while self.buf.iter().filter(|t| t.kind != Kind::Pragma).count() <= n {
            let t = self.pp.next()?;
            let eof = t.kind == Kind::Eof;
            self.buf.push_back(t);
            if eof {
                break;
            }
        }
        Ok(())
    }

    fn peek_at(&mut self, n: usize) -> R<Tok> {
        self.fill(n)?;
        Ok(self
            .buf
            .iter()
            .filter(|t| t.kind != Kind::Pragma)
            .nth(n)
            .copied()
            .unwrap_or_else(|| *self.buf.back().expect("an end token")))
    }

    fn peek(&mut self) -> R<Tok> {
        self.peek_at(0)
    }

    fn bump(&mut self) -> R<Tok> {
        self.fill(0)?;
        while let Some(t) = self.buf.pop_front() {
            if t.kind == Kind::Pragma {
                if t.has(nabicc_lex::ASSUME_NONNULL) {
                    self.objc.nonnull = t.sym == 1;
                } else {
                    self.pack = u64::from(t.sym);
                }
                continue;
            }
            if t.kind == Kind::Eof {
                self.buf.push_front(t);
            }
            self.last = t.loc;
            return Ok(t);
        }
        unreachable!("fill leaves a token")
    }

    fn is_p(&self, t: Tok, p: &str) -> bool {
        t.kind == Kind::Punct && self.name(t.sym) == p
    }

    fn at(&mut self, p: &str) -> R<bool> {
        let t = self.peek()?;
        Ok(self.is_p(t, p))
    }

    fn eat(&mut self, p: &str) -> R<bool> {
        if self.at(p)? {
            self.bump()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn second_is(&mut self, p: &str) -> R<bool> {
        let t = self.peek_at(1)?;
        Ok(self.is_p(t, p))
    }

    fn next_starts_type(&mut self) -> R<bool> {
        let t = self.peek()?;
        Ok(self.starts_type(t))
    }

    fn second_starts_type(&mut self) -> R<bool> {
        let t = self.peek_at(1)?;
        Ok(self.starts_type(t))
    }

    fn expect(&mut self, p: &str) -> R<Tok> {
        let t = self.peek()?;
        if self.is_p(t, p) {
            return self.bump();
        }
        Err(self.err(
            t.loc,
            &format!("expected '{p}', found {}", self.describe(t)),
        ))
    }

    fn describe(&self, t: Tok) -> String {
        if t.kind == Kind::Eof {
            "the end of the input".to_owned()
        } else {
            format!("'{}'", self.name(t.sym))
        }
    }

    /// Skip a balanced `(..)`, `[..]` or `{..}` from its opener.
    fn skip_balanced(&mut self) -> R<()> {
        let open = self.bump()?;
        let mut depth = 1;
        loop {
            let t = self.bump()?;
            if t.kind == Kind::Eof {
                return Err(self.err(open.loc, "this bracket is never closed"));
            }
            if t.kind != Kind::Punct {
                continue;
            }
            match self.name(t.sym) {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(());
                    }
                }
                _ => {}
            }
        }
    }

    // Translation unit.

    pub fn unit(&mut self) -> R<()> {
        loop {
            let t = self.peek()?;
            if t.kind == Kind::Eof {
                return Ok(());
            }
            self.external()?;
        }
    }

    fn external(&mut self) -> R<()> {
        let t = self.peek()?;
        if self.is_p(t, ";") {
            self.bump()?;
            return Ok(());
        }
        if t.kind == Kind::Ident {
            match t.sym {
                kw::STATIC_ASSERT2 | kw::STATIC_ASSERT => return self.static_assert(),
                kw::ASM | kw::ASM2 | kw::ASM3 => {
                    self.bump()?;
                    while matches!(
                        self.peek()?.sym,
                        kw::VOLATILE | kw::VOLATILE2 | kw::VOLATILE3
                    ) {
                        self.bump()?;
                    }
                    if self.at("(")? {
                        self.skip_balanced()?;
                    }
                    self.eat(";")?;
                    return Ok(());
                }
                _ => {}
            }
        }
        if self.objc.on && self.at_objc()? {
            return self.objc_decl(&Attrs::default());
        }
        let specs = self.specs(false)?;
        let t = self.peek()?;
        // `API_AVAILABLE(..) @interface ..`: attributes on the declaration.
        if self.objc.on && !specs.has_type() && self.at_objc()? {
            return self.objc_decl(&specs.attrs);
        }
        // Attributes alone (`API_AVAILABLE(..);`): clang warns, reads on.
        if !specs.has_type() && specs.storage == Storage::None && self.at(";")? {
            self.bump()?;
            return Ok(());
        }
        // Without a type, only a declarator (implicit `int`) may follow.
        let declarator = t.kind == Kind::Ident || self.is_p(t, "*") || self.is_p(t, "(");
        if !specs.has_type() && (specs.storage == Storage::None && !specs.konst || !declarator) {
            return Err(self.unexpected(t));
        }
        if let Some((tag, loc)) = specs.owned {
            self.decls.push(Decl {
                kind: DeclKind::Tag(tag),
                loc,
            });
        }
        if self.eat(";")? {
            return Ok(());
        }
        let base = self.base_type(&specs)?;
        let mut first = true;
        loop {
            let mut pre = Attrs::default();
            if !first {
                self.attributes(&mut pre)?;
            }
            let mut d = self.declarator()?;
            d.attrs.merge(&pre);
            self.attributes(&mut d.attrs)?;
            let Some((name, sym, loc)) = d.name.clone() else {
                let t = self.peek()?;
                return Err(self.err(
                    t.loc,
                    "expected a name in this declaration (C needs a declarator here)",
                ));
            };
            let mut attrs = specs.attrs.clone();
            attrs.merge(&d.attrs);
            let ty = self.apply(base, &specs, &d.chunks, &attrs);
            if specs.storage == Storage::Typedef {
                let td = Typedef {
                    name: name.clone(),
                    ty,
                    align: attrs.aligned,
                    loc,
                };
                self.types.typedefs.push(td);
                let id = (self.types.typedefs.len() - 1) as TypedefId;
                // The first typedef of an unnamed tag names it.
                if first {
                    self.name_anonymous(ty.ty, &name);
                }
                // Uses after a redeclaration name it.
                self.bind(sym, Ord::Typedef(id));
                self.decls.push(Decl {
                    kind: DeclKind::Typedef(id),
                    loc,
                });
            } else if matches!(self.types.get(self.types.canon(ty.ty)), Type::Func(_)) {
                let params = match d.chunks.first() {
                    Some(Chunk::Func { params, .. }) => params.clone(),
                    _ => self.synth_params(ty.ty),
                };
                let inline = specs.inline
                    || matches!(self.ords.get(&sym), Some(Ord::Func(f)) if self.fns[*f].inline);
                self.fns.push(FnDecl {
                    name,
                    ty: ty.ty,
                    params,
                    is_static: specs.storage == Storage::Static,
                    inline,
                    asm: attrs.asm.clone(),
                    loc,
                    body: None,
                });
                let id = self.fns.len() - 1;
                if self.objc.on {
                    self.objc.fn_info(id, &attrs);
                }
                if !matches!(self.ords.get(&sym), Some(Ord::Func(_))) {
                    self.bind(sym, Ord::Func(id));
                }
                self.decls.push(Decl {
                    kind: DeclKind::Function(id),
                    loc,
                });
                if first && self.at("{")? {
                    if !self.bodies_on {
                        self.skip_balanced()?;
                        return Ok(());
                    }
                    // The declaration that names it (its first) gets the body.
                    let first_id = match self.ords.get(&sym) {
                        Some(Ord::Func(f)) => *f,
                        _ => id,
                    };
                    let params = match d.chunks.first() {
                        Some(Chunk::Func { params, .. }) => params.clone(),
                        _ => self.synth_params(ty.ty),
                    };
                    self.fns[first_id].ty = ty.ty;
                    self.fns[first_id].params.clone_from(&params);
                    self.fns[first_id].is_static |= specs.storage == Storage::Static;
                    self.fns[first_id].inline |= specs.inline;
                    if self.fns[first_id].asm.is_none() {
                        self.fns[first_id].asm.clone_from(&attrs.asm);
                    }
                    self.fns[first_id].loc = loc;
                    return self.body(first_id, &params);
                }
                let t = self.peek()?;
                if first
                    && !self.is_p(t, ";")
                    && !self.is_p(t, ",")
                    && !self.is_p(t, "=")
                    && matches!(d.chunks.first(), Some(Chunk::Func { proto: false, .. }))
                {
                    return Err(self.err(
                        t.loc,
                        "K&R-style function definitions are not supported; write a prototype: \
                         `int f(int a, char b) { .. }`",
                    ));
                }
            } else {
                self.vars.push(VarDecl {
                    name,
                    ty,
                    is_static: specs.storage == Storage::Static,
                    tls: specs.tls || attrs.tls,
                    asm: attrs.asm.clone(),
                    loc,
                    is_extern: specs.storage == Storage::Extern,
                    init: None,
                });
                let id = self.vars.len() - 1;
                self.bind(sym, Ord::Var(id));
                self.decls.push(Decl {
                    kind: DeclKind::Var(id),
                    loc,
                });
            }
            if self.eat("=")? {
                if self.bodies_on {
                    let init = self.initializer()?;
                    if let Some(v) = self.vars.last().map(|v| v.ty) {
                        let q = self.complete(v, &init);
                        if let Some(v) = self.vars.last_mut() {
                            v.ty = q;
                        }
                    }
                    if let Some(v) = self.vars.last_mut() {
                        v.init = Some(init);
                    }
                } else {
                    self.const_var()?;
                    self.skip_initializer()?;
                }
            }
            first = false;
            if self.eat(",")? {
                continue;
            }
            let t = self.peek()?;
            if self.is_p(t, ";") {
                self.bump()?;
                return Ok(());
            }
            return Err(self.err(
                t.loc,
                &format!(
                    "expected ';' after the declaration of `{}`, found {}",
                    d.name.map(|n| n.0).unwrap_or_default(),
                    self.describe(t)
                ),
            ));
        }
    }

    fn unexpected(&self, t: Tok) -> Error {
        if t.kind == Kind::Ident {
            return self.err(
                t.loc,
                &format!(
                    "unknown type name '{}' (declare it, or include the header that does, before \
                     its use)",
                    self.name(t.sym)
                ),
            );
        }
        self.err(
            t.loc,
            &format!("expected a C declaration, found {}", self.describe(t)),
        )
    }

    /// Objective-C headers fold a `const` integer variable's initializer
    /// as clang does (`static const NSModalResponse NSModalResponseOK =
    /// 1;` names enumerator values).
    fn const_var(&mut self) -> R<()> {
        let Some(Decl {
            kind: DeclKind::Var(id),
            ..
        }) = self.decls.last().copied()
        else {
            return Ok(());
        };
        let q = self.types.canon_q(self.vars[id].ty);
        if !self.objc.on || !q.konst || self.types.prim_of(q.ty).is_none() || self.at("{")? {
            return Ok(());
        }
        let e = self.cond_expr()?;
        if let Val::Int(n) = self.rvalue(&e).v {
            self.objc.consts.insert(id, n);
        }
        Ok(())
    }

    fn skip_initializer(&mut self) -> R<()> {
        loop {
            let t = self.peek()?;
            if t.kind == Kind::Eof {
                return Err(self.err(t.loc, "expected ';' after an initializer"));
            }
            if self.is_p(t, ",") || self.is_p(t, ";") {
                return Ok(());
            }
            if self.is_p(t, "(") || self.is_p(t, "[") || self.is_p(t, "{") {
                self.skip_balanced()?;
            } else {
                self.bump()?;
            }
        }
    }

    fn name_anonymous(&mut self, ty: TypeId, name: &str) {
        match self.types.get(ty).clone() {
            Type::Record(r) => {
                let rec = &mut self.types.records[r as usize];
                if rec.name.is_none() && rec.typedef_name.is_none() {
                    rec.typedef_name = Some(name.to_owned());
                }
            }
            Type::Enum(e) => {
                let en = &mut self.types.enums[e as usize];
                if en.name.is_none() && en.typedef_name.is_none() {
                    en.typedef_name = Some(name.to_owned());
                }
            }
            _ => {}
        }
    }

    /// Unnamed parameters of a function declared through a typedef.
    fn synth_params(&self, t: TypeId) -> Vec<(Option<String>, QualType)> {
        match self.types.get(self.types.canon(t)) {
            Type::Func(f) => f.params.iter().map(|p| (None, *p)).collect(),
            _ => Vec::new(),
        }
    }

    fn static_assert(&mut self) -> R<()> {
        let kw_tok = self.bump()?;
        self.expect("(")?;
        let e = self.cond_expr()?;
        let mut msg = String::new();
        if self.eat(",")? {
            while self.peek()?.kind == Kind::Str {
                let s = self.bump()?;
                msg.push_str(self.name(s.sym).trim_matches('"'));
            }
        }
        self.expect(")")?;
        self.eat(";")?;
        if matches!(self.rvalue(&e).v, Val::Int(0)) {
            return Err(self.err(
                kw_tok.loc,
                &format!(
                    "static assertion failed{}{msg}",
                    if msg.is_empty() { "" } else { ": " }
                ),
            ));
        }
        Ok(())
    }
}
