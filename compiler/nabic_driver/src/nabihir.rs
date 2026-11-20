//! NABIHIR: the interface of a prebuilt library package,
//! `<name>.nabihir` next to its `lib<name>.a`.
//!
//! ```text
//! header   "NABH" | version u32 | flags u32 | body_offset u32   (LE)
//! producer str                      "nabic <version> (<compiler id>)"
//! meta     str name | str hash | str source dir
//!          varint gc type offset | varint gc type count
//!          varint n | n x str feature
//! body     varint n | n x (str path | str text)   (at body_offset)
//!          varint n | n x (str function | str summary)
//!          varint n | n x (str function | varint flags
//!                          | varint keeps_none | varint returns)
//! ```
//!
//! `str` is a `PrefixVarInt` length + UTF-8. The body is every
//! source file with concrete function bodies cut to `;` (line breaks kept
//! for diagnostics), then the `.wit` files `ffi::wit` reads, then the
//! use-after-hand-off summaries a cut body can't show, then what calls of
//! the archive's functions do (`nabic_ir_common::Summary`, sorted by
//! function).
// Source rather than binary sections: monomorphization works on
// AST anyway, and a binary tree would only save the cheap parse.

use crate::SourceFile;
use nabic_ast::{FnDef, ImplItem, Item, Ty};
use nabic_diagnostic::SourceMap;
use nabic_ir_common::Summary;
use nabic_lexer::TokenKind as K;
use std::path::{Path, PathBuf};

/// Bumped on any change to the layout or to what the body holds.
pub const VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"NABH";

#[derive(Clone, Debug, Default)]
pub struct Nabihir {
    pub producer: String,
    /// The package's mount name (`--extern <name>=`): it is in symbols.
    pub name: String,
    /// Package hash its symbols were mangled with.
    pub hash: String,
    /// Where the package's sources were: the body's files live there
    /// (an overlay; the disk is not read).
    pub src_dir: PathBuf,
    /// GC type ids `[offset, offset + count)` (above the 801 user base)
    /// are this library's.
    pub type_offset: u32,
    pub type_count: u32,
    /// The `feature` cfg values it was built with: downstream parses
    /// the body with these, not its own.
    pub features: Vec<String>,
    /// The packages it was built with (`--extern` names): whose `pub`
    /// operator tokens its files lex.
    pub deps: Vec<String>,
    /// `(path relative to src_dir, interface text)`.
    pub files: Vec<(String, String)>,
    /// `(function, summary)` (`nabic_hir::HirModule::summaries`).
    pub summaries: Vec<(String, String)>,
    /// What calls of the archive's functions do, sorted by function
    /// (`nabic_mir::summary::export`).
    pub facts: Vec<(String, Summary)>,
}

/// The cache folder of `compiler_id`'s memos.
pub const COMPILER_IDS: &str = "ids";

/// This compiler: a NABIHIR from any other one is stale.
pub fn producer() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let id = ID.get_or_init(compiler_id);
    format!("nabic {} ({id})", env!("CARGO_PKG_VERSION"))
}

/// A hash of the compiler's bytes, so one build has one id on every
/// machine and path. `nabi` hashes its `nabic`, so the two share caches.
/// Kept in the cache by the file's stamp: only a new nabic reads itself.
fn compiler_id() -> String {
    let Some(exe) = nabic_core::compiler_exe() else {
        return String::new();
    };
    let stamp = format!("{}\n{}", exe.display(), nabic_core::file_stamp(&exe));
    let memo = crate::cache_dir().map(|d| {
        d.join(COMPILER_IDS)
            .join(format!("{:016x}", nabic_core::hash::fnv(stamp.as_bytes())))
    });
    let known = |s: &str| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if let Some(id) = memo
        .as_ref()
        .and_then(|m| std::fs::read_to_string(m).ok())
        .filter(|s| known(s))
    {
        return id;
    }
    let Ok(bytes) = std::fs::read(&exe) else {
        return nabic_core::file_stamp(&exe);
    };
    let id = format!("{:016x}", nabic_core::hash::fast(&bytes));
    if let Some(m) = memo {
        let tmp = m.with_extension(format!("tmp{}", std::process::id()));
        let _ = m.parent().map(std::fs::create_dir_all);
        if std::fs::write(&tmp, &id).is_ok() && std::fs::rename(&tmp, &m).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    id
}

fn put_varint(out: &mut Vec<u8>, v: u64) {
    // PrefixVarInt: n leading 1 bits in the first byte = n more
    // bytes; the value is big-endian over the remaining bits.
    let extra = (0..=8)
        .find(|&n| n == 8 || v < 1u64 << (7 * (n + 1)))
        .unwrap_or(8);
    let first_bits = 7 - extra.min(7);
    let prefix = if extra == 8 { 0xFF } else { !(0xFFu8 >> extra) };
    let first = if extra == 8 {
        0
    } else {
        (v >> (8 * extra)) as u8 & ((1u16 << first_bits) - 1) as u8
    };
    out.push(prefix | first);
    for i in (0..extra).rev() {
        out.push((v >> (8 * i)) as u8);
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    put_varint(out, s.len() as u64);
    out.extend_from_slice(s.as_bytes());
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let v = *self.b.get(self.pos).ok_or("truncated NABIHIR")?;
        self.pos += 1;
        Ok(v)
    }

    fn varint(&mut self) -> Result<u64, String> {
        let first = self.byte()?;
        let extra = first.leading_ones() as usize;
        let mut v = if extra >= 7 {
            0
        } else {
            u64::from(first & (0x7F >> extra))
        };
        for _ in 0..extra {
            v = (v << 8) | u64::from(self.byte()?);
        }
        Ok(v)
    }

    fn str(&mut self) -> Result<String, String> {
        let n = usize::try_from(self.varint()?).map_err(|_| "bad NABIHIR length")?;
        let end = self.pos.checked_add(n).filter(|&e| e <= self.b.len());
        let s = &self.b[self.pos..end.ok_or("truncated NABIHIR")?];
        self.pos += n;
        String::from_utf8(s.to_vec()).map_err(|_| "NABIHIR string is not UTF-8".to_owned())
    }

    fn u32(&mut self) -> Result<u32, String> {
        let mut w = [0; 4];
        for x in &mut w {
            *x = self.byte()?;
        }
        Ok(u32::from_le_bytes(w))
    }
}

impl Nabihir {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut meta = Vec::new();
        put_str(&mut meta, &self.producer);
        put_str(&mut meta, &self.name);
        put_str(&mut meta, &self.hash);
        put_str(&mut meta, &self.src_dir.display().to_string());
        put_varint(&mut meta, u64::from(self.type_offset));
        put_varint(&mut meta, u64::from(self.type_count));
        put_varint(&mut meta, self.features.len() as u64);
        for f in &self.features {
            put_str(&mut meta, f);
        }
        put_varint(&mut meta, self.deps.len() as u64);
        for d in &self.deps {
            put_str(&mut meta, d);
        }
        let mut out = MAGIC.to_vec();
        out.extend(VERSION.to_le_bytes());
        out.extend(0u32.to_le_bytes());
        out.extend(u32::try_from(16 + meta.len()).unwrap_or(0).to_le_bytes());
        out.extend(meta);
        put_varint(&mut out, self.files.len() as u64);
        for (path, text) in &self.files {
            put_str(&mut out, path);
            put_str(&mut out, text);
        }
        put_varint(&mut out, self.summaries.len() as u64);
        for (f, s) in &self.summaries {
            put_str(&mut out, f);
            put_str(&mut out, s);
        }
        put_varint(&mut out, self.facts.len() as u64);
        for (f, s) in &self.facts {
            put_str(&mut out, f);
            put_varint(&mut out, u64::from(s.flags));
            put_varint(&mut out, s.keeps_none);
            put_varint(&mut out, s.returns);
        }
        out
    }

    /// Parse; a wrong magic or format version is an error.
    pub fn decode(b: &[u8]) -> Result<Self, String> {
        if !b.starts_with(MAGIC) {
            return Err("not a NABIHIR file".to_owned());
        }
        let mut r = Reader { b, pos: 4 };
        let version = r.u32()?;
        if version != VERSION {
            return Err(format!(
                "NABIHIR format version {version}, this nabic reads version {VERSION}"
            ));
        }
        let _flags = r.u32()?;
        let body = r.u32()? as usize;
        let mut out = Self {
            producer: r.str()?,
            name: r.str()?,
            hash: r.str()?,
            src_dir: PathBuf::from(r.str()?),
            type_offset: u32::try_from(r.varint()?).map_err(|_| "bad type offset")?,
            type_count: u32::try_from(r.varint()?).map_err(|_| "bad type count")?,
            features: Vec::new(),
            deps: Vec::new(),
            files: Vec::new(),
            summaries: Vec::new(),
            facts: Vec::new(),
        };
        for _ in 0..r.varint()? {
            out.features.push(r.str()?);
        }
        for _ in 0..r.varint()? {
            out.deps.push(r.str()?);
        }
        r.pos = body;
        for _ in 0..r.varint()? {
            out.files.push((r.str()?, r.str()?));
        }
        for _ in 0..r.varint()? {
            out.summaries.push((r.str()?, r.str()?));
        }
        for _ in 0..r.varint()? {
            let f = r.str()?;
            let flags = u32::try_from(r.varint()?).map_err(|_| "bad summary flags")?;
            let s = Summary {
                flags,
                keeps_none: r.varint()?,
                returns: r.varint()?,
                ..Summary::default()
            };
            out.facts.push((f, s));
        }
        Ok(out)
    }

    /// Read `path`; it must come from this compiler (else: rebuild it).
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let r = Self::decode(&bytes).map_err(|e| {
            format!(
                "{}: {e} - rebuild the dependency with this nabic",
                path.display()
            )
        })?;
        let me = producer();
        if r.producer != me {
            return Err(format!(
                "{} was built by `{}`, not this `{me}` - rebuild the dependency",
                path.display(),
                r.producer
            ));
        }
        Ok(r)
    }

    /// End of this library's GC type ids.
    #[must_use]
    pub const fn type_end(&self) -> u32 {
        self.type_offset + self.type_count
    }

    /// `nabi inspect --dump`: header fields, then every file.
    #[must_use]
    pub fn text(&self) -> String {
        let mut s = format!(
            "nabihir {VERSION}\nproducer: {}\npackage: {} (hash: {})\nsource: {}\ngc types: \
             {}..{}\nfeatures: {}\n",
            self.producer,
            self.name,
            self.hash,
            self.src_dir.display(),
            self.type_offset,
            self.type_end(),
            self.features.join(", ")
        );
        for (path, text) in &self.files {
            s.push_str(&format!("\n--- {path}\n{text}"));
            if !text.ends_with('\n') {
                s.push('\n');
            }
        }
        if !self.summaries.is_empty() {
            s.push_str("\n--- summaries\n");
        }
        for (f, sum) in &self.summaries {
            s.push_str(&format!("{f}\n"));
            for line in sum.lines() {
                s.push_str(&format!("    {}\n", line.replace('\t', " ")));
            }
        }
        if !self.facts.is_empty() {
            s.push_str("\n--- calls\n");
        }
        for (f, x) in &self.facts {
            s.push_str(f);
            for (flag, name) in [
                (Summary::READONLY, "readonly"),
                (Summary::QUIET, "quiet"),
                (Summary::COLD, "cold"),
            ] {
                if x.has(flag) {
                    s.push_str(&format!(" {name}"));
                }
            }
            if x.keeps_none != 0 {
                s.push_str(&format!(" keeps-none:{:#x}", x.keeps_none));
            }
            if x.returns != 0 {
                s.push_str(&format!(" returns:{:#x}", x.returns));
            }
            s.push('\n');
        }
        s
    }

    /// The summary of this library's function `name` (`facts`).
    #[must_use]
    pub fn fact(&self, name: &str) -> Option<Summary> {
        let i = self
            .facts
            .binary_search_by(|(f, _)| f.as_str().cmp(name))
            .ok()?;
        Some(self.facts[i].1)
    }
}

/// Whether `f`'s body goes into the `.a` only (cut from the interface):
/// a concrete, non-coroutine, non-`#[inline]` nabi function. A `-> some`
/// fn keeps it: its users take the return type from the body. `x: some
/// Tr` and an untyped parameter (free fns and inherent methods, `inherent`)
/// are type parameters, so such a fn is generic too. A `const fn` keeps
/// it: a dependent's constant may call it at compile time.
fn cut(f: &FnDef, inherent: bool) -> bool {
    let implicit = f.params.iter().any(|p| match &p.ty {
        Some(t) => matches!(&t.kind, nabic_ast::TyKind::Some(b)
            if !matches!(b.as_slice(), [Ty { kind: nabic_ast::TyKind::Fn(..), .. }])),
        None => inherent && p.name != "self" && !p.variadic,
    });
    f.body.is_some()
        && f.generics.params.is_empty()
        && !implicit
        && !matches!(
            f.ret_ty.as_ref().map(|t| &t.kind),
            Some(nabic_ast::TyKind::Some(_))
        )
        && !f.is_async
        && !f.is_generator
        && !nabic_ast::inline_wanted(&f.attrs)
        && !f
            .attrs
            .iter()
            .any(|a| a.path == [nabic_ast::EXTERN_FN_ATTR] || a.path == [nabic_ast::CONST_FN_ATTR])
}

/// Whether `f` returns one of its module's `type X = some Tr;` (named in
/// `some`): it defines `X`, so dependents need its body.
fn defines_some(f: &FnDef, some: &[&str]) -> bool {
    let Some(nabic_ast::TyKind::Path(p)) = f.ret_ty.as_ref().map(|t| &t.kind) else {
        return false;
    };
    match p.segments.as_slice() {
        [s] => some.contains(&s.name.as_str()),
        [m, s] => m.name == "self" && some.contains(&s.name.as_str()),
        _ => false,
    }
}

fn cut_spans(items: &[Item], out: &mut Vec<(nabic_diagnostic::Span, nabic_diagnostic::Span)>) {
    let some: Vec<&str> = (items.iter())
        .filter_map(|i| match i {
            Item::TypeAlias(a) if matches!(a.ty.kind, nabic_ast::TyKind::Some(_)) => {
                Some(a.name.as_str())
            }
            _ => None,
        })
        .collect();
    for item in items {
        match item {
            Item::Fn(f) if cut(f, true) && !defines_some(f, &some) => {
                out.push((f.span, f.body.as_ref().map_or(f.span, |b| b.span)));
            }
            Item::Impl(i) if i.generics.params.is_empty() => {
                for it in &i.items {
                    if let ImplItem::Method(m) = it {
                        if cut(m, i.trait_path.is_none()) {
                            out.push((m.span, m.body.as_ref().map_or(m.span, |b| b.span)));
                        }
                    }
                }
            }
            Item::Mod(m) => cut_spans(m.items.as_deref().unwrap_or_default(), out),
            _ => {}
        }
    }
}

/// `text` with the bodies of `items` (its parse) cut to `;`.
fn strip(text: &str, file_id: u32, items: &[Item]) -> String {
    let (tokens, _) = nabic_lexer::Lexer::new(text, String::new()).lex_all();
    // A template could hold a `fn` this would cut.
    if crate::defines_metas(&tokens, text) {
        return text.to_owned();
    }
    let mut spans = Vec::new();
    cut_spans(items, &mut spans);
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for (f, body) in spans {
        let (fs, bs) = (f.start as usize, body.start as usize);
        // A derived / generated fn's spans point elsewhere: leave it.
        if body.file != file_id
            || bs <= fs
            || text.as_bytes().get(bs) != Some(&b'{')
            || !text.get(fs..bs).is_some_and(|s| s.contains("fn"))
        {
            continue;
        }
        let Some(open) = tokens.iter().position(|t| t.span.start as usize == bs) else {
            continue;
        };
        let mut depth = 0i32;
        let close = tokens[open..].iter().find(|t| {
            match t.kind {
                nabic_lexer::TokenKind::LBrace => depth += 1,
                nabic_lexer::TokenKind::RBrace => depth -= 1,
                _ => {}
            }
            depth == 0
        });
        if let Some(c) = close {
            cuts.push((bs, c.span.end as usize));
        }
    }
    cuts.sort_unstable();
    let mut out = String::with_capacity(text.len() / 2);
    let mut at = 0;
    for (s, e) in cuts {
        if s < at {
            continue; // nested in a cut one
        }
        out.push_str(&text[at..s]);
        out.push(';');
        out.extend(text[s..e].chars().filter(|&c| c == '\n'));
        at = e;
    }
    out.push_str(&text[at..]);
    out
}

/// `text` with `include_str!` / `include_bytes!` replaced by their literals
/// (line breaks kept), so a relocated interface reads no file.
fn inline_includes(text: &str, file: &Path) -> String {
    let (tokens, _) = nabic_lexer::Lexer::new(text, String::new()).lex_all();
    let file = file.to_string_lossy();
    let mut out = String::new();
    let mut at = 0;
    for w in tokens.windows(5) {
        let (K::Ident(name), K::Bang, K::LParen, K::StrLit(lit), K::RParen) =
            (&w[0].kind, &w[1].kind, &w[2].kind, &w[3].kind, &w[4].kind)
        else {
            continue;
        };
        let bytes = match name.as_str() {
            "include_bytes" => true,
            "include_str" => false,
            _ => continue,
        };
        let (s, e) = (w[0].span.start as usize, w[4].span.end as usize);
        let lit = match nabic_parser::parser::include_file(&file, lit, bytes) {
            Ok(nabic_ast::Literal::ByteStr(b)) => format!("b\"{b}\""),
            Ok(nabic_ast::Literal::Str(b)) => format!("\"{b}\""),
            _ => continue,
        };
        out.push_str(&text[at..s]);
        out.push_str(&lit);
        out.extend(text[s..e].chars().filter(|&c| c == '\n'));
        at = e;
    }
    out.push_str(&text[at..]);
    out
}

/// Interface files of package `pkg` rooted in `dir`, concrete bodies cut
/// to `;`, with its `any` seeds appended to their modules.
#[must_use]
pub fn interface(
    sources: &[SourceFile],
    files: &SourceMap,
    pkg: &str,
    dir: &Path,
    seeds: &[(String, String)],
) -> Vec<(String, String)> {
    let texts = files.files();
    let mut out: Vec<(String, String)> = sources
        .iter()
        .filter(|f| f.package == pkg)
        .filter_map(|f| {
            let key = f.path.display().to_string();
            let (_, text) = texts.iter().rev().find(|(p, _)| *p == key)?;
            let rel = f.path.strip_prefix(dir).unwrap_or(&f.path);
            let rel: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let mut items = f.items.clone();
            // Inline `mod`s are in `items`; file `mod`s are their own files.
            items.retain(|i| !matches!(i, Item::Mod(m) if m.items.is_none()));
            let mut text = inline_includes(&strip(text, f.file_id, &items), &f.path);
            let module = f.module.join("::");
            let root = f.module.len() <= 1;
            for (m, seed) in seeds {
                // A seed for an inline module (no file) goes to the root.
                let has_file = sources
                    .iter()
                    .any(|g| g.package == pkg && g.module.join("::") == *m);
                if *m == module || (root && !has_file) {
                    if !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(seed);
                }
            }
            Some((rel.join("/"), text))
        })
        .collect();
    out.sort();
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// `nabi inspect` of a `.nabihir` (or the `lib<name>.a`
/// beside it): build meta, exports, generic items, archive members.
pub fn inspect(path: &Path) -> Result<String, String> {
    let (nabihir_path, archive) = companions(path);
    let r = Nabihir::decode(
        &std::fs::read(&nabihir_path).map_err(|e| format!("{}: {e}", nabihir_path.display()))?,
    )?;
    let mut s = format!(
        "Package: {}\nHash: {}\nnabic version: {}\nNABIHIR version: {VERSION}\nGC type ids: {}..{}\n",
        r.name,
        r.hash,
        r.producer.strip_prefix("nabic ").unwrap_or(&r.producer),
        r.type_offset,
        r.type_end()
    );
    let (mut exports, mut generic) = (Vec::new(), Vec::new());
    for (_, text) in r
        .files
        .iter()
        .filter(|(p, _)| Path::new(p).extension().is_some_and(|e| e == "nabi"))
    {
        signatures(text, &mut exports, &mut generic);
    }
    s.push_str("\nExports:\n");
    exports.iter().for_each(|e| s.push_str(&format!("  {e}\n")));
    s.push_str("\nGeneric items (NABIHIR):\n");
    generic.iter().for_each(|e| s.push_str(&format!("  {e}\n")));
    s.push_str("\nConcrete .o files:\n");
    match std::fs::read(&archive) {
        Ok(a) => {
            for (name, size) in ar_members(&a) {
                s.push_str(&format!("  {name} ({} KB)\n", size.div_ceil(1024)));
            }
        }
        Err(e) => s.push_str(&format!("  ({}: {e})\n", archive.display())),
    }
    Ok(s)
}

/// The WIT files package `pkg`'s `ffi::wit` items read,
/// by path relative to its source dir `dir`.
#[must_use]
pub fn wit_files(w: &nabic_mir::WitLink, pkg: &str, dir: &Path) -> Vec<(String, String)> {
    // The disk's files are canonical paths.
    let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_owned());
    w.sources
        .iter()
        .filter(|s| s.pkg == pkg)
        .flat_map(|s| &s.files)
        .map(|(path, text)| {
            let base = if path.starts_with(dir) { dir } else { &canon };
            (relative(path, base), text.clone())
        })
        .collect()
}

/// `path` from `base`, `/`-separated (`..` where it leaves `base`).
fn relative(path: &Path, base: &Path) -> String {
    let (p, b): (Vec<_>, Vec<_>) = (path.components().collect(), base.components().collect());
    let common = p.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let up = std::iter::repeat_n("..".to_owned(), b.len() - common);
    let down = p[common..]
        .iter()
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    up.chain(down).collect::<Vec<_>>().join("/")
}

/// `(x.nabihir, libx.a)` from either of them.
#[must_use]
pub fn companions(path: &Path) -> (PathBuf, PathBuf) {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let name = if path.extension().is_some_and(|e| e == "a") {
        stem.strip_prefix("lib").unwrap_or(stem)
    } else {
        stem
    };
    (
        path.with_file_name(format!("{name}.nabihir")),
        path.with_file_name(format!("lib{name}.a")),
    )
}

/// `pub` items of an interface file, one line each (methods as
/// `Type::fn ..`), generic ones apart.
// Line-level; enough for formatted source.
fn signatures(text: &str, exports: &mut Vec<String>, generic: &mut Vec<String>) {
    let mut impl_of: Option<(String, bool)> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("impl") {
            let generic_impl = rest.starts_with('<');
            let target = rest.split(" for ").last().unwrap_or(rest);
            let target = target.split('{').next().unwrap_or(target).trim();
            let target = if generic_impl {
                target
                    .trim_start_matches(|c| c != '>')
                    .trim_start_matches('>')
                    .trim()
            } else {
                target
            };
            impl_of = Some((target.to_owned(), generic_impl));
            continue;
        }
        if line == "}" {
            impl_of = None;
        }
        let Some(rest) = line.trim_start().strip_prefix("pub ") else {
            continue;
        };
        let head = rest.split(['{', ';']).next().unwrap_or(rest).trim_end();
        let in_generic = impl_of.as_ref().is_some_and(|(_, g)| *g);
        let sig = match (&impl_of, line.starts_with(' ')) {
            (Some((target, _)), true) => {
                format!("pub {}", head.replacen("fn ", &format!("fn {target}::"), 1))
            }
            _ => format!("pub {head}"),
        };
        if in_generic || head.split('(').next().unwrap_or(head).contains('<') {
            generic.push(sig);
        } else {
            exports.push(sig);
        }
    }
}

/// Member names and sizes of a `!<arch>` archive (BSD long names too).
fn ar_members(a: &[u8]) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut pos = 8;
    while pos + 60 <= a.len() && a.starts_with(b"!<arch>\n") {
        let h = &a[pos..pos + 60];
        let field = |r: std::ops::Range<usize>| String::from_utf8_lossy(&h[r]).trim().to_owned();
        let size: usize = field(48..58).parse().unwrap_or(0);
        let mut name = field(0..16);
        let mut data = size;
        if let Some(n) = name
            .strip_prefix("#1/")
            .and_then(|n| n.parse::<usize>().ok())
        {
            let body = a.get(pos + 60..pos + 60 + n).unwrap_or_default();
            String::from_utf8_lossy(body)
                .trim_end_matches('\0')
                .clone_into(&mut name);
            data = size.saturating_sub(n);
        }
        let name = name.trim_end_matches('/').to_owned();
        if !name.is_empty() && !name.starts_with("__.SYMDEF") {
            out.push((name, data));
        }
        pos += 60 + size + (size & 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips() {
        for v in [0, 1, 127, 128, 16383, 16384, 1 << 40, u64::MAX] {
            let mut b = Vec::new();
            put_varint(&mut b, v);
            assert_eq!(Reader { b: &b, pos: 0 }.varint().unwrap(), v, "{v}");
        }
        let mut b = Vec::new();
        put_varint(&mut b, 300);
        assert_eq!(b, [0x81, 0x2C]); // 10|000001 00101100
    }

    #[test]
    fn nabihir_round_trips_and_rejects_other_versions() {
        let r = Nabihir {
            producer: "p".into(),
            name: "dep".into(),
            hash: "0123abcd".into(),
            src_dir: "/x/dep".into(),
            type_offset: 3,
            type_count: 4,
            features: vec!["json".into(), "tls".into()],
            deps: vec!["base".into()],
            files: vec![("lib.nabi".into(), "pub fn f() -> i32;\n".into())],
            summaries: vec![("dep::f".into(), "g\t0\tmutex\t3\tf\tlib.nabi\n".into())],
            facts: vec![
                (
                    "dep::f".into(),
                    Summary {
                        flags: Summary::READONLY | Summary::COLD,
                        keeps_none: 0b101,
                        returns: 0b100,
                        ..Summary::default()
                    },
                ),
                (
                    "dep::g".into(),
                    Summary {
                        flags: Summary::QUIET,
                        ..Summary::default()
                    },
                ),
            ],
        };
        let mut b = r.encode();
        let back = Nabihir::decode(&b).unwrap();
        assert_eq!(back.text(), r.text());
        assert_eq!(back.facts, r.facts);
        assert_eq!(back.fact("dep::g"), Some(r.facts[1].1));
        assert_eq!(back.fact("dep::h"), None);
        assert!(
            r.text()
                .contains("--- calls\ndep::f readonly cold keeps-none:0x5 returns:0x4\n")
        );
        b[4] = 99;
        assert!(Nabihir::decode(&b).unwrap_err().contains("version 99"));
    }
}
