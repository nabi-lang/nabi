//! `nabi`: see the library. Installed as `$NABI_HOME/bin/{nabi,nabic,nabils}`
//! it is the toolchain proxy.

fn main() {
    nabi::take_guard();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        nabic_diagnostic::end_if_output_closed(info);
        default_hook(info);
        let version = concat!(env!("CARGO_PKG_VERSION"), env!("NABI_COMMIT_INFO"));
        let what = "internal error";
        let host = env!("NABI_HOST");
        eprintln!("{}", nabic_core::ice_report("nabi", what, version, host));
        std::process::exit(nabic_core::ICE_EXIT);
    }));
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if let Some(code) = nabi::proxy(&args) {
        std::process::exit(code);
    }
    std::process::exit(nabi::main_with_os(args));
}
