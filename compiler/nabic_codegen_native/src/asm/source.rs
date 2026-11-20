//! `--emit=asm`: the object as assembly source an assembler turns back
//! into the same object. Read back from the bytes the object writer made
//! (`read`), section by section: the functions as their instructions
//! (`code`), everything else (data, unwind and debug info) as data
//! directives, each relocation a symbolic expression.

use crate::asm::code::{FnSrc, Isa, local, quote};
use crate::asm::read::{self, Format, Object, Rel, Section};
use crate::obj::Obj;
use nabic_core::leb::read_uleb;
use std::fmt::Write;

/// The source of `object`, which `obj` (its functions in `obj.src`)
/// became.
#[must_use]
pub fn write(obj: &Obj, object: &[u8]) -> String {
    let o = read::read(object);
    let isa = Isa::of(obj);
    let mut fns: nabic_core::HashMap<&str, &FnSrc> = nabic_core::HashMap::default();
    for f in obj.src.iter().flatten() {
        fns.insert(f.name.as_str(), f);
    }
    let (cfi, hidden) = crate::asm::unwind::directives(&o, isa, obj.be, &fns);
    let mut w = Writer {
        o: &o,
        obj,
        isa,
        fns,
        out: String::new(),
        sect_labels: vec![false; o.sections.len() + 1],
        cfi,
        hidden,
    };
    w.header();
    w.mark_section_labels();
    w.absolute();
    for k in 0..o.sections.len() {
        w.section(k);
    }
    if o.format == Format::MachO {
        w.out.push_str("\n.subsections_via_symbols\n");
    }
    w.out
}

struct Writer<'a> {
    o: &'a Object,
    obj: &'a Obj,
    isa: Isa,
    fns: nabic_core::HashMap<&'a str, &'a FnSrc>,
    out: String,
    /// The sections a relocation refers to by their start (section
    /// symbols): a local label there.
    sect_labels: Vec<bool>,
    /// Each function's unwind directives (`unwind`), and the tables they
    /// replace, which are not written.
    cfi: nabic_core::HashMap<String, Vec<(u32, String)>>,
    hidden: Vec<bool>,
}

/// How a data relocation's place is computed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum How {
    Abs,
    Pc,
    /// ARM `.ARM.exidx`: 31-bit PC-relative.
    Prel31,
    /// COFF: image-relative, section-relative, section index.
    Rva,
    SecRel,
    SecIdx,
}

impl Writer<'_> {
    fn header(&mut self) {
        let s = &mut self.out;
        match self.isa {
            Isa::X64 | Isa::I386 => s.push_str("\t.intel_syntax noprefix\n"),
            Isa::Arm => {
                s.push_str("\t.syntax unified\n");
                // NEON code assembles under `.fpu neon`, which also says so
                // in the build attributes.
                let neon =
                    self.o.sections.iter().any(|x| {
                        x.ty == 0x7000_0003 && arm_attributes(&x.data).is_some_and(|a| a.1)
                    });
                if neon {
                    s.push_str("\t.fpu\tneon\n");
                }
            }
            Isa::A64 => {
                for (on, ext) in [
                    (self.obj.lse, "lse"),
                    (self.obj.rcpc, "rcpc"),
                    (self.obj.dotprod, "dotprod"),
                ] {
                    if on {
                        let _ = writeln!(s, "\t.arch_extension\t{ext}");
                    }
                }
            }
            Isa::Riscv => {
                let ext = match (self.obj.rv32, self.obj.vector) {
                    (true, _) => "+m, +a, +c",
                    (false, false) => "+m, +a, +f, +d, +c",
                    (false, true) => "+m, +a, +f, +d, +c, +v",
                };
                let _ = writeln!(
                    s,
                    "\t.option\tarch, {ext}\n\t.option\tnorvc\n\t.option\tnorelax"
                );
            }
            Isa::Ppc64 => s.push_str("\t.abiversion\t2\n"),
            // The vector facility's instructions.
            Isa::S390x if self.obj.z14 => s.push_str("\t.machine\tz14\n"),
            Isa::S390x if self.obj.vector => s.push_str("\t.machine\tz13\n"),
            Isa::S390x => {}
        }
        if self.o.format == Format::MachO {
            let (platform, minos) = self.obj.build_version.unwrap_or((1, 0x000b_0000));
            let name = match platform {
                2 => "ios",
                7 => "iossimulator",
                _ => "macos",
            };
            let (major, minor) = (minos >> 16, (minos >> 8) & 0xff);
            let _ = writeln!(s, "\t.build_version\t{name}, {major}, {minor}");
        }
    }

    /// COFF absolute symbols (`@feat.00`).
    fn absolute(&mut self) {
        for y in self.o.syms.iter().filter(|y| y.abs) {
            let _ = writeln!(
                self.out,
                "\t.def\t{n};\n\t.scl\t{};\n\t.type\t{};\n\t.endef\n\t.set\t{n}, {}",
                y.info,
                y.other,
                y.value,
                n = y.name
            );
        }
    }

    /// The local label at the start of section `k`.
    fn sect_label(&self, k: usize) -> String {
        local(self.obj, &format!("sec{k}"))
    }

    fn mark_section_labels(&mut self) {
        for (k, s) in self.o.sections.iter().enumerate() {
            if self.hidden[k] {
                continue;
            }
            for r in &s.relocs {
                if let Some(k) = self.section_target(r) {
                    self.sect_labels[k] = true;
                }
            }
        }
    }

    /// The section (index in `o.sections`) relocation `r` refers to by
    /// its start, if it does.
    fn section_target(&self, r: &Rel) -> Option<usize> {
        let o = self.o;
        match o.format {
            Format::Elf => {
                let s = o.syms.get(r.sym as usize)?;
                (s.info & 0xf == 3).then_some(s.sect as usize)
            }
            Format::MachO => (!r.ext).then(|| r.sym as usize - 1),
            Format::Coff => {
                let s = o.syms.get(r.sym as usize)?;
                s.section_sym.then(|| s.sect as usize - 1)
            }
        }
    }

    /// Whether section `k` is written: not the format's own tables.
    fn listed(&self, k: usize) -> bool {
        if self.hidden[k] {
            return false;
        }
        let s = &self.o.sections[k];
        match self.o.format {
            // Null, symbol and string tables, relocations, extended
            // section indices.
            Format::Elf => k != 0 && !matches!(s.ty, 2 | 3 | 4 | 9 | 18),
            Format::MachO | Format::Coff => true,
        }
    }

    fn section(&mut self, k: usize) {
        if !self.listed(k) {
            return;
        }
        let s = &self.o.sections[k];
        // 32-bit ARM: the build attributes as directives, from which an
        // assembler writes the section.
        if self.isa == Isa::Arm && s.ty == 0x7000_0003 {
            if let Some((t, _)) = arm_attributes(&s.data) {
                self.out.push('\n');
                self.out.push_str(&t);
                return;
            }
        }
        let _ = writeln!(self.out);
        let dir = match self.o.format {
            Format::Elf => self.elf_section(s),
            Format::MachO => macho_section(s),
            Format::Coff => self.coff_section(s),
        };
        self.out.push_str(&dir);
        if s.align > 1 {
            let _ = writeln!(self.out, "\t.p2align\t{}", s.align.trailing_zeros());
        }
        if self.sect_labels[k] {
            let l = self.sect_label(k);
            let _ = writeln!(self.out, "{l}:");
        }
        self.contents(k);
    }

    fn elf_section(&self, s: &Section) -> String {
        let mut f = String::new();
        for (bit, c) in [
            (2, 'a'),
            (1, 'w'),
            (4, 'x'),
            (0x10, 'M'),
            (0x20, 'S'),
            (0x80, 'o'),
            (0x200, 'G'),
            (0x400, 'T'),
        ] {
            if s.flags & bit != 0 {
                f.push(c);
            }
        }
        let at = if self.isa == Isa::Arm { '%' } else { '@' };
        let ty = match s.ty {
            1 => "progbits".to_owned(),
            7 => "note".to_owned(),
            8 => "nobits".to_owned(),
            t => format!("{t:#x}"),
        };
        let mut d = format!("\t.section\t{},\"{f}\",{at}{ty}", quote(&s.name));
        if s.flags & 0x10 != 0 {
            let _ = write!(d, ",{}", s.entsize);
        }
        if s.flags & 0x80 != 0 {
            let linked = &self.o.sections[s.link as usize].name;
            let _ = write!(d, ",{}", quote(linked));
        }
        d.push('\n');
        d
    }

    fn coff_section(&self, s: &Section) -> String {
        let c = s.ty;
        let mut f = String::new();
        if c & 0x20 != 0 || c & 0x2000_0000 != 0 {
            f.push('x');
        }
        if c & 0x80 != 0 {
            f.push('b');
        }
        if c & 0x40 != 0 {
            f.push('d');
        }
        if c & 0x8000_0000 != 0 {
            f.push('w');
        } else if c & 0x4000_0000 != 0 {
            f.push('r');
        }
        if c & 0x0200_0000 != 0 {
            f.push('D');
        }
        if c & 0x800 != 0 {
            f.push('n');
        }
        let mut d = format!("\t.section\t{},\"{f}\"", quote(&s.name));
        if let Some((sel, assoc)) = s.comdat {
            let (how, lead) = if sel == 5 {
                ("associative", self.coff_leader(assoc as usize - 1))
            } else {
                let how = match sel {
                    1 => "one_only",
                    2 => "discard",
                    3 => "same_size",
                    4 => "same_contents",
                    6 => "largest",
                    _ => "newest",
                };
                let k = self.o.sections.iter().position(|x| std::ptr::eq(x, s));
                (how, k.and_then(|k| self.coff_leader(k)))
            };
            if let Some(lead) = lead {
                let _ = write!(d, ",{how},{}", quote(&lead));
            }
        }
        d.push('\n');
        d
    }

    /// The symbol leading COMDAT section `k`: the first after its section
    /// symbol, as source names it.
    fn coff_leader(&self, k: usize) -> Option<String> {
        let syms = &self.o.syms;
        let i = syms
            .iter()
            .position(|s| s.section_sym && s.sect as usize == k + 1)?;
        let j = i + 1 + syms[i + 1..].iter().position(|s| !s.name.is_empty())?;
        Some(self.coff_name(j).unwrap_or_else(|| syms[j].name.clone()))
    }

    /// The source name of COFF symbol `i`: a weak definition's default
    /// is the weak name.
    fn coff_name(&self, i: usize) -> Option<String> {
        let s = &self.o.syms[i];
        let n = s.name.strip_prefix(".weak.")?.strip_suffix(".default")?;
        Some(n.to_owned())
    }

    /// Section `k`'s contents: symbols, functions, data.
    #[allow(clippy::too_many_lines)]
    fn contents(&mut self, k: usize) {
        let o = self.o;
        let s = &o.sections[k];
        let len = s.bss.unwrap_or(s.data.len() as u64);
        // The symbols defined here by offset: `(offset, symbol index)`.
        let sect_no = match o.format {
            Format::Elf => k as u32,
            Format::MachO | Format::Coff => k as u32 + 1,
        };
        let mut defs: Vec<(u64, usize)> = o
            .syms
            .iter()
            .enumerate()
            .filter(|(_, y)| y.sect == sect_no && !y.name.is_empty() && self.shown(y))
            .map(|(i, y)| {
                let at = if o.format == Format::MachO {
                    y.value - s.addr
                } else {
                    y.value
                };
                (at, i)
            })
            .collect();
        defs.sort_unstable();
        let mut relocs: Vec<Rel> = s.relocs.clone();
        relocs.sort_by_key(|r| r.off);
        if let Some(n) = s.bss {
            for &(_, i) in &defs {
                self.symbol(i);
            }
            let _ = writeln!(self.out, "\t.zero\t{n}");
            return;
        }
        let mut pos = 0u64;
        let mut d = 0;
        while pos < len || d < defs.len() {
            let mut func = None;
            while let Some(&(at, i)) = defs.get(d).filter(|x| x.0 <= pos) {
                let name = self.symbol(i);
                let _ = at;
                if let Some(f) = self.fns.get(name.as_str()) {
                    func = Some((*f, o.syms[i].name.clone()));
                }
                d += 1;
            }
            if pos >= len {
                break;
            }
            if let Some((f, name)) = func {
                self.function(f, &name);
                pos += u64::from(f.len);
                continue;
            }
            let next = defs.get(d).map_or(len, |x| x.0.min(len));
            self.data(s, &relocs, pos, next);
            pos = next;
        }
    }

    /// Function `f`'s text, its unwind directives after the instructions
    /// their offsets end.
    fn function(&mut self, f: &FnSrc, name: &str) {
        let Some(ds) = self.cfi.get(name) else {
            self.out.push_str(&f.text);
            return;
        };
        let mut from = 0;
        let mut k = 0;
        for &(at, i) in &f.marks {
            self.out.push_str(&f.text[from..i]);
            from = i;
            while let Some((_, d)) = ds.get(k).filter(|d| d.0 == at) {
                let _ = writeln!(self.out, "\t{d}");
                k += 1;
            }
        }
        self.out.push_str(&f.text[from..]);
    }

    /// Whether symbol `y` is written as a label: not a section's, a file's,
    /// an ARM mapping symbol or a local label.
    fn shown(&self, y: &read::Symbol) -> bool {
        match self.o.format {
            Format::Elf => {
                !matches!(y.info & 0xf, 3 | 4)
                    && !y.name.starts_with('$')
                    && !y.name.starts_with(".L")
            }
            Format::MachO => y.info & 0xe0 == 0,
            Format::Coff => !y.section_sym && y.info != 105,
        }
    }

    /// The directives and label of defined symbol `i`; its name.
    fn symbol(&mut self, i: usize) -> String {
        let o = self.o;
        let y = &o.syms[i];
        let mut name = y.name.clone();
        match o.format {
            Format::Elf => {
                let q = quote(&name);
                let at = if self.isa == Isa::Arm { '%' } else { '@' };
                match y.info >> 4 {
                    1 => {
                        let _ = writeln!(self.out, "\t.globl\t{q}");
                    }
                    2 => {
                        let _ = writeln!(self.out, "\t.weak\t{q}");
                    }
                    _ => {}
                }
                match y.other & 3 {
                    1 => {
                        let _ = writeln!(self.out, "\t.internal\t{q}");
                    }
                    2 => {
                        let _ = writeln!(self.out, "\t.hidden\t{q}");
                    }
                    3 => {
                        let _ = writeln!(self.out, "\t.protected\t{q}");
                    }
                    _ => {}
                }
                match y.info & 0xf {
                    1 => {
                        let _ = writeln!(self.out, "\t.type\t{q},{at}object");
                    }
                    2 => {
                        let _ = writeln!(self.out, "\t.type\t{q},{at}function");
                    }
                    6 => {
                        let _ = writeln!(self.out, "\t.type\t{q},{at}tls_object");
                    }
                    _ => {}
                }
                if y.size != 0 || y.info & 0xf != 0 {
                    let _ = writeln!(self.out, "\t.size\t{q}, {}", y.size);
                }
                let _ = writeln!(self.out, "{q}:");
                // powerpc64 `ELFv2`: the local entry past the global one.
                if self.isa == Isa::Ppc64 && y.other >> 5 > 1 {
                    let _ = writeln!(self.out, "\t.localentry\t{q}, {}", 1u32 << (y.other >> 5));
                }
            }
            Format::MachO => {
                let q = quote(&name);
                if y.info & 0x1 != 0 {
                    let _ = writeln!(self.out, "\t.globl\t{q}");
                }
                if y.info & 0x10 != 0 {
                    let _ = writeln!(self.out, "\t.private_extern\t{q}");
                }
                if y.other & 0x80 != 0 {
                    let _ = writeln!(self.out, "\t.weak_definition\t{q}");
                }
                let _ = writeln!(self.out, "{q}:");
            }
            Format::Coff => {
                let weak = self.coff_name(i);
                if let Some(w) = &weak {
                    name.clone_from(w);
                }
                let q = quote(&name);
                let _ = writeln!(
                    self.out,
                    "\t.def\t{q};\n\t.scl\t{};\n\t.type\t{};\n\t.endef",
                    y.info, y.other
                );
                if weak.is_some() {
                    let _ = writeln!(self.out, "\t.weak\t{q}");
                } else if y.info == 2 {
                    let _ = writeln!(self.out, "\t.globl\t{q}");
                }
                let _ = writeln!(self.out, "{q}:");
            }
        }
        name
    }

    /// Bytes `from..to` of `s` as data, its relocations as expressions.
    fn data(&mut self, s: &Section, relocs: &[Rel], from: u64, to: u64) {
        let mut pos = from;
        let start = relocs.partition_point(|r| r.off < from);
        for r in relocs[start..].iter().take_while(|r| r.off < to) {
            self.bytes(&s.data[pos as usize..r.off as usize]);
            let (n, text) = self.reloc(s, r);
            if let Some(t) = text {
                let _ = writeln!(self.out, "\t{t}");
            } else {
                let b = &s.data[r.off as usize..(r.off as usize + n).min(s.data.len())];
                let c = self.isa.comment();
                let _ = writeln!(self.out, "\t{c} relocation {} not as source", r.ty);
                self.bytes(b);
            }
            pos = r.off + n as u64;
        }
        if pos < to {
            self.bytes(&s.data[pos as usize..to as usize]);
        }
    }

    /// Plain bytes: a C string, zeros, else `.byte` rows.
    fn bytes(&mut self, b: &[u8]) {
        if b.is_empty() {
            return;
        }
        let text = |c: &u8| (0x20..0x7f).contains(c) || matches!(c, b'\t' | b'\n' | b'\r');
        if b.len() >= 2 && b.last() == Some(&0) && b[..b.len() - 1].iter().all(text) {
            let mut s = String::new();
            for &c in &b[..b.len() - 1] {
                match c {
                    b'"' => s.push_str("\\\""),
                    b'\\' => s.push_str("\\\\"),
                    b'\n' => s.push_str("\\n"),
                    b'\t' => s.push_str("\\t"),
                    b'\r' => s.push_str("\\r"),
                    c => s.push(c as char),
                }
            }
            let _ = writeln!(self.out, "\t.asciz\t\"{s}\"");
            return;
        }
        let mut i = 0;
        while i < b.len() {
            let zeros = b[i..].iter().take_while(|&&c| c == 0).count();
            if zeros >= 16 {
                let _ = writeln!(self.out, "\t.zero\t{zeros}");
                i += zeros;
                continue;
            }
            let row: Vec<String> = b[i..]
                .iter()
                .take(16)
                .map(|c| format!("{c:#04x}"))
                .collect();
            let _ = writeln!(self.out, "\t.byte\t{}", row.join(", "));
            i += row.len();
        }
    }

    /// The size of relocation `r`'s place in `s` and its directive.
    fn reloc(&self, s: &Section, r: &Rel) -> (usize, Option<String>) {
        let Some((n, how)) = self.how(r) else {
            return (4, None);
        };
        let at = r.off as usize;
        let place = |n: usize| -> i64 {
            let b = &s.data[at..at + n];
            match n {
                2 => i64::from(i16::from_le_bytes([b[0], b[1]])),
                4 => {
                    let x = [b[0], b[1], b[2], b[3]];
                    i64::from(if self.obj.be {
                        i32::from_be_bytes(x)
                    } else {
                        i32::from_le_bytes(x)
                    })
                }
                _ => {
                    let x: [u8; 8] = b.try_into().unwrap_or([0; 8]);
                    if self.obj.be {
                        i64::from_be_bytes(x)
                    } else {
                        i64::from_le_bytes(x)
                    }
                }
            }
        };
        let o = self.o;
        let (target, mut addend) = match o.format {
            Format::Elf => {
                let y = &o.syms[r.sym as usize];
                let a = if o.rela { r.addend } else { place(n) };
                if y.info & 0xf == 3 {
                    (self.sect_label(y.sect as usize), a)
                } else {
                    (quote(&y.name), a)
                }
            }
            Format::MachO if r.ext => (quote(&o.syms[r.sym as usize].name), place(n)),
            Format::MachO => {
                let k = r.sym as usize - 1;
                (self.sect_label(k), place(n) - o.sections[k].addr as i64)
            }
            Format::Coff => {
                let y = &o.syms[r.sym as usize];
                let a = place(n);
                if y.section_sym {
                    (self.sect_label(y.sect as usize - 1), a)
                } else {
                    let i = r.sym as usize;
                    (
                        quote(&self.coff_name(i).unwrap_or_else(|| y.name.clone())),
                        a,
                    )
                }
            }
        };
        if how == How::Prel31 {
            addend = (addend << 33) >> 33;
        }
        let a = match addend {
            0 => String::new(),
            v if v < 0 => format!("-{:#x}", v.unsigned_abs()),
            v => format!("+{v:#x}"),
        };
        let dir = match n {
            2 => ".short",
            4 => ".long",
            _ => ".quad",
        };
        let t = match how {
            // RISC-V: a difference an assembler would make a pair of
            // ADD32 / SUB32.
            How::Pc if self.isa == Isa::Riscv => {
                format!(".reloc\t., R_RISCV_32_PCREL, {target}{a}\n\t.long\t0")
            }
            How::Abs => format!("{dir}\t{target}{a}"),
            How::Pc => format!("{dir}\t{target}{a}-."),
            How::Prel31 => format!(".long\t{target}(prel31){a}"),
            How::Rva => format!(".rva\t{target}{a}"),
            How::SecRel => format!(".secrel32\t{target}{a}"),
            How::SecIdx => format!(".secidx\t{target}"),
        };
        (n, Some(t))
    }

    /// The size and kind of data relocation `r`.
    const fn how(&self, r: &Rel) -> Option<(usize, How)> {
        let t = r.ty;
        Some(match self.o.format {
            Format::MachO => match (t, r.len) {
                (0, 3) => (8, How::Abs),
                (0, 2) => (4, How::Abs),
                _ => return None,
            },
            Format::Coff => match (self.isa, t) {
                (Isa::X64, 1) | (Isa::A64, 0xe) => (8, How::Abs),
                (Isa::X64, 2) | (Isa::A64, 1) | (Isa::I386, 6) => (4, How::Abs),
                (Isa::X64, 3) | (Isa::A64, 2) | (Isa::I386, 7) => (4, How::Rva),
                (Isa::X64 | Isa::I386, 0xb) | (Isa::A64, 8) => (4, How::SecRel),
                (Isa::X64 | Isa::I386, 0xa) | (Isa::A64, 0xd) => (2, How::SecIdx),
                _ => return None,
            },
            Format::Elf => match (self.isa, t) {
                (Isa::X64, 1)
                | (Isa::A64, 257)
                | (Isa::Riscv, 2)
                | (Isa::Ppc64, 38)
                | (Isa::S390x, 22) => (8, How::Abs),
                (Isa::X64, 10)
                | (Isa::I386 | Isa::Riscv | Isa::Ppc64, 1)
                | (Isa::A64, 258)
                | (Isa::Arm, 2)
                | (Isa::S390x, 4) => (4, How::Abs),
                (Isa::X64 | Isa::I386, 2)
                | (Isa::A64, 261)
                | (Isa::Arm, 3)
                | (Isa::Riscv, 57)
                | (Isa::Ppc64, 26)
                | (Isa::S390x, 5) => (4, How::Pc),
                (Isa::X64, 24) | (Isa::A64, 260) | (Isa::Ppc64, 44) | (Isa::S390x, 23) => {
                    (8, How::Pc)
                }
                (Isa::Arm, 42) => (4, How::Prel31),
                _ => return None,
            },
        })
    }
}

/// The `.eabi_attribute` directives of 32-bit ARM build attributes `b`
/// (one `aeabi` subsection of file attributes), and whether they say
/// NEON.
fn arm_attributes(b: &[u8]) -> Option<(String, bool)> {
    let rest = b.strip_prefix(b"A")?.get(4..)?.strip_prefix(b"aeabi\0")?;
    let rest = rest.strip_prefix(&[1])?.get(4..)?;
    let mut s = String::new();
    let mut i = 0;
    let mut neon = false;
    while i < rest.len() {
        let tag = read_uleb(rest, &mut i)?;
        // Tags with string values: CPU_raw_name, CPU_name, conformance.
        if matches!(tag, 4 | 5 | 67) {
            let end = i + rest[i..].iter().position(|&c| c == 0)?;
            let v = std::str::from_utf8(&rest[i..end]).ok()?;
            let _ = writeln!(s, "\t.eabi_attribute\t{tag}, \"{v}\"");
            i = end + 1;
        } else {
            let v = read_uleb(rest, &mut i)?;
            neon |= tag == 12 && v == 1;
            let _ = writeln!(s, "\t.eabi_attribute\t{tag}, {v}");
        }
    }
    Some((s, neon))
}

/// A Mach-O section's directive: segment, section, type, attributes.
fn macho_section(s: &Section) -> String {
    let ty = match s.ty & 0xff {
        1 => "zerofill",
        2 => "cstring_literals",
        _ => "regular",
    };
    let mut attrs = Vec::new();
    for (bit, a) in [
        (0x8000_0000, "pure_instructions"),
        (0x4000_0000, "no_toc"),
        (0x2000_0000, "strip_static_syms"),
        (0x1000_0000, "no_dead_strip"),
        (0x0800_0000, "live_support"),
        (0x0400_0000, "self_modifying_code"),
        (0x0200_0000, "debug"),
    ] {
        if s.ty & bit != 0 {
            attrs.push(a);
        }
    }
    let mut d = format!("\t.section\t{},{}", s.seg, s.name);
    if ty != "regular" || !attrs.is_empty() {
        let _ = write!(d, ",{ty}");
    }
    if !attrs.is_empty() {
        let _ = write!(d, ",{}", attrs.join("+"));
    }
    d.push('\n');
    d
}
