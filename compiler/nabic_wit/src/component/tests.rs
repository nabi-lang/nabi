use crate::component::bin::{self, Reader};
use crate::component::core::{FuncType, Module};
use crate::component::types::{Groups, Model, group_key};
use crate::component::{componentize, decode, gc, payload_name};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const I: u8 = 0x7f;

type Sig<'a> = (&'a str, &'a [u8], &'a [u8]);

/// A core module importing functions `imports` (module, name, params,
/// results) and defining `exports` (name, params, results) as
/// `unreachable`, plus `memory`.
fn core_module(imports: &[(&str, &str, &[u8], &[u8])], exports: &[Sig<'_>]) -> Vec<u8> {
    let mut out = b"\0asm\x01\0\0\0".to_vec();
    let mut b = Vec::new();
    let sigs = imports
        .iter()
        .map(|(_, _, p, r)| (p, r))
        .chain(exports.iter().map(|(_, p, r)| (p, r)));
    let mut n = 0;
    for (p, r) in sigs {
        FuncType {
            params: p.to_vec(),
            results: r.to_vec(),
        }
        .encode(&mut b);
        n += 1;
    }
    bin::vec_section(&mut out, 1, n, &b);
    b.clear();
    for (i, (m, f, ..)) in imports.iter().enumerate() {
        bin::name(&mut b, m);
        bin::name(&mut b, f);
        b.push(0);
        bin::usize(&mut b, i);
    }
    bin::vec_section(&mut out, 2, imports.len(), &b);
    b.clear();
    for i in 0..exports.len() {
        bin::usize(&mut b, imports.len() + i);
    }
    bin::vec_section(&mut out, 3, exports.len(), &b);
    bin::vec_section(&mut out, 5, 1, &[0, 1]);
    b.clear();
    bin::name(&mut b, "memory");
    b.extend_from_slice(&[2, 0]);
    for (i, (name, ..)) in exports.iter().enumerate() {
        bin::name(&mut b, name);
        b.push(0);
        bin::usize(&mut b, imports.len() + i);
    }
    bin::vec_section(&mut out, 7, exports.len() + 1, &b);
    b.clear();
    for _ in exports {
        b.extend_from_slice(&[3, 0, 0x00, 0x0b]);
    }
    bin::vec_section(&mut out, 10, exports.len(), &b);
    out
}

/// The top-level sections of a component.
fn sections(c: &[u8]) -> Vec<(u8, &[u8])> {
    assert_eq!(&c[..8], b"\0asm\x0d\0\x01\0");
    let mut r = Reader::new(c);
    r.pos = 8;
    let mut out = Vec::new();
    while !r.eof() {
        let id = r.u8().unwrap();
        let n = r.u32().unwrap() as usize;
        out.push((id, r.bytes(n).unwrap()));
    }
    out
}

/// The names a component imports (`10`) or exports (`11`).
fn extern_names(c: &[u8], id: u8) -> Vec<String> {
    let mut out = Vec::new();
    for (sid, body) in sections(c) {
        if sid != id {
            continue;
        }
        let mut r = Reader::new(body);
        for _ in 0..r.u32().unwrap() {
            r.u8().unwrap();
            out.push(r.name().unwrap().to_owned());
            // Past the rest: only the forms the componentizer writes.
            if id == 10 {
                if r.u8().unwrap() == 0x03 {
                    if r.u8().unwrap() == 0 {
                        r.u32().unwrap();
                    }
                } else {
                    r.u32().unwrap();
                }
            } else {
                r.u8().unwrap();
                r.u32().unwrap();
                if r.u8().unwrap() == 1 {
                    r.u8().unwrap();
                    r.u32().unwrap();
                }
            }
        }
    }
    out
}

fn adapter(kind: &str) -> Option<Vec<u8>> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/lib/nabi/targets/wasm32-wasip2/wasi_snapshot_preview1.{kind}.wasm"
    ));
    std::fs::read(p).ok()
}

/// A WIT directory holding `text` as its `component.wit`.
fn wit_dir(name: &str, text: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("nabic-wit-component-{}-{name}", std::process::id()));
    std::fs::create_dir_all(dir.join("deps")).unwrap();
    std::fs::write(dir.join("component.wit"), text).unwrap();
    dir
}

#[test]
fn leb_round_trip() {
    for v in [0u32, 1, 127, 128, 300, 65535, u32::MAX] {
        let mut b = Vec::new();
        bin::u32(&mut b, v);
        assert_eq!(Reader::new(&b).u32().unwrap(), v);
    }
    for v in [0i64, -1, 63, 64, -64, -65, 1 << 40, i64::MIN, i64::MAX] {
        let mut b = Vec::new();
        bin::s64(&mut b, v);
        assert_eq!(Reader::new(&b).s64().unwrap(), v);
    }
}

#[test]
fn semver_groups() {
    assert_eq!(group_key("wasi:io/poll@0.2.0"), "wasi:io/poll@0.2");
    assert_eq!(group_key("wasi:io/poll@0.2.12"), "wasi:io/poll@0.2");
    assert_eq!(group_key("a:b/c@1.4.2"), "a:b/c@1");
    assert_eq!(group_key("a:b/c@0.0.3"), "a:b/c@0.0.3");
    assert_eq!(group_key("a:b/c@0.3.0-rc-1"), "a:b/c@0.3.0-rc-1");
    assert_eq!(group_key("tools"), "tools");
    let mut m = Model::default();
    for k in [
        "wasi:io/poll@0.2.0",
        "wasi:io/poll@0.2.12",
        "wasi:io/poll@0.2.6",
        "wasi:io/poll@0.3.0",
    ] {
        m.iface(k);
    }
    let g = Groups::new(&m);
    assert_eq!(g.groups.len(), 2);
    let p2 = g.find("wasi:io/poll@0.2.1").unwrap();
    assert_eq!(g.name(p2), "wasi:io/poll@0.2.12");
    assert_ne!(g.find("wasi:io/poll@0.3.0"), Some(p2));
}

#[test]
fn payload_names() {
    assert_eq!(
        payload_name("[stream-read-0]count-to"),
        Some((false, "read", Some(0), "count-to"))
    );
    assert_eq!(
        payload_name("[future-drop-readable-12]f"),
        Some((true, "drop-readable", Some(12), "f"))
    );
    assert_eq!(
        payload_name("[stream-new-unit]"),
        Some((false, "new", None, ""))
    );
    assert_eq!(payload_name("[resource-drop]r"), None);
    assert_eq!(payload_name("plain"), None);
}

#[test]
fn shim_and_fixup_parse() {
    let tys = vec![
        FuncType {
            params: vec![I, I],
            results: vec![I],
        },
        FuncType {
            params: vec![],
            results: vec![],
        },
    ];
    let shim = crate::component::core::shim(&tys);
    let m = Module::parse(&shim).unwrap();
    assert_eq!(m.funcs.len(), 2);
    assert!(m.export("$imports").is_some());
    assert_eq!(m.export_func("1"), Some(1));
    let fix = crate::component::core::fixup(&tys);
    let m = Module::parse(&fix).unwrap();
    assert_eq!(m.imports.len(), 3);
    assert!(Module::parse(&crate::component::core::starter()).is_ok());
}

#[test]
fn reactor_with_wit() {
    let dir = wit_dir(
        "reactor",
        "package test:t;

interface host {
    resource r;
    get: func(x: string) -> list<u8>;
}

world w {
    import host;
    export add: func(a: s32, b: s32) -> s32;
    export api: interface {
        record pt { x: u32, name: string }
        echo: func(p: pt) -> string;
    }
}
",
    );
    let core = core_module(
        &[
            ("test:t/host", "get", &[I, I, I], &[]),
            ("test:t/host", "[resource-drop]r", &[I], &[]),
        ],
        &[
            ("cabi_realloc", &[I, I, I, I], &[I]),
            ("add", &[I, I], &[I]),
            ("api#echo", &[I, I, I], &[I]),
            ("cabi_post_api#echo", &[I], &[]),
            ("_initialize", &[], &[]),
        ],
    );
    let c = componentize(&core, &[], Some(&dir), true).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(extern_names(&c, 10), ["test:t/host"]);
    assert_eq!(extern_names(&c, 11), ["api", "add"]);
    let secs = sections(&c);
    let count = |id: u8| secs.iter().filter(|(i, _)| *i == id).count();
    // Core modules: shim, main, fixup, starter; one nested component.
    assert_eq!(count(1), 4);
    assert_eq!(count(4), 1);
}

#[test]
fn missing_export_is_an_error() {
    let dir = wit_dir(
        "missing",
        "package test:t;\n\nworld w {\n    export add: func(a: s32, b: s32) -> s32;\n}\n",
    );
    let core = core_module(&[], &[("cabi_realloc", &[I, I, I, I], &[I])]);
    let e = componentize(&core, &[], Some(&dir), true).unwrap_err();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(e.contains("does not export `add`"), "{e}");
}

#[test]
fn unknown_import_is_an_error() {
    let core = core_module(&[("x:y/z", "f", &[], &[])], &[]);
    let e = componentize(&core, &[], None, true).unwrap_err();
    assert!(e.contains("x:y/z"), "{e}");
}

#[test]
fn command_needs_start() {
    let core = core_module(&[], &[]);
    let e = componentize(&core, &[], None, false).unwrap_err();
    assert!(e.contains("_start"), "{e}");
}

#[test]
fn adapter_world() {
    let Some(a) = adapter("command") else {
        eprintln!("skipped: no wasm32-wasip2 adapter in target/lib");
        return;
    };
    let md = Module::parse(&a).unwrap();
    let mut m = Model::default();
    let (_, body) = md
        .customs
        .iter()
        .find(|(n, _)| n.starts_with("component-type"))
        .unwrap();
    let w = decode::worlds(&mut m, body).unwrap().remove(0);
    let exports: Vec<&str> = w
        .exports
        .iter()
        .map(|&i| m.ifaces[i].key.as_str())
        .collect();
    assert_eq!(exports.len(), 1);
    assert!(exports[0].starts_with("wasi:cli/run@0.2."), "{exports:?}");
    let g = Groups::new(&m);
    let streams = g.find("wasi:io/streams@0.2.0").unwrap();
    assert!(g.ty(&m, streams, "output-stream").is_some());
    assert!(
        g.func(
            &m,
            streams,
            "[method]output-stream.blocking-write-and-flush"
        )
        .is_some()
    );
}

#[test]
fn adapter_gc() {
    let Some(a) = adapter("command") else {
        eprintln!("skipped: no wasm32-wasip2 adapter in target/lib");
        return;
    };
    let md = Module::parse(&a).unwrap();
    let run = md
        .exports
        .iter()
        .find(|e| e.name.starts_with("wasi:cli/run@"))
        .unwrap()
        .name
        .clone();
    let keep: BTreeSet<String> = ["fd_write", "proc_exit", "cabi_import_realloc", &run]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let out = gc::run(&a, &keep).unwrap();
    let g = Module::parse(&out).unwrap();
    let kept: BTreeSet<String> = g
        .exports
        .iter()
        .filter(|e| e.kind == 0)
        .map(|e| e.name.clone())
        .collect();
    assert_eq!(kept, keep);
    assert!(g.imports.len() < md.imports.len());
    assert!(
        g.imports
            .iter()
            .any(|i| i.module == "__main_module__" && i.name == "cabi_realloc")
    );
    // Each export first allocates the stack: a call to the last function.
    let alloc = g.func_imports() + g.funcs.len() as u32 - 1;
    let code = g.sections.iter().find(|(id, _)| *id == 10).unwrap().1;
    let mut r = Reader::new(code);
    let mut bodies = Vec::new();
    for _ in 0..r.u32().unwrap() {
        let n = r.u32().unwrap() as usize;
        bodies.push(r.bytes(n).unwrap());
    }
    for e in g.exports.iter().filter(|e| e.kind == 0) {
        let body = bodies[(e.index - g.func_imports()) as usize];
        let mut r = Reader::new(body);
        for _ in 0..r.u32().unwrap() {
            r.u32().unwrap();
            r.u8().unwrap();
        }
        assert_eq!(r.u8().unwrap(), 0x10, "{}", e.name);
        assert_eq!(r.u32().unwrap(), alloc, "{}", e.name);
    }
}

#[test]
fn command_with_adapter() {
    let Some(a) = adapter("command") else {
        eprintln!("skipped: no wasm32-wasip2 adapter in target/lib");
        return;
    };
    let core = core_module(
        &[("wasi_snapshot_preview1", "fd_write", &[I, I, I, I], &[I])],
        &[("cabi_realloc", &[I, I, I, I], &[I]), ("_start", &[], &[])],
    );
    let c = componentize(&core, &a, None, false).unwrap();
    let exports = extern_names(&c, 11);
    assert_eq!(exports.len(), 1);
    assert!(exports[0].starts_with("wasi:cli/run@0.2."), "{exports:?}");
    let imports = extern_names(&c, 10);
    assert!(
        imports
            .iter()
            .any(|i| i.starts_with("wasi:io/streams@0.2.")),
        "{imports:?}"
    );
}
