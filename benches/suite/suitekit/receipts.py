"""A cell's receipt: what one suite cell (a workload tier on one system) actually ran, written by
the driver into its invocation's manifest entry (`cells`) once the cell has run its whole protocol,
and carried by the ledger into the run record's pair. `suite.py coverage` compares it with what the
evaluated commit expects (Harness notes 2026-11-08-coverage-refresh/design.md, r2): no cell without
one, or with one that differs, counts as a current result.

| Field | What it holds |
|---|---|
| complete | true: every planned repetition ran to its end (a cell interrupted writes none) |
| invocation | its invocation's start, so that two invocations of one directory stay apart |
| commit, dirty | its invocation's checkout (manifest `git`) |
| code | repository path -> its git object id at that commit: the measurement code the cell ran |
| images | image -> content id, of the images its system's containers ran |
| build | NRESE's server: built from `commit` here, or `skipped` (--skip-build: not known) |
| settings | the suite settings in force (manifest `settings`) |
| data | /data input -> the SHA-256 of its bytes as the cell read them |
| outcome | its own rows' outcome (summary.Pair.outcome()), not its directory's |
"""
from __future__ import annotations

import dataclasses
import subprocess
from pathlib import Path

# Every suite cell's measurement code (deliberately coarse: a change anywhere under them makes every
# suite cell's receipt differ; a reviewed carry-forward for unaffected cells is a follow-up).
CODE_PATHS = ("benches/suite", "benches/cases")
CYCLE_PATHS = ("benches/nrese-bench-harness",)  # the client a served cell's queries are timed by


def cell_key(workload: str, tier: str, system: str) -> str:
    return f"{workload}/{tier}/{system}"


def repo_path(root: Path, arg) -> str | None:
    """`arg` as a path of the repository at `root`, relative (None: it names none: an option, a
    tier, an interpreter or a file outside it)."""
    text = str(arg)
    if "/" not in text.replace("\\", "/"):
        return None
    path = Path(text)
    try:
        rel = (path if path.is_absolute() else root / path).resolve().relative_to(root.resolve())
    except (ValueError, OSError):
        return None
    return rel.as_posix() if rel.parts and (root / rel).exists() else None


def code_paths(root: Path, plan, kit: str = "", own: tuple[str, ...] = ()) -> list[str]:
    """The repository paths a cell of `plan` runs: CODE_PATHS, the workload's kit, its system's own
    (`own`: an adapter's resources, Adapter.definition_paths), the timing client for a served cell,
    its queries, and every script its commands name (a file's whole directory: its helpers too)."""
    paths = {*CODE_PATHS, *own}
    if kit:
        paths.add(kit)
    if plan.kind == "cycle":
        paths.update(CYCLE_PATHS)
    if plan.queries:
        found = repo_path(root, plan.queries)
        if found:
            paths.add(found)
    for command in (plan.prepare, plan.command, plan.external, *(plan.driver or [])):
        for arg in command or []:
            found = repo_path(root, arg)
            if found:
                paths.add(found if (root / found).is_dir() else Path(found).parent.as_posix())
    return sorted(paths)


def object_ids(root: Path, commit: str, paths: list[str]) -> dict[str, str | None]:
    """path -> its git object id at `commit` (None: not there, or no commit to read)."""
    out = {}
    for path in paths:
        done = subprocess.run(["git", "-C", str(root), "rev-parse", "--verify", "--quiet", f"{commit}:{path}"],
                              capture_output=True, text=True, timeout=60) if commit else None
        out[path] = done.stdout.strip() if done is not None and done.returncode == 0 else None
    return out


def csv_row(result) -> dict:
    """A written Result as results.csv holds it: every field a string."""
    return {k: "" if v is None else str(v) for k, v in dataclasses.asdict(result).items()}
