#!/bin/sh
# Build a release toolchain archive:
#
#   dist/nabi-<version>-<triple>.tar.gz     (Windows: .zip)
#   dist/nabi-<version>-<triple>.tar.xz     the same files, about half the
#                                          size (not on Windows); install.sh
#                                          takes it where xz is at hand
#     nabi-<version>-<triple>/
#       bin/{nabi,nabic,nabicc,nabils}                (Windows: .exe)
#       lib/libnabir.a                       (Windows: nabir.lib)
#       lib/nabi/{core,std,bare}/src/      the standard library sources
#       lib/nabi/lldb/nabi.py    lldb formatters for -g programs (nabic --print lldbinit)
#       lib/nabi/gdb/nabi.py     gdb pretty-printers (nabic --print gdbinit)
#       lib/nabi/natvis/nabi.natvis  Visual Studio / WinDbg visualizers
#       lib/nabi/debug/layout.py the collections' layouts both formatters read
#       lib/nabi/targets/<triple>/   what --target links (targets/build.sh)
#       lib/nabi/packages/       the official packages, a read-only cache
#       install.sh install.ps1 README.md CHANGELOG.md LICENSE-MIT LICENSE-APACHE
#       COPYING.mingw-w64 (Windows module definitions embedded in nabic)
#       LICENSE-LLVM.txt (clang's intrinsic headers embedded in nabic)
#       THIRD-PARTY-NOTICES licenses/  what else ships, with its licenses
#
# plus their .sha256 and dist/nabi-version; every file is a release asset.
#
#   scripts/dist/native.sh [--out <dir>]    (Windows: Git Bash in a Developer prompt)
#
# Needs the pinned Rust toolchain, what targets/build.sh needs, gzip and
# xz (not on Windows). The official packages come from the default index
# (git and the network): NABI_DIST_PACKAGES=<dir> copies a
# `nabi component add packages --bundle <dir>` instead, =0 leaves them out
# (a build that ships without them, such as 0.1.0-rc.1). Any other build
# whose package set comes out empty fails.
#
# The archives are reproducible from the commit: every entry has the
# commit's time (SOURCE_DATE_EPOCH, default `git log -1 --format=%ct`),
# owner 0:0, normalized modes, no extended attributes, in sorted order;
# gzip writes no name or time. scripts/check/repro.sh builds twice and
# compares.
set -eu

cd "$(dirname "$0")/../.."
out=dist
case "${1-}" in
  --out) out=$2 ;;
  "") ;;
  *)
    echo "usage: scripts/dist/native.sh [--out <dir>]" >&2
    exit 2
    ;;
esac

# A release's --version names its commit: NABI_COMMIT_HASH and
# NABI_COMMIT_DATE (dist/gnu.sh passes the host's), else git. A
# checkout without .git (`git archive`) has neither.
if [ -z "${NABI_COMMIT_HASH-}" ] || [ -z "${NABI_COMMIT_DATE-}" ]; then
  if h=$(git rev-parse --short=9 HEAD 2> /dev/null) &&
    d=$(git log -1 --format=%cs 2> /dev/null); then
    NABI_COMMIT_HASH=${NABI_COMMIT_HASH:-$h}
    NABI_COMMIT_DATE=${NABI_COMMIT_DATE:-$d}
  else
    echo "dist/native.sh: no commit for --version: run from a git checkout, or set NABI_COMMIT_HASH and" \
      "NABI_COMMIT_DATE (\`git rev-parse --short=9 HEAD\` and \`git log -1 --format=%cs\`" \
      "in the checkout this was made from)" >&2
    exit 1
  fi
fi
export NABI_COMMIT_HASH NABI_COMMIT_DATE
if [ -z "${SOURCE_DATE_EPOCH-}" ]; then
  SOURCE_DATE_EPOCH=$(git log -1 --format=%ct 2> /dev/null) || {
    echo "dist/native.sh: no commit time for the archive entries: run from a git checkout, or set" \
      "SOURCE_DATE_EPOCH (\`git log -1 --format=%ct\` in the checkout this was made from)" >&2
    exit 1
  }
fi
export SOURCE_DATE_EPOCH

# The host's executable suffix, runtime archive and archive format.
case $(uname -s) in
  MINGW* | MSYS* | CYGWIN*)
    exe=.exe rt=nabir.lib ext=zip
    # MSYS's /usr/bin/link (coreutils) would be rustc's link.exe.
    [ -n "${VCToolsInstallDir-}" ] || {
      echo "dist/native.sh: run from a Developer prompt for the MSVC build tools" >&2
      exit 1
    }
    PATH=$(cygpath -u "${VCToolsInstallDir}bin/Host$VSCMD_ARG_HOST_ARCH/$VSCMD_ARG_TGT_ARCH"):$PATH
    # GNU tar reads `C:\..` as a remote host (CI's $RUNNER_TEMP is one).
    out=$(cygpath -u "$out")
    ;;
  *)
    exe="" rt=libnabir.a ext=tar.gz
    ;;
esac

# This machine's form of path $1 (Git Bash: C:\..., as rustc sees it).
native() {
  case $(uname -s) in
    MINGW* | MSYS* | CYGWIN*) cygpath -w "$1" ;;
    *) printf '%s\n' "$1" ;;
  esac
}
# What ships names no path of this machine: the checkout is /nabi, the
# Rust sources /rustc/<commit>, the crates /cargo, and nabic looks for no
# repository library (NABI_DIST). Encoded flags keep spaces in paths.
sep=$(printf '\037')
rustc_commit=$(rustc -vV | sed -n 's/^commit-hash: //p')
flags="--remap-path-prefix=$(native "$(pwd)")=/nabi"
flags="$flags$sep--remap-path-prefix=$(native "$(rustc --print sysroot)/lib/rustlib/src/rust")=/rustc/$rustc_commit"
flags="$flags$sep--remap-path-prefix=$(native "${CARGO_HOME:-$HOME/.cargo}")=/cargo"
for w in ${RUSTFLAGS-}; do flags="$flags$sep$w"; done
CARGO_ENCODED_RUSTFLAGS=$flags NABI_DIST=1
export CARGO_ENCODED_RUSTFLAGS NABI_DIST

# A musl nabic is static: it has no glibc symbol versions to check.
static=""
case $(rustc -vV | sed -n 's/^host: //p') in *-musl) static=1 ;; esac
cargo build --release --locked -p nabi -p nabic -p nabicc -p nabils
# The runtime as one object (fat LTO): only its exports stay global
# (runtime/nabir/localize.sh).
CARGO_PROFILE_RELEASE_LTO=fat cargo build --release --locked -p nabir
./targets/build.sh

target=${CARGO_TARGET_DIR:-target}/release
version=$("$target/nabic$exe" --version | awk '{ print $2 }')
triple=$("$target/nabic$exe" --version --verbose | sed -n 's/^host: //p')
name=nabi-$version-$triple

# install.sh / install.ps1 download this version by default.
grep -q "^version=\"\${NABI_VERSION:-$version}\"" install.sh &&
  grep -q "else { '$version' }" install.ps1 || {
  echo "dist/native.sh: install.sh's or install.ps1's default version is not $version" >&2
  exit 1
}

stage=$out/$name
rm -rf "$stage" "$out/$name".tar.* "$out/$name.$ext" "$out/$name.$ext.sha256"
mkdir -p "$stage/bin" "$stage/lib/nabi"
# Without debug info and local symbols.
mb() { echo "$(($(wc -c < "$1") / 1000000)) MB"; }
for t in nabi nabic nabicc nabils; do
  cp "$target/$t$exe" "$stage/bin/"
  # PE has no symbol table to strip; the linker leaves debug info in the .pdb.
  [ -n "$exe" ] || strip -x "$stage/bin/$t"
  echo "$t: $(mb "$target/$t$exe") -> $(mb "$stage/bin/$t$exe")"
done
runtime/nabir/localize.sh "$target/$rt" "$stage/lib/$rt"
# The library sources.
tar -C library -cf - core/src std/src bare/src lldb gdb natvis debug | tar -C "$stage/lib/nabi" -xf -
cp -R "${CARGO_TARGET_DIR:-target}/lib/nabi/targets" "$stage/lib/nabi/"
case ${NABI_DIST_PACKAGES-} in
  0) ;;
  "")
    pkgs_home=$(mktemp -d)
    NABI_HOME=$pkgs_home "$stage/bin/nabi$exe" component add packages --bundle "$stage/lib/nabi/packages"
    rm -rf "$pkgs_home"
    ;;
  *)
    [ -f "$NABI_DIST_PACKAGES/index" ] || {
      echo "dist/native.sh: $NABI_DIST_PACKAGES has no index (make it with \`nabi component add packages --bundle <dir>\`)" >&2
      exit 1
    }
    cp -R "$NABI_DIST_PACKAGES" "$stage/lib/nabi/packages"
    ;;
esac
# An empty set would ship a toolchain that silently lacks the packages.
if [ "${NABI_DIST_PACKAGES-}" != 0 ]; then
  n=$(grep -c . "$stage/lib/nabi/packages/index" 2> /dev/null || true)
  [ "${n:-0}" -gt 0 ] || {
    echo "dist/native.sh: the official package set is empty (no index lines in lib/nabi/packages/index);" \
      "publish the packages first, or set NABI_DIST_PACKAGES=0 for a build that ships without them" >&2
    exit 1
  }
  echo "packages: $n official package versions"
fi

# No shipped binary names the checkout or the home directory.
leaks=""
for f in "$stage"/bin/* "$stage/lib/$rt" "$stage"/lib/nabi/targets/*/*; do
  [ -f "$f" ] || continue
  if grep -a -q -F -e "$(pwd)" -e "$(native "$(pwd)")" -e "$HOME/" "$f"; then
    leaks="$leaks ${f#"$stage"/}"
  fi
done
[ -z "$leaks" ] || {
  echo "dist/native.sh: these name a path of the build machine ($(pwd) or $HOME):$leaks" >&2
  exit 1
}

# Linux (glibc): what ships needs at most glibc 2.34, as the -gnu
# targets' programs do. A newer build host's glibc leaks its symbol
# versions into the binaries, and its headers (C23 __isoc23_*) into the
# runtimes, which link against their target's stub libc (glibc 2.36).
if [ "$(uname -s)" = Linux ] && [ -z "$static" ]; then
  rustup component add llvm-tools > /dev/null
  tools=$(rustc --print sysroot)/lib/rustlib/$triple/bin
  readelf() { "$tools/llvm-readobj" --elf-output-style=GNU "$@"; }
  # The lines of stdin whose last GLIBC_ version is above 2.34 (or none).
  newer() {
    awk '{ v = $NF; sub(/.*GLIBC_/, "", v); split(v, n, ".")
      if (v !~ /^[0-9.]+$/ || n[1] > 2 || (n[1] == 2 && n[2] > 34)) print }'
  }
  bad=""
  for f in "$stage"/bin/*; do
    vs=$(readelf -V "$f" | grep -o 'GLIBC_[0-9][0-9.]*' | sort -u -t. -k2,2n -k3,3n)
    echo "${f##*/}: $(echo "$vs" | tail -n 1)"
    # Every one needs some version: none read is no check.
    v=$(echo "${vs:-?}" | newer)
    [ -z "$v" ] || bad="$bad${f#"$stage"/}: $(echo $v)\n"
  done
  # Runtime $1's undefined symbols as the stubs in $2 bind them (`@?`:
  # they have none), but the linker's and ld.so's.
  rt_check() {
    v=$({
      readelf --dyn-syms --wide "$2/libc.so" "$2/libm.so" |
        awk '$8 ~ /@@/ { split($8, s, "@@"); print "D", s[1], s[2] }'
      "$tools/llvm-nm" -P --defined-only "$2/libc_nonshared.a" | awk 'NF > 2 { print "D", $1, "GLIBC_2.0" }'
      "$tools/llvm-nm" -P "$1" | awk '$2 == "U" && $1 !~ /^(_GLOBAL_OFFSET_TABLE_|_DYNAMIC|_+tls_get_addr)$/ { print "U", $1 }'
    } | awk '$1 == "D" { v[$2] = $3; next } { print $2 "@" ($2 in v ? v[$2] : "?") }' | newer)
    [ -z "$v" ] || bad="$bad${1#"$stage"/}: $(echo $v)\n"
  }
  for d in "$stage"/lib/nabi/targets/*-linux-gnu*; do
    rt_check "$d/$rt" "$d"
    [ "${d##*/}" != "$triple" ] || rt_check "$stage/lib/$rt" "$d"
  done
  [ -z "$bad" ] || {
    printf 'dist/native.sh: these need a glibc newer than 2.34:\n%b' "$bad" >&2
    echo "dist/native.sh: build the Linux archives with scripts/dist/gnu.sh (in a container with an old glibc)" >&2
    exit 1
  }
fi
# The installer contract: a released `nabi toolchain update` reads
# the version from `nabic <version>` and the host from `host: <triple>`.
v=$("$stage/bin/nabic$exe" --version --verbose)
printf '%s\n' "$v" | head -n 1 | grep -Eq "^nabic $version( |\$)" &&
  printf '%s\n' "$v" | grep -qx "host: $triple" || {
  printf 'dist/native.sh: nabic --version --verbose breaks the installer contract:\n%s\n' "$v" >&2
  exit 1
}
cp install.sh install.ps1 README.md CHANGELOG.md LICENSE-MIT LICENSE-APACHE \
  THIRD-PARTY-NOTICES link/nabild/windows/COPYING.mingw-w64 \
  cc/nabicc_pp/include/LICENSE-LLVM.txt "$stage/"
mkdir -p "$stage/licenses"
cp licenses/*.txt "$stage/licenses/"
cp compiler/nabic_tools/kit/fonts/LICENSE-Geist.txt "$stage/licenses/geist.txt"

# The toolchain runs from its new place, with nothing else on PATH: its
# nabic finds its library and builds with its own backend and linker.
# Its programs need glibc 2.34: an older host (dist/gnu.sh's) only
# links them, and dist/gnu.sh runs them on 2.34.
run=1
case $(getconf GNU_LIBC_VERSION 2> /dev/null) in "glibc 2."[0-9] | "glibc 2."[12][0-9] | "glibc 2.3"[0-3]) run="" ;; esac
printf 'fn main() -> i32 {\n    println("ok");\n    0\n}\n' > "$stage/check.nabi"
for o in -O0 -O2; do
  PATH=/usr/bin:/bin "$stage/bin/nabic$exe" -q build "$stage/check.nabi" $o -o "$stage/check$exe"
  [ -z "$run" ] || [ "$("$stage/check$exe")" = ok ]
  # Every shipped target the native backend generates code for links (it
  # runs only where the machine can). Android links with the NDK's
  # libraries: without ANDROID_NDK_HOME / ANDROID_NDK_ROOT (GitHub's
  # images set it; a FreeBSD VM has none) its link is not checked.
  # shellcheck disable=SC2045 # the target dirs are controlled triples: no spaces or globs
  for t in $(ls "$stage/lib/nabi/targets"); do
    PATH=/usr/bin:/bin "$stage/bin/nabic$exe" --print target-spec --target "$t" |
      grep -q 'native_backend: true' || continue
    case $t in
      *-android)
        [ -n "${ANDROID_NDK_HOME-}${ANDROID_NDK_ROOT-}" ] || {
          echo "dist/native.sh: no Android NDK (ANDROID_NDK_HOME): $t's link not checked"
          continue
        }
        ;;
    esac
    PATH=/usr/bin:/bin "$stage/bin/nabic$exe" -q build "$stage/check.nabi" $o --target "$t" -o "$stage/check$exe"
  done
done
# Its C compiler builds and links a C program the same way.
printf '#include <stdio.h>\nint main(void) { puts("ok"); return 0; }\n' > "$stage/check.c"
PATH=/usr/bin:/bin "$stage/bin/nabicc$exe" -O2 "$stage/check.c" -o "$stage/check$exe"
[ -z "$run" ] || [ "$("$stage/check$exe")" = ok ]
rm -rf "$stage/check.nabi" "$stage/check.c" "$stage/check$exe" "$stage/.check$exe.fp" "$stage/check.pdb" \
  "$stage/check$exe.dSYM" "$stage/lib/nabi-cache"

sum=$(command -v sha256sum || echo "shasum -a 256")
if [ "$ext" != zip ] && ! command -v xz > /dev/null 2>&1; then
  echo "dist/native.sh: xz is needed for the .tar.xz archive (Debian / Ubuntu: apt install xz-utils; macOS: brew install xz)" >&2
  exit 1
fi
exts=$ext
[ "$ext" = zip ] || exts="$ext tar.xz"
scripts/dist/archive.sh "$out" "$name"
for e in $exts; do
  (cd "$out" && $sum "$name.$e" > "$name.$e.sha256")
  echo "archive: $(mb "$out/$name.$e")"
  echo "$out/$name.$e"
done
rm -rf "${out:?}/$name"
# Listing a .zip takes unzip; release/publish.sh checks every archive again.
if [ "$ext" != zip ]; then
  scripts/check/archive.sh "$version" "$out/$name.tar.gz" "$out/$name.tar.xz"
elif command -v unzip > /dev/null 2>&1; then
  scripts/check/archive.sh "$version" "$out/$name.zip"
fi
# The release's version: `nabi toolchain update` reads the newest release's
# from releases/latest/download/nabi-version.
printf '%s\n' "$version" > "$out/nabi-version"
echo "$out/nabi-version"
