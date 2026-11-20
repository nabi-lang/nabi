//! Doc tests: the ```` ```nabi ```` blocks in a library's `///` and `//!`
//! docs and in its README.md, built as code that uses the library (a
//! dependency under the package's name). The blocks that run share one
//! harness executable, one module per block; each `compile_fail` block is
//! checked on its own at build time.
//!
//! A block's file lays its code on the lines it has in the original, so
//! a diagnostic's line is the line in the documented file.

#[allow(clippy::wildcard_imports)]
use crate::*;
use nabic_harness::test::{Case, Settled};

/// The fence words after `nabi`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attrs {
    /// `norun`: built, never run.
    pub norun: bool,
    /// `ignore`: neither built nor run.
    pub ignore: bool,
    /// `should_panic`: passes only on a panic.
    pub should_panic: bool,
    /// `compile_fail`: passes only when it does not build; the error
    /// codes it must give (`compile_fail,E0008`).
    pub compile_fail: Option<Vec<String>>,
    /// Words that mean nothing here.
    pub unknown: Vec<String>,
}

/// A doc test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    /// Where it is, relative to the package root (`src/stats.nabi`).
    pub file: String,
    /// Its opening fence's line, from 1.
    pub line: usize,
    /// The item its doc comment documents, when known (`Stats::mean`).
    pub item: Option<String>,
    pub attrs: Attrs,
    /// Its lines without the comment marks; hidden lines (`# `) shown.
    pub code: Vec<String>,
    /// The `#[cfg(..)]` lines on the item it documents.
    pub cfg: Vec<String>,
}

impl Block {
    /// The case name: `doc src/stats.nabi:12 (mean)`.
    pub fn name(&self) -> String {
        match &self.item {
            Some(i) => format!("doc {}:{} ({i})", self.file, self.line),
            None => format!("doc {}:{}", self.file, self.line),
        }
    }
}

/// The fence words of a doc test after ```` ``` ````; `None` when the
/// block is not one. A bare fence is Nabi when `bare_is_nabi` (in doc
/// comments, which render it as Nabi).
pub fn fence(info: &str, bare_is_nabi: bool) -> Option<Attrs> {
    let mut words = info.split([',', ' ', '\t']).filter(|w| !w.is_empty());
    match words.next() {
        None if bare_is_nabi => {}
        Some("nabi") => {}
        _ => return None,
    }
    let mut a = Attrs::default();
    let mut codes = Vec::new();
    for w in words {
        match w {
            "norun" => a.norun = true,
            "ignore" => a.ignore = true,
            "should_panic" => a.should_panic = true,
            "compile_fail" => a.compile_fail = Some(Vec::new()),
            c if c.len() == 5
                && c.starts_with('E')
                && c[1..].bytes().all(|b| b.is_ascii_digit()) =>
            {
                codes.push(c.to_owned());
            }
            w => a.unknown.push(w.to_owned()),
        }
    }
    match &mut a.compile_fail {
        Some(c) => *c = codes,
        None => a.unknown.extend(codes),
    }
    Some(a)
}

/// A hidden line (`# code`, or a lone `#`) shown; others as they are.
fn unhide(line: &str) -> &str {
    let t = line.trim_start();
    if t == "#" {
        ""
    } else if let Some(rest) = t.strip_prefix("# ") {
        rest
    } else {
        line
    }
}

/// The text of a `///` or `//!` line, without the mark and one space,
/// and whether it is an outer (`///`) doc.
fn doc_text(line: &str) -> Option<(&str, bool)> {
    let t = line.trim_start();
    let (rest, outer) = if let Some(r) = t.strip_prefix("///") {
        if r.starts_with('/') {
            return None;
        }
        (r, true)
    } else {
        (t.strip_prefix("//!")?, false)
    };
    Some((rest.strip_prefix(' ').unwrap_or(rest), outer))
}

/// The name an item line declares: `pub fn mean<T>(` -> `mean`.
fn item_name(line: &str) -> Option<String> {
    let mut words = line
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '"'))
        .filter(|w| !w.is_empty());
    while let Some(w) = words.next() {
        match w {
            "pub" | "pkg" | "unsafe" | "async" | "extern" | "default" => {}
            w if w.starts_with('"') => {}
            "const" | "fn" | "struct" | "enum" | "trait" | "type" | "static" | "union" | "mod" => {
                let name = words.next()?;
                if w == "const" && name == "fn" {
                    return words.next().map(str::to_owned);
                }
                return Some(name.to_owned());
            }
            _ => return None,
        }
    }
    None
}

/// The type an `impl` line is for: `impl<T> Show for Stack<T> {` -> `Stack`.
fn impl_type(line: &str) -> Option<String> {
    let rest = line.strip_prefix("impl")?;
    if !rest.starts_with([' ', '<']) {
        return None;
    }
    // Drop the generic parameters right after `impl`.
    let mut rest = rest.trim_start();
    if rest.starts_with('<') {
        let mut depth = 0;
        for (i, c) in rest.char_indices() {
            match c {
                '<' => depth += 1,
                '>' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                rest = &rest[i + 1..];
                break;
            }
        }
    }
    let ty = rest.rsplit_once(" for ").map_or(rest, |(_, t)| t);
    let name: String = ty
        .trim_start()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':')
        .collect();
    (!name.is_empty()).then_some(name)
}

enum Fence {
    None,
    Test(Block, bool),
    Other,
}

/// The doc tests of the Nabi source `text` of `file`.
pub fn in_source(file: &str, text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut fence_at = Fence::None;
    // Blocks of the `///` run that are waiting for the item below it.
    let mut pending: Vec<Block> = Vec::new();
    let mut in_impl: Option<String> = None;
    for (n, line) in text.lines().enumerate() {
        let Some((doc, outer)) = doc_text(line) else {
            // An open fence ends with its comment.
            fence_at = Fence::None;
            let t = line.trim();
            if !line.starts_with([' ', '\t']) {
                if let Some(ty) = impl_type(t) {
                    in_impl = Some(ty);
                } else if t == "}" {
                    in_impl = None;
                }
            }
            if !pending.is_empty() && t.starts_with("#[cfg(") {
                for b in &mut pending {
                    b.cfg.push(t.to_owned());
                }
            }
            if pending.is_empty() || t.is_empty() || t.starts_with("#[") || t.starts_with("//") {
                continue;
            }
            let name = item_name(t).map(|i| match &in_impl {
                Some(ty) if line.starts_with([' ', '\t']) => format!("{ty}::{i}"),
                _ => i,
            });
            for mut b in std::mem::take(&mut pending) {
                b.item.clone_from(&name);
                out.push(b);
            }
            continue;
        };
        let opens = doc.trim_start().strip_prefix("```");
        // Only a bare fence closes one.
        let closes = opens.is_some_and(|i| i.trim().is_empty());
        match (&mut fence_at, opens) {
            (Fence::Test(..), Some(_)) if closes => {
                if let Fence::Test(b, outer) = std::mem::replace(&mut fence_at, Fence::None) {
                    if outer {
                        pending.push(b);
                    } else {
                        out.push(b);
                    }
                }
            }
            (Fence::Test(b, _), _) => b.code.push(unhide(doc).to_owned()),
            (Fence::Other, Some(_)) if closes => fence_at = Fence::None,
            (Fence::Other, _) | (Fence::None, None) => {}
            (Fence::None, Some(info)) => {
                fence_at = match fence(info, true) {
                    Some(attrs) => Fence::Test(
                        Block {
                            file: file.to_owned(),
                            line: n + 1,
                            item: None,
                            attrs,
                            code: Vec::new(),
                            cfg: Vec::new(),
                        },
                        outer,
                    ),
                    None => Fence::Other,
                };
            }
        }
    }
    out.append(&mut pending);
    out
}

/// The doc tests of the markdown `text` of `file`: ```` ```nabi ```` only.
pub fn in_markdown(file: &str, text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut fence_at = Fence::None;
    for (n, line) in text.lines().enumerate() {
        let opens = line.trim_start().strip_prefix("```");
        let closes = opens.is_some_and(|i| i.trim().is_empty());
        match (&mut fence_at, opens) {
            (Fence::Test(..), Some(_)) if closes => {
                if let Fence::Test(b, _) = std::mem::replace(&mut fence_at, Fence::None) {
                    out.push(b);
                }
            }
            (Fence::Test(b, _), _) => b.code.push(unhide(line).to_owned()),
            (Fence::Other, Some(_)) if closes => fence_at = Fence::None,
            (Fence::Other, _) | (Fence::None, None) => {}
            (Fence::None, Some(info)) => {
                fence_at = match fence(info, false) {
                    Some(attrs) => Fence::Test(
                        Block {
                            file: file.to_owned(),
                            line: n + 1,
                            item: None,
                            attrs,
                            code: Vec::new(),
                            cfg: Vec::new(),
                        },
                        false,
                    ),
                    None => Fence::Other,
                };
            }
        }
    }
    out
}

/// Whether library file `f` is in the build: each `mod` from the root
/// file down to it is declared, and its `#[cfg(..)]` holds. A file the
/// walk cannot place (`#[path]`, an inline module) counts as in.
fn in_build(src: &Path, root: &Path, f: &Path, env: &nabic_interface::CfgEnv) -> bool {
    let Ok(rel) = f
        .with_extension("")
        .strip_prefix(src)
        .map(Path::to_path_buf)
    else {
        return true;
    };
    let names: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let mut parent = root.to_path_buf();
    for (i, name) in names.iter().enumerate() {
        let Ok(text) = std::fs::read_to_string(&parent) else {
            return true;
        };
        let Some(attrs) = mod_attrs(&text, name) else {
            return true;
        };
        if !nabic_interface::attr_lines_pass(&attrs, env) {
            return false;
        }
        parent = src.join(names[..=i].join("/")).with_extension("nabi");
    }
    true
}

/// The `#[cfg(..)]` lines on the top-level `mod name;` of `text`; `None`
/// when it declares none.
fn mod_attrs(text: &str, name: &str) -> Option<Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|l| {
        let t = l
            .trim_start_matches("pub(crate) ")
            .trim_start_matches("pub ");
        t.strip_prefix("mod ")
            .and_then(|r| r.trim().strip_suffix(';'))
            .is_some_and(|n| n.trim() == name)
    })?;
    let attrs = lines[..at]
        .iter()
        .rev()
        .map(|l| l.trim())
        .take_while(|l| l.starts_with("#[") || l.starts_with("///") || l.is_empty())
        .filter(|l| l.starts_with("#[cfg("))
        .map(str::to_owned)
        .collect();
    Some(attrs)
}

/// The doc tests of library `lib` (its source `files` and README.md), in
/// file order; with `env`, only those of modules and items in the build.
pub fn collect(
    lib: &Package,
    files: &[PathBuf],
    env: Option<&nabic_interface::CfgEnv>,
) -> Vec<Block> {
    let abs = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let dir = abs(&lib.dir);
    let src = dir.join("src");
    // An executable's root is not the library's.
    let exe_root = src.join("main.nabi");
    // The root is read before the files the compile lists.
    let mut files: Vec<PathBuf> = files
        .iter()
        .chain([&lib.root])
        .map(|f| abs(f))
        .filter(|f| {
            f.starts_with(&src) && *f != exe_root && f.extension().is_some_and(|x| x == "nabi")
        })
        .collect();
    files.sort();
    files.dedup();
    let rel = |p: &Path| {
        p.strip_prefix(&dir)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let root = abs(&lib.root);
    let mut out = Vec::new();
    for f in &files {
        if env.is_some_and(|e| *f != root && !in_build(&src, &root, f, e)) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(f) {
            let blocks = in_source(&rel(f), &text);
            out.extend(
                blocks
                    .into_iter()
                    .filter(|b| env.is_none_or(|e| nabic_interface::attr_lines_pass(&b.cfg, e))),
            );
        }
    }
    let readme = lib.dir.join("README.md");
    if let Ok(text) = std::fs::read_to_string(&readme) {
        out.extend(in_markdown("README.md", &text));
    }
    out
}

/// Where the body of `code` starts: past its leading `use` items (which
/// stay at module level, as macro imports must), blank lines and
/// comments.
fn body_start(code: &[String]) -> usize {
    let mut in_use = false;
    for (i, l) in code.iter().enumerate() {
        let t = l.trim();
        if in_use {
            in_use = !t.ends_with(';');
            continue;
        }
        if t.starts_with("use ") || t.starts_with("pub use ") {
            in_use = !t.ends_with(';');
            continue;
        }
        if !(t.is_empty() || t.starts_with("//")) {
            return i;
        }
    }
    code.len()
}

/// `b`'s code as a file of its own, each line where it is in the
/// documented file: `head` on the fence's line, `open` (the test
/// function's start) before the first line of the body, then `tail`.
fn lay_out(b: &Block, head: &str, open: &str, tail: &str) -> String {
    let mut s = if b.line > 1 {
        format!("// {}:{}\n", b.file, b.line)
    } else {
        String::new()
    };
    for _ in 2..b.line {
        s.push('\n');
    }
    s.push_str(head);
    s.push('\n');
    let start = body_start(&b.code);
    for (i, l) in b.code.iter().enumerate() {
        if i == start {
            s.push_str(open);
        }
        s.push_str(l);
        s.push('\n');
    }
    if start == b.code.len() {
        s.push_str(open);
        s.push('\n');
    }
    s.push_str(tail);
    s
}

/// The statement calling a `fn main` the block declares, if any.
fn main_call(b: &Block) -> &'static str {
    match b.code.iter().find(|l| l.starts_with("fn main(")) {
        Some(l) if l.contains("throws") => "    try main();\n",
        Some(_) => "    main();\n",
        None => "",
    }
}

/// Build the doc tests of library `lib` (its source `files`) into the
/// harness directory; the executable (`None`: nothing to run) and its
/// cases. Warnings in the blocks are not shown; an error ends the build.
pub fn build(a: &mut Args, lib: &Package, files: &[PathBuf]) -> Option<(PathBuf, Vec<Case>)> {
    let blocks = collect(
        lib,
        files,
        Some(&nabic_interface::target_env(a.spec(), &a.cfg)),
    );
    if blocks.is_empty() {
        return None;
    }
    for b in &blocks {
        if !b.attrs.unknown.is_empty() {
            eprintln!(
                "{} {}: unknown doc test word{} `{}`; after `nabi` write `norun`, `ignore`, \
                 `should_panic` or `compile_fail` (with error codes: `compile_fail,E0008`)",
                term::label(Stream::Stderr, Role::Warning),
                b.name(),
                if b.attrs.unknown.len() == 1 { "" } else { "s" },
                b.attrs.unknown.join("`, `"),
            );
        }
    }
    let dir = out_dir(a, Some(lib)).join("harness").join("doc");
    let _ = std::fs::remove_dir_all(&dir);
    create_dir(&dir);
    // The library, as the blocks' dependency under its name.
    let archive = build_lib(a, lib, None);
    a.externs.push((
        lib_name(lib),
        nabic_interface::nabihir::companions(&archive).0,
    ));
    // Each block sees the library's public items by their names.
    let glob = format!("use {}::*;", lib_name(lib));
    let mut cases = Vec::new();
    // The blocks built into the harness: case index, module line, file.
    let mut built: Vec<(usize, String, PathBuf)> = Vec::new();
    for (k, b) in blocks.iter().enumerate() {
        let mut case = Case::new(b.name());
        // `src/stats.nabi:12` -> `src/stats-12.nabi`: never a package
        // root's name, so the file stays a module of the harness.
        let stem = b.file.strip_suffix(".nabi").unwrap_or(&b.file);
        let stem = stem.strip_suffix(".md").unwrap_or(stem);
        let file = PathBuf::from(format!("{stem}-{}.nabi", b.line));
        let path = dir.join(&file);
        if let Some(p) = path.parent() {
            create_dir(p);
        }
        if b.attrs.ignore {
            let why = "an `ignore` doc block is not built".to_owned();
            case.ignore = Some(why.clone());
            case.outcome = Some(Settled::Ignored(why));
        } else if let Some(codes) = &b.attrs.compile_fail {
            write_file(&path, &lay_out(b, &glob, "", ""));
            case.outcome = Some(compile_fail(a, &path, codes));
        } else {
            let body = lay_out(
                b,
                &glob,
                "#[test] fn run() throws { ",
                &format!("{}}}\n", main_call(b)),
            );
            write_file(&path, &body);
            let line = format!(
                "#[path(\"{}\")]\nmod b{k};\n",
                file.to_string_lossy().replace('\\', "/")
            );
            built.push((k, line, Path::new("doc").join(&file)));
            case.call = Some(format!("b{k}::run"));
            if b.attrs.norun {
                case.outcome = Some(Settled::Passed);
            }
            if b.attrs.should_panic {
                case.panics = Some(String::new());
            }
        }
        cases.push(case);
    }
    let main = dir.join("main.nabi");
    let pkg = Package {
        root: main.clone(),
        dir: dir.clone(),
        name: format!("{}-doc", lib.name),
        lib: false,
        anonymous: true,
        library: None,
    };
    let t = Instant::now();
    if !a.quiet {
        term::status(
            "Compiling",
            &format!("{} doc tests ({} blocks)", lib.name, blocks.len()),
        );
    }
    // A block that does not build fails as its own case; the rest build
    // again without it.
    let result = loop {
        let mut root = format!("// The doc tests of {}, built by `nabic test`.\n", lib.name);
        for (_, line, _) in &built {
            root.push_str(line);
        }
        write_file(&main, &root);
        let mut session = session(a, &pkg);
        session.set_test(true);
        session.set_keep_ast(true);
        let result = session.compile_file(&pkg.root);
        if !result.has_errors() {
            break result;
        }
        let errors: Vec<_> = result.errors.iter().filter(|d| d.is_error()).collect();
        let block_of = |d: &nabic_interface::NabiDiagnostic| {
            let span = d.primary.as_ref()?.span;
            let path = d
                .file
                .clone()
                .or_else(|| result.source_map.path(span.file).map(str::to_owned))?;
            built
                .iter()
                .position(|(_, _, f)| Path::new(&path).ends_with(f))
        };
        let owner: Vec<Option<usize>> = errors.iter().map(|d| block_of(d)).collect();
        let mut bad: Vec<usize> = owner.iter().flatten().copied().collect();
        bad.sort_unstable();
        bad.dedup();
        if bad.is_empty() {
            // Not in a block: the harness itself, or the library.
            result.report_errors();
            eprintln!(
                "{} the doc tests of {} do not build ({})",
                term::label(Stream::Stderr, Role::Error),
                lib.name,
                counts(errors.len(), 0),
            );
            process::exit(1);
        }
        for &i in bad.iter().rev() {
            let text: String = errors
                .iter()
                .zip(&owner)
                .filter(|(_, o)| **o == Some(i))
                .map(|(d, _)| result.render(d))
                .collect();
            let (k, _, _) = built.remove(i);
            cases[k].outcome = Some(Settled::Failed(format!(
                "the doc block does not build; its lines are the documented file's:\n{}",
                text.trim_end()
            )));
            cases[k].call = None;
        }
    };
    stage(a, "doc tests front end", t);
    let runs = built.iter().any(|(k, _, _)| cases[*k].outcome.is_none());
    let exe = runs.then(|| {
        let exe = dir.join(exe_file(a, &format!("{}-doc", lib.name)));
        link_exe(a, &pkg, &result, &exe, &format!("{}-doc", lib.name));
        exe
    });
    Some((exe.unwrap_or_default(), cases))
}

/// Whether the block at `path` fails to build, with each of `codes`.
fn compile_fail(a: &Args, path: &Path, codes: &[String]) -> Settled {
    let pkg = Package {
        root: path.to_path_buf(),
        dir: path.parent().map_or_else(PathBuf::new, Path::to_path_buf),
        name: "doc".to_owned(),
        lib: false,
        anonymous: true,
        library: None,
    };
    let mut session = session(a, &pkg);
    let (_, diags) =
        nabic_tools::analysis::check(&mut session, None, &pkg.root, &a.lint_levels, None);
    let got: Vec<String> = diags
        .iter()
        .filter(|d| d.is_error())
        .map(|d| d.code.to_string())
        .collect();
    if got.is_empty() {
        return Settled::Failed("a `compile_fail` doc block built without errors".to_owned());
    }
    let missing: Vec<&str> = codes
        .iter()
        .filter(|c| !got.contains(c))
        .map(String::as_str)
        .collect();
    if missing.is_empty() {
        Settled::Passed
    } else {
        Settled::Failed(format!(
            "it did not build, but not with {} (it gave {})",
            missing.join(", "),
            got.join(", ")
        ))
    }
}

fn write_file(path: &Path, text: &str) {
    if let Err(e) = std::fs::write(path, text) {
        fail(&format!("{}: {e}", path.display()));
    }
}

#[cfg(test)]
mod tests {
    use super::{Attrs, fence, in_markdown, in_source};

    #[test]
    fn fence_words() {
        assert_eq!(fence("nabi", false), Some(Attrs::default()));
        assert_eq!(fence("", true), Some(Attrs::default()));
        assert_eq!(fence("", false), None);
        assert_eq!(fence("text", true), None);
        assert_eq!(fence("nabion", true), None);
        let a = fence("nabi,norun", true).unwrap();
        assert!(a.norun);
        let a = fence("nabi norun should_panic", true).unwrap();
        assert!(a.norun && a.should_panic);
        let a = fence("nabi,compile_fail,E0008", true).unwrap();
        assert_eq!(a.compile_fail, Some(vec!["E0008".to_owned()]));
        let a = fence("nabi,no_run,E0008", true).unwrap();
        assert_eq!(a.unknown, ["no_run", "E0008"]);
    }

    #[test]
    fn source_blocks_and_items() {
        let src = "//! ```nabi\n//! top();\n//! ```\n\n/// Adds.\n///\n/// ```\n/// # let a = 1;\n\
                   /// add(a, 2);\n/// ```\n///\n/// ```text\n/// ```nabi\n/// ```\n#[inline]\n\
                   pub fn add(a: i32, b: i32) -> i32 { a + b }\n\nimpl<T> Show for Stack<T> {\n    \
                   /// ```nabi,ignore\n    /// x\n    /// ```\n    pub fn show(self) {}\n}\n";
        let bs = in_source("src/lib.nabi", src);
        assert_eq!(bs.len(), 3, "{bs:?}");
        assert_eq!((bs[0].line, bs[0].item.as_deref()), (1, None));
        assert_eq!(bs[0].code, ["top();"]);
        assert_eq!((bs[1].line, bs[1].item.as_deref()), (7, Some("add")));
        assert_eq!(bs[1].code, ["let a = 1;", "add(a, 2);"]);
        assert_eq!(bs[2].item.as_deref(), Some("Stack::show"));
        assert!(bs[2].attrs.ignore);
        assert_eq!(bs[1].name(), "doc src/lib.nabi:7 (add)");
    }

    #[test]
    fn markdown_blocks() {
        let md = "# x\n\n```\nnot nabi\n```\n\n```nabi\nprintln(1);\n```\n\n```nabion\nP {}\n```\n";
        let bs = in_markdown("README.md", md);
        assert_eq!(bs.len(), 1);
        assert_eq!(
            (bs[0].line, bs[0].code.as_slice()),
            (7, ["println(1);".to_owned()].as_slice())
        );
        assert_eq!(bs[0].name(), "doc README.md:7");
    }
}
