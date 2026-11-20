//! `nabi fix`.

use crate::{fails, git, nabi, ok, read, scratch, text, write};

const BEFORE: &str = "\
use core::str::StrBuf;

fn main() {
    let t = true;
    if t == true { println(\"t\"); }
    if t != true { println(\"f\"); }
    while true { break; }
    let y = 2;
    let q = 3i32;
    let w = q.clone();
    let z = w as i32;
    let name = \"n\";
    println(\"{name} {z}\");
}
";

const AFTER: &str = "\
fn main() {
    let t = true;
    if t { println(\"t\"); }
    if !t { println(\"f\"); }
    loop { break; }
    let _y = 2;
    let q = 3i32;
    let w = q;
    let z = w;
    let name = \"n\";
    println(f\"{name} {z}\");
}
";

#[test]
fn fix_applies_lint_fixes_until_clean() {
    let root = scratch("fix");
    let (home, dir) = (root.join("home"), root.join("app"));
    write(&dir.join("nabi.nabion"), "Package { name: \"app\" }\n");
    let main = dir.join("src/main.nabi");
    write(&main, BEFORE);
    git(&dir, &["init", "-q"]);
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "-qm", "x"]);

    // `--diff` shows the first round and writes nothing.
    // Paths print as the platform writes them, as `nabi fmt`'s do.
    let shown = format!("src{}main.nabi", std::path::MAIN_SEPARATOR);
    let out = nabi(&home, &dir, &["fix", "--diff"]);
    let diff = ok(&out);
    assert!(diff.contains(&format!("Diff in {shown}:")), "{diff}");
    assert!(diff.contains("     loop { break; }"), "{diff}");
    assert_eq!(read(&main), BEFORE);

    let out = nabi(&home, &dir, &["fix"]);
    ok(&out);
    assert_eq!(read(&main), AFTER);
    assert!(
        text(&out.stderr).contains(&format!("Fixed {shown}")),
        "{}",
        text(&out.stderr)
    );
    let lint = nabi(&home, &dir, &["lint"]);
    ok(&lint);
    assert!(
        text(&lint.stderr).contains("0 errors, 0 warnings"),
        "{}",
        text(&lint.stderr)
    );

    // Uncommitted changes are refused unless asked.
    fails(
        &nabi(&home, &dir, &["fix"]),
        "uncommitted changes (src/main.nabi)",
    );
    let out = nabi(&home, &dir, &["fix", "--allow-dirty"]);
    ok(&out);
    assert!(
        text(&out.stderr).contains("nothing to fix"),
        "{}",
        text(&out.stderr)
    );

    // An error stops it before any change.
    write(
        &main,
        "fn main() { let x: i32 = \"s\"; while true { break; } }\n",
    );
    let out = nabi(&home, &dir, &["fix", "--allow-dirty"]);
    fails(&out, "to fix by hand first; nothing was changed");
    assert!(read(&main).contains("while true"));
}
