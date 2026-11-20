//! The Mach-O linker: arm64 macOS and iOS executables (`MH_EXECUTE`) from
//! objects, archives and dylibs / `.tbd` stubs, ld64's command line.

pub mod a64;
pub mod dsym;
pub mod dylib;
pub mod object;
pub mod sign;
pub mod trie;
pub mod unwind;

mod write;

use crate::symtab::{Atom, AtomId, Def, Fetch, SymbolTable, Target};
use dylib::Dylib;
use nabic_core::HashMap;
use object::ObjFile;
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// `CPU_TYPE_*` of an `-arch` name.
#[must_use]
pub fn cpu_type(arch: &str) -> Option<u32> {
    match arch {
        "arm64" => Some(0x0100_000c),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub enum Input {
    File(PathBuf),
    Lib(String),
    Framework(String),
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Options {
    pub arch: String,
    /// `-platform_version`: platform, minimum OS and SDK, packed.
    pub platform: u32,
    pub min_os: u32,
    pub sdk: u32,
    pub syslibroot: Option<PathBuf>,
    pub lib_dirs: Vec<PathBuf>,
    pub framework_dirs: Vec<PathBuf>,
    pub inputs: Vec<Input>,
    pub out: PathBuf,
    pub dead_strip: bool,
    /// `-x`: no local symbols in the symbol table.
    pub strip_locals: bool,
    pub no_exports: bool,
    pub all_load: bool,
    pub rpaths: Vec<String>,
    pub entry: String,
    pub undefined: Vec<String>,
    /// `--icf=safe`: fold identical code ([`fold_code`]).
    pub icf: bool,
    /// `-dylib`: an `MH_DYLIB` named `install_name`.
    pub dylib: bool,
    pub install_name: Option<String>,
    /// `-current_version` / `-compatibility_version` of a dylib, packed
    /// `x.y.z` as `LC_ID_DYLIB` holds them.
    pub current_version: u32,
    pub compat_version: u32,
    /// `-exported_symbol`: the only exports, when any.
    pub exported: Vec<String>,
    /// `-init`: the dylib's initializer.
    pub init: Option<String>,
    /// `-nabi_anchor <word>=<symbol>`: the linker writes the symbol's
    /// address less the word's own into the 8-byte word.
    pub anchors: Vec<(String, String)>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            arch: "arm64".to_owned(),
            platform: 1,
            min_os: 0x000b_0000,
            sdk: 0x000b_0000,
            syslibroot: None,
            lib_dirs: Vec::new(),
            framework_dirs: Vec::new(),
            inputs: Vec::new(),
            out: PathBuf::from("a.out"),
            dead_strip: false,
            strip_locals: false,
            no_exports: false,
            all_load: false,
            rpaths: Vec::new(),
            entry: "_main".to_owned(),
            undefined: Vec::new(),
            icf: false,
            dylib: false,
            install_name: None,
            current_version: 0x1_0000,
            compat_version: 0x1_0000,
            exported: Vec::new(),
            init: None,
            anchors: Vec::new(),
        }
    }
}

const fn platform_id(name: &str) -> Option<u32> {
    match name.as_bytes() {
        b"macos" => Some(1),
        b"ios" => Some(2),
        b"tvos" => Some(3),
        b"watchos" => Some(4),
        b"ios-simulator" => Some(7),
        _ => None,
    }
}

impl Options {
    /// What the link must define: the entry point, or a dylib's exports,
    /// initializer and anchored symbols; and `-u` symbols.
    fn wanted(&self) -> Vec<String> {
        let mut v: Vec<String> = if self.dylib {
            let mut v = self.exported.clone();
            v.extend(self.init.clone());
            v
        } else {
            vec![self.entry.clone()]
        };
        for (a, t) in &self.anchors {
            v.push(a.clone());
            v.push(t.clone());
        }
        v.extend(self.undefined.iter().cloned());
        v
    }
}

/// Parse ld64-style arguments (the subset nabic passes).
pub fn parse_args<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Result<Options, String> {
    let mut o = Options::default();
    let args: Vec<OsString> = args.iter().map(|a| a.as_ref().to_owned()).collect();
    let mut it = args.iter();
    let next = |it: &mut std::slice::Iter<'_, OsString>, flag: &str| {
        it.next()
            .cloned()
            .ok_or_else(|| format!("{flag}: missing argument"))
    };
    while let Some(a) = it.next() {
        let s = a.to_string_lossy();
        match s.as_ref() {
            "-arch" => o.arch = next(&mut it, "-arch")?.to_string_lossy().into_owned(),
            "-o" => o.out = next(&mut it, "-o")?.into(),
            "-syslibroot" => o.syslibroot = Some(next(&mut it, "-syslibroot")?.into()),
            "-platform_version" => {
                let p = next(&mut it, &s)?.to_string_lossy().into_owned();
                o.platform = platform_id(&p)
                    .or_else(|| p.parse().ok())
                    .ok_or_else(|| format!("-platform_version: unknown platform {p}"))?;
                o.min_os = dylib::parse_version(&next(&mut it, &s)?.to_string_lossy());
                o.sdk = dylib::parse_version(&next(&mut it, &s)?.to_string_lossy());
            }
            "-dead_strip" => o.dead_strip = true,
            "-x" => o.strip_locals = true,
            "-no_exported_symbols" => o.no_exports = true,
            "-all_load" => o.all_load = true,
            "-rpath" => o
                .rpaths
                .push(next(&mut it, "-rpath")?.to_string_lossy().into_owned()),
            "-e" => o.entry = next(&mut it, "-e")?.to_string_lossy().into_owned(),
            "-dylib" => o.dylib = true,
            "-install_name" => {
                o.install_name = Some(next(&mut it, &s)?.to_string_lossy().into_owned());
            }
            "-current_version" => {
                o.current_version = dylib::parse_version(&next(&mut it, &s)?.to_string_lossy());
            }
            "-compatibility_version" => {
                o.compat_version = dylib::parse_version(&next(&mut it, &s)?.to_string_lossy());
            }
            "-exported_symbol" => o
                .exported
                .push(next(&mut it, &s)?.to_string_lossy().into_owned()),
            "-init" => o.init = Some(next(&mut it, &s)?.to_string_lossy().into_owned()),
            "-nabi_anchor" => {
                let v = next(&mut it, &s)?.to_string_lossy().into_owned();
                let (a, t) = v
                    .split_once('=')
                    .ok_or_else(|| format!("-nabi_anchor {v}: expected <word>=<symbol>"))?;
                o.anchors.push((a.to_owned(), t.to_owned()));
            }
            "-u" => o
                .undefined
                .push(next(&mut it, "-u")?.to_string_lossy().into_owned()),
            "-L" => o.lib_dirs.push(next(&mut it, "-L")?.into()),
            "-F" => o.framework_dirs.push(next(&mut it, "-F")?.into()),
            "-framework" => o.inputs.push(Input::Framework(
                next(&mut it, "-framework")?.to_string_lossy().into_owned(),
            )),
            // Folding identical code only saves space.
            // Only what is safe is ever folded.
            "--icf=safe" | "--icf=all" => o.icf = true,
            "--icf=none" => o.icf = false,
            "-demangle"
            | "-dynamic"
            | "-execute"
            | "-pie"
            | "-no_deduplicate"
            | "-headerpad_max_install_names" => {}
            _ if s.starts_with("-L") => o.lib_dirs.push(PathBuf::from(&s[2..])),
            _ if s.starts_with("-F") => o.framework_dirs.push(PathBuf::from(&s[2..])),
            _ if s.starts_with("-l") => o.inputs.push(Input::Lib(s[2..].to_owned())),
            _ if s.starts_with('-') => return Err(format!("unknown argument: {s}")),
            _ => o.inputs.push(Input::File(a.into())),
        }
    }
    if cpu_type(&o.arch) != Some(0x0100_000c) {
        return Err(format!("-arch {}: only arm64 is supported", o.arch));
    }
    Ok(o)
}

/// An input archive: its bytes and which members are loaded.
struct ArchiveIn {
    path: PathBuf,
    data: Vec<u8>,
    /// `(name, data range)`.
    members: Vec<(String, std::ops::Range<usize>)>,
    loaded: Vec<bool>,
}

pub struct Link {
    pub arch: String,
    pub cpu: u32,
    pub atoms: Vec<Atom>,
    pub syms: SymbolTable,
    pub files: Vec<ObjFile>,
    pub compact: Vec<unwind::CompactEntry>,
    pub cies: Vec<unwind::Cie>,
    pub fdes: Vec<unwind::Fde>,
    archives: Vec<ArchiveIn>,
    pub dylibs: Vec<Dylib>,
    /// Folded atoms and the copy each became.
    pub folded: Vec<(AtomId, AtomId)>,
    /// Install names already loaded.
    dylib_paths: Vec<PathBuf>,
    /// The `PLATFORM_*` objects must be built for; 0: any.
    pub platform: u32,
}

impl Link {
    /// An empty link for `arch` (`arm64`, ..), `None` for an unknown one.
    #[must_use]
    pub fn new(arch: &str) -> Option<Self> {
        Some(Self {
            arch: arch.to_owned(),
            cpu: cpu_type(arch)?,
            atoms: Vec::new(),
            syms: SymbolTable::default(),
            files: Vec::new(),
            compact: Vec::new(),
            cies: Vec::new(),
            fdes: Vec::new(),
            archives: Vec::new(),
            dylibs: Vec::new(),
            folded: Vec::new(),
            dylib_paths: Vec::new(),
            platform: 0,
        })
    }

    #[must_use]
    pub fn file_name(&self, fi: u32) -> String {
        self.files
            .get(fi as usize)
            .map_or_else(|| "<internal>".to_owned(), |f| f.name.clone())
    }

    fn load_member(&mut self, (ai, mi): Fetch) -> Result<Vec<Fetch>, String> {
        let a = &mut self.archives[ai as usize];
        if a.loaded[mi as usize] {
            return Ok(Vec::new());
        }
        a.loaded[mi as usize] = true;
        let (name, range) = a.members[mi as usize].clone();
        let data = a.data[range].to_vec();
        if crate::bytes::u32le(&data, 0) != 0xfeed_facf {
            return Ok(Vec::new());
        }
        let path = absolute(&a.path);
        let shown = format!("{}({name})", a.path.display());
        let oso = format!("{}({name})", path.display());
        let fi = self.files.len() as u32;
        object::parse(self, fi, shown, oso, data)
    }

    pub(crate) fn drain(&mut self, mut work: Vec<Fetch>) -> Result<(), String> {
        while let Some(f) = work.pop() {
            work.extend(self.load_member(f)?);
        }
        Ok(())
    }

    fn add_file(&mut self, path: &Path, o: &Options) -> Result<(), String> {
        let raw =
            std::fs::read(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        if raw.starts_with(crate::archive::MAGIC) {
            return self.add_archive(path, raw, o.all_load);
        }
        if path.extension().is_some_and(|e| e == "tbd") || raw.starts_with(b"--- !tapi") {
            return self.add_dylib(path, o);
        }
        let thin = object::thin_slice(&raw, &self.arch)
            .ok_or_else(|| format!("{}: no {} slice", path.display(), self.arch))?;
        if thin.starts_with(crate::archive::MAGIC) {
            let thin = thin.to_vec();
            return self.add_archive(path, thin, o.all_load);
        }
        match crate::bytes::u32le(thin, 12) {
            1 => {
                let fi = self.files.len() as u32;
                let shown = path.display().to_string();
                let oso = absolute(path).display().to_string();
                let thin = thin.to_vec();
                let f = object::parse(self, fi, shown, oso, thin)?;
                self.drain(f)
            }
            6 => self.add_dylib(path, o),
            _ => Err(format!("{}: unknown file type", path.display())),
        }
    }

    fn add_archive(&mut self, path: &Path, data: Vec<u8>, all: bool) -> Result<(), String> {
        let ai = self.archives.len() as u32;
        let (members, mut symbols) = {
            let a = crate::archive::parse(&data).map_err(|e| format!("{}: {e}", path.display()))?;
            let base = data.as_ptr() as usize;
            let members: Vec<(String, std::ops::Range<usize>)> = a
                .members
                .iter()
                .map(|m| {
                    let s = m.data.as_ptr() as usize - base;
                    (m.name.clone(), s..s + m.data.len())
                })
                .collect();
            (members, a.symbols)
        };
        if symbols.is_empty() {
            for (i, (_, r)) in members.iter().enumerate() {
                for s in crate::archive::defined_symbols(&data[r.clone()]) {
                    symbols.push((s, i));
                }
            }
        }
        let n = members.len();
        self.archives.push(ArchiveIn {
            path: path.to_owned(),
            data,
            members,
            loaded: vec![false; n],
        });
        let mut work = Vec::new();
        if all {
            work.extend((0..n as u32).map(|m| (ai, m)));
        } else {
            for (name, m) in symbols {
                if let Some(f) = self.syms.lazy(&name, ai, m as u32) {
                    work.push(f);
                }
            }
        }
        self.drain(work)
    }

    fn add_dylib(&mut self, path: &Path, o: &Options) -> Result<(), String> {
        if self.dylib_paths.iter().any(|p| p == path) {
            return Ok(());
        }
        let want = dylib::Want {
            arch: &o.arch,
            platform: o.platform,
        };
        let d = dylib::read_dylib(path, want, o.syslibroot.as_deref())?;
        if self.dylibs.iter().any(|x| x.install_name == d.install_name) {
            return Ok(());
        }
        self.dylib_paths.push(path.to_owned());
        self.dylibs.push(d);
        Ok(())
    }
}

fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_owned())
}

/// Library search directories: `-L`, then the SDK's (or the system's).
fn search_dirs(o: &Options) -> Vec<PathBuf> {
    let mut v = o.lib_dirs.clone();
    let root = o.syslibroot.clone().unwrap_or_else(|| PathBuf::from("/"));
    v.push(root.join("usr/lib"));
    v.push(root.join("usr/local/lib"));
    v
}

fn find_library(name: &str, o: &Options) -> Option<PathBuf> {
    for d in search_dirs(o) {
        for ext in ["tbd", "dylib", "a"] {
            let p = d.join(format!("lib{name}.{ext}"));
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn find_framework(name: &str, o: &Options) -> Option<PathBuf> {
    let root = o.syslibroot.clone().unwrap_or_else(|| PathBuf::from("/"));
    let mut dirs = o.framework_dirs.clone();
    dirs.push(root.join("System/Library/Frameworks"));
    dirs.push(root.join("Library/Frameworks"));
    for d in dirs {
        let base = d.join(format!("{name}.framework"));
        for f in [base.join(format!("{name}.tbd")), base.join(name)] {
            if f.is_file() {
                return Some(f);
            }
        }
    }
    None
}

/// Link `o.out`.
pub fn link(o: &Options) -> Result<(), String> {
    link_with(o, None)
}

/// Link `o.out`, a plugin or a host with `plugin`.
pub fn link_with(
    o: &Options,
    plugin: Option<(&crate::plugin::Plugin<'_>, &crate::plugin::Glue<'_>)>,
) -> Result<(), String> {
    let mut l = Link::new(&o.arch).ok_or_else(|| format!("unknown architecture {}", o.arch))?;
    l.platform = o.platform;
    // The entry point (a dylib's exports and initializer) and `-u`
    // symbols pull archive members like any reference.
    let wanted = o.wanted();
    for n in &wanted {
        let id = l.syms.intern(n);
        l.syms.reference(id, u32::MAX, false);
    }
    for i in &o.inputs {
        match i {
            Input::File(p) => l.add_file(p, o)?,
            Input::Lib(n) => {
                let p = find_library(n, o).ok_or_else(|| format!("library not found for -l{n}"))?;
                l.add_file(&p, o)?;
            }
            Input::Framework(n) => {
                let p = find_framework(n, o)
                    .ok_or_else(|| format!("framework not found for -framework {n}"))?;
                l.add_dylib(&p, o)?;
            }
        }
    }
    // Libraries objects ask for (`LC_LINKER_OPTION`), until none is new.
    let mut done = 0;
    while done < l.files.len() {
        let opts: Vec<Vec<String>> = l.files[done].linker_options.clone();
        done += 1;
        for opt in opts {
            let p = match opt.as_slice() {
                [f, n] if f == "-framework" => find_framework(n, o),
                [lib] if lib.starts_with("-l") => find_library(&lib[2..], o),
                _ => None,
            };
            if let Some(p) = p {
                l.add_file(&p, o)?;
            }
        }
    }
    let keep = match plugin {
        Some((p, glue)) => crate::plugin::prepare(&mut l, p, glue)?,
        None => Vec::new(),
    };
    for n in ["__mh_execute_header", "___dso_handle"] {
        let id = l.syms.intern(n);
        if matches!(l.syms.get(id).def, Def::Undefined | Def::Lazy { .. }) {
            l.syms
                .define(id, Def::Synthetic, u32::MAX, false, n == "___dso_handle")
                .map_err(|_| format!("{n} is defined twice"))?;
        }
    }
    let dylibs = &l.dylibs;
    l.syms.bind_dylibs(|n| {
        dylibs
            .iter()
            .position(|d| d.exports.contains(n))
            .map(|i| i as u32)
    });
    write::commons(&mut l);
    let mut roots: Vec<Target> = wanted
        .iter()
        .map(|n| Target::Sym(l.syms.intern(n)))
        .collect();
    roots.extend(keep.into_iter().map(Target::Sym));
    for (i, s) in l.syms.syms.iter().enumerate() {
        if s.no_dead_strip {
            roots.push(Target::Sym(i as u32));
        }
    }
    crate::gc::mark(&mut l.atoms, &l.syms, &roots, !o.dead_strip);
    check_undefined(&l, o)?;
    if o.icf {
        fold_code(&mut l, o, &roots);
    }
    write::write(&mut l, o)
}

/// The atoms as identical code folding sees them.
struct Fold<'a> {
    l: &'a Link,
    unwind: HashMap<AtomId, (u32, Option<Target>)>,
    canon: &'a HashMap<AtomId, AtomId>,
}
impl crate::icf::View for Fold<'_> {
    fn bytes(&self, a: AtomId) -> &[u8] {
        let at = &self.l.atoms[a as usize];
        let f = &self.l.files[at.file as usize];
        let start = f.sects[at.sect as usize].offset as usize + at.off as usize;
        &f.data[start..start + at.size as usize]
    }
    fn extra(&self, a: AtomId) -> u64 {
        let at = &self.l.atoms[a as usize];
        let s = &self.l.files[at.file as usize].sects[at.sect as usize];
        let mut h = std::hash::DefaultHasher::new();
        (&s.seg, &s.name, s.flags).hash(&mut h);
        self.unwind.get(&a).hash(&mut h);
        h.finish()
    }
    fn dest(&self, t: Target) -> crate::icf::Dest {
        match self.l.syms.resolve(t) {
            Some((atom, off)) => crate::icf::Dest::In {
                atom: self.canon.get(&atom).copied().unwrap_or(atom),
                off,
                tag: 0,
            },
            None => match t {
                Target::Sym(id) => crate::icf::Dest::Other(u64::from(id)),
                Target::Atom(a) => crate::icf::Dest::Other(u64::from(a) | (1 << 32)),
            },
        }
    }
}

/// Fold identical code: functions whose address is never taken, or
/// nabic's function value thunks, with the same bytes and relocations,
/// become one, and nabic's string
/// literals (named by their content, their addresses meaningless) with
/// the same bytes. A copy's unwind entry goes with it, and the debug map
/// leaves it out, so the `.dSYM` places its DWARF at 0, like dead code's.
fn fold_code(l: &mut Link, o: &Options, roots: &[Target]) {
    let mut taken = crate::icf::address_taken(
        &l.atoms,
        &l.syms,
        |_| true,
        |a, r| {
            let at = &l.atoms[a as usize];
            r.kind == a64::BRANCH26 && l.files[at.file as usize].sects[at.sect as usize].code()
        },
    );
    // Function value thunks: local, each its own atom.
    for f in &l.files {
        for (name, a, off) in &f.locals {
            let at = &l.atoms[*a as usize];
            if *off == 0
                && f.sects[at.sect as usize].code()
                && crate::icf::fn_value_thunk(name.trim_start_matches('_'))
            {
                taken[*a as usize] = false;
            }
        }
    }
    for &t in roots {
        if let Some((a, _)) = l.syms.resolve(t) {
            taken[a as usize] = true;
        }
    }
    for s in &l.syms.syms {
        if let Def::Defined { atom, .. } = s.def {
            if !o.no_exports && !s.private_extern || s.no_dead_strip {
                taken[atom as usize] = true;
            }
        }
    }
    // An LSDA keeps its function, as in lld; else the encodings must
    // match.
    let mut unwind: HashMap<AtomId, (u32, Option<Target>)> = HashMap::default();
    for c in &l.compact {
        if c.lsda.is_some() || c.func.1 != 0 {
            taken[c.func.0 as usize] = true;
        }
        unwind.insert(c.func.0, (c.enc, c.personality));
    }
    for f in &l.fdes {
        if f.lsda.is_some() || f.func.1 != 0 {
            taken[f.func.0 as usize] = true;
        }
    }
    let bytes = |l: &Link, a: AtomId| -> std::ops::Range<usize> {
        let at = &l.atoms[a as usize];
        let s = &l.files[at.file as usize].sects[at.sect as usize];
        let start = s.offset as usize + at.off as usize;
        start..start + at.size as usize
    };
    let cands: Vec<AtomId> = (0..l.atoms.len() as AtomId)
        .filter(|&a| {
            let at = &l.atoms[a as usize];
            let s = &l.files[at.file as usize].sects[at.sect as usize];
            at.live
                && !at.root
                && !taken[a as usize]
                && at.size > 0
                && s.kind == object::Kind::Atoms
                && s.code()
                && !s.zerofill()
        })
        .collect();
    // String literals: one per content.
    let mut canon: HashMap<AtomId, AtomId> = HashMap::default();
    {
        let mut first: HashMap<&[u8], AtomId> = HashMap::default();
        for f in &l.files {
            for (name, a, off) in &f.locals {
                let at = &l.atoms[*a as usize];
                let s = &f.sects[at.sect as usize];
                if *off != 0
                    || !at.live
                    || !name.trim_start_matches('_').starts_with("nabi.str.")
                    || s.code()
                {
                    continue;
                }
                if s.seg != "__TEXT" || s.name != "__const" {
                    continue;
                }
                let c = *first.entry(&f.data[bytes(l, *a)]).or_insert(*a);
                if c != *a {
                    canon.insert(*a, c);
                }
            }
        }
    }
    let v = Fold {
        l,
        unwind,
        canon: &canon,
    };
    let mut folds = crate::icf::fold(&l.atoms, &cands, &v);
    drop(v);
    folds.extend(canon);
    crate::icf::apply(&mut l.atoms, &folds);
    l.folded = folds;
}

/// Undefined symbols live code references, as lld reports them.
fn check_undefined(l: &Link, o: &Options) -> Result<(), String> {
    let mut bad: Vec<(u32, u32)> = Vec::new();
    let mut seen = nabic_core::HashSet::default();
    let mut note = |id: u32, fi: u32| {
        if seen.insert(id) {
            bad.push((id, fi));
        }
    };
    let undef = |t: Target| match t {
        Target::Sym(id) => {
            matches!(l.syms.get(id).def, Def::Undefined | Def::Lazy { .. }).then_some(id)
        }
        Target::Atom(_) => None,
    };
    for a in l.atoms.iter().filter(|a| a.live) {
        for r in &a.relocs {
            for t in std::iter::once(r.target).chain(r.sub) {
                if let Some(id) = undef(t) {
                    note(id, a.file);
                }
            }
        }
        for &k in &a.keep {
            if let Some(id) = undef(k) {
                note(id, a.file);
            }
        }
    }
    for n in o.wanted() {
        let id = l.syms.find(&n).expect("interned");
        if !matches!(l.syms.get(id).def, Def::Defined { .. }) {
            note(id, u32::MAX);
        }
    }
    if bad.is_empty() {
        return Ok(());
    }
    let mut msg = String::new();
    for (id, fi) in bad.iter().take(20) {
        if !msg.is_empty() {
            msg.push('\n');
        }
        let by = if *fi == u32::MAX {
            "the command line (-e, -exported_symbol, -init)".to_owned()
        } else {
            l.file_name(*fi)
        };
        msg.push_str(&format!(
            "undefined symbol: {}\n>>> referenced by {by}",
            l.syms.get(*id).name
        ));
    }
    if bad.len() > 20 {
        msg.push_str(&format!("\n>>> and {} more", bad.len() - 20));
    }
    Err(msg)
}
