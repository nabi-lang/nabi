//! The REPL: cells, values and their output.

#[allow(clippy::wildcard_imports)]
use crate::tools::*;

#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_keeps_state_across_cells() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
1 + 2
let x =
    40i32;
#[derive(Debug)]
struct P { x: i32; y: i32; }
fn add(p: P) -> i32 { p.x + p.y }
let p = P { x: x, y: 2 };
println(f\"sum {add(p)}\");
p
:type add(p)
let x = \"rebound\";
x
nope
fn add(p: P) -> i32 { p.x * p.y }
add(p)
panic(\"boom\");
[1, 2, 3]
let s: i32 | f64 = 7i32;
s
let a: any = 1.5;
a
:reset
:type x
:quit
1 + 1
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stdout = text(&out.stdout);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stdout}\n{stderr}");
    assert_eq!(
        stdout,
        "\
3
sum 42
P { x: 40, y: 2 }
i32
rebound
80
[1, 2, 3]
7
f64
",
        "stderr: {stderr}"
    );
    // A bad cell reports and is dropped; so is one that fails at run time.
    assert!(stderr.contains("cannot find value `nope`"), "{stderr}");
    assert!(stderr.contains("boom"), "{stderr}");
    // After `:reset` nothing is bound.
    assert!(stderr.contains("cannot find value `x`"), "{stderr}");
}

/// The REPL prints an expression cell with `Display`, else `Debug`, a
/// trailing `?` with `Debug`, only the type when the value has neither
/// (a `RichDisplay` bundle is the kernel's; `display(x)` prints its
/// `text/plain`); `println` uses `Display`; a `let` cell prints nothing.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_prints_debug_and_println_display() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
\"hello\"
struct Q { v: i32; }
impl Display for Q { fn fmt(self, ref f: Formatter) throws (FmtError) { try f.write_str(f\"Q={self.v}\") } }
let q = Q { v: 3 };
q
println(f\"{q}\");
#[derive(Debug)]
struct D { v: i32; }
let d = D { v: 4 };
println(f\"{d:?}\");
d
let x = 42;
x
let xs = [1, 2, 3];
xs?
\"hello\"?
d?
struct Dot {}
impl RichDisplay for Dot { fn mime_bundle(self) -> Dict<str, str> { let b: Dict<str, str> = Dict::new(); b[\"text/plain\"] = \"dot\"; b } }
display(Dot {})
Dot {}
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(
        text(&out.stdout),
        "hello\nQ=3\nQ=3\nD { v: 4 }\nD { v: 4 }\n42\n[1, 2, 3]\n\"hello\"\nD { v: 4 }\ndot\nDot\n",
        "stderr: {stderr}"
    );
}

/// A type a cell stored into `any` is re-seeded in later cells by its
/// full path, though no cell imports it.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_reseeds_any_types_by_path() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
let e: any = core::io::IoError::from_os_code(2);
let n = 1;
e is core::io::IoError
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(text(&out.stdout), "true\n", "stderr: {stderr}");
}

/// Piped input that ends inside a cell (a continuation) is an error,
/// not a silently dropped cell.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_reports_input_ending_inside_a_cell() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let out = piped(NABI, &["repl", "--nabir", &nabir], "1 + 2\nlet x = f(40,");
    let stderr = text(&out.stderr);
    assert_eq!(text(&out.stdout), "3\n", "{stderr}");
    assert!(
        stderr.contains("error: input ended inside a cell; not run")
            && stderr.contains("let x = f(40,"),
        "{stderr}"
    );
    assert_eq!(out.status.code(), Some(1), "{stderr}");
}

/// A cell ends by tokens: brackets in char literals and comments do not
/// count, a balanced line ends the cell (a last `let` needs no `;`), and
/// an item header or a trailing operator asks for the next line.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_cells_end_by_tokens() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
'('
'\"'
/* ( */ 1
1 + 1 /* c */
fn g(x: i32)
    -> i32 { x * 2 }
g(4)
let x = 1
2 + 2
x
let y = 1 +
    2 *
    3 // no `;`
y
y += 2
y
let zs = [1, 2]
zs[0] -= 1
zs?
{ let q = 2; q * 3 }
if y > 1 { 1 } else { 2 }
match y { 9 => 10, _ => 0 }
// done
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(
        text(&out.stdout),
        "(\n\"\n1\n2\n8\n4\n1\n7\n9\n[0, 2]\n6\n1\n10\n",
        "stderr: {stderr}"
    );
    assert!(!stderr.contains("error"), "{stderr}");
}

/// Errors quote a cell as typed, without the `;` the REPL supplies.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_quotes_cells_as_typed() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let out = piped(NABI, &["repl", "--nabir", &nabir], "let a = 1 a\nk += 1\n");
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("expected `;`, found identifier `a`") && stderr.contains("| let a = 1 a\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("`k` is not declared") && stderr.contains("| k += 1\n"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("1 a;") && !stderr.contains("+= 1;"),
        "{stderr}"
    );
}

/// A cell of several statements shows its last one's value.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_cell_value_is_its_last_statement() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
println(\"x\"); while true { break; }
let a = 1; a + 1
println(\"y\"); let b = a + 2;
b
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(text(&out.stdout), "x\n2\ny\n3\n", "stderr: {stderr}");
}

/// Errors in a cell point into its text.
#[test]
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(
            target_os = "linux",
            not(target_env = "musl"),
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
        windows
    )),
    ignore = "the REPL host runs on arm64 macOS, Linux glibc and Windows (arm64, x86_64)"
)]
pub(crate) fn repl_errors_point_into_the_cell() {
    let env = setup();
    let nabir = env.nabir.display().to_string();
    let input = "\
let a = 1; let b: str = 2;
let é = \"é\"; let w = 3; w + true
fn f() -> i64 { 1 }
fn f() -> str { \"x\" }
let p = (1 +
  2); println(p); f() + p
a + 1
";
    let out = piped(NABI, &["repl", "--nabir", &nabir], input);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(text(&out.stdout), "", "{stderr}");
    assert_eq!(
        stderr,
        "\
error[E0040]: mismatched types: `let b` expects `str`, found `i64`
  --> <repl>:1:25
   |
 1 | let a = 1; let b: str = 2;
   |                         ^
   |
   = help: to make text from it, write `f\"{2}\"`
   = help: or, to keep the `i64`, drop `: str`
error[E0054]: operator `+` cannot be applied to `i64` and `bool`
  --> <repl>:1:25
   |
 1 | let é = \"é\"; let w = 3; w + true
   |                         ^^^^^^^^
error[E0040]: mismatched types: the right operand of `+` expects `str`, found `i64`
  --> <repl>:2:25
   |
 2 |   2); println(p); f() + p
   |                         ^
   |
   = help: `+` joins a `str` to a `str` or a `char`; put the value in an f-string: `f\"{f()}{p}\"`
error[E0027]: cannot find value `a` in this scope
  --> <repl>:1:1
   |
 1 | a + 1
   | ^
"
    );
}
