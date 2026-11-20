//! Hot-reload builds (`--hot`): a plugin whose own calls go through
//! slots, its `.layout` dump, and the check of a new version against the
//! live one's. A host program loads the versions with `std::plugin`.

#[allow(clippy::wildcard_imports)]
use crate::native::*;
use std::time::Duration;

/// Whether this host links hot builds: plugins, and not COFF yet.
fn supported() -> bool {
    nabild::links_plugin(nabic_target::TargetSpec::host()) && !cfg!(windows)
}

fn write(dir: &Path, files: &[(&str, &str)]) {
    for (p, t) in files {
        let p = dir.join(p);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, t).unwrap();
    }
}

/// `nabic -q build <pkg> <extra..>` in `dir` (hot builds are `-O0`: no
/// level from the run).
fn nabic(dir: &Path, pkg: &str, extra: &[&str]) -> Output {
    let mut c = Command::new(NABIC);
    c.current_dir(dir)
        .env("NABI_HOME", crate::common::workspace())
        .args(["-q", "build", pkg])
        .args(extra);
    crate::common::output_within(&mut c, Duration::from_secs(600)).unwrap()
}

/// Run `exe` in `cwd`.
fn exec(exe: &Path, cwd: &Path) -> Output {
    let mut c = Command::new(exe);
    c.current_dir(cwd);
    crate::common::output_within(&mut c, Duration::from_secs(120)).unwrap()
}

fn dylib(name: &str) -> String {
    let t = nabic_target::TargetSpec::host();
    match t.object_format {
        nabic_target::ObjectFormat::MachO => format!("lib{name}.dylib"),
        nabic_target::ObjectFormat::Coff => format!("{name}.dll"),
        _ => format!("lib{name}.so"),
    }
}

const GAME: &str = r#"pub struct Player {
    pub name: str;
    pub hp: i32 = 100;
}

pub enum State { Idle, Walk(i32), Hurt { by: i32; } }

pub static COUNT: i32 = 0;

fn bump(p: Player) -> Player {
    Player { name: p.name, hp: p.hp + 1 }
}

pub fn step(n: i32) -> i32 {
    let q = bump(Player { name: "a" });
    match State::Walk(n) {
        State::Walk(k) => q.hp + k,
        _ => 0,
    }
}
"#;

const HOST: &str = r#"use std::plugin;

fn main() throws {
    let p = try plugin::load(plugin::file("game"));
    let step = try p.get::<fn(i32) -> i32>("game::step");
    println(step(5));
}
"#;

/// A hot plugin runs through its slots and dumps its layout; a later
/// version that adds a field with a default is taken, one without is
/// refused with what to write.
#[test]
fn hot_plugin_and_layout() {
    let _ = setup();
    if !supported() {
        return;
    }
    let dir = scratch("hot-plugin");
    write(
        &dir,
        &[("game/src/lib.nabi", GAME), ("app/src/main.nabi", HOST)],
    );
    for d in ["v1", "v2", "v3"] {
        fs::create_dir_all(dir.join(d)).unwrap();
    }
    let v1 = dir.join("v1").join(dylib("game"));
    ok(&nabic(&dir, "game", &["--hot", "-o", v1.to_str().unwrap()]));
    let layout = fs::read_to_string(v1.with_extension("layout")).unwrap();
    assert!(layout.starts_with("Layout {\n    version: 1,"), "{layout}");
    assert!(layout.contains(r#"key: "Y"#), "{layout}");
    assert!(
        layout.contains(r#"Field { name: "hp", ty: "i32", default: true }"#),
        "{layout}"
    );
    assert!(
        layout.contains(r#"Variant { name: "Idle", tag: 0, fields: [] }"#),
        "{layout}"
    );
    ok(&nabic(&dir, "app", &[]));
    let app = dir
        .join("app/target/debug")
        .join(crate::common::exe_name("app"));
    let out = exec(&app, &dir.join("v1"));
    ok(&out);
    assert_eq!(text(&out.stdout), "106\n");

    // Version 2: a new field with a default.
    let base = v1.with_extension("layout");
    let base = base.to_str().unwrap();
    let two = GAME.replace(
        "pub hp: i32 = 100;",
        "pub hp: i32 = 100;\n    pub mana: i32 = 7;",
    );
    write(&dir, &[("game/src/lib.nabi", &two)]);
    let v2 = dir.join("v2").join(dylib("game"));
    let v2s = v2.to_str().unwrap();
    ok(&nabic(
        &dir,
        "game",
        &["--hot", "--hot-base", base, "-o", v2s],
    ));
    let layout = fs::read_to_string(v2.with_extension("layout")).unwrap();
    assert!(layout.contains("version: 2,"), "{layout}");
    let out = exec(&app, &dir.join("v2"));
    ok(&out);
    assert_eq!(text(&out.stdout), "106\n");

    // Refused: a field without a default, a changed field type, a
    // `#[renamed_from]` naming no field, a struct turned into an enum.
    let bad = GAME
        .replace(
            "pub hp: i32 = 100;",
            "pub hp: f64 = 100.0;\n    pub mana: i32;\n    #[renamed_from(\"hpp\")] pub health: i32 = 1;",
        )
        .replace("hp: p.hp + 1", "hp: p.hp, mana: 0")
        .replace("Player { name: \"a\" }", "Player { name: \"a\", mana: 1 }")
        .replace("q.hp + k", "k");
    write(&dir, &[("game/src/lib.nabi", &bad)]);
    let v3 = dir.join("v3").join(dylib("game"));
    let out = nabic(
        &dir,
        "game",
        &["--hot", "--hot-base", base, "-o", v3.to_str().unwrap()],
    );
    assert!(!out.status.success());
    let err = text(&out.stderr);
    for want in [
        "error[E0149]: field `hp` of `Player` changed from `i32` to `f64` while the program runs",
        "help: give the new field its own name with a default (`hp_f64: f64 = 0.0`)",
        "error[E0149]: new field `mana` of `Player` needs a default while the program runs",
        "help: write `mana: i32 = 0;` (or restart with `r`)",
        "`Player` had no field `hpp`",
        "help: name the field as it was before: `name`, `hp`",
    ] {
        assert!(err.contains(want), "missing {want:?} in\n{err}");
    }
    assert!(!v3.exists() && !v3.with_extension("layout").exists());
    let kind = GAME.replace("pub enum State { Idle, Walk(i32), Hurt { by: i32; } }", "pub struct State { k: i32; }")
        .replace("    match State::Walk(n) {\n        State::Walk(k) => q.hp + k,\n        _ => 0,\n    }", "    q.hp + n + State { k: 0 }.k");
    write(&dir, &[("game/src/lib.nabi", &kind)]);
    let out = nabic(
        &dir,
        "game",
        &["--hot", "--hot-base", base, "-o", v3.to_str().unwrap()],
    );
    assert!(!out.status.success());
    let err = text(&out.stderr);
    assert!(
        err.contains("`State` changed from an enum to a struct while the program runs"),
        "{err}"
    );
}

/// A program's root built hot exports `<pkg>::main`; a hot dependency's
/// functions are called through slots bound to its own plugin.
#[test]
fn hot_root_and_dependency() {
    let _ = setup();
    if !supported() {
        return;
    }
    let dir = scratch("hot-root");
    write(
        &dir,
        &[
            (
                "eng/src/lib.nabi",
                "pub struct Vec2 { pub x: i32; pub y: i32 = 0; }\n\
                 pub fn add(a: Vec2, b: Vec2) -> Vec2 { Vec2 { x: a.x + b.x, y: a.y + b.y } }\n\
                 pub fn twice<T>(x: T) -> List<T> { [x, x] }\n",
            ),
            (
                "root/src/main.nabi",
                "use eng::{Vec2, add, twice};\n\n\
                 fn main() {\n    let v = add(Vec2 { x: 1 }, Vec2 { x: 2, y: 5 });\n    \
                 println(f\"v {v.x} {v.y} {twice(3).len()}\");\n}\n",
            ),
            (
                "app/src/main.nabi",
                "use std::plugin;\n\n\
                 fn main() throws {\n    let _e = try plugin::load(plugin::file(\"eng\"));\n    \
                 let p = try plugin::load(plugin::file(\"root\"));\n    \
                 try p.get::<fn()>(\"root::main\")();\n}\n",
            ),
        ],
    );
    let eng = format!("--extern=eng={}", dir.join("eng/src/lib.nabi").display());
    fs::create_dir_all(dir.join("dev")).unwrap();
    let out_eng = dir.join("dev").join(dylib("eng"));
    let out_root = dir.join("dev").join(dylib("root"));
    ok(&nabic(
        &dir,
        "eng",
        &["--hot", "-o", out_eng.to_str().unwrap()],
    ));
    ok(&nabic(
        &dir,
        "root",
        &[
            "--hot",
            "--hot-dep",
            "eng",
            &eng,
            "-o",
            out_root.to_str().unwrap(),
        ],
    ));
    ok(&nabic(&dir, "app", &[]));
    let app = dir
        .join("app/target/debug")
        .join(crate::common::exe_name("app"));
    let out = exec(&app, &dir.join("dev"));
    ok(&out);
    assert_eq!(text(&out.stdout), "v 3 5 2\n");
}

/// What `--hot` takes, and the help when it does not.
#[test]
fn hot_options() {
    let _ = setup();
    let dir = scratch("hot-options");
    write(&dir, &[("game/src/lib.nabi", "pub fn f() {}\n")]);
    let out = nabic(&dir, "game", &["--hot", "-O2"]);
    assert_eq!(out.status.code(), Some(2));
    let err = text(&out.stderr);
    assert!(
        err.contains("hot reload builds at `-O0`, not `-O2`"),
        "{err}"
    );
    assert!(
        err.contains("hot reload runs debug builds; drop `--release`"),
        "{err}"
    );
    let out = nabic(&dir, "game", &["--hot-base", "x.layout"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("add `--hot`, or drop `--hot-base`"));
}
