//! Symbol mangling.
//!
//! `_Y<pkg8>_<path>_<name>_<typehash16>`; concrete items end in `_`
//! (empty typehash). `extern` declarations keep their C name.
//!
//! Identifier bytes outside `[A-Za-z0-9]` are escaped so the `_`
//! separators stay unambiguous: a literal `_` is `_0`, anything else is
//! `_1<hex codepoint>_`. Identifiers never start with a digit, so a
//! separator `_` is never followed by `0`/`1`. Thus `mod a { fn b }` ->
//! `..._a_b_` while a root `fn a__b` -> `...__a_0_0b_`.

use nabic_core::hash::{hex, sha256};
use nabic_ir_common::Linkage;
use nabic_mir::{Link, MirFunction};

/// What symbol naming needs to know about the whole program.
#[derive(Default)]
pub struct Pkgs {
    pub link: Link,
    /// `extern` statics: they keep their C name.
    pub extern_statics: nabic_core::HashSet<String>,
    /// Windows: DLL `extern` statics, reached via `__imp_<name>` (import
    /// libraries define only that for data).
    pub dll_statics: nabic_core::HashSet<String>,
    /// Statics nothing writes after their initializer (not `mut`, not
    /// `extern`): an array of one is read in place.
    pub ro_statics: nabic_core::HashSet<String>,
    /// Lazily initialized statics: name -> symbol of their initializer.
    pub lazy_statics: nabic_core::HashMap<String, String>,
    /// Unit builds: a library's own copies of other packages' statics are
    /// `<symbol>.copy_Y<hash>_`, shared by its units and distinct from other
    /// packages' copies.
    /// C thread-local objects, reached through the thread's block.
    pub tls_statics: nabic_core::HashSet<String>,
    pub unit_copies: bool,
}

impl Pkgs {
    /// `ro_statics`, `extern_statics` and `dll_statics` (`windows`) of
    /// `statics`.
    pub fn set_statics(&mut self, statics: &[nabic_mir::MirStaticDef], windows: bool) {
        self.ro_statics = statics
            .iter()
            .filter(|s| !s.is_extern && !s.is_mut)
            .map(|s| s.name.clone())
            .collect();
        let ext = statics.iter().filter(|s| s.is_extern);
        // C objects keep their C names, defined here or not.
        self.extern_statics = statics
            .iter()
            .filter(|s| s.is_extern || s.c.is_some())
            .map(|s| s.name.clone())
            .collect();
        self.tls_statics = statics
            .iter()
            .filter(|s| s.c.as_ref().is_some_and(|c| c.tls))
            .map(|s| s.name.clone())
            .collect();
        self.dll_statics = ext
            .filter(|s| windows && s.dll_import)
            .map(|s| s.name.clone())
            .collect();
    }

    /// Linker symbol of static `name`.
    pub fn static_sym(&self, name: &str) -> String {
        if self.extern_statics.contains(name) {
            return name.to_owned();
        }
        let sym = item(self.link.hash_of(name), name, "");
        match self.link.lib.as_deref() {
            Some(lib)
                if self.unit_copies
                    && !Link::owns(lib, name)
                    && !self.link.in_prebuilt(name)
                    && !self.link.shared_static(name) =>
            {
                let own = self.link.packages.iter().find(|(p, _)| p == lib);
                format!("{sym}.copy_Y{}_", own.map_or("", |(_, h)| h.as_str()))
            }
            _ => sym,
        }
    }
}

/// Linker symbol of a MIR function: its `#[export_name]`, its
/// `#[link_name]` or own name (an `extern` declaration), or the mangled
/// path.
pub fn symbol_for(f: &MirFunction, pkgs: &Pkgs) -> String {
    if let Some(sym) = &f.facts.export_name {
        return sym.clone();
    }
    if f.facts.linkage == Linkage::Extern {
        return f.facts.link_name.clone().unwrap_or_else(|| f.name.clone());
    }
    mangled(f, pkgs)
}

/// The mangled symbol of a MIR function; an instance's type arguments
/// become the typehash.
pub fn mangled(f: &MirFunction, pkgs: &Pkgs) -> String {
    let path = f.path();
    let hash = f
        .facts
        .mono_args
        .as_ref()
        .map(|a| hex(&sha256(a.as_bytes()))[..16].to_owned())
        .unwrap_or_default();
    item(pkgs.link.hash_of(path), path, &hash)
}

/// Mangle a `::`-qualified item path (`std::fs::read` -> `std_fs`, `read`);
/// the package is the hash.
pub fn item(pkg: &str, qualified: &str, typehash: &str) -> String {
    let mut segs: Vec<&str> = qualified.split("::").collect();
    let name = segs.pop().unwrap_or_default();
    let path: Vec<String> = segs.iter().map(|s| escape(s)).collect();
    format!("_Y{pkg}_{}_{}_{typehash}", path.join("_"), escape(name))
}

/// A string literal's global, named by content so a REPL cell's text
/// compares equal to an earlier cell's.
pub fn str_global(bytes: &[u8]) -> String {
    format!("nabi.str.{}", &hex(&sha256(bytes))[..16])
}

/// 8-hex-digit package hash over the package's content.
pub fn pkg_hash(content: &str) -> String {
    hex(&sha256(content.as_bytes()))[..8].to_owned()
}

/// A `_Y` symbol taken apart.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Demangled {
    pub pkg: String,
    /// Module path segments, without a `pkg::` root.
    pub path: Vec<String>,
    pub name: String,
    /// `None` for a concrete (non-generic) item.
    pub typehash: Option<String>,
}

impl Demangled {
    /// `std::fs` style.
    #[must_use]
    pub fn qualified_path(&self) -> String {
        self.path.join("::")
    }

    /// `std::fs::read` (path + name).
    #[must_use]
    pub fn qualified_name(&self) -> String {
        let path = self.qualified_path();
        if path.is_empty() {
            self.name.clone()
        } else {
            format!("{path}::{}", self.name)
        }
    }

    /// The name as source would write it, on one line: a method
    /// (`Type.m`, or `Type__m` of a generic type's instance) as `Type::m`,
    /// one picked by trait (`m__by_Tr`) as `<path::Type as Tr>::m`, a
    /// method of an impl block named by hash (`__impl<hash>_m`) as
    /// `{impl}::m`. The typehash is left out.
    #[must_use]
    pub fn display_name(&self) -> String {
        let path = self.qualified_path();
        let join = |head: &str, tail: &str| {
            if head.is_empty() {
                tail.to_owned()
            } else {
                format!("{head}::{tail}")
            }
        };
        let n = self.name.as_str();
        if let Some((h, m)) = n.strip_prefix("__impl").and_then(|r| r.split_once('_')) {
            if !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()) && !m.is_empty() {
                return join(&path, &format!("{{impl}}::{m}"));
            }
        }
        // A type name starts with a capital: `a__b` is a function's name.
        let method = n.split_once('.').or_else(|| {
            n.split_once("__")
                .filter(|(t, m)| t.starts_with(|c: char| c.is_ascii_uppercase()) && !m.is_empty())
        });
        let Some((ty, m)) = method else {
            return join(&path, n);
        };
        match m.split_once("__by_") {
            Some((m, tr)) if !m.is_empty() && !tr.is_empty() => {
                format!("<{} as {tr}>::{m}", join(&path, ty))
            }
            _ => join(&path, &format!("{ty}::{m}")),
        }
    }
}

/// Inverse of [`item`]: `None` when `sym` is not a well-formed `_Y`
/// symbol (including the reserved `_YK` / `_YL` / `_YI` / `_YM`).
#[must_use]
pub fn demangle(sym: &str) -> Option<Demangled> {
    let rest = sym.strip_prefix("_Y")?;
    let is_hex = |s: &str| s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let (pkg, rest) = (rest.get(..8)?, rest.get(8..)?);
    if !is_hex(pkg) {
        return None;
    }
    let rest = rest.strip_prefix('_')?;
    // A typehash is 16 hex digits after the last separator; a concrete
    // item ends in the separator (empty typehash) or omits it.
    let (body, typehash) = if let Some(b) = rest.strip_suffix('_') {
        (b, None)
    } else {
        match rest.len().checked_sub(17).map(|i| rest.split_at(i)) {
            Some((b, h)) if h.starts_with('_') && is_hex(&h[1..]) => (b, Some(h[1..].to_owned())),
            _ => (rest, None),
        }
    };
    // `_0` is `_`, `_1<hex>_` a code point, any other `_` a separator.
    let mut segs = vec![String::new()];
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '_' {
            segs.last_mut()?.push(c);
            continue;
        }
        match chars.peek() {
            Some('0') => {
                chars.next();
                segs.last_mut()?.push('_');
            }
            Some('1') => {
                chars.next();
                let hex: String = chars.by_ref().take_while(|&c| c != '_').collect();
                segs.last_mut()?
                    .push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
            }
            _ => segs.push(String::new()),
        }
    }
    let name = segs.pop()?;
    // An empty path mangles as one empty segment (`_Yp__name_`).
    if segs.len() == 1 && segs[0].is_empty() {
        segs.clear();
    }
    if name.is_empty() || segs.iter().any(String::is_empty) {
        return None;
    }
    Some(Demangled {
        pkg: pkg.to_owned(),
        path: segs,
        name,
        typehash,
    })
}

fn escape(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len());
    for ch in ident.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => out.push(ch),
            '_' => out.push_str("_0"),
            _ => out.push_str(&format!("_1{:x}_", u32::from(ch))),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_path_and_underscore_names_do_not_collide() {
        assert_eq!(item("3f8a1c2b", "a::b", ""), "_Y3f8a1c2b_a_b_");
        assert_eq!(item("3f8a1c2b", "a__b", ""), "_Y3f8a1c2b__a_0_0b_");
        assert_ne!(item("p", "a::b_c", ""), item("p", "a_b::c", ""));
        assert_eq!(item("p", "malloc", ""), "_Yp__malloc_");
        assert_eq!(item("p", "std::vec::new", ""), "_Yp_std_vec_new_");
        let link = Link {
            packages: vec![
                ("core".to_owned(), "c".to_owned()),
                (String::new(), "u".to_owned()),
            ],
            ..Link::default()
        };
        assert_eq!(link.hash_of("core::str::len"), "c");
        assert_eq!(link.hash_of("corex::f"), "u");
        assert_eq!(link.hash_of("main"), "u");
    }

    #[test]
    fn demangle_round_trips() {
        for (q, th) in [
            ("a::b", ""),
            ("a__b", ""),
            ("malloc", ""),
            ("std::vec::push", "a7c91d8e44b2f190"),
            ("geom::α", "0123456789abcdef"),
            ("x_1::_y", ""),
        ] {
            let sym = item("3f8a1c2b", q, th);
            let d = demangle(&sym).unwrap_or_else(|| panic!("{sym}"));
            assert_eq!(d.qualified_name(), q, "{sym}");
            assert_eq!(d.typehash.as_deref().unwrap_or(""), th, "{sym}");
        }
        let d = demangle("_Y3f8a1c2b_geom_compute_main").unwrap();
        assert_eq!(d.qualified_name(), "geom::compute::main");
        assert_eq!(d.typehash, None);
        assert!(demangle("_YK00000000_x_").is_none());
        assert!(demangle("_ZN3foo").is_none());
    }

    #[test]
    fn display_names() {
        for (sym, want) in [
            (
                "_Ya61f26f1_core_collections_dict_Dict_0_0grow_ce3f36e15e552a70",
                "core::collections::dict::Dict::grow",
            ),
            (
                "_Y0b3cd907_core_cmp_Ordering_12e_is_0eq_",
                "core::cmp::Ordering::is_eq",
            ),
            (
                "_Y0b3cd907_core_array__0_0impldb1d67aa_0len_165b13d09219de49",
                "core::array::{impl}::len",
            ),
            (
                "_Y3f8a1c2b_geom_Point_12e_fmt_0_0by_0Display_",
                "<geom::Point as Display>::fmt",
            ),
            ("_Y3f8a1c2b_geom_distance_", "geom::distance"),
            ("_Y3f8a1c2b__a_0_0b_", "a__b"),
            ("_Y3f8a1c2b__main_", "main"),
        ] {
            let d = demangle(sym).unwrap_or_else(|| panic!("{sym}"));
            assert_eq!(d.display_name(), want, "{sym}");
        }
    }
}
