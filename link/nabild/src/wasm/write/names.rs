//! Demangling and the name section.

#[allow(clippy::wildcard_imports)]
use crate::wasm::write::*;

/// A name as the name section shows it: Itanium mangling decoded (Rust's
/// legacy symbols, C++ names with builtin parameter types; others stay
/// mangled), `__main_argc_argv` as `main`.
pub(crate) fn demangle(s: &str) -> std::borrow::Cow<'_, str> {
    if s == "__main_argc_argv" {
        return "main".into();
    }
    itanium(s).map_or_else(|| s.into(), Into::into)
}

pub(crate) fn itanium(s: &str) -> Option<String> {
    let mut rest = s.strip_prefix("_Z")?;
    let source = |rest: &mut &str| -> Option<String> {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let n: usize = rest[..digits].parse().ok()?;
        let id = rest.get(digits..digits + n)?.to_owned();
        *rest = &rest[digits + n..];
        Some(id)
    };
    let mut parts = Vec::new();
    if let Some(r) = rest.strip_prefix('N') {
        rest = r;
        while !rest.starts_with('E') {
            parts.push(source(&mut rest)?);
        }
        rest = &rest[1..];
    } else {
        parts.push(source(&mut rest)?);
    }
    if parts.is_empty() {
        return None;
    }
    let mut out = parts.join("::");
    let (params, suffix) = rest.split_once('.').map_or((rest, ""), |(p, x)| (p, x));
    if !params.is_empty() {
        let mut list = Vec::new();
        let mut p = params.as_bytes();
        while !p.is_empty() {
            let mut quals = String::new();
            let mut ptrs = String::new();
            loop {
                match p.first()? {
                    b'P' => ptrs.insert(0, '*'),
                    b'R' => ptrs.insert(0, '&'),
                    b'K' => quals.push_str(" const"),
                    _ => break,
                }
                p = &p[1..];
            }
            let t = match p.first()? {
                b'v' => "void",
                b'b' => "bool",
                b'c' => "char",
                b'a' => "signed char",
                b'h' => "unsigned char",
                b's' => "short",
                b't' => "unsigned short",
                b'i' => "int",
                b'j' => "unsigned int",
                b'l' => "long",
                b'm' => "unsigned long",
                b'x' => "long long",
                b'y' => "unsigned long long",
                b'f' => "float",
                b'd' => "double",
                b'e' => "long double",
                b'z' => "...",
                _ => return None,
            };
            p = &p[1..];
            list.push(format!("{t}{quals}{ptrs}"));
        }
        if list == ["void"] {
            list.clear();
        }
        out.push_str(&format!("({})", list.join(", ")));
    }
    if !suffix.is_empty() {
        out.push_str(&format!(" (.{suffix})"));
    }
    Some(out)
}

/// The name section: the module, function, global and data segment
/// names.
pub(crate) fn name_section(
    l: &Link,
    o: &Options,
    out: &Out,
    m: &mut Vec<u8>,
    in_binary: impl Fn(&OutSeg) -> bool,
) {
    let mut b = Vec::new();
    name(&mut b, "name");
    let sub = |b: &mut Vec<u8>, id: u8, body: &[u8]| {
        b.push(id);
        uleb(b, body.len() as u64);
        b.extend_from_slice(body);
    };
    let mut s = Vec::new();
    let module = o
        .out
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    name(&mut s, &module);
    sub(&mut b, 0, &s);
    // Functions.
    let mut names: Vec<(u32, String)> = Vec::new();
    for i in out.imports.iter().filter(|i| i.kind == 0) {
        if let Some(&x) = out.import_index.get(&i.sym) {
            names.push((x, demangle(l.name(i.sym)).into_owned()));
        }
    }
    for &a in &out.funcs {
        let Chunk::Func {
            name: n,
            debug_name,
            ..
        } = &l.chunks[a as usize]
        else {
            continue;
        };
        if n.is_empty() {
            continue;
        }
        let shown = debug_name
            .clone()
            .unwrap_or_else(|| demangle(n).into_owned());
        names.push((out.index[a as usize], shown));
    }
    if !names.is_empty() {
        s.clear();
        uleb(&mut s, names.len() as u64);
        for (i, n) in &names {
            uleb(&mut s, u64::from(*i));
            name(&mut s, n);
        }
        sub(&mut b, 1, &s);
    }
    // Globals.
    names.clear();
    for i in out.imports.iter().filter(|i| i.kind == 3) {
        if let Some(&x) = out.import_index.get(&i.sym) {
            names.push((x, demangle(l.name(i.sym)).into_owned()));
        }
    }
    for &a in &out.globals {
        let Chunk::Global { name: n, .. } = &l.chunks[a as usize] else {
            continue;
        };
        if !n.is_empty() {
            names.push((out.index[a as usize], demangle(n).into_owned()));
        }
    }
    for (k, &id) in out.got.iter().enumerate() {
        let what = if l.ext(id).kind == Some(SYM_FUNCTION) {
            "func"
        } else {
            "data"
        };
        let i = out.imported[3] + (out.globals.len() + k) as u32;
        names.push((i, format!("GOT.{what}.internal.{}", demangle(l.name(id)))));
    }
    if !names.is_empty() {
        s.clear();
        uleb(&mut s, names.len() as u64);
        for (i, n) in &names {
            uleb(&mut s, u64::from(*i));
            name(&mut s, n);
        }
        sub(&mut b, 7, &s);
    }
    // Data segments.
    let segs: Vec<&OutSeg> = out
        .segs
        .iter()
        .filter(|s| !s.name.is_empty() && in_binary(s))
        .collect();
    if !segs.is_empty() {
        s.clear();
        uleb(&mut s, segs.len() as u64);
        for g in segs {
            uleb(&mut s, u64::from(g.index));
            name(&mut s, &g.name);
        }
        sub(&mut b, 9, &s);
    }
    section(m, 0, &b);
}
