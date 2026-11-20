//! Differential fuzzing of the optimizer and back end: random well-typed
//! programs without undefined behaviour (integer arithmetic, loops,
//! calls, lists; every overflow, division by zero and bad index a defined
//! panic), built at `-O0` and at each of `NABI_FUZZ_OPTS` (default
//! `-O2 -Os`): exit status, stdout and stderr must agree.

use crate::common::{Env, NABIC, Rng, output_within};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    I64,
    I32,
    U64,
    U8,
}

const TYS: [Ty; 4] = [Ty::I64, Ty::I32, Ty::U64, Ty::U8];

impl Ty {
    const fn name(self) -> &'static str {
        match self {
            Self::I64 => "i64",
            Self::I32 => "i32",
            Self::U64 => "u64",
            Self::U8 => "u8",
        }
    }

    const fn bits(self) -> u32 {
        match self {
            Self::I64 | Self::U64 => 64,
            Self::I32 => 32,
            Self::U8 => 8,
        }
    }

    const fn signed(self) -> bool {
        matches!(self, Self::I64 | Self::I32)
    }
}

/// A helper function: its name, parameter types and result.
struct Helper {
    name: String,
    params: Vec<Ty>,
    ret: Ty,
}

struct Gen<'r> {
    rng: &'r mut Rng,
    depth: u32,
    /// Variables in scope: name, type, assignable (not a loop counter).
    vars: Vec<(String, Ty, bool)>,
    helpers: Vec<Helper>,
    /// A `List<i64>` in scope.
    list: bool,
    next: usize,
}

impl Gen<'_> {
    fn fresh(&mut self, p: &str) -> String {
        self.next += 1;
        format!("{p}{}", self.next)
    }

    fn lit(&mut self, t: Ty) -> String {
        let v: i64 = match self.rng.below(6) {
            0 => 0,
            1 => 1,
            2 => -1,
            3 => self.rng.range(-300, 300),
            4 => self.rng.range(i64::from(i32::MIN), i64::from(i32::MAX)),
            _ => self.rng.next() as i64,
        };
        let v = match t {
            Ty::I64 => v.to_string(),
            Ty::I32 => (v as i32).to_string(),
            Ty::U64 => (v as u64).to_string(),
            Ty::U8 => (v as u8).to_string(),
        };
        if v.starts_with('-') {
            format!("({v}{})", t.name())
        } else {
            format!("{v}{}", t.name())
        }
    }

    fn var(&mut self, t: Ty, assignable: bool) -> Option<String> {
        let c: Vec<&String> = self
            .vars
            .iter()
            .filter(|v| v.1 == t && (v.2 || !assignable))
            .map(|v| &v.0)
            .collect();
        (!c.is_empty()).then(|| (*self.rng.pick(&c)).clone())
    }

    fn expr(&mut self, t: Ty) -> String {
        if self.depth > 4 || self.rng.chance(20) {
            return match self.var(t, false) {
                Some(v) if self.rng.chance(70) => v,
                _ => self.lit(t),
            };
        }
        self.depth += 1;
        let e = match self.rng.below(16) {
            0..=2 => {
                let op = *self.rng.pick(&["+", "-", "*"]);
                format!("({} {op} {})", self.expr(t), self.expr(t))
            }
            3..=5 => {
                let op = *self
                    .rng
                    .pick(&["wrapping_add", "wrapping_sub", "wrapping_mul"]);
                format!("{}.{op}({})", self.expr(t), self.expr(t))
            }
            6 => {
                let op = *self.rng.pick(&["/", "%"]);
                format!("({} {op} {})", self.expr(t), self.expr(t))
            }
            7 | 8 => {
                let op = *self.rng.pick(&["&", "|", "^"]);
                format!("({} {op} {})", self.expr(t), self.expr(t))
            }
            9 => {
                let op = *self.rng.pick(&["<<", ">>"]);
                let mask = format!("{}{}", t.bits() - 1, t.name());
                format!("({} {op} ({} & {mask}))", self.expr(t), self.expr(t))
            }
            10 => {
                let from = *self.rng.pick(&TYS);
                format!("({} as {})", self.expr(from), t.name())
            }
            11 => format!(
                "(if {} {{ {} }} else {{ {} }})",
                self.cond(),
                self.expr(t),
                self.expr(t)
            ),
            12 if t == Ty::I64 && self.list => {
                let i = self.expr(Ty::U64);
                format!("xs[({i} as usize) % xs.len()]")
            }
            13 => {
                let hs: Vec<usize> = (0..self.helpers.len())
                    .filter(|&k| self.helpers[k].ret == t)
                    .collect();
                if hs.is_empty() {
                    self.lit(t)
                } else {
                    let k = *self.rng.pick(&hs);
                    let params = self.helpers[k].params.clone();
                    let args: Vec<String> = params.iter().map(|&p| self.expr(p)).collect();
                    format!("{}({})", self.helpers[k].name, args.join(", "))
                }
            }
            14 if t.signed() => format!("(-{})", self.expr(t)),
            _ => {
                let a = self.expr(t);
                let pats = self.pats(t);
                format!(
                    "(match {a} {{ {} => {}, {} => {}, _ => {} }})",
                    pats.0,
                    self.expr(t),
                    pats.1,
                    self.expr(t),
                    self.expr(t)
                )
            }
        };
        self.depth -= 1;
        e
    }

    /// Two distinct patterns over values of `t`: a literal and a range.
    fn pats(&mut self, t: Ty) -> (String, String) {
        let n = |v: i64| format!("{v}{}", t.name());
        let a = self.rng.range(0, 3);
        let lo = self.rng.range(4, 50);
        let hi = lo + self.rng.range(0, 50);
        (n(a), format!("{}..={}", n(lo), n(hi)))
    }

    fn cond(&mut self) -> String {
        self.depth += 1;
        let c = match self.rng.below(5) {
            0 if self.depth < 4 => format!("({} && {})", self.cond(), self.cond()),
            1 if self.depth < 4 => format!("({} || {})", self.cond(), self.cond()),
            2 if self.depth < 4 => format!("!{}", self.cond()),
            _ => {
                let t = *self.rng.pick(&TYS);
                let op = *self.rng.pick(&["<", "<=", ">", ">=", "==", "!="]);
                format!("({} {op} {})", self.expr(t), self.expr(t))
            }
        };
        self.depth -= 1;
        c
    }

    fn stmts(&mut self, out: &mut String, indent: usize, n: usize) {
        let scope = self.vars.len();
        for _ in 0..n {
            self.stmt(out, indent);
        }
        self.vars.truncate(scope);
    }

    fn stmt(&mut self, out: &mut String, indent: usize) {
        let pad = " ".repeat(indent);
        match self.rng.below(12) {
            0..=2 => {
                let t = *self.rng.pick(&TYS);
                let e = self.expr(t);
                let v = self.fresh("v");
                let _ = writeln!(out, "{pad}let {v}: {} = {e};", t.name());
                self.vars.push((v, t, true));
            }
            3..=4 => {
                let t = *self.rng.pick(&TYS);
                if let Some(v) = self.var(t, true) {
                    let e = self.expr(t);
                    let _ = writeln!(out, "{pad}{v} = {e};");
                }
            }
            5 if indent < 16 => {
                let c = self.cond();
                let _ = writeln!(out, "{pad}if {c} {{");
                let k = 1 + self.rng.below(3);
                self.stmts(out, indent + 4, k);
                let _ = writeln!(out, "{pad}}} else {{");
                let k = self.rng.below(3);
                self.stmts(out, indent + 4, k);
                let _ = writeln!(out, "{pad}}}");
            }
            6 if indent < 16 => {
                let i = self.fresh("i");
                let n = self.rng.range(0, 12);
                let _ = writeln!(out, "{pad}for {i} in 0i64..{n}i64 {{");
                self.vars.push((i, Ty::I64, false));
                let k = 1 + self.rng.below(4);
                self.stmts(out, indent + 4, k);
                self.vars.pop();
                let _ = writeln!(out, "{pad}}}");
            }
            7 if indent < 16 => {
                let c = self.fresh("c");
                let n = self.rng.range(0, 9);
                let _ = writeln!(out, "{pad}let {c}: i64 = 0i64;");
                let _ = writeln!(out, "{pad}while {c} < {n}i64 {{");
                let _ = writeln!(out, "{pad}    {c} += 1i64;");
                self.vars.push((c, Ty::I64, false));
                let k = 1 + self.rng.below(3);
                self.stmts(out, indent + 4, k);
                self.vars.pop();
                let _ = writeln!(out, "{pad}}}");
            }
            8 if self.list => {
                let e = self.expr(Ty::I64);
                let _ = writeln!(out, "{pad}xs.push({e});");
            }
            9 => {
                let t = *self.rng.pick(&TYS);
                let e = self.expr(t);
                let v = self.fresh("t");
                let _ = writeln!(out, "{pad}let {v}: {} = {e};", t.name());
                let _ = writeln!(out, "{pad}println(f\"{{{v}}}\");");
            }
            _ => {
                let t = *self.rng.pick(&TYS);
                if let Some(v) = self.var(t, true) {
                    let op = *self.rng.pick(&["+=", "-=", "^=", "|="]);
                    let e = self.expr(t);
                    let _ = writeln!(out, "{pad}{v} {op} {e};");
                }
            }
        }
    }

    fn helper(&mut self, out: &mut String) {
        let n = self.rng.below(4);
        let params: Vec<Ty> = (0..n).map(|_| *self.rng.pick(&TYS)).collect();
        let ret = *self.rng.pick(&TYS);
        let name = format!("h{}", self.helpers.len());
        let list: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(i, t)| format!("p{i}: {}", t.name()))
            .collect();
        let _ = writeln!(out, "fn {name}({}) -> {} {{", list.join(", "), ret.name());
        self.vars = params
            .iter()
            .enumerate()
            .map(|(i, &t)| (format!("p{i}"), t, false))
            .collect();
        self.list = false;
        let k = self.rng.below(5);
        self.stmts(out, 4, k);
        let e = self.expr(ret);
        let _ = writeln!(out, "    {e}\n}}\n");
        self.helpers.push(Helper { name, params, ret });
    }
}

/// A program of input `rng`.
pub fn program(rng: &mut Rng) -> String {
    let mut g = Gen {
        rng,
        depth: 0,
        vars: Vec::new(),
        helpers: Vec::new(),
        list: false,
        next: 0,
    };
    let mut out = String::new();
    for _ in 0..g.rng.below(5) {
        g.helper(&mut out);
    }
    out.push_str("fn main() {\n    let xs: List<i64> = [1i64, 2i64, 3i64];\n");
    g.vars.clear();
    g.list = true;
    let k = 3 + g.rng.below(12);
    for _ in 0..k {
        g.stmt(&mut out, 4);
    }
    let shown: Vec<String> = g.vars.iter().map(|v| format!("{{{}}}", v.0)).collect();
    let _ = writeln!(
        out,
        "    println(f\"{} {{xs.len()}}\");\n}}",
        shown.join(" ")
    );
    out
}

/// One build and run: `(exit, stdout, stderr)`, or why it failed.
type Run = Result<(Option<i32>, String, String), String>;

const LIMIT: Duration = Duration::from_secs(120);

fn build_run(env: &Env, dir: &Path, opt: &str, src: &str) -> Run {
    let tag: String = opt.chars().filter(char::is_ascii_alphanumeric).collect();
    // One name at every level, given relative: panic locations name the
    // file as the command line does.
    let dir = dir.join(&tag);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join("p.nabi");
    let exe = dir.join("p");
    std::fs::write(&file, src).map_err(|e| e.to_string())?;
    let mut c = Command::new(NABIC);
    c.current_dir(&dir)
        .args(["-q", "--color=never", "build", "p.nabi"])
        .args(["--bootstrap"])
        .arg(&env.bootstrap)
        .arg("--bootstrap")
        .arg(env.workspace.join("library/std/src/lib.nabi"))
        .arg("--bootstrap")
        .arg(env.workspace.join("library/bare/src/lib.nabi"))
        .arg("--nabir")
        .arg(&env.nabir)
        .args(opt.split_whitespace())
        .arg("--target-dir")
        .arg(dir.join("target"))
        .arg("-o")
        .arg(&exe);
    let b = output_within(&mut c, LIMIT)?;
    if !b.status.success() {
        let err = String::from_utf8_lossy(&b.stderr);
        let first = err
            .lines()
            .find(|l| l.contains("error"))
            .unwrap_or("")
            .to_owned();
        return Err(format!("build: {}", crate::fuzz::mask(&first)));
    }
    let r = output_within(&mut Command::new(&exe), LIMIT)?;
    Ok((
        r.status.code(),
        String::from_utf8_lossy(&r.stdout).into_owned(),
        String::from_utf8_lossy(&r.stderr).into_owned(),
    ))
}

/// The opt levels compared against `-O0`.
pub fn levels() -> Vec<String> {
    std::env::var("NABI_FUZZ_OPTS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map_or_else(
            || vec!["-O2".to_owned(), "-Os".to_owned()],
            |s| s.split(',').map(|x| x.trim().to_owned()).collect(),
        )
}

/// `Err(signature)` when a level disagrees with `-O0` (or a build
/// fails at one level only); `Ok` also for programs no level builds.
pub fn compare(env: &Env, dir: &Path, src: &str) -> Result<(), String> {
    let base = build_run(env, dir, "-O0", src);
    for opt in levels() {
        let other = build_run(env, dir, &opt, src);
        let sig = match (&base, &other) {
            (Err(a), Err(b)) if a.starts_with("build:") && b.starts_with("build:") => continue,
            (Err(e), _) => format!("-O0 {e}"),
            (_, Err(e)) => format!("{opt} {e}"),
            (Ok(a), Ok(b)) if a == b => continue,
            (Ok(a), Ok(b)) if a.0 != b.0 => format!("{opt}: exit {:?}, -O0 exit {:?}", b.0, a.0),
            (Ok(a), Ok(b)) if a.1 != b.1 => format!("{opt}: stdout differs from -O0"),
            _ => format!("{opt}: stderr differs from -O0"),
        };
        return Err(sig);
    }
    Ok(())
}
