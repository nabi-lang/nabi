//! `nabi run`: build the executable, then run it (a non-host target's
//! through its runner, `nabic::runner`).

use nabic::{
    Args, build_fresh, create_dir, discover_arg, exe_file, fail, fingerprint, link_exe,
    mount_library, out_dir, prebuild, run_command, runner, script_exe, shown,
};
use nabic_core::term;

pub(crate) fn cmd_run(mut a: Args) {
    let pkg = discover_arg(&a);
    if pkg.lib {
        fail(&format!(
            "`{}` is a library package (`src/lib.nabi`); there is nothing to run",
            pkg.dir.display()
        ));
    }
    prebuild(&mut a, Some(&pkg)).unwrap_or_else(|e| fail(&e));
    mount_library(&mut a, &pkg);
    // Before building: a missing runner says what to install.
    let runner = runner(a.spec()).unwrap_or_else(|e| fail(&e));
    let exe = match &a.output {
        None if pkg.anonymous => script_exe(&a, &pkg),
        o => {
            let exe = o.clone().unwrap_or_else(|| {
                let target = out_dir(&a, Some(&pkg));
                create_dir(&target);
                target.join(exe_file(&a, &pkg.name))
            });
            let fp = fingerprint(&a, &pkg, &exe);
            build_fresh(&a, &pkg, fp, |a, result| {
                link_exe(a, &pkg, result, &exe, &pkg.name);
            });
            exe
        }
    };
    if !a.quiet {
        // A lone file runs from the cache: name the file, `-v` the copy.
        let what = match (pkg.anonymous && a.output.is_none(), a.verbose) {
            (true, 0) => shown(&pkg.root),
            (true, _) => format!("`{}`", exe.display()),
            _ => format!("`{}`", shown(&exe)),
        };
        term::status("Running", &what);
    }
    let mut c = run_command(&runner, &exe);
    c.args(&a.run_args);
    nabic_core::strip_handoffs(&mut c);
    let status = crate::child::status(&mut c)
        .unwrap_or_else(|e| fail(&format!("cannot run {}: {e}", exe.display())));
    crate::child::exit(status);
}
