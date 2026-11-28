//! Running a command on a package: the package, its dependencies and the
//! flags for nabic, which runs the compiler commands; the tools take the
//! same flags here.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// What a command works on, and where its dependencies are prebuilt when
/// it is not a package (an environment's or a script's `target/`).
pub(crate) struct Target {
    pub(crate) ctx: Context,
    pub(crate) env: Option<String>,
    pub(crate) target_dir: Option<PathBuf>,
}

pub(crate) fn env_dir(name: &str) -> PathBuf {
    home().join("envs").join(name)
}

/// The environment used outside a project: `$NABI_ENV`, `nabi env use`'s
/// choice, or `default`.
pub(crate) fn ambient_env() -> Result<String, String> {
    if let Some(e) = std::env::var("NABI_ENV").ok().filter(|e| !e.is_empty()) {
        env::name("NABI_ENV", &e)?;
        return Ok(e);
    }
    Ok(env::chosen()?.unwrap_or_else(|| "default".to_owned()))
}

pub(crate) fn load_dir(dir: &Path) -> Result<Context, String> {
    // A workspace root has no package of its own: its lock is the members'.
    if workspace::is_root(dir) {
        let ws = workspace::load(dir)?;
        return Ok(Context {
            manifest: Manifest::default(),
            base: dir.to_path_buf(),
            lock_path: Some(dir.join(manifest::LOCK)),
            script: false,
            workspace: ws,
        });
    }
    let manifest = match manifest::find(dir) {
        Some(p) => {
            let mut m = manifest::load(&p)?;
            m.lints = manifest::inherit_lints(&m, &p)?;
            m
        }
        None => Manifest::default(),
    };
    let ws = if dir.is_dir() {
        workspace::of(dir)?
    } else {
        None
    };
    let lock_dir = ws.as_ref().map_or(dir, |w| w.root.as_path());
    Ok(Context {
        manifest,
        base: dir.to_path_buf(),
        lock_path: dir.is_dir().then(|| lock_dir.join(manifest::LOCK)),
        script: false,
        workspace: ws,
    })
}

/// A workspace root is no package: only the REPL, kernel and language
/// server (without a path) start there.
fn not_root(package: Option<&Path>, cmd: &str, path: bool) -> Result<(), String> {
    match package.filter(|d| workspace::is_root(d)) {
        Some(root) if path || !matches!(cmd, "repl" | "jupyter" | "lsp") => Err(format!(
            "{} is a workspace root, not a package: run `nabi {cmd}` in a member's directory, \
             or name one: `nabi {cmd} -p <member>`",
            shown(root)
        )),
        _ => Ok(()),
    }
}

/// `--target-dir` of a workspace member's shared `target/`, unless the
/// command names its own.
fn member_target(ctx: &Context, on_package: bool, post: &[String]) -> Option<String> {
    let own = post.iter().any(|a| a.starts_with("--target-dir"));
    let dir = shared_target(ctx).filter(|_| on_package && !own)?;
    Some(format!("--target-dir={}", dir.display()))
}

/// The `target/` a workspace member shares with the others (none with
/// `NABI_TARGET_DIR`, which wins).
pub(crate) fn shared_target(ctx: &Context) -> Option<PathBuf> {
    let ws = ctx.workspace.as_ref()?;
    if std::env::var_os("NABI_TARGET_DIR").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    Some(ws.root.join("target"))
}

pub(crate) fn env_target(name: &str) -> Result<Target, String> {
    Ok(Target {
        ctx: load_dir(&env_dir(name))?,
        env: Some(name.to_owned()),
        target_dir: Some(env_dir(name).join("target")),
    })
}

/// The environment a command outside a package works in: `--env`
/// (which must exist, but for `default`), else `ambient_env`.
pub(crate) fn selected_env(g: &Global) -> Result<String, String> {
    match &g.env {
        Some(name) => {
            env::name("--env", name)?;
            if !env_dir(name).is_dir() && name != "default" {
                return Err(format!(
                    "no environment `{name}` (create it: nabi env create {name})"
                ));
            }
            Ok(name.clone())
        }
        None => ambient_env(),
    }
}

/// The context for `path` (default: cwd). A package sees only its own
/// `nabi.nabion`'s deps; anything else works in an environment.
pub(crate) fn target_for(g: &Global, path: Option<&str>) -> Result<Target, String> {
    let start = Path::new(path.unwrap_or("."));
    let package = package_dir(start);
    if let (Some(dir), None) = (&package, &g.env) {
        return Ok(Target {
            ctx: load_dir(dir)?,
            env: None,
            target_dir: None,
        });
    }
    let env = selected_env(g)?;
    if package.is_none() && start.is_file() {
        return script_target(start, &env);
    }
    env_target(&env)
}

/// `path` relative to cwd when under it.
pub(crate) fn shown(path: &Path) -> String {
    let cwd = std::env::current_dir().and_then(nabic_core::real_path);
    match cwd {
        Ok(c) if path.starts_with(&c) => relative(&c, path).display().to_string(),
        _ => path.display().to_string(),
    }
}

/// Script `root` and the files its `mod` declarations reach, as nabic
/// finds them, with their text.
pub(crate) fn script_files(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out: Vec<(PathBuf, String)> = Vec::new();
    let dir = root.parent().map(Path::to_path_buf);
    let mut todo = vec![(root.to_path_buf(), dir)];
    while let Some((file, dir)) = todo.pop() {
        let Ok(src) = std::fs::read_to_string(&file) else {
            continue;
        };
        if out.iter().any(|(f, _)| *f == file) {
            continue;
        }
        // A token scan, not a parse: a `#[cfg]`-removed `mod` still counts.
        if let Some(dir) = dir.filter(|_| src.contains("mod")) {
            let (tokens, _) = nabic_lexer::Lexer::new(&src, "").lex_all();
            let kinds: Vec<&T> = tokens.iter().map(|t| &t.kind).collect();
            let mut path = None;
            for (i, k) in kinds.iter().enumerate() {
                match (k, kinds.get(i + 1..i + 5)) {
                    (T::HashLBracket, Some([T::Ident(a), T::LParen, T::StrLit(p), T::RParen]))
                        if a == "path" =>
                    {
                        path = Some(p.clone());
                    }
                    (T::Mod, Some([T::Ident(name), T::Semi, ..])) => {
                        let (body, sub) = match path.take() {
                            Some(p) => {
                                let body = dir.join(p);
                                let sub = body.with_extension("");
                                (body, sub)
                            }
                            None => (dir.join(format!("{name}.nabi")), dir.join(name)),
                        };
                        todo.push((body, Some(sub)));
                    }
                    (T::Semi | T::LBrace | T::RBrace, _) => path = None,
                    _ => {}
                }
            }
        }
        out.push((file, src));
    }
    // The order nabic reads them in does not matter; the key's does.
    if let Some(rest) = out.get_mut(1..) {
        rest.sort_by(|a, b| a.0.cmp(&b.0));
    }
    out
}

/// A lone script: the merged `deps` / `native` of its and its modules'
/// `//! ```nabion` blocks (paths relative to each file); conflicting
/// requirements are an error. Lock and builds live in
/// `cache/scripts/<hash>/`, keyed by every file's path and block.
pub(crate) fn script_target(file: &Path, env: &str) -> Result<Target, String> {
    let abs = nabic_core::real_path(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut manifest = Manifest::default();
    // Per dependency: (name, file whose block has it, as written, merged).
    let mut by: Vec<(String, String, String, String)> = Vec::new();
    let mut key = abs.display().to_string();
    for (f, src) in script_files(&abs) {
        let Some(block) = manifest::script_block(&src) else {
            continue;
        };
        let name = shown(&f);
        key += &format!("\0{}\0{}", f.display(), block.text);
        let label = format!("{name} (//! nabion block)");
        let m = manifest::parse(&block.text, &label)?;
        for w in &m.warnings {
            warn_once(w);
        }
        if m.dev_deps.len() + m.lints.len() + m.features.len() > 0
            || m.lints_from.is_some()
            || !m.no_overflow_checks.is_empty()
            || m.name.is_some()
            || m.version.is_some()
        {
            return Err(format!(
                "{label}: a script block holds only `deps`, `native` and `toolchain`"
            ));
        }
        // The script run picks the toolchain (`toolchain::script_pin`).
        if m.toolchain.is_some() && f != abs {
            return Err(format!(
                "{label}: `toolchain` goes in the block of {}, the script that is run; remove \
                 it here",
                shown(&abs)
            ));
        }
        let dir = f.parent().unwrap_or_else(|| Path::new("/"));
        let local = |p: &str| {
            let joined = dir.join(p);
            nabic_core::real_path(&joined)
                .unwrap_or(joined)
                .display()
                .to_string()
                .replace('\\', "/")
        };
        let mut merge = |dep: &str, req: String, text: String| -> Result<bool, String> {
            match by.iter().find(|b| manifest::same(&b.0, dep)) {
                Some(b) if b.3 == text => Ok(false),
                Some((n, file1, req1, _)) => Err(format!(
                    "`{n}` is required as {req1} in {file1} and as {req} in {name}: use one \
                     version, the same entry in both blocks (or keep it in one of them)"
                )),
                None => {
                    by.push((dep.to_owned(), name.clone(), req, text));
                    Ok(true)
                }
            }
        };
        for mut d in m.deps {
            let req = d.source.to_nabi();
            if let Source::Path(p) = &d.source {
                d.source = Source::Path(local(p));
            }
            let text = nabic_nabion::to_line(&d.to_value());
            if merge(&d.name, req, text)? {
                manifest.deps.push(d);
            }
        }
        for mut d in m.native {
            let req = nabic_nabion::to_line(&d.source.to_value());
            if let manifest::NativeSource::Direct { url, .. } = &mut d.source {
                if !url.contains("://") && Path::new(url.as_str()).is_relative() {
                    *url = local(url);
                }
            }
            let text = nabic_nabion::to_line(&d.source.to_value());
            if merge(&format!("native {}", d.name), req, text)? {
                manifest.native.push(d);
            }
        }
    }
    let cache = home()
        .join("cache/scripts")
        .join(format!("{:016x}", nabic_core::hash::fnv(key.as_bytes())));
    std::fs::create_dir_all(&cache).map_err(|e| format!("{}: {e}", cache.display()))?;
    Ok(Target {
        ctx: Context {
            manifest,
            base: abs.parent().unwrap_or_else(|| Path::new("/")).to_path_buf(),
            lock_path: Some(cache.join(manifest::LOCK)),
            script: true,
            workspace: None,
        },
        env: Some(env.to_owned()),
        target_dir: Some(cache.join("target")),
    })
}

pub(crate) fn options(g: &Global) -> Options {
    Options {
        offline: g.offline,
        locked: g.locked,
        quiet: g.quiet,
        ..Options::default()
    }
}

/// `args` without the feature options before `--`, which go to `o`.
pub(crate) fn feature_flags(args: &[String], o: &mut Options) -> Result<Vec<String>, String> {
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => {
                rest.push(a.clone());
                rest.extend(it.by_ref().cloned());
            }
            "--all-features" => o.all_features = true,
            "--no-default-features" => o.no_default_features = true,
            "--features" => {
                let v = it.next().ok_or("`--features` needs a value")?;
                o.features.extend(split_features(v));
            }
            _ => match a.strip_prefix("--features=") {
                Some(v) => o.features.extend(split_features(v)),
                // `--featurs a`: its value would be read as the path.
                None if a.starts_with("--")
                    && !crate::all_flags()
                        .iter()
                        .any(|k| k == a.split('=').next().unwrap_or(a)) =>
                {
                    let own = ["--features", "--all-features", "--no-default-features"];
                    let hint =
                        nabic_core::cli::did_you_mean(a.split('=').next().unwrap_or(a), own, &[]);
                    if !hint.is_empty() {
                        return Err(format!("unknown option `{a}`{hint}"));
                    }
                    rest.push(a.clone());
                }
                None => rest.push(a.clone()),
            },
        }
    }
    Ok(rest)
}

/// `a,b c` -> `a`, `b`, `c`.
pub(crate) fn split_features(v: &str) -> impl Iterator<Item = String> {
    v.split([',', ' '])
        .filter(|f| !f.is_empty())
        .map(str::to_owned)
}

/// Push `lints` as `-A/-W/-D` flags; where they end.
pub(crate) fn push_lints(
    extra: &mut Vec<String>,
    lints: &[(String, nabic_core::lint::Level)],
) -> usize {
    for (name, level) in lints {
        extra.push(level.flag().to_owned());
        extra.push(name.clone());
    }
    extra.len()
}

/// Say that the levels before `at` are `ctx`'s manifest `lints`, so nabic
/// names nabi.nabion as their source. The nabic is always this nabi's own (a
/// pinned toolchain gets the whole command via `handoff`).
pub(crate) fn mark_manifest_lints(extra: &mut Vec<String>, at: usize, ctx: &resolve::Context) {
    if ctx.manifest.lints.is_empty() {
        return;
    }
    if let Some(path) = manifest::find(&ctx.base) {
        extra.insert(at, format!("--lints-from-manifest={}", path.display()));
    }
}

/// Run `nabic <pre> <cmd> [--extern..] [lint levels] [--target-dir] <post>`.
pub(crate) fn delegate(
    g: &Global,
    cmd: &str,
    pre: &[String],
    post: &[String],
) -> Result<i32, String> {
    let (c, nabic) = nabic_command(g, cmd, pre, post, None)?;
    Ok(exec(c, &nabic))
}

/// Run tool `cmd` here, on the arguments nabic would get.
pub(crate) fn tool(g: &Global, cmd: &str, pre: &[String], post: &[String]) -> Result<i32, String> {
    let inv = invocation(g, cmd, pre, post, None)?;
    for (name, value) in &inv.handoffs {
        nabic_core::set_handoff(name, value.as_deref());
    }
    // `:add` in the REPL and the language server's actions run this nabi,
    // unless a nabi that ran this one said which.
    if nabic_core::handoff_var("NABI").is_none() {
        let me = std::env::current_exe().ok();
        nabic_core::set_handoff("NABI", me.as_deref().map(Path::as_os_str));
    }
    if cmd == "fmt" && !g.offline {
        fetch_for_fmt(g, post);
    }
    let deps = (cmd == "lsp").then(|| lsp_deps(g, pre, post));
    Ok(crate::tools::run(inv.args, inv.tail, deps))
}

/// `nabi fmt` lexes a package with its dependencies' operators, read from
/// the cache: fetch the sources missing there, as a build would, before
/// the first build. Never changes `nabi.lock`; a package that cannot be
/// resolved is left to fmt, which says what it could not read.
fn fetch_for_fmt(g: &Global, post: &[String]) {
    let mut paths: Vec<&str> = post
        .iter()
        .take_while(|a| *a != "--")
        .filter(|a| !a.starts_with('-'))
        .map(String::as_str)
        .collect();
    if paths.is_empty() {
        paths.push(".");
    }
    let mut seen = Vec::new();
    for p in paths {
        let Some(dir) = package_dir(Path::new(p)) else {
            continue;
        };
        if seen.contains(&dir) {
            continue;
        }
        let opts = Options {
            locked: true,
            dev: true,
            ..options(g)
        };
        if let Ok(ctx) = load_dir(&dir) {
            let _ = resolve::resolve(&home(), &ctx, &opts);
        }
        seen.push(dir);
    }
}

/// The language server's dependencies of directory `dir`: its package's
/// (nabi.nabion and nabi.lock), else the environment's, prebuilt as for
/// `nabi lsp` started there.
fn lsp_deps(g: &Global, pre: &[String], post: &[String]) -> nabils::Deps {
    let g = g.clone();
    let (pre, post) = (pre.to_vec(), post.to_vec());
    Box::new(move |dir: &Path| {
        let mut opts = options(&g);
        let post = feature_flags(&post, &mut opts)?;
        let t = target_for(&g, Some(&dir.display().to_string()))?;
        opts.target = nabic_target::TargetSpec::resolve(target_arg(&post)).ok();
        let graph = resolve::resolve(&home(), &t.ctx, &opts)?;
        let mut args: Vec<String> = g.color.iter().chain(&pre).cloned().collect();
        args.push("lsp".to_owned());
        for p in &graph.packages {
            args.extend(extern_args(p));
        }
        args.extend(dep_args(&graph, &[]));
        if let Some(d) = t.target_dir.clone().or_else(|| shared_target(&t.ctx)) {
            args.push(format!("--target-dir={}", d.display()));
        }
        args.extend(post.iter().take_while(|a| *a != "--").cloned());
        let mut a = nabic::with_defaults(nabic::parse_args(args, Vec::new()));
        let pkg = nabic_interface::discover(dir).ok().filter(|p| !p.anonymous);
        nabic::prebuild(&mut a, pkg.as_ref())?;
        Ok(nabils::Externs {
            externs: a.externs,
            deps: a.extern_deps,
        })
    })
}

/// A command's arguments up to `--` (with it) and the program's after it
/// as given, and the hand-offs (`__<name>`) that go with them.
pub(crate) struct Invocation {
    pub(crate) args: Vec<String>,
    pub(crate) tail: Vec<std::ffi::OsString>,
    pub(crate) handoffs: Vec<(&'static str, Option<std::ffi::OsString>)>,
    /// The manifest's `deployment_target`, for `MACOSX_DEPLOYMENT_TARGET`
    /// and `IPHONEOS_DEPLOYMENT_TARGET`.
    pub(crate) deployment: (Option<String>, Option<String>),
}

/// `nabi test` / `nabi bench`: `nabic` builds the harness and writes a
/// manifest (the built executables and their cases); the runner here runs
/// each case, out of the compiler, with the options it takes for itself.
pub(crate) fn harness(
    g: &Global,
    cmd: &str,
    pre: &[String],
    post: &[String],
) -> Result<i32, String> {
    let (opts, post) = nabic_harness::take_options(cmd, post)?;
    let (mut c, nabic) = nabic_command(g, cmd, pre, &post, None)?;
    let tmp = TempEntry::dir("nabi-harness-").map_err(|e| e.to_string())?;
    let manifest = tmp.path().join("manifest");
    c.arg("--emit-harness").arg(&manifest);
    // Build the harness; its progress and diagnostics stream through.
    let status = c
        .status()
        .map_err(|e| format!("cannot run {}: {e}", nabic.display()))?;
    if !status.success() {
        return Ok(status.code().unwrap_or(1));
    }
    // No manifest means there was nothing to run (e.g. `--emit`).
    match std::fs::read_to_string(&manifest) {
        Ok(text) => {
            nabic_harness::check_version(&text)?;
            Ok(nabic_harness::run_manifest(
                &text,
                &opts,
                &mut std::io::stdout().lock(),
            ))
        }
        Err(_) => Ok(0),
    }
}

/// When `package` (the one a command works on), `script` or `+<version>`
/// / `NABI_TOOLCHAIN` pins another toolchain than this one, run the
/// command with that toolchain's nabi and exit with its status.
pub(crate) fn hand_off_for(
    g: &Global,
    package: Option<&Path>,
    script: Option<&Path>,
) -> Result<(), String> {
    let pin = package.and_then(manifest::find).and_then(|p| {
        let pin = manifest::toolchain_pin(&std::fs::read_to_string(&p).ok()?)?;
        Some((pin, format!("pinned by {}", p.display())))
    });
    let pin = pin.or_else(|| script.and_then(toolchain::script_pin));
    if let Some(h) = toolchain::handoff(g.toolchain.as_deref(), pin, true)? {
        std::process::exit(toolchain::run_handoff(&h, &g.argv, g.start.as_deref()));
    }
    Ok(())
}

/// `delegate`'s nabic command, and the nabic it runs; with `in_env`, on
/// that environment's libraries (`prebuild_env`).
pub(crate) fn nabic_command(
    g: &Global,
    cmd: &str,
    pre: &[String],
    post: &[String],
    in_env: Option<&str>,
) -> Result<(Command, PathBuf), String> {
    let inv = invocation(g, cmd, pre, post, in_env)?;
    let nabic = toolchain::nabic_for()?;
    let mut c = Command::new(&nabic);
    // nabic's "did you mean" knows nabi's own flags of the command too.
    c.env(
        nabic_core::cli::FRONT_FLAGS_VAR,
        crate::flags_of(cmd).join("\n"),
    );
    c.args(&inv.args).args(&inv.tail);
    // The environment's own value wins.
    let (mac, ios) = &inv.deployment;
    for (var, d) in [
        ("MACOSX_DEPLOYMENT_TARGET", mac),
        ("IPHONEOS_DEPLOYMENT_TARGET", ios),
    ] {
        if let Some(d) = d.as_ref().filter(|_| std::env::var_os(var).is_none()) {
            c.env(var, d);
        }
    }
    for (name, value) in &inv.handoffs {
        handoff(&mut c, name, value.as_deref());
    }
    Ok((c, nabic))
}

/// What `cmd` runs on: nabic's arguments for it (or a tool's here), with
/// the package's dependencies resolved.
pub(crate) fn invocation(
    g: &Global,
    cmd: &str,
    pre: &[String],
    post: &[String],
    in_env: Option<&str>,
) -> Result<Invocation, String> {
    let mut opts = options(g);
    opts.dev = matches!(cmd, "test" | "bench");
    let resolves = !matches!(cmd, "fmt" | "clean" | "demangle" | "bindgen" | "inspect");
    let post = &if resolves {
        feature_flags(post, &mut opts)?
    } else {
        post.to_vec()
    };
    // The path is the first positional argument after the command.
    let first = positional(post);
    let path = first.filter(|p| *p != "-" && Path::new(p).exists());
    // Commands on a package say so here, before resolving anything.
    let start = Path::new(path.unwrap_or("."));
    let stdin = first == Some("-");
    let package = package_dir(start);
    // `doc --std` without a path documents the standard library alone and
    // needs no package.
    let std_doc = cmd == "doc" && first.is_none() && post.iter().any(|a| a == "--std");
    // Without a path, a REPL / kernel / language server may use an
    // environment from inside a package.
    let on_package = package.is_some()
        && !stdin
        && (path.is_some() || !matches!(cmd, "repl" | "jupyter" | "lsp"));
    if let Some(env) = g.env.as_ref().filter(|_| on_package && resolves) {
        return Err(format!(
            "`--env {env}` is for scripts and the REPL: a package builds with the dependencies \
             in its nabi.nabion only (add one: `nabi add <name>`)"
        ));
    }
    // A missing file is that, not a missing package; for test / bench a
    // word may be a filter.
    if let Some(p) = first.filter(|p| *p != "-" && path.is_none()) {
        let file = Path::new(p).extension().is_some_and(|x| x == "nabi");
        if matches!(
            cmd,
            "build" | "check" | "run" | "doc" | "lint" | "fix" | "emit" | "clean" | "repl" | "fmt"
        ) || file && matches!(cmd, "test" | "bench")
        {
            return Err(format!("`{p}` does not exist"));
        }
    }
    if matches!(
        cmd,
        "build" | "check" | "run" | "test" | "bench" | "doc" | "lint" | "fix" | "emit" | "clean"
    ) && package.is_none()
        && !start.is_file()
        && !stdin
        && !std_doc
    {
        return Err(format!(
            "not inside a package: no src/main.nabi or src/lib.nabi in {} or above (make one with \
             `nabi new <name>` or `nabi init`, or name a .nabi file)",
            std::path::absolute(start)
                .unwrap_or_else(|_| start.to_path_buf())
                .display()
        ));
    }
    not_root(package.as_deref(), cmd, path.is_some())?;
    // Before reading its manifest (which may have newer fields) or lock.
    hand_off_for(
        g,
        package
            .as_deref()
            .filter(|_| in_env.is_none() && g.env.is_none()),
        Some(start).filter(|s| package.is_none() && in_env.is_none() && s.is_file()),
    )?;
    // Not in an environment, nor for code on stdin.
    official::add_used(
        g,
        cmd,
        in_env.is_some() || g.env.is_some() || stdin,
        package.as_deref().filter(|_| on_package),
        Some(start).filter(|s| package.is_none() && s.is_file()),
    )?;
    let t = match in_env {
        Some(e) => env_target(e)?,
        None => target_for(g, path)?,
    };
    let mut extra: Vec<String> = Vec::new();
    let mut manifest_lints = None;
    // The manifest's `name` is the package's.
    let named = package.as_deref().filter(|_| t.env.is_none() && !stdin);
    if let (Some(name), Some(_)) = (&t.ctx.manifest.name, named) {
        if !matches!(
            cmd,
            "fmt" | "clean" | "demangle" | "bindgen" | "inspect" | "lsp" | "jupyter"
        ) {
            extra.push(format!("--name={name}"));
        }
    }
    if resolves {
        opts.target = nabic_target::TargetSpec::resolve(target_arg(post)).ok();
        let graph = match resolve::resolve(&home(), &t.ctx, &opts) {
            Ok(r) => r,
            // The language server still starts (without dependencies).
            Err(e) if cmd == "lsp" => {
                eprintln!("nabi: {} {e}", term::label(Stream::Stderr, Role::Warning));
                resolve::Graph {
                    packages: Vec::new(),
                    natives: Vec::new(),
                    system: Vec::new(),
                    features: Vec::new(),
                    deps: Vec::new(),
                }
            }
            Err(e) => return Err(e),
        };
        let (skip, more) = plugin_setup(g, &t, &graph, package.as_deref(), cmd, post, on_package)?;
        for p in graph.packages.iter().filter(|p| !skip.contains(&p.name)) {
            extra.extend(extern_args(p));
        }
        extra.extend(dep_args(&graph, &skip));
        extra.extend(more);
        // The package's own features.
        for f in &graph.features {
            extra.push(format!("--cfg=feature={f}"));
        }
        extra.extend(link_args(cmd, &t, &graph, post, g)?);
        // nabic picks the profile; `-C overflow-checks` beats these.
        for p in &t.ctx.manifest.no_overflow_checks {
            extra.push(format!("--no-overflow-checks-in={p}"));
        }
        // `lints` go first: the command line's own `-A/-W/-D` win.
        manifest_lints = Some(push_lints(&mut extra, &t.ctx.manifest.lints));
        // `doc --std` writes ./target/doc/, not into an environment.
        if let Some(dir) = t.target_dir.as_ref().filter(|_| !on_package && !std_doc) {
            extra.push(format!("--target-dir={}", dir.display()));
        }
        extra.extend(member_target(&t.ctx, on_package, post));
    }
    if let Some(at) = manifest_lints {
        mark_manifest_lints(&mut extra, at, &t.ctx);
    }
    let mut args: Vec<String> = g.color.iter().chain(pre).cloned().collect();
    args.push(cmd.to_owned());
    args.extend(extra);
    let tail = match post.iter().position(|a| a == "--") {
        Some(i) if post.len() - i - 1 == g.tail.len() => {
            args.extend_from_slice(&post[..=i]);
            g.tail.clone()
        }
        _ => {
            args.extend_from_slice(post);
            Vec::new()
        }
    };
    // Tell nabic what the environment has, so a script missing one of
    // those gets a hint.
    let libs = t.env.as_deref().filter(|_| t.ctx.script).map(|e| {
        let libs: Vec<String> = load_dir(&env_dir(e))
            .map(|c| c.manifest.deps.into_iter().map(|d| d.name).collect())
            .unwrap_or_default();
        libs.join(",")
    });
    let handoffs = vec![
        // nabic's usage errors and help then speak of `nabi`.
        ("NABI_CLI", Some("nabi".into())),
        // The REPL's `:add` installs into this environment (or, with
        // none, adds to the package).
        ("NABI_ENV", t.env.map(Into::into)),
        ("NABI_ENV_LIBS", libs.map(Into::into)),
        // A plugin's descriptor carries its version.
        (
            "NABI_PACKAGE_VERSION",
            t.ctx.manifest.version.clone().map(Into::into),
        ),
    ];
    Ok(Invocation {
        args,
        tail,
        handoffs,
        deployment: (
            t.ctx.manifest.deployment_target.clone(),
            t.ctx.manifest.ios_deployment_target.clone(),
        ),
    })
}

/// Hand `value` to nabic as `__<name>` (`None`: unset it).
pub(crate) fn handoff(c: &mut Command, name: &str, value: Option<&std::ffi::OsStr>) {
    let n = format!("__{name}");
    match value {
        Some(v) => c.env(n, v),
        None => c.env_remove(n),
    };
}

/// The first argument before `--` that is neither an option nor its value
/// (`-` counts: stdin).
pub(crate) fn positional(args: &[String]) -> Option<&str> {
    let mut it = args.iter().take_while(|a| *a != "--");
    while let Some(a) = it.next() {
        if VALUE_OPTIONS.contains(&a.as_str()) {
            it.next();
        } else if a == "-" || !a.starts_with('-') {
            return Some(a);
        }
    }
    None
}

/// `--extern=<name>=<lib.nabi>`, one more per other name its dependents
/// give it (nabic's aliases), and its `--extern-feature=<name>=<f>`s.
pub(crate) fn extern_args(p: &Resolved) -> Vec<String> {
    // A standard library tier's name is only another name of the package:
    // it mounts under its own first.
    let first = if nabic_core::TIERS.contains(&manifest::code_name(&p.code).as_str()) {
        &p.name
    } else {
        &p.code
    };
    let name = manifest::code_name(first);
    let mut names: Vec<String> = Vec::new();
    for n in std::iter::once(first).chain([&p.code]).chain(&p.aliases) {
        let n = manifest::code_name(n);
        if !names.contains(&n) {
            names.push(n);
        }
    }
    let mut out: Vec<String> = names
        .iter()
        .map(|n| format!("--extern={n}={}", p.root.display()))
        .collect();
    out.extend(
        p.features
            .iter()
            .map(|f| format!("--extern-feature={name}={f}")),
    );
    out
}

/// `--extern-dep=<package>=<name>` for each dependency of each package,
/// `pkg` the root: nabic then lets a package see only its own.
pub(crate) fn dep_args(graph: &resolve::Graph, skip: &[String]) -> Vec<String> {
    let mut out: Vec<String> = graph
        .deps
        .iter()
        .map(|n| format!("--extern-dep=pkg={n}"))
        .collect();
    for p in graph.packages.iter().filter(|p| !skip.contains(&p.name)) {
        let first = if nabic_core::TIERS.contains(&manifest::code_name(&p.code).as_str()) {
            &p.name
        } else {
            &p.code
        };
        let name = manifest::code_name(first);
        out.extend(p.deps.iter().map(|n| format!("--extern-dep={name}={n}")));
    }
    out
}

/// `--target <t>` / `--target=<t>` among nabic's arguments.
/// What the link needs from outside the package: native packages,
/// system libraries and frameworks, C objects, and a cdylib's kind.
fn link_args(
    cmd: &str,
    t: &Target,
    graph: &resolve::Graph,
    post: &[String],
    g: &Global,
) -> Result<Vec<String>, String> {
    let mut extra = Vec::new();
    // Native packages a link (or the REPL session) needs for the target.
    if matches!(cmd, "build" | "run" | "test" | "bench" | "repl" | "jupyter") {
        let natives: Vec<&native::Resolved> = graph.natives.iter().collect();
        if !natives.is_empty() {
            let target = nabic_target::TargetSpec::resolve(target_arg(post))?;
            extra.extend(native::nabic_args(
                &home(),
                &natives,
                &target,
                g.offline,
                g.quiet,
            )?);
        }
        if !graph.system.is_empty() {
            let target = nabic_target::TargetSpec::resolve(target_arg(post))?;
            extra.extend(crate::system::nabic_args(&graph.system, &target)?);
        }
        extra.extend(c_objects(t, graph, post, g.quiet)?);
    }
    if cmd == "build"
        && t.ctx.manifest.lib == crate::manifest::LibKind::Staticlib
        && !post.iter().any(|a| a.starts_with("--crate-type"))
    {
        extra.push("--crate-type=staticlib".to_owned());
        if t.ctx.manifest.xcframework {
            extra.push("--xcframework".to_owned());
        }
    }
    // `lib: { kind: "cdylib" }`: the shared library, its version and
    // header, unless the command line says otherwise.
    if cmd == "build"
        && t.ctx.manifest.lib == crate::manifest::LibKind::Cdylib
        && !post.iter().any(|a| a.starts_with("--crate-type"))
    {
        extra.push("--crate-type=cdylib".to_owned());
        if let Some(v) = t.ctx.manifest.version.as_deref() {
            if crate::semver::Version::parse(v).is_ok() {
                extra.push(format!(
                    "--lib-version={}",
                    v.split(['-', '+']).next().unwrap_or(v)
                ));
            }
        }
    }
    Ok(extra)
}

/// `--native-object` arguments of the `c` sources of the package and of
/// the packages it depends on.
fn c_objects(
    t: &Target,
    graph: &resolve::Graph,
    post: &[String],
    quiet: bool,
) -> Result<Vec<String>, String> {
    let mut units = Vec::new();
    if let Some(c) = &t.ctx.manifest.c {
        let name = t.ctx.manifest.name.clone().unwrap_or_default();
        units.push((name, t.ctx.base.clone(), c.clone()));
    }
    for p in &graph.packages {
        // `<dir>/src/lib.nabi`.
        let Some(dir) = p.root.parent().and_then(std::path::Path::parent) else {
            continue;
        };
        let Some(m) = crate::manifest::find(dir) else {
            continue;
        };
        if let Some(c) = crate::manifest::load(&m)?.c {
            units.push((p.name.clone(), dir.to_path_buf(), c));
        }
    }
    if units.is_empty() {
        return Ok(Vec::new());
    }
    let target = nabic_target::TargetSpec::resolve(target_arg(post))?;
    let mut include = Vec::new();
    for d in &graph.system {
        if let crate::manifest::NativeSource::System(s) = &d.source {
            include.extend(
                crate::system::find(&d.name, s, &target, &crate::system::Env::real())?.include,
            );
        }
    }
    let release = post
        .iter()
        .any(|a| matches!(a.as_str(), "--release" | "-O2" | "-O3" | "-Os"));
    let opt = post
        .iter()
        .rev()
        .find_map(|a| a.strip_prefix("-O"))
        .unwrap_or(if release { "2" } else { "0" });
    let base = t
        .target_dir
        .clone()
        .unwrap_or_else(|| t.ctx.base.join("target"))
        .join(if release { "release" } else { "debug" })
        .join("c");
    let mut args = Vec::new();
    for (name, dir, c) in &units {
        let objects = crate::cc::compile(&crate::cc::Unit {
            package: name,
            dir,
            c,
            include: &include,
            target: &target,
            opt,
            out: &base.join(name),
            quiet,
        })?;
        args.extend(
            objects
                .iter()
                .map(|o| format!("--native-object={}", o.display())),
        );
    }
    Ok(args)
}

pub(crate) fn target_arg(post: &[String]) -> Option<&str> {
    let mut it = post.iter().take_while(|a| *a != "--");
    while let Some(a) = it.next() {
        if a == "--target" {
            return it.next().map(String::as_str);
        }
        if let Some(t) = a.strip_prefix("--target=") {
            return Some(t);
        }
    }
    None
}

/// The `plugin: true` dependencies (built as plugins beside the program,
/// not linked: their names) and nabic's arguments for the package's
/// `lib: { kind: .. }`.
fn plugin_setup(
    g: &Global,
    t: &Target,
    graph: &resolve::Graph,
    package: Option<&Path>,
    cmd: &str,
    post: &[String],
    on_package: bool,
) -> Result<(Vec<String>, Vec<String>), String> {
    let plugins: Vec<&manifest::Dep> = (t.ctx.manifest.deps.iter())
        .filter(|d| d.load == manifest::Load::Plugin && on_package)
        .collect();
    let mut skip = Vec::new();
    for d in &plugins {
        let Some(p) = (graph.packages.iter()).find(|p| manifest::same(&p.name, d.package_name()))
        else {
            continue;
        };
        if matches!(cmd, "build" | "run" | "test" | "bench") {
            build_plugin(g, p, t.target_dir.as_deref(), post)?;
        }
        skip.push(p.name.clone());
    }
    let more = if on_package && matches!(cmd, "build" | "check" | "run" | "test" | "bench") {
        lib_args(&t.ctx, package, cmd, post)?
    } else {
        Vec::new()
    };
    Ok((skip, more))
}

/// nabic's arguments for the package's `lib: { kind: .. }`: its crate
/// type and the dependencies a plugin bundles.
fn lib_args(
    ctx: &Context,
    package: Option<&Path>,
    cmd: &str,
    post: &[String],
) -> Result<Vec<String>, String> {
    let m = &ctx.manifest;
    let given = post.iter().any(|a| a.starts_with("--crate-type"));
    let mut out = Vec::new();
    if m.lib == manifest::LibKind::Plugin {
        if package.is_some_and(|p| p.join("src/main.nabi").is_file()) {
            return Err(
                "this package is a plugin (`lib: { kind: \"plugin\" }`) and has src/main.nabi: a \
                 plugin is loaded by a program, so move main.nabi to a package of its own that \
                 loads this one with `std::plugin::load`"
                    .to_owned(),
            );
        }
        if cmd == "run" {
            return Err(
                "a plugin does not run by itself: run the program that loads it".to_owned(),
            );
        }
        for d in m.deps.iter().filter(|d| d.load == manifest::Load::Bundled) {
            out.push(format!("--bundle={}", manifest::code_name(&d.name)));
        }
    }
    // `lib: { kind: "cdylib" }` is `nabic_args`'.
    if !given && m.lib == manifest::LibKind::Plugin && cmd == "build" {
        out.push("--crate-type=plugin".to_owned());
    }
    Ok(out)
}

/// Build `plugin: true` dependency `p` as a plugin into `target` (the
/// program's target directory, so it lands beside the program), with
/// the program's profile.
fn build_plugin(
    g: &Global,
    p: &Resolved,
    target: Option<&Path>,
    post: &[String],
) -> Result<(), String> {
    let dir = p
        .root
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("{}: not inside a package", p.root.display()))?;
    let exe = nabic_core::current_exe().ok_or("cannot find the running nabi")?;
    let mut c = Command::new(exe);
    c.args(g.color.iter()).arg("build").arg(dir);
    c.arg("--crate-type=plugin");
    let own = std::path::absolute("target").unwrap_or_else(|_| PathBuf::from("target"));
    c.arg(format!(
        "--target-dir={}",
        target.map_or(own, Path::to_path_buf).display()
    ));
    for a in post.iter().take_while(|a| *a != "--") {
        if a == "--release" || a.starts_with("-O") || a.starts_with("--target") {
            c.arg(a);
        }
    }
    let status = c
        .status()
        .map_err(|e| format!("cannot build plugin {}: {e}", p.name))?;
    if !status.success() {
        return Err(format!("plugin {} did not build", p.name));
    }
    Ok(())
}
