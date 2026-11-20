//! `install.sh`.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// `sh install.sh` in `root` for a user whose home is `user` and login
/// shell `shell`, with no XDG or zsh directories set.
#[cfg(unix)]
pub(crate) fn install_sh(root: &Path, user: &Path, shell: &str) -> Command {
    fs::create_dir_all(user).unwrap();
    let mut c = Command::new("sh");
    c.arg(workspace().join("install.sh"))
        .current_dir(root)
        .env("HOME", user)
        .env("SHELL", shell)
        .env_remove("NABI_HOME")
        .env_remove("NABI_NO_MODIFY_PATH")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("ZDOTDIR")
        .env_remove("CLICOLOR_FORCE");
    c
}

/// `root/nabi.tar.gz`: a fake toolchain `version`.
#[cfg(unix)]
pub(crate) fn fake_archive(root: &Path, version: &str) {
    let top = format!("nabi-{version}");
    fake_toolchain(&root.join("stage").join(&top), version);
    let tar = Command::new("tar")
        .current_dir(root.join("stage"))
        .args(["-czf", "../nabi.tar.gz", &top])
        .status()
        .unwrap();
    assert!(tar.success());
}

/// The line install.sh adds to a shell startup file.
#[cfg(unix)]
pub(crate) const PROFILE_LINE: &str = "[ -f \"$HOME/.nabi/env\" ] && . \"$HOME/.nabi/env\"";

/// install.sh writes `$NABI_HOME/env`, sources it (guarded) from the
/// startup files that exist and the login shell's, once, and installs
/// the completions; a second run changes nothing.
#[cfg(unix)]
#[test]
fn install_sh_sets_up_path() {
    let root = scratch("install-sh-path");
    let user = root.join("user");
    fake_archive(&root, "9.9.3");
    write(&user.join(".bashrc"), "alias ll='ls -l'");
    let install = || {
        install_sh(&root, &user, "/bin/zsh")
            .args(["--archive", "nabi.tar.gz"])
            .output()
            .unwrap()
    };
    let out = install();
    ok(&out);
    let err = text(&out.stderr);
    assert!(
        err.starts_with("   Installed nabi 9.9.3 to ~/.nabi\n"),
        "{err}"
    );
    assert!(
        err.contains("    Modified ~/.bashrc, ~/.zshenv (PATH)\n"),
        "{err}"
    );
    assert!(err.contains("A new shell finds nabi on PATH"), "{err}");
    assert!(err.contains("  . \"$HOME/.nabi/env\""), "{err}");
    let bashrc = format!("alias ll='ls -l'\n{PROFILE_LINE}\n");
    assert_eq!(read(&user.join(".bashrc")), bashrc);
    assert_eq!(read(&user.join(".zshenv")), format!("{PROFILE_LINE}\n"));
    assert!(!user.join(".profile").exists());
    assert!(!user.join(".bash_profile").exists());
    assert!(!user.join(".config/fish").exists());
    let bash = read(&user.join(".local/share/bash-completion/completions/nabi"));
    assert!(bash.contains("complete -o bashdefault -o default -F _nabi nabi"));
    assert!(read(&user.join(".nabi/zsh/_nabi")).starts_with("#compdef nabi\n"));

    // Sourced twice, env puts the proxies on PATH once.
    let path = |shell: &str| {
        let out = Command::new(shell)
            .args([
                "-c",
                ". \"$HOME/.zshenv\"; . \"$HOME/.nabi/env\"; echo \"$PATH\"",
            ])
            .env("HOME", &user)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        ok(&out).trim().to_owned()
    };
    let want = format!("{}:/usr/bin:/bin", user.join(".nabi/bin").display());
    assert_eq!(path("sh"), want);
    if Command::new("zsh").arg("-fc").arg("true").status().is_ok() {
        assert_eq!(path("zsh"), want);
    }

    // Again: nothing to change, no line twice.
    let out = install();
    ok(&out);
    let err = text(&out.stderr);
    assert!(!err.contains("Modified"), "{err}");
    assert_eq!(read(&user.join(".bashrc")), bashrc);
    assert_eq!(read(&user.join(".zshenv")), format!("{PROFILE_LINE}\n"));

    // Without ~/.nabi the startup files still run.
    fs::remove_dir_all(user.join(".nabi")).unwrap();
    let out = Command::new("sh")
        .args([
            "-c",
            ". \"$HOME/.zshenv\"; . \"$HOME/.bashrc\"; echo started",
        ])
        .env("HOME", &user)
        .output()
        .unwrap();
    assert_eq!(ok(&out), "started\n");
}

/// A bash user with no startup files gets `~/.bashrc` and `~/.profile`
/// (never a new `~/.bash_profile`, which would hide `~/.profile`).
#[cfg(unix)]
#[test]
fn install_sh_bash_files() {
    let root = scratch("install-sh-bash");
    let user = root.join("user");
    fake_archive(&root, "9.9.4");
    let out = install_sh(&root, &user, "/usr/local/bin/bash")
        .args(["--archive", "nabi.tar.gz"])
        .output()
        .unwrap();
    ok(&out);
    let err = text(&out.stderr);
    assert!(
        err.contains("    Modified ~/.bashrc, ~/.profile (PATH)\n"),
        "{err}"
    );
    assert_eq!(read(&user.join(".profile")), format!("{PROFILE_LINE}\n"));
    assert!(!user.join(".bash_profile").exists());
    assert!(!user.join(".zshenv").exists());
}

/// fish: `conf.d/nabi.fish` (under `$XDG_CONFIG_HOME`) sources `env.fish`,
/// and the fish completions go beside it.
#[cfg(unix)]
#[test]
fn install_sh_fish_conf_d() {
    let root = scratch("install-sh-fish");
    let user = root.join("user");
    let xdg = root.join("xdg");
    fake_archive(&root, "9.9.5");
    let install = || {
        install_sh(&root, &user, "/opt/bin/fish")
            .args(["--archive", "nabi.tar.gz"])
            .env("XDG_CONFIG_HOME", &xdg)
            .output()
            .unwrap()
    };
    let out = install();
    ok(&out);
    let err = text(&out.stderr);
    let conf = xdg.join("fish/conf.d/nabi.fish");
    assert!(
        err.contains(&format!("    Modified {} (PATH)\n", conf.display())),
        "{err}"
    );
    assert_eq!(
        read(&conf),
        "test -f \"$HOME/.nabi/env.fish\"; and source \"$HOME/.nabi/env.fish\"\n"
    );
    let comp = read(&xdg.join("fish/completions/nabi.fish"));
    assert!(comp.contains("complete -c nabi "), "{comp}");
    // No sh startup file exists and fish is the login shell: none made.
    assert!(!user.join(".profile").exists());
    let out = install();
    assert!(!text(&out.stderr).contains("Modified"));
    if Command::new("fish").arg("--version").output().is_ok() {
        let out = Command::new("fish")
            .args(["--no-config", "-c"])
            .arg(format!(
                "set -gx PATH /usr/bin /bin; source {0}; source {0}; string join : $PATH",
                conf.display()
            ))
            .env("HOME", &user)
            .output()
            .unwrap();
        let want = format!("{}:/usr/bin:/bin\n", user.join(".nabi/bin").display());
        assert_eq!(ok(&out), want);
        let out = Command::new("fish")
            .args(["--no-config", "-n"])
            .arg(xdg.join("fish/completions/nabi.fish"))
            .output()
            .unwrap();
        ok(&out);
    }
}

/// `--no-modify-path` and `NABI_NO_MODIFY_PATH=1` write no shell files and
/// print what to add to PATH instead.
#[cfg(unix)]
#[test]
fn install_sh_no_modify_path() {
    let root = scratch("install-sh-no-path");
    fake_archive(&root, "9.9.6");
    for (i, how) in ["flag", "env"].into_iter().enumerate() {
        let user = root.join(format!("user{i}"));
        write(&user.join(".profile"), "# mine\n");
        let mut c = install_sh(&root, &user, "/bin/zsh");
        c.args(["--archive", "nabi.tar.gz"]);
        if how == "flag" {
            c.arg("--no-modify-path");
        } else {
            c.env("NABI_NO_MODIFY_PATH", "1");
        }
        let out = c.output().unwrap();
        ok(&out);
        let err = text(&out.stderr);
        assert!(err.contains("Add ~/.nabi/bin to PATH"), "{how}: {err}");
        assert!(!err.contains("Modified"), "{how}: {err}");
        assert_eq!(read(&user.join(".profile")), "# mine\n");
        assert!(!user.join(".zshenv").exists());
        assert!(!user.join(".nabi/env").exists());
        assert!(!user.join(".nabi/zsh").exists());
        assert!(!user.join(".local").exists());
    }
}

/// Without `$SHELL` (containers, some CI) install.sh does not fail.
/// Some `sh`s refill it from the user database, so only the run and
/// `$NABI_HOME/env` are checked.
#[cfg(unix)]
#[test]
fn install_sh_without_shell() {
    let root = scratch("install-sh-no-shell");
    fake_archive(&root, "9.9.7");
    let user = root.join("user");
    let mut c = install_sh(&root, &user, "");
    c.env_remove("SHELL").args(["--archive", "nabi.tar.gz"]);
    let out = c.output().unwrap();
    ok(&out);
    assert!(user.join(".nabi/env").exists(), "{}", text(&out.stderr));
}

/// `install.sh --archive` checks the archive against the `.sha256`
/// beside it before unpacking.
#[cfg(unix)]
#[test]
fn install_sh_checks_sums() {
    let root = scratch("install-sh");
    let home = root.join("home");
    fake_toolchain(&root.join("stage/nabi-9.9.1"), "9.9.1");
    let tar = Command::new("tar")
        .current_dir(root.join("stage"))
        .args(["-czf", "../nabi.tar.gz", "nabi-9.9.1"])
        .status()
        .unwrap();
    assert!(tar.success());
    let install = || {
        install_sh(&root, &root.join("user"), "/bin/sh")
            .args(["--archive", "nabi.tar.gz"])
            .env("NABI_HOME", &home)
            .output()
            .unwrap()
    };
    let sum = root.join("nabi.tar.gz.sha256");
    write(&sum, &format!("{}  nabi.tar.gz\n", "a".repeat(64)));
    fails(&install(), "its .sha256 says aaaa");
    assert!(!home.join("toolchains").exists());
    write(
        &sum,
        &format!("{}  nabi.tar.gz\n", sha256_of(&root.join("nabi.tar.gz"))),
    );
    let out = install();
    assert_eq!(ok(&out), "");
    let err = text(&out.stderr);
    assert!(err.starts_with("   Installed nabi 9.9.1 to "), "{err}");
    assert!(err.contains("Next steps:\n  nabi --version\n"), "{err}");
    let host = env!("NABI_HOST");
    assert!(
        home.join(format!("toolchains/9.9.1-{host}/bin/nabic"))
            .is_file()
    );
}

/// `c`'s stderr on a pseudo-terminal; it must succeed.
#[cfg(unix)]
pub(crate) fn stderr_on_tty(mut c: Command) -> String {
    // SAFETY: plain libc calls on a pseudo-terminal this function owns.
    let (master, slave) = unsafe {
        let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(m >= 0 && libc::grantpt(m) == 0 && libc::unlockpt(m) == 0);
        let name = std::ffi::CStr::from_ptr(libc::ptsname(m)).to_owned();
        let master = <fs::File as std::os::fd::FromRawFd>::from_raw_fd(m);
        let slave = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name.to_str().unwrap())
            .unwrap();
        (master, slave)
    };
    let mut child = c.stderr(slave).spawn().unwrap();
    // The slave closes with the child: the reader then sees EOF or EIO.
    drop(c);
    let reader = std::thread::spawn(move || {
        let mut master = master;
        let mut out = Vec::new();
        let mut b = [0u8; 4096];
        while let Ok(n @ 1..) = master.read(&mut b) {
            out.extend_from_slice(&b[..n]);
        }
        out
    });
    assert!(child.wait().unwrap().success());
    text(&reader.join().unwrap())
}

/// install.sh colours its status lines as nabi does: on a terminal (its
/// stderr, so `curl .. | sh` too) or with `CLICOLOR_FORCE`, never with
/// `NO_COLOR` or `TERM=dumb` or when piped.
#[cfg(unix)]
#[test]
fn install_sh_colours() {
    let root = scratch("install-sh-colour");
    fake_archive(&root, "9.9.4");
    let install = |vars: &[(&str, &str)]| {
        let mut c = install_sh(&root, &root.join("user"), "/bin/sh");
        c.args(["--no-modify-path", "--archive", "nabi.tar.gz"])
            .env("NABI_HOME", root.join("home"))
            .env("TERM", "xterm")
            .env_remove("NO_COLOR")
            .env_remove("COLORTERM")
            .envs(vars.iter().copied())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null());
        c
    };
    let sky = "\x1b[1;96m   Installed\x1b[0m nabi 9.9.4 to ";
    let err = stderr_on_tty(install(&[]));
    assert!(err.starts_with(sky), "{err:?}");
    let err = stderr_on_tty(install(&[("TERM", "xterm-256color")]));
    assert!(
        err.starts_with("\x1b[1;38;5;81m   Installed\x1b[0m "),
        "{err:?}"
    );
    for vars in [&[("NO_COLOR", "1")][..], &[("TERM", "dumb")]] {
        let err = stderr_on_tty(install(vars));
        assert!(err.starts_with("   Installed nabi 9.9.4 to "), "{err:?}");
        assert!(!err.contains('\x1b'), "{vars:?}: {err:?}");
    }
    let piped = |vars: &[(&str, &str)]| {
        let out = install(vars)
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        assert!(out.status.success());
        text(&out.stderr)
    };
    let err = piped(&[]);
    assert!(
        err.starts_with("   Installed nabi 9.9.4 to ") && !err.contains('\x1b'),
        "{err:?}"
    );
    let err = piped(&[("CLICOLOR_FORCE", "1"), ("COLORTERM", "truecolor")]);
    assert!(
        err.starts_with("\x1b[1;38;2;56;189;248m   Installed\x1b[0m "),
        "{err:?}"
    );
    let err = piped(&[("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")]);
    assert!(!err.contains('\x1b'), "{err:?}");
    // Errors: the label is there either way.
    let bad = |vars: &[(&str, &str)]| {
        let out = install_sh(&root, &root.join("user"), "/bin/sh")
            .args(["--archive", "missing.tar.gz"])
            .env("TERM", "xterm")
            .env_remove("NO_COLOR")
            .env_remove("COLORTERM")
            .envs(vars.iter().copied())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        text(&out.stderr)
    };
    assert!(bad(&[]).starts_with("install.sh: error: "), "{}", bad(&[]));
    let err = bad(&[("CLICOLOR_FORCE", "1")]);
    assert!(
        err.starts_with("install.sh: \x1b[1;91merror:\x1b[0m "),
        "{err:?}"
    );
}

/// `install.sh --archive` takes a release's `.tar.xz` as well; `-q`
/// prints nothing.
#[cfg(unix)]
#[test]
fn install_sh_reads_tar_xz() {
    let root = scratch("install-sh-xz");
    let home = root.join("home");
    fake_toolchain(&root.join("stage/nabi-9.9.2"), "9.9.2");
    let tar = Command::new("tar")
        .current_dir(root.join("stage"))
        .args(["-cJf", "../nabi.tar.xz", "nabi-9.9.2"])
        .status()
        .unwrap();
    assert!(tar.success(), "tar -cJf (tar needs xz)");
    let out = install_sh(&root, &root.join("user"), "/bin/sh")
        .args(["-q", "--archive", "nabi.tar.xz"])
        .env("NABI_HOME", &home)
        .output()
        .unwrap();
    assert_eq!(ok(&out), "");
    assert_eq!(text(&out.stderr), "");
    let host = env!("NABI_HOST");
    assert!(
        home.join(format!("toolchains/9.9.2-{host}/bin/nabic"))
            .is_file()
    );
}
