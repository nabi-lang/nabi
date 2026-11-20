#!/bin/sh
# Packs a staged aarch64-linux-android corpus (targets/devfarm/stage.sh)
# into the two APKs a Device Farm INSTRUMENTATION run takes:
#
#   targets/android/apk.sh <stage> <out> [a,b]
#
# With a,b only the cases whose name contains one of them go in (a smoke
# set).
#
#   <out>/app.apk    org.nabi_lang.corpus: each program as
#                    lib/arm64-v8a/lib<case>.so (debug info stripped)
#   <out>/tests.apk  org.nabi_lang.corpus.tests: the Corpus
#                    instrumentation, assets/cases/*.json and skip.txt
#
# Both are signed with <out>/debug.keystore, made on first use. Needs
# ANDROID_HOME (build-tools: aapt2 d8 zipalign apksigner; a platform's
# android.jar), a JDK (JAVA_HOME or javac on PATH), zip and rustup's
# llvm-tools (llvm-strip).
set -eu

stage=${1:?stage dir}
out=${2:?out dir}
filter=${3:-}
here=$(cd "$(dirname "$0")" && pwd)
sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:?set ANDROID_HOME to the Android SDK}}
bt=$(ls -d "$sdk"/build-tools/* | sort -V | tail -n 1)
jar=$(ls -d "$sdk"/platforms/android-*/android.jar | sort -V | tail -n 1)
[ -x "$bt/aapt2" ] || {
  echo "apk.sh: no build-tools in $sdk" >&2
  exit 1
}
[ -f "$jar" ] || {
  echo "apk.sh: no platforms/android-*/android.jar in $sdk" >&2
  exit 1
}
javac=${JAVA_HOME:+$JAVA_HOME/bin/}javac
keytool=${JAVA_HOME:+$JAVA_HOME/bin/}keytool
host=$(rustc -vV | sed -n 's/^host: //p')
strip=$(rustc --print sysroot)/lib/rustlib/$host/bin/llvm-strip
[ -x "$strip" ] || {
  echo "apk.sh: no llvm-strip: rustup component add llvm-tools" >&2
  exit 1
}
[ -d "$stage/cases" ] || {
  echo "apk.sh: $stage has no cases/" >&2
  exit 1
}

mkdir -p "$out"
out=$(cd "$out" && pwd)
b=$out/build
rm -rf "$b"
mkdir -p "$b/classes" "$b/app/lib/arm64-v8a" "$b/tests/assets/cases" "$b/appdex" "$b/testsdex"

ks=$out/debug.keystore
if [ ! -f "$ks" ]; then
  "$keytool" -genkeypair -keystore "$ks" -storepass android -keypass android \
    -alias nabi -keyalg RSA -keysize 2048 -validity 10000 \
    -dname "CN=Nabi corpus debug" > /dev/null 2>&1
fi

"$javac" -nowarn --release 11 -classpath "$jar" -d "$b/classes" \
  "$here/src/org/nabi_lang/corpus/Main.java" \
  "$here/src/org/nabi_lang/corpus/tests/Corpus.java"
"$bt/d8" --release --min-api 29 --lib "$jar" --output "$b/appdex" \
  "$b"/classes/org/nabi_lang/corpus/*.class
"$bt/d8" --release --min-api 29 --lib "$jar" --output "$b/testsdex" \
  "$b"/classes/org/nabi_lang/corpus/tests/*.class

# <name>: in the filter.
picked() {
  [ -n "$filter" ] || return 0
  for p in $(echo "$filter" | tr ',' ' '); do
    case $1 in *"$p"*) return 0 ;; esac
  done
  return 1
}
n=0
for f in "$stage"/bin/*; do
  name=$(basename "$f")
  picked "$name" || continue
  "$strip" --strip-debug -o "$b/app/lib/arm64-v8a/lib$name.so" "$f"
  cp "$stage/cases/$name.json" "$b/tests/assets/cases/"
  n=$((n + 1))
done
cp "$here/skip.txt" "$b/tests/assets/"

# <name> <manifest> <dex dir> <extra dir> [aapt2 args]
pack() {
  name=$1 manifest=$2 dex=$3 extra=$4
  shift 4
  "$bt/aapt2" link -I "$jar" --manifest "$manifest" --min-sdk-version 29 \
    --target-sdk-version 34 --version-code 1 --version-name 0.1 "$@" -o "$b/$name.unsigned.apk"
  (cd "$dex" && zip -q -X "$b/$name.unsigned.apk" classes.dex)
  if [ -n "$extra" ]; then
    (cd "$extra" && zip -q -X -r "$b/$name.unsigned.apk" .)
  fi
  "$bt/zipalign" -f -p 4 "$b/$name.unsigned.apk" "$b/$name.aligned.apk"
  "$bt/apksigner" sign --ks "$ks" --ks-pass pass:android --key-pass pass:android \
    --v4-signing-enabled false --out "$out/$name.apk" "$b/$name.aligned.apk"
}
pack app "$here/app.xml" "$b/appdex" "$b/app"
pack tests "$here/tests.xml" "$b/testsdex" "" -A "$b/tests/assets"
rm -rf "$b"
echo "$out/app.apk ($n cases)"
echo "$out/tests.apk"
