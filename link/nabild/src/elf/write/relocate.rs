//! Atom contents and relocation application.

#[allow(clippy::wildcard_imports)]
use crate::elf::write::*;

/// Atom contents; gaps in code are NOPs.
pub(crate) fn write_atoms(out: &Out<'_>, buf: &mut [u8]) {
    let l = out.l;
    for s in out
        .secs
        .iter()
        .filter(|s| s.synth == Synth::Input && !s.nobits())
    {
        let range = s.off as usize..(s.off + s.size) as usize;
        if s.flags & SHF_EXECINSTR != 0 {
            match l.arch {
                Arch::X86_64 | Arch::I386 => buf[range.clone()].fill(0x90),
                Arch::Arm => {
                    for w in buf[range.clone()].chunks_exact_mut(4) {
                        w.copy_from_slice(&arm32::NOP.to_le_bytes());
                    }
                }
                Arch::Aarch64 => {
                    for w in buf[range.clone()].chunks_exact_mut(4) {
                        w.copy_from_slice(&0xd503_201fu32.to_le_bytes());
                    }
                }
                Arch::Riscv64 | Arch::Riscv32 => riscv::fill_nops(&mut buf[range.clone()]),
                Arch::Ppc64 => ppc64::fill_nops(&mut buf[range.clone()]),
                Arch::S390x => {
                    for w in buf[range.clone()].chunks_mut(4) {
                        w.copy_from_slice(&s390x::NOPR.to_be_bytes()[..w.len()]);
                    }
                }
            }
        }
        for &a in &s.atoms {
            let at = &l.atoms[a as usize];
            let is = out.sect_of(at);
            let dst = (s.off + out.addrs[a as usize] - s.addr) as usize;
            if is.nobits() {
                buf[dst..dst + at.size as usize].fill(0);
                continue;
            }
            let f = &l.files[at.file as usize];
            let src = (is.off + at.off) as usize;
            buf[dst..dst + at.size as usize].copy_from_slice(&f.data[src..src + at.size as usize]);
        }
    }
}

/// A dynamic relocation: offset, type, symbol index, addend.
pub(crate) type Rela = (u64, u32, u32, i64);

/// RISC-V: the value of each `*_HI20` a `PCREL_LO12_*` may name, by
/// its place.
pub(crate) fn riscv_his(out: &Out<'_>) -> HashMap<i64, i64> {
    let l = out.l;
    let mut his = HashMap::default();
    for s in out
        .secs
        .iter()
        .filter(|s| s.synth == Synth::Input && s.alloc())
    {
        for &a in &s.atoms {
            let base = out.addrs[a as usize];
            for r in l.atoms[a as usize]
                .relocs
                .iter()
                .filter(|r| riscv::pcrel_hi(r.kind))
            {
                let (g, tg) = match r.target {
                    Target::Sym(id) => {
                        let tls = if r.kind == riscv::TLS_GD_HI20 {
                            GOT_GD
                        } else {
                            GOT_TP
                        };
                        (out.got_slot(id, GOT_ADDR), out.got_slot(id, tls))
                    }
                    Target::Atom(_) => (0, 0),
                };
                let v = Values {
                    s: out.addr(r.target),
                    a: r.addend,
                    p: base + u64::from(r.off),
                    g,
                    tg,
                    ..Values::default()
                };
                his.insert(v.p as i64, riscv::hi_value(r.kind, &v));
            }
        }
    }
    his
}

/// Apply every relocation of every placed atom; the dynamic relocations.
pub(crate) fn apply_relocs(out: &Out<'_>, buf: &mut [u8]) -> Result<Vec<Rela>, String> {
    let l = out.l;
    let o = out.o;
    let en = l.arch.endian();
    let (relative, _, symbolic, _) = reloc::dyn_types(l.arch);
    let mut rela: Vec<Rela> = Vec::new();
    let mut errors = Vec::new();
    let got_base = out.got_base();
    let his = if l.arch.riscv() {
        riscv_his(out)
    } else {
        HashMap::default()
    };
    for s in out
        .secs
        .iter()
        .filter(|s| s.synth == Synth::Input && !s.nobits())
    {
        // lld's default: 1 where 0 would end a list.
        let tomb = o.dead_reloc.iter().find(|(n, _)| *n == s.name).map_or_else(
            || u64::from(matches!(s.name.as_str(), ".debug_ranges" | ".debug_loc")),
            |(_, v)| *v,
        );
        for &a in &s.atoms {
            let at = &l.atoms[a as usize];
            if at.relocs.is_empty() {
                continue;
            }
            let start = (s.off + out.addrs[a as usize] - s.addr) as usize;
            let b = &mut buf[start..start + at.size as usize];
            let base = out.addrs[a as usize];
            for (ri, r) in at.relocs.iter().enumerate() {
                if l.arch.riscv() && r.kind == riscv::SET_ULEB128 {
                    // The pair's difference at once: either half alone
                    // may not fit the field.
                    let sa = (out.addr(r.target) as i64).wrapping_add(r.addend);
                    let sub = at
                        .relocs
                        .get(ri + 1)
                        .filter(|n| n.kind == riscv::SUB_ULEB128 && n.off == r.off);
                    let v = sub.map_or(sa, |n| {
                        sa.wrapping_sub((out.addr(n.target) as i64).wrapping_add(n.addend))
                    });
                    if let Err(e) =
                        riscv::put_uleb("R_RISCV_SET_ULEB128", b, r.off as usize, v as u64)
                    {
                        errors.push(format!(
                            "{e}\n>>> referenced by {}:({}+{:#x})",
                            l.file_name(at.file),
                            out.sect_of(at).name,
                            at.off + u64::from(r.off)
                        ));
                    }
                    continue;
                }
                if l.arch.riscv()
                    && r.kind == riscv::SUB_ULEB128
                    && ri > 0
                    && at.relocs[ri - 1].kind == riscv::SET_ULEB128
                    && at.relocs[ri - 1].off == r.off
                {
                    continue;
                }
                let dead = match r.target {
                    Target::Atom(t) => t == DEAD || !l.atoms[t as usize].live,
                    Target::Sym(id) => match l.syms.get(id).def {
                        Def::Defined { atom, .. } => !l.atoms[atom as usize].live,
                        _ => false,
                    },
                };
                if dead && !s.alloc() {
                    let at_ = r.off as usize;
                    match (l.arch, r.kind) {
                        (
                            Arch::X86_64,
                            crate::elf::arch::x64::R_64 | crate::elf::arch::x64::DTPOFF64,
                        )
                        | (
                            Arch::Aarch64,
                            crate::elf::arch::a64::ABS64 | crate::elf::arch::a64::TLS_DTPREL64,
                        )
                        | (Arch::Riscv64 | Arch::Riscv32, riscv::R_64 | riscv::TLS_DTPREL64)
                        | (Arch::Ppc64, ppc64::ADDR64 | ppc64::DTPREL64)
                        | (Arch::S390x, s390x::R_64 | s390x::TLS_LDO64) => {
                            en.put64(b, at_, tomb);
                            continue;
                        }
                        (Arch::X86_64, crate::elf::arch::x64::R_32)
                        | (Arch::Aarch64, crate::elf::arch::a64::ABS32)
                        | (Arch::Arm, arm32::ABS32 | arm32::TLS_LDO32)
                        | (Arch::Riscv64 | Arch::Riscv32, riscv::R_32 | riscv::TLS_DTPREL32)
                        | (
                            Arch::I386,
                            crate::elf::arch::i386::R_32
                            | crate::elf::arch::i386::TLS_LDO_32
                            | crate::elf::arch::i386::TLS_DTPOFF32,
                        )
                        | (Arch::Ppc64, ppc64::ADDR32)
                        | (Arch::S390x, s390x::R_32 | s390x::TLS_LDO32) => {
                            en.put32(b, at_, tomb as u32);
                            continue;
                        }
                        _ => {}
                    }
                }
                let kind = out.kind(r.target);
                let own_tls =
                    o.shared && kind == SymKind::Local && reloc::dynamic_tls(l.arch, r.kind);
                let site = Site {
                    arch: l.arch,
                    ty: r.kind,
                    kind: if own_tls && !o.dynamic_tls {
                        SymKind::Dso
                    } else {
                        kind
                    },
                    func: out.func(r.target),
                    alloc: s.alloc(),
                    pie: o.pie,
                    dyn_tls: own_tls && o.dynamic_tls,
                };
                let mut target = out.addr(r.target);
                if let Target::Sym(id) = r.target {
                    if let Some(t) = out.arm_thunk(r.kind, id) {
                        target = out.thunk_addr(id, t);
                    }
                    // POWER: a local call enters past the TOC setup.
                    if l.arch == Arch::Ppc64 && r.kind == ppc64::REL24 && kind == SymKind::Local {
                        target += ppc64::local_entry(l.ext(id).other);
                    }
                }
                // POWER: a far call goes through its stub, which adds the
                // addend itself.
                if let Some(&st) = out.far.get(&(a, ri)) {
                    target = island_stub(out, st).wrapping_sub(r.addend as u64);
                }
                // The TLS slot (pair) a relocation names.
                let tls = match (l.arch, r.kind) {
                    _ if site.dyn_tls => GOT_GD,
                    (Arch::Arm, arm32::TLS_GD32)
                    | (Arch::Riscv64 | Arch::Riscv32, riscv::TLS_GD_HI20) => GOT_GD,
                    (Arch::Arm, arm32::TLS_LDM32) => GOT_LD,
                    _ => GOT_TP,
                };
                let (g, tg, size) = match r.target {
                    Target::Sym(id) => (
                        out.got_slot(id, GOT_ADDR),
                        out.got_slot(id, tls),
                        l.ext(id).size,
                    ),
                    Target::Atom(_) => (0, 0, 0),
                };
                let v = Values {
                    s: target,
                    a: r.addend,
                    p: base + u64::from(r.off),
                    g,
                    tg,
                    got_base,
                    tp: out.tpoff(target),
                    dtp: target.wrapping_sub(out.tls.0) as i64,
                    size,
                };
                let hi = if matches!(r.kind, riscv::PCREL_LO12_I | riscv::PCREL_LO12_S)
                    && l.arch.riscv()
                {
                    his.get(&(target as i64).wrapping_add(r.addend)).copied()
                } else {
                    None
                };
                match reloc::apply_hi(site, b, r.off as usize, &v, hi) {
                    Ok(Dyn::None) => {}
                    Ok(_) if !o.dynamic() => {}
                    Ok(Dyn::Relative(x)) => rela.push((v.p, relative, 0, x as i64)),
                    Ok(Dyn::Symbolic(x)) => {
                        let Target::Sym(id) = r.target else {
                            continue;
                        };
                        let k = out.dyn_index.get(&id).copied().unwrap_or(0);
                        rela.push((v.p, symbolic, k, x));
                    }
                    Err(e) => errors.push(format!(
                        "{e}\n>>> referenced by {}:({}+{:#x})",
                        l.file_name(at.file),
                        out.sect_of(at).name,
                        at.off + u64::from(r.off)
                    )),
                }
            }
        }
    }
    if !errors.is_empty() {
        errors.truncate(20);
        return Err(errors.join("\n"));
    }
    Ok(rela)
}

/// An address-sized field.
pub(crate) fn put_word(en: Endian, b: &mut [u8], at: usize, v: u64, wide: bool) {
    if wide {
        en.put64(b, at, v);
    } else {
        en.put32(b, at, v as u32);
    }
}
