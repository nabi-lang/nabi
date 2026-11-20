//! `check`, and the front end alone for `nabi lint`.

#[allow(clippy::wildcard_imports)]
use crate::*;

/// `nabic check` (codegen) / `nabi lint` (none); exit 1 on an error or a
/// `deny` lint. A package with both roots checks the library, then the
/// executable on top of it.
pub fn cmd_check(mut a: Args, codegen: bool) {
    // `check -`: the source is stdin, an anonymous package.
    let stdin = a.positional.first().is_some_and(|p| p == "-");
    let pkg = if stdin {
        Package::stdin()
    } else {
        discover_arg(&a)
    };
    prebuild(&mut a, Some(&pkg)).unwrap_or_else(|e| fail(&e));
    a.lint_levels.library = a.check_library;
    let started = started();
    progress(&a, "Checking", &pkg);
    let source = stdin.then(|| {
        let mut src = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut src) {
            fail(&format!("cannot read stdin: {e}"));
        }
        src
    });
    let (mut errors, mut warnings) = (0, 0);
    for target in pkg.library_target().into_iter().chain([pkg.clone()]) {
        if errors > 0 {
            break;
        }
        if let Some(lib) = target.library.as_ref() {
            a.externs.push((lib_name(&target), lib.clone()));
        }
        let mut session = session(&a, &target);
        let (result, diags) = nabic_tools::analysis::check(
            &mut session,
            source.as_deref(),
            &target.root,
            &a.lint_levels,
            codegen.then_some(&a.opts),
        );
        reused(&a, &result);
        diags.iter().for_each(|d| result.report(d));
        let e = diags.iter().filter(|d| d.is_error()).count();
        errors += e;
        warnings += diags.len() - e;
    }
    if !a.quiet && !a.json {
        term::status(
            "Checked",
            &format!("{}: {}", pkg.name, counts(errors, warnings)),
        );
    }
    finished(&a, started);
    process::exit(i32::from(errors > 0));
}
