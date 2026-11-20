#!/bin/sh
# The corpus as an XCTest bundle (Corpus.m) in a host app, on a Mac with
# Xcode:
#
#   targets/ios/xctest.sh stage <triple> <out>   build the cases as dylibs
#   targets/ios/xctest.sh sim <out> <device>     build, run on a simulator
#   targets/ios/xctest.sh device <out>           build, pack for Device Farm
#
# stage: <triple> is aarch64-apple-ios-sim (the simulator) or
#   aarch64-apple-ios (a device). The corpus is built through
#   targets/devfarm/stage.sh with cc.sh as NABI_CC; <out>/corpus then
#   holds dylibs/ (<case>.dylib, <case>~<i>.dylib per extra gc-stress
#   run), cases/*.json, skip.txt and Cases.inc (a test method per case).
# sim: xcodebuild test on <device> (a simulator name or UDID), a test
#   that hangs past Corpus.m's own limit stopped at 180 s; the log is
#   <out>/xcodebuild.log, summarized by targets/devfarm/report.py.
# device: xcodebuild build-for-testing for iphoneos, then <out>/app.ipa
#   and <out>/tests.xctest.zip for `targets/devfarm/run.sh ios`. With
#   NABI_DEVELOPMENT_TEAM (a Team ID) the build is signed, Xcode's
#   automatic signing making what the team lacks (a development
#   certificate, the App ID, a profile); NABI_ASC_KEY_PATH,
#   NABI_ASC_KEY_ID and NABI_ASC_ISSUER_ID name an App Store Connect API
#   key to do that with, else Xcode's signed-in account does. Without a
#   team it is unsigned (the dylibs signed ad hoc), which only proves it
#   builds. A development profile needs a device on the team:
#   NABI_IOS_REGISTER_DEVICE (the UDID of an iPhone paired with this Mac)
#   builds for that iPhone, registering it. NABI_IOS_BUNDLE_ID
#   (org.nabi-lang.devicefarm) is the host app's bundle ID; the tests'
#   is it with `.tests`.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
cmd=${1:?stage, sim or device}

case $cmd in
  stage)
    triple=${2:?triple}
    out=${3:?out dir}
    case $triple in
      aarch64-apple-ios-sim) sdk=iphonesimulator target=arm64-apple-ios15.0-simulator ;;
      aarch64-apple-ios) sdk=iphoneos target=arm64-apple-ios15.0 ;;
      *)
        echo "xctest.sh: not an iOS target: $triple" >&2
        exit 2
        ;;
    esac
    mkdir -p "$out/shim"
    out=$(cd "$out" && pwd)
    xcrun -sdk "$sdk" clang -target "$target" -O1 -c "$here/shim.c" -o "$out/shim/shim-$sdk.o"
    # The runtime's objects name the iOS that nabic's target does.
    # A case that failed to build is reported; the rest are packed.
    IPHONEOS_DEPLOYMENT_TARGET=15.0 NABI_CC=$here/cc.sh NABI_IOS_SHIM=$out/shim \
      "$root/targets/devfarm/stage.sh" "$triple" "$out/stage" || st=$?
    c=$out/corpus
    rm -rf "$c"
    mkdir -p "$c/dylibs" "$c/cases"
    cp "$out"/stage/cases/*.json "$c/cases/"
    cp "$here/skip.txt" "$c/"
    : > "$c/Cases.inc"
    for f in "$out"/stage/bin/*; do
      name=$(basename "$f")
      runs=$(python3 -I -c 'import json,sys; print(len(json.load(open(sys.argv[1]))["runs"]))' \
        "$c/cases/$name.json")
      i=0
      while [ "$i" -lt "$runs" ]; do
        if [ "$i" = 0 ]; then d=$name.dylib; else d=$name~$i.dylib; fi
        cp "$f" "$c/dylibs/$d"
        install_name_tool -id "@rpath/$d" "$c/dylibs/$d" 2> /dev/null
        i=$((i + 1))
      done
      echo "- (void)test_$name { [self runCase:@\"$name\"]; }" >> "$c/Cases.inc"
    done
    echo "$c: $(ls "$c/cases" | wc -l | tr -d ' ') cases"
    exit "${st:-0}"
    ;;
  sim)
    out=$(cd "${2:?out dir}" && pwd)
    dev=${3:?simulator}
    case $dev in
      *-*-*-*-*) dest="id=$dev" ;;
      *) dest="platform=iOS Simulator,name=$dev" ;;
    esac
    set +e
    xcodebuild test -project "$here/Corpus.xcodeproj" -scheme Corpus -destination "$dest" \
      -derivedDataPath "$out/dd" -resultBundlePath "$out/sim.xcresult" \
      -test-timeouts-enabled YES -default-test-execution-time-allowance 180 \
      NABI_CORPUS_DIR="$out/corpus" > "$out/xcodebuild.log" 2>&1
    st=$?
    set -e
    rm -rf "$out/sim.xcresult"
    python3 -I "$root/targets/devfarm/report.py" "$out/xcodebuild.log" || st=$?
    exit "$st"
    ;;
  device)
    out=$(cd "${2:?out dir}" && pwd)
    set -- NABI_BUNDLE_ID="${NABI_IOS_BUNDLE_ID:-org.nabi-lang.devicefarm}"
    dest=generic/platform=iOS
    if [ -n "${NABI_DEVELOPMENT_TEAM:-}" ]; then
      set -- "$@" -allowProvisioningUpdates DEVELOPMENT_TEAM="$NABI_DEVELOPMENT_TEAM"
      if [ -n "${NABI_IOS_REGISTER_DEVICE:-}" ]; then
        dest=id=$NABI_IOS_REGISTER_DEVICE
        set -- "$@" -allowProvisioningDeviceRegistration
      fi
      if [ -n "${NABI_ASC_KEY_PATH:-}" ]; then
        set -- "$@" -authenticationKeyPath "$NABI_ASC_KEY_PATH" \
          -authenticationKeyID "${NABI_ASC_KEY_ID:?set NABI_ASC_KEY_ID with NABI_ASC_KEY_PATH}" \
          -authenticationKeyIssuerID "${NABI_ASC_ISSUER_ID:?set NABI_ASC_ISSUER_ID with NABI_ASC_KEY_PATH}"
      fi
    else
      echo "xctest.sh: NABI_DEVELOPMENT_TEAM unset, building unsigned" >&2
      set -- "$@" CODE_SIGNING_ALLOWED=NO
    fi
    xcodebuild build-for-testing -project "$here/Corpus.xcodeproj" -scheme Corpus \
      -sdk iphoneos -destination "$dest" -derivedDataPath "$out/dd" \
      NABI_CORPUS_DIR="$out/corpus" "$@" \
      > "$out/xcodebuild.log" 2>&1 || {
      tail -n 30 "$out/xcodebuild.log" >&2
      exit 1
    }
    p=$out/dd/Build/Products/Debug-iphoneos
    rm -rf "$out/pack" "$out/app.ipa" "$out/tests.xctest.zip"
    mkdir -p "$out/pack/Payload"
    # The app keeps its PlugIns: they are under its signature's seal, and
    # Device Farm cannot re-sign an app whose seal is broken.
    cp -R "$p/Host.app" "$out/pack/Payload/"
    (cd "$out/pack" && zip -q -r -y "$out/app.ipa" Payload)
    (cd "$p/Host.app/PlugIns" && zip -q -r -y "$out/tests.xctest.zip" Corpus.xctest)
    rm -rf "$out/pack"
    echo "$out/app.ipa"
    echo "$out/tests.xctest.zip"
    ;;
  *)
    echo "xctest.sh: unknown command $cmd" >&2
    exit 2
    ;;
esac
