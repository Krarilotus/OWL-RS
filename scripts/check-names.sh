#!/usr/bin/env bash
# Fails on a Docker container, volume or network name built outside the job namespace's helpers
# (benches/suite/suitekit/ns.py, scripts/lib/ns.sh): a hard-coded name is shared by every job on a
# host, so one job times another's build or removes its volumes (19 Oct 2026). Names come from
# ns.name / ns_name, labels with them, volume and network commands from ns.volume / ns_volume /
# ns_network, the shared resources (the datasets' volume, the Cargo registry) from ns.shared /
# ns_shared.
# Fleet's immutable bound-create prefix is accepted only in DockerRuntime.args's
# bound branch at its owning path; Fleet resolves its name and owner together.
#
#   scripts/check-names.sh [FILE...]    default: the benchmark scripts and Compose files git tracks
#
# What it finds, comments aside:
#   shell   `--name` given to docker run or create, a `docker volume` or `docker network` command,
#           a mount of one of our volumes spelt out, a variable set to one of our names, or
#           defaulting to one (${VAR:-nrese-target}), NRESE_TARGET_VOLUME naming a volume
#   Python  `--name` as a list element, a `volume` command, a string (or f-string) spelling one of
#           our names
#   Compose a container_name, or a volume's or network's `name`
# What it can't tell apart, documented by its tests (benches/suite/tests/test_job_namespace.py):
# a mount from a variable set only in the environment (-v "$X:/target": a host directory or a
# volume?), and a flag built by concatenation ('--' + 'name').
set -uo pipefail
cd "$(dirname "$0")/.."

if [ $# -eq 0 ]; then
  set -- $(git ls-files 'benches/*.py' 'benches/*.sh' 'benches/*.toml' 'scripts/*.sh' 'scripts/*.py' \
             '*compose*.yml' '*compose*.yaml' \
           | grep -v -E '(^|/)tests/|^benches/suite/suitekit/ns\.py$|^scripts/lib/ns\.sh$|^scripts/check-names\.sh$|^benches/hosts\.toml$')
fi

OURS='(nrese-(target|cargo|bench-data|suite|oxigraph-target|prof|cert|dl-reference|sc)|sc-|rsc-|qlever-index-|sparql-prep-|npd-prepare-)'
SH="(^|[^_a-z])docker +(run|create)\\b.*--name|(^|[^_a-z])docker +(volume|network) +(create|rm|inspect)|-v +\"?$OURS[a-z0-9.-]*:|(^|[ ;(])[A-Za-z_][A-Za-z0-9_]*=[\"']?$OURS|:-$OURS|NRESE_TARGET_VOLUME *= *\"nrese-"
PY="[[,] *\"--name\" *,|\"(volume|network)\", *\"(create|rm|inspect)\"|[\"']$OURS"
COMPOSE='^ *(container_name|name): *[^ ]'
py=() compose=() other=()
for file in "$@"; do
  case $file in
    *.py) py+=("$file") ;;
    *compose*.yml|*compose*.yaml) compose+=("$file") ;;
    *) other+=("$file") ;;
  esac
done
# One grep a kind (-H: the file even when it is the only one), comment lines dropped.
hits=$( { [ ${#py[@]} -eq 0 ] || grep -HnE -- "$PY" "${py[@]}"
          [ ${#compose[@]} -eq 0 ] || grep -HnE -- "$COMPOSE" "${compose[@]}"
          [ ${#other[@]} -eq 0 ] || grep -HnE -- "$SH" "${other[@]}"; } | grep -vE '^.+:[0-9]+: *#')
if [ -n "$hits" ] && [ ${#py[@]} -gt 0 ]; then
  interpreter=$(command -v python || command -v python3) || exit 1
  allowed=$("$interpreter" - "${py[@]}" <<'PY'
import ast
from pathlib import Path
import sys

owner = Path('benches/suite/suitekit/runtime.py').resolve()
prefix = ['docker', 'create', '--name', '@FLEET_CONTAINER_NAME@', '--label',
          'fleet.owner=@FLEET_CONTAINER_OWNER@', '--init', '--network', 'none', '--pull=never']
for filename in sys.argv[1:]:
    path = Path(filename)
    if path.resolve() != owner:
        continue
    source = path.read_text()
    for cls in ast.parse(source).body:
        if not isinstance(cls, ast.ClassDef) or cls.name != 'DockerRuntime':
            continue
        for method in cls.body:
            if not isinstance(method, ast.FunctionDef) or method.name != 'args' or not method.body:
                continue
            branch = method.body[0]
            if not (isinstance(branch, ast.If) and isinstance(branch.test, ast.Name)
                    and branch.test.id == 'bound'):
                continue
            for assignment in branch.body:
                if not (isinstance(assignment, ast.Assign) and len(assignment.targets) == 1
                        and isinstance(assignment.targets[0], ast.Name)
                        and assignment.targets[0].id == 'args'):
                    continue
                try:
                    value = ast.literal_eval(assignment.value)
                except (ValueError, TypeError):
                    continue
                lines = source.splitlines()
                if (value == prefix and not lines[assignment.lineno - 1][:assignment.col_offset].strip()
                        and not lines[assignment.end_lineno - 1][assignment.end_col_offset:].strip()):
                    print(f'{filename}:{assignment.lineno}:{lines[assignment.lineno - 1]}')
PY
  ) || exit 1
  [ -z "$allowed" ] || hits=$(printf '%s\n' "$hits" | grep -vxF -- "$allowed")
fi
if [ -n "$hits" ]; then
  printf '%s\n' "$hits"
  echo "check-names: a container, volume or network name outside the job namespace (suitekit/ns.py, scripts/lib/ns.sh)" >&2
  exit 1
fi
