#!/bin/sh
# Build the static musl release archive: dist/native.sh in Alpine
# (scripts/dist/musl.Containerfile), so nabi, nabic, nabicc, nabils and libnabir.a link
# against musl and run anywhere, Alpine and old glibc included.
#
#   scripts/dist/musl.sh [--out <dir>]
#
# For the host's architecture (x86_64 or aarch64), with podman, else
# docker. The checkout is mounted read-only; rustup, cargo, the
# downloads of targets/build.sh and the target directory live in
# NABI_DIST_CACHE (default ~/.cache/nabi/dist-musl), the archives land
# in <dir> (default dist). Then the installed toolchain builds and runs a
# program in a bare Alpine.
set -eu

cd "$(dirname "$0")/../.."
out=dist
case "${1-}" in
  --out) out=$2 ;;
  "") ;;
  *)
    echo "usage: scripts/dist/musl.sh [--out <dir>]" >&2
    exit 2
    ;;
esac
# The commit for --version, taken here: the container may not see the
# repository (a worktree's .git points outside the mount).
if [ -z "${NABI_COMMIT_HASH-}" ] || [ -z "${NABI_COMMIT_DATE-}" ]; then
  if h=$(git rev-parse --short=9 HEAD 2> /dev/null) &&
    d=$(git log -1 --format=%cs 2> /dev/null); then
    NABI_COMMIT_HASH=${NABI_COMMIT_HASH:-$h}
    NABI_COMMIT_DATE=${NABI_COMMIT_DATE:-$d}
  else
    echo "dist/musl.sh: no commit for --version: run from a git checkout, or set" \
      "NABI_COMMIT_HASH and NABI_COMMIT_DATE (\`git rev-parse --short=9 HEAD\` and" \
      "\`git log -1 --format=%cs\` in the checkout this was made from)" >&2
    exit 1
  fi
fi
# The archive entries' time (dist/native.sh), taken here too.
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2> /dev/null || true)}
[ -n "$SOURCE_DATE_EPOCH" ] || {
  echo "dist/musl.sh: no commit time: run from a git checkout, or set SOURCE_DATE_EPOCH" \
    "(\`git log -1 --format=%ct\` in the checkout this was made from)" >&2
  exit 1
}
export NABI_COMMIT_HASH NABI_COMMIT_DATE

engine=$(command -v podman || command -v docker) || {
  echo "dist/musl.sh: needs podman or docker" >&2
  exit 1
}
cache=${NABI_DIST_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/nabi/dist-musl}
mkdir -p "$out" "$cache"
out=$(cd "$out" && pwd)

# The image is named after its definition.
tag=nabi-dist-musl:$(sha256sum scripts/dist/musl.Containerfile | cut -c1-12)
$engine image inspect "$tag" > /dev/null 2>&1 ||
  $engine build -t "$tag" - < scripts/dist/musl.Containerfile

# NABI_DIST_PACKAGES: a directory goes in read-only (dist/native.sh).
packages=${NABI_DIST_PACKAGES-}
packages_mount=""
case $packages in
  "" | 0) ;;
  *)
    packages_mount="-v $(cd "$packages" && pwd):/packages:ro"
    packages=/packages
    ;;
esac

# Rootless podman maps the container's root to the caller; docker's is
# root, so it runs as the caller.
user=""
case $engine in */docker) user="--user $(id -u):$(id -g)" ;; esac
# shellcheck disable=SC2086
$engine run --rm $user \
  -v "$PWD:/src:ro" -v "$cache:/cache" -v "$out:/out" -w /src \
  -e HOME=/cache/home -e RUSTUP_HOME=/cache/rustup -e CARGO_HOME=/cache/cargo \
  -e NABI_CACHE=/cache/nabi -e CARGO_TARGET_DIR=/cache/target -e TMPDIR=/cache/tmp \
  -e NABI_MIRROR="${NABI_MIRROR-}" -e NABI_DIST_PACKAGES="$packages" $packages_mount \
  -e NABI_COMMIT_HASH="$NABI_COMMIT_HASH" -e NABI_COMMIT_DATE="$NABI_COMMIT_DATE" \
  -e SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  "$tag" sh -euc '
    mkdir -p "$HOME" "$TMPDIR"
    [ -x "$CARGO_HOME/bin/rustup" ] ||
      rustup-init -y -q --no-modify-path --profile minimal --default-toolchain none
    PATH=$CARGO_HOME/bin:$PATH
    rustup toolchain install
    scripts/dist/native.sh --out /out'

# The installed static archive builds and runs a program in a bare
# Alpine, with nothing else there.
name=nabi-$(cat "$out/nabi-version")-$(uname -m)-unknown-linux-musl
# shellcheck disable=SC2086
$engine run --rm $user -v "$out:/out:ro" -e HOME=/tmp \
  docker.io/library/alpine:3.21@sha256:ee16d92f96db08d9d2d07da48555a27f3a69d8f0759021dbc601195ed0d2b5bd \
  sh -euc '
    cd /tmp
    tar -xzf "/out/$1.tar.gz"
    "$1/install.sh" -q
    printf "fn main() {\n    println(\"ok\");\n}\n" >check.nabi
    for o in -O0 -O2; do
      ~/.nabi/bin/nabic -q build check.nabi $o -o check
      [ "$(./check)" = ok ]
    done
    echo "$1: runs on Alpine $(cat /etc/alpine-release)"' sh "$name"
