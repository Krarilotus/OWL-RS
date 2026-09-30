#!/usr/bin/env python3
"""The benchmark suite's run matrix: which workload runs on which system.

    benches/suite/suite.py             the matrix, and what NRESE can't run yet
    benches/suite/suite.py --check     validate the two files (exit 1 on an error)
    benches/suite/suite.py nrese       one system's column, with the reasons

A workload runs on a system if the system has every capability in the workload's `needs`
and, if the workload lists alternatives (`any`), at least one of them. Everything else is
skipped, and the matrix says which capability is missing. Needs Python 3.11.
"""
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
CAPABILITIES = {
    "load", "query", "update",
    "reason-rdfs", "reason-owl-horst", "reason-owl2-rl", "reason-owl2-ql",
    "reason-custom-rules", "reason-incremental", "consistency", "explain", "classify",
    "shacl-core", "shacl-sparql", "fulltext", "geosparql", "federation",
}
STATES = {"ready", "partial", "planned", "blocked"}


def load():
    with open(HERE / "systems.toml", "rb") as f:
        systems = tomllib.load(f)
    with open(HERE / "workloads.toml", "rb") as f:
        workloads = tomllib.load(f)
    return systems, workloads


def check(systems, workloads):
    errors = []
    for name, system in systems.items():
        for capability in system.get("capabilities", []) + list(system.get("missing", {})):
            if capability not in CAPABILITIES:
                errors.append(f"systems.toml [{name}]: unknown capability {capability!r}")
        overlap = set(system.get("capabilities", [])) & set(system.get("missing", {}))
        if overlap:
            errors.append(f"systems.toml [{name}]: both present and missing: {sorted(overlap)}")
        if system.get("publish") not in ("free", "permission"):
            errors.append(f"systems.toml [{name}]: publish must be free or permission")
    for name, workload in workloads.items():
        for capability in workload.get("needs", []) + workload.get("any", []):
            if capability not in CAPABILITIES:
                errors.append(f"workloads.toml [{name}]: unknown capability {capability!r}")
        if workload.get("state") not in STATES:
            errors.append(f"workloads.toml [{name}]: state must be one of {sorted(STATES)}")
        if workload.get("state") in ("ready", "partial") and not (HERE / "../.." / workload.get("kit", "?")).exists():
            errors.append(f"workloads.toml [{name}]: kit {workload.get('kit')!r} doesn't exist")
        if workload.get("state") == "blocked" and workload.get("waits-for") not in CAPABILITIES:
            errors.append(f"workloads.toml [{name}]: blocked needs waits-for = a capability")
        for key in ("title", "kind", "tasks", "source", "licence", "check"):
            if key not in workload:
                errors.append(f"workloads.toml [{name}]: {key} is missing")
    return errors


def verdict(system, workload):
    """None if the workload runs on the system, else what is missing."""
    have = set(system.get("capabilities", []))
    missing = [c for c in workload.get("needs", []) if c not in have]
    alternatives = workload.get("any", [])
    if alternatives and not have & set(alternatives):
        missing.append(" or ".join(alternatives))
    return ", ".join(missing) or None


def main(argv):
    systems, workloads = load()
    errors = check(systems, workloads)
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    if argv[1:] == ["--check"]:
        print(f"{len(systems)} systems, {len(workloads)} workloads: ok")
        return 0
    if len(argv) == 2:
        system = systems.get(argv[1])
        if system is None:
            print(f"unknown system {argv[1]!r}; known: {', '.join(systems)}", file=sys.stderr)
            return 1
        for name, workload in workloads.items():
            missing = verdict(system, workload)
            print(f"{name:24} {workload['state']:8} {'runs' if missing is None else 'skip: ' + missing}")
        return 0
    width = max(len(name) for name in workloads)
    names = list(systems)
    print(f"{'workload':{width}}  state     " + " ".join(f"{n[:9]:9}" for n in names))
    for name, workload in workloads.items():
        cells = ["run" if verdict(systems[n], workload) is None else "-" for n in names]
        print(f"{name:{width}}  {workload['state']:8}  " + " ".join(f"{c:9}" for c in cells))
    print("\nNRESE first: the workloads it can't run yet")
    nrese = systems["nrese"]
    for name, workload in workloads.items():
        missing = verdict(nrese, workload)
        if missing is not None:
            steps = [nrese.get("missing", {}).get(c.strip(), "?") for part in missing.split(", ") for c in part.split(" or ")]
            print(f"  {name}: needs {missing}  ->  {'; '.join(dict.fromkeys(steps))}")
    lacking = [c for c in sorted(CAPABILITIES) if c not in nrese["capabilities"]]
    print(f"  capabilities NRESE lacks: {', '.join(lacking)}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
