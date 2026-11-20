//! `nabi bindgen`: Nabi declarations of a C header.

use nabic::{Args, fail, usage_error};
use nabic_interface::TargetSpec;
use std::path::Path;

/// `nabi bindgen <header>`: one block per symbol.
pub(crate) fn cmd_bindgen(a: &Args) {
    let [header] = a.positional.as_slice() else {
        usage_error(
            "usage: nabi bindgen <header> [-x objective-c] [--target <t>] [-o <file>] [-- <C \
             compiler args>]",
        );
    };
    let spec = a.spec();
    if a.objc && spec.vendor != "apple" {
        usage_error(&format!(
            "`-x objective-c` reads Apple SDK headers, and {} is not an Apple target: \
             give `--target aarch64-apple-darwin`",
            spec.name
        ));
    }
    let sdk = (spec.vendor == "apple")
        .then(|| nabic_target::apple::sdk_for(spec))
        .transpose()
        .unwrap_or_else(|e| fail(&e));
    let clang_args: Vec<String> = a
        .run_args
        .iter()
        .enumerate()
        .map(|(i, x)| match x.to_str() {
            Some(x) => x.to_owned(),
            None => usage_error(&format!(
                "C compiler argument {} (`{}`) is not valid UTF-8",
                i + 1,
                x.to_string_lossy()
            )),
        })
        .collect();
    let triple = spec.c_triple();
    let opts = nabicc_bindgen::Options {
        header: Path::new(header),
        target: &spec.name,
        info: nabicc_bindgen::TargetInfo {
            triple: &triple,
            arch: &spec.arch,
            os: &spec.os,
            env: &spec.env,
            vendor: &spec.vendor,
            pointer_width: spec.pointer_width,
            big_endian: spec.endian == "big",
        },
        stdcall: spec.stdcall_system(),
        sdk,
        clang_args: &clang_args,
        shim: a.c_shim.is_some(),
        objc: a.objc,
        roots: &[],
        // Host headers serve the host, every Apple target (the SDK has
        // them all) and every MSVC target on an MSVC host; others come
        // from the arguments.
        system_headers: spec.is_host()
            || spec.vendor == "apple"
            || spec.env == "msvc" && TargetSpec::host().env == "msvc"
            || clang_args.iter().any(|x| x.starts_with("--sysroot")),
    };
    let (out, shim) = nabicc_bindgen::generate_with_shim(&opts).unwrap_or_else(|e| fail(&e));
    if let Some(p) = &a.c_shim {
        std::fs::write(p, shim).unwrap_or_else(|e| fail(&format!("{}: {e}", p.display())));
    }
    match &a.output {
        Some(p) => {
            std::fs::write(p, out).unwrap_or_else(|e| fail(&format!("{}: {e}", p.display())));
        }
        None => print!("{out}"),
    }
}
