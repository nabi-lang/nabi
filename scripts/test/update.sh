#!/bin/sh
# Update an older install to a staged release set: the
# archives dist/native.sh / dist/gnu.sh left in <dist>, served
# over http in the GitHub releases' layout. Three updates, each on a
# fresh NABI_HOME holding the older release:
#
#   - its `nabi toolchain update`, then the staged nabi builds and runs a
#     project the older nabi made and locked (a git dependency), and
#     leaves its nabi.lock in lock format 1;
#   - its install.sh --archive with the staged archive;
#   - the staged archive's own install.sh.
#
#   scripts/test/update.sh <dist>
#
# NABI_UPDATE_FROM names the older release (required), downloaded from
# GitHub; NABI_UPDATE_PORT the local port (default 8765). The staged set
# is signed with a throwaway minisign key that the update trusts with
# `--key`; needs minisign. Unix only; Windows runs
# scripts/test/update.ps1.
set -eu

[ $# -eq 1 ] || {
  echo "usage: scripts/test/update.sh <dist>" >&2
  exit 2
}
dist=$(cd "$1" && pwd)
v=$(cat "$dist/nabi-version")
from=${NABI_UPDATE_FROM:-}
port=${NABI_UPDATE_PORT:-8765}
gh=https://github.com/nabi-lang/nabi

fail() {
  echo "test/update.sh: $*" >&2
  exit 1
}
step() { printf '\n== %s\n' "$*"; }

name=""
for a in "$dist/nabi-$v"-*.tar.gz; do
  [ -f "$a" ] && name=${a##*/} && name=${name%.tar.gz}
done
[ -n "$name" ] || fail "no nabi-$v-<triple>.tar.gz in $dist"
triple=${name#"nabi-$v-"}
[ -n "$from" ] || fail "set NABI_UPDATE_FROM to the release to update from"
# `nabi toolchain update` takes the same version as up to date.
[ "$v" != "$from" ] || fail "the staged set is $v, the release to update from; set NABI_UPDATE_FROM to an older one"
case $v in
  0.*) series=$(echo "$v" | cut -d. -f1,2) ;;
  *) series=${v%%.*} ;;
esac

tmp=$(mktemp -d "${TMPDIR:-/tmp}/nabi-test-update.XXXXXX")
server=""
cleanup() {
  [ -z "$server" ] || kill "$server" 2> /dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT INT TERM
export HOME="$tmp/user" NABI_NO_MODIFY_PATH=1 NO_COLOR=1
mkdir -p "$HOME"
unset NABI_TOOLCHAIN NABI_HOME

step "nabi $from ($triple) from $gh"
base=$tmp/base
mkdir -p "$base"
curl -fsSL -o "$base/install.sh" "https://raw.githubusercontent.com/nabi-lang/nabi/v$from/install.sh"
curl -fsSL -o "$base/nabi-$from-$triple.tar.gz" "$gh/releases/download/v$from/nabi-$from-$triple.tar.gz"
curl -fsSL -o "$base/nabi-$from-$triple.tar.gz.sha256" "$gh/releases/download/v$from/nabi-$from-$triple.tar.gz.sha256"

# A NABI_HOME $1 with nabi $from as the default, installed by its install.sh.
install_from() {
  NABI_HOME=$1 sh "$base/install.sh" -q --archive "$base/nabi-$from-$triple.tar.gz"
  check_default "$1" "$from"
}

# The proxies in $1 run nabi $2 by default, and the toolchain list has it.
check_default() {
  got=$(cd "$tmp" && NABI_HOME=$1 "$1/bin/nabic" --version | head -n 1)
  case $got in
    "nabic $2" | "nabic $2 "*) ;;
    *) fail "$1/bin/nabic --version says '$got', not nabic $2" ;;
  esac
  NABI_HOME=$1 "$1/bin/nabi" toolchain list | grep -q "$2-$triple (default)" ||
    fail "$1: $2-$triple is not the default: $(NABI_HOME=$1 "$1/bin/nabi" toolchain list)"
}

# The staged set, as GitHub serves a release.
step "staged nabi $v on http://127.0.0.1:$port"
serve=$tmp/serve
mkdir -p "$serve/download/v$v" "$serve/latest/download"
cp "$dist/$name".* "$serve/download/v$v/"
cp "$dist/nabi-version" "$serve/latest/download/"
command -v minisign > /dev/null || fail "needs minisign (CI: .github/actions/setup-minisign)"
minisign -G -W -p "$tmp/test.pub" -s "$tmp/test.key" > /dev/null
(cd "$serve/download/v$v" && for f in "$name".tar.*; do
  case $f in *.sha256) ;; *) { sha256sum "$f" 2> /dev/null || shasum -a 256 "$f"; } ;; esac
done > SHA256SUMS)
minisign -S -s "$tmp/test.key" -m "$serve/download/v$v/SHA256SUMS" -t "nabi $v SHA256SUMS" > /dev/null
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$serve" > "$tmp/server.log" 2>&1 &
server=$!
i=0
until curl -fsS -o /dev/null "http://127.0.0.1:$port/latest/download/nabi-version" 2> /dev/null; do
  i=$((i + 1))
  [ "$i" -lt 50 ] || fail "the local server does not answer: $(cat "$tmp/server.log")"
  sleep 0.2
done

step "nabi $from: nabi toolchain update"
h=$tmp/update
install_from "$h"
# A project nabi $from made and locked: a git dependency.
dep=$tmp/greet
mkdir -p "$dep/src"
printf 'pub fn greet() -> i32 {\n    return 1;\n}\n' > "$dep/src/lib.nabi"
git -C "$dep" init -q
git -C "$dep" add .
git -C "$dep" -c user.name=nabi -c user.email=nabi@localhost commit -qm greet
(cd "$tmp" && NABI_HOME=$h "$h/bin/nabi" -q new app)
app=$tmp/app
printf 'use greet::greet;\nprintln(f"{greet()}");\n' > "$app/src/main.nabi"
(cd "$app" && NABI_HOME=$h "$h/bin/nabi" -q add greet --git "$dep")
(cd "$app" && NABI_HOME=$h "$h/bin/nabi" -q build)
[ -f "$app/nabi.lock" ] || fail "nabi $from wrote no nabi.lock"
echo "nabi.lock after nabi $from: version $(sed -n "s/^ *version: *\([0-9]*\).*/\1/p" "$app/nabi.lock" | head -n 1)"

(cd "$tmp" && NABI_HOME=$h NABI_DIST=http://127.0.0.1:$port "$h/bin/nabi" toolchain update --key "$tmp/test.pub")
check_default "$h" "$v"
NABI_HOME=$h "$h/bin/nabi" toolchain list | grep -q "$from-$triple" ||
  fail "nabi $from is no longer installed"

# The project moves to the staged series and builds as it was locked.
sed "s/toolchain: \"[^\"]*\"/toolchain: \"$series\"/" "$app/nabi.nabion" > "$app/nabi.nabion.new"
mv "$app/nabi.nabion.new" "$app/nabi.nabion"
(cd "$app" && NABI_HOME=$h "$h/bin/nabi" build)
out=$(cd "$app" && NABI_HOME=$h "$h/bin/nabi" -q run)
[ "$out" = 1 ] || fail "the project prints '$out', not 1"
[ -f "$app/nabi.lock" ] || fail "nabi $v removed nabi.lock"
lockv=$(sed -n "s/^ *version: *\([0-9]*\).*/\1/p" "$app/nabi.lock" | head -n 1)
echo "nabi.lock after nabi $v: version $lockv"
[ "$lockv" = 1 ] || fail "nabi $v left nabi.lock at version $lockv, not 1"

step "nabi $from: its install.sh --archive $name.tar.xz"
h=$tmp/install-old
install_from "$h"
NABI_HOME=$h sh "$base/install.sh" -q --archive "$dist/$name.tar.xz"
check_default "$h" "$v"

step "nabi $from: $name/install.sh"
h=$tmp/install-new
install_from "$h"
mkdir "$tmp/unpacked"
tar -xzf "$dist/$name.tar.gz" -C "$tmp/unpacked"
NABI_HOME=$h sh "$tmp/unpacked/$name/install.sh" -q
check_default "$h" "$v"

printf '\ntest/update.sh: nabi %s updates to %s (%s)\n' "$from" "$v" "$triple"
