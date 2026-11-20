//! `nabicc`: see the library.

fn main() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        let tool = nabicc::tool();
        eprintln!(
            "{}",
            nabic_core::ice_report(
                tool,
                "internal compiler error",
                nabicc::VERSION,
                env!("NABI_HOST")
            )
        );
        std::process::exit(nabic_core::ICE_EXIT);
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(nabicc::run(&args));
}
