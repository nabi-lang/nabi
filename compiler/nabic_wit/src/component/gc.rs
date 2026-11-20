//! The preview 1 adapter cut down to what the main module uses: the
//! exports asked for and the functions and imports they reach, function
//! indices renumbered. Its shadow stack, a global left at 0, is
//! allocated on first entry (`allocate_stack`, through the main
//! module's `cabi_realloc`), as `wit-component` does.

use crate::component::bin::{self, Reader, Res};
use crate::component::core::{FUNC, Module};
use std::collections::BTreeSet;

/// `allocation_state` values of the adapter.
const STACK_UNALLOCATED: i64 = 0;
const STACK_ALLOCATING: i64 = 1;
const STACK_ALLOCATED: i64 = 2;
const PAGE: i64 = 65536;

/// `adapter` with only the exports `keep` (each must exist).
pub fn run(adapter: &[u8], keep: &BTreeSet<String>) -> Res<Vec<u8>> {
    let m = Module::parse(adapter)?;
    for k in keep {
        if m.export_func(k).is_none() {
            return Err(format!("the adapter has no function export `{k}`"));
        }
    }
    let nimp = m.func_imports();
    let total = nimp + m.funcs.len() as u32;
    let code = bodies(&m)?;
    if code.len() != m.funcs.len() {
        return Err("adapter: function and code counts differ".to_owned());
    }
    let globals = global_names(&m);
    let global = |n: &str| globals.iter().find(|(_, g)| g == n).map(|(i, _)| *i);
    let realloc = m
        .imports
        .iter()
        .filter(|i| matches!(i.kind, crate::component::core::ImportKind::Func(_)))
        .position(|i| i.module == "__main_module__" && i.name == "cabi_realloc")
        .map(|i| i as u32);
    let lazy = match (
        global("__stack_pointer"),
        global("allocation_state"),
        realloc,
    ) {
        (Some(sp), Some(st), Some(re)) => Some((sp, st, re)),
        _ => None,
    };

    // Liveness from the exports kept, the start function and the table.
    let mut live = vec![false; total as usize];
    let mut work: Vec<u32> = m
        .exports
        .iter()
        .filter(|e| e.kind == FUNC && keep.contains(&e.name))
        .map(|e| e.index)
        .collect();
    work.extend(start(&m)?);
    work.extend(elem_funcs(&m)?);
    if let Some((_, _, re)) = lazy {
        work.push(re);
    }
    while let Some(f) = work.pop() {
        if f >= total || std::mem::replace(&mut live[f as usize], true) {
            continue;
        }
        if f >= nimp {
            let mut sink = Vec::new();
            walk_body(code[(f - nimp) as usize], &mut sink, &mut |g| {
                work.push(g);
                g
            })?;
        }
    }
    let mut map = vec![u32::MAX; total as usize];
    let mut n = 0;
    for (f, l) in live.iter().enumerate() {
        if *l {
            map[f] = n;
            n += 1;
        }
    }
    let alloc = n;
    let exported: BTreeSet<u32> = m
        .exports
        .iter()
        .filter(|e| e.kind == FUNC && keep.contains(&e.name))
        .map(|e| e.index)
        .collect();
    let remap = |f: u32| -> u32 { map.get(f as usize).copied().unwrap_or(u32::MAX) };

    let mut out = m.bytes[..8].to_vec();
    for (id, body) in &m.sections {
        let mut b = Vec::new();
        match id {
            0 => continue,
            2 => {
                let mut s = Reader::new(body);
                let mut kept = 0;
                let mut fi = 0;
                for _ in 0..s.u32()? {
                    let at = s.pos;
                    s.name()?;
                    s.name()?;
                    let k = s.u8()?;
                    let keep = if k == FUNC {
                        s.u32()?;
                        fi += 1;
                        live[fi - 1]
                    } else {
                        skip_import(&mut s, k)?;
                        true
                    };
                    if keep {
                        b.extend_from_slice(s.since(at));
                        kept += 1;
                    }
                }
                bin::vec_section(&mut out, 2, kept, &b);
            }
            3 => {
                let mut kept = 0;
                for (i, t) in m.funcs.iter().enumerate() {
                    if live[nimp as usize + i] {
                        bin::u32(&mut b, *t);
                        kept += 1;
                    }
                }
                if lazy.is_some() {
                    bin::u32(&mut b, empty_type(&m)?);
                    kept += 1;
                }
                bin::vec_section(&mut out, 3, kept, &b);
            }
            7 => {
                let mut kept = 0;
                for e in &m.exports {
                    if e.kind == FUNC && !keep.contains(&e.name) {
                        continue;
                    }
                    bin::name(&mut b, &e.name);
                    b.push(e.kind);
                    bin::u32(
                        &mut b,
                        if e.kind == FUNC {
                            remap(e.index)
                        } else {
                            e.index
                        },
                    );
                    kept += 1;
                }
                bin::vec_section(&mut out, 7, kept, &b);
            }
            8 => {
                let f = Reader::new(body).u32()?;
                bin::u32(&mut b, remap(f));
                bin::section(&mut out, 8, &b);
            }
            9 => {
                let n = Reader::new(body).u32()?;
                if n != 0 {
                    return Err("adapter: element segments are not supported".to_owned());
                }
                bin::section(&mut out, 9, body);
            }
            10 => {
                let mut kept = 0;
                for (i, c) in code.iter().enumerate() {
                    let f = nimp + i as u32;
                    if !live[f as usize] {
                        continue;
                    }
                    let mut nb = Vec::new();
                    let mut s = Reader::new(c);
                    for _ in 0..s.u32()? {
                        s.u32()?;
                        s.u8()?;
                    }
                    nb.extend_from_slice(&c[..s.pos]);
                    if lazy.is_some() && exported.contains(&f) {
                        nb.push(0x10);
                        bin::u32(&mut nb, alloc);
                    }
                    walk_expr(&mut s, &mut nb, &mut |g| remap(g))?;
                    bin::usize(&mut b, nb.len());
                    b.extend_from_slice(&nb);
                    kept += 1;
                }
                if let Some((sp, st, re)) = lazy {
                    let body = allocate_stack(sp, st, remap(re));
                    bin::usize(&mut b, body.len());
                    b.extend_from_slice(&body);
                    kept += 1;
                }
                bin::vec_section(&mut out, 10, kept, &b);
            }
            _ => bin::section(&mut out, *id, body),
        }
    }
    Ok(out)
}

/// `allocate_stack`: one page from `cabi_realloc` for the stack, once.
fn allocate_stack(sp: u32, state: u32, realloc: u32) -> Vec<u8> {
    let mut b = vec![0];
    let i32c = |b: &mut Vec<u8>, v: i64| {
        b.push(0x41);
        bin::s64(b, v);
    };
    b.push(0x23);
    bin::u32(&mut b, state);
    i32c(&mut b, STACK_UNALLOCATED);
    b.extend_from_slice(&[0x46, 0x04, 0x40]);
    i32c(&mut b, STACK_ALLOCATING);
    b.push(0x24);
    bin::u32(&mut b, state);
    for v in [0, 0, 8, PAGE] {
        i32c(&mut b, v);
    }
    b.push(0x10);
    bin::u32(&mut b, realloc);
    i32c(&mut b, PAGE);
    b.push(0x6a);
    b.push(0x24);
    bin::u32(&mut b, sp);
    i32c(&mut b, STACK_ALLOCATED);
    b.push(0x24);
    bin::u32(&mut b, state);
    b.extend_from_slice(&[0x0b, 0x0b]);
    b
}

/// Index of the type `() -> ()` (the adapter has one).
fn empty_type(m: &Module<'_>) -> Res<u32> {
    m.types
        .iter()
        .position(|t| t.params.is_empty() && t.results.is_empty())
        .map(|i| i as u32)
        .ok_or_else(|| "adapter: no `() -> ()` type for `allocate_stack`".to_owned())
}

fn bodies<'a>(m: &Module<'a>) -> Res<Vec<&'a [u8]>> {
    let Some((_, body)) = m.sections.iter().find(|(id, _)| *id == 10) else {
        return Ok(Vec::new());
    };
    let mut s = Reader::new(body);
    let mut out = Vec::new();
    for _ in 0..s.u32()? {
        let n = s.u32()? as usize;
        out.push(s.bytes(n)?);
    }
    Ok(out)
}

fn start(m: &Module<'_>) -> Res<Option<u32>> {
    m.sections
        .iter()
        .find(|(id, _)| *id == 8)
        .map(|(_, b)| Reader::new(b).u32())
        .transpose()
}

fn elem_funcs(m: &Module<'_>) -> Res<Vec<u32>> {
    match m.sections.iter().find(|(id, _)| *id == 9) {
        Some((_, b)) if Reader::new(b).u32()? != 0 => {
            Err("adapter: element segments are not supported".to_owned())
        }
        _ => Ok(Vec::new()),
    }
}

fn skip_import(s: &mut Reader<'_>, k: u8) -> Res<()> {
    match k {
        1 => {
            s.u8()?;
            crate::component::core::skip_limits(s)
        }
        2 => crate::component::core::skip_limits(s),
        3 => {
            s.u8()?;
            s.u8()?;
            Ok(())
        }
        _ => Err(format!("adapter: unsupported import kind {k}")),
    }
}

/// Global names from the `name` section.
fn global_names(m: &Module<'_>) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for (name, body) in &m.customs {
        if *name != "name" {
            continue;
        }
        let mut s = Reader::new(body);
        while !s.eof() {
            let Ok(id) = s.u8() else { break };
            let Ok(n) = s.u32() else { break };
            let Ok(sub) = s.bytes(n as usize) else { break };
            if id != 7 {
                continue;
            }
            let mut r = Reader::new(sub);
            let Ok(count) = r.u32() else { break };
            for _ in 0..count {
                let (Ok(i), Ok(nm)) = (r.u32(), r.name()) else {
                    break;
                };
                out.push((i, nm.to_owned()));
            }
        }
    }
    out
}

/// Copy function body `code` (locals and expression) to `out`, each
/// function index through `f`.
fn walk_body(code: &[u8], out: &mut Vec<u8>, f: &mut dyn FnMut(u32) -> u32) -> Res<()> {
    let mut s = Reader::new(code);
    for _ in 0..s.u32()? {
        s.u32()?;
        s.u8()?;
    }
    out.extend_from_slice(&code[..s.pos]);
    walk_expr(&mut s, out, f)
}

/// Copy the instructions from the cursor to the end of its bytes.
fn walk_expr(s: &mut Reader<'_>, out: &mut Vec<u8>, f: &mut dyn FnMut(u32) -> u32) -> Res<()> {
    while !s.eof() {
        let at = s.pos;
        let op = s.u8()?;
        match op {
            0x10 | 0x12 | 0xd2 => {
                out.push(op);
                let g = s.u32()?;
                bin::u32(out, f(g));
                continue;
            }
            0x00 | 0x01 | 0x05 | 0x0b | 0x0f | 0x1a | 0x1b | 0x45..=0xc4 | 0xd1 | 0xd3 | 0xd5 => {}
            0x02..=0x04 => block_type(s)?,
            0x0c | 0x0d | 0x14 | 0x15 | 0x20..=0x26 | 0x3f | 0x40 | 0xd4 | 0xd6 => {
                s.u32()?;
            }
            0x0e => {
                for _ in 0..=s.u32()? {
                    s.u32()?;
                }
            }
            0x11 | 0x13 => {
                s.u32()?;
                s.u32()?;
            }
            0x1c => {
                let n = s.u32()? as usize;
                s.bytes(n)?;
            }
            0x28..=0x3e => memarg(s)?,
            0x41 => {
                s.s64()?;
            }
            0x42 => {
                s.s64()?;
            }
            0x43 => {
                s.bytes(4)?;
            }
            0x44 => {
                s.bytes(8)?;
            }
            0xd0 => {
                s.s33()?;
            }
            0xfc => match s.u32()? {
                0..=7 => {}
                9 | 11 | 13 | 15..=17 => {
                    s.u32()?;
                }
                8 | 10 | 12 | 14 => {
                    s.u32()?;
                    s.u32()?;
                }
                x => return Err(format!("adapter: unknown instruction 0xfc {x}")),
            },
            0xfd => match s.u32()? {
                0..=11 | 92 | 93 => memarg(s)?,
                12 | 13 => {
                    s.bytes(16)?;
                }
                21..=34 => {
                    s.u8()?;
                }
                84..=91 => {
                    memarg(s)?;
                    s.u8()?;
                }
                _ => {}
            },
            0xfe => match s.u32()? {
                3 => {
                    s.u8()?;
                }
                _ => memarg(s)?,
            },
            _ => return Err(format!("adapter: unknown instruction {op:#04x}")),
        }
        out.extend_from_slice(s.since(at));
    }
    Ok(())
}

fn block_type(s: &mut Reader<'_>) -> Res<()> {
    let b = s.peek()?;
    if b == 0x40 || (0x6f..=0x7f).contains(&b) {
        s.u8()?;
    } else {
        s.s33()?;
    }
    Ok(())
}

fn memarg(s: &mut Reader<'_>) -> Res<()> {
    let align = s.u32()?;
    if align & 0x40 != 0 {
        s.u32()?;
    }
    s.u64()?;
    Ok(())
}
