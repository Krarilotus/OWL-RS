"""Published numbers of systems we don't measure, each as its source reports it, on its
source's hardware: benches/competitors/reported.toml, and a kit's reported rows
(benches/*/reported/*.csv: the result schema with runtime `reported`, the paper's hardware in
`host`, its source first in `note`; benches/reasoning/reported).

    load()               the entries, of both
    problems(systems)    an entry without a required field, or of a system we don't know
"""
from __future__ import annotations

import tomllib
from pathlib import Path

BENCHES = Path(__file__).resolve().parents[2]
REPORTED = BENCHES / "competitors" / "reported.toml"
REQUIRED = ("system", "benchmark", "metric", "value", "unit", "hardware", "source", "date")


def from_rows(path: Path) -> list[dict]:
    """A kit's reported rows as entries: a time in seconds, else a peak in MiB."""
    from .schema import read
    out = []
    for r in read(path):
        if r.get("runtime") != "reported":
            continue
        # A run that didn't finish is reported as such (its status: timeout, failed).
        value, unit = (round(float(r["ms"]) / 1000, 3), "s") if r["ms"] else             (r["peak_mib"], "MiB") if r["peak_mib"] else (r["rows"], "facts") if r["rows"] else (r["status"], "")
        version = "" if r["version"] in ("", "-") else f"{r['version']}; "
        out.append({"system": r["system"], "benchmark": r["workload"], "tier": r["tier"],
                    "metric": f"{r['task']} {r['item']}".strip(), "value": value, "unit": unit,
                    "hardware": r["host"], "source": version + r["note"], "date": r["date"]})
    return out


def load(path: Path = REPORTED, rows: list[Path] | None = None) -> list[dict]:
    out = []
    if path.exists():
        with open(path, "rb") as f:
            out += tomllib.load(f).get("reported", [])
    for csv_path in sorted(BENCHES.glob("*/reported/*.csv")) if rows is None else rows:
        out += from_rows(csv_path)
    return out


def problems(systems: dict, entries: list[dict] | None = None) -> list[str]:
    out = []
    for n, entry in enumerate(load() if entries is None else entries, start=1):
        missing = [k for k in REQUIRED if entry.get(k) in (None, "") and not (k == "unit" and entry.get("value") in ("timeout", "failed"))]
        if missing:
            out.append(f"reported.toml entry {n}: missing {', '.join(missing)} (as its source reports it)")
        if entry.get("system") and entry["system"] not in systems:
            out.append(f"reported.toml entry {n}: system {entry['system']!r} is not in suite/systems.toml")
    return out
