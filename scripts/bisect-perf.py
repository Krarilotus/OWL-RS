#!/usr/bin/env python3
"""Bisects a slowdown or a memory growth over a branch's first-parent commits, measuring every
revision itself (the owner, 14 Oct 2026, after a bisect that named the wrong merge: one revision's
row carried its neighbour's numbers, read from the wrong report).

    python scripts/bisect-perf.py --good REV --bad REV --out DIR --metric PATH \\
        [--reps 3] [--cap-gb 8] -- perf_lab ARGS...

- **Revisions:** the first-parent commits after GOOD up to BAD that changed Rust code (crates/,
  Cargo.toml, Cargo.lock, rust-toolchain.toml), oldest first.
- **A measurement** is REV's own `perf_lab` (built at REV with its pinned toolchain into
  DIR/bin/REV/), run REPS times with ARGS and `--json` to a report of its own
  (DIR/runs/REV-K.json), each run in a user scope capped at CAP_GB (in the fleet step's slice,
  FLEET_SLICE, where set) and ended with it. METRIC is read from that report alone: `sum_p50_ms`,
  `peak_mib`, `queries.NAME.p50_ms` (the query named NAME), or `scope_peak_mib` (the scope's
  memory.peak). Every run is a row of DIR/bisect.tsv: revision, binary SHA-256, run, value, report.
- **Refusals:** two revisions whose runs gave bit-identical values (a result reused, not measured);
  GOOD and BAD that don't separate (every GOOD run below the threshold, every BAD run above it;
  the threshold is their medians' geometric mean); a run without the metric.
- **The verdict:** each step judges a revision by its runs' median against the threshold. Before
  naming the first bad commit, both ends of the last step are measured again and must judge as
  before; the verdict names them with both measurements.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import shutil
import statistics
import subprocess
import sys
from pathlib import Path

RUST = ["crates", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml"]


class Refused(Exception):
    pass


def git(*args: str, cwd: Path | None = None) -> str:
    return subprocess.run(["git", *args], capture_output=True, text=True, check=True, cwd=cwd).stdout.strip()


def revisions(good: str, bad: str) -> list[str]:
    """The first-parent commits after `good` up to `bad` that changed Rust code, oldest first."""
    out = []
    for rev in git("log", "--first-parent", "--reverse", "--format=%H", f"{good}..{bad}").split():
        if git("diff", "--name-only", f"{rev}^1", rev, "--", *RUST):
            out.append(rev)
    full_bad = git("rev-parse", bad)
    if not out or out[-1] != full_bad:
        out.append(full_bad)
    return out


def metric(report: dict, path: str, scope_peak_mib: int | None = None) -> float:
    """METRIC from one run's own report."""
    if path == "scope_peak_mib":
        if scope_peak_mib is None:
            raise Refused("the run printed no scope peak")
        return float(scope_peak_mib)
    parts = path.split(".")
    if parts[0] == "queries" and len(parts) == 3:
        found = [q for q in report.get("queries", []) if q.get("name") == parts[1]]
        if not found or found[0].get(parts[2]) is None:
            raise Refused(f"no {path} in the report")
        return float(found[0][parts[2]])
    if report.get(path) is None:
        raise Refused(f"no {path} in the report")
    return float(report[path])


def threshold(good: list[float], bad: list[float]) -> tuple[float, bool]:
    """The geometric mean of the medians, and whether higher is bad. Refused unless every run
    of each end lies on its own side."""
    g, b = statistics.median(good), statistics.median(bad)
    if g <= 0 or b <= 0 or g == b:
        raise Refused(f"the ends don't separate: good {good}, bad {bad}")
    cut, higher_bad = math.sqrt(g * b), b > g
    separated = (max(good) < cut < min(bad)) if higher_bad else (min(good) > cut > max(bad))
    if not separated:
        raise Refused(f"the ends overlap around {cut:.4g}: good {good}, bad {bad}")
    return cut, higher_bad


def is_bad(values: list[float], cut: float, higher_bad: bool) -> bool:
    m = statistics.median(values)
    return m > cut if higher_bad else m < cut


class Bisect:
    """The search, with `measure(rev) -> list[float]` measuring a revision afresh at each call."""

    def __init__(self, revs: list[str], good: str, measure, say=print):
        self.revs, self.good, self.measure, self.say = revs, good, measure, say
        self.seen: dict[tuple, str] = {}

    def take(self, rev: str) -> list[float]:
        values = self.measure(rev)
        key = tuple(values)
        other = self.seen.get(key)
        if other is not None and other != rev:
            raise Refused(f"{rev[:10]} and {other[:10]} gave bit-identical runs {list(key)}: "
                          "a result reused, not measured")
        self.seen[key] = rev
        return values

    def run(self) -> dict:
        good_values, bad_values = self.take(self.good), self.take(self.revs[-1])
        cut, higher_bad = threshold(good_values, bad_values)
        self.say(f"threshold {cut:.4g} ({'higher' if higher_bad else 'lower'} is bad): "
                 f"good {self.good[:10]} {good_values}, bad {self.revs[-1][:10]} {bad_values}")
        lo, hi = -1, len(self.revs) - 1
        while hi - lo > 1:
            mid = (lo + hi) // 2
            values = self.take(self.revs[mid])
            bad = is_bad(values, cut, higher_bad)
            self.say(f"{self.revs[mid][:10]}: {values} -> {'bad' if bad else 'good'}")
            hi, lo = (mid, lo) if bad else (hi, mid)
        last_good = self.good if lo < 0 else self.revs[lo]
        first_bad = self.revs[hi]
        # Both ends of the last step again, measured afresh, before anything is named.
        again_good, again_bad = self.take(last_good), self.take(first_bad)
        if is_bad(again_good, cut, higher_bad) or not is_bad(again_bad, cut, higher_bad):
            raise Refused(f"the last step didn't hold when measured again: {last_good[:10]} {again_good}, "
                          f"{first_bad[:10]} {again_bad}")
        return {"first_bad": first_bad, "last_good": last_good, "threshold": cut, "higher_bad": higher_bad,
                "first_bad_runs": again_bad, "last_good_runs": again_good}


class Harness:
    """Builds and runs one revision's perf_lab, each run its own report and row."""

    def __init__(self, out: Path, args: list[str], path: str, reps: int, cap_gb: float):
        self.out, self.args, self.path, self.reps, self.cap_gb = out, args, path, reps, cap_gb
        self.target = out / "target"
        self.calls: dict[str, int] = {}
        (out / "runs").mkdir(parents=True, exist_ok=True)
        self.rows = out / "bisect.tsv"
        if not self.rows.exists():
            self.rows.write_text("revision\tsubject\tbinary_sha256\trun\tvalue\treport\n", encoding="utf-8")

    def binary(self, rev: str) -> tuple[Path, str]:
        home = self.out / "bin" / rev[:12]
        exe = home / "perf_lab"
        if not exe.exists():
            src = self.out / "src" / rev[:12]
            if not src.exists():
                git("worktree", "add", "-q", "--detach", str(src), rev)
            cargo = ["bash", "scripts/cargo-guarded.sh", "build", "--release", "--locked", "--quiet",
                     "-p", "nrese-store", "--example", "perf_lab"]
            env = {**os.environ, "CARGO_TARGET_DIR": str(self.target)}
            subprocess.run(cargo, cwd=src, env=env, check=True)
            home.mkdir(parents=True, exist_ok=True)
            shutil.copy2(self.target / "release" / "examples" / "perf_lab", exe)
            (home / "REVISION").write_text(rev + "\n", encoding="utf-8")
            git("worktree", "remove", "--force", str(src))
        if (home / "REVISION").read_text(encoding="utf-8").strip() != rev:
            raise Refused(f"{exe} isn't {rev}'s build")
        return exe, hashlib.sha256(exe.read_bytes()).hexdigest()

    def scoped(self, unit: str, command: list[str]) -> list[str]:
        if not shutil.which("systemd-run"):
            return command
        placed = [f"--slice={os.environ['FLEET_SLICE']}"] if os.environ.get("FLEET_SLICE") else []
        wrapper = ('"$@"; rc=$?; g=/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup); '
                   'echo "scope-peak $(cat "$g/memory.peak" 2>/dev/null)" >&2; exit $rc')
        return ["systemd-run", "--user", "--scope", "-q", *placed, f"--unit={unit}",
                "-p", f"MemoryMax={int(self.cap_gb * 1024)}M", "-p", "MemorySwapMax=0", "sh", "-c", wrapper, "sh",
                *command]

    def __call__(self, rev: str) -> list[float]:
        exe, sha = self.binary(rev)
        subject = git("log", "-1", "--format=%s", rev)[:80].replace("\t", " ")
        values = []
        for _ in range(self.reps):
            k = self.calls[rev] = self.calls.get(rev, 0) + 1
            report = self.out / "runs" / f"{rev[:12]}-{k}.json"
            if report.exists():
                raise Refused(f"{report} exists already: a run is never read twice")
            unit = f"bisect-perf-{os.getpid()}-{rev[:8]}-{k}"
            done = subprocess.run(self.scoped(unit, [str(exe), *self.args, "--json", str(report)]),
                                  capture_output=True, text=True)
            if shutil.which("systemctl"):
                subprocess.run(["systemctl", "--user", "kill", "--signal=KILL", f"{unit}.scope"], capture_output=True)
            (self.out / "runs" / f"{rev[:12]}-{k}.log").write_text(done.stdout + done.stderr, encoding="utf-8")
            if done.returncode != 0 or not report.exists():
                raise Refused(f"{rev[:10]} run {k} failed (exit {done.returncode}): {done.stderr.strip()[-300:]}")
            peaks = [int(line.split()[1]) // 1048576 for line in done.stderr.splitlines()
                     if line.startswith("scope-peak ") and line.split()[1:2] and line.split()[1].isdigit()]
            value = metric(json.loads(report.read_text(encoding="utf-8")), self.path, peaks[-1] if peaks else None)
            values.append(value)
            with self.rows.open("a", encoding="utf-8") as f:
                f.write(f"{rev}\t{subject}\t{sha}\t{k}\t{value!r}\t{report.name}\n")
        return values


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--good", required=True)
    p.add_argument("--bad", required=True)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--metric", required=True)
    p.add_argument("--reps", type=int, default=3)
    p.add_argument("--cap-gb", type=float, default=8)
    p.add_argument("command", nargs=argparse.REMAINDER, help="-- perf_lab ARGS (without --json)")
    args = p.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if command[:1] == ["perf_lab"]:
        command = command[1:]
    good = git("rev-parse", args.good)
    revs = revisions(good, args.bad)
    args.out.mkdir(parents=True, exist_ok=True)
    log = args.out / "verdict.txt"

    def say(text: str) -> None:
        print(text, flush=True)
        with log.open("a", encoding="utf-8") as f:
            f.write(text + "\n")

    say(f"{len(revs)} revisions after {good[:10]} up to {revs[-1][:10]}; metric {args.metric}, {args.reps} runs each")
    try:
        found = Bisect(revs, good, Harness(args.out, command, args.metric, args.reps, args.cap_gb), say).run()
    except Refused as why:
        say(f"REFUSED: {why}")
        return 2
    for key in ("last_good", "first_bad"):
        rev = found[key]
        say(f"{'FIRST BAD' if key == 'first_bad' else 'last good'}: {rev[:10]} "
            f"{git('log', '-1', '--format=%s', rev)} (measured again: {found[key + '_runs']})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
