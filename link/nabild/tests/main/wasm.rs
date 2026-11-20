//! The wasm linker on small hand-written relocatable objects (symbols,
//! relocations, garbage collection, the indirect function table, weak and
//! mismatched functions), then against wasi-libc for the three wasm32
//! targets when the toolchain's `lib/nabi/targets/wasm32-*` are there.
//! Modules run under `NABI_TEST_WASM_RUNNER` (wasmtime) when it is set.

#![allow(clippy::many_single_char_names)]

use nabic_core::leb::uleb;
use nabic_target::TargetSpec;
use nabild::wasm;
use std::path::{Path, PathBuf};
use std::process::Command;

fn scratch(name: &str) -> PathBuf {
    nabic_core::tmp::test_dir(&format!("link-wasm-{name}"))
}

fn read_uleb(b: &[u8], at: &mut usize) -> u64 {
    nabic_core::leb::read_uleb(b, at).expect("a whole LEB128")
}

fn name(b: &mut Vec<u8>, s: &str) {
    uleb(b, s.len() as u64);
    b.extend_from_slice(s.as_bytes());
}

fn section(m: &mut Vec<u8>, id: u8, body: &[u8]) {
    m.push(id);
    uleb(m, body.len() as u64);
    m.extend_from_slice(body);
}

const I32: u8 = 0x7f;
const UNDEFINED: u32 = 0x10;
const WEAK: u32 = 0x1;
const EXPORTED: u32 = 0x20;
const NO_STRIP: u32 = 0x80;

const R_FUNCTION_INDEX_LEB: u8 = 0;
const R_TABLE_INDEX_I32: u8 = 2;
const R_MEMORY_ADDR_SLEB: u8 = 4;

/// A symbol table entry.
enum Sym {
    Func {
        flags: u32,
        index: u32,
        name: &'static str,
    },
    Data {
        flags: u32,
        name: &'static str,
        seg: u32,
        off: u32,
        size: u32,
    },
    Table {
        flags: u32,
        index: u32,
    },
}

/// A relocatable object: function types, function imports (module,
/// field, type), an optional table import, functions (type, body
/// without its size), data segments (name, bytes), symbols, and
/// relocations (type, function or segment, offset in its body or bytes,
/// symbol, addend).
#[derive(Default)]
struct Obj {
    types: Vec<(Vec<u8>, Vec<u8>)>,
    imports: Vec<(&'static str, &'static str, u32)>,
    table: bool,
    funcs: Vec<(u32, Vec<u8>)>,
    data: Vec<(&'static str, Vec<u8>)>,
    syms: Vec<Sym>,
    code_relocs: Vec<(u8, usize, usize, u32, i32)>,
    data_relocs: Vec<(u8, usize, usize, u32, i32)>,
}

impl Obj {
    fn bytes(&self) -> Vec<u8> {
        let mut m = b"\0asm\x01\0\0\0".to_vec();
        let mut b = Vec::new();
        uleb(&mut b, self.types.len() as u64);
        for (p, r) in &self.types {
            b.push(0x60);
            uleb(&mut b, p.len() as u64);
            b.extend_from_slice(p);
            uleb(&mut b, r.len() as u64);
            b.extend_from_slice(r);
        }
        section(&mut m, 1, &b);
        b.clear();
        uleb(
            &mut b,
            (self.imports.len() + 1 + usize::from(self.table)) as u64,
        );
        name(&mut b, "env");
        name(&mut b, "__linear_memory");
        b.extend([2, 0, 0]);
        for (module, field, t) in &self.imports {
            name(&mut b, module);
            name(&mut b, field);
            b.push(0);
            uleb(&mut b, u64::from(*t));
        }
        if self.table {
            name(&mut b, "env");
            name(&mut b, "__indirect_function_table");
            b.extend([1, 0x70, 0, 0]);
        }
        section(&mut m, 2, &b);
        b.clear();
        uleb(&mut b, self.funcs.len() as u64);
        for (t, _) in &self.funcs {
            uleb(&mut b, u64::from(*t));
        }
        section(&mut m, 3, &b);
        // Sections so far: type, import, function; code and data next.
        let mut nsect = 3;
        let mut code_at = Vec::new();
        if !self.funcs.is_empty() {
            b.clear();
            uleb(&mut b, self.funcs.len() as u64);
            for (_, body) in &self.funcs {
                uleb(&mut b, body.len() as u64);
                code_at.push(b.len());
                b.extend_from_slice(body);
            }
            section(&mut m, 10, &b);
            nsect += 1;
        }
        let code_sect = nsect - 1;
        let mut data_at = Vec::new();
        if !self.data.is_empty() {
            b.clear();
            uleb(&mut b, self.data.len() as u64);
            let mut addr = 0;
            for (_, bytes) in &self.data {
                b.extend([0, 0x41]);
                uleb(&mut b, addr);
                b.push(0x0b);
                uleb(&mut b, bytes.len() as u64);
                data_at.push(b.len());
                b.extend_from_slice(bytes);
                addr += bytes.len() as u64;
            }
            section(&mut m, 11, &b);
            nsect += 1;
        }
        let data_sect = nsect - 1;
        b.clear();
        name(&mut b, "linking");
        uleb(&mut b, 2);
        let mut sub = Vec::new();
        uleb(&mut sub, self.syms.len() as u64);
        for s in &self.syms {
            match s {
                Sym::Func {
                    flags,
                    index,
                    name: n,
                } => {
                    sub.push(0);
                    uleb(&mut sub, u64::from(*flags));
                    uleb(&mut sub, u64::from(*index));
                    if flags & UNDEFINED == 0 {
                        name(&mut sub, n);
                    }
                }
                Sym::Data {
                    flags,
                    name: n,
                    seg,
                    off,
                    size,
                } => {
                    sub.push(1);
                    uleb(&mut sub, u64::from(*flags));
                    name(&mut sub, n);
                    if flags & UNDEFINED == 0 {
                        for v in [seg, off, size] {
                            uleb(&mut sub, u64::from(*v));
                        }
                    }
                }
                Sym::Table { flags, index } => {
                    sub.push(5);
                    uleb(&mut sub, u64::from(*flags));
                    uleb(&mut sub, u64::from(*index));
                }
            }
        }
        b.push(8);
        uleb(&mut b, sub.len() as u64);
        b.extend_from_slice(&sub);
        if !self.data.is_empty() {
            sub.clear();
            uleb(&mut sub, self.data.len() as u64);
            for (n, _) in &self.data {
                name(&mut sub, n);
                sub.extend([2, 0]);
            }
            b.push(5);
            uleb(&mut b, sub.len() as u64);
            b.extend_from_slice(&sub);
        }
        section(&mut m, 0, &b);
        for (relocs, at, sect, label) in [
            (&self.code_relocs, &code_at, code_sect, "reloc.CODE"),
            (&self.data_relocs, &data_at, data_sect, "reloc.DATA"),
        ] {
            if relocs.is_empty() {
                continue;
            }
            b.clear();
            name(&mut b, label);
            uleb(&mut b, sect as u64);
            uleb(&mut b, relocs.len() as u64);
            for &(ty, k, off, sym, addend) in relocs {
                b.push(ty);
                uleb(&mut b, (at[k] + off) as u64);
                uleb(&mut b, u64::from(sym));
                if ty == R_MEMORY_ADDR_SLEB {
                    let mut v = i64::from(addend);
                    loop {
                        let x = (v & 0x7f) as u8;
                        v >>= 7;
                        let done = (v == 0 && x & 0x40 == 0) || (v == -1 && x & 0x40 != 0);
                        b.push(if done { x } else { x | 0x80 });
                        if done {
                            break;
                        }
                    }
                }
            }
            section(&mut m, 0, &b);
        }
        m
    }
}

/// A module's sections: (id, custom name, payload).
fn sections(m: &[u8]) -> Vec<(u8, String, Vec<u8>)> {
    assert!(m.starts_with(b"\0asm\x01\0\0\0"), "not a wasm module");
    let mut out = Vec::new();
    let mut at = 8;
    while at < m.len() {
        let id = m[at];
        at += 1;
        let n = read_uleb(m, &mut at) as usize;
        let mut body = &m[at..at + n];
        let mut label = String::new();
        if id == 0 {
            let mut k = 0;
            let len = read_uleb(body, &mut k) as usize;
            label = String::from_utf8(body[k..k + len].to_vec()).unwrap();
            body = &body[k + len..];
        }
        out.push((id, label, body.to_vec()));
        at += n;
    }
    out
}

fn payload(m: &[u8], id: u8) -> Vec<u8> {
    sections(m)
        .into_iter()
        .find(|s| s.0 == id)
        .map(|s| s.2)
        .unwrap_or_default()
}

/// (name, kind, index) of each export.
fn exports(m: &[u8]) -> Vec<(String, u8, u64)> {
    let b = payload(m, 7);
    let mut at = 0;
    let n = read_uleb(&b, &mut at);
    (0..n)
        .map(|_| {
            let len = read_uleb(&b, &mut at) as usize;
            let s = String::from_utf8(b[at..at + len].to_vec()).unwrap();
            at += len;
            let k = b[at];
            at += 1;
            (s, k, read_uleb(&b, &mut at))
        })
        .collect()
}

/// (module, field, kind) of each import.
fn imports(m: &[u8]) -> Vec<(String, String, u8)> {
    let b = payload(m, 2);
    let mut at = 0;
    let n = read_uleb(&b, &mut at);
    let str_at = |at: &mut usize| {
        let len = read_uleb(&b, at) as usize;
        let s = String::from_utf8(b[*at..*at + len].to_vec()).unwrap();
        *at += len;
        s
    };
    let mut v = Vec::new();
    for _ in 0..n {
        let module = str_at(&mut at);
        let field = str_at(&mut at);
        let kind = b[at];
        at += 1;
        match kind {
            0 => {
                read_uleb(&b, &mut at);
            }
            2 => {
                let flags = b[at];
                at += 1;
                read_uleb(&b, &mut at);
                if flags & 1 != 0 {
                    read_uleb(&b, &mut at);
                }
            }
            3 => at += 2,
            k => panic!("import kind {k}"),
        }
        v.push((module, field, kind));
    }
    v
}

/// The function names of the name section.
fn func_names(m: &[u8]) -> Vec<String> {
    let Some((_, _, b)) = sections(m).into_iter().find(|s| s.1 == "name") else {
        return Vec::new();
    };
    let mut at = 0;
    let mut v = Vec::new();
    while at < b.len() {
        let id = b[at];
        at += 1;
        let n = read_uleb(&b, &mut at) as usize;
        let end = at + n;
        if id == 1 {
            let count = read_uleb(&b, &mut at);
            for _ in 0..count {
                read_uleb(&b, &mut at);
                let len = read_uleb(&b, &mut at) as usize;
                v.push(String::from_utf8(b[at..at + len].to_vec()).unwrap());
                at += len;
            }
        }
        at = end;
    }
    v
}

/// `wasmtime run --invoke f` of a core module: exit status and stdout.
fn invoke(module: &Path, f: &str) -> Option<(bool, String)> {
    let runner = std::env::var_os("NABI_TEST_WASM_RUNNER")?;
    let o = Command::new(runner)
        .args(["run", "-C", "cache=n", "--invoke", f])
        .arg(module)
        .output()
        .expect("cannot run NABI_TEST_WASM_RUNNER");
    Some((
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).trim().to_owned(),
    ))
}

fn link(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut a: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    for s in &mut a {
        if !s.starts_with('-') && !s.contains('/') {
            *s = dir.join(&*s).display().to_string();
        }
    }
    let o = wasm::parse_args(&a)?;
    wasm::link(&o)
}

/// `run` calls `f` of another object and adds the word at `d`; `p` holds
/// `f`'s address. `dead` is never referenced.
fn two_objects(dir: &Path) {
    let ty = (Vec::new(), vec![I32]);
    let mut run = vec![0, 0x10, 0x80, 0x80, 0x80, 0x80, 0];
    run.extend([0x41, 0x80, 0x80, 0x80, 0x80, 0, 0x28, 2, 0, 0x6a]);
    run.extend([0x41, 0x80, 0x80, 0x80, 0x80, 0, 0x28, 2, 0, 0x1a, 0x0b]);
    let a = Obj {
        types: vec![ty.clone()],
        imports: vec![("env", "f", 0)],
        table: true,
        funcs: vec![(0, run)],
        data: vec![(".data.d", vec![5, 0, 0, 0]), (".data.p", vec![0; 4])],
        syms: vec![
            Sym::Func {
                flags: EXPORTED,
                index: 1,
                name: "run",
            },
            Sym::Func {
                flags: UNDEFINED,
                index: 0,
                name: "",
            },
            Sym::Data {
                flags: 0,
                name: "d",
                seg: 0,
                off: 0,
                size: 4,
            },
            Sym::Table {
                flags: UNDEFINED | NO_STRIP,
                index: 0,
            },
            Sym::Data {
                flags: 0,
                name: "p",
                seg: 1,
                off: 0,
                size: 4,
            },
        ],
        code_relocs: vec![
            (R_FUNCTION_INDEX_LEB, 0, 2, 1, 0),
            (R_MEMORY_ADDR_SLEB, 0, 8, 2, 0),
            (R_MEMORY_ADDR_SLEB, 0, 18, 4, 0),
        ],
        data_relocs: vec![(R_TABLE_INDEX_I32, 1, 0, 1, 0)],
    };
    let b = Obj {
        types: vec![ty],
        funcs: vec![(0, vec![0, 0x41, 37, 0x0b]), (0, vec![0, 0x41, 1, 0x0b])],
        syms: vec![
            Sym::Func {
                flags: 0,
                index: 0,
                name: "f",
            },
            Sym::Func {
                flags: 0,
                index: 1,
                name: "dead",
            },
        ],
        ..Obj::default()
    };
    std::fs::write(dir.join("a.o"), a.bytes()).unwrap();
    std::fs::write(dir.join("b.o"), b.bytes()).unwrap();
}

#[test]
fn links_relocates_and_collects() {
    let dir = scratch("two");
    two_objects(&dir);
    let out = dir.join("two.wasm");
    let o = out.display().to_string();
    link(&dir, &["--no-entry", "-o", &o, "a.o", "b.o"]).unwrap();
    let m = std::fs::read(&out).unwrap();
    // `run` first (file order), then `f`; `dead` collected.
    assert_eq!(
        exports(&m),
        [("memory".to_owned(), 2, 0), ("run".to_owned(), 0, 0)]
    );
    assert_eq!(func_names(&m), ["run", "f"]);
    let code = payload(&m, 10);
    // The call to `f` (function 1) and the address of `d`, right after
    // the 64 KiB stack, padded.
    assert_eq!(&code[3..9], &[0x10, 0x81, 0x80, 0x80, 0x80, 0]);
    assert_eq!(&code[9..15], &[0x41, 0x80, 0x80, 0x84, 0x80, 0]);
    // `p` holds table slot 1, `f`'s; the table holds slot 0 (null) and it.
    let data = payload(&m, 11);
    assert_eq!(&data[data.len() - 8..], &[5, 0, 0, 0, 1, 0, 0, 0]);
    assert_eq!(payload(&m, 4), [1, 0x70, 1, 2, 2]);
    assert_eq!(payload(&m, 9), [1, 0, 0x41, 1, 0x0b, 1, 1]);
    if let Some((ok, text)) = invoke(&out, "run") {
        assert!(ok && text == "42", "run printed {text:?}");
    }
}

#[test]
fn undefined_symbols_are_errors() {
    let dir = scratch("undef");
    two_objects(&dir);
    let o = dir.join("x.wasm").display().to_string();
    let e = link(&dir, &["--no-entry", "-o", &o, "a.o"]).unwrap_err();
    assert!(e.ends_with("a.o: undefined symbol: f"), "{e}");
    let e = link(&dir, &["-o", &o, "a.o", "b.o"]).unwrap_err();
    assert_eq!(
        e,
        "entry symbol not defined (pass --no-entry to suppress): _start"
    );
    let e = link(
        &dir,
        &["--no-entry", "--export=nope", "-o", &o, "a.o", "b.o"],
    )
    .unwrap_err();
    assert_eq!(e, "symbol exported via --export not found: nope");
    let e = link(&dir, &["--no-entry", "-o", &o, "a.o", "b.o", "b.o"]).unwrap_err();
    assert!(e.starts_with("duplicate symbol: f\n>>> defined in "), "{e}");
}

/// A weak undefined function and a call whose signature differs from
/// the definition's both become traps, the second with a warning.
#[test]
fn weak_and_mismatched_calls_trap() {
    let dir = scratch("traps");
    let i32_ = (Vec::new(), vec![I32]);
    let takes = (vec![I32], Vec::new());
    let body = vec![
        0, 0x10, 0x80, 0x80, 0x80, 0x80, 0, 0x10, 0x80, 0x80, 0x80, 0x80, 0, 0x6a, 0x0b,
    ];
    let c = Obj {
        types: vec![i32_],
        imports: vec![("env", "w", 0), ("env", "g", 0)],
        funcs: vec![(0, body)],
        syms: vec![
            Sym::Func {
                flags: EXPORTED,
                index: 2,
                name: "both",
            },
            Sym::Func {
                flags: UNDEFINED | WEAK,
                index: 0,
                name: "",
            },
            Sym::Func {
                flags: UNDEFINED,
                index: 1,
                name: "",
            },
        ],
        code_relocs: vec![
            (R_FUNCTION_INDEX_LEB, 0, 2, 1, 0),
            (R_FUNCTION_INDEX_LEB, 0, 8, 2, 0),
        ],
        ..Obj::default()
    };
    let d = Obj {
        types: vec![takes],
        funcs: vec![(0, vec![0, 0x0b])],
        syms: vec![Sym::Func {
            flags: 0,
            index: 0,
            name: "g",
        }],
        ..Obj::default()
    };
    std::fs::write(dir.join("c.o"), c.bytes()).unwrap();
    std::fs::write(dir.join("d.o"), d.bytes()).unwrap();
    let out = dir.join("traps.wasm");
    let o = out.display().to_string();
    let warnings = link(&dir, &["--no-entry", "-o", &o, "c.o", "d.o"]).unwrap();
    assert!(
        warnings
            .starts_with("warning: function signature mismatch: g\n>>> defined as () -> i32 in "),
        "{warnings}"
    );
    let m = std::fs::read(&out).unwrap();
    assert_eq!(
        func_names(&m),
        ["signature_mismatch:g", "undefined_weak:w", "both"]
    );
    if let Some((ok, _)) = invoke(&out, "both") {
        assert!(!ok, "a trap stub returned");
    }
}

#[test]
fn rejects_unknown_arguments() {
    let e = wasm::parse_args(&["--frobnicate"]).unwrap_err();
    assert_eq!(e, "unknown argument: --frobnicate");
    let e = wasm::parse_args(&["-z", "norelro"]).unwrap_err();
    assert_eq!(e, "-z norelro: unsupported option");
    assert!(wasm::parse_args(&["-z", "stack-size=100"]).is_err());
    let o = wasm::parse_args(&[
        "--stack-first",
        "-z",
        "stack-size=8388608",
        "--max-memory=4294901760",
        "--shared-memory",
        "--import-memory",
        "--export-memory",
        "--export=wasi_thread_start",
        "--export-if-defined",
        "fflush",
        "--undefined",
        "u",
        "--no-entry",
        "-L/lib",
        "-Bstatic",
        "-lc",
        "-Bdynamic",
        "x.o",
    ])
    .unwrap();
    assert_eq!(o.stack_size, 8 << 20);
    assert_eq!(o.max_memory, 4294901760);
    assert!(o.shared_memory && o.import_memory && o.export_memory && o.entry.is_none());
    assert_eq!(o.exports, ["wasi_thread_start"]);
    assert_eq!(
        (
            o.exports_if_defined.len(),
            o.undefined.len(),
            o.inputs.len()
        ),
        (1, 1, 2)
    );
}

/// The toolchain's wasi-libc for `triple`, if there.
fn libc_dir(triple: &str) -> Option<PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    [
        std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from),
        Some(manifest),
    ]
    .into_iter()
    .flatten()
    .map(|t| t.join("lib/nabi/targets").join(triple))
    .find(|d| d.join("libc.a").is_file())
}

/// An object defining `__main_void` (returning `code`), or exported
/// `answer` returning it.
fn main_object(code: u8, export: bool) -> Vec<u8> {
    let (flags, n) = if export {
        (EXPORTED, "answer")
    } else {
        (0, "__main_void")
    };
    Obj {
        types: vec![(Vec::new(), vec![I32])],
        funcs: vec![(0, vec![0, 0x41, code, 0x0b])],
        syms: vec![Sym::Func {
            flags,
            index: 0,
            name: n,
        }],
        ..Obj::default()
    }
    .bytes()
}

#[test]
fn wasi_command_exits_with_main() {
    let triple = "wasm32-wasip1-threads";
    let Some(libc) = libc_dir(triple) else {
        eprintln!("no {triple} C library (targets/build.sh {triple}): skipped");
        return;
    };
    let dir = scratch("command");
    let obj = dir.join("main.o");
    std::fs::write(&obj, main_object(7, false)).unwrap();
    let out = dir.join("main.wasm");
    let t = TargetSpec::builtin(triple).unwrap();
    assert!(nabild::links(&t));
    nabild::link_executable(&t, &[&obj], &out, true, Some(&libc)).unwrap();
    let m = std::fs::read(&out).unwrap();
    let ex: Vec<String> = exports(&m).into_iter().map(|e| e.0).collect();
    assert_eq!(ex, ["memory", "_start", "wasi_thread_start"]);
    let imp = imports(&m);
    assert_eq!(imp[0], ("env".to_owned(), "memory".to_owned(), 2));
    assert!(
        imp[1..]
            .iter()
            .all(|i| i.0 == "wasi_snapshot_preview1" || i.0 == "wasi")
    );
    // Passive segments, set up by `__wasm_init_memory` at start.
    assert!(!payload(&m, 8).is_empty());
    if let Some(runner) = std::env::var_os("NABI_TEST_WASM_RUNNER") {
        let s = Command::new(runner)
            .args(["run", "-W", "threads=y,shared-memory=y", "-S", "threads=y"])
            .arg(&out)
            .status()
            .unwrap();
        assert_eq!(s.code(), Some(7));
    }
}

#[test]
fn browser_reactor_exports() {
    let triple = "wasm32-unknown-unknown";
    let Some(libc) = libc_dir(triple) else {
        eprintln!("no {triple} C library (targets/build.sh {triple}): skipped");
        return;
    };
    let dir = scratch("reactor");
    let obj = dir.join("answer.o");
    std::fs::write(&obj, main_object(42, true)).unwrap();
    let out = dir.join("answer.wasm");
    let t = TargetSpec::builtin(triple).unwrap();
    nabild::link_executable(&t, &[&obj], &out, true, Some(&libc)).unwrap();
    let m = std::fs::read(&out).unwrap();
    let ex: Vec<String> = exports(&m).into_iter().map(|e| e.0).collect();
    assert!(ex.contains(&"_initialize".to_owned()) && ex.contains(&"answer".to_owned()));
    assert!(!ex.contains(&"_start".to_owned()));
    if let Some((ok, text)) = invoke(&out, "answer") {
        assert!(ok && text == "42", "answer printed {text:?}");
    }
}

#[test]
fn wasip2_keeps_component_type_sections() {
    let triple = "wasm32-wasip2";
    let Some(libc) = libc_dir(triple) else {
        eprintln!("no {triple} C library (targets/build.sh {triple}): skipped");
        return;
    };
    let dir = scratch("wasip2");
    // `puts`: stdio reaches the preview 2 bindings, which carry the
    // component type.
    let body = vec![
        0, 0x41, 0x80, 0x80, 0x80, 0x80, 0, 0x10, 0x80, 0x80, 0x80, 0x80, 0, 0x1a, 0x41, 0, 0x0b,
    ];
    let hello = Obj {
        types: vec![(Vec::new(), vec![I32]), (vec![I32], vec![I32])],
        imports: vec![("env", "puts", 1)],
        funcs: vec![(0, body)],
        data: vec![(".rodata.s", b"hi\0".to_vec())],
        syms: vec![
            Sym::Func {
                flags: 0,
                index: 1,
                name: "__main_void",
            },
            Sym::Func {
                flags: UNDEFINED,
                index: 0,
                name: "",
            },
            Sym::Data {
                flags: 0,
                name: "s",
                seg: 0,
                off: 0,
                size: 3,
            },
        ],
        code_relocs: vec![
            (R_MEMORY_ADDR_SLEB, 0, 2, 2, 0),
            (R_FUNCTION_INDEX_LEB, 0, 8, 1, 0),
        ],
        ..Obj::default()
    };
    let obj = dir.join("main.o");
    std::fs::write(&obj, hello.bytes()).unwrap();
    let out = dir.join("core.wasm");
    // The driver drops `--component-type`; the core module is the same.
    let t = TargetSpec::builtin(triple).unwrap();
    nabild::link_executable(&t, &[&obj], &out, true, Some(&libc)).unwrap();
    let m = std::fs::read(&out).unwrap();
    let customs: Vec<String> = sections(&m).into_iter().map(|s| s.1).collect();
    assert!(
        customs.iter().any(|c| c.starts_with("component-type:")),
        "{customs:?}"
    );
    assert!(imports(&m).iter().any(|i| i.0.starts_with("wasi:")));
}
