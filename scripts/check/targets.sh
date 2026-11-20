#!/bin/sh
# Fail if the install scripts' triple lists differ from release-targets.json,
# or a build target it lists is not in targets/build.sh's TARGETS (what the
# archives ship). release.yml and release/publish.sh read the manifest;
# install.sh / install.ps1 bake the list in, so check it here (justfile
# `check-targets`, CI lint); check/archive.sh checks the archives built.
set -eu

cd "$(dirname "$0")/../.."

triples() { grep -oE '[a-z0-9_]+(-[a-z0-9_]+){2,3}' | sort -u; }

manifest=$(jq -r '.targets[].triple' release-targets.json | sort -u)
sh_list=$(grep -E '^supported=' install.sh | triples)
ps_list=$(grep -E '^\$supported ' install.ps1 | triples)

fail=0
[ -n "$manifest" ] || {
  echo "check/targets.sh: no targets in release-targets.json" >&2
  fail=1
}
[ "$manifest" = "$sh_list" ] || {
  echo "check/targets.sh: install.sh supported triples differ from release-targets.json" >&2
  fail=1
}
[ "$manifest" = "$ps_list" ] || {
  echo "check/targets.sh: install.ps1 supported triples differ from release-targets.json" >&2
  fail=1
}
[ "$fail" = 0 ] || echo "update the supported-triple list in install.sh and install.ps1 to match release-targets.json" >&2
shipped=$(sed -n '/^TARGETS="/,/"$/p' targets/build.sh | tr -d '"' | sed 's/^TARGETS=//' | tr ' ' '\n' | grep . | sort -u)
for t in $(jq -r '."build-targets"[]' release-targets.json); do
  printf '%s\n' "$shipped" | grep -qx "$t" || {
    echo "check/targets.sh: build target $t (release-targets.json) is not in targets/build.sh's TARGETS; add it there" >&2
    fail=1
  }
done
[ "$fail" = 0 ] || exit 1

echo "release targets consistent across release-targets.json, install.sh, install.ps1, targets/build.sh"
