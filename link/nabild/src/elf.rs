//! The ELF linker, from ld.lld's command line (the subset nabic passes).
//!
//! `x86_64`, aarch64, i386, 32-bit ARM, riscv64, powerpc64le and s390x
//! Linux executables, a PIE against glibc's shared libraries or a static
//! `ET_EXEC` against musl, `x86_64` FreeBSD PIEs against its libc, and
//! static riscv32 images without an OS.

pub mod arch;
pub mod dso;
pub mod object;
pub mod reloc;

mod write;

use crate::elf::arch::{a64, arm32, i386, ppc64, riscv, s390x, x64};
use crate::symtab::{Atom, AtomId, Def, Fetch, SymId, SymbolTable, Target};
use dso::Dso;
use nabic_core::{HashMap, HashSet};
use object::{ObjFile, SectKind};
use std::ffi::OsString;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

pub const ELFOSABI_FREEBSD: u8 = 9;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Arch {
    X86_64,
    Aarch64,
    /// ELF32, REL relocations.
    I386,
    /// 32-bit ARM (A32 and T32 code), ELF32, REL relocations.
    Arm,
    /// RV64 (lp64d), RELA relocations.
    Riscv64,
    /// RV32 (ilp32), ELF32, RELA relocations.
    Riscv32,
    /// 64-bit little-endian POWER, `ELFv2`, RELA relocations.
    Ppc64,
    /// z/Architecture, big endian, RELA relocations.
    S390x,
}

impl Arch {
    #[must_use]
    pub const fn machine(self) -> u16 {
        match self {
            Self::X86_64 => 62,
            Self::Aarch64 => 183,
            Self::I386 => 3,
            Self::Arm => 40,
            Self::Riscv64 | Self::Riscv32 => 243,
            Self::Ppc64 => 21,
            Self::S390x => 22,
        }
    }

    /// The byte order of the inputs and the output.
    #[must_use]
    pub const fn endian(self) -> crate::bytes::Endian {
        crate::bytes::Endian {
            big: matches!(self, Self::S390x),
        }
    }

    /// ELFCLASS64.
    #[must_use]
    pub const fn elf64(self) -> bool {
        !matches!(self, Self::I386 | Self::Arm | Self::Riscv32)
    }

    /// REL relocations (addends in the places), else RELA.
    #[must_use]
    pub const fn rel(self) -> bool {
        matches!(self, Self::I386 | Self::Arm)
    }

    /// RV64 or RV32.
    #[must_use]
    pub const fn riscv(self) -> bool {
        matches!(self, Self::Riscv64 | Self::Riscv32)
    }

    /// Pointer (and GOT slot) size.
    #[must_use]
    pub const fn ptr(self) -> u64 {
        if self.elf64() { 8 } else { 4 }
    }

    /// `x86_64` or i386.
    #[must_use]
    pub const fn x86(self) -> bool {
        matches!(self, Self::X86_64 | Self::I386)
    }

    /// lld's `-z max-page-size`: the largest page the kernel may use.
    #[must_use]
    pub const fn max_page(self) -> u64 {
        match self {
            Self::X86_64 | Self::I386 | Self::Riscv64 | Self::Riscv32 | Self::S390x => 0x1000,
            Self::Aarch64 | Self::Arm | Self::Ppc64 => 0x1_0000,
        }
    }

    /// lld's image base of a static (non-PIE) executable.
    #[must_use]
    pub const fn static_base(self) -> u64 {
        match self {
            Self::Riscv64 | Self::Riscv32 => 0x1_0000,
            Self::Ppc64 => 0x1000_0000,
            Self::S390x => 0x100_0000,
            _ => 0x20_0000,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::I386 => "i386",
            Self::Arm => "arm",
            Self::Riscv64 => "riscv64",
            Self::Riscv32 => "riscv32",
            Self::Ppc64 => "ppc64le",
            Self::S390x => "s390x",
        }
    }
}

#[derive(Clone, Debug)]
pub enum InputKind {
    File(PathBuf),
    Lib(String),
}

/// An input and the position-dependent options in force for it.
#[derive(Clone, Debug)]
pub struct Input {
    pub kind: InputKind,
    pub whole_archive: bool,
    pub as_needed: bool,
    /// `-Bstatic`: `-l` finds archives only.
    pub bstatic: bool,
}

#[derive(Clone, Debug)]
#[allow(clippy::struct_excessive_bools)]
pub struct Options {
    pub arch: Arch,
    /// `EI_OSABI`: `ELFOSABI_FREEBSD` for `elf_x86_64_fbsd`, else 0.
    pub osabi: u8,
    pub out: PathBuf,
    pub pie: bool,
    /// `-static`: no shared objects, no dynamic section.
    pub static_link: bool,
    pub interp: Option<String>,
    pub lib_dirs: Vec<PathBuf>,
    pub inputs: Vec<Input>,
    pub gc: bool,
    pub strip_all: bool,
    pub eh_frame_hdr: bool,
    pub build_id: bool,
    pub export_dynamic: bool,
    pub rpaths: Vec<String>,
    /// `-z dead-reloc-in-nonalloc=<section>=<value>`.
    pub dead_reloc: Vec<(String, u64)>,
    pub entry: String,
    /// `--icf=safe`: fold identical code ([`fold_code`]).
    pub icf: bool,
    /// `-shared`: a shared object named `soname`, exporting only
    /// `exports` (`--nabi-export`), its `init` run at load (`-init`).
    pub shared: bool,
    pub soname: Option<String>,
    pub exports: Vec<String>,
    pub init: Option<String>,
    /// `--nabi-anchor=<word>=<symbol>`: the linker writes the symbol's
    /// address less the word's own into the pointer-sized word.
    pub anchors: Vec<(String, String)>,
    /// `--nabi-dynamic-tls`: a shared object reaches its own thread-locals
    /// through the loader (general dynamic, TLS descriptors), not initial
    /// exec, which bionic refuses in a `dlopen`ed library.
    pub dynamic_tls: bool,
    /// `-z max-page-size=<n>`: the largest page the kernel may use, over
    /// the architecture's.
    pub max_page: Option<u64>,
}

impl Options {
    /// What the link must define: the entry point, or a shared object's
    /// exports and initializer; and anchored words and symbols.
    #[must_use]
    pub fn wanted(&self) -> Vec<String> {
        let mut v = if self.shared {
            let mut v = self.exports.clone();
            v.extend(self.init.clone());
            v
        } else {
            vec![self.entry.clone()]
        };
        for (a, t) in &self.anchors {
            v.push(a.clone());
            v.push(t.clone());
        }
        v
    }

    /// A dynamically linked executable: interpreter, `.dynamic`.
    #[must_use]
    pub const fn dynamic(&self) -> bool {
        !self.static_link
    }
}

/// Parse ld.lld arguments (the subset nabic passes).
pub fn parse_args<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Result<Options, String> {
    let mut o = Options {
        arch: Arch::X86_64,
        osabi: 0,
        out: PathBuf::from("a.out"),
        pie: false,
        static_link: false,
        interp: None,
        lib_dirs: Vec::new(),
        inputs: Vec::new(),
        gc: false,
        strip_all: false,
        eh_frame_hdr: false,
        build_id: false,
        export_dynamic: false,
        rpaths: Vec::new(),
        dead_reloc: Vec::new(),
        entry: "_start".to_owned(),
        icf: false,
        shared: false,
        soname: None,
        exports: Vec::new(),
        init: None,
        anchors: Vec::new(),
        dynamic_tls: false,
        max_page: None,
    };
    let mut arch = None;
    let (mut whole, mut as_needed, mut bstatic) = (false, false, false);
    let args: Vec<OsString> = args.iter().map(|a| a.as_ref().to_owned()).collect();
    let mut it = args.iter();
    let next = |it: &mut std::slice::Iter<'_, OsString>, flag: &str| {
        it.next()
            .cloned()
            .ok_or_else(|| format!("{flag}: missing argument"))
    };
    while let Some(a) = it.next() {
        let s = a.to_string_lossy();
        let input = |kind| Input {
            kind,
            whole_archive: whole,
            as_needed,
            bstatic,
        };
        match s.as_ref() {
            "-m" => {
                let m = next(&mut it, "-m")?;
                arch = Some(match m.to_string_lossy().as_ref() {
                    "elf_x86_64" => Arch::X86_64,
                    "elf_x86_64_fbsd" => {
                        o.osabi = ELFOSABI_FREEBSD;
                        Arch::X86_64
                    }
                    "aarch64linux" | "aarch64elf" => Arch::Aarch64,
                    "elf_i386" => Arch::I386,
                    "armelf_linux_eabi" | "armelf" => Arch::Arm,
                    "elf64lriscv" => Arch::Riscv64,
                    "elf32lriscv" => Arch::Riscv32,
                    "elf64lppc" => Arch::Ppc64,
                    "elf64_s390" => Arch::S390x,
                    e => return Err(format!("-m {e}: unsupported emulation")),
                });
            }
            "-o" => o.out = next(&mut it, "-o")?.into(),
            "-pie" => o.pie = true,
            "-shared" => o.shared = true,
            "-soname" => o.soname = Some(next(&mut it, "-soname")?.to_string_lossy().into_owned()),
            "-init" => o.init = Some(next(&mut it, "-init")?.to_string_lossy().into_owned()),
            "--nabi-dynamic-tls" => o.dynamic_tls = true,
            _ if s.starts_with("--nabi-export=") => o.exports.push(s[14..].to_owned()),
            _ if s.starts_with("--nabi-anchor=") => {
                let (a, t) = s[14..]
                    .split_once('=')
                    .ok_or_else(|| format!("{s}: expected <word>=<symbol>"))?;
                o.anchors.push((a.to_owned(), t.to_owned()));
            }
            "-static" => {
                o.static_link = true;
                bstatic = true;
            }
            "-dynamic-linker" => {
                o.interp = Some(
                    next(&mut it, "-dynamic-linker")?
                        .to_string_lossy()
                        .into_owned(),
                );
            }
            "--eh-frame-hdr" => o.eh_frame_hdr = true,
            "--build-id" => o.build_id = true,
            "--gc-sections" => o.gc = true,
            "--strip-all" | "-s" => o.strip_all = true,
            // Only a GNU hash table is written.
            "--hash-style=gnu" => {}
            // Only what is safe is ever folded.
            "--icf=safe" | "--icf=all" => o.icf = true,
            "--icf=none" => o.icf = false,
            "--export-dynamic" | "-E" => o.export_dynamic = true,
            "--whole-archive" => whole = true,
            "--no-whole-archive" => whole = false,
            "--as-needed" => as_needed = true,
            "--no-as-needed" => as_needed = false,
            "-Bstatic" => bstatic = true,
            "-Bdynamic" => bstatic = o.static_link,
            "-rpath" => o
                .rpaths
                .push(next(&mut it, "-rpath")?.to_string_lossy().into_owned()),
            "-z" => {
                let z = next(&mut it, "-z")?.to_string_lossy().into_owned();
                if let Some(n) = z.strip_prefix("max-page-size=") {
                    let n = n
                        .strip_prefix("0x")
                        .map_or_else(|| n.parse().ok(), |h| u64::from_str_radix(h, 16).ok())
                        .filter(|n: &u64| n.is_power_of_two())
                        .ok_or_else(|| format!("-z {z}: expected a power of two"))?;
                    o.max_page = Some(n);
                    continue;
                }
                let Some(rest) = z.strip_prefix("dead-reloc-in-nonalloc=") else {
                    return Err(format!("-z {z}: unsupported option"));
                };
                let (sect, v) = rest
                    .rsplit_once('=')
                    .ok_or_else(|| format!("-z {z}: expected <section>=<value>"))?;
                let v = v
                    .strip_prefix("0x")
                    .map_or_else(|| v.parse().ok(), |h| u64::from_str_radix(h, 16).ok())
                    .ok_or_else(|| format!("-z {z}: bad value"))?;
                o.dead_reloc.push((sect.to_owned(), v));
            }
            "-L" => o.lib_dirs.push(next(&mut it, "-L")?.into()),
            "-l" => o.inputs.push(input(InputKind::Lib(
                next(&mut it, "-l")?.to_string_lossy().into_owned(),
            ))),
            _ if s.starts_with("-L") => o.lib_dirs.push(PathBuf::from(&s[2..])),
            _ if s.starts_with("-l") => o.inputs.push(input(InputKind::Lib(s[2..].to_owned()))),
            _ if s.starts_with('-') => return Err(format!("unknown argument: {s}")),
            _ => o.inputs.push(input(InputKind::File(a.into()))),
        }
    }
    o.arch = arch.ok_or("no -m emulation")?;
    if o.static_link && o.pie {
        return Err("-static -pie (a static PIE) is not supported".to_owned());
    }
    if o.shared {
        if o.static_link {
            return Err("-static -shared is not supported".to_owned());
        }
        // Position independent, as a PIE.
        o.pie = true;
    }
    if o.dynamic() && !o.pie {
        return Err("a dynamically linked executable must be a PIE (-pie)".to_owned());
    }
    Ok(o)
}

/// Per-symbol ELF facts the format-neutral table does not keep.
#[derive(Clone, Debug, Default)]
pub struct SymExt {
    /// `STT_*` of the definition.
    pub ty: u8,
    pub size: u64,
    /// The version a `name@VERSION` reference asks for.
    pub want_version: Option<String>,
    /// Version index of the definition in its shared object.
    pub dso_ver: u16,
    /// A linker-local alias of a local symbol (GOT and TLS relocations).
    pub local_alias: bool,
    /// `st_other` of the definition (POWER: the local entry point).
    pub other: u8,
}

/// An input archive: its bytes and which members are loaded.
struct ArchiveIn {
    path: PathBuf,
    data: Vec<u8>,
    members: Vec<(String, std::ops::Range<usize>)>,
    loaded: Vec<bool>,
}

pub struct Link {
    pub arch: Arch,
    pub atoms: Vec<Atom>,
    pub syms: SymbolTable,
    pub ext: Vec<SymExt>,
    pub files: Vec<ObjFile>,
    pub dsos: Vec<Dso>,
    archives: Vec<ArchiveIn>,
    /// COMDAT signatures seen.
    comdats: HashSet<String>,
    /// `.eh_frame` FDE atoms and their CIE atoms.
    pub fdes: Vec<(AtomId, AtomId)>,
    pub errors: Vec<String>,
    /// Keep non-alloc (debug) sections.
    pub keep_debug: bool,
    /// Atoms that are copies of an earlier identical string.
    pub alias: HashMap<AtomId, AtomId>,
    /// Undefined symbols of the shared objects.
    pub dso_undefined: HashSet<String>,
    /// 32-bit ARM: the code atom and `.ARM.exidx` atom of each index
    /// table section, which lives with the code.
    pub exidx: Vec<(AtomId, AtomId)>,
    /// RISC-V: the inputs' `e_flags`, merged.
    pub eflags: Option<u32>,
    /// RISC-V: each input's `.riscv.attributes`.
    pub attributes: Vec<Vec<u8>>,
    /// Keep the `__tls_get_addr` calls of general dynamic sequences: a
    /// shared object with `--nabi-dynamic-tls`.
    pub tls_calls: bool,
}

/// The atom standing for discarded and dead targets.
pub const DEAD: AtomId = 0;

impl Link {
    #[must_use]
    pub fn new(arch: Arch, keep_debug: bool) -> Self {
        Self {
            arch,
            atoms: vec![Atom::new(u32::MAX, 0, 0, 0, 0)],
            syms: SymbolTable::default(),
            ext: Vec::new(),
            files: Vec::new(),
            dsos: Vec::new(),
            archives: Vec::new(),
            comdats: HashSet::default(),
            fdes: Vec::new(),
            errors: Vec::new(),
            keep_debug,
            alias: HashMap::default(),
            dso_undefined: HashSet::default(),
            exidx: Vec::new(),
            eflags: None,
            attributes: Vec::new(),
            tls_calls: false,
        }
    }

    pub fn ext_mut(&mut self, id: SymId) -> &mut SymExt {
        let i = id as usize;
        if self.ext.len() <= i {
            self.ext.resize(i + 1, SymExt::default());
        }
        &mut self.ext[i]
    }

    #[must_use]
    pub fn ext(&self, id: SymId) -> SymExt {
        self.ext.get(id as usize).cloned().unwrap_or_default()
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
        if !data.starts_with(b"\x7fELF") {
            return Ok(Vec::new());
        }
        let shown = format!("{}({name})", a.path.display());
        let fi = self.files.len() as u32;
        object::parse(self, fi, shown, data)
    }

    pub(crate) fn drain(&mut self, mut work: Vec<Fetch>) -> Result<(), String> {
        while let Some(f) = work.pop() {
            work.extend(self.load_member(f)?);
        }
        Ok(())
    }

    fn add_file(&mut self, path: &Path, i: &Input, o: &Options, depth: u32) -> Result<(), String> {
        let raw =
            std::fs::read(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        if raw.starts_with(crate::archive::MAGIC) {
            return self.add_archive(path, raw, i.whole_archive);
        }
        if raw.starts_with(b"!<thin>\n") {
            return Err(format!(
                "{}: thin archives are not supported",
                path.display()
            ));
        }
        if raw.starts_with(b"\x7fELF") {
            return match object::Class::of(&raw).e.u16(&raw, 16) {
                1 => {
                    let fi = self.files.len() as u32;
                    let f = object::parse(self, fi, path.display().to_string(), raw)?;
                    self.drain(f)
                }
                3 => {
                    if o.static_link {
                        return Err(format!(
                            "{}: a shared object in a static link",
                            path.display()
                        ));
                    }
                    self.add_dso(path, &raw, i.as_needed)
                }
                t => Err(format!("{}: unknown ELF file type {t}", path.display())),
            };
        }
        if depth > 8 {
            return Err(format!("{}: linker scripts nest too deep", path.display()));
        }
        let text = String::from_utf8_lossy(&raw);
        let items = dso::parse_script(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        for (item, needed) in items {
            let sub = Input {
                as_needed: i.as_needed || needed,
                ..i.clone()
            };
            let p = if let Some(n) = item.strip_prefix("-l") {
                find_library(n, &sub, o)
                    .ok_or_else(|| format!("{}: unable to find library -l{n}", path.display()))?
            } else {
                let p = PathBuf::from(&item);
                if p.is_file() {
                    p
                } else if let Some(q) = path.parent().map(|d| d.join(&item)).filter(|q| q.is_file())
                {
                    q
                } else {
                    o.lib_dirs
                        .iter()
                        .map(|d| d.join(&item))
                        .find(|q| q.is_file())
                        .ok_or_else(|| format!("{}: cannot find {item}", path.display()))?
                }
            };
            self.add_file(&p, &sub, o, depth + 1)?;
        }
        Ok(())
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

    fn add_dso(&mut self, path: &Path, raw: &[u8], as_needed: bool) -> Result<(), String> {
        let d = dso::read(path, raw, self.arch)?;
        if self.dsos.iter().any(|x| x.soname == d.soname) {
            return Ok(());
        }
        let lib = self.dsos.len() as u32;
        // `name@VERSION` references bind that version, others the
        // default one.
        let bind = |l: &mut Self, id: SymId, e: dso::Export| {
            l.syms.get_mut(id).def = Def::Dylib { lib };
            let x = l.ext_mut(id);
            x.ty = e.ty;
            x.size = e.size;
            x.dso_ver = e.ver;
        };
        for (name, e) in &d.exports {
            let id = self.syms.intern(name);
            if self.syms.get(id).def == Def::Undefined && self.ext(id).want_version.is_none() {
                bind(self, id, *e);
            }
        }
        for ((name, ver), e) in &d.versioned {
            let Some(id) = self.syms.find(name) else {
                continue;
            };
            if self.syms.get(id).def == Def::Undefined
                && self.ext(id).want_version.as_deref() == Some(ver)
            {
                bind(self, id, *e);
            }
        }
        self.dso_undefined.extend(d.undefined.iter().cloned());
        self.dsos.push(Dso { as_needed, ..d });
        Ok(())
    }
}

/// `-l<name>`: `lib<name>.so`, then `lib<name>.a`, in each directory.
fn find_library(name: &str, i: &Input, o: &Options) -> Option<PathBuf> {
    if let Some(file) = name.strip_prefix(':') {
        return o
            .lib_dirs
            .iter()
            .map(|d| d.join(file))
            .find(|p| p.is_file());
    }
    for d in &o.lib_dirs {
        if !i.bstatic && !o.static_link {
            let p = d.join(format!("lib{name}.so"));
            if p.is_file() {
                return Some(p);
            }
        }
        let p = d.join(format!("lib{name}.a"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Names lld defines when they are referenced.
const LINKER_DEFINED: &[&str] = &[
    "__ehdr_start",
    "__executable_start",
    "__dso_handle",
    "_GLOBAL_OFFSET_TABLE_",
    "_DYNAMIC",
    "__bss_start",
    "_end",
    "end",
    "_etext",
    "etext",
    "_edata",
    "edata",
    "__init_array_start",
    "__init_array_end",
    "__fini_array_start",
    "__fini_array_end",
    "__preinit_array_start",
    "__preinit_array_end",
    "__rela_iplt_start",
    "__rela_iplt_end",
    "__exidx_start",
    "__exidx_end",
    "_TLS_MODULE_BASE_",
    "__global_pointer$",
    ".TOC.",
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
    let mut l = Link::new(o.arch, !o.strip_all);
    l.tls_calls = o.shared && o.dynamic_tls;
    for n in o.wanted() {
        let id = l.syms.intern(&n);
        l.syms.reference(id, u32::MAX, false);
    }
    for i in &o.inputs {
        match &i.kind {
            InputKind::File(p) => l.add_file(p, i, o, 0)?,
            InputKind::Lib(n) => {
                let p =
                    find_library(n, i, o).ok_or_else(|| format!("unable to find library -l{n}"))?;
                l.add_file(&p, i, o, 0)?;
            }
        }
    }
    if !l.errors.is_empty() {
        return Err(l.errors.join("\n"));
    }
    let keep = match plugin {
        Some((p, glue)) => crate::plugin::prepare(&mut l, p, glue)?,
        None => Vec::new(),
    };
    for n in LINKER_DEFINED {
        if let Some(id) = l.syms.find(n) {
            let s = l.syms.get(id);
            let dynamic_only = *n == "_DYNAMIC" && !o.dynamic();
            if s.referenced && matches!(s.def, Def::Undefined | Def::Lazy { .. }) && !dynamic_only {
                let _ = l.syms.define(id, Def::Synthetic, u32::MAX, false, true);
                // The start of the TLS block (TLS descriptors' local
                // dynamic form).
                if *n == "_TLS_MODULE_BASE_" {
                    l.syms.get_mut(id).tlv = true;
                    l.ext_mut(id).ty = object::STT_TLS;
                }
            }
        }
    }
    // `__start_<section>` / `__stop_<section>`: bound at layout.
    let start_stop: Vec<(SymId, String)> = l
        .syms
        .syms
        .iter()
        .enumerate()
        .filter(|(_, s)| s.referenced && matches!(s.def, Def::Undefined | Def::Lazy { .. }))
        .filter_map(|(i, s)| {
            let sect = s
                .name
                .strip_prefix("__start_")
                .or_else(|| s.name.strip_prefix("__stop_"))?;
            Some((i as SymId, sect.to_owned()))
        })
        .collect();
    for (id, _) in &start_stop {
        let _ = l.syms.define(*id, Def::Synthetic, u32::MAX, false, true);
    }
    write::commons(&mut l);
    let mut roots = roots(&mut l, o, &start_stop);
    roots.extend(keep.into_iter().map(Target::Sym));
    // FDEs live with their functions.
    for k in 0..l.fdes.len() {
        let (fde, _) = l.fdes[k];
        if let Some(f) = object::fde_function(&l, fde) {
            l.atoms[f as usize].keep.push(Target::Atom(fde));
        }
    }
    crate::gc::mark(&mut l.atoms, &l.syms, &roots, false);
    for k in 0..l.atoms.len() {
        let a = &l.atoms[k];
        if a.file != u32::MAX && !l.files[a.file as usize].sects[a.sect as usize].alloc() {
            l.atoms[k].live = true;
        }
    }
    l.atoms[DEAD as usize].live = false;
    check_undefined(&l, o)?;
    if o.icf {
        fold_code(&mut l, o, &start_stop);
    }
    let r = write::write(&mut l, o, &start_stop);
    // Freeing every atom and name takes as long as some links: let a
    // thread do it.
    let _ = std::thread::Builder::new().spawn(move || drop(l));
    r
}

/// Garbage collection roots: the entry, `_init` / `_fini`, exported
/// symbols and the sections lld keeps (`.init`, init arrays, notes,
/// `SHF_GNU_RETAIN`, `__start_` / `__stop_` sections); with no
/// `--gc-sections`, every allocated section.
fn roots(l: &mut Link, o: &Options, start_stop: &[(SymId, String)]) -> Vec<Target> {
    let mut roots: Vec<Target> = o
        .wanted()
        .iter()
        .map(|n| Target::Sym(l.syms.intern(n)))
        .collect();
    for n in ["_init", "_fini"] {
        if let Some(id) = l.syms.find(n) {
            roots.push(Target::Sym(id));
        }
    }
    for (i, s) in l.syms.syms.iter().enumerate() {
        if exported(l, o, i as SymId) || s.no_dead_strip {
            roots.push(Target::Sym(i as SymId));
        }
    }
    let wanted: HashSet<&str> = start_stop.iter().map(|(_, s)| s.as_str()).collect();
    for k in 1..l.atoms.len() {
        let a = &l.atoms[k];
        let s = &l.files[a.file as usize].sects[a.sect as usize];
        if !s.alloc() || s.kind == object::SectKind::EhFrame || s.ty == object::SHT_ARM_EXIDX {
            continue;
        }
        let keep = !o.gc || s.keep() || wanted.contains(object::output_name(&s.name));
        if keep {
            roots.push(Target::Atom(k as AtomId));
        }
    }
    roots
}

/// Whether relocation `r` of code atom `a` only calls or jumps to its
/// target.
fn calls(l: &Link, a: AtomId, r: &crate::symtab::Reloc) -> bool {
    let code = || {
        let at = &l.atoms[a as usize];
        let f = &l.files[at.file as usize];
        let s = &f.sects[at.sect as usize];
        let start = (s.off + at.off) as usize;
        &f.data[start..start + at.size as usize]
    };
    match l.arch {
        Arch::X86_64 => {
            matches!(r.kind, x64::PC32 | x64::PLT32) && crate::icf::x86_branch(code(), r.off)
        }
        Arch::I386 => {
            matches!(r.kind, i386::PC32 | i386::PLT32) && crate::icf::x86_branch(code(), r.off)
        }
        Arch::Aarch64 => matches!(
            r.kind,
            a64::CALL26 | a64::JUMP26 | a64::CONDBR19 | a64::TSTBR14
        ),
        Arch::Arm => matches!(
            r.kind,
            arm32::PC24
                | arm32::PLT32
                | arm32::CALL
                | arm32::JUMP24
                | arm32::THM_CALL
                | arm32::THM_JUMP24
                | arm32::THM_JUMP19
        ),
        // A low half points at its `auipc`, not at what that addresses.
        Arch::Riscv64 | Arch::Riscv32 => matches!(
            r.kind,
            riscv::BRANCH
                | riscv::JAL
                | riscv::CALL
                | riscv::CALL_PLT
                | riscv::RVC_BRANCH
                | riscv::RVC_JUMP
                | riscv::PCREL_LO12_I
                | riscv::PCREL_LO12_S
        ),
        Arch::Ppc64 => matches!(r.kind, ppc64::REL24 | ppc64::REL14),
        // `brasl` and `brcl` (`larl` shares the relocation types).
        Arch::S390x => {
            matches!(r.kind, s390x::PC32DBL | s390x::PLT32DBL) && {
                let (c, k) = (code(), r.off as usize);
                k >= 2 && c[k - 2] == 0xc0 && matches!(c[k - 1] & 0xf, 4 | 5)
            }
        }
    }
}

/// The atoms as identical code folding sees them.
struct Fold<'a> {
    l: &'a Link,
    tags: HashMap<AtomId, Vec<(u64, u32)>>,
    canon: HashMap<AtomId, AtomId>,
}
impl crate::icf::View for Fold<'_> {
    fn bytes(&self, a: AtomId) -> &[u8] {
        let at = &self.l.atoms[a as usize];
        let f = &self.l.files[at.file as usize];
        let start = (f.sects[at.sect as usize].off + at.off) as usize;
        &f.data[start..start + at.size as usize]
    }
    fn extra(&self, a: AtomId) -> u64 {
        let at = &self.l.atoms[a as usize];
        let s = &self.l.files[at.file as usize].sects[at.sect as usize];
        let mut h = std::hash::DefaultHasher::new();
        (s.flags, s.ty).hash(&mut h);
        self.tags.get(&a).hash(&mut h);
        h.finish()
    }
    fn dest(&self, t: Target) -> crate::icf::Dest {
        let (atom, off, tag) = match t {
            Target::Atom(a) => (a, 0, 0),
            Target::Sym(id) => match self.l.syms.get(id).def {
                Def::Defined { atom, off } => {
                    let x = self.l.ext(id);
                    (
                        atom,
                        off,
                        u32::from(x.ty) | (u32::from(x.other & 0xe0) << 8),
                    )
                }
                _ => return crate::icf::Dest::Other(u64::from(id)),
            },
        };
        let atom = self.canon.get(&atom).copied().unwrap_or(atom);
        crate::icf::Dest::In { atom, off, tag }
    }
}

/// Fold identical code: functions whose address is never taken, or
/// nabic's function value thunks, with the same bytes and relocations,
/// become one. A copy's FDE and index
/// table entry go with it; debug info that described it points at the
/// tombstone, like dead code's.
fn fold_code(l: &mut Link, o: &Options, start_stop: &[(SymId, String)]) {
    let alloc_atom = |l: &Link, a: AtomId| {
        let at = &l.atoms[a as usize];
        at.file != u32::MAX && l.files[at.file as usize].sects[at.sect as usize].alloc()
    };
    let unwind = |l: &Link, a: AtomId| {
        let at = &l.atoms[a as usize];
        let s = &l.files[at.file as usize].sects[at.sect as usize];
        s.kind == SectKind::EhFrame || s.ty == object::SHT_ARM_EXIDX
    };
    let mut taken = crate::icf::address_taken(
        &l.atoms,
        &l.syms,
        |a| a != DEAD && alloc_atom(l, a) && !unwind(l, a),
        |a, r| {
            let at = &l.atoms[a as usize];
            l.files[at.file as usize].sects[at.sect as usize].flags & object::SHF_EXECINSTR != 0
                && calls(l, a, r)
        },
    );
    // Function value thunks, each in its own `.text.<symbol>.v`.
    for (a, t) in taken.iter_mut().enumerate().skip(1) {
        let at = &l.atoms[a];
        if at.file == u32::MAX {
            continue;
        }
        let s = &l.files[at.file as usize].sects[at.sect as usize];
        if s.kind == SectKind::Atom
            && s.flags & object::SHF_EXECINSTR != 0
            && s.name
                .strip_prefix(".text.")
                .is_some_and(crate::icf::fn_value_thunk)
        {
            *t = false;
        }
    }
    // The roots `--gc-sections` would keep: the entry, exports, kept
    // sections.
    let all = Options {
        gc: true,
        ..o.clone()
    };
    for t in roots(l, &all, start_stop) {
        if let Some((a, _)) = l.syms.resolve(t) {
            taken[a as usize] = true;
        }
    }
    // The symbols in each atom; indirect functions resolve at run time.
    let mut tags: HashMap<AtomId, Vec<(u64, u32)>> = HashMap::default();
    for (i, s) in l.syms.syms.iter().enumerate() {
        if let Def::Defined { atom, off } = s.def {
            let x = l.ext(i as SymId);
            if x.ty == object::STT_GNU_IFUNC {
                taken[atom as usize] = true;
            }
            tags.entry(atom)
                .or_default()
                .push((off, u32::from(x.ty) | (u32::from(x.other & 0xe0) << 8)));
        }
    }
    for f in &l.files {
        for s in &f.locals {
            // 32-bit ARM's mapping symbols say A32, T32 or data.
            let map = match crate::bytes::cstr(&f.data, s.name).as_bytes() {
                [b'$', c, ..] => u32::from(*c),
                _ => 0,
            };
            tags.entry(s.atom).or_default().push((
                s.off,
                u32::from(s.ty) | (u32::from(s.other & 0xe0) << 8) | (map << 16),
            ));
        }
    }
    for v in tags.values_mut() {
        v.sort_unstable();
    }
    // An FDE's: an LSDA (a relocation past the function's) keeps the
    // function, as in lld.
    let mut fde_of: HashMap<AtomId, AtomId> = HashMap::default();
    for &(fde, _) in &l.fdes {
        if !l.atoms[fde as usize].live {
            continue;
        }
        if let Some(f) = object::fde_function(l, fde) {
            if l.atoms[fde as usize].relocs.len() > 1 {
                taken[f as usize] = true;
            }
            fde_of.insert(f, fde);
        }
    }
    // An index table entry with its own table (`.ARM.extab`) likewise.
    let mut exidx_of: HashMap<AtomId, AtomId> = HashMap::default();
    for &(code, x) in &l.exidx {
        let own = l.atoms[x as usize].relocs.iter().all(|r| {
            r.kind == arm32::NONE || l.syms.resolve(r.target).is_some_and(|(t, _)| t == code)
        });
        if !own {
            taken[code as usize] = true;
        }
        exidx_of.insert(code, x);
    }
    let cands: Vec<AtomId> = (1..l.atoms.len() as AtomId)
        .filter(|&a| {
            let at = &l.atoms[a as usize];
            if !at.live || at.root || taken[a as usize] || at.size == 0 || at.file == u32::MAX {
                return false;
            }
            let s = &l.files[at.file as usize].sects[at.sect as usize];
            s.kind == SectKind::Atom
                && s.alloc()
                && !s.nobits()
                && s.flags & object::SHF_EXECINSTR != 0
                && !s.keep()
        })
        .collect();
    // Merged strings, and nabic's string literals (named by their
    // content, their addresses meaningless): identical ones are one.
    let mut canon: HashMap<AtomId, AtomId> = HashMap::default();
    {
        // Literals and pieces apart: a piece may yet become a copy itself.
        let mut first: HashMap<(bool, &str, &[u8]), AtomId> = HashMap::default();
        for f in &l.files {
            for s in &f.sects {
                let literal = s.kind == SectKind::Atom
                    && s.name.starts_with(".rodata.nabi.str.")
                    && s.flags & (object::SHF_WRITE | object::SHF_EXECINSTR) == 0;
                if s.kind != SectKind::Pieces && !literal {
                    continue;
                }
                for &a in &s.atoms {
                    let at = &l.atoms[a as usize];
                    if !at.live {
                        continue;
                    }
                    let start = (s.off + at.off) as usize;
                    let bytes = &f.data[start..start + at.size as usize];
                    let c = *first
                        .entry((literal, object::output_name(&s.name), bytes))
                        .or_insert(a);
                    if c != a {
                        canon.insert(a, c);
                    }
                }
            }
        }
    }
    // Literals go now; pieces when they are placed.
    let literals: Vec<(AtomId, AtomId)> = canon
        .iter()
        .filter(|&(&a, _)| {
            let at = &l.atoms[a as usize];
            l.files[at.file as usize].sects[at.sect as usize].kind == SectKind::Atom
        })
        .map(|(&a, &c)| (a, c))
        .collect();
    let v = Fold { l, tags, canon };
    let folds = crate::icf::fold(&l.atoms, &cands, &v);
    drop(v);
    crate::icf::apply(&mut l.atoms, &folds);
    crate::icf::apply(&mut l.atoms, &literals);
    for &(dup, keep) in folds.iter().chain(&literals) {
        l.alias.insert(dup, keep);
        for x in [fde_of.get(&dup), exidx_of.get(&dup)].into_iter().flatten() {
            l.atoms[*x as usize].live = false;
        }
    }
}

/// In the dynamic symbol table as a definition: everything with
/// `--export-dynamic`, else what a shared object refers to.
#[must_use]
pub fn exported(l: &Link, o: &Options, id: SymId) -> bool {
    let s = l.syms.get(id);
    if !o.dynamic() || s.private_extern || s.name.starts_with('\0') {
        return false;
    }
    if !matches!(
        s.def,
        Def::Defined { .. } | Def::Absolute(_) | Def::Common { .. }
    ) {
        return false;
    }
    if o.shared {
        return o.exports.contains(&s.name);
    }
    o.export_dynamic || l.dso_undefined.contains(&s.name)
}

/// Undefined symbols that live code references, as lld reports them.
fn check_undefined(l: &Link, o: &Options) -> Result<(), String> {
    let mut bad: Vec<(SymId, u32)> = Vec::new();
    let mut seen = HashSet::default();
    let undef = |t: Target| match t {
        Target::Sym(id) => {
            let s = l.syms.get(id);
            (matches!(s.def, Def::Undefined | Def::Lazy { .. }) && !s.weak_ref).then_some(id)
        }
        Target::Atom(_) => None,
    };
    for a in l.atoms.iter().filter(|a| a.live && a.file != u32::MAX) {
        if !l.files[a.file as usize].sects[a.sect as usize].alloc() {
            continue;
        }
        for r in &a.relocs {
            if let Some(id) = undef(r.target) {
                if seen.insert(id) {
                    bad.push((id, a.file));
                }
            }
        }
    }
    // A static RISC-V image without its entry starts at 0, as lld
    // leaves it (firmware enters through its own vector).
    let no_entry_ok = o.static_link && l.arch.riscv();
    for n in o.wanted() {
        let id = l.syms.find(&n).expect("interned");
        let ok = matches!(l.syms.get(id).def, Def::Defined { .. }) || (no_entry_ok && n == o.entry);
        if !ok && seen.insert(id) {
            bad.push((id, u32::MAX));
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
            "the command line (the entry point, -init, an export)".to_owned()
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
