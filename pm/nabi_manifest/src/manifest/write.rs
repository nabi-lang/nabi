//! Edits of manifest, install-list and script-block text.

#[allow(clippy::wildcard_imports)]
use crate::manifest::*;

/// `text` with `field` (`deps` / `dev_deps`) entry `key` set to the
/// NABION text `value`; the rest of the text (comments too) as it was.
pub fn set_dep(
    text: &str,
    file: &str,
    field: &str,
    key: &str,
    value: &str,
) -> Result<String, String> {
    parse(text, file)?;
    let out = edit::set(text, &[Step::Field(field), Step::Key(key)], value)
        .map_err(|e| e.render(text, file))?;
    parse(&out, file).map(|_| out)
}

/// `text` with `dep` in `field`; an optional one also gets a feature of
/// its name enabling it, unless the package has one.
pub fn add_dep(text: &str, file: &str, field: &str, dep: &Dep) -> Result<String, String> {
    let m = parse(text, file)?;
    let edit = |text: &str, steps: &[Step<'_>], value: &str| {
        edit::set(text, steps, value).map_err(|e| e.render(text, file))
    };
    let mut out = text.to_owned();
    if dep.optional && !m.features.iter().any(|(f, _)| *f == dep.name) {
        let enable = strings(&[format!("dep:{}", dep.name)]);
        let steps = [Step::Field("features"), Step::Key(&dep.name)];
        out = edit(&out, &steps, &nabic_nabion::to_line(&enable))?;
    }
    out = edit(
        &out,
        &[Step::Field(field), Step::Key(&dep.name)],
        &dep.to_nabi(),
    )?;
    parse(&out, file).map(|_| out)
}

/// `text` without `field` entry `key`, or `None` when it has none.
pub fn remove_dep(
    text: &str,
    file: &str,
    field: &str,
    key: &str,
) -> Result<Option<String>, String> {
    parse(text, file)?;
    edit::remove(text, &[Step::Field(field), Step::Key(key)]).map_err(|e| e.render(text, file))
}

/// `$NABI_HOME/installs.nabion` (a `Package` with `deps`): its entries, and
/// what it holds that this nabi skipped.
///
/// Every toolchain shares the file, so fields, arguments and entry kinds a
/// newer nabi wrote are skipped.
pub fn parse_installs(text: &str, file: &str) -> Result<(Vec<Dep>, Vec<String>), String> {
    let doc = Doc::parse(text, file)?;
    let pkg = doc.root().record("Package")?;
    let mut skipped: Vec<String> = pkg
        .known(&["deps"])
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut out = Vec::new();
    for (k, v) in pkg
        .opt("deps")
        .map(|d| d.dict())
        .transpose()?
        .unwrap_or_default()
    {
        let name = k.string()?;
        let kind = v.variant("Dep").ok().map(|v| v.name);
        if kind.is_some_and(|k| !["path", "git", "version"].contains(&k)) {
            skipped.push(format!("deps[\"{name}\"]"));
            continue;
        }
        let mut dep = dep(v, true, true)?;
        dep.name = name;
        out.push(dep);
    }
    Ok((out, skipped))
}

/// `installs.nabion` text with `deps` entry `dep.name` set to `dep`, the
/// rest (a newer nabi's fields too) as it was.
pub fn set_install(text: &str, file: &str, dep: &Dep) -> Result<String, String> {
    parse_installs(text, file)?;
    let out = edit::set(
        text,
        &[Step::Field("deps"), Step::Key(&dep.name)],
        &dep.to_nabi(),
    )
    .map_err(|e| e.render(text, file))?;
    parse_installs(&out, file).map(|_| out)
}

/// `installs.nabion` text without entry `name`, or `None` when it has none.
pub fn remove_install(text: &str, file: &str, name: &str) -> Result<Option<String>, String> {
    parse_installs(text, file)?;
    edit::remove(text, &[Step::Field("deps"), Step::Key(name)]).map_err(|e| e.render(text, file))
}

/// A script's `//! ```nabion` block.
pub struct Block {
    /// A `nabi.nabion` value, without the `//!` prefixes.
    pub text: String,
    /// The bytes of the file its lines take, fences included.
    pub span: std::ops::Range<usize>,
}

/// Leading `//!` lines after a shebang: (start, end incl. newline, text).
pub(crate) fn doc_lines(src: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    let start = if nabic_core::shebang_len(src) > 0 {
        src.find('\n').map_or(src.len(), |i| i + 1)
    } else {
        0
    };
    let mut at = start;
    src[start..].split_inclusive('\n').map_while(move |line| {
        let rest = line.trim_end_matches(['\n', '\r']).strip_prefix("//!")?;
        let from = at;
        at += line.len();
        Some((from, at, rest.strip_prefix(' ').unwrap_or(rest)))
    })
}

/// A script's block of leading `//!` lines from a ```` ```nabion ````
/// line to a ```` ``` ```` line.
pub fn script_block(src: &str) -> Option<Block> {
    let mut open = None;
    let mut text = String::new();
    for (from, to, l) in doc_lines(src) {
        match open {
            None if l.trim() == "```nabion" => open = Some(from),
            Some(o) if l.trim() == "```" => {
                return Some(Block { text, span: o..to });
            }
            Some(_) => {
                text.push_str(l);
                text.push('\n');
            }
            None => {}
        }
    }
    None
}

/// `src` with `text` as its block, replacing the old one or placed after
/// the shebang and leading `//!` lines; `None` removes it.
pub fn set_script_block(src: &str, text: Option<&str>) -> String {
    let lines = text.map_or_else(String::new, |t| {
        let mut out = "//! ```nabion\n".to_owned();
        for l in t.lines() {
            out += &if l.is_empty() {
                "//!\n".to_owned()
            } else {
                format!("//! {l}\n")
            };
        }
        out + "//! ```\n"
    });
    if let Some(b) = script_block(src) {
        return format!("{}{lines}{}", &src[..b.span.start], &src[b.span.end..]);
    }
    let at = doc_lines(src).last().map_or_else(
        || {
            if nabic_core::shebang_len(src) > 0 {
                src.find('\n').map_or(src.len(), |i| i + 1)
            } else {
                0
            }
        },
        |(_, to, _)| to,
    );
    let gap = if at > 0 && !src[..at].ends_with('\n') {
        "\n"
    } else {
        ""
    };
    format!("{}{gap}{lines}{}", &src[..at], &src[at..])
}
