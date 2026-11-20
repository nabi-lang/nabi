#!/bin/sh
# Runs a packed corpus on an AWS Device Farm phone and reports it:
#
#   targets/devfarm/run.sh android <app.apk> <tests.apk> <out> [-e key value ..]
#   targets/devfarm/run.sh ios <app.ipa> <tests.xctest.zip> <out>
#   targets/devfarm/run.sh fetch <run arn> <out>
#
# Uploads the app and the tests, schedules a run on the platform's pool,
# waits for it, downloads its artifacts into <out> and prints "N of M
# cases ran, K failed" with the failing names (report.py). Android runs
# under a test spec (targets/android/spec.yml; its -e pairs go to the
# instrumentation, `-e filter 001,hello`) whose junit.xml comes back in
# the Customer Artifacts. XCTest has no test spec: Device Farm runs the
# bundle itself and reports each test, which become <out>/tests.tsv.
# `fetch` downloads and reports a finished run again.
#
# NABI_DEVFARM_PROFILE (default nabi-devicefarm-runner), NABI_DEVFARM_REGION
# (us-west-2), NABI_DEVFARM_PROJECT (nabi-devicefarm-corpus), NABI_DEVFARM_POOL
# (nabi-devicefarm-<triple>) and NABI_DEVFARM_TIMEOUT (job limit in
# minutes, default 30) choose where and how long. Needs the aws CLI with a
# live session for the profile, curl, unzip and python3.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
profile=${NABI_DEVFARM_PROFILE:-nabi-devicefarm-runner}
region=${NABI_DEVFARM_REGION:-us-west-2}
project=${NABI_DEVFARM_PROJECT:-nabi-devicefarm-corpus}
timeout=${NABI_DEVFARM_TIMEOUT:-30}

df() {
  aws devicefarm --profile "$profile" --region "$region" "$@"
}

project_arn() {
  arn=$(df list-projects --query "projects[?name=='$project'].arn | [0]" --output text)
  [ "$arn" != None ] || {
    echo "run.sh: no project $project" >&2
    exit 1
  }
  echo "$arn"
}

# <project arn> <file> <type>: the upload's arn once Device Farm took it.
upload() {
  set -- "$1" "$2" "$3" "$(basename "$2")"
  got=$(df create-upload --project-arn "$1" --name "$4" --type "$3" \
    --query 'upload.[arn,url]' --output text)
  arn=${got%%	*}
  url=${got#*	}
  curl -fsS --retry 3 -T "$2" "$url" > /dev/null
  while :; do
    st=$(df get-upload --arn "$arn" --query 'upload.[status,metadata]' --output text)
    case $st in
      SUCCEEDED*) break ;;
      FAILED*)
        echo "run.sh: $4 rejected: $(df get-upload --arn "$arn" --query upload.message --output text)" >&2
        exit 1
        ;;
    esac
    sleep 5
  done
  echo "$arn"
}

# <run arn> <out>: the Customer Artifacts unpacked into <out>, then the report.
fetch() {
  mkdir -p "$2"
  df list-artifacts --arn "$1" --type FILE --query 'artifacts[].[name,extension,url]' \
    --output text | while IFS='	' read -r name ext url; do
    case $name in
      "Customer Artifacts")
        curl -fsS --retry 3 -o "$2/customer.zip" "$url"
        unzip -q -o "$2/customer.zip" -d "$2/customer"
        ;;
      *) curl -fsS --retry 3 -o "$2/$(echo "$name" | tr ' /' '__').$ext" "$url" || true ;;
    esac
  done
  df get-run --arn "$1" --query 'run.[result,deviceMinutes.total]' --output text |
    {
      read -r result minutes
      echo "run $result, $minutes device-minutes"
    }
  junit=$(find "$2/customer" -name junit.xml 2> /dev/null | head -n 1)
  if [ -n "$junit" ]; then
    python3 -I "$here/report.py" "$junit"
    return
  fi
  # Device Farm's own results: name, result, message per test.
  : > "$2/tests.tsv"
  for job in $(df list-jobs --arn "$1" --query 'jobs[].arn' --output text); do
    for suite in $(df list-suites --arn "$job" --query 'suites[].arn' --output text); do
      df list-tests --arn "$suite" --query 'tests[].[name,result,message]' --output text \
        >> "$2/tests.tsv"
    done
  done
  python3 -I "$here/report.py" "$2/tests.tsv"
}

os=${1:?android, ios or fetch}
case $os in
  fetch)
    fetch "${2:?run arn}" "${3:?out dir}"
    exit
    ;;
  android) triple=aarch64-linux-android app_type=ANDROID_APP test_type=INSTRUMENTATION ;;
  ios) triple=aarch64-apple-ios app_type=IOS_APP test_type=XCTEST ;;
  *)
    echo "run.sh: unknown platform $os" >&2
    exit 2
    ;;
esac
app=${2:?app}
tests=${3:?tests}
out=${4:?out dir}
shift 4
args=
while [ $# -gt 0 ]; do
  [ "$1" = -e ] && [ $# -ge 3 ] || {
    echo "run.sh: expected -e <key> <value>" >&2
    exit 2
  }
  args="$args -e $2 $3"
  shift 3
done
mkdir -p "$out"
out=$(cd "$out" && pwd)

pool=${NABI_DEVFARM_POOL:-nabi-devicefarm-$triple}
parn=$(project_arn)
pool_arn=$(df list-device-pools --arn "$parn" --type PRIVATE \
  --query "devicePools[?name=='$pool'].arn | [0]" --output text)
[ "$pool_arn" != None ] || {
  echo "run.sh: no device pool $pool" >&2
  exit 1
}

echo "uploading $app"
app_arn=$(upload "$parn" "$app" "$app_type")
echo "uploading $tests"
case $os in
  android)
    tests_arn=$(upload "$parn" "$tests" INSTRUMENTATION_TEST_PACKAGE)
    sed "s|@ARGS@|$args|" "$here/../android/spec.yml" > "$out/spec.yml"
    spec_arn=$(upload "$parn" "$out/spec.yml" INSTRUMENTATION_TEST_SPEC)
    test="type=$test_type,testPackageArn=$tests_arn,testSpecArn=$spec_arn"
    ;;
  ios)
    tests_arn=$(upload "$parn" "$tests" XCTEST_TEST_PACKAGE)
    test="type=$test_type,testPackageArn=$tests_arn"
    ;;
esac
run=$(df schedule-run --project-arn "$parn" --app-arn "$app_arn" \
  --device-pool-arn "$pool_arn" --name "corpus $os $(date -u +%Y%m%dT%H%M%SZ)" \
  --test "$test" \
  --execution-configuration "jobTimeoutMinutes=$timeout,videoCapture=false" \
  --query run.arn --output text)
echo "$run" > "$out/run.arn"
echo "scheduled $run"
last=
while :; do
  st=$(df get-run --arn "$run" --query run.status --output text)
  [ "$st" = "$last" ] || echo "  $(date -u +%H:%M:%S) $st"
  last=$st
  [ "$st" = COMPLETED ] && break
  sleep 20
done
fetch "$run" "$out"
