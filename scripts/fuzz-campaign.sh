#!/usr/bin/env bash
# A bug hunt over many seeds: the random differential and property tests (SPARQL against
# the reference evaluator, reasoner closures, index models, the inferred stack, compact
# equality, incremental SHACL, the parsers of untrusted input, random HTTP requests) run
# again and again, each run with another NRESE_FUZZ_SEED, until the time budget is
# spent. A failing run's output is kept with its seed, which replays it:
#
#   NRESE_FUZZ_SEED=<seed> cargo test --release -p <crate> --test <target> -- <test>
#
#   scripts/fuzz-campaign.sh [HOURS] [OUT_DIR]     (default 2 hours, ./fuzz-out)
#
# Runs niced; the release test binaries are built once through cargo-guarded.sh.
set -u
cd "$(dirname "$0")/.."
hours="${1:-2}"
out="${2:-fuzz-out}"
mkdir -p "$out"
deadline=$(( $(date +%s) + ${hours%.*} * 3600 ))

# The test binaries (crate:target) and the tests in them that draw random cases.
mapfile -t targets < <(grep -v '^#' scripts/lib/fuzz-targets.txt | tr -d '\r' | grep .)

# A build's test harness: the one executable cargo built for the requested target (an
# integration test by its name, or the library) in the test profile. The build of an
# integration test also gives its crate's program, and a warm build listed it last: on
# 12 November every server run of a campaign was the program refusing the test filter.
# Dependencies are never built in the test profile; -p names the crate.
python=${NRESE_PYTHON:-$(command -v python3 || command -v python)}
harness() {  # CARGO_JSON TARGET: its path, or why there isn't exactly one
  "$python" - "$1" "$2" <<'EOF'
import json, sys
path, target = sys.argv[1:]
found = []
for line in open(path, encoding="utf-8"):
    try:
        message = json.loads(line)
    except ValueError:
        continue
    kinds = (message.get("target") or {}).get("kind") or []
    if target == "lib":
        wanted = any(k in ("lib", "rlib", "dylib", "proc-macro") for k in kinds)
    else:
        wanted = kinds == ["test"] and message["target"].get("name") == target
    if (message.get("reason") == "compiler-artifact" and message.get("executable") and wanted
            and (message.get("profile") or {}).get("test")):
        found.append(message["executable"])
if len(found) != 1:
    sys.exit(f"{len(found)} test harnesses for target {target} in {path}, not one: {found}")
print(found[0])
EOF
}

# Cargo's JSON stays beside each build log, and binaries.txt says which executable ran.
declare -A binary
: >"$out/binaries.txt"
for entry in "${targets[@]}"; do
  IFS=: read -r crate target filter <<<"$entry"
  if [ "$target" = lib ]; then kind=(--lib); else kind=(--test "$target"); fi
  build_log="$out/build-$crate-$target.log"
  build_json="$out/build-$crate-$target.json"
  # A failed build stops the campaign, even when the harness was built before what failed.
  if ! nice scripts/cargo-guarded.sh test --release --locked -p "$crate" "${kind[@]}" --no-run \
         --message-format=json >"$build_json" 2>"$build_log" \
     || ! path=$(harness "$build_json" "$target" 2>>"$build_log"); then
    cat "$build_log" >&2
    echo "no test binary for $crate $target" >&2
    exit 1
  fi
  binary["$crate:$target"]="$path"
  echo "$crate:$target $path" >>"$out/binaries.txt"
done

seed=${NRESE_FUZZ_FIRST:-1}
runs=0
failures=0
while [ "$(date +%s)" -lt "$deadline" ]; do
  for entry in "${targets[@]}"; do
    IFS=: read -r crate target filter <<<"$entry"
    log="$out/$crate-$target-$seed.log"
    if NRESE_FUZZ_SEED=$seed nice "${binary[$crate:$target]}" $filter -q >"$log" 2>&1; then
      rm -f "$log"
    else
      failures=$((failures + 1))
      echo "FAIL seed=$seed $crate $target ($log)"
    fi
    runs=$((runs + 1))
  done
  seed=$((seed + 1))
done
echo "DONE runs=$runs seeds=$((seed - ${NRESE_FUZZ_FIRST:-1})) failures=$failures"
