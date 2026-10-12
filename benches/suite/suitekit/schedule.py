"""benches/suite/schedule.toml: the order the benchmark families are measured in.

    load()               the schedule
    rank(name)           a family's place: its index in `now`, then the backlog, then `last`
    problems(workloads)  families in no list or in two, names that are nothing
"""
from __future__ import annotations

import tomllib
from pathlib import Path

SCHEDULE = Path(__file__).resolve().parent.parent / "schedule.toml"
LISTS = ("now", "backlog", "last")
# A job naming no family, or one the schedule doesn't know, waits behind `now` and before
# the backlog.
UNPLACED = 500


def load(path: Path = SCHEDULE) -> dict:
    with open(path, "rb") as f:
        return tomllib.load(f)


def rank(name: str | None, schedule: dict | None = None) -> int:
    schedule = load() if schedule is None else schedule
    for offset, key in zip((0, 1000, 2000), LISTS):
        if name in schedule.get(key, []):
            return offset + schedule[key].index(name)
    return UNPLACED


def problems(workloads: dict, schedule: dict | None = None) -> list[str]:
    schedule = load() if schedule is None else schedule
    out = []
    listed = [name for key in LISTS for name in schedule.get(key, [])]
    catalogue = schedule.get("catalogue", {})
    for name in sorted({n for n in listed if listed.count(n) > 1}):
        out.append(f"schedule.toml: {name} is in more than one place")
    for name in sorted(set(workloads) - set(listed)):
        out.append(f"schedule.toml: workload {name} is in no list (now, backlog, last)")
    for name in sorted(set(listed) - set(workloads) - set(catalogue)):
        out.append(f"schedule.toml: {name} is neither a workload nor in [catalogue]")
    for name in sorted(set(catalogue) - set(listed)):
        out.append(f"schedule.toml: catalogue entry {name} is in no list")
    for name in sorted(set(catalogue) & set(workloads)):
        out.append(f"schedule.toml: {name} is a workload now: remove it from [catalogue]")
    return out
