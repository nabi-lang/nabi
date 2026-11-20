#!/bin/sh
# Build the FreeBSD release archive: dist/native.sh in a FreeBSD 14.4 VM
# (scripts/test/freebsd.sh), so it is built and checked on the host it
# is for. Then the archive installs (install.sh) into a scratch home in
# the VM, and its nabi makes, builds and runs a new project there.
#
#   scripts/dist/freebsd.sh [--out <dir>]
#
# On x86_64 Linux with KVM and what scripts/test/freebsd.sh needs. The
# working tree (git's files, without .git and target/) goes into the VM;
# git, python3 and curl come from pkg, Rust from rustup-init (pinned) and
# rust-toolchain.toml. The VM lives in NABI_FREEBSD_DIR (default
# <cache>/freebsd-dist, a 40G disk) and keeps them and its target
# directory for the next run; it is stopped at the end.
set -eu

cd "$(dirname "$0")/../.."
out=dist
case "${1-}" in
  --out) out=$2 ;;
  "") ;;
  *)
    echo "usage: scripts/dist/freebsd.sh [--out <dir>]" >&2
    exit 2
    ;;
esac
if [ -z "${NABI_COMMIT_HASH-}" ] || [ -z "${NABI_COMMIT_DATE-}" ]; then
  if h=$(git rev-parse --short=9 HEAD 2> /dev/null) &&
    d=$(git log -1 --format=%cs 2> /dev/null); then
    NABI_COMMIT_HASH=${NABI_COMMIT_HASH:-$h}
    NABI_COMMIT_DATE=${NABI_COMMIT_DATE:-$d}
  else
    echo "dist/freebsd.sh: no commit for --version: run from a git checkout, or set" \
      "NABI_COMMIT_HASH and NABI_COMMIT_DATE (\`git rev-parse --short=9 HEAD\` and" \
      "\`git log -1 --format=%cs\` in the checkout this was made from)" >&2
    exit 1
  fi
fi
# The archive entries' time (dist/native.sh), taken here too.
SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git log -1 --format=%ct 2> /dev/null || true)}
[ -n "$SOURCE_DATE_EPOCH" ] || {
  echo "dist/freebsd.sh: no commit time: run from a git checkout, or set SOURCE_DATE_EPOCH" \
    "(\`git log -1 --format=%ct\` in the checkout this was made from)" >&2
  exit 1
}

RUSTUP=1.28.2
RUSTUP_SHA256=4d9fef2e40731489f3186c61a0f178d54d79864fe3d34791edf5b17f70074956

cache=${NABI_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/nabi}
NABI_FREEBSD_DIR=${NABI_FREEBSD_DIR:-$cache/freebsd-dist}
NABI_FREEBSD_DISK=${NABI_FREEBSD_DISK:-40G}
export NABI_FREEBSD_DIR NABI_FREEBSD_DISK
vm=scripts/test/freebsd.sh
mkdir -p "$out"
out=$(cd "$out" && pwd)

$vm up
trap '$vm down' EXIT INT TERM

# The tools, once: pkg needs root (su: the image's root has no password).
$vm ssh 'su root -c "env ASSUME_ALWAYS_YES=yes pkg install -q git-lite python3 curl"' < /dev/null
$vm ssh "set -eu
  [ -x ~/.cargo/bin/rustup ] && exit 0
  fetch -q -o /tmp/rustup-init https://static.rust-lang.org/rustup/archive/$RUSTUP/x86_64-unknown-freebsd/rustup-init
  [ \"\$(sha256 -q /tmp/rustup-init)\" = $RUSTUP_SHA256 ] || { echo 'dist/freebsd.sh: rustup-init checksum mismatch' >&2; exit 1; }
  chmod +x /tmp/rustup-init
  /tmp/rustup-init -y -q --no-modify-path --profile minimal --default-toolchain none
  rm /tmp/rustup-init" < /dev/null

# The working tree, fresh each run; the target directory stays. A
# checkout without .git (`git archive`) goes in whole but target/ and dist/.
if git rev-parse --git-dir > /dev/null 2>&1; then
  git ls-files -co --exclude-standard | while IFS= read -r f; do
    [ -e "$f" ] && printf '%s\n' "$f"
  done
else
  find . \( -path ./target -o -path ./dist -o -path ./.git \) -prune -o -type f -print
fi | tar -cf - -T - |
  $vm ssh 'set -eu; rm -rf ~/src ~/out; mkdir -p ~/src ~/out; tar -xf - -C ~/src'

# NABI_MIRROR / NABI_DIST_PACKAGES as dist/native.sh takes them; a
# packages directory goes in beside the tree.
packages=${NABI_DIST_PACKAGES-}
case $packages in
  "" | 0) ;;
  *)
    tar -C "$packages" -cf - . | $vm ssh 'set -eu; rm -rf ~/packages; mkdir ~/packages; tar -xf - -C ~/packages'
    packages=/home/freebsd/packages
    ;;
esac
$vm ssh "set -eu
  export PATH=\$HOME/.cargo/bin:\$PATH CARGO_TARGET_DIR=\$HOME/target
  export NABI_COMMIT_HASH=$NABI_COMMIT_HASH NABI_COMMIT_DATE=$NABI_COMMIT_DATE
  export SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH
  export NABI_MIRROR='${NABI_MIRROR-}' NABI_DIST_PACKAGES='$packages'
  cd ~/src
  rustup toolchain install
  scripts/dist/native.sh --out ~/out" < /dev/null

# The archive alone: install.sh into a scratch home, then a new project.
$vm ssh 'set -eu
  rm -rf ~/smoke; mkdir ~/smoke; cd ~/smoke
  name=nabi-$(cat ~/out/nabi-version)-x86_64-unknown-freebsd
  tar -xzf ~/out/$name.tar.gz
  NABI_HOME=$PWD/home NABI_NO_MODIFY_PATH=1 ./$name/install.sh -q
  export PATH=$PWD/home/bin:/usr/bin:/bin
  nabi new hello
  cd hello
  nabi build
  nabi run
  [ "$(nabi run 2> /dev/null)" = "Hello, world!" ]
  echo "$name: installs and runs on $(freebsd-version)"' < /dev/null

$vm ssh 'cd ~/out && tar -cf - .' < /dev/null | tar -xf - -C "$out"
$vm ssh 'rm -rf ~/src ~/out ~/smoke ~/packages' < /dev/null
ls "$out"
