//! `nabi bindgen`: nabi declarations from a C header, read by nabic's own
//! C front end (`nabicc`) as the target's C compiler would.
//!
//! Binds what the header and its non-system includes declare (a system
//! header: with its own tree), plus the types they use, in the target's C
//! layout: typedefs as `type`, structs / unions `#[repr(C)]` (bitfields as
//! bytes; inexpressible layouts as opaque blobs), enums as an integer alias
//! with `const`s, functions and globals `extern "C"` (`"system"` for
//! stdcall), object pointers `CPtr<T>`, function pointers
//! `unsafe extern "C" fn(..) | none`, and number / string macros as
//! `const`s. Anything else is skipped with a comment, including by-value
//! passing of blob records (C classifies them by their real fields).

mod objc;

pub use nabicc_target::TargetInfo;

use nabic_core::{HashMap, HashSet};
use nabicc_lex::{Kind, Loc, Tok, is_keyword};
use nabicc_parse::ty::{CallConv, QualType, RecordId, Type, TypeId, TypedefId, Types};
use nabicc_parse::{DeclKind, Parser, Tag, Typed, Val};
use nabicc_pp::File;
use nabicc_target::{Arch, Os, Prim};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// What to bind and for which target.
#[allow(clippy::struct_excessive_bools)] // independent switches
pub struct Options<'a> {
    pub header: &'a Path,
    /// The target's name, for the header comment.
    pub target: &'a str,
    /// The target as the C front end needs it (`triple` is clang's).
    pub info: TargetInfo<'a>,
    /// `extern "system"` is stdcall (32-bit x86 Windows).
    pub stdcall: bool,
    /// Apple targets: the SDK.
    pub sdk: Option<PathBuf>,
    /// More compiler arguments (`-I`, `-D`, ...).
    pub clang_args: &'a [String],
    /// The machine's own system include directories describe the
    /// target's C library; else only nabic's built-in headers are.
    pub system_headers: bool,
    /// `static inline` functions get bindings to wrappers in C (`shim`
    /// of `generate_with_shim`), which the package compiles (`c`).
    pub shim: bool,
    /// Read Objective-C (`-x objective-c`): classes and protocols become
    /// `extern "objc"` blocks.
    pub objc: bool,
    /// More directories whose headers are bound, beside the header's
    /// own (the frameworks an umbrella header imports).
    pub roots: &'a [PathBuf],
}

/// [`generate`], and with `opts.shim` the C wrappers of the header's
/// `static inline` functions: `R nabi_shim_f(A a) { return f(a); }`.
pub fn generate_with_shim(opts: &Options<'_>) -> Result<(String, String), String> {
    let out = generate(opts)?;
    let shim = SHIM.with(|s| std::mem::take(&mut *s.borrow_mut()));
    Ok((out, shim))
}

thread_local! {
    /// The C wrappers `generate` made last on this thread.
    static SHIM: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Bind `opts.header`.
pub fn generate(opts: &Options<'_>) -> Result<String, String> {
    let header = nabic_core::real_path(opts.header)
        .map_err(|e| format!("{}: {e}", opts.header.display()))?;
    let cfg = nabicc_parse::read::Config {
        target: opts.info,
        args: opts.clang_args,
        host: opts.system_headers,
        sdk: opts.sdk.as_deref(),
        objc: opts.objc,
    };
    nabicc_parse::read::header(&header, &cfg, |p| bind(p, &header, opts)).map_err(|e| {
        let e = e.to_string();
        if opts.system_headers || !e.contains("file not found") {
            return e;
        }
        format!(
            "{e}\nnote: {} is not this machine's platform, so its system headers are not \
                 used; give the target's after `--` (`-isystem <dir>` or `--sysroot <dir>`)",
            opts.target
        )
    })
}

fn bind(p: &mut Parser<'_, '_>, header: &Path, opts: &Options<'_>) -> String {
    let mut dirs = vec![
        header
            .parent()
            .unwrap_or_else(|| Path::new("/"))
            .to_path_buf(),
    ];
    dirs.extend(
        opts.roots
            .iter()
            .map(|d| nabic_core::real_path(d).unwrap_or_else(|_| d.clone())),
    );
    let roots = Roots {
        dirs,
        files: RefCell::new(HashMap::default()),
    };
    let macros = eval_macros(p, &roots);
    let defs: Vec<(String, Loc)> =
        p.pp.defs
            .iter()
            .filter(|d| {
                let f = &p.pp.files[d.loc.file as usize];
                !f.builtin || f.system
            })
            .map(|d| (p.pp.names.get(d.name).to_owned(), d.loc))
            .collect();
    let decls = p.decls.clone();
    let mut g = Gen {
        t: &p.types,
        files: &p.pp.files,
        fns: &p.fns,
        vars: &p.vars,
        roots: &roots,
        ptr_bytes: p.types.t.ptr as i64,
        stdcall: opts.stdcall,
        underscore: p.types.t.underscore,
        items: String::new(),
        blocks: [String::new(), String::new()],
        types: HashSet::default(),
        values: HashSet::default(),
        typedefs: HashMap::default(),
        anon: HashMap::default(),
        layouts: HashMap::default(),
        queue: VecDeque::new(),
        cptr: false,
        // By file name: the package's `c.include` names its directory.
        shim: opts.shim.then(|| {
            let name = header.file_name().unwrap_or_default().to_string_lossy();
            format!("// Generated by `nabi bindgen --c-shim`.\n#include \"{name}\"\n")
        }),
        oc: opts.objc.then_some(&p.objc),
        objc: objc::State::default(),
        extra_null: HashMap::default(),
    };
    g.set_deployment(p.types.t.deployment(), p.types.t.ios);
    for (name, loc) in &defs {
        if !g.has(*loc) {
            continue;
        }
        if let Some((ty, value)) = macros.get(name) {
            let id = ident(name);
            if g.values.insert(id.clone()) {
                let _ = writeln!(g.items, "pub const {id}: {ty} = {value};");
            }
        }
    }
    for d in decls {
        if !g.has(d.loc) {
            continue;
        }
        match d.kind {
            DeclKind::Tag(t) => g.tag_decl(t),
            DeclKind::Typedef(id) => {
                let name = ident(&g.t.typedefs[id as usize].name);
                if g.types.insert(name.clone()) {
                    g.typedef(id, &name);
                }
            }
            DeclKind::Function(f) => g.function(f),
            DeclKind::Var(v) => g.var(v),
        }
        g.drain();
    }
    g.objc_items();
    g.drain();
    g.finish(&header.to_string_lossy(), opts)
}

/// What gets bound: non-system headers' declarations, and those of the
/// header and headers beside or below it.
struct Roots {
    dirs: Vec<PathBuf>,
    /// By file index.
    files: RefCell<HashMap<u32, bool>>,
}

impl Roots {
    fn has(&self, files: &[File], loc: Loc) -> bool {
        // Implicit declarations have no file; clang counts them in.
        let Some(f) = files.get(loc.file as usize).filter(|_| loc.file != 0) else {
            return true;
        };
        if !f.system && !f.builtin {
            return true;
        }
        *self.files.borrow_mut().entry(loc.file).or_insert_with(|| {
            !f.builtin
                && nabic_core::real_path(&f.path)
                    .is_ok_and(|p| self.dirs.iter().any(|d| p.starts_with(d)))
        })
    }
}

/// Object-like macros of the roots that evaluate to a number or string,
/// `name -> (nabi type, value)`, as they are at the header's end.
fn eval_macros(p: &mut Parser<'_, '_>, roots: &Roots) -> HashMap<String, (String, String)> {
    let msvc = p.types.t.msvc;
    let mut names = Vec::new();
    for d in &p.pp.defs {
        let file = &p.pp.files[d.loc.file as usize];
        // The predefines and -D options are not the header's.
        if file.builtin && !file.system {
            continue;
        }
        if !d.function_like
            && roots.has(&p.pp.files, d.loc)
            && expression_like(&d.body, |t| p.pp.names.get(t.sym), msvc)
            && !names.contains(&d.name)
        {
            names.push(d.name);
        }
    }
    let mut out = HashMap::default();
    for n in names {
        if !p.pp.is_defined(n) {
            continue;
        }
        let Ok(toks) = p.pp.expand_name(n) else {
            continue;
        };
        if let Some(v) = p.eval_tokens(toks).and_then(|e| macro_value(&p.types, &e)) {
            out.insert(p.pp.names.get(n).to_owned(), v);
        }
    }
    out
}

/// Could a macro body be an expression: balanced parentheses, no braces,
/// `;` or `#`, no keyword but `sizeof`, `true` / `false` and type names?
fn expression_like<'a>(tokens: &[Tok], spell: impl Fn(&Tok) -> &'a str, msvc: bool) -> bool {
    const KEYWORDS: &[&str] = &[
        "sizeof", "true", "false", "char", "short", "int", "long", "signed", "unsigned", "float",
        "double", "_Bool", "bool", "const",
    ];
    let mut depth = 0i32;
    for t in tokens {
        let s = spell(t);
        match t.kind {
            Kind::Punct => match s {
                "(" => depth += 1,
                ")" => depth -= 1,
                "{" | "}" | ";" | "#" | "##" => return false,
                _ => {}
            },
            Kind::Ident if is_keyword(t.sym, msvc) && !KEYWORDS.contains(&s) => return false,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    !tokens.is_empty() && depth == 0
}

/// The nabi type and literal of an evaluated macro constant.
fn macro_value(t: &Types, e: &Typed) -> Option<(String, String)> {
    let canon = t.canon(e.ty.ty);
    let prim = t.prim_of(canon);
    let size = t.size_align(canon).map_or(-1, |x| x.0 as i64);
    let nabi = match (t.get(canon), prim) {
        (Type::Prim(_) | Type::Enum(_), Some(p)) => {
            prim_name(p, if size < 0 { 0 } else { size }).unwrap_or_default()
        }
        _ => "",
    };
    let value = match (&e.v, nabi) {
        (Val::Int(v), "bool") => Some((*v != 0).to_string()),
        (Val::Int(v), "c_char") => Some(format!("c'\\x{:02x}'", *v as u8)),
        (Val::Int(v), n) if n.starts_with('u') => Some((*v as u64).to_string()),
        (Val::Int(v), n) if n.starts_with('i') => Some(int_literal(*v as i64)),
        (Val::Float(x), "f32" | "f64") => x.is_finite().then(|| {
            float_literal(if nabi == "f32" {
                f64::from(*x as f32)
            } else {
                *x
            })
        }),
        (Val::Str(b), _) => {
            // The value is a C string: it ends at a NUL.
            let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
            std::str::from_utf8(&b[..end]).ok().map(str_literal)
        }
        _ => None,
    };
    let nabi = if value.as_deref().is_some_and(|v| v.starts_with('"')) {
        "str"
    } else {
        nabi
    };
    value.map(|v| (nabi.to_owned(), v))
}

fn int_literal(v: i64) -> String {
    if v == i64::MIN {
        // The positive literal would not fit.
        "-9223372036854775807 - 1".to_owned()
    } else {
        v.to_string()
    }
}

fn float_literal(x: f64) -> String {
    let s = format!("{x:?}");
    if s.contains(['.', 'e']) {
        s
    } else {
        format!("{s}.0")
    }
}

fn str_literal(s: &str) -> String {
    let mut out = String::from("\"");
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The nabi primitive of a builtin C type of `size` bytes.
fn prim_name(p: Prim, size: i64) -> Option<&'static str> {
    let int = |signed: bool| {
        Some(match (signed, size) {
            (true, 1) => "i8",
            (true, 2) => "i16",
            (true, 4) => "i32",
            (true, 8) => "i64",
            (false, 1) => "u8",
            (false, 2) => "u16",
            (false, 4) => "u32",
            (false, 8) => "u64",
            _ => return None,
        })
    };
    match p {
        Prim::Bool => Some("bool"),
        Prim::Char => Some("c_char"),
        Prim::SChar | Prim::Short | Prim::Int | Prim::Long | Prim::LongLong | Prim::Int128 => {
            int(true)
        }
        Prim::UChar | Prim::UShort | Prim::UInt | Prim::ULong | Prim::ULongLong | Prim::UInt128 => {
            int(false)
        }
        Prim::Float16 => Some("f16"),
        Prim::BF16 => Some("bf16"),
        Prim::Float => Some("f32"),
        Prim::Double => Some("f64"),
        Prim::LongDouble if size == 8 => Some("f64"),
        _ => None,
    }
}

/// A C name as a nabi identifier: keywords and nabi's primitive type names
/// get a trailing `_`.
fn ident(name: &str) -> String {
    const TAKEN: &[&str] = &[
        "type", "any", "in", "where", "throws", "rethrows", "catch", "finally", "const", "static",
        "extern", "mut", "union", "pkg", "sub", "sup", "i8", "i16", "i32", "i64", "i128", "u8",
        "u16", "u32", "u64", "u128", "isize", "usize", "f32", "f64", "bool", "char", "str",
        "c_char",
    ];
    if nabic_lexer::token::RESERVED.contains(&name) || TAKEN.contains(&name) {
        format!("{name}_")
    } else {
        name.to_owned()
    }
}

const fn round_up(x: i64, a: i64) -> i64 {
    (x + a - 1) / a * a
}

/// One field as nabi lays it out.
#[derive(Clone)]
struct Field {
    name: String,
    ty: String,
    /// Offset, size and alignment in bytes (C's).
    off: i64,
    size: i64,
    align: i64,
    /// Comment lines above it.
    note: Option<String>,
}

enum Dep {
    Tag(Tag, String),
    Typedef(TypedefId, String),
}

struct Gen<'a> {
    t: &'a Types,
    files: &'a [File],
    fns: &'a [nabicc_parse::FnDecl],
    vars: &'a [nabicc_parse::VarDecl],
    roots: &'a Roots,
    ptr_bytes: i64,
    stdcall: bool,
    /// Symbols take a leading `_` (Apple, 32-bit x86 Windows).
    underscore: bool,
    items: String,
    /// The `extern "C"` and `extern "system"` blocks' bodies.
    blocks: [String; 2],
    /// Type names written or queued.
    types: HashSet<String>,
    /// Value names written (functions, statics, constants).
    values: HashSet<String>,
    /// Typedefs used, by C name: their nabi name, or why they are not
    /// usable.
    typedefs: HashMap<String, Result<String, Skip>>,
    /// Names given to anonymous structs, unions and enums.
    anon: HashMap<Tag, String>,
    /// Records' `repr` and fields, or why they are written as bytes.
    layouts: HashMap<RecordId, Result<(String, Vec<Field>), Skip>>,
    /// Types used but not yet written.
    queue: VecDeque<Dep>,
    cptr: bool,
    /// With `Options::shim`: the C wrappers so far.
    shim: Option<String>,
    /// `Options::objc`: the Objective-C declarations read.
    oc: Option<&'a nabicc_parse::objc::Objc>,
    objc: objc::State,
    /// Nullability a property's attributes give its accessors' types.
    extra_null: HashMap<TypeId, nabicc_parse::objc::Null>,
}

/// Why a declaration or type is skipped.
type Skip = String;

impl Gen<'_> {
    fn has(&self, loc: Loc) -> bool {
        self.roots.has(self.files, loc)
    }

    fn skip(&mut self, what: &str, name: &str, why: &str) {
        let _ = writeln!(self.items, "// skipped {what} `{name}`: {why}");
    }

    fn drain(&mut self) {
        while let Some(d) = self.queue.pop_front() {
            match d {
                Dep::Tag(c, name) => self.tag(c, &name),
                Dep::Typedef(c, name) => self.typedef(c, &name),
            }
        }
    }

    /// The declared name and typedef-for-linkage of a tag.
    fn tag_names(&self, tag: Tag) -> (Option<&str>, Option<&str>) {
        match tag {
            Tag::Record(r) => {
                let rec = &self.t.records[r as usize];
                (rec.name.as_deref(), rec.typedef_name.as_deref())
            }
            Tag::Enum(e) => {
                let en = &self.t.enums[e as usize];
                (en.name.as_deref(), en.typedef_name.as_deref())
            }
        }
    }

    /// A struct, union or enum declaration seen at the top level.
    fn tag_decl(&mut self, d: Tag) {
        // Anonymous ones are named by the typedef or field that uses
        // them; an enum used by none is its constants.
        let Some(name) = self.tag_name(d) else {
            if matches!(d, Tag::Enum(_)) && self.tag_names(d) == (None, None) {
                self.enumeration(d, None);
            }
            return;
        };
        if self.types.insert(name.clone()) {
            self.tag(d, &name);
        }
    }

    /// A tag's nabi name: its own, or the one given to it when anonymous.
    fn tag_name(&self, d: Tag) -> Option<String> {
        // An unnamed tag a typedef names for linkage goes by that name.
        match self.tag_names(d) {
            (Some(n), _) | (None, Some(n)) => Some(ident(n)),
            (None, None) => self.anon.get(&d).cloned(),
        }
    }

    fn tag(&mut self, d: Tag, name: &str) {
        if matches!(d, Tag::Enum(_)) {
            self.enumeration(d, Some(name));
        } else {
            self.record(d, name);
        }
    }

    /// Strip `typeof` sugar (not typedefs).
    fn peel(&self, t: TypeId) -> TypeId {
        match self.t.get(t) {
            Type::TypeOf(_) => self.t.canon(t),
            _ => t,
        }
    }

    /// The function type `t` names, through typedefs.
    fn function_type(&self, t: TypeId) -> Option<TypeId> {
        let t = self.peel(t);
        match self.t.get(t) {
            Type::Func(_) => Some(t),
            Type::Typedef(d) => self.function_type(self.t.typedefs[*d as usize].ty.ty),
            _ => None,
        }
    }

    fn size_align(&self, t: TypeId) -> (i64, i64) {
        self.t
            .size_align(t)
            .map_or((-2, -2), |(s, a)| (s as i64, a as i64))
    }

    fn prim(&self, t: TypeId) -> Option<&'static str> {
        match self.t.get(t) {
            Type::Prim(p) => prim_name(*p, self.size_align(t).0),
            _ => None,
        }
    }

    fn tag_of(&self, t: TypeId) -> Option<Tag> {
        match self.t.get(t) {
            Type::Record(r) => Some(Tag::Record(*r)),
            Type::Enum(e) => Some(Tag::Enum(*e)),
            _ => None,
        }
    }

    /// The nabi spelling of a C type, queueing the declarations it names.
    fn ty(&mut self, t: TypeId) -> Result<String, Skip> {
        let t = self.peel(t);
        if let Some(p) = self.prim(t) {
            return Ok(p.to_owned());
        }
        match self.t.get(t).clone() {
            Type::Pointer(p) => self.pointer(p),
            Type::Record(_) | Type::Enum(_) => {
                let d = self.tag_of(t).expect("a tag");
                match self.tag_name(d) {
                    Some(name) => {
                        if self.types.insert(name.clone()) {
                            self.queue.push_back(Dep::Tag(d, name.clone()));
                        }
                        Ok(name)
                    }
                    // An unnamed enum is its integer type.
                    None => match d {
                        Tag::Enum(e) => match self.t.enums[e as usize].int {
                            Some(i) => self.ty(i),
                            None => Err("type `` has no nabi form".to_owned()),
                        },
                        Tag::Record(_) => Err("an unnamed struct or union".to_owned()),
                    },
                }
            }
            Type::Typedef(d) => self.typedef_ref(d),
            Type::Array { elem, len: Some(n) } => Ok(format!("[{}; {n}]", self.ty(elem.ty)?)),
            Type::Array { elem, len: None } => Ok(format!("[{}; 0]", self.ty(elem.ty)?)),
            _ => Err(format!(
                "type `{}` has no nabi form",
                self.t.spell(QualType::new(t))
            )),
        }
    }

    /// A typedef use: builtin typedefs outside the roots resolve
    /// (`size_t` -> `usize`), others keep their name.
    fn typedef_ref(&mut self, d: TypedefId) -> Result<String, Skip> {
        let td = &self.t.typedefs[d as usize];
        let c_name = td.name.clone();
        let (loc, under) = (td.loc, td.ty.ty);
        let canon = self.t.canon(under);
        if self.oc.is_some() {
            match c_name.as_str() {
                "BOOL" => return Ok("bool".to_owned()),
                "id" | "instancetype" => {
                    self.cptr = true;
                    return Ok("CPtr<u8>".to_owned());
                }
                _ => {}
            }
            if let Some(s) = self.c_obj(under) {
                return Ok(s);
            }
            if matches!(self.t.get(canon), Type::Block(_)) {
                return Err(format!(
                    "`{c_name}` is a block type, which C declarations cannot pass"
                ));
            }
        }
        let (size, _) = self.size_align(canon);
        if let Some(p) = self.pointer_sized(&c_name, size) {
            return Ok(p.to_owned());
        }
        if !self.has(loc) {
            if let Some(p) = self.prim(canon) {
                return Ok(p.to_owned());
            }
        }
        if self.function_type(under).is_some() {
            return Err(format!("function type `{c_name}` used by value"));
        }
        if let Some(r) = self.typedefs.get(&c_name) {
            return r.clone();
        }
        let name = ident(&c_name);
        // Usable only if what it names is (an entry first: a typedef may
        // be reached again through what it names).
        self.typedefs.insert(c_name.clone(), Ok(name.clone()));
        let r = match self.ty(under) {
            Ok(_) => Ok(name.clone()),
            Err(why) => Err(format!("`{c_name}` is {why}")),
        };
        self.typedefs.insert(c_name, r.clone());
        if r.is_ok() && self.types.insert(name.clone()) {
            self.queue.push_back(Dep::Typedef(d, name));
        }
        r
    }

    /// `size_t` and its kin of pointer size: `usize` / `isize`.
    fn pointer_sized(&self, c_name: &str, size: i64) -> Option<&'static str> {
        if size != self.ptr_bytes {
            return None;
        }
        match c_name {
            "size_t" | "uintptr_t" => Some("usize"),
            "ssize_t" | "intptr_t" | "ptrdiff_t" => Some("isize"),
            _ => None,
        }
    }

    /// `T*` with `pointee` as `T`.
    fn pointer(&mut self, pointee: QualType) -> Result<String, Skip> {
        if let Some(f) = self.function_type(pointee.ty) {
            return Ok(format!("{} | none", self.fn_type(f)?));
        }
        let objc = self.oc.is_some_and(|o| {
            matches!(self.t.get(self.t.canon(pointee.ty)), Type::Record(r) if o.kind(*r).is_some())
        });
        let inner = if self.t.is_void(pointee.ty) || objc {
            "u8".to_owned()
        } else {
            self.ty(pointee.ty)?
        };
        self.cptr = true;
        Ok(format!("CPtr<{inner}>"))
    }

    /// The calling convention clang gives a function type on this target:
    /// conventions the target lacks are ignored.
    fn cc(&self, f: TypeId) -> CallConv {
        let Type::Func(func) = self.t.get(self.t.canon(f)) else {
            return CallConv::C;
        };
        let windows = self.t.t.os == Os::Windows;
        match (self.t.t.arch, func.cc) {
            (Arch::X86, cc) => cc,
            (Arch::X86_64, CallConv::Vectorcall | CallConv::Regcall | CallConv::Other(_)) => {
                func.cc
            }
            (Arch::X86_64, CallConv::Win64) if !windows => CallConv::Win64,
            (Arch::X86_64, CallConv::SysV) if windows => CallConv::SysV,
            (_, CallConv::Other(n)) => CallConv::Other(n),
            _ => CallConv::C,
        }
    }

    /// The ABI of a function type: 0 `"C"`, 1 `"system"`.
    fn abi(&self, f: TypeId) -> Result<usize, Skip> {
        match self.cc(f) {
            CallConv::C => Ok(0),
            CallConv::Stdcall if self.stdcall => Ok(1),
            cc => Err(format!(
                "calling convention `{}` has no nabi ABI",
                cc.name()
            )),
        }
    }

    fn func(&self, f: TypeId) -> nabicc_parse::ty::Func {
        match self.t.get(self.t.canon(f)) {
            Type::Func(func) => func.clone(),
            _ => unreachable!("a function type"),
        }
    }

    /// A function pointer type (without `| none`).
    fn fn_type(&mut self, f: TypeId) -> Result<String, Skip> {
        let abi = ["C", "system"][self.abi(f)?];
        let func = self.func(f);
        // Without a prototype, as with `...`, calls promote the arguments.
        if func.variadic || !func.proto {
            return Err("a variadic function pointer".to_owned());
        }
        let mut params = Vec::new();
        for p in &func.params {
            params.push(self.ty(p.ty)?);
            self.by_value(p.ty)?;
        }
        let ret = self.ret(func.ret.ty)?;
        self.by_value(func.ret.ty)?;
        if ret.contains("| none") {
            return Err("a function pointer returning a function pointer".to_owned());
        }
        Ok(format!(
            "unsafe extern \"{abi}\" fn({}){ret}",
            params.join(", ")
        ))
    }

    /// ` -> T`, or nothing for `void`.
    fn ret(&mut self, t: TypeId) -> Result<String, Skip> {
        if self.t.is_void(t) {
            Ok(String::new())
        } else {
            Ok(format!(" -> {}", self.ty(t)?))
        }
    }

    fn typedef(&mut self, d: TypedefId, name: &str) {
        let under = self.t.typedefs[d as usize].ty.ty;
        let peeled = self.peel(under);
        if let Some(tag) = self.tag_of(peeled) {
            let (n, td) = self.tag_names(tag);
            match n.or(td) {
                // `typedef struct foo foo;`
                Some(n) if ident(n) == name => return self.tag(tag, name),
                Some(_) => {}
                // `typedef struct { .. } foo;`
                None => {
                    self.anon.entry(tag).or_insert_with(|| name.to_owned());
                    self.tag(tag, name);
                    return;
                }
            }
        }
        // Methods read a block typedef through it; C cannot pass one.
        if self.oc.is_some() && matches!(self.t.get(self.t.canon(under)), Type::Block(_)) {
            return;
        }
        // A function type, directly or through another typedef.
        if self.function_type(under).is_some() {
            let _ = writeln!(
                self.items,
                "// `{name}` is a function type: pointers to it are written as `extern \"C\" fn` types"
            );
            return;
        }
        let size = self.size_align(under).0;
        let ty = match self.pointer_sized(name, size) {
            Some(p) => Ok(p.to_owned()),
            None => self.ty(under),
        };
        match ty {
            Ok(t) => {
                let _ = writeln!(self.items, "pub type {name} = {t};");
            }
            Err(why) => self.skip("typedef", name, &why),
        }
    }

    /// An enum as its integer type `name` and one constant per value;
    /// without a name, the constants alone.
    fn enumeration(&mut self, d: Tag, name: Option<&str>) {
        let Tag::Enum(e) = d else { return };
        let en = &self.t.enums[e as usize];
        let Some(int) = en.int.filter(|_| en.complete) else {
            let n = name.unwrap_or("(unnamed)");
            return self.skip("enum", n, "declared, not defined: its size is unknown");
        };
        let canon = self.t.canon(int);
        let unsigned = self.prim(canon).is_some_and(|p| p.starts_with('u'));
        let int = match self.ty(int) {
            Ok(t) => t,
            Err(why) => return self.skip("enum", name.unwrap_or("(unnamed)"), &why),
        };
        let kind = self.oc.and_then(|o| o.enums.get(&e).copied());
        match (name, kind) {
            (Some(name), Some(k)) => self.objc_enum(k, name, &int),
            (Some(name), None) => {
                let _ = writeln!(self.items, "pub type {name} = {int};");
            }
            _ => {}
        }
        let wrap = name.is_some() && kind.is_some();
        let ty = name.unwrap_or(&int).to_owned();
        let bits = self.size_align(canon).0 * 8;
        for v in &self.t.enums[e as usize].values {
            let id = ident(&v.name);
            if !self.values.insert(id.clone()) {
                continue;
            }
            let value = if unsigned {
                let m = if bits >= 64 {
                    v.value as u64
                } else {
                    (v.value as u64) & ((1u64 << bits) - 1)
                };
                m.to_string()
            } else {
                int_literal(v.value as i64)
            };
            if wrap {
                let _ = writeln!(self.items, "pub const {id}: {ty} = {ty}({value});");
            } else {
                let _ = writeln!(self.items, "pub const {id}: {ty} = {value};");
            }
        }
    }

    fn record(&mut self, d: Tag, name: &str) {
        let Tag::Record(r) = d else { return };
        if self.t.records[r as usize].def.is_none() {
            let _ = writeln!(
                self.items,
                "/// Opaque: declared, not defined.\npub struct {name};"
            );
            return;
        }
        let (repr, fields) = match self.layout(r, name) {
            Ok(l) => l,
            Err(why) => {
                let (size, align) = self
                    .t
                    .layout(r)
                    .map_or((-2, -2), |l| (l.size as i64, l.align as i64));
                return self.blob(name, size, align, &why);
            }
        };
        let kw = if self.t.records[r as usize].union {
            "union"
        } else {
            "struct"
        };
        let _ = writeln!(self.items, "#[repr({repr})]\npub {kw} {name} {{");
        for f in fields {
            if let Some(n) = f.note {
                let _ = writeln!(self.items, "    // {n}");
            }
            let _ = writeln!(self.items, "    pub {}: {};", f.name, f.ty);
        }
        self.items.push_str("}\n");
    }

    /// The `repr` and fields of record `r` named `name`, or why it has no
    /// nabi form (it is then written as bytes).
    fn layout(&mut self, r: RecordId, name: &str) -> Result<(String, Vec<Field>), Skip> {
        if let Some(l) = self.layouts.get(&r) {
            return l.clone();
        }
        let l = self.compute_layout(r, name);
        self.layouts.insert(r, l.clone());
        l
    }

    fn compute_layout(&mut self, r: RecordId, name: &str) -> Result<(String, Vec<Field>), Skip> {
        let union = self.t.records[r as usize].union;
        let no_form = || "the C layout has no nabi form".to_owned();
        let Some(cl) = self.t.layout(r) else {
            return Err(no_form());
        };
        let (size, align) = (cl.size as i64, cl.align as i64);
        let fields = self.fields(r, name, union)?;
        // nabi's C layout of these fields, against C's.
        let lay = |packed: bool| {
            let (mut end, mut al) = (0i64, 1);
            for f in &fields {
                let a = if packed { 1 } else { f.align };
                let off = if union { 0 } else { round_up(end, a) };
                if off != f.off {
                    return None;
                }
                end = if union { end.max(f.size) } else { off + f.size };
                al = al.max(a);
            }
            Some((end, al))
        };
        let (repr, end, al) = match (lay(false), lay(true)) {
            (Some((end, al)), _) if align >= al => {
                let repr = if align > al {
                    format!("C, align({align})")
                } else {
                    "C".to_owned()
                };
                (repr, end, align)
            }
            (None, Some((end, _))) if align == 1 && !union => ("packed".to_owned(), end, 1),
            _ => return Err(no_form()),
        };
        if round_up(end, al) != size {
            return Err(no_form());
        }
        Ok((repr, fields))
    }

    /// `Err` when a `t` value wouldn't cross the C ABI as C passes it: a
    /// byte-blob record, alone or inside one, classifies unlike its fields.
    fn by_value(&mut self, t: TypeId) -> Result<(), Skip> {
        let t = self.t.canon(t);
        match self.t.get(t).clone() {
            Type::Array { elem, len: Some(_) } => self.by_value(elem.ty),
            Type::Record(r) => {
                // Incomplete types aren't passed by value; unnamed ones have
                // no nabi form anyway.
                let Some(def) = self.t.records[r as usize].def.clone() else {
                    return Ok(());
                };
                let Some(name) = self.tag_name(Tag::Record(r)) else {
                    return Ok(());
                };
                self.layout(r, &name).map_err(|why| {
                    format!("`{name}` is passed by value but written as bytes ({why})")
                })?;
                for f in &def.fields {
                    if f.name.is_some() || f.anon || f.bits.is_some() {
                        self.by_value(f.ty.ty)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// A record's fields, bitfield runs as byte arrays. `Err` when one
    /// has no nabi form.
    fn fields(&mut self, r: RecordId, name: &str, union: bool) -> Result<Vec<Field>, Skip> {
        let def = self.t.records[r as usize]
            .def
            .clone()
            .expect("a definition");
        let offsets = self.t.layout(r).expect("a layout").offsets;
        let mut out: Vec<Field> = Vec::new();
        // An open bitfield run: first and end bit, and its members.
        let mut run: Option<(i64, i64, Vec<String>)> = None;
        let mut runs = 0;
        let mut anon = 0;
        let mut close = |run: &mut Option<(i64, i64, Vec<String>)>,
                         end_byte: Option<i64>,
                         out: &mut Vec<Field>| {
            if let Some((start, end, members)) = run.take() {
                let off = start / 8;
                let size = end_byte.unwrap_or_else(|| (end + 7) / 8) - off;
                out.push(Field {
                    name: format!("_bitfield{runs}"),
                    ty: format!("[u8; {size}]"),
                    off,
                    size,
                    align: 1,
                    note: Some(format!(
                        "bitfields from bit {}: {}",
                        off * 8,
                        members.join(", ")
                    )),
                });
                runs += 1;
            }
        };
        for (i, f) in def.fields.iter().enumerate() {
            let fname = f.name.clone().unwrap_or_default();
            let bits = if f.anon {
                // As clang reports it: through the first named field.
                if self.first_field(f.ty.ty).is_none() {
                    -1
                } else {
                    offsets[i] as i64
                }
            } else {
                offsets[i] as i64
            };
            if bits < 0 {
                return Err(format!("field `{fname}` has no offset"));
            }
            if let Some(width) = f.bits {
                if union {
                    return Err("a bitfield in a union".to_owned());
                }
                let width = width as i64;
                let r = run.get_or_insert((bits, bits, Vec::new()));
                r.1 = r.1.max(bits + width);
                if !fname.is_empty() && width > 0 {
                    r.2.push(format!(
                        "{fname} ({}..{})",
                        bits - r.0 / 8 * 8,
                        bits + width - r.0 / 8 * 8
                    ));
                }
                continue;
            }
            close(&mut run, Some(bits / 8), &mut out);
            let field = if fname.is_empty() {
                anon += 1;
                format!("_anon{}", anon - 1)
            } else {
                ident(&fname)
            };
            let ft = f.ty.ty;
            // A field of an unnamed struct, union or enum type names it.
            let peeled = self.peel(ft);
            if let Some(tag) = self.tag_of(peeled) {
                if self.tag_name(tag).is_none() {
                    self.anon
                        .insert(tag, format!("{name}_{}", field.trim_start_matches('_')));
                }
            }
            let ty = self
                .ty(ft)
                .map_err(|why| format!("field `{field}`: {why}"))?;
            let (mut size, mut align) = self.size_align(ft);
            if let Type::Array { elem, len: None } = self.t.get(self.t.canon(peeled)) {
                align = self.size_align(elem.ty).1;
                size = 0;
            }
            if size < 0 || align <= 0 {
                return Err(format!("field `{field}` has no size"));
            }
            out.push(Field {
                name: field,
                ty,
                off: bits / 8,
                size,
                align,
                note: None,
            });
        }
        close(&mut run, None, &mut out);
        Ok(out)
    }

    /// The first named field of a record, or of its anonymous members.
    fn first_field(&self, t: TypeId) -> Option<String> {
        let Type::Record(r) = self.t.get(self.t.canon(t)) else {
            return None;
        };
        let def = self.t.records[*r as usize].def.as_ref()?;
        def.fields.iter().find_map(|f| {
            if f.anon {
                self.first_field(f.ty.ty)
            } else {
                f.name.clone().filter(|n| !n.is_empty())
            }
        })
    }

    /// A record nabi cannot lay out field by field: its bytes.
    fn blob(&mut self, name: &str, size: i64, align: i64, why: &str) {
        if size < 0 || align <= 0 {
            return self.skip("struct", name, why);
        }
        let repr = if align > 1 {
            format!("C, align({align})")
        } else {
            "C".to_owned()
        };
        let _ = writeln!(
            self.items,
            "// `{name}` as bytes ({why})\n#[repr({repr})]\npub struct {name} {{\n    pub _opaque: [u8; {size}];\n}}"
        );
    }

    fn function(&mut self, f: usize) {
        let d = &self.fns[f];
        let c_name = d.name.clone();
        let id = ident(&c_name);
        if self.values.contains(&id) {
            return;
        }
        if (d.is_static || d.inline) && self.shim.is_none() {
            return self.skip(
                "fn",
                &c_name,
                "static or inline, no symbol to link; `nabi bindgen --c-shim <file.c>` writes a \
                 C wrapper for it, compiled from the manifest's `c.sources`",
            );
        }
        if d.is_static || d.inline {
            return self.shim_fn(f);
        }
        if self.oc.is_some() && self.objc_fn(f) {
            return self.objc_function(f);
        }
        match self.signature(f) {
            Ok((abi, sig)) => {
                self.values.insert(id.clone());
                // A stdcall symbol's `@N` is nabi's to add.
                let sym = if abi == 0 {
                    self.symbol(&c_name, self.fns[f].asm.as_deref())
                } else {
                    c_name
                };
                let block = &mut self.blocks[abi];
                if id != sym {
                    let _ = writeln!(block, "    #[link_name = \"{sym}\"]");
                }
                let _ = writeln!(block, "    pub fn {id}{sig};");
            }
            Err(why) => self.skip("fn", &c_name, &why),
        }
    }

    /// A `static inline` function through its C wrapper `nabi_shim_<f>`.
    fn shim_fn(&mut self, f: usize) {
        let c_name = self.fns[f].name.clone();
        let id = ident(&c_name);
        let Some(t) = self.function_type(self.fns[f].ty) else {
            return self.skip("fn", &c_name, "not a function type");
        };
        let func = self.func(t);
        if func.variadic {
            return self.skip("fn", &c_name, "a variadic inline function has no wrapper");
        }
        match self.signature(f) {
            Ok((abi, sig)) => {
                self.values.insert(id.clone());
                let block = &mut self.blocks[abi];
                let _ = writeln!(block, "    #[link_name = \"nabi_shim_{c_name}\"]");
                let _ = writeln!(block, "    pub fn {id}{sig};");
                let spell = |q: QualType| format!("__typeof__({})", self.t.spell(q));
                let params: Vec<String> = func
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("{} a{i}", spell(*p)))
                    .collect();
                let args: Vec<String> = (0..func.params.len()).map(|i| format!("a{i}")).collect();
                let void = self.t.is_void(func.ret.ty);
                let line = format!(
                    "{} nabi_shim_{c_name}({}) {{ {}{c_name}({}); }}\n",
                    spell(func.ret),
                    if params.is_empty() {
                        "void".to_owned()
                    } else {
                        params.join(", ")
                    },
                    if void { "" } else { "return " },
                    args.join(", ")
                );
                if let Some(s) = self.shim.as_mut() {
                    s.push_str(&line);
                }
            }
            Err(why) => self.skip("fn", &c_name, &why),
        }
    }

    /// The symbol a declaration links to, without the target's `_`
    /// prefix: its name, or its asm label (`fopen$DARWIN_EXTSN`).
    fn symbol(&self, name: &str, asm: Option<&str>) -> String {
        match asm {
            Some(l) if self.underscore => l.strip_prefix('_').unwrap_or(l).to_owned(),
            Some(l) => l.to_owned(),
            None => name.to_owned(),
        }
    }

    /// `(abi, "(params) -> R")` of a function declaration.
    fn signature(&mut self, f: usize) -> Result<(usize, String), Skip> {
        let d = &self.fns[f];
        let t = self.function_type(d.ty).ok_or("not a function type")?;
        let abi = self.abi(t)?;
        let func = self.func(t);
        let mut seen = HashSet::default();
        let mut params = Vec::new();
        for (i, (pname, pt)) in self.fns[f].params.iter().enumerate() {
            let mut name = ident(pname.as_deref().unwrap_or(""));
            if name.is_empty() || !seen.insert(name.clone()) {
                name = format!("arg{i}");
            }
            params.push(format!("{name}: {}", self.ty(pt.ty)?));
            self.by_value(pt.ty)?;
        }
        if func.variadic || !func.proto {
            params.push("...".to_owned());
        }
        let ret = self.ret(func.ret.ty)?;
        self.by_value(func.ret.ty)?;
        Ok((abi, format!("({}){ret}", params.join(", "))))
    }

    fn var(&mut self, var: usize) {
        let d = &self.vars[var];
        let c_name = d.name.clone();
        let id = ident(&c_name);
        let t = d.ty;
        // An array is as const as its elements.
        let mut elem = t;
        while let Type::Array { elem: inner, .. } = self.t.get(self.peel(elem.ty)) {
            elem = *inner;
        }
        let constant = self.t.canon_q(elem).konst;
        if d.is_static || !self.values.insert(id.clone()) {
            return;
        }
        if d.tls {
            return self.skip("static", &c_name, "thread-local");
        }
        if self.oc.is_some() && self.objc_var(var) {
            return;
        }
        match self.ty(t.ty) {
            Ok(ty) => {
                let sym = self.symbol(&c_name, self.vars[var].asm.as_deref());
                let block = &mut self.blocks[0];
                if id != sym {
                    let _ = writeln!(block, "    #[link_name = \"{sym}\"]");
                }
                let m = if constant { "" } else { "mut " };
                let _ = writeln!(block, "    pub static {m}{id}: {ty};");
            }
            Err(why) => self.skip("static", &c_name, &why),
        }
    }

    fn finish(self, header: &str, opts: &Options<'_>) -> String {
        if let Some(s) = &self.shim {
            SHIM.with(|x| s.clone_into(&mut x.borrow_mut()));
        }
        let file = Path::new(header)
            .file_name()
            .map_or_else(|| header.to_owned(), |f| f.to_string_lossy().into_owned());
        let mut out = format!(
            "// Generated by `nabi bindgen` from {file} for {} ({}).\n#![allow(nonstandard_style)]\n\n",
            opts.target, opts.info.triple
        );
        if self.cptr {
            out.push_str("use bare::ptr::CPtr;\n\n");
        }
        out.push_str(&self.items);
        for (abi, body) in ["C", "system"].iter().zip(&self.blocks) {
            if !body.is_empty() {
                let _ = write!(out, "\nextern \"{abi}\" {{\n{body}}}\n");
            }
        }
        self.objc_finish(&mut out);
        out
    }
}
