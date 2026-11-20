//! Data segments, string merging and the linear memory layout.

#[allow(clippy::wildcard_imports)]
use crate::wasm::write::*;

/// Per string of a merged chunk: (input offset, output offset).
pub(crate) type Pieces = Vec<(u32, u32)>;

/// The NUL-terminated strings of `chunks`, each once, a string that ends
/// another sharing its tail (LLVM's raw string table): the bytes and each
/// chunk's pieces.
pub(crate) fn merge_strings(chunks: &[&[u8]]) -> Result<(Vec<u8>, Vec<Pieces>), String> {
    let mut uniq: Vec<&[u8]> = Vec::new();
    let mut at: HashMap<&[u8], usize> = HashMap::default();
    let mut refs: Vec<Vec<(u32, usize)>> = Vec::with_capacity(chunks.len());
    for c in chunks {
        let mut v = Vec::new();
        let mut off = 0;
        while off < c.len() {
            let end = c[off..]
                .iter()
                .position(|&b| b == 0)
                .ok_or("string is not null terminated")?;
            let s = &c[off..=off + end];
            let k = *at.entry(s).or_insert_with(|| {
                uniq.push(s);
                uniq.len() - 1
            });
            v.push((off as u32, k));
            off += end + 1;
        }
        refs.push(v);
    }
    let mut order: Vec<usize> = (0..uniq.len()).collect();
    order.sort_by(|&a, &b| uniq[b].iter().rev().cmp(uniq[a].iter().rev()));
    let mut offs = vec![0u32; uniq.len()];
    let mut bytes: Vec<u8> = Vec::new();
    let mut prev: &[u8] = &[];
    for k in order {
        let s = uniq[k];
        if prev.ends_with(s) {
            offs[k] = (bytes.len() - s.len()) as u32;
            continue;
        }
        offs[k] = bytes.len() as u32;
        bytes.extend_from_slice(s);
        prev = s;
    }
    let maps = refs
        .into_iter()
        .map(|v| v.into_iter().map(|(i, k)| (i, offs[k])).collect())
        .collect();
    Ok((bytes, maps))
}

/// Offset `off` of a merged chunk in its group.
pub(crate) fn piece(map: &[(u32, u32)], off: u64) -> u64 {
    let i = map.partition_point(|&(i, _)| u64::from(i) <= off);
    let (i, o) = map[i.saturating_sub(1)];
    u64::from(o) + off - u64::from(i)
}

/// The output name of a data segment: TLS ones as `.tdata`, the rest by
/// prefix.
pub(crate) fn seg_name(name: &str, flags: u32) -> &str {
    if flags & SEG_TLS != 0 {
        return ".tdata";
    }
    for p in [".text", ".data", ".bss", ".rodata"] {
        if name.len() > p.len() && name.starts_with(p) && name.as_bytes()[p.len()] == b'.' {
            return p;
        }
    }
    name
}

pub(crate) fn c_ident(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with(|c: char| c.is_ascii_digit())
        && s.bytes().all(|c| c == b'_' || c.is_ascii_alphanumeric())
}

/// Live data chunks into output segments, by name, in file order; sorted
/// `.tdata`, `.rodata`, `.data`, others, `.bss`; strings merged.
pub(crate) fn segments(l: &mut Link, out: &mut Out) -> Result<(), String> {
    let mut by_name: HashMap<String, usize> = HashMap::default();
    let mut segs: Vec<(OutSeg, Vec<AtomId>)> = Vec::new();
    for &fi in &l.order {
        for &a in &l.files[fi as usize].segs {
            if !l.atoms[a as usize].live {
                continue;
            }
            let Chunk::Data { name, flags, .. } = &l.chunks[a as usize] else {
                continue;
            };
            let n = seg_name(name, *flags);
            let k = *by_name.entry(n.to_owned()).or_insert_with(|| {
                segs.push((
                    OutSeg {
                        name: n.to_owned(),
                        items: Vec::new(),
                        align: 0,
                        size: 0,
                        start: 0,
                        bss: n.starts_with(".bss"),
                        passive: false,
                        index: 0,
                    },
                    Vec::new(),
                ));
                segs.len() - 1
            });
            segs[k].1.push(a);
        }
    }
    let rank = |n: &str| {
        if n.starts_with(".tdata") {
            0
        } else if n.starts_with(".rodata") {
            1
        } else if n.starts_with(".data") {
            2
        } else if n.starts_with(".bss") {
            4
        } else {
            3
        }
    };
    segs.sort_by_key(|(s, _)| rank(&s.name));
    for (k, (mut s, atoms)) in segs.into_iter().enumerate() {
        s.index = k as u32;
        // One merge group per segment flags, where its first chunk was.
        let mut groups: Vec<(u32, usize)> = Vec::new();
        for &a in &atoms {
            let Chunk::Data { flags, merge, .. } = &l.chunks[a as usize] else {
                continue;
            };
            if !*merge {
                s.items.push(Item::Atom(a));
                continue;
            }
            let g = if let Some(&(_, g)) = groups.iter().find(|(f, _)| f == flags) {
                g
            } else {
                out.merged.push(Merged {
                    atoms: Vec::new(),
                    bytes: Vec::new(),
                    off: 0,
                });
                let g = out.merged.len() - 1;
                groups.push((*flags, g));
                s.items.push(Item::Merged(g));
                g
            };
            out.merged[g].atoms.push(a);
        }
        for &(_, g) in &groups {
            merge_group(l, out, g)?;
        }
        let mut size = 0u64;
        for &it in &s.items {
            let (align, len) = match it {
                Item::Atom(a) => {
                    let at = &l.atoms[a as usize];
                    (at.align, at.size)
                }
                Item::Merged(g) => (0, out.merged[g].bytes.len() as u64),
            };
            s.align = s.align.max(align);
            size = size.next_multiple_of(1 << align);
            match it {
                Item::Atom(a) => l.atoms[a as usize].addr = size,
                Item::Merged(g) => out.merged[g].off = size,
            }
            size += len;
        }
        s.size = size;
        for &it in &s.items {
            match it {
                Item::Atom(a) => {
                    out.seg_of.insert(a, k);
                }
                Item::Merged(g) => {
                    for &a in &out.merged[g].atoms {
                        out.seg_of.insert(a, k);
                    }
                }
            }
        }
        out.segs.push(s);
    }
    Ok(())
}

/// Merge group `g`'s strings; each chunk's pieces.
pub(crate) fn merge_group(l: &Link, out: &mut Out, g: usize) -> Result<(), String> {
    let atoms = out.merged[g].atoms.clone();
    let chunks: Vec<&[u8]> = atoms
        .iter()
        .map(|&a| {
            let at = &l.atoms[a as usize];
            &l.files[at.file as usize].data[at.off as usize..(at.off + at.size) as usize]
        })
        .collect();
    let (bytes, maps) = merge_strings(&chunks).map_err(|e| {
        let a = &l.atoms[atoms[0] as usize];
        format!("{}: {e}", l.file_name(a.file))
    })?;
    for (&a, m) in atoms.iter().zip(maps) {
        out.pieces.insert(a, (g, m));
    }
    out.merged[g].bytes = bytes;
    Ok(())
}

/// Linear memory: the stack (first with `--stack-first`), the segments,
/// the init flag, the heap; the linker's address symbols.
pub(crate) fn layout(l: &mut Link, o: &Options, out: &mut Out) -> Result<(), String> {
    let mut at = 0u64;
    let mut stack = (0, 0);
    let mut place_stack = |at: &mut u64| {
        *at = at.next_multiple_of(STACK_ALIGN);
        let low = *at;
        *at += o.stack_size;
        stack = (low, *at);
    };
    if o.stack_first {
        place_stack(&mut at);
        if o.global_base != 0 {
            if o.global_base < at {
                return Err(
                    "--global-base cannot be less than stack size when --stack-first is used"
                        .to_owned(),
                );
            }
            at = o.global_base;
        }
    } else {
        at = if o.global_base != 0 {
            o.global_base
        } else {
            1024
        };
    }
    let global_base = at;
    let data_start = at;
    let shared = o.shared_memory;
    for s in &mut out.segs {
        at = at.next_multiple_of(1 << s.align);
        s.start = at;
        s.passive = shared;
        at += s.size;
    }
    let mut tls = (0, 0, 0);
    for s in &out.segs {
        if s.tls() {
            tls = (s.start, s.size, 1u64 << s.align);
        }
        for &it in &s.items {
            match it {
                Item::Atom(a) => l.atoms[a as usize].addr += s.start,
                Item::Merged(g) => {
                    let base = s.start + out.merged[g].off;
                    for &a in &out.merged[g].atoms {
                        l.atoms[a as usize].addr = base;
                    }
                }
            }
        }
    }
    out.tls_start = tls.0;
    let has_tls = out.segs.iter().any(OutSeg::tls);
    if has_tls {
        let set = |l: &mut Link, id: Option<SymId>, v: u64| {
            if let Some(id) = id {
                set_global(l, id, v);
            }
        };
        set(l, l.synth.tls_size, tls.1);
        set(l, l.synth.tls_align, tls.2);
        if !shared {
            set(l, l.synth.tls_base, tls.0);
        }
    }
    let emit_bss = emit_bss(o, out);
    let passive_init = out
        .segs
        .iter()
        .any(|s| s.passive || (o.import_memory && s.bss && !emit_bss));
    if shared && passive_init {
        at = at.next_multiple_of(4);
        out.flag_addr = at;
        at += 4;
    }
    let data_end = at;
    if !o.stack_first {
        place_stack(&mut at);
    }
    set_global(l, l.synth.stack_pointer, stack.1);
    at = at.next_multiple_of(16);
    let heap_base = at;
    let max_setting = 1u64 << 32;
    if o.initial_memory != 0 {
        if o.initial_memory % PAGE != 0 {
            return Err(format!(
                "initial memory must be aligned to the page size ({PAGE} bytes)"
            ));
        }
        if at > o.initial_memory {
            return Err(format!("initial memory too small, {at} bytes needed"));
        }
        if o.initial_memory > max_setting {
            return Err(format!(
                "initial memory too large, cannot be greater than {max_setting}"
            ));
        }
        at = o.initial_memory;
    }
    at = at.next_multiple_of(PAGE);
    out.pages = at / PAGE;
    let heap_end = at;
    if o.max_memory != 0 {
        if o.max_memory % PAGE != 0 {
            return Err(format!(
                "maximum memory must be aligned to the page size ({PAGE} bytes)"
            ));
        }
        if at > o.max_memory {
            return Err(format!("maximum memory too small, {at} bytes needed"));
        }
        if o.max_memory > max_setting {
            return Err(format!(
                "maximum memory too large, cannot be greater than {max_setting}"
            ));
        }
    }
    // Shared memory needs a maximum.
    let max = match o.max_memory {
        0 if shared => at,
        m => m,
    };
    out.max_pages = max / PAGE;
    let optional = l.synth.optional.clone();
    for (id, n) in optional {
        let v = match n {
            "__dso_handle" => data_start,
            "__data_end" => data_end,
            "__stack_low" => stack.0,
            "__stack_high" => stack.1,
            "__global_base" => global_base,
            "__heap_base" => heap_base,
            "__heap_end" => heap_end,
            "__table_base" => 1,
            "__wasm_first_page_end" => PAGE,
            _ => 0,
        };
        l.syms.get_mut(id).def = Def::Absolute(v);
    }
    Ok(())
}

/// With an imported memory that `memory.fill` cannot clear, `.bss` goes
/// into the module.
pub(crate) fn emit_bss(o: &Options, out: &Out) -> bool {
    o.import_memory && !out.features.iter().any(|f| f == "bulk-memory")
}
