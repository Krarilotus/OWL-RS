"""The suite's one result schema: a CSV row per measured step.

| Field | Meaning |
|---|---|
| date | when the run started (ISO 8601, minutes) |
| host, runtime | the machine and how the system ran (docker, apptainer, process) |
| system, version, publish | the system (systems.toml key), its version where it says, and whether its results may be published (free) or need the vendor's permission |
| workload, tier, regime | the workload (workloads.toml key), its size, and the entailment regime the system ran (none, rdfs, owl-horst, owl2-rl, owl2-ql) |
| run | the repetition (a fresh store each) |
| task | load, reason, size, restart, count, query, update, serve, conformance (a skipped pair: load, status skipped) |
| item | the query or test the row is about (task query, conformance) |
| repeat | the measured repetition of a query within a run; 0 is its first execution on the fresh server (a warm-up run, not in the medians) |
| status | ok, failed, timeout, wrong (an answer count differs from the expected one), skipped |
| ms | wall time |
| peak_mib | peak resident memory of the step |
| rows | answers (query), statements (load: asserted; reason: inferred; count: all answered) |
| bytes | store size (task size) |
| note | what else the row needs to be read right |
| cache | the system's result cache while the queries ran: off, on (the system's default), or - (it has none) |
| order | the query order: fixed (each query's runs back to back), or shuffled:SEED (rounds over all queries, each in a new order; the same seed gives every system the same orders) |

The results of systems with publish = permission stay in the result files; `suite.py
report` leaves them out unless asked (and they may not leave the machine without the
vendor's written consent: benches/competitors/README.md).
"""
from __future__ import annotations

import csv
from dataclasses import asdict, dataclass, fields
from pathlib import Path

TASKS = {"load", "reason", "size", "restart", "count", "query", "update", "serve", "conformance"}
STATUSES = {"ok", "failed", "timeout", "wrong", "skipped"}
REGIMES = {"none", "rdfs", "owl-horst", "owl2-rl", "owl2-ql", "-"}


@dataclass
class Result:
    date: str
    host: str
    runtime: str
    system: str
    version: str
    publish: str
    workload: str
    tier: str
    regime: str
    run: int
    task: str
    item: str = ""
    repeat: int | str = ""
    status: str = "ok"
    ms: float | str = ""
    peak_mib: int | str = ""
    rows: int | str = ""
    bytes: int | str = ""
    note: str = ""
    cache: str = "-"
    order: str = "-"


FIELDS = [f.name for f in fields(Result)]


def problems(result: Result) -> list[str]:
    out = []
    if result.task not in TASKS:
        out.append(f"unknown task {result.task!r}")
    if result.status not in STATUSES:
        out.append(f"unknown status {result.status!r}")
    if result.regime not in REGIMES:
        out.append(f"unknown regime {result.regime!r}")
    if result.publish not in ("free", "permission"):
        out.append(f"publish must be free or permission, not {result.publish!r}")
    if result.task == "query" and not result.item:
        out.append("a query row names its query (item)")
    return out


class Writer:
    """Appends results to a CSV file (with its header when new). A file written with other
    fields (an older schema) is left alone: the rows go to results-2.csv, -3, … instead."""

    def __init__(self, path: Path, echo=None):
        n = 1
        while path.exists() and path.stat().st_size > 0 and header(path) != FIELDS:
            n += 1
            path = path.with_name(f"{path.stem.split('-')[0]}-{n}{path.suffix}")
        self.path = path
        self.echo = echo
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists() or path.stat().st_size == 0:
            with open(path, "w", newline="", encoding="utf-8") as f:
                csv.writer(f).writerow(FIELDS)

    def write(self, result: Result):
        found = problems(result)
        if found:
            raise ValueError(f"result {result}: {'; '.join(found)}")
        row = asdict(result)
        if isinstance(row["ms"], float):
            row["ms"] = f"{row['ms']:.2f}"
        with open(self.path, "a", newline="", encoding="utf-8") as f:
            csv.DictWriter(f, FIELDS).writerow(row)
        if self.echo:
            shown = [result.system, result.workload, result.tier, f"run {result.run}", result.task]
            if result.cache != "-":
                shown.append(f"cache {result.cache}")
            if result.item:
                shown.append(result.item)
            if result.task == "query" and result.repeat == 0:
                shown.append("first")
            shown.append(result.status)
            for key, unit in (("ms", " ms"), ("peak_mib", " MiB"), ("rows", " rows"), ("bytes", " bytes")):
                value = row[key]
                if value not in ("", None):
                    shown.append(f"{value}{unit}")
            if result.note:
                shown.append(f"({result.note})")
            self.echo("  " + " ".join(str(s) for s in shown))


def header(path: Path) -> list[str]:
    with open(path, newline="", encoding="utf-8") as f:
        return next(csv.reader(f), [])


def read(path: Path) -> list[dict]:
    with open(path, newline="", encoding="utf-8") as f:
        return list(csv.DictReader(f))
