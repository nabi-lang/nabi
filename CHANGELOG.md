# Changelog

## 0.1.2 (2026-11-22)

<!-- begin summary -->
Summary: A fix release: packages whose types go through `any`, such as `spatial` and `sparse`, build again as dependencies.
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- 0.1.1 failed to build a project that depends on `spatial` ("cannot find type `spatial::euler::EulerOrder`"), or on `spatial` and `sparse` together ("cannot find trait `sparse::Preconditioner`"). The code `nabic` writes for a package's types stored in `any` now names them as that package sees them, and a package no longer carries that code for packages it does not depend on.
- `any ::dep::Trait` parses as an `any` bound again; with a renamed dependency, `any al::Trait` failed with "cannot find type `any::alpha::Trait`".
- Changing only which dependencies a package sees now rebuilds it; before, the build reused a stale result.
<!-- end highlights -->

## 0.1.1 (2026-11-22)

<!-- begin summary -->
Summary: A fix release: each package now sees only its own dependencies, and the toolchain bundles the updated official packages.
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- A package sees only the packages it depends on, under the names it gives them. Before, a bare `x::` path could resolve to any package in the build graph, so `table` 0.1.0 failed to build next to the `csv` package. When a dependency and a child module share a name, `x::` is now an error that names both and says to write `::x::..` or `pkg::x::..` (#11).
- The bundled official packages: `table` 0.1.3 (builds in any graph, docs without a `plot` cycle), and `autodiff`, `linalg`, `ode`, `optimize`, `signal`, `sparse` and `stats` 0.1.1, whose tutorials moved into their API docs at docs.nabi-lang.org/packages.
<!-- end highlights -->

## 0.1.0 (2026-11-22)

<!-- begin summary -->
Summary: The first release of Nabi: the compiler, the `nabi` package tool, the language server, the runtime, the standard library and 20 official packages, with Nabi's own code generator and linker for 17 supported targets and release archives for 8 hosts.
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- One toolchain with no LLVM and no system linker: `nabic` generates machine code and links it itself, and each archive ships the runtime of every supported target (the Apple ones in the macOS archive), so `--target <triple>` cross-compiles to the Linux, FreeBSD, Windows, Android and wasm32 targets from any host (Android links with the NDK).
- 17 supported build targets, and release archives for 8 hosts: macOS on Apple Silicon, Windows 11 on x86_64 and aarch64, Linux on x86_64 and aarch64 (glibc, and static musl), and FreeBSD 14 on x86_64.
- `nabi` runs the whole workflow: `new`, `build`, `run`, `test`, `bench`, `fmt`, `lint`, `doc`, `repl`, dependencies with `nabi.lock`, `publish` to the package registry at packages.nabi-lang.org, `toolchain` to install and switch versions, and `bindgen` for C headers.
- 20 official packages on packages.nabi-lang.org (`json`, `http`, `regex`, `toml`, `csv`, `markdown`, `table`, `stats`, `linalg`, `plot` and more) come with the toolchain: code that uses one gets it added to its manifest on first use, with no network.
- `nabils` (`nabi lsp`) for editors, with extensions for VS Code, Vim and Zed; a REPL and a Jupyter kernel (`nabi jupyter install`).
- A generational, moving garbage collector that works with threads; async tasks; a standard library with collections, files, processes, and networking with TLS.

### Platforms

- Supported build targets: x86_64 and aarch64 Linux (glibc, and static musl), macOS on Apple Silicon, x86_64 and aarch64 Windows, x86_64 FreeBSD, i686 Linux and Windows, armv7 Linux (glibc, and static musl), `wasm32-wasip2`, `wasm32-wasip1-threads` and `wasm32-unknown-unknown`, Android on x86_64 (`x86_64-linux-android`, the emulator) and the iOS simulator (`aarch64-apple-ios-sim`).
- Six targets are in progress and come in a later release: Android and iOS devices (`aarch64-linux-android`, `aarch64-apple-ios`), riscv64, powerpc64le and s390x Linux, and `riscv32imac-unknown-none-elf`. `--target` refuses them and names what to build for now.
- Apple targets build only on macOS, with the Xcode Command Line Tools SDK (the iOS simulator with Xcode's iOS SDK).

### Compatibility notes

- Before 1.0 the minor version is the breaking slot: 0.2.0 may break code written for 0.1, while 0.1.x releases add features and fixes without breaking it. Only the latest series gets fixes.
- Programs linked for the `-gnu` Linux targets need glibc 2.34 or newer. On an older system the installer picks the static musl archive.

### Known limits

- The TLS stack and the cryptography in `core::crypto` are Nabi's own code and have had no external audit. Treat them as experimental.
- Releases are signed: `SHA256SUMS` with minisign (key `B33E6FE61BE5314C`, `keys/release.pub`), which `nabi toolchain install` / `update` require and `install.sh` / `install.ps1` check when `minisign` is on PATH, plus GitHub build provenance and SBOM attestations. The binaries themselves are not code-signed (no macOS notarization, no Windows Authenticode): Gatekeeper and SmartScreen warn about an archive downloaded with a browser; the `curl | sh` and `irm | iex` installers are not affected.
- Only the registry's maintainers can publish packages for now; anyone can sign in and use them.
- The toolchain's build cache is capped at 5 GiB; `NABI_CACHE_MAX_SIZE` changes the cap (`off`: none).
<!-- end highlights -->

