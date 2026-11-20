# The corpus on phones

The end-to-end corpus (`compiler/nabic/tests/main/integration.rs`) runs on
real phones in AWS Device Farm: Android through an instrumentation test,
iOS through an XCTest bundle. Neither is part of CI; a maintainer runs them
by hand before a release.

## Staging

`NABI_TEST_STAGE=<dir>` makes the corpus build and check every case as
usual but not run it: the program goes to `<dir>/bin/<case>` and what its
runs expect to `<dir>/cases/<case>.json` (exit code, stdout, stderr
patterns, time limit, the workspace prefix cut from panic locations, its
`needs:`, one environment per run: a gc-stress case has three).
Build-time cases (`expect-error`, lints, warnings) and the directive skips
(`only-os`, `needs:` the target lacks, ...) finish on the host, so the
phone gets only what would run. `stage.sh <triple> <out>` does this.

## Android

`android/apk.sh <stage> <out> [a,b]` packs two APKs with the SDK's build
tools, no Gradle:

- `org.nabi_lang.corpus` (`app.apk`) carries each program, debug info
  stripped, as `lib/arm64-v8a/lib<case>.so`. With `extractNativeLibs` the
  installer copies them to `nativeLibraryDir`, the one app directory the
  sandbox may execute from.
- `org.nabi_lang.corpus.tests` (`tests.apk`) is the `Corpus`
  instrumentation, with the JSON files and `android/skip.txt` as assets.
  It runs in the app's process, so the programs it starts have the app's
  uid and SELinux domain.

Android package names cannot hold `-`, so `org.nabi-lang.corpus` is
spelled `org.nabi_lang.corpus`, as Java spells such a domain.

`Corpus` runs the cases, as many at once as the phone has cores. Each run
starts the program with `ProcessBuilder` in a fresh directory in the app's
cache, `TMPDIR` pointing into it, stdin `/dev/null`, stdout and stderr to
files, and is killed at its limit (capped at 120 s). The checks are
`check_run`'s. Each case is reported in `am instrument -r` form and in
`junit.xml` in the app's external files directory. `filter`, `jobs` and
`timeout` arguments narrow a run.

The test spec (`android/spec.yml`) runs the instrumentation and pulls
`junit.xml` into the Customer Artifacts.

## iOS

An iOS app can neither fork nor spawn, so every case runs inside the
XCTest host's process (`ios/Corpus.m`):

- `ios/cc.sh` is the corpus's `NABI_CC`: it links each program as a dylib
  exporting only `main`, with `ios/shim.c` linked in. The host app
  (`ios/Host.m`) carries the dylibs in `Frameworks/`; each gc-stress run
  has a copy of its own (`<case>~1.dylib`), so every run gets a fresh
  runtime: its statics are the dylib's.
- The shim's `exit`, `_exit`, `abort`, `atexit`, `mmap`, `munmap`,
  `pthread_create` and `_tlv_atexit` win over libSystem's for the case's
  code and its runtime, and call hooks the harness sets after `dlopen`.
- A case runs `main` on a thread of its own (8 MiB stack) while the
  test's main thread keeps its run loop going. An exit, an abort or a
  fatal signal (128 + the signal) on that thread runs the case's atexit
  handlers where `exit` would and jumps back to the harness; on another
  thread it ends the case and parks that thread.
- What the process holds once for all cases is the case's while it runs
  and put back after: stdin, stdout, stderr, the working directory, the
  environment, `_NSGetArgv` and every signal action.
- Threads a case leaves behind (an async runtime's workers, a detached
  thread) are ended: one blocked in a wait gets `SIGUSR2` and returns
  from its start routine. Then the case's heap reservation (capped at
  1 GiB) is unmapped and the pthread key dyld took for its runtime's
  thread-local block is deleted. A process has 512 keys and dyld never
  gives an image's back, so without this every case after the first 450
  or so aborted in its first TLS access. A case whose thread keeps
  computing, or whose runtime adopted a thread it did not start (a GCD
  worker), keeps both; the log's last line counts them.
- One test method per case (`Cases.inc`, written by `xctest.sh`). Those
  in `ios/skip.txt` are skipped with the reason.

A crash the harness cannot catch (one inside libSystem, a case whose
threads keep running past their limit and corrupt the process) ends the
run; the cases after it are lost.

`ios/xctest.sh` stages the corpus (`stage`), runs it on a simulator
(`sim`), or builds and packs it for a device (`device`). A device build
is signed when `NABI_DEVELOPMENT_TEAM` names the team, Xcode's automatic
signing making the certificate, App ID and profile it lacks through the
App Store Connect API key that `NABI_ASC_KEY_PATH`, `NABI_ASC_KEY_ID` and
`NABI_ASC_ISSUER_ID` name; without a team it is unsigned. The host's
bundle ID is `NABI_IOS_BUNDLE_ID` (`org.nabi-lang.devicefarm`), the
tests' that with `.tests`. A team without a device gets no development
profile; `NABI_IOS_REGISTER_DEVICE` (a paired iPhone's UDID) builds for
that iPhone and registers it. The app keeps `PlugIns/`, which its
signature seals.

Device Farm does not run the full corpus yet. Every run of it ends
ERRORED within seconds, before any device time: "Signing error with app
or tests ... an error while re-signing your app or test package". Tried,
all refused:

- unsigned (`CODE_SIGNING_ALLOWED=NO`, the dylibs signed ad hoc);
- that re-signed with a development identity, no profile;
- signed with an embedded profile, `PlugIns/` removed (seal broken);
- the same keeping 5 of the dylibs, re-signed with the profile's
  entitlements;
- signed, `PlugIns/` kept, seal intact (`codesign -v --deep --strict`).

Accepted, reaching a device (iPhone 14 Pro Max, iOS 16.6) and stopped
there: a plain host app with a trivial test, and this project signed the
same way with a one-case corpus. Same toolchain (Xcode 27.2, iphoneos27.2
SDK) and team, so neither is the cause; the full corpus is. Not yet
bisected: the 202 gc-stress copies with `~` in their names, the file
count (about 1890 dylibs, 3600 sealed files) and the size (770 MB of
dylibs, a 258 MB `.ipa`).

## Running

```sh
# Android
ANDROID_NDK_HOME=... targets/devfarm/stage.sh aarch64-linux-android target/devfarm/android/stage
ANDROID_HOME=... JAVA_HOME=... targets/android/apk.sh target/devfarm/android/stage target/devfarm/android
targets/devfarm/run.sh android target/devfarm/android/app.apk target/devfarm/android/tests.apk target/devfarm/android/run

# iOS simulator, on a Mac with Xcode
targets/ios/xctest.sh stage aarch64-apple-ios-sim ../ios-sim
targets/ios/xctest.sh sim ../ios-sim <simulator>

# iOS device
targets/ios/xctest.sh stage aarch64-apple-ios ../ios
NABI_DEVELOPMENT_TEAM=<team id> NABI_ASC_KEY_PATH=<AuthKey_ID.p8> \
  NABI_ASC_KEY_ID=<key id> NABI_ASC_ISSUER_ID=<issuer id> \
  targets/ios/xctest.sh device ../ios
targets/devfarm/run.sh ios ../ios/app.ipa ../ios/tests.xctest.zip ../ios/run
```

`run.sh` uploads, schedules on the platform's pool with a job limit
(`NABI_DEVFARM_TIMEOUT`, 30 minutes), waits, downloads the artifacts and
prints `N of M cases ran, K failed` with the failing cases (`report.py`).
Run a small set first (`apk.sh`'s filter, or `NABI_TEST_FILTER` for
`stage`): Device Farm bills by the device minute.
