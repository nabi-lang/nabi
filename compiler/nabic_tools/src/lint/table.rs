//! The lint table, lint groups and command-line levels.

#[allow(clippy::wildcard_imports)]
use crate::lint::*;

pub struct Lint {
    pub name: &'static str,
    pub default: Level,
    pub desc: &'static str,
    /// The release that added it (`lint --list` shows it).
    pub since: &'static str,
}

pub(crate) const fn warn(name: &'static str, desc: &'static str) -> Lint {
    Lint {
        name,
        default: Level::Warn,
        desc,
        since: "0.1.0",
    }
}

/// Every lint, with its default level.
pub const LINTS: &[Lint] = &[
    warn(
        "unused_variables",
        "a local or parameter that is never read",
    ),
    warn(
        "unused_imports",
        "a `use` whose name the module never mentions",
    ),
    warn("dead_code", "a non-`pub` function nothing calls"),
    warn(
        "unreachable_code",
        "statements after `return` / `break` / `continue` / `throw` / `panic`",
    ),
    warn(
        "unused_must_use",
        "the result of a `#[must_use]` function is discarded",
    ),
    warn(
        "ignored_error",
        "a `try? f();` statement silently drops the error",
    ),
    Lint {
        name: "nan_comparison",
        default: Level::Deny,
        desc: "comparing with NaN is always false (`!=`: true)",
        since: "0.1.0",
    },
    warn(
        "bool_comparison",
        "comparing a `bool` with `true` / `false`",
    ),
    Lint {
        name: "self_assignment",
        default: Level::Deny,
        desc: "`x = x;`",
        since: "0.1.0",
    },
    warn("while_true", "`while true` instead of `loop`"),
    warn(
        "closed_type_switch",
        "an `if T is ..` chain whose fallback panics: a trait fits a closed set of types",
    ),
    warn("redundant_cast", "`x as T` where `x` already has type `T`"),
    warn("redundant_clone", "`.clone()` of a primitive value"),
    warn(
        "non_snake_case",
        "functions, locals, parameters and modules are snake_case",
    ),
    warn(
        "non_camel_case_types",
        "types, traits and variants are UpperCamelCase",
    ),
    warn(
        "non_upper_case_globals",
        "`const` / `static` names are UPPER_SNAKE_CASE",
    ),
    warn(
        "sup_path",
        "a `sup::` path or `pub(sub)` outside an inline `mod x { }`: write `pkg::..` or `pub(pkg)`",
    ),
    warn(
        "local_use",
        "a `use` inside a function body: move it to the top of the file",
    ),
    warn("confusable_idents", "identifiers that look alike"),
    warn(
        "hidden_chars",
        "an invisible or unassigned character written directly in a string or comment",
    ),
    warn(
        "deprecated",
        "use of a `#[deprecated]` function, method, field, type, constant or static",
    ),
    warn(
        "unused_doc_comments",
        "a doc comment on what is not documented (a statement, an expression, a match arm)",
    ),
    warn(
        "missing_f_prefix",
        "a plain string holds `{name}` of a local in scope: it needs `f\"..\"`",
    ),
    warn(
        "literal_spread_mismatch",
        "`f(..[a, b, c])` or `f(..{\"k\": v})` whose literal cannot fill the parameters: the call panics",
    ),
    warn(
        "deprecated_safe",
        "a call of a `#[deprecated_safe]` function outside `unsafe`",
    ),
    warn(
        "assoc_type_shadowing",
        "an associated type named like an outer type, which it hides inside its trait or impl",
    ),
    warn(
        "unused_result",
        "a statement drops a value other than `()` or `none`: use it, or write `_ = expr;`",
    ),
    warn(
        "resize_while_iterating",
        "`for x in xs` whose body pushes to or removes from `xs`: the loop sees the new length",
    ),
    warn(
        "plain_module_doc",
        "a library's root or `pub` module file that starts with `//` lines but has no `//!` doc",
    ),
    Lint {
        name: "missing_docs",
        default: Level::Allow,
        desc: "a `pub` item, method or module without a `///` doc comment",
        since: "0.1.0",
    },
];

/// A warning lint of a later release, for test tables with a newer lint.
#[cfg(test)]
pub(crate) const fn warn_since(name: &'static str, desc: &'static str) -> Lint {
    Lint {
        name,
        default: Level::Warn,
        desc,
        since: "0.2.0",
    }
}

/// Lint groups usable wherever a lint name is (`all`: every lint;
/// `warnings`: every warning-level lint, later ones included).
pub(crate) const GROUPS: &[(&str, &[&str])] = &[
    (
        "unused",
        &[
            "unused_variables",
            "unused_imports",
            "dead_code",
            "unreachable_code",
            "unused_must_use",
            "unused_result",
            "unused_doc_comments",
        ],
    ),
    (
        "nonstandard_style",
        &[
            "non_snake_case",
            "non_camel_case_types",
            "non_upper_case_globals",
            "sup_path",
            "local_use",
        ],
    ),
];

/// The lints `name` stands for (a lint, a group, `all`, `warnings`), or
/// `None` for an unknown name.
#[must_use]
pub fn expand(name: &str) -> Option<Vec<&'static str>> {
    expand_in(LINTS, name)
}

pub(crate) fn expand_in(lints: &'static [Lint], name: &str) -> Option<Vec<&'static str>> {
    match name {
        "all" => return Some(lints.iter().map(|l| l.name).collect()),
        "warnings" => {
            return Some(
                lints
                    .iter()
                    .filter(|l| l.default == Level::Warn)
                    .map(|l| l.name)
                    .collect(),
            );
        }
        _ => {}
    }
    if let Some(l) = lints.iter().find(|l| l.name == name) {
        return Some(vec![l.name]);
    }
    GROUPS
        .iter()
        .find(|(g, _)| *g == name)
        .map(|(_, ls)| ls.to_vec())
}

/// "unknown lint `name`" with a did-you-mean and where the list is.
pub(crate) fn unknown(name: &str, at: &str) -> String {
    let names = LINTS
        .iter()
        .map(|l| l.name)
        .chain(GROUPS.iter().map(|(g, _)| *g))
        .chain(["all", "warnings"]);
    format!(
        "unknown lint `{name}`{at}{} (`nabi lint --list` shows every lint and group)",
        nabic_core::cli::did_you_mean(name, names, &[])
    )
}

/// Levels set outside the source: `-A/-W/-D` in order. The first
/// `manifest.1` of them are `nabi.nabion` `lints`, from the manifest at
/// `manifest.0`.
#[derive(Clone, Debug, Default)]
pub struct Levels {
    pub flags: Vec<(Level, String)>,
    pub manifest: Option<(String, usize)>,
    /// `--check-library`: the library's own files are linted too, at
    /// the library's levels (`LIBRARY_LEVELS`) under the flags.
    pub library: bool,
}

impl From<Vec<(Level, String)>> for Levels {
    fn from(flags: Vec<(Level, String)>) -> Self {
        Self {
            flags,
            ..Self::default()
        }
    }
}

/// The library's lint levels: its manifest, were it a package.
pub(crate) const LIBRARY_LEVELS: &[(Level, &str)] = &[(Level::Deny, "missing_docs")];

/// `nabi lint --list` text.
#[must_use]
pub fn list() -> String {
    let mut s = String::from("lints (default level, release that added it):\n");
    for l in LINTS {
        s.push_str(&format!(
            "  {:<24} {:<5} {:<6} {}\n",
            l.name,
            l.default.name(),
            l.since,
            l.desc
        ));
    }
    s.push_str("groups: all (every lint), warnings (every warning-level lint)");
    for (g, ls) in GROUPS {
        s.push_str(&format!(", {g} ({})", ls.join(", ")));
    }
    s.push('\n');
    s
}

/// Each lint's default level; with `library` the library's levels on top.
pub(crate) fn defaults(library: bool) -> HashMap<&'static str, Vec<Set>> {
    let mut base: HashMap<&'static str, Vec<Set>> = LINTS
        .iter()
        .map(|l| (l.name, vec![(l.default, None)]))
        .collect();
    for &(lvl, name) in LIBRARY_LEVELS.iter().filter(|_| library) {
        if let Some(sets) = base.get_mut(name) {
            let from = format!("`{name}` is {} in the library", lvl.name());
            sets.push((lvl, Some(from)));
        }
    }
    base
}
