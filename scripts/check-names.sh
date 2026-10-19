#!/usr/bin/env bash
# Fails on a Docker container or volume name built outside the job namespace's helpers
# (benches/suite/suitekit/ns.py, scripts/lib/ns.sh): a hard-coded name is shared by every job on a
# host, so one job times another's build or removes its volumes (19 Oct 2026). Names come from
# ns.name / ns_name, labels with them, volume commands from ns.volume / ns_volume, the shared
# resources (the datasets' volume, the Cargo registry) from ns.shared / ns_shared.
#
#   scripts/check-names.sh [FILE...]    default: the benchmark scripts git tracks
set -uo pipefail
cd "$(dirname "$0")/.."

if [ $# -eq 0 ]; then
  set -- $(git ls-files 'benches/*.py' 'benches/*.sh' 'benches/*.toml' 'scripts/*.sh' 'scripts/*.py' \
           | grep -v -E '(^|/)tests/|^benches/suite/suitekit/ns\.py$|^scripts/lib/ns\.sh$|^scripts/check-names\.sh$|^benches/hosts\.toml$')
fi

# Per line, comments aside: a `--name` given to docker run, a `docker volume` command, a
# volume name of ours spelt out.
SH='(^|[^_a-z])docker +(run|create)\b.*--name|(^|[^_a-z])docker +volume +(create|rm|inspect)|-v +nrese-[a-z0-9.-]+:|NRESE_TARGET_VOLUME *= *"nrese-'
PY='[[,] *"--name" *,|"volume", *"(create|rm|inspect)"|"nrese-(target|cargo|bench-data|suite-)'
py=() other=()
for file in "$@"; do case $file in *.py) py+=("$file") ;; *) other+=("$file") ;; esac; done
# One grep a kind (-H: the file even when it is the only one), comment lines dropped.
hits=$( { [ ${#py[@]} -eq 0 ] || grep -HnE -- "$PY" "${py[@]}"
          [ ${#other[@]} -eq 0 ] || grep -HnE -- "$SH" "${other[@]}"; } | grep -vE '^.+:[0-9]+: *#')
if [ -n "$hits" ]; then
  printf '%s\n' "$hits"
  echo "check-names: a container or volume name outside the job namespace (suitekit/ns.py, scripts/lib/ns.sh)" >&2
  exit 1
fi
