"""A cell's attempt and receipt: what one suite cell (a workload tier on one system) ran. The
attempt is appended to the results' attempts.jsonl, on disk, before the cell's work starts
(manifest.append_attempts); its receipt goes into its invocation's manifest entry (`cells`), under
the same attempt id, once the cell has run its whole protocol, and its answers once the run's
answers.json is written. The ledger takes the newest attempt of each cell, finished or not, its
receipt only under that attempt, and never an older one in its place; `suite.py
coverage` compares it with what the evaluated commit expects (Harness notes
2026-11-08-coverage-refresh/design.md r2; review cell-receipts r1).

| Field | What it holds |
|---|---|
| attempt, started | the attempt's id and start: the newest attempt of a cell is its evidence |
| complete | false until the cell ran its whole protocol (an interrupted attempt stays false) |
| commit, dirty | its invocation's checkout (manifest `git`) |
| code | path -> its git tree id at that commit: all a cell can read in the repository (CODE_PATHS, |
|  | and BUILD_PATHS for a compiled NRESE or harness); coarse, exact: any change re-qualifies it |
| images | image -> the id its containers ran, from the run's pin map (ns.pinned) |
| build | NRESE's server and the timing harness: built in this run from `commit`, or unknown |
| settings | the suite settings in force (manifest `settings`) |
| data | input -> the SHA-256 of its bytes as the cell read them |
| outcome | its own rows' outcome (summary.Pair.outcome()) |
| answers | its worst answer verdict this attempt (answers.PRECEDENCE); `not captured` without |
|  | --answers full; `pending` until the run wrote its answers |

What a cell can't evidence is `{unknown = "why"}`, never an empty map: a driver or kit plan's
inputs, images and settings (they are its child's), an input outside the datasets, an external
driver, a binary not built here. Such a cell is never current.
"""
from __future__ import annotations

import dataclasses
import subprocess
from pathlib import Path

CODE_PATHS = ("benches", "scripts")
BUILD_PATHS = ("crates", "Cargo.toml", "Cargo.lock")  # a compiled NRESE server or timing harness


def cell_key(workload: str, tier: str, system: str) -> str:
    return f"{workload}/{tier}/{system}"


def unknown(why: str) -> dict:
    return {"unknown": why}


def object_ids(root: Path, commit: str, paths: tuple[str, ...] | list[str]) -> dict:
    """path -> its git object id at `commit`; unknown where there is no commit to read or the
    path isn't in it."""
    out = {}
    for path in paths:
        done = subprocess.run(["git", "-C", str(root), "rev-parse", "--verify", "--quiet", f"{commit}:{path}"],
                              capture_output=True, text=True, timeout=60) if commit else None
        out[path] = (done.stdout.strip() if done is not None and done.returncode == 0
                     else unknown("no commit to read" if not commit else "not in its commit"))
    return out


def csv_row(result) -> dict:
    """A written Result as results.csv holds it: every field a string."""
    return {k: "" if v is None else str(v) for k, v in dataclasses.asdict(result).items()}
