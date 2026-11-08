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
| code | path -> its git object id at that commit: what a cell can execute in the repository |
|  | (code_paths(): benches/ by entry less RESULTS, scripts, rust-toolchain.toml, .cargo; and |
|  | BUILD_PATHS for a compiled NRESE or harness); coarse, exact: any change re-qualifies it |
| images | image -> `id`, the one its containers ran (the run's pin map, ns.pinned), and `source`, |
|  | read off that id, never off the tag: "built from benches/CONTEXT@TREE" (its build label), |
|  | "pulled, pinned DIGEST" (the id holds its tag's pin, images.toml), else unknown, with why |
| build | NRESE's server and the timing harness: built in this run from `commit` (its `dirty`), with |
|  | `target_cpu`, inherited `rustflags_env`, `sources` (its program's BUILD_SOURCES -> git id at |
|  | the build's own commit, which is not the invocation's: what it compiled, by content), `inputs` |
|  | (BUILD_INPUTS -> "no files" or a digest: what its build scripts read from outside git), and |
|  | in Docker `rust_image`, the pinned `rust_image_id` it compiled in, and `rustflags`; or unknown |
| settings | the suite settings in force that change a measurement (manifest `settings` less |
|  | OPERATIONAL_SETTINGS: where a run keeps its files) |
| protocol | the attempt's own (manifest `protocol`): runs, query runs, cache modes, order, limits |
| data | input -> the SHA-256 of its bytes as the cell read them |
| outcome | its own rows' outcome (summary.Pair.outcome(): a table, its verdict in `outcome`) |
| answers | `worst`, `verdicts` (count each) and `qualified` (every query ok or deviates) of this |
|  | attempt; `not captured` without --answers full; `pending` until the run wrote its answers |

What a cell can't evidence is `{unknown = "why"}`, never an empty map: a driver or kit plan's
inputs, images and settings (they are its child's), an input outside the datasets, an external
driver, a binary not built here. Such a cell is never current.
"""
from __future__ import annotations

import dataclasses
import subprocess
from pathlib import Path

# What a cell can execute or read through the repository: benches/ entry by entry, scripts, and the
# toolchain's configuration; less what measuring itself writes into it, its records and the views made
# from them: committing a run's own record must not change the identity its receipt is held to
# (P2 design review F1). Coarse, exact: any other change re-qualifies the cell.
CODE_ROOTS = ("benches", "scripts", "rust-toolchain.toml", ".cargo")
RESULTS = ("benches/runs", "benches/baselines", "benches/STATUS.md", "benches/STANDINGS.md", "benches/CATALOG.md")
BUILD_PATHS = ("crates", "Cargo.toml", "Cargo.lock")  # a compiled NRESE server or timing harness
# What each program's build compiles, with the toolchain it compiles with (rustup and cargo read
# rust-toolchain.toml and .cargo from the repository root, where both builds run). The harness is a
# workspace of its own (--manifest-path, its own Cargo.lock) whose path dependencies are crates/
# members that inherit fields from the root Cargo.toml; it doesn't read the root Cargo.lock.
# Both build through scripts/cargo-guarded.sh, which sources the other two (P2 design check r4 B).
LAUNCHER = ("scripts/cargo-guarded.sh", "scripts/lib/target-cpu.sh", "scripts/lib/disk.sh")
BUILD_SOURCES = {
    "nrese": (*BUILD_PATHS, "rust-toolchain.toml", ".cargo", *LAUNCHER),
    "harness": ("benches/nrese-bench-harness", "crates", "Cargo.toml", "rust-toolchain.toml", ".cargo", *LAUNCHER),
}
# What a program's build scripts read that git doesn't hold (r4 A): crates/nrese-server/build.rs
# embeds every file of the ignored console build.
BUILD_INPUTS = {"nrese": ("apps/nrese-console/dist",), "harness": ()}


def code_paths(root: Path, commit: str) -> tuple[str, ...]:
    """CODE_ROOTS at `commit`, benches/ as its entries without RESULTS (none to list: benches/ whole,
    for object_ids() to say it can't be read)."""
    listed = subprocess.run(["git", "-C", str(root), "ls-tree", "--name-only", f"{commit}:benches"],
                            capture_output=True, text=True, timeout=60) if commit else None
    if listed is None or listed.returncode != 0:
        return CODE_ROOTS
    entries = [f"benches/{name}" for name in listed.stdout.splitlines() if name]
    return tuple(p for p in entries if p not in RESULTS) + CODE_ROOTS[1:]


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
