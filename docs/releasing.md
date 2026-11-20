# Releasing

How a `nabi` release is cut. A release ships one host archive set for every
triple in `release-targets.json`, built on GitHub-hosted runners. Nothing
runs on its own: a release starts only when someone dispatches
`release.yml`, and only `release.yml`, through the `nabi-lang-release` App,
creates `v<version>` tags.

## Versioning and cadence

Nabi shifts semantic versioning down one level while the major version is 0,
the way Cargo treats 0.x crates: the minor number is the breaking slot and
the patch number carries features and fixes. A minor bump (`0.2.0`) is for
breaking changes; a patch bump (`0.1.1`, `0.1.2`, ...) adds
backward-compatible features and fixes. 1.0 waits until the language and
standard library are meant to be stable.

Releases are cut on demand, when a version's work is ready; there is no
fixed schedule. A release is cut from `main` and tagged `v<version>`.
There is no release or maintenance branch: only the latest series gets
fixes, so a fix always ships as the next release from `main`. Once a new
series is out, the one before it is no longer supported (no fixes, no
security releases); its archives stay downloadable (`SECURITY.md`). A
version may first go out as a release
candidate (`v<version>-rc.N`) to exercise the pipeline before the final
tag.

A release candidate `X.Y.Z-rc.N` goes through the same flow: dispatch
`release-prepare.yml` and `release.yml` with version `0.1.0-rc.1`. It is always a GitHub
prerelease: `release/publish.sh --publish` passes `--prerelease
--latest=false` for a `-rc.N` version, and `smoke` checks `latest/` does
not name it. GitHub's `latest/` skips prereleases, so `latest/download/nabi-version` keeps naming the last final
release, and a bare `nabi toolchain update` (or `install.sh` / `install.ps1`
without a version) never installs an rc. An rc is installed only by name:
`install.sh --version 0.1.0-rc.1`, `install.ps1 -Version 0.1.0-rc.1` or
`nabi toolchain update 0.1.0-rc.1`. `scripts/test/release.sh` checks this
against a stubbed `gh` (CI `lint`).

A candidate has no CHANGELOG section of its own. `release/prep.sh
X.Y.Z-rc.N` sets `X.Y.Z-rc.N` in `Cargo.toml`, `Cargo.lock`, `install.sh`
and `install.ps1` (commit `release: X.Y.Z-rc.N`, branch
`release/X.Y.Z-rc.N`) and writes the `## X.Y.Z (date)` section exactly as
the final would: the milestone `X.Y.Z`, the PRs since the previous final
tag (candidate tags are skipped), the same hand-written blocks.
`check-changelog.yml` checks that section for the candidate, so its Summary
and Highlights must be written before an rc ships, and the rc's release
notes are that section. The final `X.Y.Z` later refreshes the section
(date, PRs merged since), keeping the hand-written blocks. A candidate of
a version that is already released is refused; the tag ruleset and
`check-release-tag.yml` treat `vX.Y.Z-rc.N` like any `v*` tag.

## Flow

1. Prepare the release commit: Actions, Release / Prepare the branch, Run workflow,
   `version` `X.Y.Z` (below). Write the hand-written blocks on
   `release/X.Y.Z` until `Check / Release changelog` passes.
2. Rehearse it: Actions, Release / Ship a version, Run workflow, `version` `X.Y.Z`, `mode`
   `rehearse` (the default). It runs every stage below on a copy of the
   release commit (`rehearse/X.Y.Z`) and makes nothing public.
3. Release it: the same with `mode` `release`, then approve the one
   deployment review (environment `release`) when the run stops for it.
   Its summary shows the version, the commit, every asset with size and
   SHA-256, and the notes.

`release.yml`, `release-prepare.yml` and nothing else share the concurrency
group `nabi-release`: one runs at a time, the next waits.

A pushed tag is never moved or deleted, and a release is never re-cut. If
something is wrong after the tag, the fix ships as the next patch version.
The `immutable release tags` ruleset enforces it, and `check-release-tag.yml`
opens an `alarm` issue for any `v*` tag the App did not create.

Branches: `main` only, changed through the merge queue (squash) and, for
the release commit, by the App's fast-forward. `release/X.Y.Z` and
`rehearse/X.Y.Z` live for one release; the `release branches` ruleset lets
only the App (and an org admin, to break glass) create or delete them,
while anyone with write access pushes the hand-written blocks to
`release/X.Y.Z`. There are no maintenance branches: only the latest series
gets fixes.

## What release.yml does

Each job first looks at what is already done and skips it, so a run that
failed or was cancelled resumes when the same version and mode are
dispatched again. The record lives in the issue `Release X.Y.Z`
(`Rehearsal X.Y.Z`, labels `release`, `rehearsal`): a table and a JSON
block with the state reached, the evidence of each stage and, after a
failure, `blocked` with the failed jobs.

| Job | State | What it does | Rehearsal |
|---|---|---|---|
| `plan` | PLANNED | checks the version, opens or reuses the record | same |
| `prepare` | PREPARED | `release/prep.sh --push` (a no-op when up to date), or finds `release: X.Y.Z` already on `main`; `scripts/changelog/lint.sh` must pass | on `rehearse/X.Y.Z`, open milestone issues and a draft changelog only warn |
| `land` | LANDED | fast-forwards `main` to the release commit (the App bypasses the `main` ruleset for this push); a `main` that moved fails, and the next dispatch rebases | skipped |
| `ready`, `ci-green` | READY | `release-ready.yml` on the commit (full ci and a `release-build.yml` dry run, passing runs reused), then `main`'s own `CI gate` | no `ci-green` |
| `build-find`, `build`, `collect` | BUILT | reuses the dry run's `dist-*` artifacts (`scripts/release/dist.sh find`), else builds; checks every build is of the commit, `release/publish.sh --dir dist` | same |
| `tag` | TAGGED | the one approval (environment `release`); the SBOM, `SHA256SUMS` and its signature with the release key (Signing, below), checked; then the App creates the annotated tag; a tag already on the commit is kept, one on another commit stops the run | environment `rehearsal`: no reviewer, signs with a key made for the run, checks the tag is free and the App token mints |
| `publish` | PUBLISHED | build provenance and SBOM attestations, then `release/publish.sh --publish`: draft, check, tag check, public; a public release with every asset is left alone | `release/publish.sh --rehearse`: a draft without a tag, checked against the run's key, deleted |
| `smoke` | SMOKED | installs `vX.Y.Z` with `install-nabi` on Linux, macOS and Windows; `latest/download/nabi-version` names it | installs the build run's archives |
| `finish`, `cleanup` | | writes the furthest state, or `blocked`, and comments | closes the record, deletes `rehearse/X.Y.Z` |

`packages` `none` builds archives without the official packages
(Bootstrap, below); `bundled` (the default) is every other release.
The run name says `(no packages)`, and only builds made the same way are
reused.

`mode` `abort` drops an attempt with no tag: it deletes `release/X.Y.Z`
and `rehearse/X.Y.Z` and closes the record as ABANDONED. A commit that
already landed stays on `main` (it is never rewritten); the next
`release-prepare.yml` run makes a new `release: X.Y.Z` commit over it. Once tagged, the
version is burned: ship the next one.

## Recovering

Read the record's `blocked` and the failed job, fix the cause, then
dispatch `release` again with the same version and mode.

- `prepare` failed on the changelog lint: write the Summary and Highlights on
  `release/X.Y.Z`. On open issues or a bad `changelog:` block: close or
  move the issues, fix the PR description.
- `land` failed because `main` moved: dispatch again; prepare rebases.
- `ready` or `ci-green` failed: fix `main` (a red `CI gate`) or rerun the
  failed ci run, then dispatch again; passing runs are reused.
- `build` failed: dispatch again; only a run that holds every `dist-*`
  artifact is reused, so it builds again.
- `tag` stopped on a tag on another commit: someone tagged by hand; the
  version is burned (see the `check-release-tag.yml` alarm), ship the next one.
- `publish` failed after the tag: dispatch again; the leftover draft is
  made again. A public release with other assets is not touched.
- `smoke` failed: the release is public. Find why it does not install;
  the fix ships as the next patch.

## Preparing the release commit

`release-prepare.yml` (manual, from `main`, in the environment `release-prep`)
runs `scripts/release/prep.sh --push X.Y.Z` and pushes as the
`nabi-lang-release` App. Its commits (and those of `release.yml`) are
authored as `nabi-lang-release[bot]` with the bot user's id from the
repository variable `NABI_RELEASE_BOT_ID` (`gh api
'/users/nabi-lang-release%5Bbot%5D' --jq .id` once the App exists); both
workflows stop with an error until it is set. The version follows the previous tag (the next
patch, minor or major); with no tag yet it must be 0.1.0, the first
release, which takes every PR merged so far.
It fails, listing each, when the milestone `X.Y.Z` has open issues or a PR
merged since the previous tag has no valid `changelog:` block (fix the PR
description, then run it again). Otherwise it pushes `release/X.Y.Z`: one
commit `release: X.Y.Z` on `main` that sets the version in `Cargo.toml`,
`Cargo.lock` (`cargo update --workspace --offline`), `install.sh` and
`install.ps1`, and puts on top of `CHANGELOG.md`:

```
## X.Y.Z (YYYY-MM-DD)

<!-- begin summary -->
Summary: ...
<!-- end summary -->

<!-- begin highlights -->
### Highlights
...
<!-- end highlights -->

<!-- begin migration -->     (X.Y.0 only)
### Migration
...
<!-- end migration -->

### Added / ### Changed / ### Fixed / ### Removed
- <the PR's line> (#N)

<!-- silent: #N ... -->
```

The kinds and their lines come from each PR's `changelog:` block (the pull
request template; `Check / PR changelog block` checks it on every PR). Silent lines and
Dependabot PRs go only into the `silent` comment, so every PR stays
accounted for. To change a generated line, edit the PR description and run
`release-prepare.yml` again.

The blocks between the `begin` / `end` markers are written by hand on the
branch (the web editor is enough): the one `Summary:` line (the roadmap and
nabi-lang.org show it), the highlights, and for X.Y.0 the migration notes.
Delete a block that has nothing to say. Every push to `release/*` runs
`check-changelog.yml` (`scripts/changelog/lint.sh X.Y.Z`): the first
section is `## X.Y.Z (YYYY-MM-DD)` and the only one for X.Y.Z, `Cargo.toml`
sets X.Y.Z, no heading is empty, every PR since the previous tag is cited
as `(#N)` or listed silent, the Summary line is written and no `TODO` is
left in a block.

Running `release-prepare.yml` again refreshes the branch: when `main` has moved it
rebases onto it, regenerates the PR lines and keeps the hand-written blocks
(and their deletion). The branch may hold extra commits that touch only the
release files; they are squashed into the one release commit. A branch that
changes anything else is refused. When nothing changed it writes nothing.
`scripts/release/prep.sh X.Y.Z` without `--push` does the same into the
local branch only.

## Targets

`release-targets.json` is the single list of release targets:
`release-build.yml` builds its matrix from it, `scripts/release/publish.sh` verifies exactly its
archives, and `scripts/check/targets.sh` checks the install scripts against
it. Each entry names the triple, the runner image (`runs-on`), the build
script (`kind`: `native` is `dist/native.sh`, `gnu` / `musl` the container
scripts, `freebsd` `dist/freebsd.sh`) and the archive set (`archive`: `unix`
or `win`). Adding or dropping a host is an edit there, not here.

Its `build-targets` are the Supported build targets. Every archive ships
the runtime of each under `lib/nabi/targets/<triple>/` (`libnabir.a`,
Windows targets `nabir.lib`; the archive's own triple's is `lib/`'s), so
every host builds for every one of them; the Apple targets are the
exception, since their runtime is made with Apple's linker:

| Archive | Build targets it covers |
|---|---|
| `aarch64-apple-darwin` | all 17 |
| the other seven | all but `aarch64-apple-darwin` and `aarch64-apple-ios-sim` (15) |

`targets/build.sh`'s `TARGETS` is what goes in: `scripts/check/targets.sh`
fails when a build target is missing there, and `scripts/check/archive.sh`
(run by `dist/native.sh` and `release/publish.sh`) when an archive lacks a
runtime it should carry.

## What a build does

`release-build.yml` builds one commit; `release-ready` dispatches it as the
dry run and `release.yml` reuses its artifacts:

1. `plan`: reads the targets from `release-targets.json`.
2. `build`, one independent job per target, all in parallel (`fail-fast`
   off, so one failure does not cancel the rest):
   - `native` targets (macOS, Windows x64 and arm64) build with
     `dist/native.sh` (on Windows in the MSVC environment of the
     `host-env` action, for the runner's architecture).
   - `gnu` and `musl` targets build in a container: `dist/gnu.sh`
     (binaries that need at most glibc 2.34) and `dist/musl.sh` (static musl), each on a runner
     of its own architecture.
   - FreeBSD has no GitHub runner: `dist/freebsd.sh` boots a FreeBSD 14.4
     VM under KVM on `ubuntu-24.04` (`scripts/test/freebsd.sh`), runs
     `dist/native.sh` in it, then installs the archive there with
     `install.sh` and builds and runs a `nabi new` project. A native build,
     not a cross build from Linux, so the checks `dist/native.sh` makes
     (the toolchain builds and runs a program, `nabicc` too) run on
     FreeBSD itself, and a maintainer runs the same script on any x86_64
     Linux box with KVM.
   - each uploads its `nabi-<version>-<triple>` archives (`.tar.gz` +
     `.tar.xz`; Windows a `.zip`), a `.sha256` beside each, a
     `nabi-version` file, the commit it built and how (`packages-<name>`:
     `bundled` or `none`).
3. `verify`: gathers every archive, checks every build ran on the commit
   the same way (`scripts/release/dist.sh check`) and runs
   `release/publish.sh --dir dist`: every target's archives and checksums
   are present and match, and every toolchain archive keeps the installer
   contract (below). The set is not signed yet.

`release.yml`'s `tag`, after the approval, writes the SBOM
(`scripts/release/sbom.sh`), `SHA256SUMS` and `SHA256SUMS.minisig`
(`scripts/release/sign.sh`) and checks the signature before it tags.
`publish` then records the build provenance attestation and the SBOM
attestation over every archive, writes `dist/notes.md` from the version's
`CHANGELOG.md` section (`scripts/release/notes.sh`) and runs
`release/publish.sh --dir dist --publish --commit <sha>`. That checks the
signed `SHA256SUMS` lists exactly the archives and the SBOM, matches them
and verifies with `keys/release.pub`, checks the tag on origin still
names `<sha>`, creates the `v<version>` release as a draft with those
notes as its body, uploads every planned asset, checks the draft holds
exactly them (every archive, its `.sha256`, `nabi-version`,
`nabi-<version>.cdx.json`, `SHA256SUMS` and `SHA256SUMS.minisig`, each at
its local size), checks the tag again and only then makes the release
public. A draft that fails a
check stays unpublished; `latest/` never sees it. `install.sh` /
`install.ps1` download `v<version>` by default; `nabi-version` is served
from `releases/latest/`.

## Signing

Every release carries:

- `SHA256SUMS`: `<sha256>  <file>` for every archive and the SBOM, the
  form `sha256sum -c` reads;
- `SHA256SUMS.minisig`: its [minisign](https://jedisct1.github.io/minisign/)
  signature (Ed25519, minisign's default prehashed form), trusted comment
  `nabi <version> SHA256SUMS`;
- `nabi-<version>.cdx.json`: a CycloneDX 1.6 SBOM, made by
  `scripts/release/sbom.sh` from `Cargo.lock` and `THIRD-PARTY-NOTICES`;
- GitHub artifact attestations: build provenance and the SBOM, for every
  archive (`gh attestation verify <archive> --repo nabi-lang/nabi`).

The per-file `.sha256` stay: the `install-nabi` action, `scripts/nix.sh`
and `install.sh --archive` read them.

The release key is `keys/release.pub`:

```
untrusted comment: minisign public key B33E6FE61BE5314C
RWRMMeUb5m8+s0rEtzbR47dmyrMdlGyGQSvWUn/Ob5QYh0GHgs1GvJEs
```

- key id `B33E6FE61BE5314C`
- SHA-256 of the 32-byte Ed25519 public key:
  `33f78a3cd04d2fb15f4032b8293ba11c380b56ce79b2a86dde812a1405d1b7b7`

`nabi toolchain install` and `update` build this key in
(`pm/nabi_toolchain/src/sign.rs`, which includes `keys/release.pub`). A
download is installed only when `SHA256SUMS.minisig` verifies with it and
the archive matches its line in `SHA256SUMS`; anything else, a missing
signature included, installs nothing. The verifier (SHA-512, BLAKE2b-512,
Ed25519, minisign's format) is in the tree, verify only, tested against
RFC 8032, RFC 7693 and minisign-made vectors. `install.sh` and
`install.ps1` carry the same key: with `minisign` on PATH they verify the
signature first; without it they check `SHA256SUMS` fetched over HTTPS and
end by saying how to verify by hand:

```sh
minisign -Vm SHA256SUMS -P RWRMMeUb5m8+s0rEtzbR47dmyrMdlGyGQSvWUn/Ob5QYh0GHgs1GvJEs
sha256sum -c --ignore-missing SHA256SUMS
gh attestation verify nabi-<version>-<triple>.tar.xz --repo nabi-lang/nabi
```

To test a set signed with a throwaway key (`scripts/test/update.sh`),
`nabi toolchain install|update --key <file>`, `install.sh --key <file>`
and `install.ps1 -Key <file>` take a minisign public key file instead of
the built-in key and print a warning naming its key id. It is a flag on
purpose: nothing in the environment changes which key is trusted.

The secret key has no password and lives only in the secret
`NABI_MINISIGN_KEY` of the environment `release` (the whole `.key` file,
both lines), so only a job that passed the release approval can read it.
`tag` writes it to a file only the job reads, signs and removes it. The
key is made offline with `minisign -G -W`; the offline copy is the backup.
Code signing of the binaries (macOS notarization, Windows Authenticode) is
not done yet.

Rotation (planned, for example yearly or when a maintainer with access
leaves):

1. Make the new key offline: `minisign -G -W -p new.pub -s new.key`.
2. Commit `keys/release.pub` = `new.pub`, the `release_key` lines of
   `install.sh` and `install.ps1`, and this section, in one PR. Releases
   made from then on carry the new key in their `nabi` and installers.
3. Sign the first release that carries the new key with the old one:
   toolchains from before it verify only the old key. Once it is out, set
   `NABI_MINISIGN_KEY` to `new.key`; the releases after it are signed
   with the new key. Name both key ids in that first release's notes: a
   toolchain older than it updates to it first
   (`nabi toolchain update <that version>`), then on.
4. Keep the old secret offline until no supported release trusts it,
   then destroy it.

Compromise (the secret leaked, or a signature not made by `release.yml`
shows up):

1. Delete `NABI_MINISIGN_KEY` at once; no release can be signed.
2. Make a new key and commit it as in rotation step 2.
3. Publish a security advisory naming the old key id, the window, and the
   releases signed in it; check every one of those archives against its
   build provenance attestation (made by GitHub, not with the key).
4. Ship the next patch release signed with the new key. Clients that
   trust only the old key cannot update automatically: the advisory says
   to reinstall with `install.sh` / `install.ps1` from nabi.sh, after
   checking the new key against this file on GitHub.

## Supply chain checks

`deny.toml` lists what `Cargo.lock` may hold: licenses (MIT, Apache-2.0),
RustSec advisories (any fails, yanked crates too), sources (crates.io
only). CI's `lint` runs `cargo deny --locked check` with a pinned,
checksum-verified cargo-deny. A new dependency under another license adds
it to `allow` in the same change, and its `THIRD-PARTY-NOTICES` entry if it
ships.

## Reproducible archives

`dist/native.sh` makes the archives from the staged tree in a fixed way:
every entry has the release commit's time (`SOURCE_DATE_EPOCH`, from
`git log -1 --format=%ct`; `dist/gnu.sh`, `musl.sh` and `freebsd.sh` pass
it in), owner and group 0, modes 755 or 644, no extended attributes, ACLs
or macOS metadata, in sorted order; `gzip -n` writes no name or time; the
Windows zip comes from `scripts/dist/zip.py` the same way.
`scripts/check/repro.sh` builds this host's archives twice and compares
them (`--clean`: the second build from an empty target directory).

Not reproducible yet, or not checked:

- the binaries are reproducible only on the same build image: the Rust
  toolchain, the container image and the runner image all go into them,
  and only `rust-toolchain.toml` and the Containerfiles' base digests are
  pinned (the Alpine packages and the runner images are not);
- `.tar.xz` uses `xz -T0`, which is the same for any thread count, but
  only for the same xz version;
- the official packages come from the index at build time, so a build
  bundles what the index held then;
- `--clean` (a full rebuild) has not been run on every host.

## Bootstrap: rc, packages, final

The official packages are built and published with a released toolchain,
and a final toolchain bundles them. So the first release goes in three
steps:

1. Release `0.1.0-rc.1` with `packages` `none`: `dist/native.sh` runs with
   `NABI_DIST_PACKAGES=0`, and its archives carry no `lib/nabi/packages`.
   Without that, a build whose package set comes out empty fails, so a
   release never ships without its packages by accident.
2. Publish the 20 official packages to packages.nabi-lang.org, built and
   tested with `0.1.0-rc.1`.
3. Release `0.1.0` with `packages` `bundled`: the build bundles them from
   the index (`nabi component add packages --bundle`).

`release.yml` refuses `packages` `none` for a final version in `release`
mode, so only a candidate can ship without the packages.

## Update pointer

A series is the releases one breaking slot covers: `0.MINOR` while the
major version is 0 (`0.1` is 0.1.0, 0.1.1, ...), then `MAJOR`.

`latest/download/nabi-version` is one line naming the newest release, of
any series; each release uploads it. A bare `nabi toolchain update`
installs what it names and makes it the default, as rustup does, also
when that moves to a new series. The previous toolchain
stays installed, and when the series changed `nabi` says so once: the old
series gets no more fixes, so move projects to the new one; until then a
project can pin it (`toolchain: "0.1"` in `nabi.nabion`, or in a script's
`//! nabion` block) or run `nabi +0.1 ..`. Every released client
reads this file the same way, so it must keep its name and format.

Any other release is installed by its full version:
`nabi toolchain update 0.1.0`, `nabi toolchain install 0.1.0`,
`install.sh --version 0.1.0`, `install.ps1 -Version 0.1.0`. There is no
series lookup and no channel file: an argument that is not a full version
is an error.

## Installer contract

Every released `nabi toolchain update` and install script must be able to
install a later release, so the archive keeps what they rely on:

- the name `nabi-<version>-<triple>.tar.gz` / `.tar.xz` (Windows `.zip`),
  holding one top directory `nabi-<version>-<triple>/`;
- in it `bin/nabi`, `bin/nabic` (`.exe` on Windows), `lib/libnabir.a`
  (Windows `lib/nabir.lib`) and `lib/nabi/core/src/lib.nabi`;
- `nabic --version --verbose` printing `nabic <version>` first and a
  `host: <triple>` line.

`dist/native.sh` checks the `nabic --version --verbose` output of what it
builds; `scripts/check/archive.sh`, run by `dist/native.sh` and by
`release/publish.sh` on every toolchain archive, checks the name and layout, and
that `THIRD-PARTY-NOTICES` and the license texts under `licenses/` ship
with it (one for each C runtime family under `lib/nabi/targets/`). A new
third-party part (a sysroot, a vendored file) gets an entry in
`THIRD-PARTY-NOTICES` and its text in `licenses/`. The nightly `dist`
job (linux-x64 and windows-x64, docs/ci.md) installs the release named by
the repository variable `NABI_UPDATE_FROM` and updates it to the archive it
built, with `nabi toolchain update`, that release's install script and the
new one (`scripts/test/update.sh`, `scripts/test/update.ps1`); then the new
toolchain builds a project the older one made and locked. Until there is a
release to update from, the variable is unset and the step is skipped. Run
it (`jobs: dist`) before tagging.

## Before the tag

- The version must agree across `Cargo.toml` (the source of truth, baked
  into `nabic --version`), `install.sh` and `install.ps1`; `dist/native.sh`
  fails the build if they disagree.
- `CHANGELOG.md` has a dated, non-empty `## <version> (<date>)` section;
  it becomes the release notes, and the release fails without it. An rc
  uses its own `## <version>-rc.N` section if there is one, else the
  section of the final version (`## 0.1.0` for `0.1.0-rc.1`). Check with
  `scripts/release/notes.sh <version>`.
- The release commit (version bump and changelog, from `release-prepare.yml`) goes straight to `main`,
  its `ci.yml` run there has a green `CI gate`, and a `release-ready` run passed
  on it; `release.yml` does all three and stops on the first that fails. The
  `corpus` jobs build and run every integration case: each shard reports
  its share of the corpus, and one that selects no case, or runs fewer than
  its share of the floor (docs/ci.md), fails (an empty `NABI_TEST_FILTER` is
  the whole corpus).
- The packages, the sites and CI version and deploy on their own terms; this
  file covers only the toolchain release.

## After the release

Once `release.yml` reached SMOKED (its record says so):

- Nix: run `scripts/nix.sh <version>`, which rewrites
  `nix/sources.json` from the release's `.sha256` assets, and land it as
  `chore(nix): install <version>` through the merge queue. It cannot be in
  the release commit: the checksums exist only once the archives are
  published. Until the first release the file is a template (`"template":
  true`, zero checksums): the flake refuses to evaluate a package and
  `ci-nix.yml` builds nothing. The first time, also make `flake.lock`, which
  pins nixpkgs: `nix flake lock` in the checkout (any machine with Nix and
  flakes enabled: `nix --extra-experimental-features 'nix-command flakes'
  flake lock`), then `nix build .#nabi` and `nix run . -- --version` to check,
  and commit `flake.lock` with the sources. `nix flake update` moves the pin
  later; `ci-nix.yml` fails without a `flake.lock`.
- Roadmap: close the version's milestone, and move what shipped in
  `roadmap.md` in nabi-lang/www (the nabi-lang.org roadmap). That repository
  deploys on its own; the toolchain release does not touch it.

## Building an archive by hand

The same scripts build one host's archives locally, for a spot check or a
host CI cannot build:

| Host | Machine | Command |
|---|---|---|
| `aarch64-apple-darwin` | an Apple-silicon Mac | `scripts/dist/native.sh --out dist` |
| `x86_64-unknown-linux-gnu` | x86_64 Linux | `scripts/dist/gnu.sh --out dist` |
| `aarch64-unknown-linux-gnu` | arm64 Linux | `scripts/dist/gnu.sh --out dist` |
| `x86_64-pc-windows-msvc` | Windows x86_64 | `scripts/dist/native.sh --out dist` (Git Bash, MSVC environment) |
| `aarch64-pc-windows-msvc` | Windows arm64 | `scripts/dist/native.sh --out dist` (Git Bash, MSVC arm64 environment: `vcvarsarm64.bat`) |
| `x86_64-unknown-linux-musl` | x86_64 Linux | `scripts/dist/musl.sh --out dist` (Alpine container; static) |
| `aarch64-unknown-linux-musl` | arm64 Linux | `scripts/dist/musl.sh --out dist` |
| `x86_64-unknown-freebsd` | x86_64 Linux with KVM | `scripts/dist/freebsd.sh --out dist` (FreeBSD VM) |

The table follows `release-targets.json`; the file wins if they differ.
`dist/gnu.sh` and `dist/musl.sh` run in a container, so one
x86_64 and one arm64 Linux box build every Linux target, and the x86_64
one the FreeBSD archive too. Intel macOS is not a target. Gather the output
of every host into one `dist/` directory and check it with:

```sh
scripts/release/publish.sh --dir dist
```

It verifies that the archives and checksums of every triple in
`release-targets.json` are present and that every `.sha256` matches, and
changes nothing without `--publish`.
