//! Plugin and host links.
//!
//! A plugin links no runtime: every Nabi symbol
//! it takes from the process (`nabi_*`, `_Y*`) becomes a slot the module
//! loader fills, a call to one goes through a stub that jumps through
//! the slot, and a GOT-form load of one loads the slot itself. A host
//! keeps every runtime entry and lists its Nabi symbols in an export
//! table. The stubs, slots and tables come from a glue object the caller
//! builds from the [`Scan`]; the linker only finds what they must be.

mod coff;
mod elf;
mod macho;

use crate::symtab::{Atom, AtomId, Def, Fetch, Reloc, SymId, SymbolTable, Target};
use nabic_core::{HashMap, HashSet};
use nabir_abi::module::MODULE_DESC;
use nabir_abi::typedesc::offset_of;

/// What a plugin or host link adds.
pub struct Plugin<'a> {
    /// Symbols to keep and (host) list besides the automatic ones.
    pub exports: &'a [String],
    /// Hot build: references to the module's own `X` go through a slot;
    /// the export table names the body `X.hot`.
    pub import_own: bool,
    /// A program that loads plugins.
    pub host: bool,
    /// Hot build: the package hashes of the other hot packages, whose
    /// functions it calls through hot slots too.
    pub hot_deps: &'a [String],
}

/// A symbol the module takes from the process.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// The C name (no Mach-O `_`).
    pub name: String,
    /// Something calls it or takes its address: `name` is defined as a
    /// stub jumping through the slot.
    pub stub: bool,
    /// One of the module's own functions (hot builds).
    pub hot: bool,
}

/// A host export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    pub name: String,
    /// Defined in code (else data).
    pub code: bool,
}

/// What the glue object must define, names sorted.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    pub imports: Vec<Import>,
    /// Host only (a plugin's export table is the compiler's).
    pub exports: Vec<Export>,
    pub host: bool,
    /// COFF: the slot of `X` is `__imp_X`, and the DLL's entry
    /// ([`DLL_ENTRY`], returning TRUE) is in the glue.
    pub coff: bool,
}

impl Scan {
    /// The slot symbol of import `name`.
    #[must_use]
    pub fn slot(&self, name: &str) -> String {
        if self.coff {
            format!("__imp_{name}")
        } else {
            format!("{SLOT}{name}")
        }
    }
}

/// Builds the glue object of a scan.
pub type Glue<'a> = dyn Fn(&Scan) -> Result<Vec<u8>, String> + 'a;

/// Prefix of a slot's symbol (not COFF).
pub const SLOT: &str = "nabi.slot.";
/// The import table, `Import[n]`.
pub const IMPORTS: &str = "nabi.module.imports";
/// A host's export table, `Export[n]`.
pub const EXPORTS: &str = "nabi.module.exports";
/// Absolute 0: the loader reads the text range from the image.
const TEXT: [&str; 2] = ["nabi.module.text_lo", "nabi.module.text_hi"];
/// The counts the linker writes into the descriptor.
const IMPORTS_LEN: &str = "\0nabi.module.imports.len";
const EXPORTS_LEN: &str = "\0nabi.module.exports.len";
/// A plugin DLL's entry point.
pub const DLL_ENTRY: &str = "nabi.dll_entry";
/// The descriptors.
pub const MODULE: &str = "nabi_module";
pub const HOST_MODULE: &str = "nabi_host_module";

/// How a relocation reaches its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Use {
    /// Through the target's GOT slot; the kind that reaches a word
    /// directly in the same instructions.
    Got(u16),
    /// A GOT form this linker cannot point at a slot.
    BadGot,
    /// A thread-local access.
    Tls,
    /// A call, jump or address.
    Plain,
}

/// One format's link as plugin linking sees it.
pub trait Image {
    fn syms(&mut self) -> &mut SymbolTable;
    fn atoms(&mut self) -> &mut Vec<Atom>;
    /// The platform's C symbol prefix (Mach-O `_`).
    fn prefix(&self) -> &'static str;
    /// Atom `a` is code.
    fn code(&self, a: AtomId) -> bool;
    /// Load an archive member and what it fetches.
    fn fetch(&mut self, f: Fetch) -> Result<(), String>;
    /// Add an object from memory.
    fn add(&mut self, name: &str, data: Vec<u8>) -> Result<(), String>;
    fn classify(&self, kind: u16) -> Use;
    /// Kind and log2 width of an absolute pointer-sized word.
    fn abs(&self) -> (u16, u8);
    /// Pointer size in bytes.
    fn ptr(&self) -> u32;
    fn coff(&self) -> bool {
        false
    }
}

/// The C name of a Nabi symbol the process provides, else `None`.
fn nabi_name<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    let c = name.strip_prefix(prefix)?;
    let nabi = (c.starts_with("nabi_") || c.starts_with("_Y"))
        && !c.contains(".copy_")
        && c != MODULE
        && c != HOST_MODULE;
    nabi.then_some(c)
}

/// Before dead stripping: pull a host's runtime, find a plugin's
/// imports, add the glue object `glue` builds, define the counts and
/// patch them into the descriptor. Returns the symbols to keep.
pub fn prepare<I: Image>(
    img: &mut I,
    p: &Plugin<'_>,
    glue: &Glue<'_>,
) -> Result<Vec<SymId>, String> {
    let pre = img.prefix();
    if p.host {
        pull_runtime(img, pre)?;
    }
    let hot = if p.import_own {
        own_imports(img, pre)?
    } else {
        HashSet::default()
    };
    let mut scan = Scan {
        host: p.host,
        coff: img.coff(),
        ..Scan::default()
    };
    if !p.host {
        scan.imports = imports(img, pre, &hot, scan.coff)?;
        for i in &mut scan.imports {
            let dep = |d: &String| {
                i.name
                    .strip_prefix("_Y")
                    .and_then(|r| r.strip_prefix(d.as_str()))
                    .is_some_and(|r| r.starts_with('_'))
            };
            if i.stub && p.hot_deps.iter().any(dep) {
                i.hot = true;
            }
        }
    }
    let mut keep = Vec::new();
    for n in p.exports {
        keep.push(img.syms().intern(&format!("{pre}{n}")));
    }
    if p.host {
        scan.exports = exports(img, pre, p.exports);
        for e in &scan.exports {
            keep.push(img.syms().intern(&format!("{pre}{}", e.name)));
        }
    }
    let data = glue(&scan)?;
    img.add("<plugin glue>", data)?;
    let module = if p.host { HOST_MODULE } else { MODULE };
    let lens = [(17, IMPORTS_LEN, scan.imports.len())];
    let host_lens = [(19, EXPORTS_LEN, scan.exports.len())];
    let lens: Vec<_> = if p.host {
        lens.iter().chain(&host_lens).copied().collect()
    } else {
        lens.to_vec()
    };
    for (_, n, v) in &lens {
        let id = img.syms().intern(n);
        img.syms().get_mut(id).def = Def::Absolute(*v as u64);
    }
    for n in TEXT {
        let id = img.syms().intern(&format!("{pre}{n}"));
        if matches!(img.syms().get(id).def, Def::Undefined | Def::Lazy { .. }) {
            let _ = img
                .syms()
                .define(id, Def::Absolute(0), u32::MAX, false, true);
        }
    }
    let desc = img.syms().intern(&format!("{pre}{module}"));
    keep.push(desc);
    if let Def::Defined { atom, off } = img.syms().get(desc).def {
        let ptr = img.ptr();
        let (kind, len) = img.abs();
        for (word, n, _) in lens {
            let target = img.syms().find(n).expect("interned");
            let at = (off + u64::from(offset_of(MODULE_DESC, word, ptr))) as u32;
            let relocs = &mut img.atoms()[atom as usize].relocs;
            relocs.retain(|r| r.off != at);
            relocs.push(Reloc {
                off: at,
                kind,
                len,
                pcrel: false,
                target: Target::Sym(target),
                addend: 0,
                sub: None,
            });
        }
    } else if !p.host {
        return Err(format!(
            "a plugin defines no {module}: build it with `--crate-type=plugin`"
        ));
    }
    Ok(keep)
}

/// Load every archive member that defines a runtime entry, until none is
/// left.
fn pull_runtime<I: Image>(img: &mut I, pre: &str) -> Result<(), String> {
    loop {
        let mut work = Vec::new();
        let n = img.syms().syms.len();
        for id in 0..n as SymId {
            let s = img.syms().get(id);
            let lazy = matches!(s.def, Def::Lazy { .. });
            if lazy
                && s.name
                    .strip_prefix(pre)
                    .is_some_and(|c| c.starts_with("nabi_"))
            {
                if let Some(f) = img.syms().reference(id, u32::MAX, false) {
                    work.push(f);
                }
            }
        }
        if work.is_empty() {
            return Ok(());
        }
        for f in work {
            img.fetch(f)?;
        }
    }
}

/// Hot builds: each referenced `X.hot` becomes the body of `X`, and `X`
/// itself an import. The names of those `X`.
fn own_imports<I: Image>(img: &mut I, pre: &str) -> Result<HashSet<String>, String> {
    let mut own = HashSet::default();
    let n = img.syms().syms.len();
    for id in 0..n as SymId {
        let s = img.syms().get(id).clone();
        let Some(base) = s.name.strip_suffix(".hot") else {
            continue;
        };
        if !matches!(s.def, Def::Undefined) || !s.referenced {
            continue;
        }
        let Some(b) = img.syms().find(base) else {
            continue;
        };
        let bs = img.syms().get(b).clone();
        if !matches!(bs.def, Def::Defined { .. }) {
            return Err(format!(
                "{}: no body for the hot slot",
                &s.name[pre.len()..]
            ));
        }
        let hs = img.syms().get_mut(id);
        hs.def = bs.def;
        hs.file = bs.file;
        hs.private_extern = true;
        let bm = img.syms().get_mut(b);
        bm.def = Def::Undefined;
        bm.referenced = true;
        own.insert(base[pre.len()..].to_owned());
    }
    Ok(own)
}

/// The plugin's imports: referenced Nabi symbols nothing defines. GOT
/// relocations to them are pointed at their slots.
fn imports<I: Image>(
    img: &mut I,
    pre: &str,
    hot: &HashSet<String>,
    coff: bool,
) -> Result<Vec<Import>, String> {
    // Undefined Nabi symbols and the import each stands for.
    let mut of: HashMap<SymId, (String, bool)> = HashMap::default();
    let n = img.syms().syms.len();
    for id in 0..n as SymId {
        let s = img.syms().get(id);
        if s.def != Def::Undefined || !s.referenced {
            continue;
        }
        if coff {
            if let Some(c) = s.name.strip_prefix("__imp_") {
                if nabi_name(c, "").is_some() {
                    of.insert(id, (c.to_owned(), true));
                    continue;
                }
            }
        }
        if let Some(c) = nabi_name(&s.name, pre) {
            of.insert(id, (c.to_owned(), false));
        }
    }
    let mut found: HashMap<String, Import> = HashMap::default();
    for (name, _) in of.values() {
        found.entry(name.clone()).or_insert_with(|| Import {
            name: name.clone(),
            stub: false,
            hot: hot.contains(name),
        });
    }
    let mut slots: HashMap<String, SymId> = HashMap::default();
    let mut errors = Vec::new();
    let natoms = img.atoms().len();
    for a in 0..natoms {
        let nrel = img.atoms()[a].relocs.len();
        for k in 0..nrel {
            let r = img.atoms()[a].relocs[k];
            let Target::Sym(id) = r.target else {
                continue;
            };
            let Some((name, is_slot)) = of.get(&id).cloned() else {
                continue;
            };
            if is_slot {
                continue;
            }
            match img.classify(r.kind) {
                Use::Plain => found.get_mut(&name).expect("found").stub = true,
                Use::Got(kind) => {
                    let slot = *slots.entry(name.clone()).or_insert_with(|| {
                        let n = if coff {
                            format!("__imp_{name}")
                        } else {
                            format!("{pre}{SLOT}{name}")
                        };
                        img.syms().intern(&n)
                    });
                    img.syms().reference(slot, u32::MAX, false);
                    let r = &mut img.atoms()[a].relocs[k];
                    r.kind = kind;
                    r.target = Target::Sym(slot);
                }
                Use::BadGot => errors.push(format!(
                    "{name}: a plugin cannot reach the process's symbol through this \
                     GOT relocation (kind {})",
                    r.kind
                )),
                Use::Tls => errors.push(format!(
                    "{name}: plugin code reads a thread-local of the process; \
                     plugins reach the TLS block through nabi_tls_get"
                )),
            }
        }
    }
    if !errors.is_empty() {
        errors.sort();
        errors.dedup();
        return Err(errors.join("\n"));
    }
    let mut v: Vec<Import> = found.into_values().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}

/// A host's exports: every defined Nabi symbol, and `extra`.
fn exports<I: Image>(img: &mut I, pre: &str, extra: &[String]) -> Vec<Export> {
    let mut v = Vec::new();
    let mut seen = HashSet::default();
    let n = img.syms().syms.len();
    for id in 0..n as SymId {
        let s = img.syms().get(id);
        let Def::Defined { atom, .. } = s.def else {
            continue;
        };
        let Some(c) = s.name.strip_prefix(pre) else {
            continue;
        };
        if nabi_name(&s.name, pre).is_none() && !extra.iter().any(|e| e == c) {
            continue;
        }
        let c = c.to_owned();
        if seen.insert(c.clone()) {
            v.push(Export {
                name: c,
                code: img.code(atom),
            });
        }
    }
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_of_the_process() {
        assert_eq!(nabi_name("_nabi_gc_alloc", "_"), Some("nabi_gc_alloc"));
        assert_eq!(nabi_name("_Y1234abcd_x_f_0", ""), Some("_Y1234abcd_x_f_0"));
        assert_eq!(nabi_name("_Yab_f.copy_Ycd_", ""), None);
        assert_eq!(nabi_name("malloc", ""), None);
        assert_eq!(nabi_name("nabi.slot.x", ""), None);
    }
}
