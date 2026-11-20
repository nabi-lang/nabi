//! Naming lints: `snake_case`, `CamelCase`, `UPPER_CASE`.

#[allow(clippy::wildcard_imports)]
use crate::lint::*;

pub(crate) fn is_snake(n: &str) -> bool {
    !n.chars().any(|c| c.is_ascii_uppercase())
}

/// rustc's rule: no lowercase first letter, no `__`, and an inner `_`
/// only between caseless characters (`V1_0`).
pub(crate) fn is_camel(n: &str) -> bool {
    let t = n.trim_matches('_');
    let cased = |c: char| c.is_lowercase() || c.is_uppercase();
    let cs: Vec<char> = t.chars().collect();
    !t.starts_with(char::is_lowercase)
        && !t.contains("__")
        && !cs
            .windows(2)
            .any(|w| (w[1] == '_' && cased(w[0])) || (w[0] == '_' && cased(w[1])))
}

pub(crate) fn is_upper(n: &str) -> bool {
    !n.chars().any(|c| c.is_ascii_lowercase())
}

pub(crate) fn to_snake(n: &str) -> String {
    let mut s = String::new();
    for (i, c) in n.char_indices() {
        if c.is_ascii_uppercase() {
            if i > 0 && !s.ends_with('_') {
                s.push('_');
            }
            s.push(c.to_ascii_lowercase());
        } else {
            s.push(c);
        }
    }
    s
}

pub(crate) fn to_camel(n: &str) -> String {
    n.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut cs = p.chars();
            cs.next()
                .map(|c| c.to_ascii_uppercase().to_string() + cs.as_str())
                .unwrap_or_default()
        })
        .collect()
}

impl Cx<'_> {
    pub(crate) fn name_lint(&mut self, what: &str, name: &str, span: Span, kind: u8) {
        // An item in a block by its own name.
        let leaf = nabic_core::local::leaf(name);
        let name = demangle(&leaf);
        let (ok, lint, fixed, style) = match kind {
            0 => (
                is_snake(name),
                "non_snake_case",
                to_snake(name),
                "a snake case",
            ),
            1 => (
                is_camel(name),
                "non_camel_case_types",
                to_camel(name),
                "an upper camel case",
            ),
            _ => (
                is_upper(name),
                "non_upper_case_globals",
                to_snake(name).to_ascii_uppercase(),
                "an upper case",
            ),
        };
        if !ok {
            self.findings.push(Finding {
                lint,
                span,
                msg: format!("{what} `{name}` should have {style} name"),
                notes: vec![format!("rename it to `{fixed}`")],
                secondary: None,
                base: None,
                fix: None,
            });
        }
    }
}
