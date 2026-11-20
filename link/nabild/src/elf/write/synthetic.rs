//! The linker's own sections and the i386 PLT.

#[allow(clippy::wildcard_imports)]
use crate::elf::write::*;

/// The linker's own sections, the FDE CIE pointers and the
/// `.eh_frame_hdr` table.
#[allow(clippy::too_many_lines, clippy::match_same_arms)]
pub(crate) fn write_synthetic(
    out: &Out<'_>,
    buf: &mut [u8],
    dyn_tags: &[(u64, u64)],
    mut rela: Vec<Rela>,
    symtab: Option<&Vec<u8>>,
    strtab: &StrTab,
    shstr: &StrTab,
) -> Result<(), String> {
    let l = out.l;
    let o = out.o;
    let en = l.arch.endian();
    let (relative, glob_dat, _, tpoff64) = reloc::dyn_types(l.arch);
    let wide = l.arch.elf64();
    let ptr = out.ptr();
    let (dtpmod, dtpoff) = match l.arch {
        Arch::Riscv64 => (riscv::TLS_DTPMOD64, u32::from(riscv::TLS_DTPREL64)),
        Arch::Riscv32 => (riscv::TLS_DTPMOD32, u32::from(riscv::TLS_DTPREL32)),
        Arch::Ppc64 => (ppc64::DTPMOD64, u32::from(ppc64::DTPREL64)),
        Arch::S390x => (s390x::TLS_DTPMOD, s390x::TLS_DTPOFF),
        _ => (arm32::TLS_DTPMOD32, arm32::TLS_DTPOFF32),
    };
    // A DTV entry points this far into its block.
    let dtv_bias = match l.arch {
        Arch::Riscv64 | Arch::Riscv32 => riscv::DTV_OFFSET as u64,
        Arch::Ppc64 => ppc64::DTV_OFFSET as u64,
        _ => 0,
    };
    // GOT slots.
    if let Some(g) = out.sec(Synth::Got) {
        for (k, &(id, gk)) in out.got.iter().enumerate() {
            let at = g.off as usize + ptr as usize * k;
            let slot = g.addr + ptr * k as u64;
            let kind = out.kind(Target::Sym(id));
            let dyn_index = out.dyn_index.get(&id).copied().unwrap_or(0);
            match (gk, kind) {
                (GOT_TP, SymKind::Dso) => rela.push((slot, tpoff64, dyn_index, 0)),
                // A shared object's own thread-local: where its block is
                // is the loader's to say.
                (GOT_TP, _) if o.shared => {
                    let off = out.addr(Target::Sym(id)).wrapping_sub(out.tls.0);
                    rela.push((slot, tpoff64, 0, off as i64));
                }
                (GOT_TP, _) => {
                    put_word(
                        en,
                        buf,
                        at,
                        out.tpoff(out.addr(Target::Sym(id))) as u64,
                        wide,
                    );
                }
                (GOT_DYNAMIC, _) => {
                    let d = out.sec(Synth::Dynamic).map_or(0, |d| d.addr);
                    put_word(en, buf, at, d, wide);
                    continue;
                }
                (GOT_TOC, _) => {
                    put_word(en, buf, at, out.got_base(), wide);
                    continue;
                }
                (GOT_RESERVED, _) => continue,
                (GOT_GD, SymKind::Dso) => rela.push((slot, dtpmod, dyn_index, 0)),
                (GOT_GD_OFF, SymKind::Dso) => rela.push((slot, dtpoff, dyn_index, 0)),
                // A shared object's own thread-local through the loader:
                // the module (x86_64), or the TLS descriptor (aarch64),
                // of this object.
                (GOT_GD, _) if o.shared && o.dynamic_tls => {
                    let off = out.addr(Target::Sym(id)).wrapping_sub(out.tls.0);
                    rela.push(match l.arch {
                        Arch::Aarch64 => (slot, crate::elf::arch::a64::TLSDESC, 0, off as i64),
                        _ => (slot, crate::elf::arch::x64::DTPMOD64, 0, 0),
                    });
                }
                (GOT_GD_OFF, _) if o.shared && o.dynamic_tls && l.arch == Arch::Aarch64 => {
                    put_word(en, buf, at, 0, wide);
                }
                // The executable is module 1.
                (GOT_GD | GOT_LD, _) => put_word(en, buf, at, 1, wide),
                (GOT_GD_OFF, _) => {
                    let off = out.addr(Target::Sym(id)).wrapping_sub(out.tls.0);
                    put_word(en, buf, at, off.wrapping_sub(dtv_bias), wide);
                }
                (GOT_LD_ZERO, _) => put_word(en, buf, at, 0, wide),
                _ => {}
            }
            if gk != GOT_ADDR {
                continue;
            }
            let v = out.addr(Target::Sym(id));
            match kind {
                SymKind::Dso => rela.push((slot, glob_dat, dyn_index, 0)),
                SymKind::Local => {
                    put_word(en, buf, at, v, wide);
                    if o.pie {
                        rela.push((slot, relative, 0, v as i64));
                    }
                }
                SymKind::Abs => put_word(en, buf, at, v, wide),
                SymKind::UndefWeak => {}
            }
        }
    }
    // `.got.plt` and its `JUMP_SLOT`s (i386, s390x), lazily bound through
    // PLT0: each slot first points back into its entry.
    let mut jump_slots: Vec<Rela> = Vec::new();
    if let (Some(g), Some(p), Arch::S390x) = (out.sec(Synth::GotPlt), out.sec(Synth::Plt), l.arch) {
        for (k, &id) in out.plt.iter().enumerate() {
            let entry = p.addr + s390x::PLT_HEADER + s390x::PLT_ENTRY * k as u64;
            en.put64(buf, g.off as usize + 8 * k, entry + s390x::PLT_LAZY);
            let dyn_index = out.dyn_index.get(&id).copied().unwrap_or(0);
            jump_slots.push((g.addr + 8 * k as u64, s390x::JMP_SLOT, dyn_index, 0));
        }
    } else if let (Some(g), Some(p)) = (out.sec(Synth::GotPlt), out.sec(Synth::Plt)) {
        let at = g.off as usize;
        en.put32(
            buf,
            at,
            out.sec(Synth::Dynamic).map_or(0, |d| d.addr) as u32,
        );
        for (k, &id) in out.plt.iter().enumerate() {
            let entry = p.addr + 16 + 16 * k as u64;
            en.put32(buf, at + 4 * (3 + k), (entry + 6) as u32);
            let dyn_index = out.dyn_index.get(&id).copied().unwrap_or(0);
            jump_slots.push((
                g.addr + 4 * (3 + k as u64),
                u32::from(crate::elf::arch::i386::JUMP_SLOT),
                dyn_index,
                0,
            ));
        }
    } else if let Some(g) = out.sec(Synth::GotPlt) {
        en.put32(
            buf,
            g.off as usize,
            out.sec(Synth::Dynamic).map_or(0, |d| d.addr) as u32,
        );
    }
    // PLT entries through their GOT slots.
    if let Some(p) = out.sec(Synth::Plt) {
        if l.arch == Arch::I386 {
            write_i386_plt(out, p, buf);
        }
        if l.arch == Arch::S390x {
            let got = out.sec(Synth::Got).map_or(0, |g| g.addr);
            s390x::write_plt0(buf, p.off as usize, p.addr, got);
        }
        let n = out.plt_entry();
        for (k, &id) in out.plt.iter().enumerate() {
            let at = (p.off + out.plt_header() + n * k as u64) as usize;
            let pc = p.addr + out.plt_header() + n * k as u64;
            let slot = out.got_slot(id, GOT_ADDR);
            match l.arch {
                Arch::I386 => break,
                Arch::S390x => {
                    let g = out.sec(Synth::GotPlt).map_or(0, |g| g.addr);
                    let rel = (24 * k) as u32;
                    s390x::write_plt(buf, at, pc, g + 8 * k as u64, p.addr, rel);
                }
                Arch::Ppc64 => ppc64::write_plt(buf, at, slot, out.got_base()),
                Arch::Arm => arm32::write_plt(buf, at, pc, slot),
                Arch::Riscv64 | Arch::Riscv32 => {
                    riscv::write_plt(buf, at, pc, slot, l.arch == Arch::Riscv64);
                }
                Arch::X86_64 => {
                    // jmp *slot(%rip); int3 padding
                    buf[at..at + 16].fill(0xcc);
                    buf[at] = 0xff;
                    buf[at + 1] = 0x25;
                    en.put32(buf, at + 2, slot.wrapping_sub(pc + 6) as u32);
                }
                Arch::Aarch64 => {
                    // adrp x16, slot; ldr x17, [x16, :lo12:slot]; br x17; nop
                    let adrp = crate::macho::a64::page21(0x9000_0010, pc, slot)?;
                    let ldr = crate::macho::a64::pageoff12(0xf940_0211, slot)?;
                    en.put32(buf, at, adrp);
                    en.put32(buf, at + 4, ldr);
                    en.put32(buf, at + 8, 0xd61f_0220);
                    en.put32(buf, at + 12, 0xd503_201f);
                }
            }
        }
    }
    if let Some(s) = out.sec(Synth::Interp) {
        let i = o.interp.as_deref().unwrap_or("");
        buf[s.off as usize..s.off as usize + i.len()].copy_from_slice(i.as_bytes());
    }
    if let Some(s) = out.sec(Synth::BuildId) {
        let at = s.off as usize;
        en.put32(buf, at, 4);
        en.put32(buf, at + 4, 8);
        en.put32(buf, at + 8, 3);
        buf[at + 12..at + 16].copy_from_slice(b"GNU\0");
    }
    // FDEs point at their CIEs by distance.
    for &(fde, cie) in &l.fdes {
        if !l.atoms[fde as usize].live {
            continue;
        }
        let Some(s) = out.secs.iter().find(|s| s.name == ".eh_frame" && s.alloc()) else {
            break;
        };
        let f = out.addrs[fde as usize];
        let c = out.addrs[cie as usize];
        en.put32(buf, (s.off + f - s.addr + 4) as usize, (f + 4 - c) as u32);
    }
    if let Some(h) = out.sec(Synth::EhFrameHdr) {
        let eh = out
            .secs
            .iter()
            .find(|s| s.name == ".eh_frame" && s.alloc())
            .expect(".eh_frame");
        let at = h.off as usize;
        buf[at..at + 4].copy_from_slice(&[1, 0x1b, 0x03, 0x3b]);
        en.put32(buf, at + 4, eh.addr.wrapping_sub(h.addr + 4) as u32);
        let mut table: Vec<(u64, u64)> = out
            .fdes
            .iter()
            .filter_map(|&f| {
                let r = l.atoms[f as usize].relocs.iter().find(|r| r.off == 8)?;
                let pc = (out.addr(r.target) as i64 + r.addend) as u64;
                Some((pc, out.addrs[f as usize]))
            })
            .collect();
        table.sort_unstable();
        table.dedup_by_key(|e| e.0);
        en.put32(buf, at + 8, table.len() as u32);
        for (k, (pc, f)) in table.iter().enumerate() {
            let e = at + 12 + 8 * k;
            en.put32(buf, e, pc.wrapping_sub(h.addr) as u32);
            en.put32(buf, e + 4, f.wrapping_sub(h.addr) as u32);
        }
    }
    // Dynamic symbols, versions, hash.
    if let Some(s) = out.sec(Synth::Dynsym) {
        for (k, d) in out.dynsyms.iter().enumerate() {
            let at = s.off as usize + s.entsize as usize * (k + 1);
            let sym = l.syms.get(d.id);
            let x = l.ext(d.id);
            let (shndx, value, size) = match sym.def {
                Def::Defined { atom, off } => {
                    let sec = out.shndx(atom);
                    let v = out.atom_addr(atom) + off;
                    let v = if x.ty == STT_TLS { v - out.tls.0 } else { v };
                    (sec, v, x.size)
                }
                Def::Absolute(v) => (0xfff1, v, x.size),
                _ => (0, 0, 0),
            };
            let bind = if shndx == 0 {
                if sym.weak_ref { 2 } else { 1 }
            } else if sym.weak_def {
                2
            } else {
                1
            };
            let ty = if shndx == 0 && !matches!(x.ty, STT_TLS) {
                if out.func(Target::Sym(d.id)) {
                    STT_FUNC
                } else {
                    x.ty.min(STT_FUNC)
                }
            } else {
                x.ty
            };
            // POWER keeps the local entry points.
            let other = if l.arch == Arch::Ppc64 && shndx != 0 {
                x.other & 0xe0
            } else {
                0
            };
            write_sym(
                en,
                &mut buf[at..],
                wide,
                d.name,
                (bind << 4) | ty,
                other,
                shndx,
                value,
                size,
            );
        }
    }
    if let Some(s) = out.sec(Synth::Versym) {
        en.put16(buf, s.off as usize, 0);
        for (k, d) in out.dynsyms.iter().enumerate() {
            en.put16(buf, s.off as usize + 2 * (k + 1), d.ver);
        }
    }
    if let Some(s) = out.sec(Synth::Verneed) {
        let mut at = s.off as usize;
        let files: Vec<u32> = out
            .needed
            .iter()
            .copied()
            .filter(|i| out.verneed.iter().any(|v| v.0 == *i))
            .chain(
                out.verneed
                    .iter()
                    .map(|v| v.0)
                    .filter(|i| !out.needed.contains(i)),
            )
            .collect::<Vec<_>>();
        let mut files_dedup = Vec::new();
        for f in files {
            if !files_dedup.contains(&f) {
                files_dedup.push(f);
            }
        }
        for (n, &f) in files_dedup.iter().enumerate() {
            let vers: Vec<&(u32, String, u16)> = out.verneed.iter().filter(|v| v.0 == f).collect();
            en.put16(buf, at, 1);
            en.put16(buf, at + 2, vers.len() as u16);
            en.put32(buf, at + 4, out.dynstr.at[&l.dsos[f as usize].soname]);
            en.put32(buf, at + 8, 16);
            let next = if n + 1 == files_dedup.len() {
                0
            } else {
                16 + 16 * vers.len() as u32
            };
            en.put32(buf, at + 12, next);
            at += 16;
            for (k, v) in vers.iter().enumerate() {
                en.put32(buf, at, elf_hash(&v.1));
                en.put16(buf, at + 4, 0);
                en.put16(buf, at + 6, v.2);
                en.put32(buf, at + 8, out.dynstr.at[&v.1]);
                en.put32(buf, at + 12, if k + 1 == vers.len() { 0 } else { 16 });
                at += 16;
            }
        }
    }
    if let Some(s) = out.sec(Synth::GnuHash) {
        let at = s.off as usize;
        let nb = out.gnu_buckets;
        let exports = &out.dynsyms[out.exports_from - 1..];
        en.put32(buf, at, nb);
        en.put32(buf, at + 4, out.exports_from as u32);
        en.put32(buf, at + 8, 1);
        en.put32(buf, at + 12, 26);
        let bits = 8 * u32::try_from(ptr).unwrap_or(8);
        let mut bloom = 0u64;
        for e in exports {
            bloom |= (1u64 << (e.hash % bits)) | (1u64 << ((e.hash >> 26) % bits));
        }
        put_word(en, buf, at + 16, bloom, wide);
        let buckets = at + 16 + ptr as usize;
        let chain = buckets + 4 * nb as usize;
        for (k, e) in exports.iter().enumerate() {
            let b = e.hash % nb;
            let idx = (out.exports_from + k) as u32;
            let slot = buckets + 4 * b as usize;
            if en.u32(buf, slot) == 0 {
                en.put32(buf, slot, idx);
            }
            let last = exports.get(k + 1).is_none_or(|n| n.hash % nb != b);
            en.put32(buf, chain + 4 * k, (e.hash & !1) | u32::from(last));
        }
    }
    if let Some(s) = out.sec(Synth::Dynstr) {
        let at = s.off as usize;
        buf[at..at + out.dynstr.data.len()].copy_from_slice(&out.dynstr.data);
    }
    if let Some(s) = out.sec(Synth::RelaDyn) {
        // RELATIVE first, for DT_RELACOUNT.
        rela.sort_by_key(|r| (r.1 != relative, r.0));
        if rela.len() != out.nrela {
            return Err(format!(
                "internal error: {} dynamic relocations, {} expected",
                rela.len(),
                out.nrela
            ));
        }
        for (k, &(off, ty, sym, add)) in rela.iter().enumerate() {
            let at = s.off as usize + s.entsize as usize * k;
            if wide {
                en.put64(buf, at, off);
                en.put64(buf, at + 8, (u64::from(sym) << 32) | u64::from(ty));
                en.put64(buf, at + 16, add as u64);
            } else {
                // REL: the addend is in the place.
                en.put32(buf, at, off as u32);
                en.put32(buf, at + 4, (sym << 8) | ty);
            }
        }
    }
    if let Some(s) = out.sec(Synth::RelPlt) {
        for (k, &(off, ty, sym, add)) in jump_slots.iter().enumerate() {
            let at = s.off as usize + s.entsize as usize * k;
            if wide {
                en.put64(buf, at, off);
                en.put64(buf, at + 8, (u64::from(sym) << 32) | u64::from(ty));
                en.put64(buf, at + 16, add as u64);
            } else {
                en.put32(buf, at, off as u32);
                en.put32(buf, at + 4, (sym << 8) | ty);
            }
        }
    }
    if let Some(s) = out.sec(Synth::Dynamic) {
        let sec = |x: Synth| out.sec(x);
        let ty_range = |ty: u32| {
            out.secs
                .iter()
                .find(|s| s.ty == ty && s.alloc())
                .map_or((0, 0), |s| (s.addr, s.size))
        };
        let sym_addr = |n: &str| l.syms.find(n).map_or(0, |id| out.addr(Target::Sym(id)));
        let nrel = rela.iter().filter(|r| r.1 == relative).count() as u64;
        for (k, &(tag, v)) in dyn_tags.iter().enumerate() {
            let val = match tag {
                DT_NEEDED => u64::from(out.dynstr.at[&l.dsos[v as usize].soname]),
                DT_RUNPATH => u64::from(out.dynstr.at[&o.rpaths.join(":")]),
                DT_INIT => sym_addr(o.init.as_deref().unwrap_or("_init")),
                DT_SONAME => o.soname.as_ref().map_or(0, |n| u64::from(out.dynstr.at[n])),
                DT_FINI => sym_addr("_fini"),
                DT_PREINIT_ARRAY => ty_range(SHT_PREINIT_ARRAY).0,
                DT_PREINIT_ARRAYSZ => ty_range(SHT_PREINIT_ARRAY).1,
                DT_INIT_ARRAY => ty_range(SHT_INIT_ARRAY).0,
                DT_INIT_ARRAYSZ => ty_range(SHT_INIT_ARRAY).1,
                DT_FINI_ARRAY => ty_range(SHT_FINI_ARRAY).0,
                DT_FINI_ARRAYSZ => ty_range(SHT_FINI_ARRAY).1,
                DT_GNU_HASH => sec(Synth::GnuHash).map_or(0, |s| s.addr),
                DT_STRTAB => sec(Synth::Dynstr).map_or(0, |s| s.addr),
                DT_SYMTAB => sec(Synth::Dynsym).map_or(0, |s| s.addr),
                DT_STRSZ => out.dynstr.data.len() as u64,
                DT_SYMENT => sec(Synth::Dynsym).map_or(0, |s| s.entsize),
                DT_RELA | DT_REL => sec(Synth::RelaDyn).map_or(0, |s| s.addr),
                DT_RELASZ | DT_RELSZ => sec(Synth::RelaDyn).map_or(0, |s| s.size),
                DT_RELAENT => 24,
                DT_RELENT => 8,
                DT_RELACOUNT | DT_RELCOUNT => nrel,
                DT_JMPREL => sec(Synth::RelPlt).map_or(0, |s| s.addr),
                DT_PLTRELSZ => sec(Synth::RelPlt).map_or(0, |s| s.size),
                // s390x: `.got`, whose header ld.so fills.
                DT_PLTGOT if l.arch == Arch::S390x => sec(Synth::Got).map_or(0, |s| s.addr),
                DT_PLTGOT => sec(Synth::GotPlt).map_or(0, |s| s.addr),
                DT_PLTREL if l.arch.rel() => DT_REL,
                DT_PLTREL => DT_RELA,
                DT_VERSYM => sec(Synth::Versym).map_or(0, |s| s.addr),
                DT_VERNEED => sec(Synth::Verneed).map_or(0, |s| s.addr),
                DT_VERNEEDNUM => u64::from(sec(Synth::Verneed).map_or(0, |s| s.info)),
                _ => v,
            };
            let at = s.off as usize + 2 * ptr as usize * k;
            put_word(en, buf, at, tag, wide);
            put_word(en, buf, at + ptr as usize, val, wide);
        }
    }
    if let (Some(s), Some(t)) = (out.sec(Synth::Symtab), symtab) {
        buf[s.off as usize..s.off as usize + t.len()].copy_from_slice(t);
        let st = out.sec(Synth::Strtab).expect(".strtab");
        buf[st.off as usize..st.off as usize + strtab.data.len()].copy_from_slice(&strtab.data);
    }
    if let Some(s) = out.sec(Synth::Shstrtab) {
        buf[s.off as usize..s.off as usize + shstr.data.len()].copy_from_slice(&shstr.data);
    }
    Ok(())
}

/// i386's PLT: PLT0 pushes the link map and jumps to ld.so's resolver
/// (`.got.plt` words 1 and 2); entry `k` jumps through its slot, which
/// first points back at its push of the `.rel.plt` offset. PIC entries
/// find `.got.plt` in `%ebx`.
pub(crate) fn write_i386_plt(out: &Out<'_>, p: &OSec, buf: &mut [u8]) {
    let en = out.l.arch.endian();
    let got = out.sec(Synth::GotPlt).map_or(0, |g| g.addr) as u32;
    let at = p.off as usize;
    let pic = out.o.pie;
    if pic {
        // pushl 4(%ebx); jmp *8(%ebx); nop
        buf[at..at + 16].copy_from_slice(&[
            0xff, 0xb3, 4, 0, 0, 0, 0xff, 0xa3, 8, 0, 0, 0, 0x90, 0x90, 0x90, 0x90,
        ]);
    } else {
        // pushl GOTPLT+4; jmp *GOTPLT+8; nop
        buf[at..at + 16].copy_from_slice(&[
            0xff, 0x35, 0, 0, 0, 0, 0xff, 0x25, 0, 0, 0, 0, 0x90, 0x90, 0x90, 0x90,
        ]);
        en.put32(buf, at + 2, got + 4);
        en.put32(buf, at + 8, got + 8);
    }
    for k in 0..out.plt.len() {
        let e = at + 16 + 16 * k;
        let slot = got + 4 * (3 + k as u32);
        // jmp *slot; pushl $reloc_offset; jmp PLT0
        buf[e..e + 16]
            .copy_from_slice(&[0xff, 0x25, 0, 0, 0, 0, 0x68, 0, 0, 0, 0, 0xe9, 0, 0, 0, 0]);
        if pic {
            buf[e + 1] = 0xa3;
            en.put32(buf, e + 2, slot - got);
        } else {
            en.put32(buf, e + 2, slot);
        }
        en.put32(buf, e + 7, 8 * k as u32);
        en.put32(buf, e + 12, (-(16 * (k as i64 + 2))) as u32);
    }
}
