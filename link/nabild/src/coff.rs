//! The COFF linker: `x86_64` and arm64 Windows executables (PE32+) and
//! x86 ones (PE32) from lld-link's command line (the subset nabic
//! passes), against import libraries. Also the import library writer.

pub mod defs;
pub mod import;
pub mod object;

mod cv;
mod pdb;
mod write;

use crate::symtab::{Atom, AtomId, Def, Fetch, SymId, SymbolTable, Target};
use nabic_core::{HashMap, HashSet};
use object::{InSect, ObjFile};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const MACHINE_AMD64: u16 = 0x8664;
pub const MACHINE_ARM64: u16 = 0xaa64;
pub const MACHINE_I386: u16 = 0x14c;

pub const SCN_CNT_CODE: u32 = 0x20;
pub const SCN_CNT_INITIALIZED_DATA: u32 = 0x40;
pub const SCN_CNT_UNINITIALIZED_DATA: u32 = 0x80;
pub const SCN_LNK_INFO: u32 = 0x200;
pub const SCN_LNK_REMOVE: u32 = 0x800;
pub const SCN_LNK_COMDAT: u32 = 0x1000;
pub const SCN_MEM_DISCARDABLE: u32 = 0x0200_0000;
pub const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
pub const SCN_MEM_READ: u32 = 0x4000_0000;
pub const SCN_MEM_WRITE: u32 = 0x8000_0000;

pub const CODE: u32 = SCN_CNT_CODE | SCN_MEM_EXECUTE | SCN_MEM_READ;
pub const RDATA: u32 = SCN_CNT_INITIALIZED_DATA | SCN_MEM_READ;
pub const DATA: u32 = RDATA | SCN_MEM_WRITE;
pub const BSS: u32 = SCN_CNT_UNINITIALIZED_DATA | SCN_MEM_READ | SCN_MEM_WRITE;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arch {
    X86_64,
    Aarch64,
    X86,
}

impl Arch {
    #[must_use]
    pub const fn machine(self) -> u16 {
        match self {
            Self::X86_64 => MACHINE_AMD64,
            Self::Aarch64 => MACHINE_ARM64,
            Self::X86 => MACHINE_I386,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x64",
            Self::Aarch64 => "arm64",
            Self::X86 => "x86",
        }
    }

    /// PE32+ rather than PE32.
    #[must_use]
    pub const fn is64(self) -> bool {
        !matches!(self, Self::X86)
    }

    /// The symbol of C name `name`: x86 prefixes an `_`.
    #[must_use]
    pub fn mangle(self, name: &str) -> String {
        if self == Self::X86 {
            format!("_{name}")
        } else {
            name.to_owned()
        }
    }
}

/// An input file and whether all its members load.
#[derive(Clone, Debug)]
pub struct Input {
    pub path: PathBuf,
    pub whole_archive: bool,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Options {
    pub arch: Arch,
    pub out: PathBuf,
    pub lib_dirs: Vec<PathBuf>,
    pub inputs: Vec<Input>,
    /// The entry symbol (`/entry` mangled).
    pub entry: String,
    /// `IMAGE_SUBSYSTEM_*`.
    pub subsystem: u16,
    pub stack_reserve: u64,
    pub stack_commit: u64,
    pub includes: Vec<String>,
    /// `/opt:ref`.
    pub gc: bool,
    /// `/opt:icf`: fold identical code ([`fold_code`]); by default with
    /// `/opt:ref`.
    pub icf: bool,
    pub debug: bool,
    pub repro: bool,
    pub dynamic_base: bool,
    pub nx_compat: bool,
    pub high_entropy_va: bool,
    pub large_address_aware: bool,
    /// `/nodefaultlib`: every default library ignored.
    pub no_default_libs: bool,
    /// `/nodefaultlib:<name>`, lower case without `.lib`.
    pub no_default: HashSet<String>,
    /// `-lldmingw`.
    pub mingw: bool,
    pub export_all: bool,
    /// `/dll`: a DLL exporting `exports` (`/export:`), its import library
    /// at `implib` (`/implib:`).
    pub dll: bool,
    pub exports: Vec<String>,
    pub implib: Option<PathBuf>,
    /// `/nabianchor:<word>=<symbol>`: the linker writes the symbol's
    /// address less the word's own into the pointer-sized word.
    pub anchors: Vec<(String, String)>,
}

pub const SUBSYSTEM_WINDOWS_GUI: u16 = 2;
pub const SUBSYSTEM_WINDOWS_CUI: u16 = 3;

fn parse_num(s: &str) -> Option<u64> {
    s.strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .map_or_else(|| s.parse().ok(), |h| u64::from_str_radix(h, 16).ok())
}

/// A library name as `/defaultlib` and `/nodefaultlib` compare it.
fn lib_key(name: &str) -> String {
    let n = name.to_ascii_lowercase();
    n.strip_suffix(".lib").map_or(n.clone(), str::to_owned)
}

/// Parse lld-link arguments (the subset nabic passes). Flags start with
/// `/` or `-` and are matched case-insensitively; anything else, or a
/// `/` path that names no flag, is an input file.
pub fn parse_args<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Result<Options, String> {
    let mut o = Options {
        arch: Arch::X86_64,
        out: PathBuf::new(),
        lib_dirs: Vec::new(),
        inputs: Vec::new(),
        entry: String::new(),
        subsystem: 0,
        stack_reserve: 0x10_0000,
        stack_commit: 0x1000,
        includes: Vec::new(),
        gc: true,
        icf: false,
        debug: false,
        repro: false,
        dynamic_base: true,
        nx_compat: true,
        high_entropy_va: true,
        large_address_aware: true,
        no_default_libs: false,
        no_default: HashSet::default(),
        mingw: false,
        export_all: false,
        dll: false,
        exports: Vec::new(),
        implib: None,
        anchors: Vec::new(),
    };
    let mut machine = None;
    let mut gc = None;
    let mut icf = None;
    let mut high_entropy_va = None;
    let mut large_address_aware = None;
    let args: Vec<OsString> = args.iter().map(|a| a.as_ref().to_owned()).collect();
    for a in &args {
        let s = a.to_string_lossy();
        let flag = s
            .strip_prefix('/')
            .or_else(|| s.strip_prefix('-'))
            .map(|f| match f.split_once(':') {
                Some((k, v)) => (k.to_ascii_lowercase(), Some(v)),
                None => (f.to_ascii_lowercase(), None),
            });
        let input = |o: &mut Options, whole| {
            o.inputs.push(Input {
                path: PathBuf::from(a),
                whole_archive: whole,
            });
        };
        let Some((k, v)) = flag else {
            input(&mut o, false);
            continue;
        };
        let need = |v: Option<&'_ str>| -> Result<String, String> {
            v.map(str::to_owned)
                .ok_or_else(|| format!("{s}: missing argument"))
        };
        let yes_no = |v: Option<&str>| match v {
            None => Ok(true),
            Some(x) if x.eq_ignore_ascii_case("no") => Ok(false),
            Some(_) => Err(format!("{s}: expected no argument or `:no`")),
        };
        match k.as_str() {
            "nologo" => {}
            "nodefaultlib" => match v {
                None => o.no_default_libs = true,
                Some(l) => {
                    o.no_default.insert(lib_key(l));
                }
            },
            "dynamicbase" => o.dynamic_base = yes_no(v)?,
            "nxcompat" => o.nx_compat = yes_no(v)?,
            "highentropyva" => high_entropy_va = Some(yes_no(v)?),
            "largeaddressaware" => large_address_aware = Some(yes_no(v)?),
            // No `/safeseh` table: nothing here registers SEH handlers.
            "safeseh" => {
                if yes_no(v)? {
                    return Err("/safeseh is not supported: pass /safeseh:no".to_owned());
                }
            }
            "machine" => {
                machine = Some(match need(v)?.to_ascii_lowercase().as_str() {
                    "x64" | "amd64" => Arch::X86_64,
                    "arm64" => Arch::Aarch64,
                    "x86" | "i386" => Arch::X86,
                    m => return Err(format!("/machine:{m}: unsupported machine")),
                });
            }
            "out" => o.out = PathBuf::from(need(v)?),
            "libpath" => o.lib_dirs.push(PathBuf::from(need(v)?)),
            "opt" => {
                for x in need(v)?.split(',') {
                    match x.to_ascii_lowercase().as_str() {
                        "ref" => gc = Some(true),
                        "noref" => gc = Some(false),
                        // Only what is safe is ever folded.
                        "icf" | "safeicf" => icf = Some(true),
                        "noicf" => icf = Some(false),
                        _ => return Err(format!("/opt:{x}: unsupported option")),
                    }
                }
            }
            "subsystem" => {
                let v = need(v)?;
                let v = v.as_str();
                let name = v.split(',').next().unwrap_or("");
                o.subsystem = match name.to_ascii_lowercase().as_str() {
                    "console" => SUBSYSTEM_WINDOWS_CUI,
                    "windows" => SUBSYSTEM_WINDOWS_GUI,
                    _ => return Err(format!("/subsystem:{v}: unsupported subsystem")),
                };
                if v.contains(',') {
                    return Err(format!("/subsystem:{v}: a version is not supported"));
                }
            }
            "entry" => o.entry = need(v)?,
            "stack" => {
                let v = need(v)?;
                let v = v.as_str();
                let (r, c) = v.split_once(',').map_or((v, None), |(r, c)| (r, Some(c)));
                o.stack_reserve = parse_num(r).ok_or_else(|| format!("{s}: bad number"))?;
                if let Some(c) = c {
                    o.stack_commit = parse_num(c).ok_or_else(|| format!("{s}: bad number"))?;
                }
            }
            "include" => o.includes.push(need(v)?),
            "brepro" => o.repro = true,
            "debug" => {
                if v.is_some_and(|x| !x.eq_ignore_ascii_case("full")) {
                    return Err(format!("{s}: unsupported option"));
                }
                o.debug = true;
            }
            "wholearchive" => {
                let p = need(v)?;
                o.inputs.push(Input {
                    path: PathBuf::from(p),
                    whole_archive: true,
                });
            }
            "lldmingw" => o.mingw = true,
            "dll" => o.dll = true,
            "export" => o.exports.push(need(v)?),
            "implib" => o.implib = Some(PathBuf::from(need(v)?)),
            "nabianchor" => {
                let v = need(v)?;
                let (a, t) = v
                    .split_once('=')
                    .ok_or_else(|| format!("{s}: expected <word>=<symbol>"))?;
                o.anchors.push((a.to_owned(), t.to_owned()));
            }
            "export-all-symbols" => o.export_all = true,
            // An absolute path on Unix.
            _ if s.starts_with('/') && Path::new(a).exists() => input(&mut o, false),
            _ => return Err(format!("unknown argument: {s}")),
        }
    }
    o.arch = machine.ok_or("no /machine")?;
    // lld-link's defaults: both for 64-bit images only.
    o.high_entropy_va = o.arch.is64() && high_entropy_va.unwrap_or(true);
    o.large_address_aware = large_address_aware.unwrap_or_else(|| o.arch.is64());
    if o.out.as_os_str().is_empty() {
        return Err("no /out".to_owned());
    }
    if o.entry.is_empty() {
        return Err("no /entry".to_owned());
    }
    o.entry = o.arch.mangle(&o.entry);
    if o.subsystem == 0 {
        return Err("no /subsystem".to_owned());
    }
    if o.export_all && !o.mingw {
        return Err("/export-all-symbols needs -lldmingw".to_owned());
    }
    // lld-link: /debug keeps unreferenced code unless /opt:ref says.
    o.gc = gc.unwrap_or(!o.debug);
    o.icf = icf.unwrap_or(o.gc);
    Ok(o)
}

/// An input archive: its bytes and which members are loaded.
struct ArchiveIn {
    path: PathBuf,
    data: Vec<u8>,
    members: Vec<(String, std::ops::Range<usize>)>,
    loaded: Vec<bool>,
}

/// A function or variable imported from a DLL.
#[derive(Debug)]
pub struct Import {
    pub dll: String,
    /// The symbol without `__imp_`, which orders a DLL's imports.
    pub sym: String,
    /// The name looked up in the DLL; `None`: by ordinal.
    pub name: Option<String>,
    /// The ordinal, or the hint of a name.
    pub ordinal: u16,
    /// The IAT and lookup table slots, the hint / name entry.
    pub iat: AtomId,
    pub ilt: AtomId,
    pub hint_name: Option<AtomId>,
}

/// An entry of the export table.
#[derive(Clone, Debug)]
pub struct Export {
    pub name: String,
    pub sym: SymId,
    pub data: bool,
}

/// How a still undefined symbol resolves: a weak external's default or
/// an `/alternatename`.
#[derive(Clone, Copy, Debug)]
pub struct Alias {
    pub target: Target,
    pub off: u64,
}

pub struct Link {
    pub arch: Arch,
    pub atoms: Vec<Atom>,
    pub syms: SymbolTable,
    /// File 0 holds the linker's own sections.
    pub files: Vec<ObjFile>,
    archives: Vec<ArchiveIn>,
    /// COMDAT leader names and the size of the kept section.
    pub comdats: HashMap<String, u64>,
    pub aliases: HashMap<SymId, Alias>,
    pub imports: Vec<Import>,
    pub exports: Vec<Export>,
    /// `/include` symbols of objects.
    pub includes: Vec<SymId>,
    /// `/merge:<from>=<to>` of objects.
    pub merges: Vec<(String, String)>,
    pub default_libs: Vec<String>,
    /// `/failifmismatch` keys: value and the file that set it.
    pub mismatch: HashMap<String, (String, String)>,
    pub errors: Vec<String>,
    /// Absolute local symbols' stand-ins.
    pub abs: HashMap<u64, SymId>,
    /// `/debug`: objects keep their `CodeView` for the PDB.
    pub debug: bool,
    /// Folded atoms and the copy each became.
    pub folded: Vec<(AtomId, AtomId)>,
}

/// The atom standing for discarded and dead targets.
pub const DEAD: AtomId = 0;

impl Link {
    #[must_use]
    pub fn new(arch: Arch) -> Self {
        Self {
            arch,
            atoms: vec![Atom::new(u32::MAX, 0, 0, 0, 0)],
            syms: SymbolTable::default(),
            files: vec![ObjFile {
                name: "<linker>".to_owned(),
                archive: String::new(),
                data: Vec::new(),
                sects: Vec::new(),
                cv: Vec::new(),
                locals: Vec::new(),
            }],
            archives: Vec::new(),
            comdats: HashMap::default(),
            aliases: HashMap::default(),
            imports: Vec::new(),
            exports: Vec::new(),
            includes: Vec::new(),
            merges: Vec::new(),
            default_libs: Vec::new(),
            mismatch: HashMap::default(),
            errors: Vec::new(),
            abs: HashMap::default(),
            debug: false,
            folded: Vec::new(),
        }
    }

    #[must_use]
    pub fn file_name(&self, fi: u32) -> String {
        self.files
            .get(fi as usize)
            .map_or_else(|| "<internal>".to_owned(), |f| f.name.clone())
    }

    /// The input section of atom `a`.
    #[must_use]
    pub fn sect(&self, a: AtomId) -> &InSect {
        let a = &self.atoms[a as usize];
        &self.files[a.file as usize].sects[a.sect as usize]
    }

    /// The bytes of atom `a` (empty for zero fill).
    #[must_use]
    pub fn bytes(&self, a: AtomId) -> &[u8] {
        let at = &self.atoms[a as usize];
        let f = &self.files[at.file as usize];
        let s = &f.sects[at.sect as usize];
        if s.zerofill() {
            return &[];
        }
        &f.data[s.off..s.off + s.size as usize]
    }

    /// A section of the linker's own: `data` (zero fill when `chars` say
    /// uninitialized) of `size` bytes, aligned to `1 << align`.
    pub fn synth(&mut self, name: &str, chars: u32, align: u8, data: Vec<u8>, size: u64) -> AtomId {
        let f = &mut self.files[0];
        let off = f.data.len();
        let zero = chars & SCN_CNT_UNINITIALIZED_DATA != 0;
        if !zero {
            f.data.extend_from_slice(&data);
        }
        let si = f.sects.len() as u32;
        f.sects.push(InSect {
            name: name.to_owned(),
            chars,
            off,
            size: if zero { size } else { data.len() as u64 },
            comdat: false,
            atom: self.atoms.len() as AtomId,
            literal: false,
            thunk: false,
        });
        let id = self.atoms.len() as AtomId;
        self.atoms
            .push(Atom::new(0, si, 0, f.sects[si as usize].size, align));
        id
    }

    /// The stand-in symbol of absolute value `v`.
    pub fn absolute(&mut self, v: u64) -> SymId {
        if let Some(&id) = self.abs.get(&v) {
            return id;
        }
        let id = self.syms.intern(&format!("\0abs.{v:x}"));
        self.syms.get_mut(id).def = Def::Absolute(v);
        self.abs.insert(v, id);
        id
    }

    fn load_member(&mut self, (ai, mi): Fetch) -> Result<Vec<Fetch>, String> {
        let a = &mut self.archives[ai as usize];
        if a.loaded[mi as usize] {
            return Ok(Vec::new());
        }
        a.loaded[mi as usize] = true;
        let (name, range) = a.members[mi as usize].clone();
        let data = a.data[range].to_vec();
        let path = a.path.display().to_string();
        let shown = format!("{path}({name})");
        self.add_bytes(shown, path, data)
    }

    /// An object or a short import member.
    pub(crate) fn add_bytes(
        &mut self,
        name: String,
        archive: String,
        data: Vec<u8>,
    ) -> Result<Vec<Fetch>, String> {
        if import::is_short_import(&data) {
            import::load(self, &name, &data)?;
            return Ok(Vec::new());
        }
        let fi = self.files.len() as u32;
        object::parse(self, fi, name, archive, data)
    }

    pub(crate) fn drain(&mut self, mut work: Vec<Fetch>) -> Result<(), String> {
        while let Some(f) = work.pop() {
            work.extend(self.load_member(f)?);
        }
        Ok(())
    }

    fn add_file(&mut self, path: &Path, whole: bool) -> Result<(), String> {
        let raw =
            std::fs::read(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        if raw.starts_with(crate::archive::MAGIC) {
            return self.add_archive(path, raw, whole);
        }
        if raw.starts_with(b"!<thin>\n") {
            return Err(format!(
                "{}: thin archives are not supported",
                path.display()
            ));
        }
        let f = self.add_bytes(path.display().to_string(), String::new(), raw)?;
        self.drain(f)
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
        if symbols.is_empty() && !all {
            for (i, (_, r)) in members.iter().enumerate() {
                for s in import::member_symbols(&data[r.clone()]) {
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
            work.extend((0..n as u32).rev().map(|m| (ai, m)));
        } else {
            for (name, m) in symbols {
                if let Some(f) = self.syms.lazy(&name, ai, m as u32) {
                    work.push(f);
                }
            }
        }
        self.drain(work)
    }

    /// A strong reference from the command line or a directive.
    fn require(&mut self, name: &str) -> Result<SymId, String> {
        let id = self.syms.intern(name);
        if let Some(f) = self.syms.reference(id, u32::MAX, false) {
            self.drain(vec![f])?;
        }
        Ok(id)
    }
}

/// `name` in the library search path: as given, then each `/libpath`
/// directory, then `LIB`'s.
fn find_file(name: &Path, o: &Options) -> Option<PathBuf> {
    if name.is_file() {
        return Some(name.to_owned());
    }
    if name.is_absolute() {
        return None;
    }
    let env = std::env::var_os("LIB").unwrap_or_default();
    let lib: Vec<PathBuf> = std::env::split_paths(&env)
        .filter(|d| !d.as_os_str().is_empty())
        .collect();
    o.lib_dirs
        .iter()
        .chain(&lib)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

/// C names the linker defines when they are referenced: the image base
/// and the control flow guard tables (empty), x86's empty `/safeseh`
/// table.
const LINKER_DEFINED: &[&str] = &[
    "__ImageBase",
    "__guard_fids_table",
    "__guard_fids_count",
    "__guard_flags",
    "__guard_iat_table",
    "__guard_iat_count",
    "__guard_longjmp_table",
    "__guard_longjmp_count",
    "__guard_eh_cont_table",
    "__guard_eh_cont_count",
    "__enclave_config",
    "__dynamic_value_reloc_table",
    "__safe_se_handler_table",
    "__safe_se_handler_count",
];

/// Link `o.out`.
pub fn link(o: &Options) -> Result<(), String> {
    link_with(o, None)
}

/// Link `o.out`, a plugin or a host with `plugin`.
pub fn link_with(
    o: &Options,
    plugin: Option<(&crate::plugin::Plugin<'_>, &crate::plugin::Glue<'_>)>,
) -> Result<(), String> {
    let mut l = Link::new(o.arch);
    l.debug = o.debug;
    let entry = l.require(&o.entry)?;
    let mut roots = vec![Target::Sym(entry)];
    for n in &o.includes {
        roots.push(Target::Sym(l.require(n)?));
    }
    for (a, t) in &o.anchors {
        roots.push(Target::Sym(l.require(a)?));
        roots.push(Target::Sym(l.require(t)?));
    }
    let mut exported = Vec::new();
    for n in &o.exports {
        exported.push((n.clone(), l.require(&o.arch.mangle(n))?));
    }
    for i in &o.inputs {
        let p = find_file(&i.path, o)
            .ok_or_else(|| format!("could not open '{}': no such file", i.path.display()))?;
        l.add_file(&p, i.whole_archive)?;
        if !l.errors.is_empty() {
            return Err(l.errors.join("\n"));
        }
    }
    // Default libraries the objects ask for, unless told not to.
    let mut seen = HashSet::default();
    while let Some(lib) = l
        .default_libs
        .iter()
        .find(|n| !seen.contains(&lib_key(n)))
        .cloned()
    {
        let key = lib_key(&lib);
        seen.insert(key.clone());
        if o.no_default_libs || o.no_default.contains(&key) {
            continue;
        }
        let file = if Path::new(&lib).extension().is_some() {
            lib.clone()
        } else {
            format!("{lib}.lib")
        };
        if let Some(p) = find_file(Path::new(&file), o) {
            l.add_file(&p, false)?;
        }
    }
    if !l.errors.is_empty() {
        return Err(l.errors.join("\n"));
    }
    if let Some((p, glue)) = plugin {
        roots.extend(
            crate::plugin::prepare(&mut l, p, glue)?
                .into_iter()
                .map(Target::Sym),
        );
    }
    resolve_aliases(&mut l)?;
    for n in LINKER_DEFINED {
        if let Some(id) = l.syms.find(&o.arch.mangle(n)) {
            let s = l.syms.get(id);
            if s.referenced && matches!(s.def, Def::Undefined | Def::Lazy { .. }) {
                let def = if *n == "__ImageBase" {
                    Def::Synthetic
                } else {
                    Def::Absolute(0)
                };
                let _ = l.syms.define(id, def, u32::MAX, false, true);
            }
        }
    }
    write::local_imports(&mut l);
    write::commons(&mut l);
    for &id in &l.includes {
        roots.push(Target::Sym(id));
    }
    if o.export_all {
        write::export_all(&mut l);
    }
    for (name, sym) in exported {
        l.exports.push(Export {
            name,
            sym,
            data: false,
        });
    }
    for e in &l.exports {
        roots.push(Target::Sym(e.sym));
    }
    // Non-COMDAT sections always stay; COMDATs when something reaches
    // them.
    crate::gc::mark(&mut l.atoms, &l.syms, &roots, !o.gc);
    l.atoms[DEAD as usize].live = false;
    check_undefined(&l, entry)?;
    for e in &l.exports {
        if !matches!(l.syms.get(e.sym).def, Def::Defined { .. }) {
            return Err(format!(
                "undefined symbol: {}\n>>> exported by /export:{}",
                l.syms.get(e.sym).name,
                e.name
            ));
        }
    }
    if o.icf {
        fold_code(&mut l, &roots);
    }
    let r = write::write(&mut l, o);
    let _ = std::thread::Builder::new().spawn(move || drop(l));
    r
}

/// Whether relocation `r` of code atom `a` only calls or jumps to its
/// target.
fn calls(l: &Link, a: AtomId, r: &crate::symtab::Reloc) -> bool {
    const AMD64_REL32: u16 = 4;
    const ARM64_BRANCH26: u16 = 3;
    const ARM64_BRANCH19: u16 = 0xf;
    const ARM64_BRANCH14: u16 = 0x10;
    const I386_REL32: u16 = 0x14;
    match l.arch {
        Arch::X86_64 => r.kind == AMD64_REL32 && crate::icf::x86_branch(l.bytes(a), r.off),
        Arch::X86 => r.kind == I386_REL32 && crate::icf::x86_branch(l.bytes(a), r.off),
        Arch::Aarch64 => matches!(r.kind, ARM64_BRANCH26 | ARM64_BRANCH19 | ARM64_BRANCH14),
    }
}

/// The atoms as identical code folding sees them.
struct Fold<'a> {
    l: &'a Link,
    canon: &'a HashMap<AtomId, AtomId>,
}
impl crate::icf::View for Fold<'_> {
    fn bytes(&self, a: AtomId) -> &[u8] {
        self.l.bytes(a)
    }
    fn extra(&self, a: AtomId) -> u64 {
        u64::from(self.l.sect(a).chars)
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
/// literals with the same bytes. A copy's associated sections (its
/// `.pdata`, `.xdata` and `CodeView`) go with it.
fn fold_code(l: &mut Link, roots: &[Target]) {
    let pdata = |l: &Link, a: AtomId| l.sect(a).name == ".pdata";
    let mut taken = crate::icf::address_taken(
        &l.atoms,
        &l.syms,
        |a| a != DEAD && !pdata(l, a),
        |a, r| l.sect(a).code() && calls(l, a, r),
    );
    // Function value thunks (a COMDAT each).
    for (a, t) in taken.iter_mut().enumerate().skip(1) {
        if l.atoms[a].file != u32::MAX && l.sect(a as AtomId).thunk {
            *t = false;
        }
    }
    for &t in roots {
        if let Some((a, _)) = l.syms.resolve(t) {
            taken[a as usize] = true;
        }
    }
    // Unwind info with a handler (relocations of its own) keeps its
    // function, as an LSDA does in lld; so does any other associated
    // section.
    for (a, t) in taken.iter_mut().enumerate().skip(1) {
        let own = l.atoms[a].keep.iter().all(|&k| {
            let Target::Atom(k) = k else {
                return false;
            };
            let s = l.sect(k);
            match s.name.as_str() {
                ".xdata" => l.atoms[k as usize].relocs.is_empty(),
                ".pdata" => l.atoms[k as usize].relocs.iter().all(|r| {
                    l.syms.resolve(r.target).is_some_and(|(t, _)| {
                        t == a as AtomId
                            || l.sect(t).name == ".xdata" && l.atoms[t as usize].relocs.is_empty()
                    })
                }),
                _ => false,
            }
        });
        if !own {
            *t = true;
        }
    }
    let cands: Vec<AtomId> = (1..l.atoms.len() as AtomId)
        .filter(|&a| {
            let at = &l.atoms[a as usize];
            let s = l.sect(a);
            at.live && !at.root && !taken[a as usize] && at.size > 0 && s.code() && !s.zerofill()
        })
        .collect();
    let mut canon: HashMap<AtomId, AtomId> = HashMap::default();
    {
        let mut first: HashMap<&[u8], AtomId> = HashMap::default();
        for a in 1..l.atoms.len() as AtomId {
            let s = l.sect(a);
            if !l.atoms[a as usize].live || !s.literal || s.code() || s.zerofill() {
                continue;
            }
            let c = *first.entry(l.bytes(a)).or_insert(a);
            if c != a {
                canon.insert(a, c);
            }
        }
    }
    let v = Fold { l, canon: &canon };
    let mut folds = crate::icf::fold(&l.atoms, &cands, &v);
    for &(dup, _) in &folds {
        for k in l.atoms[dup as usize].keep.clone() {
            if let Target::Atom(k) = k {
                l.atoms[k as usize].live = false;
            }
        }
    }
    folds.extend(canon);
    crate::icf::apply(&mut l.atoms, &folds);
    l.folded = folds;
}

/// Weak externals and `/alternatename`s of symbols nothing defines: the
/// targets are fetched from archives, then the names bound to them.
fn resolve_aliases(l: &mut Link) -> Result<(), String> {
    loop {
        let mut work = Vec::new();
        let pending: Vec<(SymId, Alias)> = l
            .aliases
            .iter()
            .filter(|(id, _)| matches!(l.syms.get(**id).def, Def::Undefined | Def::Lazy { .. }))
            .map(|(id, a)| (*id, *a))
            .collect();
        for (_, a) in &pending {
            if let Target::Sym(t) = a.target {
                let s = l.syms.get(t);
                if matches!(s.def, Def::Lazy { .. }) || !s.referenced {
                    if let Some(f) = l.syms.reference(t, u32::MAX, false) {
                        work.push(f);
                    }
                }
            }
        }
        if work.is_empty() {
            break;
        }
        l.drain(work)?;
        if !l.errors.is_empty() {
            return Err(l.errors.join("\n"));
        }
    }
    // Bind, following chains of aliases.
    for _ in 0..16 {
        let mut changed = false;
        let ids: Vec<SymId> = l.aliases.keys().copied().collect();
        for id in ids {
            if !matches!(l.syms.get(id).def, Def::Undefined | Def::Lazy { .. }) {
                continue;
            }
            let a = l.aliases[&id];
            let def = match a.target {
                Target::Atom(at) => Some(Def::Defined {
                    atom: at,
                    off: a.off,
                }),
                Target::Sym(t) => match l.syms.get(t).def {
                    Def::Defined { atom, off } => Some(Def::Defined {
                        atom,
                        off: off + a.off,
                    }),
                    d @ (Def::Absolute(_) | Def::Synthetic) => Some(d),
                    _ => None,
                },
            };
            if let Some(d) = def {
                let s = l.syms.get_mut(id);
                s.def = d;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(())
}

/// Undefined symbols that live code references, as lld-link reports
/// them.
fn check_undefined(l: &Link, entry: SymId) -> Result<(), String> {
    let mut bad: Vec<(SymId, u32)> = Vec::new();
    let mut seen = HashSet::default();
    for a in l.atoms.iter().filter(|a| a.live) {
        for r in &a.relocs {
            if let Target::Sym(id) = r.target {
                let s = l.syms.get(id);
                if matches!(
                    s.def,
                    Def::Undefined | Def::Lazy { .. } | Def::Common { .. }
                ) && seen.insert(id)
                {
                    bad.push((id, a.file));
                }
            }
        }
    }
    if !matches!(l.syms.get(entry).def, Def::Defined { .. }) && seen.insert(entry) {
        bad.push((entry, u32::MAX));
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
            "the entry point".to_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_flags_nabic_passes() {
        let o = parse_args(&[
            "/nologo",
            "/nodefaultlib",
            "/dynamicbase",
            "/nxcompat",
            "/machine:arm64",
            "/out:a.exe",
            "main.obj",
            "/libpath:lib",
            "/opt:ref",
            "/subsystem:console",
            "/entry:mainCRTStartup",
            "/stack:8388608",
            "/include:_tls_used",
            "/Brepro",
            "/debug",
            "/wholearchive:nabir.lib",
        ])
        .unwrap();
        assert_eq!(o.arch, Arch::Aarch64);
        assert_eq!(o.out, PathBuf::from("a.exe"));
        assert_eq!(o.inputs.len(), 2);
        assert!(o.inputs[1].whole_archive);
        assert_eq!(o.stack_reserve, 8 << 20);
        assert_eq!(o.subsystem, SUBSYSTEM_WINDOWS_CUI);
        assert!(o.gc && o.debug && o.repro && o.no_default_libs);
        // lld-link: folding goes with `/opt:ref` unless told.
        assert!(o.icf);
        assert_eq!(o.includes, ["_tls_used"]);
        assert!(
            parse_args(&[
                "/machine:x64",
                "/out:a",
                "/entry:e",
                "/subsystem:console",
                "/frob"
            ])
            .is_err()
        );
        let o = parse_args(&[
            "/machine:x64",
            "/out:a",
            "/entry:e",
            "/subsystem:windows",
            "/debug",
        ])
        .unwrap();
        assert!(!o.gc && !o.icf);
        let o = parse_args(&[
            "/machine:x64",
            "/out:a",
            "/entry:e",
            "/subsystem:console",
            "/opt:ref,noicf",
        ])
        .unwrap();
        assert!(o.gc && !o.icf);
    }
}
