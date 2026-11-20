//! Path roots no package declares: the packages code may be missing.
//!
//! `json` in `use json::parse;` or `json::parse(s)` when nothing in the
//! sources declares `json`. A token scan, not a parse, so it stays cheap;
//! it errs on the side of reporting nothing.

use nabic_lexer::{Lexer, TokenKind as T};
use std::collections::BTreeSet;

/// Roots no package is added for: the standard library, the path keywords,
/// and `nabi` (a leftover root the compiler's help rewrites).
const RESERVED: [&str; 8] = [
    "std", "core", "bare", "nabi", "pkg", "sup", "crate", "super",
];

/// Lowercase names the sources start a path with and none of them
/// declares (as a module, item, import or binding), sorted.
#[must_use]
pub fn missing_roots<'a>(sources: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut used = BTreeSet::new();
    let mut declared = BTreeSet::new();
    for src in sources {
        scan(src, &mut used, &mut declared);
    }
    used.difference(&declared).cloned().collect()
}

fn scan(src: &str, used: &mut BTreeSet<String>, declared: &mut BTreeSet<String>) {
    let (tokens, _) = Lexer::new(src, "").lex_all();
    let k: Vec<&T> = tokens.iter().map(|t| &t.kind).collect();
    let mut in_use = false;
    for (i, t) in k.iter().enumerate() {
        match t {
            T::Use => in_use = true,
            T::Semi => in_use = false,
            T::Ident(name) => {
                let prev = i.checked_sub(1).map(|j| k[j]);
                let next = k.get(i + 1);
                let after_path = matches!(prev, Some(T::ColonColon | T::Dot));
                if in_use && (after_path || matches!(prev, Some(T::As | T::LBrace | T::Comma))) {
                    // A name an import brings in.
                    declared.insert(name.clone());
                }
                if matches!(
                    prev,
                    Some(T::Mod | T::Fn | T::Struct | T::Enum | T::Trait | T::Let | T::As)
                ) || matches!(prev, Some(T::Ident(w)) if matches!(w.as_str(), "const" | "static" | "type" | "union"))
                {
                    declared.insert(name.clone());
                }
                let root = !after_path
                    && matches!(next, Some(T::ColonColon))
                    && name.starts_with(|c: char| c.is_ascii_lowercase())
                    && !RESERVED.contains(&name.as_str());
                if root {
                    used.insert(name.clone());
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::missing_roots;

    #[test]
    fn roots() {
        let src = "use json::parse;\nuse std::fs;\nmod util;\nuse geom::{area, shapes};\n\
                   fn main() { let v = toml::from_str(s); fs::read(p); util::x(); shapes::sq(); \
                   Point::new(); pkg::a(); self::b(); x.y::z(); }\n";
        assert_eq!(missing_roots([src]), ["geom", "json", "toml"]);
        assert_eq!(
            missing_roots(["use a::b as table;\nfn f() { table::x(); }"]),
            ["a"]
        );
        assert!(missing_roots(["// json::parse\nlet s = \"toml::x\";"]).is_empty());
        // Declared in one file, used in another.
        assert!(missing_roots(["mod util;", "fn f() { util::x(); }"]).is_empty());
    }
}
