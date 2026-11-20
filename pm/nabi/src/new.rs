//! `nabi new`, `nabi init --script` and `nabi jupyter install`.

#[allow(clippy::wildcard_imports)]
use crate::*;

pub(crate) const MAIN_TEMPLATE: &str = "println(\"Hello, world!\");\n";

pub(crate) const LIB_TEMPLATE: &str = "\
pub fn add(a: i32, b: i32) -> i32 {
    return a + b;
}

#[test]
fn adds() {
    if add(2, 2) != 4 {
        panic(\"add(2, 2) != 4\");
    }
}
";

/// `nabi new [--lib] [--bin] <name>`: a directory with `src/main.nabi`
/// (default), `src/lib.nabi`, or both, and `nabi.nabion`. `init`: the same in
/// an existing directory without those files.
pub(crate) fn cmd_new(g: &Global, cmd: &str, args: &[String]) -> Result<i32, String> {
    let (mut lib, mut bin) = (false, false);
    let mut pos = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--lib" => lib = true,
            "--bin" => bin = true,
            "--script" if cmd == "init" && args.len() == 2 => {
                let file = it.next().ok_or("`--script` needs a value")?;
                return init_script(g, Path::new(file));
            }
            "--script" => return Err("usage: nabi init --script <file.nabi>".into()),
            f if f.starts_with('-') => return Err(format!("unknown option `{f}`")),
            p => pos.push(p),
        }
    }
    let bin = bin || !lib;
    let init = cmd == "init";
    let dir = match (init, pos.as_slice()) {
        (false, [name]) => PathBuf::from(name),
        (true, []) => PathBuf::from("."),
        (true, [d]) => PathBuf::from(d),
        _ => return Err(format!("usage: nabi {cmd} [--lib] [--bin] <name>")),
    };
    // Check the name as typed, before touching the file system: on
    // Windows `nul` names the device, which exists and has no file name.
    if !init {
        if let Some(n) = dir.file_name().and_then(|n| n.to_str()) {
            manifest::check_new_name(n)?;
        }
    }
    if init {
        for f in ["src/main.nabi", "src/lib.nabi"] {
            if dir.join(f).exists() {
                return Err(format!("`{}` already exists", dir.join(f).display()));
            }
        }
    } else if dir.exists() {
        return Err(format!("`{}` already exists", dir.display()));
    }
    // The package name is the directory's.
    let abs = std::path::absolute(&dir).map_err(|e| e.to_string())?;
    let dir_name = abs.file_name().and_then(|n| n.to_str()).unwrap_or("app");
    let name = if init {
        let mut n: String = dir_name
            .to_ascii_lowercase()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        if !n.starts_with(|c: char| c.is_ascii_lowercase())
            || nabic_core::check_os_name("package", &n).is_err()
        {
            n.insert_str(0, "pkg-");
        }
        n.truncate(nabic_core::MAX_NAME);
        if n != dir_name && !g.quiet {
            eprintln!(
                "{} the package name is `{n}` (`{dir_name}` is not a package name)",
                term::label(Stream::Stderr, Role::Note)
            );
        }
        n
    } else {
        manifest::check_new_name(dir_name)?;
        dir_name.to_owned()
    };
    let src = dir.join("src");
    std::fs::create_dir_all(&src).map_err(|e| format!("{}: {e}", src.display()))?;
    // With both, the executable uses the library by the package's name.
    let main = if lib {
        format!(
            "use {}::add;\n\nprintln(f\"2 + 2 = {{add(2, 2)}}\");\n",
            manifest::code_name(&name)
        )
    } else {
        MAIN_TEMPLATE.to_owned()
    };
    for (file, text, on) in [
        ("src/lib.nabi", LIB_TEMPLATE, lib),
        ("src/main.nabi", main.as_str(), bin),
    ] {
        let root = dir.join(file);
        if on {
            std::fs::write(&root, text).map_err(|e| format!("{}: {e}", root.display()))?;
        }
    }
    if manifest::find(&dir).is_none() {
        let value = Value::record(
            Some("Package"),
            vec![
                ("name", Value::str(&name)),
                ("version", Value::str("0.1.0")),
                (
                    "nabi_version",
                    Value::str(VERSION.split(['-', '+']).next().unwrap_or(VERSION)),
                ),
                // Stays on this series when a newer one is the default.
                (
                    "toolchain",
                    Value::str(toolchain::series(VERSION).unwrap_or_else(|| VERSION.to_owned())),
                ),
                ("deps", Value::new(Kind::Dict(Vec::new()))),
            ],
        );
        let path = dir.join(MANIFEST);
        std::fs::write(&path, nabic_nabion::to_string(&value) + "\n")
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        std::fs::write(&ignore, "target/\n").map_err(|e| format!("{}: {e}", ignore.display()))?;
    }
    // Its own git repository, unless already inside one or git is missing.
    let in_repo = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(&dir)
        .output();
    if in_repo.is_ok_and(|o| !o.status.success()) {
        let _ = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status();
    }
    if !g.quiet {
        let what = match (lib, bin) {
            (true, true) => "library and executable",
            (true, false) => "library",
            _ => "executable",
        };
        term::status("Created", &format!("{what} package ({})", dir.display()));
        // What to type next, under the message. Windows PowerShell 5.1
        // has no `&&`: one command per line there.
        let then = if bin { "nabi run" } else { "nabi test" };
        let d = dir.display().to_string();
        let cd = match d.as_str() {
            "." => None,
            d if d.contains(char::is_whitespace) => Some(format!("cd \"{d}\"")),
            d => Some(format!("cd {d}")),
        };
        let lines = match cd {
            None => vec![then.to_owned()],
            Some(cd) if cfg!(windows) => vec![cd, then.to_owned()],
            Some(cd) => vec![format!("{cd} && {then}")],
        };
        for l in lines {
            eprintln!("{:>12} {l}", "");
        }
    }
    Ok(0)
}

/// `nabi init --script <file.nabi>`: turn the script's directory into a
/// package named after it; the merged blocks go into `nabi.nabion` and the
/// files move under `src/` (the script as `main.nabi`) without their blocks.
pub(crate) fn init_script(g: &Global, file: &Path) -> Result<i32, String> {
    if package_dir(file).is_some() || !file.is_file() {
        return Err(format!(
            "`{}` is not a script outside a package",
            file.display()
        ));
    }
    let t = script_target(file, &ambient_env()?)?;
    let root = nabic_core::real_path(file).map_err(|e| e.to_string())?;
    let dir = root.parent().unwrap_or_else(|| Path::new("/"));
    let stem = root.file_stem().and_then(|s| s.to_str()).unwrap_or("app");
    let name = stem.to_ascii_lowercase();
    manifest::check_new_name(&name)?;
    let files = script_files(&root);
    let mut moves = Vec::new();
    for (f, src) in files {
        let to = if f == root {
            dir.join("src/main.nabi")
        } else {
            let rel = f.strip_prefix(dir).map_err(|_| {
                format!(
                    "{} is outside {}: move it there first",
                    f.display(),
                    dir.display()
                )
            })?;
            dir.join("src").join(rel)
        };
        if to.exists() {
            return Err(format!("`{}` already exists", to.display()));
        }
        moves.push((f, to, manifest::set_script_block(&src, None)));
    }
    if dir.join(MANIFEST).exists() {
        return Err(format!("`{}` already exists", dir.join(MANIFEST).display()));
    }
    let deps = t.ctx.manifest.deps.iter().map(|d| {
        let mut d = d.clone();
        if let Source::Path(p) = &d.source {
            let rel = relative(dir, Path::new(p)).display().to_string();
            d.source = Source::Path(rel.replace('\\', "/"));
        }
        (Value::str(&d.name), d.to_value())
    });
    let mut fields = vec![
        ("name", Value::str(&name)),
        ("version", Value::str("0.1.0")),
        ("deps", Value::new(Kind::Dict(deps.collect()))),
    ];
    if !t.ctx.manifest.native.is_empty() {
        let natives = t.ctx.manifest.native.iter().map(|d| {
            let mut s = d.source.clone();
            if let NativeSource::Direct { url, .. } = &mut s {
                if Path::new(url.as_str()).is_absolute() {
                    *url = relative(dir, Path::new(url.as_str())).display().to_string();
                }
            }
            (Value::str(&d.name), s.to_value())
        });
        fields.push(("native", Value::new(Kind::Dict(natives.collect()))));
    }
    let manifest = nabic_nabion::to_string(&Value::record(Some("Package"), fields)) + "\n";
    let write = |p: &Path, t: &str| {
        std::fs::create_dir_all(p.parent().unwrap_or(dir))
            .and_then(|()| std::fs::write(p, t))
            .map_err(|e| format!("{}: {e}", p.display()))
    };
    write(&dir.join(MANIFEST), &manifest)?;
    for (from, to, text) in &moves {
        write(to, text)?;
        std::fs::remove_file(from).map_err(|e| format!("{}: {e}", from.display()))?;
        // A submodule folder left empty goes too.
        if let Some(p) = from.parent().filter(|p| *p != dir) {
            let _ = std::fs::remove_dir(p);
        }
    }
    let ignore = dir.join(".gitignore");
    if !ignore.exists() {
        write(&ignore, "target/\n")?;
    }
    if !g.quiet {
        term::status(
            "Created",
            &format!(
                "executable package `{name}` from {} ({})",
                file.display(),
                dir.display()
            ),
        );
    }
    Ok(0)
}

/// `nabi jupyter install`: the kernel spec (`nabirepl_jupyter::spec`),
/// launching this `nabi`.
pub(crate) fn install_kernel(g: &Global, args: &[String]) -> Result<i32, String> {
    if let Some(a) = args.first() {
        return Err(format!("`nabi jupyter install` takes no `{a}`"));
    }
    if g.env.is_some() {
        selected_env(g)?;
    }
    // The proxy, when installed: the kernel follows the default toolchain.
    let proxy = home()
        .join("bin")
        .join(format!("nabi{}", std::env::consts::EXE_SUFFIX));
    let exe = if proxy.is_file() {
        proxy
    } else {
        std::env::current_exe().map_err(|e| e.to_string())?
    };
    let mut argv = vec![exe.display().to_string()];
    if let Some(e) = &g.env {
        argv.extend(["--env".to_owned(), e.clone()]);
    }
    argv.extend(["jupyter", "--connection-file", "{connection_file}"].map(str::to_owned));
    let (name, display) = match &g.env {
        Some(e) => (format!("nabi-{e}"), format!("nabi ({e})")),
        None => ("nabi".to_owned(), "nabi".to_owned()),
    };
    let file = nabirepl_jupyter::spec::install(&name, display, argv, &home())?;
    if !g.quiet {
        term::status(
            "Installed",
            &format!("kernel `{name}` ({})", file.display()),
        );
    }
    Ok(0)
}
