"""The case registry (benches/cases/README.md, the schema's one owner): every case of every
group, read from benches/cases/*.toml, and its validation.

    load()                       every case, each with `group` (its file's stem), names unique; with
                                 `family` or NRESE_FAMILY (an A-B-B-A family, benches/suite/abba.py)
                                 its own cases too, group "family"
    problems(cases, ...)         what is wrong with them, one line each (empty: valid)

`suite.py --check` and the gate's suite contracts run the validation over the repository.
"""
from __future__ import annotations

import os
import re
import tomllib
from pathlib import Path

CASES = Path(__file__).resolve().parents[2] / "cases"
ROOT = Path(__file__).resolve().parents[3]


def family_cases(path: str | Path) -> list[dict]:
    """A family file's own cases (its [[case]] entries that aren't `catalog = NAME`), in the
    catalogue's schema; a relative path is the repository's."""
    path = Path(path)
    path = path if path.is_absolute() else ROOT / path
    entries = tomllib.loads(path.read_text(encoding="utf-8")).get("case", [])
    return [{k: v for k, v in e.items() if k not in ("dimensions", "min_samples")}
            for e in entries if "catalog" not in e]

# The vocabulary (benches/cases/README.md explains each).
AREAS = {"access", "cache", "concurrency", "dl", "kernel", "layout", "load", "maintenance", "memory", "operator",
         "parse", "plan", "query", "reasoning", "rules", "service", "shacl", "geo", "text", "vector", "federation",
         "rdf12"}
SEMANTICS = {"none", "rdfs", "rdfs-full", "rdfs-plus", "owl-horst", "owl2-ql", "owl2-rl", "owl2-el", "owl2-dl",
             "custom", "el", "dl", "canonical"}
# What a comparator computes: the same data without reasoning (plain), NRESE's closure, NRESE's
# rewritten queries, or its own reasoner or service under that name.
COMPARATOR_SEMANTICS = {"plain", "closure", "rewritten", "rdfs", "rdfs-plus", "owl-horst", "owl2-rl", "owl2-ql",
                        "custom", "el", "dl", "shacl", "text", "geosparql", "federation", "vector"}
TOOLS = {"perf_lab", "tableau", "classify-el", "classify-horn"}
OUTCOMES = {"ok", "memory-limit", "gave-up", "timeout", "unsupported", "wrong"}
LEVELS = {"focused", "full"}
NEEDS = {"docker", "java", "apptainer"}
SCENARIOS = {"native", "matched", "frontier"}
REWRITES = {"expressible", "per-query", "not-expressible"}
PAGE_CACHE = {"warm", "cold"}
DATA = re.compile(r"^(none|volume:\S+|ore:\S+|concat:\S+|[a-z][a-z0-9-]*( \S+=\S+)*)$")
KEYS = {
    # identity and what it measures
    "name", "area", "what", "group", "workload", "tier", "levels", "est_min", "needs", "scenario",
    # what NRESE computes and on what
    "semantics", "data", "data_alt", "extra", "setup", "queries", "tool", "args", "env", "repeat", "sweep",
    "tool_loads", "page_cache", "timeout_s", "cap_gb", "metric", "counters",
    # what a right answer and the right route are
    "check", "pinned", "route", "expect", "reference", "outcome", "known", "frontier",
    # the comparators
    "systems", "home_turf", "rewrite", "hangs", "tracks", "guards",
}
REQUIRED = ("name", "area", "what", "semantics", "data", "metric", "cap_gb", "systems")


def load(directory: Path = CASES, family: str | Path | None = None) -> list[dict]:
    """Every case of every group file, in file and then entry order; each gets its `group`."""
    out = []
    for path in sorted(directory.glob("*.toml")):
        for case in tomllib.loads(path.read_text(encoding="utf-8")).get("case", []):
            out.append({**case, "group": path.stem})
    family = family or os.environ.get("NRESE_FAMILY")
    if family:
        out += [{**case, "group": "family"} for case in family_cases(family)]
    names = [c["name"] for c in out]
    duplicate = sorted({n for n in names if names.count(n) > 1})
    if duplicate:
        raise ValueError(f"case names must be unique across benches/cases/: {', '.join(duplicate)}")
    return out


def problems(cases: list[dict], systems: dict, workloads: dict | None = None,
             datasets: set[str] | None = None) -> list[str]:
    """One line per problem: unknown keys or values, a comparator that isn't a system, a
    reference that isn't one of the case's systems, a full-level dataset that isn't frozen."""
    out = []
    for c in cases:
        where = f"{c.get('group', '?')}.toml [{c.get('name', '?')}]"
        missing = [k for k in REQUIRED if k not in c]
        if missing:
            out.append(f"{where}: missing {', '.join(missing)}")
            continue
        out += [f"{where}: unknown key {k!r}" for k in sorted(set(c) - KEYS)]
        for key, allowed in (("area", AREAS), ("semantics", SEMANTICS), ("tool", TOOLS), ("outcome", OUTCOMES),
                             ("scenario", SCENARIOS), ("rewrite", REWRITES)):
            if key in c and c[key] not in allowed:
                out.append(f"{where}: {key} {c[key]!r} is not one of {', '.join(sorted(allowed))}")
        for key, allowed in (("levels", LEVELS), ("needs", NEEDS), ("page_cache", PAGE_CACHE)):
            if key in c and not set(c[key]) <= allowed:
                out.append(f"{where}: {key} {sorted(set(c[key]) - allowed)} not in {', '.join(sorted(allowed))}")
        if not DATA.match(c["data"]):
            out.append(f"{where}: data {c['data']!r} is none, volume:FILE, ore:ID, concat:A+B or a generator spec")
        if datasets is not None and "full" in c.get("levels", []) and c["data"].startswith("volume:"):
            if c["data"][len("volume:"):] not in datasets:
                out.append(f"{where}: a full-level case reads a frozen dataset: {c['data']} isn't in "
                           "benches/datasets.toml")
        if c.get("outcome", "ok") != "ok" and not c.get("known"):
            out.append(f"{where}: an expected outcome ({c['outcome']}) names its issue in `known`")
        compared = set()
        for entry in c["systems"]:
            system, _, semantics = entry.partition(":")
            if system not in systems:
                out.append(f"{where}: comparator {system!r} is not in suite/systems.toml")
            elif systems[system].get("refused"):
                out.append(f"{where}: comparator {system!r} is never measured ({systems[system]['refused']})")
            if semantics not in COMPARATOR_SEMANTICS:
                out.append(f"{where}: comparator {entry!r}: semantics {semantics!r} is not one of "
                           f"{', '.join(sorted(COMPARATOR_SEMANTICS))}")
            compared.add(system)
        for entry in c.get("hangs") or {}:
            if entry not in c["systems"]:
                out.append(f"{where}: hangs names {entry!r}, which isn't among its systems")
        turf = c.get("home_turf")
        if turf and turf not in systems and turf not in OTHER_TURF:
            out.append(f"{where}: home_turf {turf!r} is not in suite/systems.toml")
        references = (c.get("expect") or {}).get("answers", [])
        for system in references if isinstance(references, list) else [references]:
            if system == "nrese" or system not in compared:
                out.append(f"{where}: expect.answers {system!r} must be one of the case's comparators (never NRESE)")
        if workloads is not None and c.get("workload") and c["workload"] not in workloads:
            out.append(f"{where}: workload {c['workload']!r} is not in suite/workloads.toml")
    return out


# Home turfs of systems without a runner here, named for the catalog (fast.py TURF).
OTHER_TURF = {"glog-vlog"}
