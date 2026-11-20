#!/bin/sh
# Check toolchain archives against the installer contract,
# which every released `nabi toolchain update` and install script relies on:
#
#   nabi-<version>-<triple>.tar.gz / .tar.xz / .zip, one top directory
#   nabi-<version>-<triple>/ holding bin/nabi, bin/nabic (.exe on Windows),
#   lib/libnabir.a (Windows: lib/nabir.lib) and lib/nabi/core/src/lib.nabi;
#   plus THIRD-PARTY-NOTICES with licenses/ holding the text for each
#   third-party part it ships (a C runtime per target family). Beyond the
#   contract, the runtime of every Supported build target
#   (release-targets.json's build-targets): lib/nabi/targets/<triple>/
#   libnabir.a (Windows targets: nabir.lib), the archive's own triple's
#   lib/'s; Apple targets only in an Apple host's archive.
#
#   scripts/check/archive.sh <version> <archive>...
#
# Prints one line per archive and exits 1 if any breaks the contract.
# `nabic --version --verbose` is checked where it runs: dist/native.sh.
set -eu

[ $# -gt 1 ] || {
  echo "usage: scripts/check/archive.sh <version> <archive>..." >&2
  exit 2
}
version=$1
shift

# build-targets, one per line; sed, not jq: dist/native.sh runs this
# where jq may be missing.
build_targets=$(sed -n '/"build-targets"/,/]/p' "$(dirname "$0")/../../release-targets.json" |
  grep -oE '"[a-z0-9_.-]+"' | tr -d '"' | grep -vx build-targets)
[ -n "$build_targets" ] || {
  echo "check/archive.sh: no build-targets in release-targets.json" >&2
  exit 2
}

# The entries of archive $1, one per line, without a leading ./
entries() {
  case $1 in
    *.tar.gz) tar -tzf "$1" ;;
    *.tar.xz) tar -tJf "$1" ;;
    *.zip) unzip -Z1 "$1" ;;
  esac | sed 's|^\./||'
}

bad=0
for a in "$@"; do
  file=${a##*/}
  case $file in
    nabi-*.tar.gz) top=${file%.tar.gz} ;;
    nabi-*.tar.xz) top=${file%.tar.xz} ;;
    nabi-*.zip) top=${file%.zip} ;;
    *)
      echo "breaks    $file: not named nabi-<version>-<triple>.tar.gz, .tar.xz or .zip" >&2
      bad=$((bad + 1))
      continue
      ;;
  esac
  triple=${top#"nabi-$version-"}
  case $triple in
    "$top" | *[!a-z0-9_-]* | *-*-*-*-*) triple="" ;;
    *-*-*) ;;
    *) triple="" ;;
  esac
  [ -n "$triple" ] || {
    echo "breaks    $file: not named nabi-$version-<triple>" >&2
    bad=$((bad + 1))
    continue
  }
  case $triple in
    *-windows-*) exe=.exe rt=nabir.lib ;;
    *) exe="" rt=libnabir.a ;;
  esac
  list=$(entries "$a") || {
    echo "breaks    $file: cannot list it" >&2
    bad=$((bad + 1))
    continue
  }
  why=""
  outside=$(printf '%s\n' "$list" | grep -v "^$top/" | grep -v "^$top\$" | head -n 1 || true)
  [ -z "$outside" ] || why="$outside is outside the top directory $top/"
  needs="bin/nabi$exe bin/nabic$exe bin/nabicc$exe lib/$rt lib/nabi/core/src/lib.nabi THIRD-PARTY-NOTICES
    LICENSE-MIT LICENSE-APACHE LICENSE-LLVM.txt COPYING.mingw-w64 licenses/unicode.txt
    licenses/geist.txt licenses/rust.txt licenses/builtins.txt"
  # A target family's C runtime ships only with its license.
  for fam in linux-musl:musl linux-gnu:glibc wasm32-:wasi freebsd:freebsd; do
    ! printf '%s\n' "$list" | grep -q "^$top/lib/nabi/targets/[^/]*${fam%%:*}" ||
      needs="$needs licenses/${fam#*:}.txt"
  done
  for t in $build_targets; do
    case $t:$triple in
      *-apple-*:*-apple-*) ;;
      *-apple-*:*) continue ;;
    esac
    [ "$t" != "$triple" ] || continue
    case $t in
      *-windows-*) needs="$needs lib/nabi/targets/$t/nabir.lib" ;;
      *) needs="$needs lib/nabi/targets/$t/libnabir.a" ;;
    esac
  done
  for need in $needs; do
    [ -n "$why" ] || printf '%s\n' "$list" | grep -qx "$top/$need" || why="no $top/$need"
  done
  case $why in
    "no $top/lib/nabi/targets/"*) why="$why, the runtime of a Supported build target (add it to targets/build.sh's TARGETS)" ;;
  esac
  if [ -n "$why" ]; then
    echo "breaks    $file: $why" >&2
    bad=$((bad + 1))
  else
    echo "contract  $file"
  fi
done
[ "$bad" -eq 0 ] || {
  echo "check/archive.sh: $bad archive(s) break the installer contract" >&2
  exit 1
}
