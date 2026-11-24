//! The `nabic` command line as text, for `nabic help` and its help per
//! command.
//!
//! `nabi` renders its own commands' help with the same [`CmdHelp`] table,
//! and its completion scripts take nabic's options.

use crate::completions::{Spec, Values, section_flags};
use crate::term::{self, Depth, Role, Stream};
use std::fmt::Write as _;

/// `nabic help`.
pub const NABIC_USAGE: &str = "\
usage: nabic [global options] <command> [options] [args]

commands:
  build [path]           build the package: the executable
                         target/<profile>/<name> of `src/main.nabi`, the
                         library target/<profile>/lib<name>.a + <name>.nabihir
                         of `src/lib.nabi`, or both (the executable uses the
                         library as `<name>`); <profile> is debug, or release
                         at -O2+ / -Os; a --target build goes under
                         target/<target>/
  test [path] [filter]   build the `#[test]` harness, the library's then the
                         executable's; `nabi test` runs it (filter: regex with
                         ^ $ . * + ?, or a substring)
  bench [path] [filter]  build (-O2 unless -O given) the `#[bench]` harness;
                         `nabi bench` runs it and prints the time per
                         iteration (+/- deviation)
  check [path | -]       front end, lints and codegen, no native build
                         (`-`: source on stdin)
  emit [path]            write an intermediate artifact (see --emit)
  version                print the version
  help                   print this help

path is a package directory (default: the current one), a file inside a
package (the nearest directory upward with `src/main.nabi` or `src/lib.nabi`),
or a lone `.nabi` file outside a package's `src/`, which is built as an
anonymous executable package.

global options:
  -v, --verbose                stage timings
  -vv                          also the files read and the generic
                               instances made
  -q, --quiet                  no progress output
  --color=auto|always|never    color output (--no-color = never)
  --cwd=<path>                 change directory first
  --message-format=human|json  diagnostics as JSON lines on stdout
  --explain <code>             what an error code (E0001) means and what
                               to write instead, then exit
  -A|-W|-D <lint|group>        allow / warn / deny a lint (repeatable, later
                               wins; `#[allow(..)]` etc. in the source
                               override)

build options:
  -o, --output <path>          output file
  --emit=<kind>[,<kind>...]    source | hir | mir | obj | asm | listing |
                               expansions | c-header (source: the
                               package's whole AST after meta expansion
                               and desugaring, as Nabi
                               source; asm: assembly source, which an
                               assembler turns into the same object;
                               listing: an objdump-style listing, each
                               instruction with its offset and bytes;
                               expansions: each meta expansion's output;
                               c-header: a C header declaring the
                               package's exported extern \"C\" fns and
                               the #[repr(C)] types they use)
                               (written to
                               target/<profile>/build/<name>.<ext> unless -o)
  --crate-type=lib|cdylib|staticlib|plugin
                               a library package's output: lib (default) the
                               static lib<name>.a + <name>.nabihir; cdylib
                               also a shared library C code loads
                               (lib<name>.so / lib<name>.dylib / <name>.dll
                               + <name>.lib), exporting its
                               #[export_name] extern \"C\" fns; staticlib
                               also one static library C links alone
                               (static/lib<name>.a, with the runtime, and
                               include/module.modulemap); plugin also
                               a shared library a Nabi program loads with
                               std::plugin (same file names, no .lib)
  --xcframework                with staticlib: also put the library into
                               target/<profile>/<name>.xcframework (one
                               slice per Apple target built: build for
                               aarch64-apple-ios and aarch64-apple-ios-sim)
  --bundle=<dep>               a plugin links dependency <dep> in instead of
                               taking it from the program that loads it
  --hot                        a hot-reload plugin for `nabi dev` (implies
                               --crate-type=plugin, -O0 only); also writes
                               <output stem>.layout beside it
  --hot-base=<file>            the .layout of the version the program runs:
                               the new version is checked against it
  --hot-dep=<dep>              dependency <dep> is hot too: called through
                               reload slots instead of compiled in
  --target=<target>            a built-in target (--print target-list) or
                               a custom one's <name>.nabion file
  --release                    optimized build (same as -O2)
  -O0 | -O1 | -O2 | -O3 | -Os  optimization level (default -O0). -O1: loops
                               optimized as at -O2 (iterators made counted,
                               vectorized), little inlined elsewhere; -O2:
                               more inlined; -O3: also scalar loops unrolled,
                               more inlined in loops; -Os: small code, only
                               tiny functions inlined (more in loops), loops
                               still counted and vectorized. -O2 / -O3 / -Os
                               build the release profile (no debug info, code
                               folded, symbols stripped), the others the
                               debug profile (debug info)
  -C overflow-checks[=yes|no]  integer overflow panics (default: on)
  -C icf[=yes|no]              fold identical functions at link (default:
                               on in release, off in debug)
  -C target-cpu=<cpu>|native   build for that CPU's features instead of the
                               target's baseline (native: this machine's;
                               x86_64: avx2 vector loops, popcnt / lzcnt /
                               bmi in scalar code)
  -C target-feature=+f1,-f2,.. turn CPU features on / off (x86_64: +avx2, ..;
                               -simd128 on wasm32, -neon on armv7, -vsx on
                               powerpc64le: loops without vectors); the
                               binary may not run on CPUs without them
  -C subsystem=console|windows Windows executable subsystem (default: console;
                               windows: a GUI program entered through WinMain)
  --cfg <flag | key=value>     #[cfg] flag (repeatable)
  --print cfg                  print the active cfg (target keys, --cfg,
                               debug_assertions below -O1) and exit
  --print target-list          print the supported targets and exit
  --print target-spec          print the --target's spec as NABION and exit
  --print lldbinit             print the lldb command that loads Nabi's
                               formatters (sums, collections, str) and exit
  --print gdbinit              print the gdb command that loads Nabi's
                               pretty-printers and exit
  --print natvis               print the path of Nabi's natvis
                               visualizers (Visual Studio, WinDbg) and exit
  --bootstrap <file>           library root to preload (repeatable; default:
                               <sysroot>/lib/nabi/{core,std,bare}/src/lib.nabi)
  --nabir <lib>                runtime library: path, or name for -l<name>
                               (default: <sysroot>/lib/libnabir.a)
  -L <dir>                     native library search directory, before
                               LIBRARY_PATH (repeatable)
  -l [static=|dylib=|framework=]<name>
                               link a native library or Apple framework,
                               as `#[link]` does (repeatable)
  -F <dir>                     Apple framework search directory
                               (repeatable)
  --native-object <file>       an object or archive linked in as it is
                               (repeatable)
  --lib-version <x.y.z>        a cdylib's version: its ELF soname
                               lib<name>.so.<x> (lib<name>.so.0.<y> below
                               1.0) and Mach-O current and compatibility
                               versions
  --native-runtime <file>      a shared library the executable loads:
                               copied beside it, and the executable looks
                               in its own directory (repeatable)
  --extern <name>=<path>       dependency package: its directory or `src/lib.nabi`
                               (prebuilt into target/<profile>/deps/ first,
                               each after the ones before it), or a prebuilt
                               <name>.nabihir / lib<name>.a; importable as
                               `use name::..`; another name for the same path
                               is an alias (repeatable; `nabi` passes these)
  --extern-feature <name>=<f>  build source dependency <name> with
                               cfg feature=<f> (repeatable; `--cfg
                               feature=..` is the package's own)
  --extern-dep <pkg>=<name>    package <pkg> (`pkg`: the one being built)
                               depends on extern <name>; with any, a
                               package sees only the externs it depends on
                               (repeatable; `nabi` passes these)
  --name <name>                the package name (default: its directory's;
                               `nabi` passes nabi.nabion's `name`); a
                               library's is in its symbols
  --target-dir <dir>           instead of the package's target/ (without a
                               package: the system temp directory's
                               nabic-target-<uid>/, private to the user, a
                               cache trimmed to what was built in the last
                               week)
  -g                           debug info in a release build too (a debug
                               build always has it; macOS: a .dSYM next to
                               the executable)
  --remap-path-prefix <from>=<to>
                               write paths under <from> as under <to> in
                               panic locations, debug info and .nabihir
                               (repeatable, the last match wins). The
                               standard library is always /nabi/library;
                               a release build also writes the package's
                               files relative to it and a dependency's as
                               under <name>
  --keep                       keep the temporary build directory

check, build:   --check-library (check and compile every library body,
                reached or not)
harness:        --emit-harness <file> (write the built harness and its cases
                to <file> for `nabi` to run, instead of naming it)
bindgen:        --c-shim <file> (C wrappers of the header's `static inline`
                functions to <file>, for the manifest's `c.sources`; the
                bindings call them)
test options:   --doc (only the doc tests: ```nabi blocks in the library's
                docs and README.md), --no-doc (none of them)

The sysroot is the directory above nabic's own bin/ (a toolchain:
$NABI_HOME/toolchains/<version>-<triple>/); a build in the repository uses
its library/ and the libnabir.a beside nabic.

environment:
  NABI_CC           C compiler driver for the final link (default: nabic's
                    own linker)
  NABI_TARGET_DIR   target/ location (default: inside the package)
  NABI_VERBOSE      1: same as --verbose
  NABI_TIME_PASSES  1: the wall time of each compiler pass on stderr
  NABI_INCREMENTAL  where the front end keeps parses and lowered bodies
                    (default: target/<profile>/incremental/; 0: nowhere)
  NABI_INCREMENTAL_VERIFY
                    1: lower every body again and fail on a difference
  NABI_FN_CACHE     0: keep no standard library code in the toolchain's
                    cache (`nabi cache`) and take none from it
  NABI_HOME         the toolchains, environments and caches (default:
                    ~/.nabi)
  NABI_RUNNER       <program> [args]: runs another target's executables for
                    run, test and bench (default: wasmtime, node, qemu-<cpu>)
  LIBRARY_PATH      native library search directories, after -L
  NO_COLOR          non-empty: --color=auto uses no color
  CLICOLOR_FORCE    non-empty and not 0: --color=auto uses color even when
                    the stream is not a terminal, unless NO_COLOR is set
  COLORTERM         truecolor or 24bit: 24-bit color
  TERM              dumb: --color=auto uses no color; *256color*: the
                    256-color palette
  __NABI_CLI        nabi: messages name `nabi`, not nabic (`nabi` sets it)
  __NABI_ENV, __NABI_ENV_LIBS
                    the environment `nabi` runs a script in, and its
                    libraries (`nabi` sets them)
                    (the test and bench runner keeps these three out
                    of the program's environment)

Programs read NABIR_WORKERS, NABIR_THREAD_PER_CORE, NABIR_DYNAMIC_WORKERS,
NABIR_GC_* and NABIR_HEAP_DUMP.
";

/// One command's help. `{tool}` in `usage`, `about` and `examples`
/// reads as the program the user ran (`nabi` or `nabic`).
#[derive(Clone, Copy, Debug)]
pub struct CmdHelp {
    pub name: &'static str,
    /// One line for the command list.
    pub summary: &'static str,
    /// Forms after the program name: `build [path] [options]`.
    pub usage: &'static [&'static str],
    /// A paragraph, wrapped.
    pub about: &'static str,
    /// Said only under `nabic`.
    pub nabic_note: &'static str,
    /// `(command line, what it does)`.
    pub examples: &'static [(&'static str, &'static str)],
    /// More examples, shown only under `nabi`: its runner's options.
    pub nabi_examples: &'static [(&'static str, &'static str)],
    /// The command's own options: `(head, description)`.
    pub flags: &'static [(&'static str, &'static str)],
    /// Option sections of `NABIC_USAGE` that apply too.
    pub sections: &'static [&'static str],
}

impl CmdHelp {
    pub const EMPTY: Self = Self {
        name: "",
        summary: "",
        usage: &[],
        about: "",
        nabic_note: "",
        examples: &[],
        nabi_examples: &[],
        flags: &[],
        sections: &[],
    };
}

/// The program the user ran: `nabi` when it runs nabic for them (it sets
/// `__NABI_CLI=nabi`), else `nabic`.
#[must_use]
pub fn invoked_as() -> &'static str {
    if crate::handoff_var("NABI_CLI").is_some_and(|v| v == "nabi") {
        "nabi"
    } else {
        "nabic"
    }
}

/// This process's arguments without the program name, split at the first
/// `--`: those up to it (with it) as strings, the program's after it as
/// given. A non-UTF-8 argument before `--` is an error message.
pub fn os_args() -> Result<(Vec<String>, Vec<std::ffi::OsString>), String> {
    split_os_args(std::env::args_os().skip(1))
}

/// [`os_args`] of `args`.
pub fn split_os_args(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<(Vec<String>, Vec<std::ffi::OsString>), String> {
    let mut it = args.into_iter();
    let mut head = Vec::new();
    for a in it.by_ref() {
        let a = utf8_arg(head.len() + 1, a)?;
        let end = a == "--";
        head.push(a);
        if end {
            break;
        }
    }
    Ok((head, it.collect()))
}

/// Argument `n` (from 1) as a string: options, commands and paths must
/// be UTF-8; a program's arguments, after `--`, need not.
pub fn utf8_arg(n: usize, a: std::ffi::OsString) -> Result<String, String> {
    a.into_string().map_err(|a| {
        format!(
            "argument {n} (`{}`) is not valid UTF-8; only a program's arguments, after `--`, \
             may be other bytes",
            a.to_string_lossy()
        )
    })
}

/// Whether command-line word `w` names a script, not a command: it has a
/// `/` (Windows: or `\`) or ends in `.nabi`, which no command does.
#[must_use]
pub fn looks_like_script(w: &str) -> bool {
    std::path::Path::new(w)
        .extension()
        .is_some_and(|x| x == "nabi")
        || w.contains('/')
        || cfg!(windows) && w.contains('\\')
}

/// Words people type for a command: `(command, [synonyms])`.
pub const CMD_SYNONYMS: &[(&str, &[&str])] = &[
    ("build", &["compile", "make"]),
    ("run", &["exec", "start"]),
    ("fmt", &["format", "fix"]),
    ("install", &["i"]),
    ("remove", &["rm", "del", "delete"]),
    ("update", &["upgrade", "up"]),
    ("new", &["create"]),
    ("toolchain list", &["ls", "list"]),
    ("version", &["ver"]),
];

/// The front end's own flags of a command (`nabi run`, its tools),
/// which an unknown option is also matched against.
static FLAG_SOURCE: std::sync::OnceLock<fn(&str) -> Vec<String>> = std::sync::OnceLock::new();

/// Register where the flags of the front end's commands come from.
pub fn set_flag_source(f: fn(&str) -> Vec<String>) {
    let _ = FLAG_SOURCE.set(f);
}

/// The variable a front end passes its flags of the command in, one per
/// line, when it runs nabic as a child.
pub const FRONT_FLAGS_VAR: &str = "NABI_FRONT_FLAGS";

/// The front end's flags of `cmd` (none outside it).
#[must_use]
pub fn front_end_flags(cmd: &str) -> Vec<String> {
    if let Some(f) = FLAG_SOURCE.get() {
        return f(cmd);
    }
    std::env::var(FRONT_FLAGS_VAR)
        .map(|v| v.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// "; did you mean `x`?" for `name` among `cands` (synonyms first), or
/// nothing.
#[must_use]
pub fn did_you_mean<'a>(
    name: &str,
    cands: impl IntoIterator<Item = &'a str>,
    syn: &[(&str, &[&str])],
) -> String {
    crate::suggest::suggest_or_synonym(name, cands, syn)
        .map(|s| format!("; did you mean `{s}`?"))
        .unwrap_or_default()
}

/// nabic's commands, in `NABIC_USAGE` order.
pub const NABIC_COMMANDS: &[CmdHelp] = &[
    CmdHelp {
        name: "build",
        summary: "compile the package",
        usage: &["build [path] [options]"],
        about: "\
Build the package at <path> (default: the current directory). src/main.nabi
becomes the executable target/<profile>/<name>, src/lib.nabi the library
target/<profile>/lib<name>.a and <name>.nabihir. <profile> is debug, or
release with --release, -O2, -O3 or -Os. A lone .nabi file outside a
package's src/ builds as a one-file program, its executable beside it
(hello.nabi: hello) unless -o names another file.",
        examples: &[
            ("{tool} build", "debug build of the package here"),
            ("{tool} build --release", "optimized build"),
            (
                "{tool} build hello.nabi -o hello",
                "one file, the executable named hello",
            ),
        ],
        sections: &["build options"],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "test",
        summary: "run the tests",
        usage: &["test [path] [filter] [options]"],
        about: "\
Build the `#[test]` functions of the library, then of the executable, and
run them. A filter keeps the tests whose name matches: a regex with
^ $ . * + ?, or a substring.",
        nabic_note: "nabic only builds the harness; `nabi test` runs it.",
        examples: &[
            ("{tool} test", "every test"),
            ("{tool} test parse", "the tests whose name contains parse"),
        ],
        nabi_examples: &[(
            "{tool} test --threads=1 --show-output",
            "one at a time, with their output",
        )],
        sections: &["build options"],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "bench",
        summary: "run the benchmarks",
        usage: &["bench [path] [filter] [options]"],
        about: "\
Build the `#[bench]` functions (at -O2 unless an -O level is given) and run
them; each prints its time per iteration and the deviation. A filter works
as for tests.",
        nabic_note: "nabic only builds the harness; `nabi bench` runs it.",
        examples: &[
            ("{tool} bench", "every benchmark"),
            ("{tool} bench sort", "those matching sort"),
        ],
        nabi_examples: &[(
            "{tool} bench sort --samples=20",
            "those matching sort, 20 samples each",
        )],
        sections: &["build options"],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "check",
        summary: "find errors without building",
        usage: &["check [path | -] [options]"],
        about: "\
Run the front end, the lints and code generation, without the native
build. `-` reads the source from standard input.",
        examples: &[
            ("{tool} check", "check the package here"),
            (
                "{tool} check src/main.nabi",
                "the package that holds this file",
            ),
        ],
        sections: &["build options", "check options"],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "emit",
        summary: "write an intermediate form",
        usage: &["emit [path] [--emit=<kind>[,<kind>...]] [options]"],
        about: "\
Write intermediate forms of the package to target/<profile>/build/ (or
-o): source, hir, mir (the default), obj, asm, listing or expansions.",
        examples: &[
            ("{tool} emit", "the MIR"),
            ("{tool} emit --emit=hir,mir", "HIR and MIR"),
            ("{tool} emit --emit=obj -o out.o", "the object file"),
            ("{tool} emit --emit=asm -o out.s", "the assembly source"),
            (
                "{tool} emit --emit=listing -o out.lst",
                "the code with offsets and bytes",
            ),
        ],
        sections: &["build options"],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "version",
        summary: "print the version",
        usage: &["version [-v]"],
        about: "Print the version; with -v, also the host.",
        examples: &[("{tool} version -v", "")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "help",
        summary: "print help",
        usage: &["help [command]"],
        about: "\
With a command: its usage, examples and options (`{tool} <command> --help`
is the same). Without one: every command and the global options.",
        examples: &[("{tool} help build", "")],
        ..CmdHelp::EMPTY
    },
];

/// The entry for `cmd` in `table`.
#[must_use]
pub fn find<'a>(table: &'a [CmdHelp], cmd: &str) -> Option<&'a CmdHelp> {
    table.iter().find(|h| h.name == cmd)
}

/// Section `name` of `NABIC_USAGE`: its first line and the indented ones
/// after it.
fn usage_section(name: &str) -> Vec<&'static str> {
    let mut lines = NABIC_USAGE
        .lines()
        .skip_while(|l| !l.starts_with(&format!("{name}:")));
    let mut out: Vec<&str> = lines.next().into_iter().collect();
    out.extend(lines.take_while(|l| l.starts_with(' ') && !l.trim().is_empty()));
    out
}

/// `rows` as `  name  description`, descriptions aligned; a long name
/// puts its description on the next line.
#[must_use]
pub fn rows(d: Depth, rows: &[(String, &str)]) -> String {
    rows_from(d, rows, 0, 26)
}

/// [`rows`], the descriptions at least `min` columns in; a name longer
/// than `wide` goes on a line of its own.
#[must_use]
pub fn rows_from(d: Depth, rows: &[(String, &str)], min: usize, wide: usize) -> String {
    let width = rows
        .iter()
        .map(|r| r.0.chars().count())
        .filter(|&n| n <= wide)
        .max()
        .unwrap_or(0)
        .max(min);
    let mut out = String::new();
    for (name, desc) in rows {
        let n = name.chars().count();
        out.push_str("  ");
        out.push_str(&term::paint_in(d, Role::Emph, name));
        let mut lines = desc.lines();
        match lines.next() {
            None => {}
            Some(first) if n <= wide => {
                out.push_str(&" ".repeat(width - n + 2));
                out.push_str(first);
            }
            Some(first) => {
                out.push('\n');
                out.push_str(&" ".repeat(width + 4));
                out.push_str(first);
            }
        }
        out.push('\n');
        for l in lines {
            out.push_str(&" ".repeat(width + 4));
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

/// `title:` as a help heading.
#[must_use]
pub fn heading(d: Depth, title: &str) -> String {
    term::paint_in(d, Role::Status, &format!("{title}:"))
}

/// Option sections a caller adds to a command's help: `(title, rows)`.
pub type Extra<'a> = &'a [(&'a str, &'a [(&'a str, &'a str)])];

/// The help of `h` for `tool`, with `extra` option sections after its
/// own.
#[must_use]
pub fn render_help(tool: &str, h: &CmdHelp, extra: Extra<'_>) -> String {
    render_help_in(term::depth(Stream::Stdout), tool, h, extra)
}

#[must_use]
pub fn render_help_in(d: Depth, tool: &str, h: &CmdHelp, extra: Extra<'_>) -> String {
    #[allow(clippy::literal_string_with_formatting_args)] // a placeholder
    let t = |s: &str| s.replace("{tool}", tool);
    let mut o = String::new();
    for (i, u) in h.usage.iter().enumerate() {
        let lead = if i == 0 {
            heading(d, "usage")
        } else {
            " ".repeat(6)
        };
        let _ = writeln!(o, "{lead} {tool} {}", t(u));
    }
    let _ = writeln!(o, "\n{}", t(h.about));
    if tool == "nabic" && !h.nabic_note.is_empty() {
        let _ = writeln!(o, "\n{}", h.nabic_note);
    }
    let nabi_only = if tool == "nabic" {
        &[][..]
    } else {
        h.nabi_examples
    };
    if !h.examples.is_empty() {
        let ex: Vec<(String, &str)> = h
            .examples
            .iter()
            .chain(nabi_only)
            .map(|(c, w)| (t(c), *w))
            .collect();
        let _ = write!(
            o,
            "\n{}\n{}",
            heading(d, "examples"),
            rows_from(d, &ex, 0, 40)
        );
    }
    let own: [(&str, &[(&str, &str)]); 1] = [("options", h.flags)];
    for (title, flags) in own.iter().chain(extra).filter(|(_, f)| !f.is_empty()) {
        let fl: Vec<(String, &str)> = flags.iter().map(|(f, w)| ((*f).to_owned(), *w)).collect();
        let _ = write!(o, "\n{}\n{}", heading(d, title), rows(d, &fl));
    }
    for s in h.sections {
        let lines = usage_section(s);
        let Some((first, rest)) = lines.split_first() else {
            continue;
        };
        let _ = writeln!(o, "\n{}{}", heading(d, s), &first[s.len() + 1..]);
        for l in rest {
            let _ = writeln!(o, "{l}");
        }
    }
    if tool != "nabic" {
        o = o.replace("`nabic ", &format!("`{tool} "));
    }
    let _ = writeln!(o, "\nglobal options (-q, -v, --color, ...): `{tool} help`");
    o
}

/// What `nabic`'s command line takes: its option suggestions, and the
/// options `nabi`'s completion scripts offer for the commands it passes on.
#[must_use]
pub fn nabic_spec() -> Spec {
    let mut s = Spec::parse(
        "nabic",
        NABIC_USAGE,
        |n| n == "commands",
        |n| n == "global options",
    );
    for h in NABIC_COMMANDS {
        for sec in h.sections {
            s.add_flags(h.name, &section_flags(NABIC_USAGE, sec));
        }
    }
    s.values = vec![
        (
            "--target".into(),
            Values::Run("nabic --print target-list".into()),
        ),
        ("help".into(), Values::Commands),
    ];
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_nabic_command_has_help() {
        let spec = nabic_spec();
        for (name, _) in &spec.commands {
            let h = find(NABIC_COMMANDS, name).unwrap_or_else(|| panic!("no help for {name}"));
            assert!(h.usage[0].starts_with(name.as_str()), "{name}");
            assert!(!h.examples.is_empty(), "{name}");
        }
        assert_eq!(spec.commands.len(), NABIC_COMMANDS.len());
    }

    #[test]
    fn own_flags_complete() {
        let spec = nabic_spec();
        for h in NABIC_COMMANDS {
            let sub = format!("{} ", h.name);
            let known: Vec<&String> = spec
                .flags
                .iter()
                .filter(|(k, _)| *k == h.name || k.starts_with(&sub))
                .flat_map(|(_, v)| v)
                .collect();
            for (head, _) in h.flags {
                for f in head.split([',', ' ', '=']).filter(|w| w.starts_with('-')) {
                    assert!(
                        known.iter().any(|k| *k == f) || spec.global.iter().any(|k| k == f),
                        "{} {f}",
                        h.name
                    );
                }
            }
        }
    }

    #[test]
    fn renders_for_either_tool() {
        let h = find(NABIC_COMMANDS, "build").unwrap();
        let nabi = render_help_in(Depth::Off, "nabi", h, &[]);
        assert!(
            nabi.starts_with("usage: nabi build [path] [options]\n\n"),
            "{nabi}"
        );
        assert!(nabi.contains("examples:\n  nabi build "), "{nabi}");
        assert!(nabi.contains("\nbuild options:\n  -o, --out"), "{nabi}");
        assert!(!nabi.contains("nabic "), "{nabi}");
        let nabic = render_help_in(Depth::Off, "nabic", h, &[]);
        assert!(nabic.contains("`nabic help`"), "{nabic}");
        let t = find(NABIC_COMMANDS, "test").unwrap();
        assert!(render_help_in(Depth::Off, "nabic", t, &[]).contains("only builds the harness"));
        assert!(!render_help_in(Depth::Off, "nabi", t, &[]).contains("harness"));
        let painted = render_help_in(Depth::Ansi16, "nabi", h, &[]);
        assert!(
            painted.starts_with("\x1b[1;96musage:\x1b[0m nabi build"),
            "{painted:?}"
        );
    }

    #[test]
    fn suggestions() {
        let names = || NABIC_COMMANDS.iter().map(|h| h.name);
        let dym = |w| did_you_mean(w, names(), CMD_SYNONYMS);
        assert_eq!(dym("biuld"), "; did you mean `build`?");
        assert_eq!(dym("compile"), "; did you mean `build`?");
        assert_eq!(dym("zzzz"), "");
        assert!(looks_like_script("fc.nabi") && looks_like_script("a/b"));
        assert!(!looks_like_script("build"));
    }
}
