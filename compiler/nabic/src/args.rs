//! The command line: option parsing, help and usage errors.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// `nabic <cmd> --help` (`nabi <cmd> --help` under nabi).
pub fn command_help(cmd: &str) -> String {
    let Some(h) = cli::find(NABIC_COMMANDS, cmd) else {
        usage_error(&format!("unknown command `{cmd}`{}", suggest_command(cmd)));
    };
    cli::render_help(cli::invoked_as(), h, &[])
}

/// "; did you mean ..?" for an unknown command: `build <file>` for a
/// script, else the closest command.
pub fn suggest_command(cmd: &str) -> String {
    if cli::looks_like_script(cmd) {
        return format!("; did you mean `{} build {cmd}`?", cli::invoked_as());
    }
    cli::did_you_mean(
        cmd,
        NABIC_COMMANDS.iter().map(|h| h.name),
        cli::CMD_SYNONYMS,
    )
}

/// "; did you mean ..?" for an unknown option of `cmd`.
pub fn suggest_flag(cmd: &str, flag: &str) -> String {
    let spec = cli::nabic_spec();
    let sub = format!("{cmd} ");
    let own = spec
        .flags
        .iter()
        .filter(|(k, _)| cmd.is_empty() || *k == cmd || k.starts_with(&sub))
        .flat_map(|(_, v)| v);
    let cands: Vec<&str> = own.chain(&spec.global).map(String::as_str).collect();
    cli::did_you_mean(flag, cands, &[])
}

/// A usage error (exit 2), named as the program the user ran, and where
/// to read the usage.
pub fn usage_error(msg: &str) -> ! {
    let tool = cli::invoked_as();
    let msg = msg.replace("usage: nabic ", &format!("usage: {tool} "));
    eprintln!("{tool}: {msg}\n{}", read_usage());
    process::exit(2);
}

/// "run `nabic help build` for usage"; under `nabi`, the command may be
/// one of its tools.
fn read_usage() -> String {
    let tool = cli::invoked_as();
    match CMD
        .get()
        .filter(|c| tool == "nabi" || cli::find(NABIC_COMMANDS, c).is_some())
    {
        Some(c) => format!("run `{tool} help {c}` for usage"),
        None => format!("run `{tool} help` for usage"),
    }
}

/// `--message-format json` seen by `early_style`.
static JSON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// An option given a value it does not take (E0132, exit 2), with what to
/// write instead.
pub fn option_error(msg: impl Into<String>, help: impl Into<String>) -> ! {
    let d = nabic_interface::NabiDiagnostic::error(
        nabic_interface::Code::INVALID_OPTION,
        nabic_interface::Span::DUMMY,
        msg,
    )
    .with_help(help);
    print_rendered(&SourceMap::default().render(&d));
    if !JSON.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("{}", read_usage());
    }
    process::exit(2);
}

/// Colour and message format before the options are read, so an option
/// error is reported in them wherever they stand.
fn early_style(raw: &[String]) {
    let mut color = ColorChoice::Auto;
    let mut json = false;
    let mut it = raw.iter().map(String::as_str);
    while let Some(arg) = it.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (arg, None),
        };
        match name {
            "--" => break,
            "--no-color" => color = ColorChoice::Never,
            "--color" => {
                if let Some(c) = inline.or_else(|| it.next()).and_then(ColorChoice::parse) {
                    color = c;
                }
            }
            "--message-format" => json = inline.or_else(|| it.next()) == Some("json"),
            _ => {}
        }
    }
    JSON.store(json, std::sync::atomic::Ordering::Relaxed);
    term::set_choice(color);
    nabic_interface::set_report_depth(json, term::depth(Stream::Stderr));
}

pub fn fail(msg: &str) -> ! {
    eprintln!(
        "{}: {} {msg}",
        cli::invoked_as(),
        term::label(Stream::Stderr, Role::Error)
    );
    process::exit(1);
}

/// `raw` up to `--`, then the program's arguments `tail` (`cli::os_args`).
pub fn parse_args(raw: Vec<String>, mut tail: Vec<OsString>) -> Args {
    early_style(&raw);
    let mut it = raw.into_iter();
    let mut a = Args::default();
    // Value of `--name=value` or the next argument for `--name value`.
    let value = |name: &str, inline: Option<String>, it: &mut std::vec::IntoIter<String>| {
        inline.or_else(|| it.next()).unwrap_or_else(|| {
            option_error(
                format!("`{name}` needs a value"),
                if name.starts_with("--") {
                    format!("write `{name} <value>` or `{name}=<value>`")
                } else {
                    format!("write `{name} <value>`")
                },
            )
        })
    };
    while let Some(arg) = it.next() {
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n.to_owned(), Some(v.to_owned())),
            _ => (arg.clone(), None),
        };
        match name.as_str() {
            "--" => {
                a.run_args = it.by_ref().map(OsString::from).collect();
                a.run_args.append(&mut tail);
            }
            // Global options, accepted before or after the command.
            "-v" | "--verbose" => a.verbose += 1,
            "-vv" => a.verbose += 2,
            "-q" | "--quiet" => a.quiet = true,
            "--no-color" => a.color = ColorChoice::Never,
            "--color" => {
                let v = value(&name, inline, &mut it);
                a.color = ColorChoice::parse(&v).unwrap_or_else(|| {
                    option_error(
                        format!("`--color` takes auto, always or never, not `{v}`"),
                        "write `--color=auto`, `--color=always` or `--color=never`",
                    )
                });
            }
            "--cwd" => {
                let dir = value(&name, inline, &mut it);
                if let Err(e) = std::env::set_current_dir(&dir) {
                    option_error(
                        format!("`--cwd {dir}`: {e}"),
                        "name a directory that exists: `--cwd <dir>`",
                    );
                }
            }
            "--message-format" => {
                a.json = match value(&name, inline, &mut it).as_str() {
                    "json" => true,
                    "human" => false,
                    v => option_error(
                        format!("unknown message format `{v}`"),
                        "write `--message-format=human` or `--message-format=json`",
                    ),
                };
            }
            "-o" | "--output" => a.output = Some(value(&name, inline, &mut it).into()),
            "--bootstrap" => a.bootstrap.push(value(&name, inline, &mut it).into()),
            "--cfg" => a.cfg.push(value(&name, inline, &mut it)),
            "--print" => a.print = Some(value(&name, inline, &mut it)),
            "--explain" => a.explain = Some(value(&name, inline, &mut it)),
            "--extern" => a.externs.push(parse_extern(&value(&name, inline, &mut it))),
            "--extern-feature" => {
                let v = value(&name, inline, &mut it);
                let Some((dep, f)) = v
                    .split_once('=')
                    .filter(|(d, f)| !d.is_empty() && !f.is_empty())
                else {
                    option_error(
                        format!("`--extern-feature {v}`: expected <name>=<feature>"),
                        "name the dependency and one of its features: \
                         `--extern-feature json=std`",
                    );
                };
                a.extern_features
                    .push((dep.replace('-', "_"), f.to_owned()));
            }
            "--extern-dep" => {
                let v = value(&name, inline, &mut it);
                let Some((dep, n)) = v
                    .split_once('=')
                    .filter(|(d, n)| !d.is_empty() && !n.is_empty())
                else {
                    option_error(
                        format!("`--extern-dep {v}`: expected <package>=<name>"),
                        "name the package (`pkg`: the one being built) and the extern it \
                         depends on: `--extern-dep pkg=json`",
                    );
                };
                a.extern_deps
                    .push((dep.replace('-', "_"), n.replace('-', "_")));
            }
            "--name" => a.name = Some(value(&name, inline, &mut it)),
            "--target-dir" => a.target_dir = Some(value(&name, inline, &mut it).into()),
            "--emit-harness" => a.emit_harness = Some(value(&name, inline, &mut it).into()),
            "--c-shim" => a.c_shim = Some(value(&name, inline, &mut it).into()),
            "-x" | "--language" => match value(&name, inline, &mut it).as_str() {
                "c" => a.objc = false,
                "objective-c" | "objc" => a.objc = true,
                v => option_error(
                    format!("unknown header language `{v}`"),
                    "write `-x c` (the default) or `-x objective-c`",
                ),
            },
            "--crate-type" => match value(&name, inline, &mut it).as_str() {
                "lib" => (a.cdylib, a.plugin, a.staticlib) = (false, false, false),
                "cdylib" => (a.cdylib, a.plugin, a.staticlib) = (true, false, false),
                "staticlib" => (a.cdylib, a.plugin, a.staticlib) = (false, false, true),
                "plugin" => (a.cdylib, a.plugin, a.staticlib) = (false, true, false),
                v => option_error(
                    format!("unknown crate type `{v}`"),
                    "write `--crate-type=lib` (a static library, the default), \
                     `--crate-type=cdylib` (a shared library C code loads), \
                     `--crate-type=staticlib` (a static library C code links alone) or \
                     `--crate-type=plugin` (a shared library a Nabi program loads)",
                ),
            },
            "--bundle" => a
                .bundle
                .push(value(&name, inline, &mut it).replace('-', "_")),
            "--hot" => (a.hot, a.cdylib, a.plugin, a.staticlib) = (true, false, true, false),
            "--xcframework" => a.xcframework = true,
            "--hot-base" => a.hot_base = Some(value(&name, inline, &mut it).into()),
            "--hot-dep" => a
                .hot_deps
                .push(value(&name, inline, &mut it).replace('-', "_")),
            "--target" => a.target = Some(value(&name, inline, &mut it)),
            "--nabir" => a.opts.nabir = value(&name, inline, &mut it),
            "-L" => a.opts.lib_dirs.push(value(&name, inline, &mut it).into()),
            s if s.starts_with("-L") => a.opts.lib_dirs.push(s[2..].into()),
            "-l" => push_lib(&mut a.opts, &value(&name, inline, &mut it)),
            s if s.starts_with("-l") => push_lib(&mut a.opts, &s[2..]),
            "-F" => a
                .opts
                .framework_dirs
                .push(value(&name, inline, &mut it).into()),
            s if s.starts_with("-F") => a.opts.framework_dirs.push(s[2..].into()),
            "--native-object" => a.opts.objects.push(value(&name, inline, &mut it).into()),
            "--lib-version" => {
                let v = value(&name, inline, &mut it);
                let ok = v.split('.').count() == 3
                    && v.split('.')
                        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
                if !ok {
                    option_error(
                        format!("`--lib-version {v}`: expected <x.y.z>"),
                        "give the package's version as three numbers: `--lib-version 0.3.1`",
                    );
                }
                a.opts.lib_version = Some(v);
            }
            "--native-runtime" => a
                .opts
                .runtime_files
                .push(value(&name, inline, &mut it).into()),
            "--emit" => {
                for k in value(&name, inline, &mut it).split(',') {
                    a.emit.push(EmitKind::parse(k).unwrap_or_else(|| {
                        option_error(
                            format!("unknown emit kind `{k}`{}", cli::did_you_mean(k, EMIT, &[])),
                            "`--emit` takes `source`, `hir`, `mir`, `obj`, `asm`, `listing`, \
                             `expansions` or `c-header`, several joined by `,`: `--emit=mir,asm`",
                        )
                    }));
                }
            }
            "--release" => {
                "2".clone_into(&mut a.opts.opt_level);
                a.opt_set = true;
            }
            "-O0" | "-O1" | "-O2" | "-O3" | "-Os" => {
                name[2..].clone_into(&mut a.opts.opt_level);
                a.opt_set = true;
            }
            "-C" | "--codegen" => codegen_option(&mut a, &value(&name, inline, &mut it)),
            "--keep" => a.opts.keep_temps = true,
            "-g" => a.opts.debug = true,
            "--remap-path-prefix" => {
                let v = value(&name, inline, &mut it);
                let Some((from, to)) = v.split_once('=').filter(|(f, _)| !f.is_empty()) else {
                    option_error(
                        format!("`--remap-path-prefix {v}`: expected <from>=<to>"),
                        "name the directory and how to write it: \
                         `--remap-path-prefix /home/me/app=app`",
                    );
                };
                let from = PathBuf::from(from);
                let from = std::path::absolute(&from).unwrap_or(from);
                a.opts.remap.push((from, to.to_owned()));
            }
            "--check" => a.check = true,
            "--check-library" => a.check_library = true,
            "--allow-dirty" => a.allow_dirty = true,
            "--std" => a.std = true,
            "--diff" => a.diff = true,
            "--list" => a.list = true,
            "--doc" if a.cmd == "test" => a.doc_tests = Some(true),
            "--no-doc" if a.cmd == "test" => a.doc_tests = Some(false),
            "--dump" => a.dump = true,
            "--connection-file" => {
                a.connection_file = Some(value(&name, inline, &mut it).into());
            }
            "-A" | "-W" | "-D" | "--allow" | "--warn" | "--deny" => {
                let lvl = Level::from_flag(&name).expect("a lint level flag");
                a.lint_levels
                    .flags
                    .push((lvl, value(&name, inline, &mut it)));
            }
            // From `nabi`: `profile: { "<p>": { overflow_checks: false } }`.
            "--no-overflow-checks-in" => {
                a.no_overflow_checks.push(value(&name, inline, &mut it));
            }
            // From `nabi`: the levels so far are this manifest's `lints`.
            "--lints-from-manifest" => {
                let p = PathBuf::from(value(&name, inline, &mut it));
                let shown = std::env::current_dir()
                    .ok()
                    .and_then(|d| p.strip_prefix(d).ok().map(Path::to_path_buf))
                    .unwrap_or_else(|| p.clone());
                a.lint_levels.manifest =
                    Some((shown.display().to_string(), a.lint_levels.flags.len()));
            }
            "--open" => a.open = true,
            "-h" | "--help" if a.cmd.is_empty() => "help".clone_into(&mut a.cmd),
            "-h" | "--help" => {
                print!("{}", command_help(&a.cmd));
                process::exit(0);
            }
            "--version" | "-V" if a.cmd.is_empty() => "version".clone_into(&mut a.cmd),
            // `-` alone is a positional (stdin).
            s if s.starts_with('-') && s.len() > 1 => {
                usage_error(&format!("unknown option `{s}`{}", suggest_flag(&a.cmd, s)));
            }
            _ if a.cmd.is_empty() => {
                let _ = CMD.set(arg.clone());
                a.cmd = arg;
            }
            _ => a.positional.push(arg),
        }
    }
    if a.cmd.is_empty() && a.print.is_none() && a.explain.is_none() {
        usage_error("missing command (e.g. `build`)");
    }
    tier_aliases(&a.externs);
    hot_options(&a);
    set_overflow_checks(&mut a);
    // Debug builds carry debug info; `-g` adds it to release. A bench is
    // `-O2` unless a level is given.
    let output = (matches!(a.cmd.as_str(), "build" | "run" | "test")
        || a.cmd == "bench" && a.opt_set)
        && a.emit.is_empty();
    if output && !a.opts.release() {
        a.opts.debug = true;
    }
    let mut spec = TargetSpec::resolve(a.target.as_deref()).unwrap_or_else(|e| fail(&e));
    spec.set_cpu(a.target_cpu.as_deref(), &a.target_features)
        .unwrap_or_else(|e| {
            option_error(
                e,
                "write a CPU and features this target has, or leave out `-C target-cpu`",
            )
        });
    a.spec = Some(spec);
    if nabic_core::env_flag("NABI_VERBOSE") {
        a.verbose = a.verbose.max(1);
    }
    a
}

/// `--hot` builds at `-O0` (8.4's root rule), and `--hot-base` /
/// `--hot-dep` belong to it.
fn hot_options(a: &Args) {
    if !a.hot {
        if a.hot_base.is_some() || !a.hot_deps.is_empty() {
            let flag = if a.hot_base.is_some() {
                "--hot-base"
            } else {
                "--hot-dep"
            };
            option_error(
                format!("`{flag}` is for hot-reload builds"),
                format!("add `--hot`, or drop `{flag}`"),
            );
        }
        return;
    }
    match a.opts.opt_level.as_str() {
        "0" => {}
        "1" => option_error("hot reload builds at `-O0`, not `-O1`", "drop `-O1`"),
        l => option_error(
            format!("hot reload builds at `-O0`, not `-O{l}`"),
            "hot reload runs debug builds; drop `--release`",
        ),
    }
}

/// Overflow checks are on in every profile; `-C overflow-checks=no` or
/// the manifest's profile turns them off.
pub fn set_overflow_checks(a: &mut Args) {
    let profile = if a.opts.release() { "release" } else { "debug" };
    a.opts.overflow_checks = a
        .overflow_checks
        .unwrap_or_else(|| !a.no_overflow_checks.iter().any(|p| p == profile));
}

/// `-C <v>`.
pub fn codegen_option(a: &mut Args, v: &str) {
    match v {
        "overflow-checks" | "overflow-checks=yes" | "overflow-checks=on" => {
            a.overflow_checks = Some(true);
        }
        "overflow-checks=no" | "overflow-checks=off" => {
            a.overflow_checks = Some(false);
        }
        "icf" | "icf=yes" | "icf=on" => a.opts.icf = Some(true),
        "icf=no" | "icf=off" => a.opts.icf = Some(false),
        _ if v.starts_with("target-cpu=") && v.len() > 11 => {
            a.target_cpu = Some(v[11..].to_owned());
        }
        _ if v.starts_with("target-feature=") => {
            a.target_features.extend(
                v[15..]
                    .split(',')
                    .filter(|f| !f.is_empty())
                    .map(str::to_owned),
            );
        }
        _ => match v
            .strip_prefix("subsystem=")
            .and_then(nabic_interface::Subsystem::parse)
        {
            Some(s) => a.opts.subsystem = s,
            None if v.starts_with("subsystem=") => option_error(
                format!("unknown codegen option `{v}`"),
                "write `-C subsystem=console` or `-C subsystem=windows`",
            ),
            None => {
                let key = v.split_once('=').map_or(v, |(k, _)| k);
                option_error(
                    format!(
                        "unknown codegen option `{v}`{}",
                        cli::did_you_mean(key, CODEGEN, &[])
                    ),
                    "`-C` takes `overflow-checks[=yes|no]`, `icf[=yes|no]`, \
                     `target-cpu=<cpu>`, `target-feature=+f1,-f2` or \
                     `subsystem=console|windows`",
                )
            }
        },
    }
}

/// The `-C` keys, for "did you mean".
const CODEGEN: [&str; 5] = [
    "overflow-checks",
    "icf",
    "target-cpu",
    "target-feature",
    "subsystem",
];

/// The `--emit` kinds, for "did you mean".
const EMIT: [&str; 8] = [
    "source",
    "hir",
    "mir",
    "obj",
    "asm",
    "listing",
    "expansions",
    "c-header",
];

/// `-l [static=|dylib=]<name>`: `(name, static)`.
pub fn parse_lib(v: &str) -> (String, bool) {
    let (kind, name) = v.split_once('=').unwrap_or(("dylib", v));
    if name.is_empty() || !matches!(kind, "static" | "dylib") {
        option_error(
            format!("`-l {v}`: expected [static=|dylib=|framework=]<name>"),
            "write `-l z`, `-l static=z` or `-l dylib=z` for libz, `-l framework=Foundation` for \
             a framework",
        );
    }
    (name.to_owned(), kind == "static")
}

/// `-l`: a library, or with `framework=` an Apple framework.
fn push_lib(opts: &mut BuildOptions, v: &str) {
    match v.strip_prefix("framework=") {
        Some(f) if !f.is_empty() => {
            if !opts.frameworks.iter().any(|x| x == f) {
                opts.frameworks.push(f.to_owned());
            }
        }
        _ => opts.libs.push(parse_lib(v)),
    }
}

/// An `--extern` named like a standard library tier must be another name
/// of a dependency given before it (`--extern helper=p --extern std=p`).
fn tier_aliases(externs: &[(String, PathBuf)]) {
    for (i, (name, path)) in externs.iter().enumerate() {
        if nabic_core::TIERS.contains(&name.as_str())
            && !externs[..i].iter().any(|(_, p)| p == path)
        {
            let e = nabic_core::reserved_package(name).unwrap_or_default();
            option_error(
                format!("`--extern {name}`: {e}"),
                format!(
                    "give the dependency its own name first: `--extern <name>={0} --extern \
                     {name}={0}`",
                    path.display()
                ),
            );
        }
    }
}

/// `--extern name=path`: a package directory, its
/// `src/lib.nabi`, or a prebuilt `<name>.nabihir` / `lib<name>.a`.
pub fn parse_extern(v: &str) -> (String, PathBuf) {
    let Some((name, path)) = v.split_once('=') else {
        option_error(
            format!("`--extern {v}`: expected <name>=<path>"),
            format!("write `--extern {v}=<path>`, the path of its package directory"),
        );
    };
    // A package name (`my-lib`) stands for its code name (`my_lib`).
    let name = &name.replace('-', "_");
    let ident = |s: &str| {
        s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && s.chars().all(|c| c.is_alphanumeric() || c == '_')
    };
    if !ident(name) {
        option_error(
            format!("`--extern`: `{name}` is not a package name"),
            "a package name is letters, digits, `_` and `-`, starting with a letter: \
             `--extern my_lib=../my-lib`",
        );
    }
    // A tier's name may be another name of a dependency (an alias), not
    // its own: `tier_aliases` checks that once all are in.
    if let Some(e) =
        nabic_core::reserved_package(name).filter(|_| !nabic_core::TIERS.contains(&name.as_str()))
    {
        option_error(
            format!("`--extern {name}`: {e}"),
            "give the dependency another name: `--extern <name>=<path>`",
        );
    }
    let path = PathBuf::from(path);
    let prebuilt = path.extension().is_some_and(|e| e == "a" || e == "nabihir");
    let root = if prebuilt {
        nabic_interface::nabihir::companions(&path).0
    } else if path.is_dir() {
        path.join("src/lib.nabi")
    } else {
        path
    };
    if !root.is_file() {
        fail(&format!("`--extern {name}`: no `{}`", root.display()));
    }
    (name.to_owned(), root)
}
