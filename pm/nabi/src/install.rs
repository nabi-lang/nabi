//! `nabi install`, `nabi uninstall` and installed executables.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// Manifest of what `nabi install` put in `$NABI_HOME/bin`.
pub(crate) fn installs() -> PathBuf {
    home().join("installs.nabion")
}

/// `installs.nabion`'s entries; under `-v`, what a newer nabi wrote that
/// this one skipped.
pub(crate) fn read_installs(text: &str, file: &str) -> Result<Vec<Dep>, String> {
    let (deps, newer) = manifest::parse_installs(text, file)?;
    skipped(file, &newer.iter().map(String::as_str).collect::<Vec<_>>());
    Ok(deps)
}

pub(crate) const INSTALLS_HEADER: &str =
    "// nabi install: the executables in bin/ and where they came from\n";

/// The toolchain's executables in `$NABI_HOME/bin`.
pub(crate) const PROXIES: [&str; 4] = ["nabi", "nabic", "nabicc", "nabils"];

pub(crate) const INSTALL_USAGE: &str = "usage: nabi install <name>[@<req>] [--registry <url>] | [<name>] --path \
                             <dir | file.nabi> | [<name>] --git <url> [--rev|--tag|--branch <r>] \
                             [--features <f,..>] [--no-default-features] [--lib | --bin] [--force] | \
                             --native <name>[@<req>] | --list";

/// `nabi install`: an executable (package with `src/main.nabi`, or a script)
/// built `--release` into `$NABI_HOME/bin`, or a library (`src/lib.nabi`
/// only, or `--lib`) into the active environment; a package with both
/// is an error without `--lib` or `--bin`. An executable already
/// installed from the same version or commit and features is kept unless
/// `--force`; path installs always rebuild.
pub(crate) fn cmd_install(g: &Global, args: &[String]) -> Result<i32, String> {
    if args.iter().any(|a| a == "--list") {
        return match args {
            [_] => list_installs(g),
            _ => Err(INSTALL_USAGE.into()),
        };
    }
    let env = selected_env(g)?;
    if args.iter().any(|a| a == "--native") {
        return add_native(g, args, Some(&env), None);
    }
    let lib = args.iter().any(|a| a == "--lib");
    let bin = args.iter().any(|a| a == "--bin");
    if lib && bin {
        return Err(
            "`--lib` and `--bin` go apart: `--lib` installs the library, `--bin` the executable"
                .into(),
        );
    }
    let force = args.iter().any(|a| a == "--force");
    let args: Vec<String> = args
        .iter()
        .filter(|a| *a != "--lib" && *a != "--bin" && *a != "--force")
        .cloned()
        .collect();
    if args.is_empty() {
        return Err(INSTALL_USAGE.into());
    }
    let again = (!force).then(|| format!("nabi install {} --force", args.join(" ")));
    let (mut dep, dev) = parse_dep_args(&args)?;
    if dev || dep.optional {
        return Err(
            "`nabi install` takes no --dev / --optional: they are for a package's nabi.nabion (`nabi \
             add`)"
                .into(),
        );
    }
    let named = !dep.name.is_empty();
    if let Source::Path(p) = &dep.source {
        let file = Path::new(p);
        if file.is_file() {
            if lib {
                return Err(format!(
                    "`{p}` is a script: --lib installs a package's library (src/lib.nabi)"
                ));
            }
            if !named {
                file.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .clone_into(&mut dep.name);
            }
            manifest::check_name(&dep.name)?;
            let abs = nabic_core::real_path(file).map_err(|e| format!("--path {p}: {e}"))?;
            dep.source = Source::Path(abs.display().to_string().replace('\\', "/"));
            return install_bin(g, &dep, &abs, false, again.as_deref());
        }
    }
    if !named {
        dep.name = match &dep.source {
            Source::Path(p) => std::path::absolute(p)
                .ok()
                .and_then(|a| Some(a.file_name()?.to_str()?.to_owned())),
            Source::Git { url, .. } => url
                .trim_end_matches('/')
                .rsplit(['/', ':', '\\'])
                .next()
                .map(|n| n.trim_end_matches(".git").to_owned()),
            Source::Registry { .. } => None,
        }
        .unwrap_or_default();
    }
    pin_newest(g, &mut dep)?;
    let (dir, exact) = resolve::fetch_package(&home(), &dep, Path::new("."), &options(g))?;
    if !named {
        if let Some(n) = manifest::find(&dir)
            .map(|m| manifest::load(&m))
            .transpose()?
            .and_then(|m| m.name)
        {
            dep.name = n;
        }
    }
    manifest::check_name(&dep.name)?;
    let has_lib = dir.join("src/lib.nabi").is_file();
    let has_bin = dir.join("src/main.nabi").is_file();
    if has_lib && has_bin && !lib && !bin {
        return Err(format!(
            "`{}` has an executable and a library (src/lib.nabi): write `nabi install --bin ..` \
             for the executable, or `nabi install --lib ..` to put the library into environment \
             `{env}` (for `nabi repl` / `nabi jupyter`)",
            dep.name
        ));
    }
    if has_bin && !lib {
        if let Source::Path(p) = &dep.source {
            let abs = nabic_core::real_path(p).map_err(|e| format!("--path {p}: {e}"))?;
            dep.source = Source::Path(abs.display().to_string().replace('\\', "/"));
        } else {
            dep.source = exact;
        }
        let copy = !matches!(dep.source, Source::Path(_));
        return install_bin(g, &dep, &dir, copy, again.as_deref());
    }
    if bin {
        return Err(format!(
            "`{}` has no executable (src/main.nabi) for --bin{}",
            dep.name,
            if has_lib {
                format!("; without --bin, its library goes into environment `{env}`")
            } else {
                String::new()
            }
        ));
    }
    if !has_lib {
        return Err(if lib {
            format!(
                "`{}` has no library (src/lib.nabi) for --lib; without --lib, its executable \
                 (src/main.nabi) goes into {}",
                dep.name,
                home().join("bin").display()
            )
        } else {
            format!(
                "`{}` is not a package: no src/lib.nabi (a library) or src/main.nabi (an \
                 executable)",
                dir.display()
            )
        });
    }
    let (path, _) = manifest_to_edit(Some(&env))?;
    let old = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = dep.name.clone();
    add_dep(g, dep, false, Some(&env))?;
    if !prebuild_env(g, &env)? {
        let added = std::fs::read_to_string(&path).ok();
        write_and_resolve(g, &path, added.as_deref(), &old)?;
        return Err(format!(
            "`{name}` did not build in environment `{env}`; nothing was installed"
        ));
    }
    Ok(0)
}

/// Build `src` `--release` into `$NABI_HOME/bin` (from a scratch copy when
/// `copy`, so a cached checkout isn't written) and record `dep`. `again`:
/// skip when `dep` is already installed (not a path).
pub(crate) fn install_bin(
    g: &Global,
    dep: &Dep,
    src: &Path,
    copy: bool,
    again: Option<&str>,
) -> Result<i32, String> {
    let name = &dep.name;
    if PROXIES.contains(&name.as_str()) {
        return Err(format!(
            "`{name}` is the toolchain's own executable; install it under another name: `nabi \
             install <name> ..`"
        ));
    }
    let bin = home().join("bin");
    let exe = bin.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    let record = installs();
    let file = record.display().to_string();
    let old = std::fs::read_to_string(&record)
        .unwrap_or_else(|_| format!("{INSTALLS_HEADER}{}", manifest::EMPTY));
    let prev = read_installs(&old, &file)?
        .into_iter()
        .find(|d| d.name == *name);
    if exe.exists() && prev.is_none() {
        return Err(format!(
            "{} is there and `nabi install` did not put it there; remove it first",
            exe.display()
        ));
    }
    if let Some(again) = again {
        if prev.as_ref() == Some(dep) && exe.exists() && !matches!(dep.source, Source::Path(_)) {
            if !g.quiet {
                eprintln!(
                    "`{name}` {} is installed already ({}); to rebuild it, run `{again}`",
                    installed_as(&dep.source),
                    exe.display()
                );
            }
            return Ok(0);
        }
    }
    std::fs::create_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let id = std::process::id();
    let work = TempEntry::adopt(std::env::temp_dir().join(format!("nabi-install-{id}-{name}")));
    let from = if copy {
        let _ = std::fs::remove_dir_all(work.path());
        toolchain::copy_tree(src, work.path())?;
        work.path()
    } else {
        src
    };
    let tmp = TempEntry::adopt(bin.join(format!(".{name}.{id}{}", std::env::consts::EXE_SUFFIX)));
    let mut c = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
    c.args(&g.color);
    for (on, flag) in [
        (g.offline, "--offline"),
        (g.locked, "--locked"),
        (g.quiet, "-q"),
    ] {
        if on {
            c.arg(flag);
        }
    }
    c.arg("build")
        .arg(from)
        .args(["--release", "-o"])
        .arg(tmp.path());
    if !dep.features.is_empty() {
        c.arg(format!("--features={}", dep.features.join(",")));
    }
    if !dep.default_features {
        c.arg("--no-default-features");
    }
    let built = c.status().map_err(|e| format!("cannot run nabi: {e}"));
    drop(work);
    let built = built?;
    if built.code() == Some(nabic_core::ICE_EXIT) {
        return Err(format!(
            "`{name}` did not build: internal compiler error; nothing was installed"
        ));
    }
    if !built.success() {
        return Err(format!("`{name}` did not build; nothing was installed"));
    }
    std::fs::rename(tmp.path(), &exe).map_err(|e| format!("{}: {e}", exe.display()))?;
    let text = manifest::set_install(&old, &file, dep)?;
    std::fs::write(&record, text).map_err(|e| format!("cannot write {file}: {e}"))?;
    if !g.quiet {
        let new = installed_as(&dep.source);
        let (verb, what) = match prev.map(|p| installed_as(&p.source)) {
            None => ("Installed", format!("{name} {new}")),
            Some(old) if old == new => ("Reinstalled", format!("{name} {new}")),
            Some(old) => match (semver::Version::parse(&old), semver::Version::parse(&new)) {
                (Ok(a), Ok(b)) if a < b => ("Upgraded", format!("{name} from {old} to {new}")),
                (Ok(a), Ok(b)) if a > b => ("Downgraded", format!("{name} from {old} to {new}")),
                _ => ("Replaced", format!("{name} {old} with {new}")),
            },
        };
        term::status(verb, &format!("{what} ({})", exe.display()));
    }
    Ok(0)
}

/// An executable's source as `nabi install` shows it.
pub(crate) fn installed_as(source: &Source) -> String {
    match source {
        Source::Registry { req, .. } => req.trim_start_matches('=').to_owned(),
        Source::Git { url, reference } => match reference.pair() {
            Some((_, r)) => format!("{url} at {}", &r[..r.len().min(12)]),
            None => url.clone(),
        },
        Source::Path(p) => p.clone(),
    }
}

/// Prebuild environment `env`'s libraries into its `target/` by building
/// an empty file, as its REPL would. Whether that worked.
pub(crate) fn prebuild_env(g: &Global, env: &str) -> Result<bool, String> {
    let dir = TempEntry::dir("nabi-prebuild-").map_err(|e| format!("temporary directory: {e}"))?;
    let probe = dir.path().join("probe.nabi");
    let out = probe.with_extension(std::env::consts::EXE_EXTENSION);
    std::fs::write(&probe, "").map_err(|e| format!("{}: {e}", probe.display()))?;
    let args = [
        probe.display().to_string(),
        "-o".to_owned(),
        out.display().to_string(),
    ];
    let built = nabic_command(g, "build", &["-q".to_owned()], &args, Some(env))
        .and_then(|(mut c, _)| c.status().map_err(|e| format!("cannot run nabic: {e}")));
    drop(dir);
    Ok(built?.success())
}

/// `nabi install --list`: environment libraries, then installed
/// executables, `<name> <source>` each.
pub(crate) fn list_installs(g: &Global) -> Result<i32, String> {
    let env = selected_env(g)?;
    let libs = load_dir(&env_dir(&env))?.manifest;
    println!("libraries (environment `{env}`):");
    for d in &libs.deps {
        println!("  {} {}", d.name, d.source.to_nabi());
    }
    for d in &libs.native {
        println!(
            "  {} native {}",
            d.name,
            nabic_nabion::to_line(&d.source.to_value())
        );
    }
    println!("executables ({}):", home().join("bin").display());
    if let Ok(text) = std::fs::read_to_string(installs()) {
        for d in read_installs(&text, &installs().display().to_string())? {
            println!("  {} {}", d.name, d.source.to_nabi());
        }
    }
    Ok(0)
}

/// `nabi uninstall [--native] <name>..`: from the active environment, else
/// from installed executables.
pub(crate) fn cmd_uninstall(g: &Global, args: &[String]) -> Result<i32, String> {
    let native = args.first().is_some_and(|a| a == "--native");
    let names = &args[usize::from(native)..];
    if names.is_empty() || names.iter().any(|n| n.starts_with('-')) {
        return Err("usage: nabi uninstall [--native] <name>..".into());
    }
    let env = selected_env(g)?;
    let manifest = env_dir(&env).join(MANIFEST);
    for name in names {
        if remove_from(g, &manifest, native, name)? {
            if !g.quiet {
                term::status("Removed", &format!("{name} from environment `{env}`"));
            }
            continue;
        }
        let record = installs();
        let file = record.display().to_string();
        let removed = match std::fs::read_to_string(&record) {
            Ok(old) if !native => manifest::remove_install(&old, &file, name)?,
            _ => None,
        };
        let Some(text) = removed else {
            return Err(format!(
                "`{name}` is not installed: environment `{env}` has no library `{name}`, and \
                 `nabi install` put no `{name}` in {} (see `nabi install --list`)",
                home().join("bin").display()
            ));
        };
        let exe = home()
            .join("bin")
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        if exe.exists() {
            std::fs::remove_file(&exe).map_err(|e| format!("{}: {e}", exe.display()))?;
        }
        std::fs::write(&record, text).map_err(|e| format!("cannot write {file}: {e}"))?;
        if !g.quiet {
            term::status("Removed", &format!("{}", exe.display()));
        }
    }
    Ok(0)
}
