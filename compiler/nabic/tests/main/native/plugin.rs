//! Plugins: a host program loads packages built with
//! `--crate-type=plugin` and calls them with every kind of value; the
//! errors say what to do.

#[allow(clippy::wildcard_imports)]
use crate::native::*;

/// Whether this host links plugins.
fn supported() -> bool {
    nabild::links_plugin(nabic_target::TargetSpec::host())
}

/// Write each `(path, text)` under `dir`.
fn write(dir: &Path, files: &[(&str, &str)]) {
    for (p, t) in files {
        let p = dir.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, t).unwrap();
    }
}

/// `nabic build <pkg> <extra..>` in `dir`, at the run's `-O` level.
fn nabic(dir: &Path, pkg: &str, extra: &[&str]) -> Output {
    Command::new(NABIC)
        .current_dir(dir)
        .env("NABI_HOME", crate::common::workspace())
        .args(["-q", "build", pkg])
        .args(extra)
        .args(crate::common::opt_flags())
        .output()
        .unwrap()
}

/// The profile directory of package `pkg` in `dir`.
fn out_dir(dir: &Path, pkg: &str) -> std::path::PathBuf {
    let release = crate::common::opt_flags()
        .iter()
        .any(|f| matches!(f.as_str(), "-O2" | "-O3" | "-Os"));
    dir.join(pkg)
        .join("target")
        .join(if release { "release" } else { "debug" })
}

/// Build plugin `pkg`, then host `app` (`extern`: `--extern` arguments of
/// both), run it in the plugin's output directory; its output.
fn run(dir: &Path, pkg: &str, externs: &[&str], env: &[(&str, &str)]) -> Output {
    let mut args = vec!["--crate-type=plugin"];
    args.extend(externs);
    ok(&nabic(dir, pkg, &args));
    ok(&nabic(dir, "app", externs));
    let exe = out_dir(dir, "app").join(format!("app{}", std::env::consts::EXE_SUFFIX));
    Command::new(exe)
        .current_dir(out_dir(dir, pkg))
        .envs(env.iter().copied())
        .output()
        .unwrap()
}

const SHAPES: &str = r#"pub struct Circle {
    pub r: f64;
}

pub enum Shape {
    Dot,
    Round(Circle),
    Box { w: i32; h: i32; },
}

impl Display for Circle {
    fn fmt(self, ref f: Formatter) throws (FmtError) { try f.write_str(f"circle {self.r}") }
}

pub fn area(s: Shape) -> f64 {
    match s {
        Shape::Dot => 0.0,
        Shape::Round(c) => 3.0 * c.r * c.r,
        Shape::Box { w, h } => (w * h) as f64,
    }
}
"#;

const VALUES: &str = r#"use shapes::{Circle, Shape};

pub struct Orc {
    pub name: str;
    pub hp: i32;
}

impl Display for Orc {
    fn fmt(self, ref f: Formatter) throws (FmtError) { try f.write_str(f"orc {self.name} ({self.hp})") }
}

pub fn greet(name: str) -> str {
    f"hello {name}"
}

pub fn squares(n: i32) -> List<i32> {
    let xs: List<i32> = List::new();
    for i in 0..n {
        xs.push(i * i);
    }
    xs
}

pub fn count(words: List<str>) -> Dict<str, i32> {
    let d: Dict<str, i32> = Dict::new();
    for w in words {
        d[w] = (d.get(w) ?? 0) + 1;
    }
    d
}

pub fn counter(start: i32) -> fn() -> i32 {
    let n = [start];
    || {
        n[0] += 1;
        n[0]
    }
}

pub fn twice(f: fn(i32) -> i32, x: i32) -> i32 {
    f(f(x))
}

pub fn horde(n: i32) -> List<Orc> {
    let out: List<Orc> = List::new();
    for i in 0..n {
        out.push(Orc { name: f"o{i}", hp: 10 + i });
    }
    out
}

pub fn champion() -> any Display {
    Orc { name: "grak", hp: 99 }
}

pub fn describe(x: any Display) -> str {
    f"plugin sees {x}"
}

pub fn classify(x: any) -> str {
    match x {
        v: i32 => f"i32 {v}",
        v: str => f"str {v}",
        c: Circle => f"circle r={c.r}",
        o: Orc => f"orc {o.name}",
        _ => "other",
    }
}

pub fn either(n: i32) -> i32 | str {
    if n > 0 { n } else { "none" }
}

pub fn ring(r: f64) -> any {
    Circle { r: r }
}

pub fn shape(n: i32) -> Shape {
    match n {
        0 => Shape::Dot,
        1 => Shape::Round(Circle { r: 2.0 }),
        _ => Shape::Box { w: n, h: 3 },
    }
}

pub fn churn(n: i32) -> i32 {
    let keep: List<str> = List::new();
    for i in 0..n {
        let s = f"item {i}";
        if i % 100 == 0 {
            keep.push(s);
        }
    }
    keep.len() as i32
}

pub fn first<T>(xs: List<T>) -> T {
    xs[0]
}

pub fn rings(n: i32) -> List<Circle> {
    let out: List<Circle> = List::new();
    for i in 1..=n {
        out.push(Circle { r: i as f64 });
    }
    out
}

pub fn widest(d: Dict<str, Circle>) -> str {
    let best = "";
    let r = -1.0;
    for (k, c) in d {
        if c.r > r {
            best = k;
            r = c.r;
        }
    }
    best
}
"#;

const VALUES_HOST: &str = r#"use std::plugin;
use shapes::{Circle, Shape, area};

struct Note {
    text: str;
}

impl Display for Note {
    fn fmt(self, ref f: Formatter) throws (FmtError) { try f.write_str(f"note {self.text}") }
}

fn main() throws {
    let p = try plugin::load(plugin::file("values"));
    println(p.name());
    let greet = try p.get::<fn(str) -> str>("values::greet");
    println(greet("host"));
    println(try p.get::<fn(i32) -> List<i32>>("values::squares")(5));
    let d = try p.get::<fn(List<str>) -> Dict<str, i32>>("values::count")(["a", "b", "a"]);
    let (a, b) = (d["a"], d["b"]);
    println(f"{a} {b}");
    let next = try p.get::<fn(i32) -> fn() -> i32>("values::counter")(10);
    println(f"{next()} {next()}");
    let k = 3;
    println(try p.get::<fn(fn(i32) -> i32, i32) -> i32>("values::twice")(|x| x * k, 2));
    let horde = try p.get::<fn(i32) -> List<any Display>>("values::horde") catch e { println(e); |_n| [] };
    println(horde(3).len());
    let champ = try p.get::<fn() -> any Display>("values::champion")();
    println(f"{champ}");
    let describe = try p.get::<fn(any Display) -> str>("values::describe");
    println(describe(Note { text: "from host" }));
    println(describe(champ));
    let classify = try p.get::<fn(any) -> str>("values::classify");
    println(classify(7i32));
    println(classify("s"));
    println(classify(Circle { r: 1.5 }));
    println(classify(2.5));
    let ring = try p.get::<fn(f64) -> any>("values::ring")(4.0);
    match ring {
        c: Circle => println(f"host sees circle {c.r}"),
        _ => println("host sees something else"),
    }
    let either = try p.get::<fn(i32) -> (i32 | str)>("values::either");
    for n in [3, 0] {
        match either(n) {
            v: i32 => println(f"int {v}"),
            s: str => println(f"text {s}"),
        }
    }
    let shape = try p.get::<fn(i32) -> Shape>("values::shape");
    println(f"{area(shape(0))} {area(shape(1))} {area(shape(4))}");
    let churn = try p.get::<fn(i32) -> i32>("values::churn");
    let kept: List<str> = List::new();
    for i in 0..20 {
        kept.push(greet(f"{i}"));
        if churn(2000) != 20 {
            println("churn broke");
        }
    }
    println(f"{kept[0]} {kept[19]}");
    let _ = try p.get::<fn(List<i32>) -> i32>("values::first") catch e { println(e); |_xs| 0 };
    let rs = try p.get::<fn(i32) -> List<Circle>>("values::rings")(4);
    let total = 0.0;
    for c in rs {
        total += c.r;
    }
    let d: Dict<str, Circle> = Dict::new();
    d["a"] = Circle { r: 1.0 };
    d["b"] = Circle { r: 3.0 };
    println(f"{total} {try p.get::<fn(Dict<str, Circle>) -> str>("values::widest")(d)}");
}
"#;

/// Every kind of value crosses both ways; one type is one type on both
/// sides (tags by key); a generic function is not exported.
#[test]
fn plugin_values() {
    let _ = setup();
    if !supported() {
        return;
    }
    let dir = scratch("plugin-values");
    write(
        &dir,
        &[
            ("shapes/src/lib.nabi", SHAPES),
            ("values/src/lib.nabi", VALUES),
            ("app/src/main.nabi", VALUES_HOST),
        ],
    );
    let shapes = format!(
        "--extern=shapes={}",
        dir.join("shapes/src/lib.nabi").display()
    );
    let out = run(&dir, "values", &[&shapes], &[]);
    ok(&out);
    let o = text(&out.stdout);
    let lines: Vec<&str> = o.lines().collect();
    let want = [
        "values",
        "hello host",
        "[0, 1, 4, 9, 16]",
        "2 1",
        "11 12",
        "18",
        "values::horde is not a `fn(i32) -> List<any Display>`;",
        "0",
        "orc grak (99)",
        "plugin sees note from host",
        "plugin sees orc grak (99)",
        "i32 7",
        "str s",
        "circle r=1.5",
        "other",
        "host sees circle 4",
        "int 3",
        "text none",
        "0 12 12",
        "hello 0 hello 19",
        "the plugin has no `pub fn` or static at values::first",
        "10 b",
    ];
    assert_eq!(lines.len(), want.len(), "{o}");
    for (l, w) in lines.iter().zip(want) {
        assert!(l.starts_with(w), "{l:?} is not {w:?} in\n{o}");
    }
}

/// The same with collections in the plugin while the host holds its
/// objects, and objects moving at every safepoint.
#[test]
fn plugin_values_gc_stress() {
    let _ = setup();
    if !supported() {
        return;
    }
    let dir = scratch("plugin-values-gc");
    write(
        &dir,
        &[
            ("shapes/src/lib.nabi", SHAPES),
            ("values/src/lib.nabi", VALUES),
            ("app/src/main.nabi", VALUES_HOST),
        ],
    );
    let shapes = format!(
        "--extern=shapes={}",
        dir.join("shapes/src/lib.nabi").display()
    );
    let out = run(
        &dir,
        "values",
        &[&shapes],
        &[("NABIR_GC_STRESS", "50"), ("NABIR_GC_EVAC", "always")],
    );
    ok(&out);
    assert!(
        text(&out.stdout).contains("hello 0 hello 19\n"),
        "{}",
        text(&out.stdout)
    );
}

/// What fails to load or is not there says what to do.
#[test]
fn plugin_errors() {
    let _ = setup();
    if !supported() {
        return;
    }
    let dir = scratch("plugin-errors");
    write(
        &dir,
        &[
            (
                "tools/src/lib.nabi",
                "pub fn spin(n: i32) -> i32 { n + 1 }\npub fn spun() -> i32 { 0 }\n",
            ),
            ("tools/target/notes.txt", "not a plugin\n"),
            (
                "app/src/main.nabi",
                r#"use std::plugin;

fn main() {
    let _ = try plugin::load("nowhere/libnothing.so") catch e { println(e); try! plugin::load(plugin::file("tools")) };
    let p = try plugin::load(plugin::file("tools")) catch e { println(e); return; };
    let _ = try p.get::<fn(i32) -> str>("tools::spin") catch e { println(e); |_x| "" };
    let _ = try p.get::<fn(i32) -> i32>("tools::spinn") catch e { println(e); |_x| 0 };
    let _ = try p.get_static::<i32>("tools::NOTHING") catch e { println(e); 0 };
    println(try p.get::<fn(i32) -> i32>("tools::spin")(1) catch e { println(e); 0 });
}
"#,
            ),
        ],
    );
    let out = run(&dir, "tools", &[], &[]);
    ok(&out);
    let o = text(&out.stdout);
    for want in [
        "no plugin at nowhere/libnothing.so; build its package with `nabi build`",
        "tools::spin is not a `fn(i32) -> str`; give `get` the exact type tools::spin is declared with in its package: `fn(i32) -> i32`",
        "the plugin has no `pub fn` or static at tools::spinn; did you mean tools::spin",
        "the plugin has no `pub fn` or static at tools::NOTHING",
    ] {
        assert!(o.contains(want), "missing {want:?} in\n{o}");
    }
    assert!(o.ends_with("2\n"), "{o}");
}

/// A plugin's type descriptors reach the host: by `Plugin::types`, and
/// by tag for a value of its type.
#[test]
fn plugin_types() {
    let _ = setup();
    if !supported() {
        return;
    }
    let dir = scratch("plugin-types");
    write(
        &dir,
        &[
            (
                "geo/src/lib.nabi",
                r"use core::reflect;

#[derive(Inspect)]
pub struct Point {
    pub x: i32;
    pub y: i32;
}

pub fn describe() -> str { reflect::info::<Point>().path() }

pub fn origin() -> any { Point { x: 1, y: 2 } }
",
            ),
            (
                "app/src/main.nabi",
                r#"use core::reflect;
use std::plugin;

fn main() {
    let p = try! plugin::load(plugin::file("geo"));
    println(try! p.get::<fn() -> str>("geo::describe")());
    for t in p.types() {
        if t.path() == "geo::Point" {
            println(f"types {t.name()} {t.field_count()} {t.field_at(1).name()}");
        }
    }
    let v = try! p.get::<fn() -> any>("geo::origin")();
    match reflect::info_of(v) {
        t: reflect::TypeInfo => println(f"of {t.path()} {t.holds(v)}"),
        none => println("of none"),
    }
}
"#,
            ),
        ],
    );
    let out = run(&dir, "geo", &[], &[]);
    ok(&out);
    assert_eq!(
        text(&out.stdout),
        "geo::Point\ntypes Point 2 y\nof geo::Point true\n"
    );
}

/// A plugin of another toolchain build is refused with both ids.
#[test]
fn plugin_toolchain_mismatch() {
    let _ = setup();
    if !supported() {
        return;
    }
    let dir = scratch("plugin-toolchain");
    write(
        &dir,
        &[
            ("old/src/lib.nabi", "pub fn one() -> i32 { 1 }\n"),
            (
                "app/src/main.nabi",
                "use std::plugin;\nfn main() {\n    let _ = try plugin::load(plugin::file(\"old\")) catch e { println(e); return; };\n    println(\"loaded\");\n}\n",
            ),
        ],
    );
    let mut c = Command::new(NABIC);
    c.current_dir(&dir)
        .env("NABI_HOME", crate::common::workspace())
        .env("NABI_TEST_PLUGIN_TOOLCHAIN", "nabic 0.0.1 (0123456789ab)")
        .args(["-q", "build", "old", "--crate-type=plugin"])
        .args(crate::common::opt_flags());
    ok(&c.output().unwrap());
    ok(&nabic(&dir, "app", &[]));
    let exe = out_dir(&dir, "app").join(format!("app{}", std::env::consts::EXE_SUFFIX));
    let out = Command::new(exe)
        .current_dir(out_dir(&dir, "old"))
        .output()
        .unwrap();
    ok(&out);
    let o = text(&out.stdout);
    assert!(
        o.contains("the plugin was built by nabic 0.0.1 (0123456789ab), this program by nabic ")
            && o.contains("rebuild the plugin with this program's toolchain"),
        "{o}"
    );
}

/// Unload waits while a closure the plugin made is alive, and the
/// plugin is gone once it is not.
#[test]
fn plugin_unload() {
    let _ = setup();
    if !supported() {
        return;
    }
    let dir = scratch("plugin-unload");
    write(
        &dir,
        &[
            (
                "temp/src/lib.nabi",
                "pub fn adder(k: i32) -> fn(i32) -> i32 { |x| x + k }\npub fn id(x: i32) -> i32 { x }\n",
            ),
            (
                "app/src/main.nabi",
                r#"use std::plugin;

/// Unload while a closure of the plugin is alive.
fn unload_holding(p: plugin::Plugin) {
    let add = try p.get::<fn(i32) -> fn(i32) -> i32>("temp::adder")(5) catch _ { |x| x };
    println(add(1));
    match p.unload() {
        plugin::Unload::Done => println("done"),
        plugin::Unload::Pending(why) => println("pending"),
        plugin::Unload::Kept(why) => println(f"kept {why}"),
    }
    println(add(2));
}

fn main() {
    let p = try plugin::load(plugin::file("temp")) catch e { println(e); return; };
    unload_holding(p);
    println(plugin::loaded().len());
    let _ = try p.get::<fn(i32) -> i32>("temp::id") catch _ { println("not found after unload"); |x| x };
    println(plugin::wait_unloaded(p, 5000u64));
}
"#,
            ),
        ],
    );
    let out = run(&dir, "temp", &[], &[]);
    ok(&out);
    assert_eq!(
        text(&out.stdout),
        "6\npending\n7\n0\nnot found after unload\ntrue\n"
    );
}

/// What cannot be a plugin, or be asked of one, is a compile error that
/// says what to write.
#[test]
fn plugin_must_fail() {
    let _ = setup();
    if !supported() {
        return;
    }
    let dir = scratch("plugin-fail");
    write(
        &dir,
        &[
            ("both/src/lib.nabi", "pub fn f() -> i32 { 1 }\n"),
            ("both/src/main.nabi", "println(1);\n"),
            (
                "app/src/main.nabi",
                "use std::plugin;\nfn main() throws {\n    let p = try plugin::load(\"x\");\n    let n = try p.get::<i32>(\"x::N\");\n}\n",
            ),
        ],
    );
    let out = nabic(&dir, "both", &["--crate-type=plugin"]);
    let err = text(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(err.contains("a plugin is loaded by a program"), "{err}");
    let out = nabic(&dir, "app", &[]);
    let err = text(&out.stderr);
    assert!(!out.status.success(), "{err}");
    assert!(
        err.contains("`Plugin::get` returns a function, and `i32` is not a function type")
            && err.contains("p.get_static::<i32>"),
        "{err}"
    );
    for t in ["wasm32-wasip2", "x86_64-unknown-linux-musl"] {
        let out = nabic(
            &dir,
            "both",
            &["--crate-type=plugin", &format!("--target={t}")],
        );
        let err = text(&out.stderr);
        assert!(!out.status.success(), "{err}");
        assert!(
            err.contains("build one program") || err.contains("-gnu"),
            "{t}: {err}"
        );
    }
}
