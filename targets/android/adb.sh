#!/bin/sh
# Runs an Android executable on a device or emulator over adb, the
# runner of aarch64-linux-android and x86_64-linux-android
# (NABI_TEST_RUNNER for the corpus).
#
#   adb.sh <serial> <exe> [args...]
#
# <serial>: as `adb devices` lists it (emulator-5554), or `-` for the
# only one. The program goes to /data/local/tmp/nabi-<pid>/ with a tmp/
# beside it (its TMPDIR, TMP and TEMP), runs with stdin closed, and is
# removed after. It sees the variables NABI_*, NABIR_*, TZ, LANG and
# LC_*; its exit code, stdout and stderr come back as they are.
# ADB names the adb to use (default: adb on PATH).
set -eu
dev=${1:?serial}
exe=${2:?exe}
shift 2
adb=${ADB:-adb}
if ! command -v "$adb" > /dev/null 2>&1; then
  echo "adb.sh: no adb: install the SDK's platform-tools" \
    "(\`sdkmanager platform-tools\`) and put them on PATH, or set ADB" >&2
  exit 127
fi
if [ "$dev" != - ]; then
  adb="$adb -s $dev"
fi

# Single-quoted for the device's sh.
quote() {
  printf "'%s'" "$(printf %s "$1" | sed "s/'/'\\\\''/g")"
}

dir=/data/local/tmp/nabi-$$
name=$(basename "$exe")
$adb shell "mkdir -p $dir/tmp" < /dev/null
trap '$adb shell "rm -rf $dir" < /dev/null > /dev/null 2>&1 || :' EXIT
$adb push -q "$exe" "$dir/$name" < /dev/null > /dev/null
vars="TMPDIR=$dir/tmp TMP=$dir/tmp TEMP=$dir/tmp"
for v in $(env | sed -nE 's/^(NABIR?_[A-Za-z0-9_]*|TZ|LANG|LC_[A-Z]*)=.*/\1/p'); do
  vars="$vars $v=$(quote "$(printenv "$v")")"
done
args=
for a in "$@"; do
  args="$args $(quote "$a")"
done
set +e
# exec: the device's sh reports no "Aborted" of its own for a signal.
$adb shell "chmod 755 $dir/$name && cd $dir && $vars exec ./$name$args" < /dev/null
rc=$?
set -e
exit $rc
