#!/bin/sh
# Runs an iOS simulator executable on a simulator device, the runner of
# aarch64-apple-ios-sim (nabic's runner.rs; NABI_TEST_RUNNER for the
# corpus).
#
#   simrun.sh <device> <exe> [args...]
#
# <device>: a device name or UDID, or `booted`. `xcrun simctl spawn`
# hands the program only the variables named SIMCTL_CHILD_<name>, as
# <name>: every variable goes over so.
set -eu
dev=${1:?device}
shift
if ! xcrun --find simctl > /dev/null 2>&1; then
  echo "simrun.sh: no simctl: install Xcode from the App Store and its iOS simulator" \
    "runtime (\`xcodebuild -downloadPlatform iOS\`), then run" \
    "\`sudo xcode-select -s /Applications/Xcode.app\`" >&2
  exit 127
fi
eval "$(export -p | sed 's/^export \([A-Za-z_][A-Za-z0-9_]*\)=/export SIMCTL_CHILD_\1=/')"
exec xcrun simctl spawn "$dev" "$@"
