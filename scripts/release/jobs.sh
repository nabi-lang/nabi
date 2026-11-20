#!/bin/sh
# Print expected-jobs.json: the name of every job a ci.yml run must pass,
# as ci.yml's plan picked them, and whether the run is a full one (every
# host and job, no filter). release/ready.sh reads it from the run.
#
# Reads the plan from the environment: TIER, EVENT, JOBS (words), FILTER,
# and the JSON lists ALL (every host), PICKED, GATE, CROSS, OPTS, BINDGEN,
# DIST. The names follow the jobs' `name:` in ci.yml.
set -eu

jq -n --arg tier "$TIER" --arg event "$EVENT" --arg jobs "$JOBS" --arg filter "$FILTER" \
  --argjson all "$ALL" --argjson picked "$PICKED" --argjson gate "$GATE" \
  --argjson cross "$CROSS" --argjson opts "$OPTS" \
  --argjson bindgen "$BINDGEN" --argjson dist "$DIST" '
  ([$jobs | splits("[\\s,]+") | select(. != "")]) as $j
  | {
      contract: "expected-jobs/1",
      tier: $tier,
      event: $event,
      hosts: [$picked[].name],
      jobs: $j,
      opts: $opts,
      filter: $filter,
      full: ($tier == "nightly" and $filter == ""
        and ([$picked[].name] | sort) == ([$all[].name] | sort)
        and ($j | sort) == (["gate", "cross", "bindgen", "dist"] | sort)),
      names: (
        ["Lint", "Plan jobs"]
        + [$gate[] as $h
            | "Check and build tests (\($h))",
              (range(1; 4) | "Test workspace (\($h), \(.)/3)"),
              ($opts[] as $o | range(1; 5) | "Test corpus (\($h), \($o), \(.)/4)")]
        + (if any($cross[]; .host == "linux-x64") then ["Link cross targets from the sysroot (linux-x64)"] else [] end)
        + [$cross[] as $c | $opts[] | "Test corpus (\($c.host), \($c.target), \(.))"]
        + [$bindgen[] | "Test bindgen (\(.))"]
        + [$dist[] | "Build and install archive (\(.))"]
        + (if $event == "merge_group" then ["Check queued PR heads"] else [] end)
        + ["CI gate"])
    }'
