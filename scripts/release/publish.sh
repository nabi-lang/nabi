#!/bin/sh
# Check a collected set of release archives and, with --publish, cut the
# GitHub release.
#
#   scripts/release/publish.sh [--dir <dir>] [--publish | --rehearse <name>] [--commit <sha>]
#                              [--pubkey <file>]
#
# Gather every host's archives, their .sha256 sidecars and nabi-version into
# one <dir> (default dist). Without --publish it only checks the full set is
# present and verifies, and that every toolchain archive keeps the installer
# contract (scripts/check/archive.sh), so a release never ships a missing
# archive or one a released client cannot install. The SBOM
# (nabi-<version>.cdx.json), SHA256SUMS and SHA256SUMS.minisig are checked
# when there (scripts/release/sign.sh verify, with --pubkey, default
# keys/release.pub); --publish and --rehearse need them. --publish needs the tag
# v<nabi-version> already on origin and still on <sha> (the commit the
# archives were built from, default HEAD). It uploads the planned assets to
# a draft release with <dir>/notes.md (scripts/release/notes.sh) as the
# body, checks the draft holds exactly them, checks the tag again and only
# then makes the release public. A rerun resumes: a published release
# holding exactly the planned assets is left alone, a draft is dropped and
# made again. --rehearse does the same into a draft called <name> on
# <sha>, with no tag, and deletes it after the check. A -rc.N version is
# published as a prerelease, so it never becomes latest/ and a bare
# `nabi toolchain update` never installs it. A tag build whose tag is not v<nabi-version> fails. It
# does not deploy the sites or publish packages.
set -eu

dir=dist
publish=""
rehearse=""
commit=""
pubkey=$(dirname "$0")/../../keys/release.pub
while [ $# -gt 0 ]; do
  case $1 in
    --dir)
      dir=$2
      shift 2
      ;;
    --publish)
      publish=1
      shift
      ;;
    --rehearse)
      publish=1
      rehearse=$2
      shift 2
      ;;
    --commit)
      commit=$2
      shift 2
      ;;
    --pubkey)
      pubkey=$2
      shift 2
      ;;
    *)
      echo "usage: scripts/release/publish.sh [--dir <dir>] [--publish | --rehearse <name>] [--commit <sha>] [--pubkey <file>]" >&2
      exit 2
      ;;
  esac
done

[ -f "$dir/nabi-version" ] || {
  echo "release/publish.sh: no $dir/nabi-version; build the archives into $dir first (docs/releasing.md)" >&2
  exit 1
}
version=$(cat "$dir/nabi-version")
tag=v$version

# A tag build must ship the version it was tagged for.
if [ "${GITHUB_REF_TYPE-}" = tag ] && [ "${GITHUB_REF_NAME-}" != "$tag" ]; then
  echo "release/publish.sh: tag $GITHUB_REF_NAME but $dir/nabi-version is $version; tag the commit that sets $version, or fix the version" >&2
  exit 1
fi

# <triple> <archive> per target, from release-targets.json (the single
# source of truth). unix: .tar.gz + .tar.xz. win: .zip.
targets=$(jq -r '.targets[] | "\(.triple) \(.archive)"' \
  "$(dirname "$0")/../../release-targets.json")
bad=$(echo "$targets" | awk '$2 != "unix" && $2 != "win"')
[ -z "$bad" ] || {
  echo "release/publish.sh: unknown archive in release-targets.json: $bad" >&2
  exit 1
}

# The release's signed files (release.yml's tag job makes them).
signed="nabi-$version.cdx.json SHA256SUMS SHA256SUMS.minisig"

# Every asset the release carries, one per line: each archive, its .sha256,
# nabi-version, the SBOM, SHA256SUMS and its signature.
plan() {
  echo "$targets" | while read -r t kind; do
    [ -n "$t" ] || continue
    case $kind in
      unix) set -- "nabi-$version-$t.tar.gz" "nabi-$version-$t.tar.xz" ;;
      *) set -- "nabi-$version-$t.zip" ;;
    esac
    for a in "$@"; do
      echo "$a"
      echo "$a.sha256"
    done
  done
  echo nabi-version
  for a in $signed; do echo "$a"; done
}
planned=$(plan)

want() {
  if [ ! -f "$dir/$1" ]; then
    echo "missing   $1"
    return
  fi
  if [ ! -f "$dir/$1.sha256" ]; then
    echo "no sha256 $1.sha256"
    return
  fi
  if ! (cd "$dir" && { shasum -a 256 -c "$1.sha256" || sha256sum -c "$1.sha256"; } > /dev/null 2>&1); then
    echo "bad sha   $1"
    return
  fi
  echo "ok        $1"
}

for a in $planned; do
  case " $signed " in *" $a "*) continue ;; esac
  case $a in *.sha256 | nabi-version) ;; *) want "$a" ;; esac
done > "$dir/.release-check"
cat "$dir/.release-check"
problems=$(grep -c '^\(missing\|no sha256\|bad sha\)' "$dir/.release-check" || true)
rm -f "$dir/.release-check"

# The toolchain archives keep the contract every released client relies on.
if [ "${problems:-0}" -eq 0 ]; then
  set --
  for a in "$dir/nabi-$version"-*.tar.gz "$dir/nabi-$version"-*.tar.xz "$dir/nabi-$version"-*.zip; do
    [ ! -f "$a" ] || set -- "$@" "$a"
  done
  [ $# -eq 0 ] || "$(dirname "$0")/../check/archive.sh" "$version" "$@" || problems=1
fi

# Signed: SHA256SUMS lists exactly the archives and the SBOM, and its
# signature verifies. A dry run before the tag job has none of them.
have=""
for a in $signed; do [ ! -f "$dir/$a" ] || have="$have $a"; done
if [ -n "$have" ] || [ -n "$publish" ]; then
  for a in $signed; do
    [ -f "$dir/$a" ] || {
      echo "missing   $a (release.yml's tag job signs the set: scripts/release/sign.sh)"
      problems=$((${problems:-0} + 1))
    }
  done
  if [ -f "$dir/SHA256SUMS" ]; then
    for a in $planned; do
      case $a in *.sha256 | nabi-version | SHA256SUMS | SHA256SUMS.minisig) ;; *) echo "$a" ;; esac
    done | LC_ALL=C sort > "$dir/.release-want"
    awk '{ print $2 }' "$dir/SHA256SUMS" | LC_ALL=C sort > "$dir/.release-got"
    if ! cmp -s "$dir/.release-want" "$dir/.release-got"; then
      echo "SHA256SUMS does not list exactly the archives and the SBOM (< planned, > listed):"
      diff "$dir/.release-want" "$dir/.release-got" || true
      problems=$((${problems:-0} + 1))
    fi
    rm -f "$dir/.release-want" "$dir/.release-got"
  fi
  if [ -f "$dir/SHA256SUMS" ] && [ -f "$dir/SHA256SUMS.minisig" ]; then
    "$(dirname "$0")/sign.sh" verify "$dir" "$pubkey" || problems=$((${problems:-0} + 1))
  fi
else
  echo "unsigned  $signed (release.yml's tag job makes them)"
fi

echo
if [ "${problems:-0}" -ne 0 ]; then
  echo "release/publish.sh: $problems problem(s); $tag is not ready" >&2
  exit 1
fi
echo "release/publish.sh: every archive for $tag is present and verified"

if [ -z "$publish" ]; then
  echo "release/publish.sh: dry run; re-run with --publish to create $tag on GitHub"
  exit 0
fi

command -v gh > /dev/null || {
  echo "release/publish.sh: --publish needs the gh CLI" >&2
  exit 1
}
[ -s "$dir/notes.md" ] || {
  echo "release/publish.sh: no $dir/notes.md; write it with scripts/release/notes.sh $version > $dir/notes.md" >&2
  exit 1
}
[ -n "$commit" ] || commit=$(git rev-parse HEAD)

# A moved or deleted tag would publish these archives under another
# commit. ^{} is the commit an annotated tag points at.
tag_on_commit() {
  refs=$(git ls-remote origin "refs/tags/$tag" "refs/tags/$tag^{}") || {
    echo "release/publish.sh: git ls-remote origin failed" >&2
    return 1
  }
  at=$(echo "$refs" | awk '$2 ~ /\^\{\}$/ { p = $1 } { s = $1 } END { print p ? p : s }')
  if [ -z "$at" ]; then
    echo "release/publish.sh: tag $tag is not on origin; push it before publishing" >&2
    return 1
  fi
  if [ "$at" != "$commit" ]; then
    echo "release/publish.sh: tag $tag is on $at, but the archives were built from $commit; a tag never moves, ship the fix as the next version" >&2
    return 1
  fi
}
# The release holds exactly the planned assets, each at its local size.
assets_match() {
  for a in $planned; do
    echo "$a $(wc -c < "$dir/$a" | tr -d ' ')"
  done | sort > "$dir/.release-want"
  gh release view "$1" --json assets --jq '.assets[] | "\(.name) \(.size)"' | sort > "$dir/.release-got"
  if cmp -s "$dir/.release-want" "$dir/.release-got"; then
    rm -f "$dir/.release-want" "$dir/.release-got"
    return 0
  fi
  echo "release/publish.sh: $1 does not hold the $(echo "$planned" | wc -l | tr -d ' ') planned assets (< planned, > uploaded):" >&2
  diff "$dir/.release-want" "$dir/.release-got" >&2 || true
  rm -f "$dir/.release-want" "$dir/.release-got"
  return 1
}

if [ -n "$rehearse" ]; then
  name=$rehearse
else
  name=$tag
  tag_on_commit
  # A rerun: done if already public, a leftover draft is made again.
  if draft=$(gh release view "$tag" --json isDraft --jq .isDraft 2> /dev/null); then
    if [ "$draft" = false ]; then
      assets_match "$tag" || {
        echo "release/publish.sh: $tag is public with other assets; a release never changes, ship the fix as the next version" >&2
        exit 1
      }
      echo "release/publish.sh: $tag is already public with every planned asset"
      exit 0
    fi
    echo "release/publish.sh: dropping the draft $tag a failed run left"
    gh release delete "$tag" --yes
  fi
fi

# A release candidate is a prerelease: GitHub's latest/ skips it, so the
# nabi-version there keeps naming the last final release.
pre=""
case $version in *-rc.*) pre="--prerelease --latest=false" ;; esac
set --
for a in $planned; do
  set -- "$@" "$dir/$a"
done
# A draft is not public and latest/ skips it, so a failed upload never
# leaves a partial release behind.
if [ -n "$rehearse" ]; then
  # shellcheck disable=SC2086
  gh release create "$name" "$@" --draft --target "$commit" \
    --title "nabi $version (rehearsal)" --notes-file "$dir/notes.md" $pre
else
  # shellcheck disable=SC2086
  gh release create "$tag" "$@" --draft --verify-tag \
    --title "nabi $version" --notes-file "$dir/notes.md" $pre
fi

n=$(echo "$planned" | wc -l | tr -d ' ')
assets_match "$name" || {
  echo "release/publish.sh: the draft $name stays unpublished; rerun to make it again" >&2
  exit 1
}
echo "release/publish.sh: draft $name holds all $n planned assets"

if [ -n "$rehearse" ]; then
  gh release delete "$name" --yes
  echo "release/publish.sh: rehearsal done, draft $name deleted"
  exit 0
fi

tag_on_commit || {
  echo "release/publish.sh: the draft stays unpublished" >&2
  exit 1
}
gh release edit "$tag" --draft=false
echo "release/publish.sh: published $tag${pre:+ (a prerelease)}"
