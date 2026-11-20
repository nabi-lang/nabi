//! Workspaces: members share one lock and one `target/`; `-p` and
//! `--workspace` pick the members a command runs on.

#[allow(clippy::wildcard_imports)]
use crate::*;

#[test]
fn members_share_lock_and_target() {
    let root = scratch("workspace");
    let home = root.join("home");
    let ws = root.join("ws");
    // A git library both members use: one commit pinned for both.
    let ext = root.join("ext");
    commit(
        &ext,
        &[("src/lib.nabi", "pub fn base() -> i32 { return 40; }\n")],
    );
    let dep = format!("\"ext\": Dep::git({})", q(ext.display()));
    write(
        &ws.join("nabi.nabion"),
        "Workspace { members: [\"app\", \"libs/*\"] }\n",
    );
    write(
        &ws.join("libs/util/nabi.nabion"),
        &format!("Package {{ name: \"util\", version: \"0.1.0\", deps: {{ {dep} }} }}\n"),
    );
    write(
        &ws.join("libs/util/src/lib.nabi"),
        "pub fn two() -> i32 { return ext::base() / 20; }\n\n#[test]\nfn adds() { assert(two() == 2); }\n",
    );
    write(
        &ws.join("app/nabi.nabion"),
        &format!(
            "Package {{ name: \"app\", deps: {{ \"util\": Dep::path(\"../libs/util\"), {dep} }} }}\n"
        ),
    );
    write(
        &ws.join("app/src/main.nabi"),
        "println(f\"{util::two() + ext::base()}\");\n",
    );
    // At the root: every member; one lock and one target/ there.
    ok(&nabi(&home, &ws, &["-q", "build"]));
    assert!(ws.join("nabi.lock").is_file());
    assert!(ws.join("target").is_dir());
    for m in ["app", "libs/util"] {
        assert!(!ws.join(m).join("nabi.lock").exists(), "{m}");
        assert!(!ws.join(m).join("target").exists(), "{m}");
    }
    let lock = read(&ws.join("nabi.lock"));
    assert_eq!(lock.matches("name: \"ext\"").count(), 1, "{lock}");
    assert_eq!(ok(&nabi(&home, &ws, &["-q", "run", "-p", "app"])), "42\n");
    // In a member: that member, or `--workspace` for all; the lock stays.
    let out = nabi(&home, &ws.join("libs/util"), &["test"]);
    assert!(
        ok(&out).contains("test adds ... ok"),
        "{}",
        text(&out.stdout)
    );
    let out = nabi(&home, &ws.join("app"), &["check", "--workspace"]);
    ok(&out);
    let err = text(&out.stderr);
    assert!(
        err.contains("Member app") && err.contains("Member util"),
        "{err}"
    );
    assert_eq!(read(&ws.join("nabi.lock")), lock);
    ok(&nabi(
        &home,
        &ws,
        &["--locked", "-q", "check", "-p", "util"],
    ));
    // At the root, `update` re-resolves every member; tools run on each.
    ok(&nabi(&home, &ws, &["-q", "update"]));
    assert_eq!(read(&ws.join("nabi.lock")), lock);
    ok(&nabi(&home, &ws, &["-q", "lint"]));
    // Mistakes say what to write.
    fails(
        &nabi(&home, &ws, &["build", "-p", "nope"]),
        "the workspace has no member `nope` (members: app, util)",
    );
    fails(&nabi(&home, &ws, &["run"]), "`nabi run -p <member>`");
    fails(
        &nabi(&home, &ws, &["emit", "--emit", "ir"]),
        "is a workspace root, not a package",
    );
    fails(
        &nabi(&home, &ws, &["add", "x"]),
        "is a workspace root (`Workspace { .. }`)",
    );
    fails(
        &nabi(&home, &ext, &["build", "-p", "x"]),
        "`-p` and `--workspace` work inside a workspace",
    );
    write(
        &ws.join("nabi.nabion"),
        "Workspace { members: [\"app\", \"gone\"] }\n",
    );
    fails(
        &nabi(&home, &ws, &["build"]),
        "member `gone` has no nabi.nabion",
    );
}
