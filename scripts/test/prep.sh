#!/bin/sh
# release/prep.sh, changelog/lint.sh, changelog/pr.sh and the expected-jobs
# check of release/ready.sh against fixtures: a fake history in a local
# origin, PRs and milestones served by a stubbed gh, a stubbed cargo.
# Nothing leaves $TMPDIR.
#
#   scripts/test/prep.sh
set -eu

cd "$(dirname "$0")/../.."
root=$(pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/nabi-test-prep.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

export HOME="$tmp/home" GIT_CONFIG_NOSYSTEM=1 GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com \
  GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com GH_REPO=o/r FIX="$tmp/fix"
mkdir -p "$HOME" "$FIX/api/repos/o/r/pulls" "$FIX/api/repos/o/r/actions/workflows/ci.yml" \
  "$FIX/api/repos/o/r/actions/runs/7" "$FIX/runs" "$tmp/stub"
today=$(date -u +%Y-%m-%d)

fail() {
  echo "test/prep.sh: $*" >&2
  exit 1
}
ok() { echo "ok        $*"; }

# gh api [--paginate] <path> [--jq <expr>] serves $FIX/api/<path> without
# its query; gh run download <id> -D <dir> copies $FIX/runs/<id>/.
cat > "$tmp/stub/gh" << 'EOF'
#!/bin/sh
case $1 in
  api)
    shift
    path="" jq=""
    while [ $# -gt 0 ]; do
      case $1 in
        --paginate) shift ;;
        --jq)
          jq=$2
          shift 2
          ;;
        *)
          path=$1
          shift
          ;;
      esac
    done
    f=$FIX/api/${path%%\?*}
    [ -f "$f" ] || {
      echo "gh: HTTP 404: $path" >&2
      exit 1
    }
    if [ -n "$jq" ]; then jq -r "$jq" "$f"; else cat "$f"; fi
    ;;
  run)
    run=$3
    [ -d "$FIX/runs/$run" ] || {
      echo "no valid artifacts found to download" >&2
      exit 1
    }
    while [ $# -gt 0 ] && [ "$1" != -D ]; do shift; done
    mkdir -p "$2"
    cp "$FIX/runs/$run"/* "$2/"
    ;;
  *)
    echo "gh stub: $*" >&2
    exit 1
    ;;
esac
EOF
cat > "$tmp/stub/cargo" << 'EOF'
#!/bin/sh
echo "$*" >> "$CARGO_LOG"
[ "$*" = "update --workspace --offline" ] || exit 1
v=$(sed -n '/^\[workspace\.package\]/,/^\[/ s/^version = "\(.*\)"/\1/p' Cargo.toml)
awk -v v="$v" '/^name = "nabi"$/ { print; getline; $0 = "version = \"" v "\"" } { print }' Cargo.lock > Cargo.lock.new
mv Cargo.lock.new Cargo.lock
EOF
chmod +x "$tmp/stub/gh" "$tmp/stub/cargo"
PATH=$tmp/stub:$PATH
export PATH CARGO_LOG="$tmp/cargo.log"

pr() {
  jq -n --arg n "$1" --arg login "$2" --arg title "$3" --arg body "$4" \
    '{number: ($n | tonumber), user: {login: $login}, title: $title, body: $body}' \
    > "$FIX/api/repos/o/r/pulls/$1"
}
milestone() {
  echo '[{"title": "0.1.1", "number": 1, "state": "open"}]' > "$FIX/api/repos/o/r/milestones"
  printf '%s\n' "$1" > "$FIX/api/repos/o/r/issues"
}

# A repository at 0.1.0 with four PRs merged since its tag.
git init -q --bare "$tmp/origin.git"
git init -q "$tmp/work"
cd "$tmp/work"
git symbolic-ref HEAD refs/heads/main
mkdir -p scripts/changelog scripts/check scripts/release
cp "$root/scripts/changelog/lint.sh" "$root/scripts/changelog/lib.sh" scripts/changelog/
cp "$root/scripts/check/version.sh" scripts/check/
cp "$root/scripts/release/prep.sh" scripts/release/
printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.1.0"\nedition = "2024"\n' > Cargo.toml
printf '[[package]]\nname = "nabi"\nversion = "0.1.0"\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\n' > Cargo.lock
printf '#!/bin/sh\nversion="${NABI_VERSION:-0.1.0}"\n' > install.sh
printf "if (-not \$Version) { \$Version = if (\$env:NABI_VERSION) { \$env:NABI_VERSION } else { '0.1.0' } }\n" > install.ps1
printf '# Changelog\n\n## 0.1.0 (2026-11-04)\n\n- The old release.\n' > CHANGELOG.md
git add -A
git commit -q -m "release: 0.1.0"
git tag v0.1.0
merge() {
  echo "$1" >> src.txt
  git add src.txt
  git commit -q -m "$1 (#$2)"
}
merge "feat(lang): a" 10
merge "fix: b" 11
merge "ci: c" 12
merge "chore(deps): bump d" 13
git remote add origin "$tmp/origin.git"
git push -q origin main --tags

pr 10 miniex "feat(lang): a" "$(printf -- '- a\n\nchangelog:\n- added: `a` does a thing.\n- silent: tests\n')"
pr 11 miniex "fix: b" "- b, no block"
pr 12 miniex "ci: c" "$(printf 'changelog:\n- Same code, new result: c\n')"
pr 13 'dependabot[bot]' "chore(deps): bump d" "Bumps d."
milestone '[{"number": 5, "title": "open bug"}]'

prep() {
  "$tmp/work/scripts/release/prep.sh" "$@" > "$tmp/out" 2>&1
}
at() { git ls-remote "$tmp/origin.git" "refs/heads/$1" | cut -f1; }

# Open issues and bad blocks: a list, and nothing pushed.
if prep --push 0.1.1; then fail "release/prep.sh passed with an open issue and bad blocks"; fi
for want in "#5 open bug" "#11 fix: b" "no changelog: block" "#12 ci: c" "has no kind"; do
  grep -qF "$want" "$tmp/out" || fail "no '$want' in: $(cat "$tmp/out")"
done
grep -q "#13" "$tmp/out" && fail "the dependabot PR was reported: $(cat "$tmp/out")"
[ -z "$(at release/0.1.1)" ] || fail "a failed prep pushed a branch"
ok "open issues and PRs without a valid changelog: block are listed, nothing pushed"

milestone '[]'
pr 11 miniex "fix: b" "$(printf 'did b\r\n\r\nchangelog:\r\n- fixed: b no longer\r\n  breaks.\r\n')"
pr 12 miniex "ci: c" "$(printf '<!--\nchangelog:\n- added: not this\n-->\nchangelog:\n- silent: ci only\n\nfooter\n')"
prep --push 0.1.1 || fail "release/prep.sh 0.1.1 failed: $(cat "$tmp/out")"
main=$(git rev-parse main)
b1=$(at release/0.1.1)
[ -n "$b1" ] || fail "no release/0.1.1 on origin: $(cat "$tmp/out")"
git fetch -q origin release/0.1.1
[ "$(git rev-parse "$b1^")" = "$main" ] || fail "release/0.1.1 is not one commit on main"
[ "$(git log -1 --format=%s "$b1")" = "release: 0.1.1" ] || fail "subject: $(git log -1 --format=%s "$b1")"
[ "$(git diff --name-only "$main" "$b1" | tr '\n' ' ')" = "CHANGELOG.md Cargo.lock Cargo.toml install.ps1 install.sh " ] ||
  fail "files: $(git diff --name-only "$main" "$b1")"
git show "$b1:Cargo.toml" | grep -qx 'version = "0.1.1"' || fail "Cargo.toml not bumped"
git show "$b1:Cargo.lock" | grep -A1 -x 'name = "nabi"' | grep -qx 'version = "0.1.1"' || fail "Cargo.lock not bumped"
git show "$b1:Cargo.lock" | grep -qx 'version = "1.0.0"' || fail "Cargo.lock lost a dependency"
git show "$b1:install.sh" | grep -qxF 'version="${NABI_VERSION:-0.1.1}"' || fail "install.sh not bumped"
git show "$b1:install.ps1" | grep -qF "else { '0.1.1' }" || fail "install.ps1 not bumped"
grep -qx "update --workspace --offline" "$CARGO_LOG" || fail "cargo: $(cat "$CARGO_LOG")"
cat > "$tmp/want" << EOF
# Changelog

## 0.1.1 ($today)

<!-- begin summary -->
Summary: TODO one line on what 0.1.1 brings; the roadmap and nabi-lang.org show it
<!-- end summary -->

<!-- begin highlights -->
### Highlights

- TODO the changes to read first; delete this block if there are none
<!-- end highlights -->

### Added

- \`a\` does a thing (#10)

### Fixed

- B no longer breaks (#11)

<!-- silent: #12 #13 -->

## 0.1.0 (2026-11-04)

- The old release.
EOF
git show "$b1:CHANGELOG.md" > "$tmp/got"
diff -u "$tmp/want" "$tmp/got" || fail "CHANGELOG.md differs"
grep -q "changelog/lint.sh: the 0.1.1 section is well formed (draft" "$tmp/out" || fail "no draft lint: $(cat "$tmp/out")"
ok "release/0.1.1: one release commit, versions bumped, section grouped by kind, silent PRs listed"

prep --push 0.1.1 || fail "second release/prep.sh failed: $(cat "$tmp/out")"
[ "$(at release/0.1.1)" = "$b1" ] || fail "an unchanged second run moved the branch"
grep -q "up to date" "$tmp/out" || fail "second run: $(cat "$tmp/out")"
ok "a second run with nothing new writes nothing"

# The hand-written part, written on the branch in a second commit.
git clone -q -b release/0.1.1 "$tmp/origin.git" "$tmp/edit"
cd "$tmp/edit"
if scripts/changelog/lint.sh 0.1.1 > "$tmp/out" 2>&1; then fail "lint passed a TODO Summary"; fi
grep -q "the summary block still has a TODO" "$tmp/out" || fail "lint: $(cat "$tmp/out")"
awk '
  /^Summary: TODO/ { print "Summary: b stops breaking."; next }
  /^<!-- begin highlights -->$/ { skip = 1 }
  !skip { print }
  /^<!-- end highlights -->$/ { skip = 0; getline }
' CHANGELOG.md > "$tmp/c"
cat "$tmp/c" > CHANGELOG.md
git commit -q -am "docs(changelog): 0.1.1 summary"
git push -q origin release/0.1.1
scripts/changelog/lint.sh 0.1.1 > "$tmp/out" 2>&1 || fail "lint failed a filled section: $(cat "$tmp/out")"
ok "changelog/lint.sh: a TODO Summary fails, a filled one passes"

# main moves on: the refresh rebases, keeps the hand edits, adds the PR.
cd "$tmp/work"
merge "fix: e" 14
git push -q origin main
pr 14 miniex "fix: e" "$(printf 'changelog:\n- fixed: e\n')"
prep --push 0.1.1 || fail "refresh failed: $(cat "$tmp/out")"
b2=$(at release/0.1.1)
git fetch -q origin release/0.1.1
[ "$(git rev-parse "$b2^")" = "$(git rev-parse main)" ] || fail "refresh is not one commit on the new main"
git show "$b2:CHANGELOG.md" > "$tmp/got"
grep -qx "Summary: b stops breaking." "$tmp/got" || fail "refresh lost the Summary"
grep -q "Highlights" "$tmp/got" && fail "refresh brought back the deleted Highlights"
grep -qx -- "- E (#14)" "$tmp/got" || fail "refresh did not add #14"
scripts/changelog/lint.sh --rev "$b2" 0.1.1 > "$tmp/out" 2>&1 || fail "lint after refresh: $(cat "$tmp/out")"
ok "a refresh after main moved: one commit on main, hand edits kept, new PR added"

# A branch that changes more than the release files is refused.
git checkout -q --detach "$b2"
echo x > other.txt
git add other.txt
git commit -q -m "stray"
git push -q origin HEAD:refs/heads/release/0.1.1
git checkout -q main
if prep --push 0.1.1; then fail "prep took a branch with other files"; fi
grep -q "changes more than the release files (other.txt" "$tmp/out" || fail "stray: $(cat "$tmp/out")"
git push -q -f origin "$b2:refs/heads/release/0.1.1"
ok "a branch with other changes is refused"

# Lint failures on hand-made sections.
lint_case() {
  want=$1
  shift
  git checkout -q --detach "$b2"
  "$@"
  git commit -q -am "case"
  r=$(git rev-parse HEAD)
  git checkout -q main
  if scripts/changelog/lint.sh --rev "$r" 0.1.1 > "$tmp/out" 2>&1; then fail "lint passed: $want"; fi
  grep -qF "$want" "$tmp/out" || fail "no '$want' in: $(cat "$tmp/out")"
}
edit() {
  sed "$2" "$1" > "$tmp/c"
  cat "$tmp/c" > "$1"
}
lint_case "#11 (merged since v0.1.0) is not in the section" edit CHANGELOG.md 's/ (#11)$//'
lint_case '"### Fixed" is empty' edit CHANGELOG.md '/^- [BE].* (#1[14])$/d'
lint_case "the first section is '## 0.1.1 (2026-13-01)'" edit CHANGELOG.md "s/^## 0.1.1 .*/## 0.1.1 (2026-13-01)/"
lint_case "Cargo.toml says 0.1.0, not 0.1.1" edit Cargo.toml 's/^version = "0.1.1"/version = "0.1.0"/'
ok "changelog/lint.sh: uncited PR, empty heading, bad date, version fail"

# Other versions.
if prep 0.1.5; then fail "prep took 0.1.5 after 0.1.0"; fi
grep -q "0.1.5 does not follow 0.1.0; the next is 0.1.1 (patch) or 0.2.0 (minor)" "$tmp/out" || fail "0.1.5: $(cat "$tmp/out")"
prep 0.2.0 || fail "prep 0.2.0: $(cat "$tmp/out")"
git show release/0.2.0:CHANGELOG.md | grep -qx "### Migration" || fail "0.2.0 has no Migration block"
[ -z "$(at release/0.2.0)" ] || fail "prep without --push pushed"
git tag v0.1.1 "$b2"
git push -q origin v0.1.1
if prep --push 0.1.1; then fail "prep took a tagged version"; fi
grep -q "v0.1.1 is already tagged" "$tmp/out" || fail "tagged: $(cat "$tmp/out")"
ok "a version that skips one, a series with Migration, a tagged version"

# A release candidate: the version files say X.Y.Z-rc.N, the section is
# X.Y.Z's, and changelog/lint.sh checks it as X.Y.Z's.
prep 0.2.0-rc.1 || fail "prep 0.2.0-rc.1: $(cat "$tmp/out")"
git show release/0.2.0-rc.1:Cargo.toml | grep -qx 'version = "0.2.0-rc.1"' || fail "rc: Cargo.toml not bumped"
git show release/0.2.0-rc.1:install.sh | grep -qxF 'version="${NABI_VERSION:-0.2.0-rc.1}"' || fail "rc: install.sh not bumped"
git show release/0.2.0-rc.1:install.ps1 | grep -qF "else { '0.2.0-rc.1' }" || fail "rc: install.ps1 not bumped"
[ "$(git log -1 --format=%s release/0.2.0-rc.1)" = "release: 0.2.0-rc.1" ] || fail "rc subject"
[ "$(git show release/0.2.0-rc.1:CHANGELOG.md | grep -m 1 '^## ')" = "## 0.2.0 ($today)" ] ||
  fail "rc section: $(git show release/0.2.0-rc.1:CHANGELOG.md | grep -m 1 '^## ')"
git show release/0.2.0-rc.1:CHANGELOG.md | grep -q -- '-rc' && fail "rc: the CHANGELOG names the candidate"
git show release/0.2.0-rc.1:CHANGELOG.md | grep -qx "### Migration" || fail "rc: 0.2.0 has no Migration block"
grep -q "changelog/lint.sh: the 0.2.0-rc.1 section is well formed (draft" "$tmp/out" || fail "rc: no draft lint: $(cat "$tmp/out")"
if scripts/changelog/lint.sh --draft --rev release/0.2.0-rc.1 0.2.0 > "$tmp/out" 2>&1; then
  fail "lint took 0.2.0 on the rc commit"
fi
grep -q "Cargo.toml says 0.2.0-rc.1, not 0.2.0" "$tmp/out" || fail "rc lint: $(cat "$tmp/out")"
if prep 0.1.1-rc.1; then fail "prep took a candidate of a released version"; fi
grep -q "0.1.1 is already released" "$tmp/out" || fail "released rc: $(cat "$tmp/out")"
for bad in 0.2.0-rc 0.2.0-rc.0 0.2.0-beta.1; do
  if prep "$bad"; then fail "prep took $bad"; fi
  grep -q "is not a release version" "$tmp/out" || fail "$bad: $(cat "$tmp/out")"
done
if prep 0.3.0-rc.1; then fail "prep took 0.3.0-rc.1 after 0.1.0"; fi
grep -q "0.3.0 does not follow 0.1.0" "$tmp/out" || fail "0.3.0-rc.1: $(cat "$tmp/out")"
ok "a candidate: rc versions, the final's section, a released or malformed candidate refused"

# changelog/pr.sh on descriptions.
cd "$root"
pr_lint() {
  printf '%b' "$2" > "$tmp/body"
  if scripts/changelog/pr.sh "$tmp/body" > "$tmp/out" 2>&1; then r=pass; else r=fail; fi
  [ "$r" = "$1" ] || fail "changelog/pr.sh should $1 on: $2; it said: $(cat "$tmp/out")"
}
pr_lint pass 'x\r\n\r\nchangelog:\r\n- fixed: a\r\n  b\r\n- Silent: c\r\n\r\nfooter\r\n'
grep -q "fixed	a b" "$tmp/out" || fail "changelog/pr.sh did not join the continuation: $(cat "$tmp/out")"
pr_lint fail 'no block\n'
pr_lint fail 'changelog:\n- fix: a\n'
pr_lint fail 'changelog:\n\n- added: a\n'
pr_lint fail '<!--\nchangelog:\n- added: a\n-->\n'
pr_lint fail 'changelog:\n- added:\n'
scripts/changelog/pr.sh .github/PULL_REQUEST_TEMPLATE.md > "$tmp/out" 2>&1 && fail "changelog/pr.sh passed the unfilled template"
ok "changelog/pr.sh: kinds, continuations, CRLF, comments, the unfilled template"

# release/ready.sh ci reads the run's expected-jobs.json.
all=$(sed -n "/^ *all='\[/,/\]'/p" .github/workflows/ci.yml | sed "s/^ *all='//; s/'\$//")
cross=$(sed -n "/^ *cross='\[/,/\]'/p" .github/workflows/ci.yml | sed "s/^ *cross='//; s/'\$//")
full() {
  TIER=nightly EVENT=workflow_dispatch JOBS="gate cross bindgen dist" FILTER=${1-} ALL=$all PICKED=$all \
    GATE='["linux-x64", "linux-arm64", "macos-arm64", "windows-x64"]' CROSS=$cross OPTS='["-O0", "-O2"]' \
    BINDGEN='["linux-x64", "windows-x64"]' DIST='["linux-x64", "windows-x64"]' scripts/release/jobs.sh
}
full > "$tmp/exp.json"
[ "$(jq -r .full "$tmp/exp.json")" = true ] || fail "a full plan is not full"
[ "$(jq '.names | length' "$tmp/exp.json")" = 62 ] || fail "expected $(jq '.names | length' "$tmp/exp.json") jobs, not 62"
[ "$(full x | jq -r .full)" = false ] || fail "a filtered plan is full"
# Each name is a job `name:` of ci.yml, its matrix values in parentheses.
jq -r '.names[] | sub(" \\(.*"; "")' "$tmp/exp.json" | sort -u | while IFS= read -r n; do
  grep -Eq "^    name: $n( \\(|\$)" .github/workflows/ci.yml || fail "jobs.sh names \"$n\", which is no job name in ci.yml"
done

echo '{"workflow_runs": [{"id": 7, "event": "workflow_dispatch"}]}' > "$FIX/api/repos/o/r/actions/workflows/ci.yml/runs"
passed() {
  jq '{jobs: [.[] | {name: ., conclusion: "success"}]}' > "$FIX/api/repos/o/r/actions/runs/7/jobs"
}
ready() {
  scripts/release/ready.sh ci 0123456789abcdef0123456789abcdef01234567 > "$tmp/out" 2>&1
}
mkdir -p "$FIX/runs/7"
cp "$tmp/exp.json" "$FIX/runs/7/expected-jobs.json"
jq '.names' "$tmp/exp.json" | passed
ready || fail "release-ready ci failed a full passing run: $(cat "$tmp/out")"
jq '.names - ["Test corpus (windows-arm64, aarch64-pc-windows-msvc, -O2)"]' "$tmp/exp.json" | passed
if ready; then fail "release-ready ci passed a run without Test corpus (windows-arm64, aarch64-pc-windows-msvc, -O2)"; fi
jq '.full = false | .hosts = ["linux-x64"]' "$tmp/exp.json" > "$FIX/runs/7/expected-jobs.json"
jq '.names' "$tmp/exp.json" | passed
if ready; then fail "release-ready ci passed a run that is not full"; fi
rm -rf "$FIX/runs/7"
jq '.names' "$tmp/exp.json" | passed
if ready; then fail "release-ready ci passed a run without the expected-jobs artifact"; fi
grep -q "no expected-jobs artifact" "$tmp/out" || fail "no missing-artifact reason: $(cat "$tmp/out")"
ok "release-ready: expected-jobs.json of the run, a missing job, a partial run, no artifact"

# The first release: no tag yet. Only 0.1.0, with every PR so far.
git init -q --bare "$tmp/first.git"
git init -q "$tmp/first"
cd "$tmp/first"
git symbolic-ref HEAD refs/heads/main
mkdir -p scripts/changelog scripts/check scripts/release
cp "$root/scripts/changelog/lint.sh" "$root/scripts/changelog/lib.sh" scripts/changelog/
cp "$root/scripts/check/version.sh" scripts/check/
cp "$root/scripts/release/prep.sh" scripts/release/
cp "$tmp/work/Cargo.toml" "$tmp/work/Cargo.lock" "$tmp/work/install.sh" "$tmp/work/install.ps1" .
printf '# Changelog\n\n## 0.1.0 (unreleased)\n' > CHANGELOG.md
git add -A
git commit -q -m "initial"
merge "feat(lang): first" 20
git remote add origin "$tmp/first.git"
git push -q origin main
pr 20 miniex "feat(lang): first" "$(printf 'changelog:\n- added: the first thing.\n')"
milestone '[]'
first() { "$tmp/first/scripts/release/prep.sh" "$@" > "$tmp/out" 2>&1; }
if first --push 0.1.1; then fail "release/prep.sh made 0.1.1 the first release"; fi
grep -q "the first release is 0.1.0" "$tmp/out" || fail "first 0.1.1: $(cat "$tmp/out")"
# 0.1.0-rc.1 first, as the launch goes: the 0.1.0 section, every PR so far.
first --push 0.1.0-rc.1 || fail "release/prep.sh 0.1.0-rc.1 as the first release failed: $(cat "$tmp/out")"
git -C "$tmp/first" fetch -q origin release/0.1.0-rc.1
git -C "$tmp/first" show FETCH_HEAD:CHANGELOG.md | grep -qx -- "- The first thing (#20)" ||
  fail "the rc's section lacks #20: $(git -C "$tmp/first" show FETCH_HEAD:CHANGELOG.md)"
[ "$(git -C "$tmp/first" show FETCH_HEAD:CHANGELOG.md | grep -c '^## 0.1.0 (')" = 1 ] ||
  fail "the rc's CHANGELOG: $(git -C "$tmp/first" show FETCH_HEAD:CHANGELOG.md)"
first --push 0.1.0 || fail "release/prep.sh 0.1.0 as the first release failed: $(cat "$tmp/out")"
git -C "$tmp/first" fetch -q origin release/0.1.0
git -C "$tmp/first" show FETCH_HEAD:CHANGELOG.md | grep -qx -- "- The first thing (#20)" ||
  fail "the first section lacks #20: $(git -C "$tmp/first" show FETCH_HEAD:CHANGELOG.md)"
ok "the first release: 0.1.0 (or 0.1.0-rc.1) only, with every PR merged so far"
