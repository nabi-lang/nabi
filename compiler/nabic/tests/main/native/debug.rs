//! Debug info under debuggers.

#[allow(clippy::wildcard_imports)]
use crate::native::*;
use std::io::Read;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub(crate) const DEBUGGEE: &str = "fn add(a: i32, b: i32) -> i32 {
    let s = a + b;
    let t = s * 2;
    return t;
}

fn main() {
    let x = add(3, 4);
    let p = P { x: x, y: 2.5 };
    let name = \"nabi\";
    let pair = (p, [1u8, 2u8]);
    if x > 0 {
        let x = pair.1[0];
        println(f\"{pair.0.x + x as i32 - 1}\");
    }
}

struct P { x: i32; y: f64; }
";

/// `-g`: a subprogram named by the nabi path with its parameters and
/// locals, and a line table pointing into the `.nabi`;
/// `llvm-dwarfdump --verify` finds no error.
#[test]
fn debug_info_names_functions_lines_and_locals() {
    let env = setup();
    let Some(dump) = ["llvm-dwarfdump", "dwarfdump"]
        .into_iter()
        .find(|t| Command::new(t).arg("--version").output().is_ok())
    else {
        return;
    };
    let backend = "native";
    let dir = scratch(&format!("debug_{backend}"));
    fs::write(dir.join("main.nabi"), DEBUGGEE).unwrap();
    let exe = dir.join("out");
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &["-g", "-o", exe.to_str().unwrap()],
    ));
    let run = Command::new(&exe).output().unwrap();
    ok(&run);
    assert_eq!(text(&run.stdout), "14\n");
    // macOS keeps DWARF in the `.dSYM` nabic writes beside it.
    let dsym = dir.join("out.dSYM");
    let target = if dsym.exists() { dsym } else { exe };
    check_debuggee(dump, &target, backend);
}

/// `-g`: an item declared in a function body and its method and
/// `static` read as written, not by their lifted names.
#[test]
fn debug_info_names_block_local_items() {
    let env = setup();
    let Some(dump) = ["llvm-dwarfdump", "dwarfdump"]
        .into_iter()
        .find(|t| Command::new(t).arg("--version").output().is_ok())
    else {
        return;
    };
    let src = "fn f() -> i32 {\n    static HITS: i32 = 2;\n    struct L { v: i32; }\n    \
               impl L { fn get(self) -> i32 { self.v + HITS } }\n    let l = L { v: 1 };\n    \
               l.get()\n}\nfn main() { println(f\"{f()}\"); }\n";
    let backend = "native";
    let dir = scratch(&format!("debug_local_{backend}"));
    fs::write(dir.join("main.nabi"), src).unwrap();
    let exe = dir.join("out");
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &["-g", "-o", exe.to_str().unwrap()],
    ));
    let dsym = dir.join("out.dSYM");
    let target = if dsym.exists() { dsym } else { exe };
    let info = Command::new(dump)
        .arg("--debug-info")
        .arg(&target)
        .output()
        .unwrap();
    let info = text(&info.stdout);
    for name in ["(\"L\")", "(\"L__get\")"] {
        assert!(info.contains(name), "{backend}: no {name}:\n{info}");
    }
    assert!(!info.contains("(\"f/"), "{backend}: a lifted name:\n{info}");
}

pub(crate) fn check_debuggee(dump: &str, target: &Path, backend: &str) {
    let info = Command::new(dump)
        .arg("--debug-info")
        .arg(target)
        .output()
        .unwrap();
    let info = text(&info.stdout);
    let sub = info
        .split("DW_TAG_subprogram")
        .find(|s| s.contains("(\"add\")") && s.contains("main.nabi"))
        .unwrap_or_else(|| panic!("{backend}: no `add` subprogram:\n{info}"));
    assert!(sub.contains("DW_AT_decl_line\t(1)"), "{backend}: {sub}");
    for var in ["(\"a\")", "(\"b\")", "(\"s\")", "(\"t\")", "\"i32\")"] {
        assert!(sub.contains(var), "{backend}: `add` lacks {var}:\n{sub}");
    }
    // The shadowing `x` lives in the `if` body's lexical block.
    let main = info
        .split("DW_TAG_subprogram")
        .find(|s| s.contains("(\"main\")") && s.contains("main.nabi"))
        .unwrap_or_else(|| panic!("{backend}: no nabi `main`:\n{info}"));
    let block = main
        .split("DW_TAG_lexical_block")
        .nth(1)
        .unwrap_or_else(|| panic!("{backend}: no lexical block:\n{main}"));
    assert!(
        block.contains("(\"x\")") && block.contains("\"u8\")"),
        "{backend}: {block}"
    );
    // Aggregates: a struct with its fields, `str`, a tuple, an array.
    let p = info
        .split("DW_TAG_structure_type")
        .find(|s| s.contains("DW_AT_name\t(\"P\")"))
        .unwrap_or_else(|| panic!("{backend}: no `P` type:\n{info}"));
    for want in ["(\"x\")", "(\"y\")", "DW_AT_data_member_location\t(0x08)"] {
        assert!(p.contains(want), "{backend}: `P` lacks {want}:\n{p}");
    }
    for want in [
        "(\"p\")",
        "(\"name\")",
        "(\"pair\")",
        "(\"str\")",
        "(\"tuple\")",
        "DW_TAG_array_type",
    ] {
        assert!(info.contains(want), "{backend}: no {want}:\n{info}");
    }
    let lines = Command::new(dump)
        .arg("--debug-line")
        .arg(target)
        .output()
        .unwrap();
    let lines = text(&lines.stdout);
    assert!(lines.contains("main.nabi"), "{backend}: {lines}");
    // Only the program's `main` is named so: the C entry calling it is
    // `nabi_main`, so a backtrace has one `main` frame.
    let mains = info
        .split("DW_TAG_")
        .filter(|d| d.starts_with("subprogram") && d.contains("DW_AT_name\t(\"main\")"))
        .count();
    assert_eq!(mains, 1, "{backend}: {mains} `main` subprograms");
    assert!(
        info.contains("DW_AT_name\t(\"nabi_main\")"),
        "{backend}: no `nabi_main`:\n{info}"
    );
    // `let t = s * 2;` (line 3) has a row.
    assert!(
        lines
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some("3")),
        "{backend}: no line-3 row:\n{lines}"
    );
    verify_dwarf(dump, target, backend);
}

/// `(depth, tag, name)` of each entry of the unit named `unit` in a
/// `--debug-info` dump, sorted. LLVM's `__ARRAY_SIZE_TYPE__` (the type
/// of its subrange bounds) is left out.
pub(crate) fn unit_dies(info: &str, unit: &str) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let mut inside = false;
    for die in info.split("\n0x") {
        let Some((_, rest)) = die.split_once(':') else {
            continue;
        };
        let head = rest.lines().next().unwrap_or("");
        let tag = head.trim();
        if !tag.starts_with("DW_TAG_") && tag != "NULL" {
            continue;
        }
        if tag == "DW_TAG_compile_unit" {
            inside = rest.contains(&format!("DW_AT_name\t(\"{unit}\")"));
            continue;
        }
        let name = rest
            .lines()
            .find_map(|l| l.trim().strip_prefix("DW_AT_name\t(\""))
            .map_or("", |n| n.trim_end_matches("\")"));
        if inside && name != "__ARRAY_SIZE_TYPE__" {
            let depth = head.len() - head.trim_start().len();
            out.push((depth, tag.to_owned(), name.to_owned()));
        }
    }
    out.sort();
    out
}

/// `llvm-dwarfdump --verify` reports no error (`dwarfdump` has no
/// verifier).
pub(crate) fn verify_dwarf(dump: &str, target: &Path, what: &str) {
    if dump != "llvm-dwarfdump" {
        return;
    }
    let verify = Command::new(dump)
        .arg("--verify")
        .arg(target)
        .output()
        .unwrap();
    let verify = text(&verify.stdout);
    assert!(!verify.contains("error:"), "{what}: {verify}");
}

pub(crate) const DEBUGGEE_TYPES: &str = "enum Shape { Circle(f64), Rect(i32, i32), Empty }

fn pick(b: bool) -> i64 | f64 {
    if b { return 1.5f64; }
    return 7i64;
}

fn show(s: Shape, v: i64 | f64, xs: List<i32>) -> i32 {
    let n = xs.len() as i32;
    return n;
}

fn main() {
    let xs = [10, 20, 30];
    println(f\"{show(Shape::Rect(3, 4), pick(true), xs)}\");
    println(f\"{held(7i64)}\");
}

fn held(a: any) -> i32 {
    let total = 5;
    let f = [ref total] |k: i32| { total += k; return total; };
    let r = f(3);
    return r + total + call(dbl, 1);
}

fn dbl(k: i32) -> i32 { return k * 2; }

fn call(g: fn(i32) -> i32, k: i32) -> i32 {
    let r = g(k);
    return r;
}
";

/// `-g` DWARF for sums, `any`, `GcBuf`, captured variables and `fn`
/// values, as lldb (with nabi's formatters) and gdb show them.
#[test]
fn debug_info_shows_sums_and_gc_objects() {
    let env = setup();
    let Some(dump) = ["llvm-dwarfdump", "dwarfdump"]
        .into_iter()
        .find(|t| Command::new(t).arg("--version").output().is_ok())
    else {
        return;
    };
    let backend = "native";
    let dir = scratch(&format!("debug_types_{backend}"));
    fs::write(dir.join("main.nabi"), DEBUGGEE_TYPES).unwrap();
    let exe = dir.join("out");
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &["-g", "-o", exe.to_str().unwrap()],
    ));
    let dsym = dir.join("out.dSYM");
    let target = if dsym.exists() { &dsym } else { &exe };
    let info = Command::new(dump)
        .arg("--debug-info")
        .arg(target)
        .output()
        .unwrap();
    let info = text(&info.stdout);
    verify_dwarf(dump, target, backend);
    // The program's unit has its entries, tag and name.
    let unit = unit_dies(&info, "main.nabi");
    assert!(unit.len() > 20, "{unit:?}");
    let part = info
        .split("DW_TAG_variant_part")
        .find(|s| s.contains("(\"Rect\")"))
        .unwrap_or_else(|| panic!("{backend}: no `Shape` variant part:\n{info}"));
    assert!(
        part.contains("(\"Circle\")")
            && part.contains("DW_AT_discr_value")
            && part.contains("\"Shape::Rect\""),
        "{backend}: {part}"
    );
    assert!(
        info.split("DW_TAG_variant_part")
            .any(|s| s.contains("(\"f64\")") && s.contains("(\"i64\")")),
        "{backend}: no `f64 | i64` variant part:\n{info}"
    );
    assert!(
        info.split("DW_TAG_variant_part")
            .any(|s| s.contains("(\"any::i64\")")),
        "{backend}: no `any` variant part:\n{info}"
    );
    let buf = info
        .split("DW_TAG_structure_type")
        .find(|s| s.contains("(\"GcBuf<i32>\")"))
        .unwrap_or_else(|| panic!("{backend}: no `GcBuf<i32>` object type:\n{info}"));
    assert!(
        buf.contains("(\"items\")") && buf.contains("(\"type_id\")"),
        "{backend}: {buf}"
    );
    // Parameters have variables, used or not.
    let show = info
        .split("DW_TAG_subprogram")
        .find(|s| s.contains("(\"show\")"))
        .unwrap_or_else(|| panic!("{backend}: no `show` subprogram:\n{info}"));
    for p in ["s", "v", "xs"] {
        assert!(
            show.split("DW_TAG_formal_parameter")
                .any(|d| d.contains(&format!("(\"{p}\")")) && d.contains("DW_AT_location")),
            "{backend}: no parameter `{p}`:\n{show}"
        );
    }
    let fv = info
        .split("DW_TAG_structure_type")
        .find(|s| s.contains("DW_AT_name\t(\"fn(i32) -> i32\")"))
        .unwrap_or_else(|| panic!("{backend}: no `fn(i32) -> i32` type:\n{info}"));
    assert!(
        fv.contains("(\"code\")") && fv.contains("(\"env\")"),
        "{backend}: {fv}"
    );
    assert!(
        info.contains("DW_TAG_subroutine_type"),
        "{backend}: no subroutine type:\n{info}"
    );
    assert!(
        info.split("DW_TAG_subprogram")
            .any(|s| s.contains("(\"nabi.register_types\")")),
        "{backend}: no `nabi.register_types` subprogram:\n{info}"
    );
    assert!(
        info.split("DW_TAG_subprogram")
            .filter(|s| s.contains("(\"main\")"))
            .count()
            >= 2,
        "{backend}: no subprogram for the C `main`:\n{info}"
    );
    // `call` stops before its call: past its last use the slot of
    // `g` is cleared (a dead GC root).
    if let Some(out) = gdb(
        &exe,
        &[
            "break main.nabi:10",
            "break main.nabi:23",
            "break main.nabi:29",
            "run",
            "info args",
            "c",
            "info args",
            "info locals",
            "c",
            "info args",
        ],
    ) {
        for want in [
            "s = Shape::Rect(3, 4)",
            "v = f64 | i64::f64(1.5)",
            "a = any::i64(7)",
            "total = 8",
            "g = fn(i32) -> i32 {code: 0x",
            "<dbl__fnval>, env: 0x0}",
        ] {
            assert!(out.contains(want), "{backend}: no `{want}`:\n{out}");
        }
    }
    let Some(out) = lldb(
        &exe,
        &[
            "b main.nabi:10",
            "run",
            "frame variable s v xs.h->items[0].buf->items[2]",
            "b main.nabi:23",
            "c",
            "frame variable a total",
            "b main.nabi:29",
            "c",
            "frame variable g",
        ],
    ) else {
        return;
    };
    // The active variant only: its name, then its fields.
    let vars = out.split("frame variable").nth(1).unwrap_or_default();
    assert!(
        vars.contains("(Shape) s = Rect {\n  0 = 3\n  1 = 4\n}"),
        "{backend}: {out}"
    );
    assert!(vars.contains("v = f64 {\n  0 = 1.5\n}"), "{backend}: {out}");
    assert!(
        !vars.contains("Circle") && !vars.contains("$discr$"),
        "{backend}: {out}"
    );
    assert!(vars.contains("= 30"), "{backend}: {out}");
    let held = out.split("frame variable").nth(2).unwrap_or_default();
    assert!(
        held.contains("(any) a = i64 {\n  0 = 7\n}"),
        "{backend}: {out}"
    );
    assert!(held.contains("total = 8"), "{backend}: {out}");
    let g = out.split("frame variable").nth(3).unwrap_or_default();
    assert!(
        g.contains("(fn(i32) -> i32) g = {\n  code = 0x")
            && g.contains("`dbl__fnval at main.nabi:26"),
        "{backend}: {out}"
    );
}

/// `lldb --batch` output for `cmds` on `exe`; `None` without a usable lldb
/// (a wedged debug service hangs `run`; Rosetta processes take none).
pub(crate) fn lldb(exe: &Path, cmds: &[&str]) -> Option<String> {
    if cfg!(target_os = "macos") && std::env::consts::ARCH != "aarch64" {
        return None;
    }
    let init = Command::new(NABIC)
        .args(["--print", "lldbinit"])
        .output()
        .unwrap();
    ok(&init);
    let init = text(&init.stdout);
    let mut c = Command::new("lldb");
    c.args(["--batch", "-O", init.trim()]);
    debugger(c, exe, cmds, "-o", "kill").filter(|o| o.contains("stop reason"))
}

/// The output of `gdb -batch` running `cmds` on `exe`; `None` without a
/// gdb that runs it.
pub(crate) fn gdb(exe: &Path, cmds: &[&str]) -> Option<String> {
    let mut c = Command::new("gdb");
    c.args(["-batch", "-nx"]);
    // A stop: `Breakpoint 2, f (..) at ..` (`Thread 1 "out" hit Breakpoint 2.1, ..`).
    let stop = |l: &str| {
        l.split_once("Breakpoint ")
            .and_then(|(_, r)| r.split_once(", "))
            .is_some_and(|(n, _)| n.chars().all(|c| c.is_ascii_digit() || c == '.'))
    };
    debugger(c, exe, cmds, "-ex", "kill").filter(|o| o.lines().any(stop))
}

/// The stdout of debugger `c` running `cmds` (each after `flag`), then
/// `last`, on `exe`; `None` if it cannot start, or prints nothing for a
/// minute, or another session stalled before it.
///
/// Sessions run one at a time: under a full `cargo test --workspace`
/// several debuggers loading their programs at once starve each other.
/// A stall is a debugger that will not get further (on macOS without
/// developer mode, `run` waits on a password prompt once the last
/// approval, good for ten hours, has run out), so the sessions after it
/// are skipped at once.
pub(crate) fn debugger(
    mut c: Command,
    exe: &Path,
    cmds: &[&str],
    flag: &str,
    last: &str,
) -> Option<String> {
    static ONE: Mutex<()> = Mutex::new(());
    static STALLED: AtomicBool = AtomicBool::new(false);
    for cmd in cmds.iter().chain(&[last]) {
        c.args([flag, cmd]);
    }
    c.arg(exe);
    let _one = ONE.lock().unwrap_or_else(PoisonError::into_inner);
    if STALLED.load(Ordering::Relaxed) {
        eprintln!("skipped: {c:?}: an earlier debugger session stalled");
        return None;
    }
    let mut child = c
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drained as it comes (a debugger blocked on a full pipe never ends),
    // each read marking progress.
    let out = Arc::new(Mutex::new((Vec::new(), Instant::now())));
    let mut pipe = child.stdout.take().unwrap();
    let sink = Arc::clone(&out);
    let reader = std::thread::spawn(move || {
        let mut b = [0; 4096];
        while let Ok(n @ 1..) = pipe.read(&mut b) {
            let mut o = sink.lock().unwrap();
            o.0.extend_from_slice(&b[..n]);
            o.1 = Instant::now();
        }
    });
    let start = Instant::now();
    while child.try_wait().unwrap().is_none() {
        let idle = out.lock().unwrap().1.elapsed();
        if idle > Duration::from_secs(60) || start.elapsed() > Duration::from_secs(600) {
            kill_tree(&mut child);
            STALLED.store(true, Ordering::Relaxed);
            let o = text(&out.lock().unwrap().0);
            let hint = if cfg!(target_os = "macos") && !o.contains(" launched: ") {
                " (it did not start the program: turn on developer mode with \
                 `sudo DevToolsSecurity -enable`)"
            } else {
                ""
            };
            eprintln!("skipped: {c:?} stalled{hint}; its output:\n{o}");
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    reader.join().unwrap();
    let o = std::mem::take(&mut out.lock().unwrap().0);
    Some(text(&o))
}

/// Kill `child` and every process below it. lldb's debugserver starts
/// the debuggee in a process group of its own, and a debuggee stopped
/// under a dead tracer stays stopped for good: the tree is read before
/// anything dies, as orphans move to init.
fn kill_tree(child: &mut Child) {
    let ps = Command::new("ps").args(["-A", "-o", "pid=,ppid="]).output();
    let table: Vec<(i32, i32)> = ps
        .map(|o| text(&o.stdout))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace().map(str::parse);
            Some((w.next()?.ok()?, w.next()?.ok()?))
        })
        .collect();
    let mut tree = vec![i32::try_from(child.id()).unwrap()];
    let mut i = 0;
    while let Some(&p) = tree.get(i) {
        tree.extend(table.iter().filter(|e| e.1 == p).map(|e| e.0));
        i += 1;
    }
    // Stopped first, so none forks or reaps while the rest die.
    for sig in [libc::SIGSTOP, libc::SIGKILL] {
        for &p in &tree {
            // SAFETY: kill(2) takes any pid; one gone is ESRCH.
            unsafe { libc::kill(p, sig) };
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) const DEBUGGEE_OPT: &str = "struct Wrap { inner: any; n: i32; }

fn fib(n: i64) -> i64 {
    if n < 2 { return n; }
    let a = fib(n - 1);
    let b = fib(n - 2);
    return a + b;
}

yield fn evens(limit: i32) -> i32 {
    let k = 0;
    while k < limit {
        let twice = k * 2;
        yield twice;
        k += 1;
    }
}

fn main() {
    let base = 10;
    let f = |k: i32| { return k + base; };
    let w: any = Wrap { inner: 5i64, n: 2 };
    let sum = 0;
    for v in evens(3) {
        sum += v;
    }
    println(f\"{fib(10)} {f(sum)} {held(w)}\"); core::hint::black_box(f);
}

#[inline(never)] fn held(w: any) -> i32 {
    core::hint::black_box(w); return 1;
}
";

/// `-g` at `-O0` and `-O2`: coroutine locals, closures and nested `any`;
/// both levels name the same variables, `-O2` marks the unit optimized.
/// `black_box(f)` keeps the closure live at the breakpoint: `-O2` calls a
/// closure of known code directly.
#[test]
fn debug_info_optimized_coroutines_closures() {
    let env = setup();
    let Some(dump) = ["llvm-dwarfdump", "dwarfdump"]
        .into_iter()
        .find(|t| Command::new(t).arg("--version").output().is_ok())
    else {
        return;
    };
    let mut units = Vec::new();
    for (name, args) in [("o0", &[][..]), ("o2", &["-O2"][..])] {
        let dir = scratch(&format!("debug_opt_{name}"));
        fs::write(dir.join("main.nabi"), DEBUGGEE_OPT).unwrap();
        let exe = dir.join("out");
        let mut extra = vec!["-g", "-o", exe.to_str().unwrap()];
        extra.extend_from_slice(args);
        ok(&build(&env, &dir.join("main.nabi"), &extra));
        let run = Command::new(&exe).output().unwrap();
        ok(&run);
        assert_eq!(text(&run.stdout), "55 16 1\n");
        let dsym = dir.join("out.dSYM");
        let target = if dsym.exists() { &dsym } else { &exe };
        verify_dwarf(dump, target, name);
        let info = Command::new(dump)
            .arg("--debug-info")
            .arg(target)
            .output()
            .unwrap();
        let info = text(&info.stdout);
        // `isOptimized` (Mach-O: `DW_AT_APPLE_optimized`).
        if dsym.exists() {
            let unit = info
                .split("DW_TAG_compile_unit")
                .find(|u| u.contains("DW_AT_name\t(\"main.nabi\")"))
                .and_then(|u| u.split("DW_TAG_").next())
                .unwrap_or_else(|| panic!("{name}: no main.nabi unit:\n{info}"));
            let opt = unit.contains("DW_AT_APPLE_optimized");
            assert_eq!(opt, name == "o2", "{name}: {unit}");
        }
        units.push(unit_dies(&info, "main.nabi"));
        let Some(out) = lldb(
            &exe,
            &[
                "b main.nabi:14",
                "b main.nabi:27",
                "b main.nabi:31",
                "run",
                "frame variable",
                "breakpoint delete 1",
                "c",
                "frame variable -P 3 f",
                "c",
                "frame variable -P 6 -D 12 w",
                "finish",
            ],
        ) else {
            continue;
        };
        // The resume function by its creator; no compiler temporaries.
        let co = out.split("frame variable").nth(1).unwrap_or_default();
        assert!(
            co.contains("k = 0") && co.contains("twice = 0"),
            "{name}: {out}"
        );
        assert!(!co.contains("__"), "{name}: {out}");
        assert!(out.contains("out`evens::{coroutine#0}"), "{name}: {out}");
        let f = out.split("frame variable").nth(2).unwrap_or_default();
        assert!(
            f.contains("env = 0x") && f.contains("0 = 10"),
            "{name}: {out}"
        );
        let w = out.split("frame variable").nth(3).unwrap_or_default();
        assert!(
            w.contains("(any) w = Wrap {") && w.contains("inner = i64 {") && w.contains("0 = 5"),
            "{name}: {out}"
        );
        assert!(w.contains("Return value: (int) $0 = 1"), "{name}: {out}");
    }
    assert!(units[0].len() > 20, "{:?}", units[0]);
    // The variables and parameters, wherever optimization put them.
    let vars = |u: &[(usize, String, String)]| {
        let mut v: Vec<String> = u
            .iter()
            .filter(|d| d.1 == "DW_TAG_variable" || d.1 == "DW_TAG_formal_parameter")
            .map(|d| d.2.clone())
            .filter(|n| ["k", "twice", "base", "f", "w", "sum", "limit"].contains(&n.as_str()))
            .collect();
        v.sort();
        v.dedup();
        v
    };
    assert_eq!(vars(&units[0]), vars(&units[1]), "-O0 vs -O2 variables");
}

pub(crate) const DEBUGGEE_INLINED: &str = "struct P { x: i64; y: i64; }

fn sq(v: i64) -> i64 {
    let r = v * v;
    return r;
}

fn norm(p: P) -> i64 {
    let a = sq(p.x);
    let b = sq(p.y);
    let s = a + b;
    return s;
}

#[inline(never)]
fn work(n: i64, name: str) -> i64 {
    let total = 0;
    let i = 0;
    while i < n {
        let p = P { x: i, y: i + 1 };
        let d = norm(p);
        total += d;
        i += 1;
    }
    println(f\"{name} {total}\");
    return total;
}

fn main() {
    let words = [\"a\", \"bb\", \"ccc\"];
    let k = work(5, words[1]);
    println(f\"{k}\");
}
";

/// The code of executable `b`: its `__text` (Mach-O) or `.text` (ELF64).
pub(crate) fn text_section(b: &[u8]) -> Option<&[u8]> {
    let u16_ = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap()) as usize;
    let u32_ = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
    let u64_ = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap()) as usize;
    let name = |o: usize, n: usize| &b[o..o + n];
    if b.get(..4)? == [0xcf, 0xfa, 0xed, 0xfe] {
        let mut at = 32;
        for _ in 0..u32_(16) {
            let (cmd, size) = (u32_(at), u32_(at + 4));
            if cmd == 0x19 {
                for k in 0..u32_(at + 64) {
                    let s = at + 72 + 80 * k;
                    if name(s, 7) == b"__text\0" {
                        let off = u32_(s + 48);
                        return b.get(off..off + u64_(s + 40));
                    }
                }
            }
            at += size;
        }
        return None;
    }
    if b.get(..5)? != [0x7f, b'E', b'L', b'F', 2] {
        return None;
    }
    let (shoff, shsize, shnum) = (u64_(0x28), u16_(0x3a), u16_(0x3c));
    let strs = u64_(shoff + shsize * u16_(0x3e) + 24);
    (0..shnum).find_map(|k| {
        let h = shoff + shsize * k;
        let n = strs + u32_(h);
        (name(n, 6) == b".text\0").then(|| b.get(u64_(h + 24)..u64_(h + 24) + u64_(h + 32)))?
    })
}

/// `-O2 -g` is `-O2`'s code plus debug info: inlined bodies as
/// `DW_TAG_inlined_subroutine`, variables by location list, and
/// debuggers stopping in an inlined body with its frames and values.
#[test]
fn debug_info_optimized_inlined_frames() {
    let env = setup();
    if crate::common::test_target().is_some() {
        return;
    }
    let dir = scratch("debug_inlined");
    fs::write(dir.join("main.nabi"), DEBUGGEE_INLINED).unwrap();
    let (plain, exe) = (dir.join("plain"), dir.join("out"));
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &["-O2", "-o", plain.to_str().unwrap()],
    ));
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &["-O2", "-g", "-o", exe.to_str().unwrap()],
    ));
    let run = Command::new(&exe).output().unwrap();
    ok(&run);
    assert_eq!(text(&run.stdout), "bb 85\n85\n");
    // The same code (unless `NABI_TEST_OPT` adds `-g` to both).
    let (a, b) = (fs::read(&plain).unwrap(), fs::read(&exe).unwrap());
    if let (Some(a), Some(b)) = (text_section(&a), text_section(&b)) {
        assert!(a == b, "-O2 -g changes the code");
    }
    let dsym = dir.join("out.dSYM");
    let target = if dsym.exists() { &dsym } else { &exe };
    if let Some(dump) = ["llvm-dwarfdump", "dwarfdump"]
        .into_iter()
        .find(|t| Command::new(t).arg("--version").output().is_ok())
    {
        verify_dwarf(dump, target, "inlined");
        let info = Command::new(dump)
            .arg("--debug-info")
            .arg(target)
            .output()
            .unwrap();
        let info = text(&info.stdout);
        let work = info
            .split("DW_TAG_subprogram")
            .find(|s| s.contains("(\"work\")") && s.contains("DW_AT_low_pc"))
            .unwrap_or_else(|| panic!("no `work` subprogram:\n{info}"));
        for want in ["\"norm\")", "\"sq\")", "DW_AT_call_line\t(21)"] {
            assert!(
                work.contains("DW_TAG_inlined_subroutine") && work.contains(want),
                "`work` lacks {want}:\n{work}"
            );
        }
    }
    if let Some(out) = gdb(
        &exe,
        &[
            "break sq",
            "run",
            "bt",
            "up",
            "info args",
            "up",
            "info locals",
        ],
    ) {
        for want in [
            "sq (v=0)",
            "norm (p=...)",
            "work (n=5",
            "p = P {x: 0, y: 1}",
        ] {
            assert!(out.contains(want), "no `{want}`:\n{out}");
        }
    }
    let Some(out) = lldb(
        &exe,
        &[
            "b sq",
            "run",
            "bt",
            "up",
            "frame variable p",
            "up",
            "frame variable n i",
        ],
    ) else {
        return;
    };
    for want in [
        "sq(v=0) at main.nabi:4",
        "[inlined]",
        "norm(p=",
        "work(n=5, name=",
        "(P) p = (x = 0, y = 1)",
        "(long) i = 0",
    ] {
        assert!(out.contains(want), "no `{want}`:\n{out}");
    }
}

/// `-O2 -g`: a parameter taken apart into its fields shows what they
/// hold now, after the body changed them, not what arrived.
#[test]
fn debug_info_split_parameter_follows_its_fields() {
    let env = setup();
    if crate::common::test_target().is_some() {
        return;
    }
    let dir = scratch("debug_split_param");
    fs::write(dir.join("main.nabi"), DEBUGGEE_SPLIT_PARAM).unwrap();
    let exe = dir.join("out");
    ok(&build(
        &env,
        &dir.join("main.nabi"),
        &["-O2", "-g", "-o", exe.to_str().unwrap()],
    ));
    let run = Command::new(&exe).output().unwrap();
    ok(&run);
    assert_eq!(text(&run.stdout), "7\n14\n21\n");
    let cmds = ["b show", "run", "c", "up", "frame variable p"];
    if let Some(out) = lldb(&exe, &cmds) {
        assert!(out.contains("(P) p = (x = 2, y = 12)"), "{out}");
    }
    let cmds = ["break show", "run", "continue", "up", "info args"];
    if let Some(out) = gdb(&exe, &cmds) {
        assert!(out.contains("p = P {x: 2, y: 12}"), "{out}");
    }
}

pub(crate) const DEBUGGEE_SPLIT_PARAM: &str = "struct P { x: i64; y: i64; }

#[inline(never)]
fn show(v: i64) -> i64 {
    println(f\"{v}\");
    v
}

#[inline(never)]
fn walk(p: P, n: i64) -> i64 {
    let s = 0;
    let i = 0;
    while i < n {
        p.x += i;
        p.y *= 2;
        s += show(p.x + p.y);
        i += 1;
    }
    s
}

fn main() {
    let p = P { x: 1, y: 3 };
    println(f\"{walk(p, 2)}\");
}
";

/// `-g` ELF executables linked from several units by nabic's linker:
/// per-unit tables, exact lexical blocks, no ranges left by `--gc-sections`.
#[test]
fn elf_debug_info_of_several_objects() {
    let _ = setup();
    let targets = Path::new(NABIC)
        .parent()
        .unwrap()
        .join("../lib/nabi/targets");
    let dump = "llvm-dwarfdump";
    if !targets.is_dir() || Command::new(dump).arg("--version").output().is_err() {
        eprintln!("skipped: no {} or no {dump}", targets.display());
        return;
    }
    let dir = scratch("elf-debug");
    fs::create_dir_all(dir.join("greet/src")).unwrap();
    fs::write(
        dir.join("greet/src/lib.nabi"),
        "pub fn greet(n: i32) -> i32 {\n    let s = 0;\n    for i in 0..n {\n        \
         if i % 2 == 0 {\n            let d = i * 3;\n            s += d;\n        } else {\n            \
         let e = i + 7;\n            s -= e;\n        }\n    }\n    return s;\n}\n\
         pub fn unused(n: i32) -> i32 { return n * 9; }\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("app/src")).unwrap();
    fs::write(
        dir.join("app/src/main.nabi"),
        "use greet::greet;\nstruct Node { id: i32; kids: List<Node>; }\n\
         enum Shape { Circle(f64), Rect(i32, i32), Empty }\n\
         fn kids(k: i32) -> i64 {\n    let n = Node { id: k, kids: [] };\n    \
         let s = Shape::Rect(k, 2);\n    return n.kids.len() as i64;\n}\n\
         let k = greet(5);\nif k > 0 {\n    let w = k * 2;\n    \
         println(f\"{w}\");\n}\nprintln(f\"{k} {kids(k)}\");\n",
    )
    .unwrap();
    let builds = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-musl"]
        .into_iter()
        .flat_map(|t| [(t, "-O0"), (t, "-O1")]);
    for (t, opt) in builds {
        if !targets.join(t).is_dir() {
            continue;
        }
        let exe = dir.join(format!("app-{t}{opt}"));
        let out = Command::new(NABIC)
            .current_dir(&dir)
            .env("NABI_HOME", crate::common::workspace())
            .args(["-q", "build", "app", "--extern", "greet=greet", "-g"])
            .args([opt, "--target", t, "-o"])
            .arg(&exe)
            .output()
            .unwrap();
        ok(&out);
        let info = Command::new(dump)
            .arg("--debug-info")
            .arg(&exe)
            .output()
            .unwrap();
        let info = text(&info.stdout);
        let sub = info
            .split("DW_TAG_subprogram")
            .find(|s| s.contains("(\"greet::greet\")"))
            .unwrap_or_else(|| panic!("{t} {opt}: no `greet` subprogram:\n{info}"));
        assert!(sub.contains("greet/src/lib.nabi\")"), "{t} {opt}: {sub}");
        // Optimized code keeps no dead locals (nor their types).
        let o0 = opt == "-O0";
        assert!(
            !o0 || sub.contains("(\"d\")") && sub.contains("(\"e\")"),
            "{t} {opt}: {sub}"
        );
        // Sums are variant parts; a struct reached again through a
        // `GcBuf` field is a declaration.
        assert!(
            !o0 || info
                .split("DW_TAG_variant_part")
                .any(|s| s.contains("\"Shape::Rect\")") && s.contains("DW_AT_discr_value")),
            "{t} {opt}: no `Shape` variant part:\n{info}"
        );
        assert!(
            !o0 || info
                .split("DW_TAG_structure_type")
                .any(|s| s.contains("(\"Node\")") && s.contains("DW_AT_declaration")),
            "{t} {opt}: no `Node` declaration:\n{info}"
        );
        let lines = Command::new(dump)
            .arg("--debug-line")
            .arg(&exe)
            .output()
            .unwrap();
        let lines = text(&lines.stdout);
        // Directories and names are separate entries; LLVM's DWARF 4
        // leaves a unit's own directory implicit: its comp_dir.
        for f in ["\"lib.nabi\"", "\"main.nabi\""] {
            assert!(lines.contains(f), "{t} {opt}: no line table names {f}");
        }
        for d in ["/greet/src\"", "/app/src\""] {
            assert!(
                lines.contains(d) || !o0 && info.contains(&format!("{d})")),
                "{t} {opt}: no line table or unit names {d}"
            );
        }
        // Dropped code's DWARF addresses are the tombstone, so no unit
        // (nabi's, the runtime's, the C library's) has an error.
        verify_dwarf(dump, &exe, &format!("{t} {opt}"));
    }
}
