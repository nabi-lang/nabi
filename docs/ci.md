# CI

`.github/workflows/ci.yml` and `.github/workflows/nightly.yml` (GitHub Actions). Changes to docs or Markdown alone run `lint` only; a newer run of
the same branch and event cancels the older one.

Jobs are named here by their ids; GitHub shows each job's `name:`, which puts the matrix values in
parentheses in the order host, target, level, shard: `check` is `Check and build tests (linux-x64)`, `corpus`
is `Test corpus (linux-x64, -O0, 1/4)`, `cross` is `Test corpus (linux-x64, x86_64-unknown-linux-musl, -O0)`,
`ci-gate` is `CI gate`. `scripts/release/jobs.sh` writes the same names.

## Tiers

| Event | Hosts | Jobs |
|---|---|---|
| pull request | linux-x64, windows-x64 | `check`, `test`, `corpus` at `-O0` |
| merge queue (`merge_group`), push to `main` | linux-x64, linux-arm64, macos-arm64, windows-x64 | `check`, `test`, `corpus` at `-O0` |
| nightly (`schedule`, 21:00 UTC), manual run | every host | `check`, `test`, `corpus` at `-O0` and `-O2`, `cross-link`, `cross`, `bindgen`, `dist` |

A push to `main` that the merge queue already passed runs `lint` only.

A manual run (`workflow_dispatch`) takes `hosts` (all, or some of `linux-x64 linux-arm64 macos-arm64
windows-x64 windows-arm64`), `jobs` (all, or some of `gate cross bindgen dist`; `none`: plan only) and `filter`
(`NABI_TEST_FILTER`: the integration cases to run; the differential suites always run). Hosts: `ubuntu-24.04` x86_64, `ubuntu-24.04-arm` aarch64, `macos-26`
arm64, `windows-2022` x86_64, `windows-11-arm` aarch64 (nightly and manual runs only). Intel macOS is not a target.

## Jobs

- Every build job starts with `.github/actions/setup-host-env`: on Windows the MSVC environment (`vswhere`) for the
  runner's architecture (`vcvars64.bat` or `vcvarsarm64.bat`; `arch: x64` picks the x64 tools on arm64), on a
  self-hosted runner `CARGO_TARGET_DIR` under `NABI_CI_HOME`. Building needs only the pinned Rust toolchain:
  no LLVM, no prebuilt prefix.
- `check`, per host: fmt, the layout rules (`just check-layout`), clippy, every test binary built and archived (`cargo nextest archive --workspace`,
  with the `nabicc` and `nabils` it builds first, `.config/nextest.toml`; uploaded as `tests-<host>`), doctests, `nabi fmt --check library`, and the compiler / runtime crate
  isolation check (`nabir_abi`, shared by both, aside); every step runs even after one fails. On Linux after
  `targets/build.sh <host>`.
- `test`, per host and part: every test but the two corpora from that archive, `nextest --partition
  hash:i/3`, with no build but `libnabir.a` (`NABI_TEST_PREBUILT=1`). Seven tests build C with `clang`:
  windows-2022 has it on PATH; on windows-11-arm, when the image's clang is missing or not an arm64 one, the
  job installs LLVM 23.1.6's `woa64` MSI (SHA-256 pinned).
- `corpus`, per host, level and shard: the `integration::` and `differential::` tests of nabic's `main` binary
  from the same archive with `NABI_TEST_SHARD=i/4`. A shard takes every fourth integration case of the sorted
  list (and every fourth differential suite); unset, the tests run everything. `NABI_TEST_FILTER` comes from
  the plan: empty (the whole corpus) except in a manual run that sets `filter`. A filter that selects no case
  fails the job. Without a filter, fewer than `MIN_CASES / n` cases run in shard `i/n` (`MIN_CASES / 1`
  unsharded; a case skipped on the target does not count) fail it too, so a lost corpus or an over-eager skip
  cannot pass.
- rustc runs through sccache (`.github/actions/setup-sccache`), cached in the `nabi-lang-artifacts` bucket under
  `sccache/<os>-<arch>/` (expired after 14 days). Pull requests read it; other events write it through the
  role whose ARN is the repository variable `NABI_SCCACHE_ROLE`. Unset, every event only reads it. A cache
  error builds without it.
- A test name filter runs through `scripts/test/filter.sh` (`cargo test` that fails when a filter matches no
  test), and every `nextest run` passes `--no-tests=fail`, so a renamed test cannot drop out of CI.
- Each build and run of an `integration` case, and each run of a `differential` suite, has its own limit:
  600 s to build and 300 s to run, `NABI_TEST_CASE_TIMEOUT=<seconds>` for both, or a case's `// timeout: N`.
  Past it the process is killed and that case fails as `<case>: timed out after Ns`; the other cases go on.
- nextest (`.config/nextest.toml`) kills a test that runs past 8 minutes (15 for the `integration::` corpus shards). The archive's baked paths assume
  every job checks out at the same path, true on GitHub-hosted runners.
- `cross-link` (linux-x64): `targets/build.sh` for the x86_64 and aarch64 Linux targets (glibc and musl),
  then the sysroot link test (`cli::targets::cross_targets_link_from_the_sysroot`).
- `cross`, per target and level: the corpora for `NABI_TEST_TARGET` on a host that runs it:
  `x86_64-unknown-linux-musl` (static) on linux-x64, `aarch64-unknown-linux-musl` (static) on
  linux-arm64, `aarch64-pc-windows-msvc` on windows-arm64 with the x64 build of nabic under emulation (the
  `check`, `test` and `corpus` of windows-arm64 run its native arm64 build).
  The other targets' corpora run in `nightly.yml` (below).
- `bindgen` (linux-x64 and windows-x64, nightly and manual runs only): the `nabi bindgen` tests (`tools::bindgen::`
  of `nabi`'s `main` test binary) against the runner's own system headers (glibc's; the Windows SDK and
  MSVC's). The compiler reads C headers itself; the `test` job runs the same tests.
- `dist` (linux-x64, windows-x64 and windows-arm64, nightly and manual runs only): the release archive.
  linux-x64 builds it with `scripts/dist/gnu.sh` (in a container, as `release.yml` does). Windows builds it with
  `scripts/dist/native.sh` (Git Bash with the MSVC environment of
  `host-env`; `targets/build.sh` falls back to the image's `python`), then `install.ps1 -NoModifyPath` into a scratch `NABI_HOME` and, with only
  its `bin\` on PATH, `nabi run` of a file and a `nabi new --lib --bin` package's build / run / `--release`
  build / test. Both then update the release named by the repository variable `NABI_UPDATE_FROM` (unset until
  there is one; the step is skipped) to that archive, served over a local http server (`scripts/test/update.sh`,
  `scripts/test/update.ps1`; docs/releasing.md "Installer contract"), signed with a throwaway minisign key that
  the update trusts with `--key` / `-Key`.
- `lint` also runs `scripts/test/release.sh`: `release/publish.sh --publish` against a stubbed `gh`, an `-rc.N`
  version a prerelease, a draft made public only once it holds every planned asset, and a moved tag, a
  missing archive or one that breaks the installer contract refused, and a set whose `SHA256SUMS` is missing,
  altered or signed with another key refused (with throwaway minisign keys; `.github/actions/setup-minisign` puts a
  pinned, checksum-verified minisign on PATH).
- `lint` also runs `cargo deny --locked check` (`deny.toml`: licenses, advisories, bans, sources) with a pinned,
  checksum-verified cargo-deny.
- `dco`: every commit of a pull request carries a `Signed-off-by` line (the org's `reusable-dco.yml`).
- `lint` also runs `scripts/test/prep.sh`: `release/prep.sh`, `changelog/lint.sh` and `changelog/pr.sh`
  against a fake history and stubbed `gh` and `cargo`, and `release/ready.sh ci` against fake runs and their
  `expected-jobs` artifacts.
- `plan` uploads the artifact `expected-jobs` (`expected-jobs.json`, from `scripts/release/jobs.sh`): every
  job name the run must pass and whether it is a full run (every host and job, no filter). `release-ready`
  requires a full run that passed all of them.
- `lint` runs `scripts/check/tree.sh` too: a gitlink that `.gitmodules` does not list, or any tracked path
  under `.claude/`, fails it.
- `queue-head` (merge queue only): each commit of the queue group must have the tree its PR's current head
  merges to. A PR pushed again after it was queued fails here and has to be queued again with the new head.
- `nightly-report` (nightly only): when a job of the nightly failed, it opens the issue "Nightly CI is
  failing" (label `type: bug`) with the failed jobs and the run link, or comments on it while it is open;
  the next passing nightly closes it.
- `ci-gate` (check name `CI gate`): the one required status check. It passes when `lint`, `dco`, `plan`, `check`, `test`,
  `corpus` and `queue-head` passed or were skipped, and fails if any failed or was cancelled. A release tag
  needs it green on `main` (docs/releasing.md).

## Self-hosted runners

linux-x64 and windows-x64 run on a self-hosted runner when the repository variable `NABI_LINUX_RUNNER` /
`NABI_WINDOWS_RUNNER` is `self-hosted` (Settings -> Secrets and variables -> Actions -> Variables); otherwise on
GitHub's.

- Labels: `self-hosted, linux, x64` / `self-hosted, windows, x64` (the runner's defaults on that OS and
  architecture).
- Service: configure with `./config.sh` (`config.cmd`), then install it as a service: `sudo ./svc.sh install
  && sudo ./svc.sh start` on Linux; on Windows answer yes to "run as service" in `config.cmd`.
- Persistent directory: set `NABI_CI_HOME` in the runner's `.env` file (next to `config.sh`; restart the
  service), e.g. `NABI_CI_HOME=/var/lib/nabi-ci` or `NABI_CI_HOME=D:\nabi-ci`. It holds one cargo target
  directory per runner (`target/<runner name>`), outside the workspace; give each runner on a machine its own
  name.
- Prerequisites: Linux: rustup, a C compiler (`cc`, Rust's linker), curl, xz, python3. Windows: rustup, Git
  for Windows (bash), Visual Studio or its Build Tools with `VC.Tools.x86.x64` (the MSVC environment comes
  from `vswhere`), python3.
- Security: a self-hosted runner runs whatever a workflow gives it. Pull requests from forks never get one:
  the plan job forces GitHub-hosted runners when `github.event.pull_request.head.repo.full_name !=
  github.repository`. On a public repository also keep
  "Require approval for all outside collaborators" (Settings -> Actions -> General) and put the runners in a
  runner group limited to this repository.

## Nightly schedule

The cron schedule runs only where the repository variable `NABI_NIGHTLY` is `on`, so a private copy pays for nightly runs (macOS included) only when it opts in. nabi-lang/nabi has it on: `-O2` runs there, not in the merge queue.

## nightly.yml

What ci.yml's nightly leaves out, on the same `NABI_NIGHTLY` switch (cron 22:00 UTC; a manual run takes `jobs`
and `filter`). Each job runs the `integration::` and `differential::` corpora unless it says otherwise.

- `wasm` (linux-x64): `scripts/test/wasm.sh`: the three wasm32 targets at `-O0` and `-O2` under wasmtime and
  node, then the CLI tests that run wasm modules.
- `qemu` (linux-x64), per target, level (`-O0`, `-O2`) and shard (2): `i686-unknown-linux-gnu` natively with
  the 32-bit loader (`libc6-i386`); `armv7-unknown-linux-gnueabihf`, `riscv64gc-unknown-linux-gnu`,
  `powerpc64le-unknown-linux-gnu` and `s390x-unknown-linux-gnu` under qemu-user (`NABI_TEST_RUNNER=qemu-<arch>
  -L /usr/<triple>`, binfmt for the programs a case starts). riscv64gc, powerpc64le and s390x are in progress,
  not supported: the job sets `NABI_IN_PROGRESS_TARGETS=1`, the maintainers' switch that lets `--target` take
  them. armv7 is supported: before a release its corpora, and those of `armv7-unknown-linux-musleabihf`, also
  run natively on an arm64 machine whose CPU runs AArch32 code (Neoverse N1), with no runner.
- `i686-windows` (windows-2022): `i686-pc-windows-msvc` natively (WOW64) at `-O0`, `-O2` and `-O2 -g`.
- `freebsd` (linux-x64): `x86_64-unknown-freebsd` at `-O2` in a FreeBSD 14.4 VM under KVM
  (`scripts/test/freebsd.sh`: `up` boots it, `run` is `NABI_TEST_RUNNER` and copies each executable in over ssh).
- `freebsd-dist` (linux-x64): the FreeBSD host archive, as `release-build.yml` builds it: `scripts/dist/freebsd.sh`
  runs `dist/native.sh` in the VM, installs the archive there and builds and runs a `nabi new` project. No
  corpus runs in it.
- `opts` (linux-x64, macos-arm64), per level and shard (2): `-O1`, `-O3`, `-Os` and `-O2 -g`.
- `gc-stress` (linux-x64, linux-arm64), per shard (4): the `integration::` corpus at `-O2` with
  `NABIR_GC_STRESS=50 NABIR_GC_EVAC=always`.
- `report`: as ci.yml's `nightly-report`, with the issue "Nightly target matrix is failing".

Not in CI: `riscv32imac-unknown-none-elf` (in progress; 29 of its cases are expected to fail: they need a C library;
the cases marked `// needs: os` (a clock, time zones, environment variables, an entropy source) are skipped), the
workspace tests on the FreeBSD host (`cargo test --workspace` in the FreeBSD VM: GitHub has no FreeBSD runner, and
a VM build of every test binary does not fit a nightly job), the `-O0` byte-identity and `-O2` / `-O2 -g` code-identity checks against a baseline
compiler, and the benchmarks. These run on the maintainers' test machines before a release.

So does `wasm32-unknown-unknown` in real browsers: its corpora at `-O0` and `-O2` in Chromium, Firefox and
WebKit, through Playwright 1.64.1 (Chromium 156, Firefox 157, WebKit 27.2). `NABI_TEST_WASM_BROWSER` makes the
harness run each module through `scripts/test/browser.mjs` instead of node: one browser per test binary, each
run in a fresh dedicated Worker of a page served from 127.0.0.1 (not cross-origin isolated), stdout, stderr
and the exit code passed back to the harness. Install Playwright into a scratch prefix (git-ignored under
`target/`); `npx playwright install` fetches its browsers when they are not cached yet:

```sh
PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1 npm install --prefix target/playwright playwright@1.64.1
targets/build.sh wasm32-unknown-unknown
for b in chromium firefox webkit; do
  for o in "" -O2; do
    NABI_TEST_TARGET=wasm32-unknown-unknown NABI_TEST_WASM_BROWSER=$b NABI_TEST_OPT=$o \
      NABI_TEST_PLAYWRIGHT=$PWD/target/playwright scripts/test/filter.sh -p nabic --test main -- \
      integration:: differential:: native::abi:: native::ffi::
  done
done
```

The cases marked `// needs: deep-stack` (recursion a native 8 MB stack holds and a browser's engine stack
does not) are skipped there.

Also before a release: `scripts/test/wasi.sh`, the WASI corpora at `-O0` and `-O2` under runtimes other
than wasmtime, through the `targets/wasi/` wrappers as `NABI_TEST_WASM_RUNNER`: `wasm32-wasip2` under jco
1.42.0 (transpiled to JS, on node 24), `wasm32-wasip1-threads` under WAMR 2.4.6's iwasm (its LLVM JIT) and
wasmer 7.5.0, on Linux x86_64. A case a runtime cannot run says so with `// skip-wasm-runner:` and the
reason; `// wasm-invoke:` calls the export under jco too (`jco.sh --invoke`, typed by the
component's WIT from wasm-tools 1.263.0). WasmEdge (no WASI preview 2 host, no wasi-threads) and
wazero (no wasi-threads) run neither target; wasmer runs no components.
