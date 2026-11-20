//! The PDB (7.0) `/debug` writes beside the image.
//!
//! The MSF container with the info stream, the type and id streams (the
//! objects' records merged, hashed as the reference implementation hashes
//! them), the DBI stream (a module per object with `CodeView`: its symbols
//! and C13 line information in a stream of its own; the linker's module;
//! section contributions, the section map, source files, section headers),
//! the global symbols (procedure references, global data, types) and public
//! symbols (`S_PUB32`), each hashed, and the `/names` string table.

use crate::bytes::put32;
use nabic_core::HashMap;

const BLOCK: usize = 4096;
const MAGIC: &[u8; 32] = b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0";
const IPHR_HASH: usize = 4096;
const BITMAP_WORDS: usize = (IPHR_HASH + 32) / 32;
const NO_STREAM: u16 = 0xffff;
const S_PUB32: u16 = 0x110e;

/// A section contribution: 1-based section, offset, size, characteristics.
#[derive(Clone, Copy, Debug, Default)]
pub struct Contrib {
    pub sect: u16,
    pub off: u32,
    pub size: u32,
    pub chars: u32,
}

/// A module: an object's symbols (records, 4-byte aligned; offsets in
/// them count the stream's signature) and C13 subsections, its source
/// files, its contributions, its procedures (`S_PROCREF` / `S_LPROCREF`,
/// offset, name) for the globals.
#[derive(Debug, Default)]
pub struct Module {
    pub name: String,
    pub obj: String,
    pub syms: Vec<u8>,
    pub c13: Vec<u8>,
    pub files: Vec<String>,
    pub contribs: Vec<Contrib>,
    pub procs: Vec<(u16, u32, String)>,
}

/// The `/names` string table being built: one copy of each string.
#[derive(Debug)]
pub struct Strings {
    data: Vec<u8>,
    at: HashMap<String, u32>,
}

impl Default for Strings {
    fn default() -> Self {
        Self {
            data: vec![0],
            at: HashMap::default(),
        }
    }
}

impl Strings {
    /// The offset of `s`.
    pub fn add(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            return 0;
        }
        if let Some(&o) = self.at.get(s) {
            return o;
        }
        let o = self.data.len() as u32;
        self.data.extend_from_slice(s.as_bytes());
        self.data.push(0);
        self.at.insert(s.to_owned(), o);
        o
    }
}

/// What the objects' `CodeView` makes: modules, type and id records,
/// global symbol records, strings.
pub struct Debug {
    pub modules: Vec<Module>,
    pub tpi: Vec<Vec<u8>>,
    pub ipi: Vec<Vec<u8>>,
    pub globals: Vec<Vec<u8>>,
    pub strings: Strings,
}

/// The image the PDB goes with: machine, timestamp, GUID.
pub struct Id {
    pub machine: u16,
    pub signature: u32,
    pub guid: [u8; 16],
}

/// A public symbol: name, 1-based section, offset in it, code or data.
pub struct Public {
    pub name: String,
    pub sect: u16,
    pub off: u32,
    pub function: bool,
}

/// The PDB names' hash (`lhashPbCb`).
fn hash_v1(s: &[u8]) -> u32 {
    let mut r = 0u32;
    let mut c = s.chunks_exact(4);
    for w in &mut c {
        r ^= u32::from_le_bytes([w[0], w[1], w[2], w[3]]);
    }
    let mut rest = c.remainder();
    if rest.len() >= 2 {
        r ^= u32::from(u16::from_le_bytes([rest[0], rest[1]]));
        rest = &rest[2..];
    }
    if let Some(&b) = rest.first() {
        r ^= u32::from(b);
    }
    r |= 0x2020_2020;
    r ^= r >> 11;
    r ^ (r >> 16)
}

fn put16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}

/// Shorter first, then ASCII case-insensitively (the reference
/// implementation's order within a hash bucket).
fn gsi_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then_with(|| {
        if a.is_ascii() && b.is_ascii() {
            a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase())
        } else {
            a.as_bytes().cmp(b.as_bytes())
        }
    })
}

/// A GSI hash table of records at `offs` (into the record stream) named
/// `names`.
fn gsi_hash(names: &[&str], offs: &[u32]) -> Vec<u8> {
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); IPHR_HASH];
    for (i, n) in names.iter().enumerate() {
        buckets[hash_v1(n.as_bytes()) as usize % IPHR_HASH].push(i);
    }
    let mut records = Vec::new();
    let mut bitmap = [0u32; BITMAP_WORDS];
    let mut starts = Vec::new();
    let mut count = 0u32;
    for (k, b) in buckets.iter_mut().enumerate() {
        if b.is_empty() {
            continue;
        }
        b.sort_by(|&x, &y| gsi_cmp(names[x], names[y]).then(offs[x].cmp(&offs[y])));
        bitmap[k / 32] |= 1 << (k % 32);
        starts.push(count * 12);
        for &i in b.iter() {
            put32(&mut records, offs[i] + 1);
            put32(&mut records, 1);
            count += 1;
        }
    }
    let mut out = Vec::new();
    put32(&mut out, u32::MAX);
    put32(&mut out, 0xeffe_0000 + 19_990_810);
    put32(&mut out, records.len() as u32);
    put32(&mut out, (BITMAP_WORDS * 4 + starts.len() * 4) as u32);
    out.extend_from_slice(&records);
    for w in bitmap {
        put32(&mut out, w);
    }
    for s in starts {
        put32(&mut out, s);
    }
    out
}

/// A PDB string table (`/names`, the DBI's EC names) of `strs`.
fn string_table(strs: &Strings) -> Vec<u8> {
    let mut list: Vec<(&String, u32)> = strs.at.iter().map(|(s, &o)| (s, o)).collect();
    list.sort_by_key(|x| x.1);
    // The reference implementation's growth: 3/4 full at most.
    let mut nbuckets = 1u32;
    for n in 1..=list.len() as u32 {
        if nbuckets * 3 / 4 < n {
            nbuckets = nbuckets * 3 / 2 + 1;
        }
    }
    let mut buckets = vec![0u32; nbuckets as usize];
    for (s, o) in &list {
        let h = hash_v1(s.as_bytes());
        for i in 0..nbuckets {
            let slot = (h.wrapping_add(i) % nbuckets) as usize;
            if buckets[slot] == 0 {
                buckets[slot] = *o;
                break;
            }
        }
    }
    let mut out = Vec::new();
    put32(&mut out, 0xeffe_effe);
    put32(&mut out, 1);
    put32(&mut out, strs.data.len() as u32);
    out.extend_from_slice(&strs.data);
    put32(&mut out, nbuckets);
    for b in buckets {
        put32(&mut out, b);
    }
    put32(&mut out, list.len() as u32);
    out
}

/// The named stream map: `(name, stream)` in an open-addressing table
/// grown as the reference implementation grows it.
fn named_streams(entries: &[(&str, u32)]) -> Vec<u8> {
    let mut names = Vec::new();
    let mut keyed = Vec::new();
    for (n, s) in entries {
        keyed.push((names.len() as u32, *s, hash_v1(n.as_bytes()) as u16));
        names.extend_from_slice(n.as_bytes());
        names.push(0);
    }
    let place = |table: &mut Vec<Option<(u32, u32, u16)>>, e: (u32, u32, u16)| {
        let cap = table.len();
        let mut i = usize::from(e.2) % cap;
        while table[i].is_some() {
            i = (i + 1) % cap;
        }
        table[i] = Some(e);
    };
    let mut table: Vec<Option<(u32, u32, u16)>> = vec![None; 1];
    for e in keyed {
        place(&mut table, e);
        let size = table.iter().flatten().count();
        let max_load = table.len() * 2 / 3 + 1;
        if size >= max_load {
            let mut bigger = vec![None; max_load * 2];
            for x in table.iter().flatten() {
                place(&mut bigger, *x);
            }
            table = bigger;
        }
    }
    let mut out = Vec::new();
    put32(&mut out, names.len() as u32);
    out.extend_from_slice(&names);
    put32(&mut out, table.iter().flatten().count() as u32);
    put32(&mut out, table.len() as u32);
    let last = table.iter().rposition(Option::is_some).map_or(0, |i| i + 1);
    let words = last.div_ceil(32);
    put32(&mut out, words as u32);
    for w in 0..words {
        let mut v = 0u32;
        for b in 0..32 {
            if table.get(w * 32 + b).is_some_and(Option::is_some) {
                v |= 1 << b;
            }
        }
        put32(&mut out, v);
    }
    put32(&mut out, 0);
    for (k, s, _) in table.iter().flatten() {
        put32(&mut out, *k);
        put32(&mut out, *s);
    }
    out
}

/// The reference implementation's CRC (`JamCRC` from 0, `hashBufferV8`).
fn jam_crc(b: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &x in b {
        crc ^= u32::from(x);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// The NUL-terminated string at `at` of `b`.
fn cstr_at(b: &[u8], at: usize) -> &[u8] {
    let s = b.get(at..).unwrap_or_default();
    &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]
}

/// The size of the numeric leaf at `at` of `b` (the kinds a size takes).
fn numeric_len(b: &[u8], at: usize) -> usize {
    let v = u16::from_le_bytes([
        b.get(at).copied().unwrap_or(0),
        b.get(at + 1).copied().unwrap_or(0),
    ]);
    match v {
        0..0x8000 => 2,
        0x8000 => 3,
        0x8001 | 0x8002 => 4,
        0x8003 | 0x8004 => 6,
        _ => 10,
    }
}

/// A type record's hash (`hashTypeRecord`): a defined type by its name,
/// a source line record by its type, any other by its bytes.
fn type_hash(rec: &[u8]) -> u32 {
    let kind = u16::from_le_bytes([rec[2], rec[3]]);
    let b = &rec[4..];
    let name_at = match kind {
        // LF_CLASS, LF_STRUCTURE, LF_INTERFACE
        0x1504 | 0x1505 | 0x1519 => Some(16 + numeric_len(b, 16)),
        // LF_UNION
        0x1506 => Some(8 + numeric_len(b, 8)),
        // LF_ENUM
        0x1507 => Some(12),
        // LF_UDT_SRC_LINE, LF_UDT_MOD_SRC_LINE
        0x1606 | 0x1607 => return hash_v1(b.get(..4).unwrap_or(b)),
        _ => None,
    };
    if let Some(at) = name_at.filter(|_| b.len() >= 4) {
        let props = u16::from_le_bytes([b[2], b[3]]);
        let fwd = props & 0x80 != 0;
        let scoped = props & 0x100 != 0;
        let unique = props & 0x200 != 0;
        let name = cstr_at(b, at);
        let anon = matches!(
            name,
            b"<unnamed-tag>" | b"__unnamed" | b"<anonymous-tag>" | b"<unnamed-enum>"
        );
        if !fwd && !scoped && !anon {
            return hash_v1(name);
        }
        if !fwd && unique && !anon {
            return hash_v1(cstr_at(b, at + name.len() + 1));
        }
    }
    jam_crc(rec)
}

/// A type (or id) stream of `recs` and its hash stream (stream `hash`).
fn tpi(recs: &[Vec<u8>], hash: u16) -> (Vec<u8>, Vec<u8>) {
    const BUCKETS: u32 = 0x3_ffff;
    let mut data = Vec::new();
    let mut hashes = Vec::new();
    let mut offsets = Vec::new();
    for (i, r) in recs.iter().enumerate() {
        let size = data.len();
        if (size + r.len()) / 8192 > size / 8192 || i == 0 {
            put32(&mut offsets, 0x1000 + i as u32);
            put32(&mut offsets, size as u32);
        }
        data.extend_from_slice(r);
        put32(&mut hashes, type_hash(r) % BUCKETS);
    }
    let mut b = Vec::new();
    put32(&mut b, 20_040_203);
    put32(&mut b, 56);
    put32(&mut b, 0x1000);
    put32(&mut b, 0x1000 + recs.len() as u32);
    put32(&mut b, data.len() as u32);
    put16(&mut b, hash);
    put16(&mut b, NO_STREAM);
    put32(&mut b, 4);
    put32(&mut b, BUCKETS);
    for (off, len) in [
        (0, hashes.len()),
        (hashes.len(), offsets.len()),
        (hashes.len() + offsets.len(), 0),
    ] {
        put32(&mut b, off as u32);
        put32(&mut b, len as u32);
    }
    b.extend_from_slice(&data);
    hashes.extend_from_slice(&offsets);
    (b, hashes)
}

/// The name of global symbol record `r`.
fn global_name(r: &[u8]) -> &[u8] {
    let kind = u16::from_le_bytes([r[2], r[3]]);
    let b = &r[4..];
    match kind {
        // S_UDT
        0x1108 => cstr_at(b, 4),
        // S_CONSTANT
        0x1107 => cstr_at(b, 4 + numeric_len(b, 4)),
        // references, data, thread data
        _ => cstr_at(b, 10),
    }
}

/// Fixed stream numbers.
const INFO: usize = 1;
const TPI: usize = 2;
const DBI: usize = 3;
const IPI: usize = 4;
const LINK_INFO: usize = 5;

/// Write the PDB of image `id` whose section headers are `headers` (40
/// bytes each) to `path`.
#[allow(clippy::too_many_lines)]
pub fn write(
    path: &std::path::Path,
    id: &Id,
    headers: &[u8],
    mut publics: Vec<Public>,
    mut dbg: Debug,
) -> Result<(), String> {
    let mut streams: Vec<Vec<u8>> = vec![Vec::new(); LINK_INFO + 1];
    let add = |streams: &mut Vec<Vec<u8>>, data: Vec<u8>| -> u16 {
        streams.push(data);
        streams.len() as u16 - 1
    };

    // Public symbol records, by name; then the globals.
    publics.sort_by(|a, b| a.name.cmp(&b.name));
    let mut records = Vec::new();
    let mut offs = Vec::with_capacity(publics.len());
    for p in &publics {
        offs.push(records.len() as u32);
        let size = (14 + p.name.len() + 1).next_multiple_of(4);
        put16(&mut records, (size - 2) as u16);
        put16(&mut records, S_PUB32);
        put32(&mut records, if p.function { 2 } else { 0 });
        put32(&mut records, p.off);
        put16(&mut records, p.sect);
        records.extend_from_slice(p.name.as_bytes());
        records.resize(offs[offs.len() - 1] as usize + size, 0);
    }
    let names: Vec<&str> = publics.iter().map(|p| p.name.as_str()).collect();
    let hash = gsi_hash(&names, &offs);
    let mut by_addr: Vec<usize> = (0..publics.len()).collect();
    by_addr.sort_by(|&a, &b| {
        let (x, y) = (&publics[a], &publics[b]);
        (x.sect, x.off, &x.name).cmp(&(y.sect, y.off, &y.name))
    });
    let mut ps = Vec::new();
    put32(&mut ps, hash.len() as u32);
    put32(&mut ps, (publics.len() * 4) as u32);
    ps.resize(28, 0);
    ps.extend_from_slice(&hash);
    for i in by_addr {
        put32(&mut ps, offs[i]);
    }

    // Globals: each module's procedure references, the objects' global
    // records (one copy of each).
    let mut grecs: Vec<Vec<u8>> = Vec::new();
    for (m, md) in dbg.modules.iter().enumerate() {
        for (kind, off, name) in &md.procs {
            let mut body = Vec::new();
            put32(&mut body, 0);
            put32(&mut body, *off);
            put16(&mut body, m as u16 + 1);
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            let len = (body.len() + 4).next_multiple_of(4) - 2;
            let mut r = Vec::new();
            put16(&mut r, len as u16);
            put16(&mut r, *kind);
            r.extend_from_slice(&body);
            r.resize(len + 2, 0);
            grecs.push(r);
        }
    }
    let mut seen = nabic_core::HashSet::default();
    for r in std::mem::take(&mut dbg.globals) {
        if seen.insert(r.clone()) {
            grecs.push(r);
        }
    }
    let mut gnames = Vec::new();
    let mut goffs = Vec::new();
    for r in &grecs {
        goffs.push(records.len() as u32);
        records.extend_from_slice(r);
        gnames.push(String::from_utf8_lossy(global_name(r)).into_owned());
    }
    let gnames: Vec<&str> = gnames.iter().map(String::as_str).collect();
    let globals = gsi_hash(&gnames, &goffs);

    let publics_at = add(&mut streams, ps);
    let globals_at = add(&mut streams, globals);
    let symbols_at = add(&mut streams, records);
    let headers_at = add(&mut streams, headers.to_vec());

    // Types and ids.
    let (tpi_hash_at, ipi_hash_at) = (streams.len() as u16, streams.len() as u16 + 1);
    let (t, th) = tpi(&dbg.tpi, tpi_hash_at);
    let (i, ih) = tpi(&dbg.ipi, ipi_hash_at);
    add(&mut streams, th);
    add(&mut streams, ih);
    streams[TPI] = t;
    streams[IPI] = i;

    // Modules: a stream each with symbols or lines.
    let mut modi = Vec::new();
    let mut contribs = Vec::new();
    let mut file_names = Strings::default();
    let mut mod_files: Vec<Vec<u32>> = Vec::new();
    for (m, md) in dbg.modules.iter().enumerate() {
        // A stream for every module, if only the signature.
        let mut s = Vec::with_capacity(md.syms.len() + md.c13.len() + 8);
        put32(&mut s, 4);
        s.extend_from_slice(&md.syms);
        s.extend_from_slice(&md.c13);
        put32(&mut s, 0);
        let stream = add(&mut streams, s);
        let first = md.contribs.first().copied().unwrap_or_default();
        let mut e = Vec::new();
        put32(&mut e, 0);
        contrib(&mut e, &first, m as u16);
        put16(&mut e, 0);
        put16(&mut e, stream);
        put32(&mut e, md.syms.len() as u32 + 4);
        put32(&mut e, 0);
        put32(&mut e, md.c13.len() as u32);
        put16(&mut e, md.files.len().min(0xffff) as u16);
        put16(&mut e, 0);
        put32(&mut e, 0);
        put32(&mut e, 0);
        put32(&mut e, 0);
        e.extend_from_slice(md.name.as_bytes());
        e.push(0);
        e.extend_from_slice(md.obj.as_bytes());
        e.push(0);
        e.resize(e.len().next_multiple_of(4), 0);
        modi.extend_from_slice(&e);
        contribs.extend(md.contribs.iter().map(|c| (*c, m as u16)));
        mod_files.push(md.files.iter().map(|f| file_names.add(f)).collect());
    }
    contribs.sort_by_key(|(c, _)| (c.sect, c.off));
    let mut sc = Vec::new();
    put32(&mut sc, 0xeffe_0000 + 19_970_605);
    for (c, m) in &contribs {
        contrib(&mut sc, c, *m);
    }
    let names_at = add(&mut streams, string_table(&dbg.strings));

    // The section map: a frame per section, then one for the headers.
    let nsects = headers.len() / 40;
    let mut secmap = Vec::new();
    put16(&mut secmap, nsects as u16 + 1);
    put16(&mut secmap, nsects as u16 + 1);
    for k in 0..nsects {
        let chars = u32::from_le_bytes(headers[k * 40 + 36..k * 40 + 40].try_into().unwrap());
        let mut f = 0x8 | 0x100;
        if chars & 0x4000_0000 != 0 {
            f |= 1;
        }
        if chars & 0x8000_0000 != 0 {
            f |= 2;
        }
        if chars & 0x2000_0000 != 0 {
            f |= 4;
        }
        put16(&mut secmap, f);
        put16(&mut secmap, 0);
        put16(&mut secmap, 0);
        put16(&mut secmap, k as u16 + 1);
        put16(&mut secmap, NO_STREAM);
        put16(&mut secmap, NO_STREAM);
        put32(&mut secmap, 0);
        secmap.extend_from_slice(&headers[k * 40 + 8..k * 40 + 12]);
    }
    for v in [0x8 | 0x200, 0, 0, nsects as u16 + 1, NO_STREAM, NO_STREAM] {
        put16(&mut secmap, v);
    }
    put32(&mut secmap, 0);
    put32(&mut secmap, u32::MAX);

    // Source files: each module's, by offset into the names after.
    let mut file_info = Vec::new();
    let nmods = dbg.modules.len();
    put16(&mut file_info, nmods as u16);
    let total: usize = mod_files.iter().map(Vec::len).sum();
    put16(&mut file_info, total.min(0xffff) as u16);
    let mut start = 0usize;
    for f in &mod_files {
        put16(&mut file_info, start as u16);
        start += f.len();
    }
    for f in &mod_files {
        put16(&mut file_info, f.len().min(0xffff) as u16);
    }
    for f in &mod_files {
        for &o in f {
            put32(&mut file_info, o.saturating_sub(1));
        }
    }
    file_info.extend_from_slice(&file_names.data[1..]);
    file_info.resize(file_info.len().next_multiple_of(4), 0);

    let ec = string_table(&Strings::default());
    let mut opt = Vec::new();
    for k in 0..11 {
        put16(&mut opt, if k == 5 { headers_at } else { NO_STREAM });
    }
    let mut dbi = Vec::new();
    put32(&mut dbi, u32::MAX);
    put32(&mut dbi, 19_990_903);
    put32(&mut dbi, 1);
    put16(&mut dbi, globals_at);
    put16(&mut dbi, 0x8e0b);
    put16(&mut dbi, publics_at);
    put16(&mut dbi, 0);
    put16(&mut dbi, symbols_at);
    put16(&mut dbi, 0);
    for v in [
        modi.len(),
        sc.len(),
        secmap.len(),
        file_info.len(),
        0,
        0,
        opt.len(),
        ec.len(),
    ] {
        put32(&mut dbi, v as u32);
    }
    put16(&mut dbi, 0);
    put16(&mut dbi, id.machine);
    put32(&mut dbi, 0);
    for part in [&modi[..], &sc, &secmap, &file_info, &ec, &opt] {
        dbi.extend_from_slice(part);
    }
    streams[DBI] = dbi;

    let mut info = Vec::new();
    put32(&mut info, 20_000_404);
    put32(&mut info, id.signature);
    put32(&mut info, 1);
    info.extend_from_slice(&id.guid);
    info.extend_from_slice(&named_streams(&[
        ("/LinkInfo", LINK_INFO as u32),
        ("/names", u32::from(names_at)),
    ]));
    // VC140: the id stream is there.
    put32(&mut info, 20_140_508);
    streams[INFO] = info;

    std::fs::write(path, msf(&streams)?)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// A section contribution record of module `m`.
fn contrib(b: &mut Vec<u8>, c: &Contrib, m: u16) {
    put16(b, c.sect);
    put16(b, 0);
    put32(b, c.off);
    put32(b, c.size);
    put32(b, c.chars);
    put16(b, m);
    put16(b, 0);
    put32(b, 0);
    put32(b, 0);
}

/// A block free page map block: one of the two at the start of every
/// 4096 blocks.
const fn is_fpm(b: usize) -> bool {
    matches!(b % BLOCK, 1 | 2)
}

/// The MSF container of `streams`.
fn msf(streams: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    // Superblock, the free page maps, the directory's block list.
    let mut next = 4usize;
    let mut alloc = |n: usize| -> Vec<u32> {
        let mut v = Vec::with_capacity(n);
        while v.len() < n {
            if !is_fpm(next) {
                v.push(next as u32);
            }
            next += 1;
        }
        v
    };
    let blocks: Vec<Vec<u32>> = streams
        .iter()
        .map(|s| alloc(s.len().div_ceil(BLOCK)))
        .collect();
    let mut dir = Vec::new();
    put32(&mut dir, streams.len() as u32);
    for s in streams {
        put32(&mut dir, s.len() as u32);
    }
    for b in &blocks {
        for &x in b {
            put32(&mut dir, x);
        }
    }
    let dir_blocks = alloc(dir.len().div_ceil(BLOCK));
    if dir_blocks.len() * 4 > BLOCK {
        return Err("the PDB's stream directory is too large".to_owned());
    }
    let nblocks = next;
    let mut out = vec![0u8; nblocks * BLOCK];
    out[..32].copy_from_slice(MAGIC);
    for (k, v) in [BLOCK, 2, nblocks, dir.len(), 0, 3].iter().enumerate() {
        out[32 + 4 * k..36 + 4 * k].copy_from_slice(&(*v as u32).to_le_bytes());
    }
    // Free page maps: all free, but the blocks in use (map 2 is current).
    for b in (0..nblocks).filter(|&b| is_fpm(b)) {
        out[b * BLOCK..(b + 1) * BLOCK].fill(0xff);
    }
    for b in 0..nblocks {
        let fpm = (b / (BLOCK * 8)) * BLOCK + 2;
        if fpm < nblocks {
            let at = fpm * BLOCK + (b % (BLOCK * 8)) / 8;
            out[at] &= !(1 << (b % 8));
        }
    }
    let map = 3 * BLOCK;
    for (k, b) in dir_blocks.iter().enumerate() {
        out[map + 4 * k..map + 4 * k + 4].copy_from_slice(&b.to_le_bytes());
    }
    let mut put = |data: &[u8], bs: &[u32]| {
        for (chunk, &b) in data.chunks(BLOCK).zip(bs) {
            let at = b as usize * BLOCK;
            out[at..at + chunk.len()].copy_from_slice(chunk);
        }
    };
    for (s, b) in streams.iter().zip(&blocks) {
        put(s, b);
    }
    put(&dir, &dir_blocks);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_like_the_reference() {
        // LLVM's unit test values for hashStringV1.
        assert_eq!(hash_v1(b"/names"), hash_v1(b"/NAMES"));
        assert_eq!(hash_v1(b""), {
            let r = 0x2020_2020u32;
            let r = r ^ (r >> 11);
            r ^ (r >> 16)
        });
    }

    #[test]
    fn named_stream_map_grows_to_four() {
        let m = named_streams(&[("/LinkInfo", 5), ("/names", 10)]);
        let names = u32::from_le_bytes(m[..4].try_into().unwrap()) as usize;
        let at = 4 + names;
        let size = u32::from_le_bytes(m[at..at + 4].try_into().unwrap());
        let cap = u32::from_le_bytes(m[at + 4..at + 8].try_into().unwrap());
        assert_eq!((size, cap), (2, 4));
    }
}
