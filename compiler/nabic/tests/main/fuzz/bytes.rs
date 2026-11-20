//! Seeded mutation fuzzing of the binary and text decoders, in process:
//!
//! - `nabihir`: a library interface decodes or is rejected, never
//!   panics; what decodes encodes back to the bytes it decodes from.
//! - `nabion`: NABION text parses or is rejected; what parses prints to
//!   text that parses to the same value.
//! - `object`: every object, archive, shared object and import library
//!   reader of the linker (ELF, Mach-O, COFF, wasm) rejects or reads.

use crate::common::{NABIC, Rng, scratch};
use crate::fuzz::{PANIC_AT, budget, input_rng, mask, seed};
use nabic_driver::nabihir::Nabihir;
use nabild::archive::{self, Kind};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Instant;

/// Bytes that make readers take odd paths: lengths and offsets at their
/// limits.
const INTERESTING: &[&[u8]] = &[
    &[0],
    &[0xff],
    &[0x7f],
    &[0x80],
    &[0xff, 0xff, 0xff, 0xff],
    &[0xff, 0xff, 0xff, 0x7f],
    &[0, 0, 0, 0x80],
    &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
    &[0x10, 0, 0, 0],
];

/// A mutation of one of `seeds`.
pub fn mutate(rng: &mut Rng, seeds: &[Vec<u8>]) -> Vec<u8> {
    let mut b = rng.pick(seeds).clone();
    for _ in 0..=rng.below(8) {
        if b.is_empty() {
            b.push(0);
        }
        let at = rng.below(b.len());
        match rng.below(7) {
            0 => b[at] ^= 1 << rng.below(8),
            1 => b[at] = rng.next() as u8,
            2 => {
                let v = *rng.pick(INTERESTING);
                let end = (at + v.len()).min(b.len());
                b[at..end].copy_from_slice(&v[..end - at]);
            }
            3 => b.truncate(at),
            4 => {
                let n = rng.below(64).min(b.len() - at);
                b.drain(at..at + n);
            }
            5 => {
                let other = rng.pick(seeds);
                if !other.is_empty() {
                    let from = rng.below(other.len());
                    let n = rng.below(256).min(other.len() - from);
                    b.splice(at..at, other[from..from + n].iter().copied());
                }
            }
            _ => {
                let n = 1 + rng.below(16);
                b.splice(at..at, std::iter::repeat_n(rng.next() as u8, n));
            }
        }
    }
    b
}

/// An input slower than this is a hang.
const HANG: std::time::Duration = std::time::Duration::from_secs(10);

/// [`guarded`] on a thread of its own: `Err("timeout")` past [`HANG`]
/// (the thread is left to spin; the run reports it).
fn watched(test: Test, data: &[u8]) -> Result<(), String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let data = data.to_vec();
    thread::spawn(move || {
        let _ = tx.send(guarded(test, &data));
    });
    rx.recv_timeout(HANG)
        .unwrap_or_else(|_| Err("timeout".into()))
}

/// `test` on `data` with panics caught: `Err(signature)`.
fn guarded(test: Test, data: &[u8]) -> Result<(), String> {
    match std::panic::catch_unwind(|| test(data)) {
        Ok(r) => r,
        Err(p) => {
            let file = PANIC_AT
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .unwrap_or_default();
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default();
            Err(format!("panic {file}: {}", mask(&msg)))
        }
    }
}

pub fn nabihir(data: &[u8]) -> Result<(), String> {
    let Ok(h) = Nabihir::decode(data) else {
        return Ok(());
    };
    let again = Nabihir::decode(&h.encode()).map_err(|e| format!("re-decode: {}", mask(&e)))?;
    if again.encode() != h.encode() {
        return Err("encode / decode round trip differs".into());
    }
    Ok(())
}

pub fn nabion(data: &[u8]) -> Result<(), String> {
    let Ok(text) = std::str::from_utf8(data) else {
        return Ok(());
    };
    let Ok(v) = nabic_nabion::parse(text) else {
        return Ok(());
    };
    let printed = nabic_nabion::to_string(&v);
    match nabic_nabion::parse(&printed) {
        Ok(w) if w == v => Ok(()),
        Ok(_) => Err("printed value parses to another value".into()),
        Err(_) => Err("printed value does not parse".into()),
    }
}

// A test's shape, as the others.
#[allow(clippy::unnecessary_wraps)]
pub fn object(data: &[u8]) -> Result<(), String> {
    let _ = nabild::read_input(data);
    Ok(())
}

/// A byte target: `Err(signature)` for a failure.
pub type Test = fn(&[u8]) -> Result<(), String>;
type Seeds = fn() -> Vec<Vec<u8>>;

/// A target's function and seeds.
pub fn target(name: &str) -> Option<(Test, Seeds)> {
    Some(match name {
        "nabihir" => (nabihir as Test, nabihir_seeds as Seeds),
        "nabion" => (nabion, nabion_seeds),
        "object" => (object, object_seeds),
        _ => return None,
    })
}

fn nabihir_seeds() -> Vec<Vec<u8>> {
    let h = Nabihir {
        producer: "nabic 0.1.0 (fuzz)".into(),
        name: "geom".into(),
        hash: "0123abcd".into(),
        src_dir: PathBuf::from("/nabi/geom/src"),
        type_offset: 801,
        type_count: 3,
        features: vec!["simd".into()],
        deps: vec!["core".into()],
        files: vec![(
            "lib.nabi".into(),
            "pub fn area(w: f64, h: f64) -> f64;\npub struct P { x: i64 }\n".into(),
        )],
        summaries: vec![("area".into(), "keeps none".into())],
        facts: Vec::new(),
    };
    vec![h.encode(), Nabihir::default().encode()]
}

fn nabion_seeds() -> Vec<Vec<u8>> {
    let mut out = vec![
        b"name = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nlinalg = \"0.2\"\n".to_vec(),
        b"{ a: [1, 2.5, -3e9, true, none], b: { c: \"x\\n\\u{41}\" } }\n".to_vec(),
    ];
    let o = Command::new(NABIC)
        .args([
            "--print",
            "target-spec",
            "--target=x86_64-unknown-linux-gnu",
        ])
        .output();
    if let Some(o) = o.ok().filter(|o| o.status.success()) {
        out.push(o.stdout);
    }
    out
}

/// Objects nabic writes for each format (`emit --emit=obj` needs no
/// target runtime), and archives of them.
fn object_seeds() -> Vec<Vec<u8>> {
    let dir = scratch("fuzz-objects");
    let src = dir.join("o.nabi");
    std::fs::write(
        &src,
        "fn add(a: i64, b: i64) -> i64 { a + b }\n\
         fn pick(xs: List<i64>, i: usize) -> i64 { xs[i] }\n\
         static N: i64 = 7;\n\
         fn main() { println(f\"{add(N, 2)} {pick([1, 2], 1)}\") }\n",
    )
    .unwrap();
    let mut out = Vec::new();
    for (t, kind) in [
        ("x86_64-unknown-linux-gnu", Kind::Gnu),
        ("aarch64-apple-darwin", Kind::Darwin),
        ("x86_64-pc-windows-msvc", Kind::Coff),
        ("riscv64gc-unknown-linux-gnu", Kind::Gnu),
        ("wasm32-wasip2", Kind::Gnu),
    ] {
        let obj = dir.join(format!("{t}.o"));
        let _ = Command::new(NABIC)
            .env(nabic_target::IN_PROGRESS_VAR, "1")
            .args(["-q", "emit", "--emit=obj"])
            .arg(&src)
            .arg(format!("--target={t}"))
            .arg("--target-dir")
            .arg(dir.join("target"))
            .arg("-o")
            .arg(&obj)
            .status();
        let Ok(b) = std::fs::read(&obj) else {
            println!("object seeds: no {}", obj.display());
            continue;
        };
        out.push(archive::write(kind, &[("o.o".to_owned(), b.clone())]));
        out.push(b);
    }
    out
}

/// Crashes found, by signature: `(input index, bytes)`.
pub type Found = Mutex<BTreeMap<String, (usize, Vec<u8>)>>;

/// Inputs `0..` on all cores until the budget is spent.
pub fn drive(target: &str, test: Test, seeds: &[Vec<u8>]) -> Found {
    assert!(!seeds.is_empty(), "{target}: no seeds");
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let found: Found = Mutex::new(BTreeMap::new());
    let deadline = Instant::now() + budget();
    let workers = thread::available_parallelism().map_or(4, std::num::NonZero::get);
    thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    if Instant::now() > deadline {
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let data = mutate(&mut input_rng(i), seeds);
                    if let Err(sig) = watched(test, &data) {
                        found
                            .lock()
                            .unwrap()
                            .entry(sig)
                            .and_modify(|e| {
                                if i < e.0 {
                                    *e = (i, data.clone());
                                }
                            })
                            .or_insert((i, data));
                    }
                }
            });
        }
    });
    println!(
        "{target}: {} inputs in {:.1}s (seed {:#x}), {} crash signature(s)",
        next.load(Ordering::Relaxed),
        budget().as_secs_f64(),
        seed(),
        found.lock().unwrap().len()
    );
    found
}

/// Shrink `data` while it fails with `sig`: chunks removed, halving.
pub fn minimize(test: Test, data: &[u8], sig: &str) -> Vec<u8> {
    let mut cur = data.to_vec();
    let mut chunk = cur.len().div_ceil(2).max(1);
    loop {
        let mut i = 0;
        while i < cur.len() {
            let end = (i + chunk).min(cur.len());
            let mut cand = cur[..i].to_vec();
            cand.extend_from_slice(&cur[end..]);
            if watched(test, &cand).err().as_deref() == Some(sig) {
                cur = cand;
            } else {
                i += chunk;
            }
        }
        if chunk == 1 {
            return cur;
        }
        chunk = chunk.div_ceil(2);
    }
}

/// The hex lines of a recorded input.
pub fn to_hex(b: &[u8]) -> String {
    let mut out = String::new();
    for c in b.chunks(32) {
        for x in c {
            let _ = write!(out, "{x:02x}");
        }
        out.push('\n');
    }
    out
}

/// The bytes of a recorded input's hex lines (after its `//` header).
pub fn from_hex(text: &str) -> Vec<u8> {
    let hex: String = text
        .lines()
        .filter(|l| !l.starts_with("//"))
        .flat_map(str::chars)
        .filter(char::is_ascii_hexdigit)
        .collect();
    (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok())
        .collect()
}

/// `test` on a recorded input: its signature now, `None` if it passes.
pub fn replay(test: Test, text: &str) -> Option<String> {
    watched(test, &from_hex(text)).err()
}
