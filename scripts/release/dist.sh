#!/bin/sh
# The dist-* artifacts of a release commit.
#
#   scripts/release/dist.sh check <dir> <sha> [<packages>]
#       every target's commit-<name> file names <sha> and its
#       packages-<name> file says <packages> (bundled, the default, or
#       none); the files are removed
#   scripts/release/dist.sh find <sha> [<packages>]
#       print the newest run of release-build.yml or release.yml on <sha>
#       that still holds a dist-* artifact for every target, built with
#       <packages> (its run name says "(no packages)" for none), or nothing
#   scripts/release/dist.sh assets <dir>
#       the assets as JSON lines {name, size, sha256}
#
# find needs gh (GH_REPO, GH_TOKEN with actions read).
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)

die() {
  echo "release/dist.sh: $*" >&2
  exit 1
}

sha256_of() {
  { sha256sum "$1" 2> /dev/null || shasum -a 256 "$1"; } | awk '{ print $1 }'
}

check() {
  dir=$1 sha=$2 packages=${3:-bundled}
  want=$(jq '.targets | length' "$root/release-targets.json")
  n=$(find "$dir" -maxdepth 1 -name 'commit-*' | wc -l | tr -d ' ')
  got=$(cat "$dir"/commit-* 2> /dev/null | sort -u)
  if [ "$n" -ne "$want" ] || [ "$got" != "$sha" ]; then
    die "$n of $want builds report a commit ($(echo "$got" | tr '\n' ' ')); the release commit is $sha"
  fi
  rm "$dir"/commit-*
  # A build from before packages-<name> files bundled its packages.
  got=$(for f in $(jq -r '.targets[].name' "$root/release-targets.json"); do
    cat "$dir/packages-$f" 2> /dev/null || echo bundled
  done | sort -u)
  [ "$got" = "$packages" ] ||
    die "the builds are made with packages $(echo "$got" | tr '\n' ' ')but this release asks for $packages; build again with packages=$packages"
  rm -f "$dir"/packages-*
  echo "release/dist.sh: all $want builds are of $sha, packages $packages"
}

find_run() {
  sha=$1 packages=${2:-bundled}
  none=false
  [ "$packages" != none ] || none=true
  : "${GH_REPO:?release/dist.sh: set GH_REPO}"
  names=$(jq -r '.targets[] | "dist-\(.name)"' "$root/release-targets.json" | sort)
  for wf in release-build.yml release.yml; do
    gh api "repos/$GH_REPO/actions/workflows/$wf/runs?head_sha=$sha&per_page=20" \
      --jq ".workflow_runs[] | select(.status == \"completed\")
        | select((.display_title | contains(\"(no packages)\")) == $none) | .id" 2> /dev/null || true
  done | grep -E '^[0-9]+$' | sort -rn | while read -r id; do
    have=$(gh api "repos/$GH_REPO/actions/runs/$id/artifacts?per_page=100" \
      --jq '.artifacts[] | select(.expired | not) | select(.name | startswith("dist-")) | .name' | sort -u)
    if [ "$have" = "$names" ]; then
      echo "$id"
      return 0
    fi
  done
}

assets() {
  for f in "$1"/*; do
    [ -f "$f" ] || continue
    jq -nc --arg n "${f##*/}" --argjson s "$(wc -c < "$f" | tr -d ' ')" --arg h "$(sha256_of "$f")" \
      '{name: $n, size: $s, sha256: $h}'
  done
}

cmd=${1:-}
[ $# -eq 0 ] || shift
case $cmd in
  check)
    [ $# -ge 2 ] && [ $# -le 3 ] || die "usage: check <dir> <sha> [<packages>]"
    check "$@"
    ;;
  find)
    [ $# -ge 1 ] && [ $# -le 2 ] || die "usage: find <sha> [<packages>]"
    find_run "$@"
    ;;
  assets)
    [ $# -eq 1 ] || die "usage: assets <dir>"
    assets "$1"
    ;;
  *) die "usage: release/dist.sh check|find|assets ..." ;;
esac
