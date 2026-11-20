//! Build script of `nabir_common`: the runtime crates' wasm32 cfgs
//! (`../build.rs`), and on powerpc64 the object of `nabi_tls`,
//! `nabi_tls_addr` and the weak `rust_eh_personality` (tls.rs), which
//! stable Rust's missing inline assembly there cannot define, as a
//! bundled static library.

#[path = "../build.rs"]
mod shared;

fn main() {
    shared::main();
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.starts_with("powerpc64le-") {
        let dir = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
        std::fs::write(dir.join("libnabir_tls.a"), archive(&object()))
            .expect("write libnabir_tls.a");
        println!("cargo::rustc-link-search=native={}", dir.display());
        println!("cargo::rustc-link-lib=static=nabir_tls");
    }
}

/// A section header: name, type, flags, contents, link, info, align,
/// entsize.
type Shdr<'a> = (u64, u64, u64, &'a [u8], u64, u64, u64, u64);

/// The block's bytes; tls.rs checks the block fits.
const BLOCK: u64 = 256;

fn put(b: &mut Vec<u8>, v: u64, n: usize) {
    b.extend_from_slice(&v.to_le_bytes()[..n]);
}

/// An ELF64 little-endian `ELFv2` relocatable object: `.text` holds
/// `nabi_tls_addr` (`addis r3, r13, nabi_tls@tprel@ha; addi r3, r3,
/// nabi_tls@tprel@l; blr`, local exec) and `rust_eh_personality`
/// (`blr`), `.tbss` the block.
fn object() -> Vec<u8> {
    let mut text = Vec::new();
    for w in [0x3c6d_0000u32, 0x3863_0000, 0x4e80_0020, 0x4e80_0020] {
        put(&mut text, u64::from(w), 4);
    }
    // R_PPC64_TPREL16_HA / _LO of symbol 1, at the immediates.
    let mut rela = Vec::new();
    for (off, ty) in [(0u64, 72u64), (4, 70)] {
        put(&mut rela, off, 8);
        put(&mut rela, (1 << 32) | ty, 8);
        put(&mut rela, 0, 8);
    }
    let strtab = b"\0nabi_tls\0nabi_tls_addr\0rust_eh_personality\0";
    // (name, info, other, shndx, value, size).
    let syms: [(u64, u8, u8, u64, u64, u64); 4] = [
        (0, 0, 0, 0, 0, 0),
        // GLOBAL TLS in .tbss.
        (1, 0x16, 0, 3, 0, BLOCK),
        // GLOBAL FUNC, hidden.
        (10, 0x12, 2, 1, 0, 12),
        // WEAK FUNC.
        (24, 0x22, 0, 1, 12, 4),
    ];
    let mut symtab = Vec::new();
    for (name, info, other, shndx, value, size) in syms {
        put(&mut symtab, name, 4);
        symtab.extend_from_slice(&[info, other]);
        put(&mut symtab, shndx, 2);
        put(&mut symtab, value, 8);
        put(&mut symtab, size, 8);
    }
    let shstrtab = b"\0.text\0.rela.text\0.tbss\0.symtab\0.strtab\0.shstrtab\0.note.GNU-stack\0";
    // (name, type, flags, contents, link, info, align, entsize); `.tbss`
    // is NOBITS, its size apart.
    let sects: [Shdr<'_>; 8] = [
        (0, 0, 0, &[], 0, 0, 0, 0),
        (1, 1, 6, &text, 0, 0, 4, 0),
        (7, 4, 0x40, &rela, 4, 1, 8, 24),
        (18, 8, 0x403, &[], 0, 0, 8, 0),
        (24, 2, 0, &symtab, 5, 1, 8, 24),
        (32, 3, 0, strtab, 0, 0, 1, 0),
        (40, 3, 0, shstrtab, 0, 0, 1, 0),
        (50, 1, 0, &[], 0, 0, 1, 0),
    ];
    let mut b = vec![0u8; 64];
    let mut offs = Vec::new();
    for s in &sects {
        b.resize(b.len().next_multiple_of(8), 0);
        offs.push(b.len() as u64);
        b.extend_from_slice(s.3);
    }
    b.resize(b.len().next_multiple_of(8), 0);
    let shoff = b.len() as u64;
    for (k, s) in sects.iter().enumerate() {
        let size = if k == 3 { BLOCK } else { s.3.len() as u64 };
        put(&mut b, s.0, 4);
        put(&mut b, s.1, 4);
        put(&mut b, s.2, 8);
        put(&mut b, 0, 8);
        put(&mut b, if k == 0 { 0 } else { offs[k] }, 8);
        put(&mut b, size, 8);
        put(&mut b, s.4, 4);
        put(&mut b, s.5, 4);
        put(&mut b, s.6, 8);
        put(&mut b, s.7, 8);
    }
    let mut h = vec![0x7f, b'E', b'L', b'F', 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    // ET_REL, EM_PPC64, version 1, no entry or program headers.
    put(&mut h, 1, 2);
    put(&mut h, 21, 2);
    put(&mut h, 1, 4);
    put(&mut h, 0, 8);
    put(&mut h, 0, 8);
    put(&mut h, shoff, 8);
    // ELFv2.
    put(&mut h, 2, 4);
    for v in [64, 0, 0, 64, sects.len() as u64, 6] {
        put(&mut h, v, 2);
    }
    b[..64].copy_from_slice(&h);
    b
}

/// A GNU archive of the one member `tls.o`.
fn archive(o: &[u8]) -> Vec<u8> {
    let mut a = b"!<arch>\n".to_vec();
    let hdr = format!(
        "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        "tls.o/",
        0,
        0,
        0,
        644,
        o.len()
    );
    a.extend_from_slice(hdr.as_bytes());
    a.extend_from_slice(o);
    if o.len() % 2 == 1 {
        a.push(b'\n');
    }
    a
}
