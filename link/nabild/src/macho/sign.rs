//! Ad-hoc code signature: a `SuperBlob` holding one `CodeDirectory` of
//! SHA-256 page hashes, flagged linker-signed as ld64 and lld make it.
//! arm64 macOS runs no unsigned code.

use nabic_core::hash::sha256;

const PAGE: u64 = 4096;
const CD_HEADER: u64 = 88;
const CSMAGIC_EMBEDDED_SIGNATURE: u32 = 0xfade_0cc0;
const CSMAGIC_CODEDIRECTORY: u32 = 0xfade_0c02;
const CS_ADHOC: u32 = 0x2;
const CS_LINKER_SIGNED: u32 = 0x2_0000;
const CS_EXECSEG_MAIN_BINARY: u64 = 1;

/// The signature's size for `code_limit` bytes signed as `ident`.
#[must_use]
pub const fn size(code_limit: u64, ident: &str) -> u64 {
    let slots = code_limit.div_ceil(PAGE);
    (12 + 8 + CD_HEADER + ident.len() as u64 + 1 + 32 * slots).next_multiple_of(16)
}

/// SHA-256 of each 4 KiB page of `file`, on all cores.
#[must_use]
pub fn page_hashes(file: &[u8]) -> Vec<[u8; 32]> {
    let pages: Vec<&[u8]> = file.chunks(PAGE as usize).collect();
    let mut out = vec![[0u8; 32]; pages.len()];
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let per = pages.len().div_ceil(threads).max(16);
    std::thread::scope(|s| {
        for (o, p) in out.chunks_mut(per).zip(pages.chunks(per)) {
            s.spawn(move || {
                for (h, page) in o.iter_mut().zip(p) {
                    *h = sha256(page);
                }
            });
        }
    });
    out
}

/// The hash of page `k` of `file`, again (after a patch).
pub fn rehash(file: &[u8], hashes: &mut [[u8; 32]], k: usize) {
    let s = k * PAGE as usize;
    let e = (s + PAGE as usize).min(file.len());
    hashes[k] = sha256(&file[s..e]);
}

/// Sign `file[..code_limit]` with its page `hashes`, writing the blob at
/// `code_limit`. The executable segment is `text` (file offset, size),
/// `main` in an executable (not a dylib).
pub fn sign(
    file: &mut [u8],
    code_limit: u64,
    hashes: &[[u8; 32]],
    ident: &str,
    text: (u64, u64),
    main: bool,
) {
    let slots = code_limit.div_ceil(PAGE);
    let ident_off = CD_HEADER;
    let hash_off = ident_off + ident.len() as u64 + 1;
    let cd_len = hash_off + 32 * slots;
    let total = size(code_limit, ident);
    let mut b: Vec<u8> = Vec::with_capacity(total as usize);
    let be32 = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_be_bytes());
    let be64 = |b: &mut Vec<u8>, v: u64| b.extend_from_slice(&v.to_be_bytes());
    be32(&mut b, CSMAGIC_EMBEDDED_SIGNATURE);
    be32(&mut b, total as u32);
    be32(&mut b, 1);
    // CSSLOT_CODEDIRECTORY at offset 20.
    be32(&mut b, 0);
    be32(&mut b, 20);
    be32(&mut b, CSMAGIC_CODEDIRECTORY);
    be32(&mut b, cd_len as u32);
    be32(&mut b, 0x2_0400);
    be32(&mut b, CS_ADHOC | CS_LINKER_SIGNED);
    be32(&mut b, hash_off as u32);
    be32(&mut b, ident_off as u32);
    be32(&mut b, 0);
    be32(&mut b, slots as u32);
    be32(&mut b, code_limit as u32);
    // hashSize, hashType (SHA-256), platform, pageSize (log2).
    b.extend_from_slice(&[32, 2, 0, 12]);
    // spare2, scatterOffset, teamOffset, spare3.
    for _ in 0..4 {
        be32(&mut b, 0);
    }
    be64(&mut b, 0);
    be64(&mut b, text.0);
    be64(&mut b, text.1);
    be64(&mut b, if main { CS_EXECSEG_MAIN_BINARY } else { 0 });
    debug_assert_eq!(b.len() as u64, 20 + CD_HEADER);
    b.extend_from_slice(ident.as_bytes());
    b.push(0);
    debug_assert_eq!(hashes.len() as u64, slots);
    for h in hashes {
        b.extend_from_slice(h);
    }
    b.resize(total as usize, 0);
    let at = code_limit as usize;
    file[at..at + b.len()].copy_from_slice(&b);
}
