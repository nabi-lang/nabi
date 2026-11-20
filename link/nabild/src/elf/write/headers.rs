//! ELF and program headers, section headers and the build id.

#[allow(clippy::wildcard_imports)]
use crate::elf::write::*;

/// The ELF header, program headers and section headers.
pub(crate) fn write_headers(
    out: &Out<'_>,
    buf: &mut [u8],
    groups: &[u8],
    nph: usize,
    base: u64,
    shoff: u64,
    shstr: &StrTab,
) {
    let l = out.l;
    let o = out.o;
    let en = l.arch.endian();
    let secs = &out.secs;
    let entry = l
        .syms
        .find(&o.entry)
        .map_or(0, |id| out.addr(Target::Sym(id)));
    let wide = l.arch.elf64();
    let ptr = l.arch.ptr();
    let (ehdr, phent, shent) = if wide { (64, 56, 64) } else { (52, 32, 40) };
    buf[..16].copy_from_slice(&[
        0x7f,
        b'E',
        b'L',
        b'F',
        if wide { 2 } else { 1 },
        if en.big { 2 } else { 1 },
        1,
        o.osabi,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ]);
    en.put16(buf, 16, if o.pie { 3 } else { 2 });
    en.put16(buf, 18, l.arch.machine());
    en.put32(buf, 20, 1);
    let w = ptr as usize;
    put_word(en, buf, 24, entry, wide);
    put_word(en, buf, 24 + w, ehdr as u64, wide);
    put_word(en, buf, 24 + 2 * w, shoff, wide);
    let at = 24 + 3 * w;
    let flags = match l.arch {
        Arch::Arm => arm32::EF_ARM,
        Arch::Riscv64 | Arch::Riscv32 => l.eflags.unwrap_or(0),
        Arch::Ppc64 => ppc64::EF_ELFV2,
        _ => 0,
    };
    en.put32(buf, at, flags);
    en.put16(buf, at + 4, ehdr);
    en.put16(buf, at + 6, phent);
    en.put16(buf, at + 8, nph as u16);
    en.put16(buf, at + 10, shent);
    en.put16(buf, at + 12, secs.len() as u16 + 1);
    en.put16(buf, at + 14, secs.len() as u16);

    let mut ph: Vec<[u64; 7]> = Vec::new();
    let mut phdr =
        |ty: u32, flags: u32, off: u64, addr: u64, filesz: u64, memsz: u64, align: u64| {
            ph.push([
                u64::from(ty) | (u64::from(flags) << 32),
                off,
                addr,
                addr,
                filesz,
                memsz,
                align,
            ]);
        };
    let span = |p: &dyn Fn(&OSec) -> bool| -> Option<(u64, u64, u64, u64, u64)> {
        let v: Vec<&OSec> = secs.iter().filter(|s| s.alloc() && p(s)).collect();
        let first = v.first()?;
        let file_end = v
            .iter()
            .filter(|s| !s.nobits())
            .map(|s| s.off + s.size)
            .max()
            .unwrap_or(first.off);
        let mem_end = v
            .iter()
            .map(|s| s.addr + s.size)
            .max()
            .unwrap_or(first.addr);
        let align = v.iter().map(|s| s.align).max().unwrap_or(1);
        Some((
            first.off,
            first.addr,
            file_end - first.off,
            mem_end - first.addr,
            align,
        ))
    };
    let (ehdr, phent) = (u64::from(ehdr), u64::from(phent));
    let hdr_size = ehdr + phent * nph as u64;
    if out.sec(Synth::Interp).is_some() || l.arch.riscv() {
        let n = hdr_size - ehdr;
        phdr(PT_PHDR, 4, ehdr, base + ehdr, n, n, ptr);
    }
    if let Some(s) = out.sec(Synth::Interp) {
        phdr(PT_INTERP, 4, s.off, s.addr, s.size, s.size, 1);
    }
    let page = out.o.max_page.unwrap_or_else(|| l.arch.max_page());
    for &g in groups {
        let flags = match g {
            0 => 4,
            1 => 5,
            _ => 6,
        };
        let sp = span(&|s: &OSec| s.rank().0 == g && !(s.tls() && s.nobits()));
        let (off, addr, filesz, memsz) = match (g, sp) {
            (0, Some((_, _, _, _, _))) => {
                let (o2, a2, f2, m2, _) = sp.unwrap();
                (0, base, o2 + f2, (a2 + m2) - base)
            }
            (0, None) => (0, base, hdr_size, hdr_size),
            (_, Some((o2, a2, f2, m2, _))) => (o2, a2, f2, m2),
            (_, None) => continue,
        };
        phdr(PT_LOAD, flags, off, addr, filesz, memsz, page);
    }
    if let Some((off, addr, filesz, memsz, align)) = span(&|s: &OSec| s.tls()) {
        phdr(PT_TLS, 4, off, addr, filesz, memsz, align);
    }
    if let Some(s) = out.sec(Synth::Exidx) {
        phdr(arm32::PT_ARM_EXIDX, 4, s.off, s.addr, s.size, s.size, 4);
    }
    if let Some(s) = out.sec(Synth::Dynamic) {
        phdr(PT_DYNAMIC, 6, s.off, s.addr, s.size, s.size, ptr);
    }
    if let Some((off, addr, filesz, memsz, _)) =
        span(&|s: &OSec| s.rank().0 == 2 && !(s.tls() && s.nobits()))
    {
        let end = (addr + memsz).next_multiple_of(COMMON_PAGE);
        phdr(PT_GNU_RELRO, 4, off, addr, filesz, end - addr, 1);
    }
    if let Some(s) = out.sec(Synth::EhFrameHdr) {
        phdr(PT_GNU_EH_FRAME, 4, s.off, s.addr, s.size, s.size, 4);
    }
    phdr(PT_GNU_STACK, 6, 0, 0, 0, 0, 0);
    if let Some((off, addr, filesz, memsz, align)) = span(&|s: &OSec| s.ty == SHT_NOTE) {
        phdr(PT_NOTE, 4, off, addr, filesz, memsz, align);
    }
    if let Some(s) = secs
        .iter()
        .find(|s| s.ty == riscv::SHT_ATTRIBUTES && l.arch.riscv())
    {
        phdr(riscv::PT_ATTRIBUTES, 4, s.off, 0, s.size, s.size, 1);
    }
    debug_assert_eq!(ph.len(), nph);
    for (k, p) in ph.iter().enumerate() {
        let at = (ehdr + phent * k as u64) as usize;
        let (ty, flags) = (p[0] as u32, (p[0] >> 32) as u32);
        en.put32(buf, at, ty);
        if wide {
            en.put32(buf, at + 4, flags);
            for (j, v) in p[1..].iter().enumerate() {
                en.put64(buf, at + 8 + 8 * j, *v);
            }
        } else {
            // type, offset, vaddr, paddr, filesz, memsz, flags, align
            for (j, v) in p[1..6].iter().enumerate() {
                en.put32(buf, at + 4 + 4 * j, *v as u32);
            }
            en.put32(buf, at + 24, flags);
            en.put32(buf, at + 28, p[6] as u32);
        }
    }

    // Section headers.
    let shent = usize::from(shent);
    let mut at = shoff as usize + shent;
    for s in secs {
        // SHF_INFO_LINK stays on `.rel.plt`, as lld writes it.
        let mut strip = object::SHF_MERGE | object::SHF_STRINGS | object::SHF_GROUP;
        if s.synth != Synth::RelPlt {
            strip |= object::SHF_INFO_LINK;
        }
        en.put32(buf, at, shstr.at[&s.name]);
        en.put32(buf, at + 4, s.ty);
        let mut f = at + 8;
        for v in [s.flags & !strip, s.addr, s.off, s.size] {
            put_word(en, buf, f, v, wide);
            f += w;
        }
        en.put32(buf, f, s.link);
        en.put32(buf, f + 4, s.info);
        put_word(en, buf, f + 8, s.align.max(1), wide);
        put_word(en, buf, f + 8 + w, s.entsize, wide);
        at += shent;
    }
}

/// `--build-id`: a hash of the whole file into the note.
pub(crate) fn build_id(out: &Out<'_>, buf: &mut [u8]) {
    let Some(s) = out.sec(Synth::BuildId) else {
        return;
    };
    // Two lanes of a multiply-xorshift hash over 8-byte words.
    let (mut a, mut b) = (0x9e37_79b9_7f4a_7c15u64, 0xc2b2_ae3d_27d4_eb4fu64);
    let mut words = buf.chunks_exact(8);
    for w in &mut words {
        let x = u64::from_le_bytes(w.try_into().unwrap());
        a = (a ^ x).wrapping_mul(0xff51_afd7_ed55_8ccd).rotate_left(31);
        b = (b ^ x.rotate_left(17))
            .wrapping_mul(0xc4ce_b9fe_1a85_ec53)
            .rotate_left(29);
    }
    for &x in words.remainder() {
        a = (a ^ u64::from(x)).wrapping_mul(0xff51_afd7_ed55_8ccd);
    }
    let h = (a ^ (b >> 7)).wrapping_mul(0x94d0_49bb_1331_11eb) ^ b;
    let at = s.off as usize + 16;
    buf[at..at + 8].copy_from_slice(&h.to_le_bytes());
}
