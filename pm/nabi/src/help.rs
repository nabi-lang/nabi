//! `nabi help`: the command list, and each command's own help. Commands
//! nabi passes to nabic take their text from nabic's table, written as `nabi`.

use nabic_core::cli::{self, CmdHelp, NABIC_COMMANDS};
use nabic_core::term::{self, Depth, Stream};
use std::fmt::Write as _;

/// nabi's global options, anywhere before `--`.
pub const GLOBAL: &[(&str, &str)] = &[
    (
        "--env <name>",
        "work in that environment (REPL, Jupyter, install)",
    ),
    ("--offline", "never fetch (git dependencies must be cached)"),
    (
        "--locked",
        "fail instead of changing nabi.lock (--frozen: and --offline)",
    ),
    ("--cwd <dir>", "change directory first"),
    (
        "--explain <code>",
        "what an error code (E0001) means, then exit",
    ),
    ("--color <when>", "auto|always|never (--no-color: never)"),
    ("-q, --quiet", "less output"),
    ("-v, --verbose", "more output"),
];

/// nabi's feature options, for the commands that resolve dependencies.
pub const FEATURES: &[(&str, &str)] = &[
    (
        "--features <f,..>",
        "the package's features, or `<dep>/<f>` of a dependency",
    ),
    ("--all-features", "every feature of the package"),
    ("--no-default-features", "not its `default` feature"),
];

/// The commands that take the feature options.
pub const WITH_FEATURES: [&str; 14] = [
    "build", "check", "run", "dev", "test", "bench", "lint", "fix", "doc", "emit", "repl", "lsp",
    "jupyter", "deps",
];

/// The member options of a workspace (`crate::workspace::COMMANDS`).
pub const MEMBERS: &[(&str, &str)] = &[
    (
        "-p, --package <name>",
        "in a workspace: that member (repeat for more)",
    ),
    (
        "--workspace",
        "in a workspace: every member (the default at its root)",
    ),
];

/// The runner's options of `nabi test` / `nabi bench`: their help heading
/// and rows.
#[must_use]
pub fn runner_options(
    cmd: &str,
) -> Option<(&'static str, &'static [(&'static str, &'static str)])> {
    match cmd {
        "test" => Some(("test options", nabic_harness::TEST_OPTIONS)),
        "bench" => Some(("bench options", nabic_harness::BENCH_OPTIONS)),
        _ => None,
    }
}

/// The command list of `nabi --help`, in groups.
const GROUPS: &[(&str, &[&str])] = &[
    (
        "packages",
        &[
            "new", "init", "add", "remove", "update", "deps", "search", "audit",
        ],
    ),
    (
        "building",
        &[
            "build", "run", "dev", "check", "test", "bench", "lint", "fix", "fmt", "doc", "clean",
            "emit", "repl",
        ],
    ),
    (
        "installing and publishing",
        &["install", "uninstall", "publish", "yank", "login", "logout"],
    ),
    (
        "toolchains and tools",
        &[
            "toolchain",
            "cache",
            "component",
            "env",
            "jupyter",
            "lsp",
            "completions",
            "cc",
            "bindgen",
            "inspect",
            "demangle",
            "version",
            "help",
        ],
    ),
];

/// The nabic commands nabi runs; their help is nabic's.
const FROM_NABIC: [&str; 6] = ["build", "check", "test", "bench", "emit", "help"];

/// nabi's own commands.
pub const NABI_COMMANDS: &[CmdHelp] = &[
    CmdHelp {
        name: "new",
        summary: "create a package",
        usage: &["new [--lib] [--bin] <name>"],
        about: "\
Create the directory <name> with nabi.nabion and src/main.nabi (--bin, the
default), src/lib.nabi (--lib) or both, and make it a git repository.",
        examples: &[
            ("nabi new hello", "a program: hello/src/main.nabi"),
            ("nabi new geom --lib", "a library: geom/src/lib.nabi"),
        ],
        flags: &[
            ("--bin", "src/main.nabi (the default)"),
            ("--lib", "src/lib.nabi"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "init",
        summary: "make a directory a package",
        usage: &["init [--lib] [--bin] [dir]", "init --script <file.nabi>"],
        about: "\
Make <dir> (default: the current one) a package, as `nabi new` does. With
--script, a script's directory becomes a package: its //! ```nabion blocks
become nabi.nabion and its files move under src/, the script being
src/main.nabi.",
        examples: &[
            ("nabi init", "the current directory"),
            (
                "nabi init --script tool.nabi",
                "turn a script into a package",
            ),
        ],
        flags: &[
            ("--bin", "src/main.nabi (the default)"),
            ("--lib", "src/lib.nabi"),
            ("--script <file.nabi>", "from a script"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "add",
        summary: "add a dependency",
        usage: &[
            "add <name>[@<req>] [options]",
            "add <name> --path <dir> [options]",
            "add <name> --git <url> [--rev|--tag|--branch <r>] [options]",
            "add --native <name>[@<req>] [--registry <url>]",
            "add ... <file.nabi>",
        ],
        about: "\
Add a dependency to nabi.nabion and resolve it: a package from the index
(default requirement: ^ its newest version), a directory or a git
repository. Naming a script last adds it to the script's //! ```nabion
block instead (made at the top when missing).",
        examples: &[
            ("nabi add json", "the newest json package"),
            ("nabi add json@^0.1", "a version requirement"),
            ("nabi add geom --path ../geom", "a package beside this one"),
            ("nabi add json tool.nabi", "into a script's block"),
        ],
        flags: &[
            (
                "--registry <url>",
                "another index (default: packages.nabi-lang.org;\na mirror of it goes in \
                 $NABI_HOME/sources.nabion)",
            ),
            ("--path <dir>", "a local package"),
            ("--git <url>", "a git repository"),
            ("--rev, --tag, --branch <r>", "which commit of it"),
            ("--native", "a native package (prebuilt C library)"),
            ("--dev", "to dev_deps (tests and benchmarks only)"),
            ("--features <f,..>", "features of the dependency to enable"),
            ("--no-default-features", "not its `default` feature"),
            ("--optional", "enabled by a new feature of its name"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "remove",
        summary: "remove a dependency",
        usage: &["remove [--native] <name> [<file.nabi>]"],
        about: "\
Remove a dependency from nabi.nabion, or from a script's //! ```nabion block
when a script is named.",
        examples: &[
            ("nabi remove json", ""),
            ("nabi remove json tool.nabi", "from a script's block"),
        ],
        flags: &[("--native", "a native package")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "update",
        summary: "update dependencies in nabi.lock",
        usage: &["update [name...]"],
        about: "\
Resolve git and index dependencies again, all or the named ones, and
rewrite nabi.lock.",
        examples: &[("nabi update", "everything"), ("nabi update json", "one")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "deps",
        summary: "print the resolved dependencies",
        usage: &["deps [--dev] [--format json] [feature options]"],
        about: "\
Print the resolved dependencies as name=lib.nabi, each after its own
dependencies, then a line per other name its dependents call it, then
its features (--extern-feature=<name>=<f>).
Native packages print as `# native <name> <version>` and their compiler
arguments. --format json prints one JSON object per line instead: a
package or native record each, then a summary.",
        examples: &[
            ("nabi deps", ""),
            ("nabi deps --dev", "with the dev dependencies"),
            ("nabi deps --format json", "for tools"),
        ],
        flags: &[
            ("--dev", "with dev_deps"),
            ("--format <human|json>", "the output form (default human)"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "search",
        summary: "search the index for packages",
        usage: &["search [--native] <query>"],
        about: "The packages (or native packages) whose name contains <query>.",
        examples: &[("nabi search json", "")],
        flags: &[("--native", "native packages")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "audit",
        summary: "check nabi.lock against security advisories",
        usage: &["audit [--json] [--ignore <id>].. [--offline]"],
        about: "\
Check every registry package in nabi.lock (official ones the toolchain
carries too) against the security advisories of its registry, and say
what to upgrade to. Path and git dependencies and native packages are
not checked. The advisory list is fetched once and kept for an hour;
--offline uses that copy at any age. Exit status: 0 none apply, 1 some
do, 2 the check could not run. --json (--format json) prints one JSON
object per line: an advisory record per finding, a skipped record per
package not checked, then a summary.",
        examples: &[
            ("nabi audit", ""),
            ("nabi audit --ignore NABI-2026-0001", "accept one advisory"),
            ("nabi audit --json", "for tools and CI"),
        ],
        flags: &[
            ("--json", "JSON records (same as --format json)"),
            (
                "--ignore <id>",
                "skip an advisory by id or alias (repeat for more)",
            ),
            ("--offline", "use only the cached advisory list"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "install",
        summary: "install an executable or a library",
        usage: &[
            "install <name>[@<req>] [--registry <url>] [options]",
            "install [<name>] --path <dir | file.nabi> [options]",
            "install [<name>] --git <url> [--rev|--tag|--branch <r>] [options]",
            "install --native <name>[@<req>]",
            "install --list",
        ],
        about: "\
An executable package (src/main.nabi) is built --release into
$NABI_HOME/bin; a library (src/lib.nabi only, or with --lib) goes into the
active environment, for `nabi repl` and `nabi jupyter`; a package with
both needs --bin or --lib to say which. A script file is built into
$NABI_HOME/bin. An executable already installed from the same index
version or git commit is kept unless --force; one from a path is always
rebuilt.",
        examples: &[
            ("nabi install fmtx", "an executable package"),
            ("nabi install --path .", "the package here"),
            ("nabi install json --lib", "a library for the REPL"),
            ("nabi install --list", "what is installed"),
        ],
        flags: &[
            ("--registry <url>", "another index"),
            ("--path <dir | file.nabi>", "a local package or script"),
            ("--git <url>", "a git repository"),
            ("--rev, --tag, --branch <r>", "which commit of it"),
            ("--features <f,..>", "features to enable"),
            ("--no-default-features", "not its `default` feature"),
            ("--lib", "install the library, not the executable"),
            ("--bin", "install the executable, not the library"),
            ("--force", "rebuild even if installed"),
            ("--native", "a native package into the environment"),
            ("--list", "the environment's libraries and the executables"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "uninstall",
        summary: "remove installed executables or libraries",
        usage: &["uninstall [--native] <name>..."],
        about: "\
Remove each from the active environment, or else from $NABI_HOME/bin.",
        examples: &[("nabi uninstall fmtx", "")],
        flags: &[("--native", "a native package")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "publish",
        summary: "publish this package to the index",
        usage: &[
            "publish [--dry-run] [--registry <url> | --index-dir <dir>]",
            "publish --native <file> [--dry-run] [--registry <url> | --index-dir <dir>]",
        ],
        about: "\
Make the index entry for this package: its git remote `origin` (an
https:// URL), HEAD commit and tree, dependencies, features, `toolchain`
(the pin, else this nabi's series) and `links`; sources are not uploaded.
It is committed to the index checkout <dir>, or without --index-dir sent
to the registry API of packages.nabi-lang.org or of --registry <url>,
with the manifest's descriptive fields, the README and the packed size (log
in first with `nabi login`, or set $NABI_REGISTRY_TOKEN for packages.nabi-lang.org).
--dry-run only checks it, against <dir> if given.",
        examples: &[
            ("nabi publish --dry-run", "check what would be published"),
            ("nabi publish", "publish it"),
        ],
        flags: &[
            ("--dry-run", "only check"),
            ("--registry <url>", "another index than the default"),
            ("--index-dir <dir>", "a checkout of the index"),
            (
                "--native <file>",
                "the native package `Native { .. }` in <file>",
            ),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "yank",
        summary: "mark a version yanked",
        usage: &[
            "yank [--native] <name>@<version> [--undo] [--message <why>] [--registry <url> | \
             --index-dir <dir>]",
        ],
        about: "\
Mark a version yanked, so new resolutions skip it: committed to the index
checkout <dir>, or through the registry API of packages.nabi-lang.org or of
--registry <url> (log in with `nabi login`), which keeps --message.",
        examples: &[
            ("nabi yank num@1.2.0", "yank num 1.2.0"),
            (
                "nabi yank num@1.2.0 --message \"wrong sums\"",
                "and say why",
            ),
            ("nabi yank num@1.2.0 --undo", "take it back"),
        ],
        flags: &[
            ("--undo", "un-yank it"),
            ("--message <why>", "why it is yanked (at most 512 bytes)"),
            ("--registry <url>", "another index than the default"),
            ("--index-dir <dir>", "a checkout of the index"),
            ("--native", "a native package"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "login",
        summary: "keep a registry token",
        usage: &["login [--registry <url>] [<token>]"],
        about: "\
Keep a registry token for one index (default: packages.nabi-lang.org) in
$NABI_HOME/tokens.nabion, after checking its account may publish; it is
sent only to that index. Get one from <api>/me (e.g.
https://packages.nabi-lang.org/me); without <token> it is read from standard input.",
        examples: &[
            ("nabi login", "paste the token"),
            (
                "nabi login --registry https://example.com/index",
                "a token for another index",
            ),
        ],
        flags: &[("--registry <url>", "another index than the default")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "logout",
        summary: "remove the registry token",
        usage: &["logout [--registry <url> | --all]"],
        about: "\
Remove the token `nabi login` kept for the index (default: packages.nabi-lang.org),
or with --all every token.",
        examples: &[
            ("nabi logout", ""),
            ("nabi logout --all", "every index's token"),
        ],
        flags: &[
            ("--registry <url>", "another index than the default"),
            ("--all", "every index"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "env",
        summary: "named environments",
        usage: &[
            "env create|remove|use <name>",
            "env list",
            "env export <name> <dir>",
            "env import <name> <dir | nabi.nabion>",
            "env shell [name]",
        ],
        about: "\
An environment holds the libraries `nabi install` puts there, for the REPL
and Jupyter. The active one is --env, else
$NABI_ENV, else the one `nabi env use` chose, else `default`. `list` marks
the active one with `*`; `shell` starts $SHELL with NABI_ENV=<name>.",
        examples: &[
            ("nabi env create sci", "a new environment"),
            ("nabi env use sci", "make it the active one"),
            (
                "nabi env export sci out/",
                "its nabi.nabion and nabi.lock into out/",
            ),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "cache",
        summary: "the toolchain's build cache",
        usage: &[
            "cache dir",
            "cache prune [--unused <days>] [--max-size <size>]",
            "cache clean",
        ],
        about: "\
Builds keep the standard library code they compile in the toolchain's
cache, by content, so later builds of any package take it from there.
Past its cap (NABI_CACHE_MAX_SIZE, default 5G; `off`: none) a build
removes the least recently used entries down to 80% of it.
`dir` prints where it is. `prune` removes what no build used for
--unused days (default 30; builds do that themselves once a day), then
the least recently used past --max-size (bytes, or with K, M or G;
default the cap). `clean` empties it; the next builds fill it again.",
        examples: &[
            ("nabi cache prune --max-size 1G", "at most 1 GiB left"),
            ("nabi cache clean", "empty it"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "component",
        summary: "optional parts (the official packages, offline)",
        usage: &[
            "component list",
            "component add packages [--bundle <dir>]",
            "component update [packages]",
            "component remove packages",
        ],
        about: "\
`packages` is the registry archive of the newest version (that this toolchain
builds) of every official package, so they resolve and come in on use without
a network. A release toolchain carries them; `add` downloads them into the
cache when it does not, `update` fetches newer ones, `remove` deletes them
(the toolchain's too). Manifests and locks still pick the versions, the
toolchain's first. `--bundle <dir>` writes them as a toolchain's
`lib/nabi/packages` (release builds).",
        examples: &[
            ("nabi component list", "whether the toolchain carries them"),
            ("nabi --offline run", "build with no network"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "toolchain",
        summary: "install and pick toolchains",
        usage: &[
            "toolchain list",
            "toolchain install <version> [--from <dir | .tar[.gz] | .zip | url>] [--key <file>]",
            "toolchain default [<version>]",
            "toolchain remove <version>",
            "toolchain clean",
            "toolchain update [<version>] [--key <file>]",
        ],
        about: "\
Toolchains live in $NABI_HOME/toolchains/<version>-<triple>/ ($NABI_HOME:
default ~/.nabi); the $NABI_HOME/bin proxies run the default one. `update`
installs the newest release, or <version>, from $NABI_DIST (default:
https://github.com/nabi-lang/nabi/releases), checked against the release's
SHA256SUMS once its signature verifies with the nabi release key (--key
<file>: a minisign public key file instead, with a warning), and makes it
the default; it is rolled back if the proxies then do not
run it. For `update` and `install`, <version> is a full version (0.1.0).
`install` without --from downloads one and keeps the default. `clean`
removes every toolchain but the default. A first argument +<version> runs
one command with that toolchain, as does $NABI_TOOLCHAIN, a script's or
nabi.nabion's `toolchain`.",
        examples: &[
            ("nabi toolchain update", "the newest release"),
            ("nabi toolchain update 0.1.0", "exactly 0.1.0"),
            (
                "nabi toolchain install 0.1.0",
                "download 0.1.0 beside the default",
            ),
            ("nabi toolchain default 0.1.0", "switch"),
            ("nabi +0.1.0 build", "one build with 0.1.0"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "jupyter",
        summary: "the Jupyter kernel",
        usage: &[
            "jupyter install [--env <name>]",
            "jupyter --connection-file <file>",
        ],
        about: "\
`install` installs the `nabi` Jupyter kernel; with --env it is `nabi-<env>`,
running in that environment. Jupyter itself runs the kernel with
--connection-file.",
        examples: &[
            ("nabi jupyter install", "the `nabi` kernel"),
            ("nabi --env sci jupyter install", "the `nabi-sci` kernel"),
        ],
        flags: &[("--connection-file <file>", "the file Jupyter wrote")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "repl",
        summary: "start an interactive session",
        usage: &["repl [path] [options]"],
        about: "\
Each cell is compiled and run. In the session, :type <expr> shows a type,
:reset starts over, :exit leaves and :add <dep> adds a dependency (`nabi
add` in a package, else `nabi install`). A program path runs in the
session first; a library is loaded and `use`d. Outside a package the
session uses the active environment's libraries (see `nabi help env`).
Bare `nabi` is `nabi repl`.",
        examples: &[
            ("nabi", "an empty session"),
            ("nabi repl src/main.nabi", "start from a program's bindings"),
            ("nabi --env sci repl", "with the sci environment"),
        ],
        flags: &[("-q, --quiet", "no banner")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "run",
        summary: "build, then run the program",
        usage: &["run [path] [options] [-- args]"],
        about: "\
Build the executable, then run it. Arguments after `--` go to the program,
and the exit code is the program's.",
        examples: &[
            ("{tool} run", "run the package here"),
            ("{tool} run hello.nabi", "run one file"),
            (
                "{tool} run --release -- in.txt",
                "optimized, with in.txt as argument",
            ),
        ],
        sections: &["build options"],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "dev",
        summary: "run the program, applying edits while it runs",
        usage: &["dev [path] [-p <member>] [--bin <name>] [options] [-- args]"],
        about: "\
Build the program in hot mode and run it. Each save rebuilds the changed
local packages (the package and its path dependencies) and the program
loads them at its next safepoint, keeping its state; a compile error keeps
the old code running. A change to nabi.nabion, nabi.lock or the toolchain
restarts the program. Press `r` to restart and `q` to quit; the program's
standard input is empty. Debug builds only.",
        examples: &[
            ("{tool} dev", "run the package here"),
            ("{tool} dev -p game", "run workspace member game"),
            ("{tool} dev -- level1", "with level1 as argument"),
        ],
        flags: &[
            ("-p, --package <name>", "in a workspace: that member"),
            ("--bin <name>", "the executable (the package's own)"),
        ],
        sections: &["build options"],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "lint",
        summary: "run the lints",
        usage: &["lint [path] [options]"],
        about: "Run the front end and the lints. Exits with 1 if a `deny` lint fires.",
        examples: &[
            ("{tool} lint", "lint the package here"),
            ("{tool} lint -D unused", "make the `unused` lints errors"),
            ("{tool} lint --list", "every lint and its level"),
        ],
        flags: &[
            ("--list", "every lint and its level"),
            (
                "--check-library",
                "check every library body, reached or not",
            ),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "fix",
        summary: "apply the lints' and warnings' fixes",
        usage: &["fix [path] [options]"],
        about: "\
Check the package and apply every fix a warning or lint carries (one
sure edit, such as `while true` -> `loop` or deleting an unused `use`),
again until none is left. The work tree must have no uncommitted changes,
so the fixes can be reviewed and undone with git. Errors stop it: fix
those by hand first.",
        examples: &[
            ("{tool} fix", "fix the package here"),
            ("{tool} fix --diff", "print the fixes, change nothing"),
            ("{tool} fix -A unused_imports", "keep the unused imports"),
        ],
        flags: &[
            ("--allow-dirty", "fix even with uncommitted changes"),
            (
                "--diff",
                "print the fixes as a diff instead of writing them",
            ),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "lsp",
        summary: "run the language server",
        usage: &["lsp"],
        about: "\
The language server, on standard input and output. Editors start it;
`nabils` is the same server. `--stdio`, which many editors pass, is
accepted and changes nothing.",
        examples: &[("{tool} lsp", "serve on stdio")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "fmt",
        summary: "format source files",
        usage: &["fmt [path] [options]"],
        about: "\
Format .nabi and .nabion files in place: <path> is one file, or a directory
whose files are all formatted (default: the package the current directory
is in).",
        examples: &[
            ("{tool} fmt", "format everything here"),
            ("{tool} fmt --check", "exit 1 if a file would change (CI)"),
            (
                "{tool} fmt --diff src/main.nabi",
                "show the change, write nothing",
            ),
        ],
        flags: &[
            ("--check", "exit 1 if a file would change"),
            ("--diff", "show the changes, write nothing"),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "doc",
        summary: "build the API docs",
        usage: &["doc [path] [options]"],
        about: "\
HTML docs of the package's library (else of its executable) and of its
dependencies, in target/doc/.",
        examples: &[
            ("{tool} doc", "docs of the package here"),
            ("{tool} doc . --std", "with the standard library"),
            ("{tool} doc --std", "the standard library alone"),
            ("{tool} doc --open", "then open them in a browser"),
        ],
        flags: &[
            (
                "--std",
                "document the standard library too; without a path, only\nit, into ./target/doc/",
            ),
            (
                "--open",
                "open the package's root page in $BROWSER or the system's\nbrowser",
            ),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "clean",
        summary: "remove target/",
        usage: &["clean [path]"],
        about: "Remove the package's target/ directory.",
        examples: &[("{tool} clean", "remove ./target")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "cc",
        summary: "compile C with Nabi's own C compiler",
        usage: &["cc [gcc options] <file>..."],
        about: "\
Compile and link C with the toolchain's C compiler, `nabicc` (the same
program; `make CC=\"nabi cc\"` and `make CC=nabicc` both work, as does
CMake).
It takes gcc's and clang's options: -c, -S, -E, -o, -I, -D, -U, -std=,
-O, -g, -l, -L, -Wl, the -M family and --target. No other C compiler
is used: code comes from Nabi's backend and links with Nabi's linker.",
        examples: &[
            (
                "{tool} cc -O2 hello.c -o hello",
                "compile and link a program",
            ),
            (
                "{tool} cc -c --target=x86_64-unknown-linux-gnu z.c",
                "an object for Linux",
            ),
            (
                "{tool} cc -M -MF dep.d z.c",
                "a make rule of what z.c includes",
            ),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "bindgen",
        summary: "Nabi declarations of a C header",
        usage: &["bindgen <header> [options] [-- <C compiler args>]"],
        about: "\
Write the Nabi declarations of a C header for the --target, read by
the compiler's own C front end as the target's C compiler lays it out: no
other C compiler is needed. C compiler arguments go after `--`: -I <dir>,
-isystem <dir>, -iquote <dir>, -idirafter <dir>, -D <name>[=<value>],
-U <name>, -include <file>, --sysroot <dir>, -isysroot <dir>, -nostdinc,
-nostdlibinc, -nobuiltininc.",
        examples: &[
            (
                "{tool} bindgen zlib.h -o src/zlib.nabi",
                "declarations into a file",
            ),
            (
                "{tool} bindgen app.h -- -I include",
                "with an include directory",
            ),
            (
                "{tool} bindgen -x objective-c $SDK/System/Library/Frameworks/Metal.framework/Headers/Metal.h",
                "Objective-C classes as `extern \"objc\"` blocks",
            ),
        ],
        flags: &[
            ("-o <file>", "write to <file> (default: stdout)"),
            (
                "-x objective-c",
                "read Objective-C: classes, protocols and categories too (Apple targets)",
            ),
            ("--target=<target>", "the target the declarations are for"),
            (
                "--c-shim <file>",
                "C wrappers of `static inline` functions to <file> (add it to `c.sources`)",
            ),
        ],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "demangle",
        summary: "decode `_Y` symbols",
        usage: &["demangle [-v] <symbol>...", "demangle"],
        about: "\
Print each `_Y` symbol's name as source writes it, one a line
(`core::collections::dict::Dict::grow`); the Mach-O form with an
extra leading `_` is accepted. With -v, the package, path, name and
generics each encodes. With no symbol, copy stdin to stdout with every
Nabi symbol in it replaced by its name, as a filter for profiler, nm and
crash output.",
        examples: &[
            (
                "{tool} demangle _Y3f8a1c2b_std_vec_push_a7c91d8e44b2f190",
                "std::vec::push",
            ),
            (
                "{tool} demangle < profile.txt",
                "names in text, as `perf report --stdio | nabi demangle`",
            ),
        ],
        flags: &[("-v, --verbose", "the parts of each symbol, a block each")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "inspect",
        summary: "describe a prebuilt library",
        usage: &["inspect [--dump] <file>"],
        about: "\
Print a prebuilt library's package, exports, generic items and archive
members; with --dump, its whole NABIHIR as text. <file> is <name>.nabihir
or the lib<name>.a beside it.",
        examples: &[
            ("{tool} inspect target/debug/libgeom.a", ""),
            (
                "{tool} inspect --dump target/debug/libgeom.a",
                "the whole NABIHIR",
            ),
        ],
        flags: &[("--dump", "the whole NABIHIR as text")],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "completions",
        summary: "print a shell completion script",
        usage: &["completions bash|zsh|fish|powershell"],
        about: "\
Print the completion script for that shell on standard output. install.sh
and install.ps1 put it in place; `nabi toolchain default` and `update`
rewrite it.",
        examples: &[(
            "nabi completions fish > ~/.config/fish/completions/nabi.fish",
            "",
        )],
        ..CmdHelp::EMPTY
    },
    CmdHelp {
        name: "version",
        summary: "print the version",
        usage: &["version [-v] [--format json]"],
        about: "\
Print the version (also `nabi --version`); with -v, also where nabi and nabic
are, the toolchain and the host, as `key: value` lines. --format json prints
one JSON object instead.",
        examples: &[
            ("nabi version -v", ""),
            ("nabi version -v --format json", "for tools"),
        ],
        flags: &[("--format <human|json>", "the output form (default human)")],
        ..CmdHelp::EMPTY
    },
];

/// The help of nabi command `cmd`: its own, else nabic's.
#[must_use]
pub fn find(cmd: &str) -> Option<&'static CmdHelp> {
    cli::find(NABI_COMMANDS, cmd).or_else(|| {
        FROM_NABIC
            .contains(&cmd)
            .then(|| cli::find(NABIC_COMMANDS, cmd))
            .flatten()
    })
}

/// Every command `nabi help` lists, in its order.
pub fn all() -> impl Iterator<Item = &'static CmdHelp> {
    GROUPS
        .iter()
        .flat_map(|(_, names)| names.iter())
        .filter_map(|n| find(n))
}

/// `nabi help <cmd>`.
#[must_use]
pub fn command(cmd: &str) -> Option<String> {
    let h = find(cmd)?;
    let mut extra: Vec<(&str, &[(&str, &str)])> = runner_options(cmd).into_iter().collect();
    if WITH_FEATURES.contains(&cmd) {
        extra.push(("feature options (nabi.nabion's `features`)", FEATURES));
    }
    if crate::workspace::COMMANDS.contains(&cmd) {
        extra.push(("workspace options", MEMBERS));
    }
    Some(cli::render_help("nabi", h, &extra))
}

/// `nabi --help`: the commands, one line each, and the global options.
#[must_use]
pub fn short_usage() -> String {
    short_usage_in(term::depth(Stream::Stdout))
}

#[must_use]
pub fn short_usage_in(d: Depth) -> String {
    let mut o = format!(
        "{} nabi [options] <command> [args]
       nabi [options] <file.nabi> [args]  run a script
       nabi [options]                     start the REPL
",
        cli::heading(d, "usage")
    );
    for (group, names) in GROUPS {
        let rows: Vec<(String, &str)> = names
            .iter()
            .filter_map(|n| find(n))
            .map(|h| (h.name.to_owned(), h.summary))
            .collect();
        let _ = write!(
            o,
            "\n{}\n{}",
            cli::heading(d, group),
            cli::rows_from(d, &rows, 14, 26)
        );
    }
    let rows: Vec<(String, &str)> = GLOBAL
        .iter()
        .map(|(f, w)| ((*f).to_owned(), *w))
        .chain([("+<version>".to_owned(), "(first) use that toolchain")])
        .collect();
    let _ = write!(
        o,
        "\n{}\n{}",
        cli::heading(d, "options (anywhere before `--`)"),
        cli::rows_from(d, &rows, 14, 26)
    );
    o.push_str("\none command's help: `nabi help <command>`, e.g. `nabi help build`\n");
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_command_has_help() {
        for (_, names) in GROUPS {
            for n in *names {
                let h = find(n).unwrap_or_else(|| panic!("no help for {n}"));
                assert_eq!(h.name, *n);
                assert!(h.usage[0].starts_with(n), "{n}");
                assert!(!h.examples.is_empty(), "{n}");
                for (ex, _) in h.examples {
                    assert!(ex.starts_with("nabi") || ex.starts_with("{tool}"), "{ex}");
                }
            }
        }
    }

    #[test]
    fn short_and_plain() {
        let u = short_usage_in(Depth::Off);
        assert!(u.lines().count() < 66, "{u}");
        assert!(u.contains("\n  build  ") && !u.contains("nabic"), "{u}");
    }
}
