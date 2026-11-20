#!/bin/sh
# The GitHub side of release-ready.yml: runs that must pass on the exact
# commit of a release before it is tagged.
#
#   scripts/release/ready.sh ci <sha> [<branch>]
#   scripts/release/ready.sh dry-run <sha> [<branch>]
#
# ci: a ci run with every host and job (jobs=all, hosts=all) passed on
# <sha>: every job its plan listed in the expected-jobs artifact.
# dry-run: a release-build.yml run (build and verify, no publish) passed
# on <sha>. Either reuses a passing run of <sha> if there is one,
# else dispatches one on <branch>, which must point at <sha>, and waits.
# PACKAGES (bundled, the default, or none) is the dry run's packages
# input; only a run made with it counts (its name says "(no packages)").
# Needs gh with GH_REPO and GH_TOKEN.
set -eu

usage() {
  echo "usage: scripts/release/ready.sh ci|dry-run <sha> [<branch>]" >&2
  exit 2
}
[ $# -ge 2 ] || usage
cmd=$1 sha=$2 branch=${3-}
packages=${PACKAGES:-bundled}
case $packages in bundled | none) ;; *) usage ;; esac
: "${GH_REPO:?release/ready.sh: set GH_REPO (owner/repo)}"

die() {
  echo "release/ready.sh: $*" >&2
  exit 1
}

# The job names run $1 must pass, from its expected-jobs artifact; fails,
# saying why in $tmp/lacks, when it has none or the plan was not full.
expected_jobs() {
  rm -rf "$tmp/exp"
  if ! gh run download "$1" -n expected-jobs -D "$tmp/exp" > /dev/null 2>&1; then
    echo "  run $1 has no expected-jobs artifact (its ci.yml plan did not write one)" > "$tmp/lacks"
    return 1
  fi
  f=$tmp/exp/expected-jobs.json
  c=$(jq -r '.contract // empty' "$f")
  if [ "$c" != expected-jobs/1 ]; then
    echo "  expected-jobs.json of run $1 is ${c:-unversioned}, not expected-jobs/1" > "$tmp/lacks"
    return 1
  fi
  if [ "$(jq -r .full "$f")" != true ]; then
    jq -r '"  run \($r) is not a full run: \(.tier), hosts \(.hosts | join(" ")), jobs \(.jobs | join(" ")), filter \"\(.filter)\""' \
      --arg r "$1" "$f" > "$tmp/lacks"
    return 1
  fi
  jq -r '.names[]' "$f"
}

# Run $1 passed every job a full ci run has; prints what it lacks.
full_ci() {
  expected_jobs "$1" > "$tmp/expected" || return 1
  grep -qx "CI gate" "$tmp/expected" || {
    echo "  expected-jobs.json of run $1 lists no CI gate" > "$tmp/lacks"
    return 1
  }
  gh api --paginate "repos/$GH_REPO/actions/runs/$1/jobs?per_page=100" \
    --jq '.jobs[] | select(.conclusion == "success") | .name' > "$tmp/passed"
  while IFS= read -r j; do
    grep -qxF "$j" "$tmp/passed" || echo "  $j"
  done < "$tmp/expected" > "$tmp/lacks"
  [ ! -s "$tmp/lacks" ]
}

# Passing runs of workflow $1 on $sha by a manual run or the schedule;
# with $2, only those whose name has "(no packages)" ($2 true) or not.
passed_runs() {
  f=""
  [ -z "${2-}" ] || f="| select((.display_title | contains(\"(no packages)\")) == $2)"
  gh api "repos/$GH_REPO/actions/workflows/$1/runs?head_sha=$sha&status=success&per_page=100" \
    --jq ".workflow_runs[] | select(.event == \"workflow_dispatch\" or .event == \"schedule\") $f | .id"
}

# Dispatch workflow $1 on $branch with the -f fields after it and wait for
# its run of $sha; prints the run id once it completed.
dispatch() {
  wf=$1
  shift
  [ -n "$branch" ] || die "no passing $wf run on $sha and no branch at $sha to dispatch one; run release-ready with ref set to that branch"
  at=$(git ls-remote origin "refs/heads/$branch" | cut -f1)
  [ "$at" = "$sha" ] || die "branch $branch is on ${at:-nothing}, not $sha; dispatch on a branch whose head is the release commit"
  since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  gh workflow run "$wf" --ref "$branch" "$@" >&2
  q="repos/$GH_REPO/actions/workflows/$wf/runs?head_sha=$sha&event=workflow_dispatch&created=%3E%3D$since"
  id=""
  i=0
  while [ -z "$id" ]; do
    i=$((i + 1))
    [ "$i" -le 30 ] || die "the dispatched $wf run did not show up"
    sleep 20
    id=$(gh api "$q" --jq '.workflow_runs[0].id // empty')
  done
  echo "release/ready.sh: waiting for $wf run $id" >&2
  while [ "$(gh api "repos/$GH_REPO/actions/runs/$id" --jq .status)" != completed ]; do
    sleep 60
  done
  echo "$id"
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/nabi-release-ready.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

case $cmd in
  ci)
    for id in $(passed_runs ci.yml); do
      if full_ci "$id"; then
        echo "release/ready.sh: full ci passed on $sha in run $id"
        exit 0
      fi
      echo "release/ready.sh: run $id does not count:" >&2
      cat "$tmp/lacks" >&2
    done
    id=$(dispatch ci.yml -f hosts=all -f jobs=all -f filter=)
    full_ci "$id" || {
      cat "$tmp/lacks" >&2
      die "ci run $id on $sha is not a passing full run"
    }
    echo "release/ready.sh: full ci passed on $sha in run $id"
    ;;
  dry-run)
    none=false
    [ "$packages" != none ] || none=true
    id=$(passed_runs release-build.yml "$none" | head -n 1)
    if [ -z "$id" ]; then
      id=$(dispatch release-build.yml -f packages="$packages")
      c=$(gh api "repos/$GH_REPO/actions/runs/$id" --jq .conclusion)
      [ "$c" = success ] || die "release-build.yml dry run $id on $sha: $c"
    fi
    echo "release/ready.sh: release-build.yml dry run passed on $sha in run $id"
    ;;
  *) usage ;;
esac
