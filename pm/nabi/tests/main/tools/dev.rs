//! `nabi dev`: the program runs while its sources change; an edit is
//! applied in place, a compile error keeps the old code, and the
//! refusals say what to write.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A game loop: a bounded one, so a leaked program still ends.
const GAME: &str = r#"use core::time::Duration;

struct Player {
    name: str;
    hp: i32;
}

fn tick(p: Player) -> str {
    f"tick one {p.hp}"
}

fn main() {
    let p = Player { name: "ann", hp: 7 };
    let i = 0;
    while i < 3000 {
        println(tick(p));
        std::thread::sleep(Duration::ms(50));
        i += 1;
    }
}
"#;

/// Output read by a thread, for polling.
struct Captured(Arc<Mutex<String>>);

impl Captured {
    fn start(r: impl Read + Send + 'static) -> Self {
        let buf = Arc::new(Mutex::new(String::new()));
        let b = Arc::clone(&buf);
        std::thread::spawn(move || {
            let mut r = BufReader::new(r);
            let mut line = String::new();
            while matches!(r.read_line(&mut line), Ok(n) if n > 0) {
                b.lock().unwrap().push_str(&line);
                line.clear();
            }
        });
        Self(buf)
    }

    fn text(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

/// Wait up to `secs` for `pred` over (stdout, stderr).
fn until(out: &Captured, err: &Captured, secs: u64, what: &str, pred: &dyn Fn(&str, &str) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let (o, e) = (out.text(), err.text());
        if pred(&o, &e) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no {what} after {secs}s\nstdout: {}\nstderr: {e}",
            o.lines().rev().take(5).collect::<Vec<_>>().join("\n")
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Ask `nabi dev` to end (it reaps the program), then reap it.
fn stop(child: &mut Reaped) {
    #[cfg(unix)]
    {
        // SAFETY: a signal to our own child.
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        let st = wait_within(child, 20, &|| "nabi dev after SIGTERM".to_owned());
        assert_eq!(st.code(), Some(130));
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn dev_child(env: &Env, dir: &Path) -> Reaped {
    build_nabic();
    let mut c = Command::new(crate::NABI);
    c.current_dir(dir)
        .env("NABI_HOME", tool_home())
        .env_remove("NABI_ENV")
        .env_remove("CLICOLOR_FORCE")
        .args(["dev", "--nabir"])
        .arg(&env.nabir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    Reaped(c.spawn().unwrap())
}

/// What `nabi dev` refuses before it builds anything.
#[test]
fn dev_refuses_release_and_libraries() {
    let dir = scratch("dev-refuses");
    let app = dir.join("app");
    put(app.join("src/main.nabi"), "fn main() {}\n").unwrap();
    let out = nabi_in(None, &app, &["dev", "--release"]);
    assert_exit(&out, 1);
    let e = text(&out.stderr);
    assert!(
        e.contains("hot reload runs debug builds; drop `--release`"),
        "{e}"
    );
    let out = nabi_in(None, &app, &["dev", "-O3"]);
    assert!(text(&out.stderr).contains("drop `-O3`"));
    let out = nabi_in(None, &app, &["dev", "--bin", "other"]);
    assert!(
        text(&out.stderr).contains("`app` has one executable, `app`, not `other`"),
        "{}",
        text(&out.stderr)
    );
    let lib = dir.join("lib");
    put(lib.join("src/lib.nabi"), "pub fn f() {}\n").unwrap();
    let out = nabi_in(None, &lib, &["dev"]);
    assert_exit(&out, 1);
    let e = text(&out.stderr);
    assert!(e.contains("has no src/main.nabi"), "{e}");
}

/// The program runs; an edit of a function body is applied in place and
/// the program goes on with its state; a compile error keeps the old code.
#[test]
fn dev_applies_an_edit() {
    // Hot builds need plugins, and COFF hot builds are not done yet.
    if !nabild::links_plugin(nabic_target::TargetSpec::host()) || cfg!(windows) {
        return;
    }
    let env = setup();
    let dir = scratch("dev-applies");
    let app = dir.join("game");
    put(app.join("nabi.nabion"), "Package { name: \"game\" }\n").unwrap();
    let main = app.join("src/main.nabi");
    put(&main, GAME).unwrap();
    let mut child = dev_child(&env, &app);
    let out = Captured::start(child.stdout.take().unwrap());
    let err = Captured::start(child.stderr.take().unwrap());
    until(&out, &err, 180, "first tick", &|o, _| {
        o.contains("tick one 7")
    });
    // A compile error: the old code keeps running.
    put(
        &main,
        GAME.replace("f\"tick one {p.hp}\"", "f\"tick {p.nope}\""),
    )
    .unwrap();
    until(&out, &err, 120, "kept", &|_, e| e.contains("Kept"));
    put(&main, GAME.replace("tick one", "tick two")).unwrap();
    until(&out, &err, 120, "applied edit", &|o, e| {
        o.contains("tick two 7") && e.contains("Applied")
    });
    stop(&mut child);
}
