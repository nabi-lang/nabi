//! Build script of the runtime crates (`nabir_common`'s includes it):
//! what `target_feature = "atomics"` would say on wasm32, which stable
//! Rust does not let `cfg` see.
//! `nabi_wasi_threads`: wasi-threads (`wasm32-wasip1-threads`); `nabi_solo`:
//! one thread (wasm32 without threads, and targets without an OS);
//! `nabi_flat`: no address space to reserve, the heap's blocks come from
//! `malloc` (wasm32, targets without an OS); `nabi_frames`: panic
//! backtraces walk compiled code's frame pointers (an OS, and inline
//! assembly for the panic entry stubs).

pub fn main() {
    println!("cargo::rustc-check-cfg=cfg(nabi_solo, nabi_wasi_threads, nabi_flat, nabi_frames)");
    let target = std::env::var("TARGET").unwrap_or_default();
    let arch = target.split('-').next().unwrap_or_default();
    let frames = matches!(
        arch,
        "x86_64" | "i686" | "i586" | "aarch64" | "armv7" | "riscv64gc" | "s390x"
    );
    if frames && !target.ends_with("-none-elf") {
        println!("cargo::rustc-cfg=nabi_frames");
    }
    if target.starts_with("wasm32-") || target.ends_with("-none-elf") {
        println!("cargo::rustc-cfg=nabi_flat");
    }
    if target == "wasm32-wasip1-threads" {
        println!("cargo::rustc-cfg=nabi_wasi_threads");
    } else if target.starts_with("wasm32-") || target.ends_with("-none-elf") {
        println!("cargo::rustc-cfg=nabi_solo");
    }
}
