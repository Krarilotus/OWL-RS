"""benches/competitors/reported.toml: published numbers of systems we don't measure, each as
its source reports it, on its source's hardware.

    load()               the entries
    problems(systems)    an entry without a required field, or of a system we don't know
"""
from __future__ import annotations

import tomllib
from pathlib import Path

REPORTED = Path(__file__).resolve().parents[2] / "competitors" / "reported.toml"
REQUIRED = ("system", "benchmark", "metric", "value", "unit", "hardware", "source", "date")


def load(path: Path = REPORTED) -> list[dict]:
    if not path.exists():
        return []
    with open(path, "rb") as f:
        return tomllib.load(f).get("reported", [])


def problems(systems: dict, entries: list[dict] | None = None) -> list[str]:
    out = []
    for n, entry in enumerate(load() if entries is None else entries, start=1):
        missing = [k for k in REQUIRED if entry.get(k) in (None, "")]
        if missing:
            out.append(f"reported.toml entry {n}: missing {', '.join(missing)} (as its source reports it)")
        if entry.get("system") and entry["system"] not in systems:
            out.append(f"reported.toml entry {n}: system {entry['system']!r} is not in suite/systems.toml")
    return out
