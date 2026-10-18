#!/usr/bin/env python3
"""A seed sweep of the seeded random and fuzz tests (benches/seed-sweeps.toml): every target's
seeded tests once per seed (NRESE_FUZZ_SEED), seed after seed, in the working tree it runs in
(a queue job's checkout). A problem-finder: untimed, resumable, a chunk at a time.

    python scripts/seed-sweep.py build [--targets FILE]
    python scripts/seed-sweep.py run OUT [--seeds 1-100000] [--first 52580,...] [--budget-min 27]

`build` compiles every target's tests (release with debug assertions and overflow checks:
release speed, the checks a debug build has). Each test run sits under its target's own memory
cap (`memory_mib` in the targets file, else --memory-mib, 4000) through scripts/capped-chunk.py;
a run that reaches it is CAP-REACHED, the sweep's setting, never counted as a finding. A chunk's
own cap must hold the largest target's (`run` says which). `run` writes OUT/results.tsv (seed, target,
status, seconds, cap) as it goes and a failure's whole output to OUT/failures/. A test the
budget stopped is INTERRUPTED (capped-chunk's exit 75), never ok. On resume a seed whose
latest row stands is skipped: ok, FAILED, or CAP-REACHED at the target's current cap; an
INTERRUPTED or BUILD-FAILED run, or a cap reached below a raised memory_mib, runs again. No
new seed starts past the budget. A failure is data, never the chunk's end: the chunk exits 0
and says FAILED with the seed, and every chunk's summary counts the sweep's findings so far,
the earlier chunks' included (the queue goes on with the sweep).

A failure found on a host under diagnosis (hosts.toml [[host.<key>.unstable]], Khaleesi since
12 Oct 2026) is reproduced on another host before anyone acts on it.
"""
from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ENV = {"CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS": "true", "CARGO_PROFILE_RELEASE_OVERFLOW_CHECKS": "true"}


def targets(path: Path) -> list[dict]:
    with open(path, "rb") as f:
        return tomllib.load(f)["target"]


def name(t: dict) -> str:
    return f"{t['crate']}:{'lib' if t.get('lib') else t['test']}"


CAP_REACHED = 86  # scripts/capped-chunk.py's exit status at its memory cap
BUDGET_ENDED = 75  # and when its time budget stopped the test (not done, run again)


def capped(t: dict, extra: list[str], default_mib: int, budget_min: float) -> list[str]:
    """A target's test run under its own memory cap (14 Oct 2026: tableau_fuzz's seed 600026
    needs 6 GB; the sweep's 4 GiB ended it, and every answer was right)."""
    return [sys.executable, str(ROOT / "scripts" / "capped-chunk.py"), "--memory-mib",
            str(t.get("memory_mib", default_mib)), "--budget-min", f"{max(budget_min, 1):g}", "--", *cargo(t, extra)]


def status_of(returncode: int, stderr: str) -> str:
    """ok, FAILED(code) (a finding), or the sweep's own problems, never counted as findings:
    BUILD-FAILED (13 Oct 2026: nrese-server's test binary under the memory cap), CAP-REACHED
    and INTERRUPTED (the budget stopped the test: no answer yet)."""
    if returncode == 0:
        return "ok"
    if returncode == BUDGET_ENDED:
        return "INTERRUPTED"
    if "could not compile" in stderr or "process didn't exit successfully" in stderr.split("Running")[0]:
        return "BUILD-FAILED"
    if returncode == CAP_REACHED:
        return "CAP-REACHED"
    return f"FAILED({returncode})"


def final(status: str, cap_then: str, cap_now: int) -> bool:
    """Whether a recorded run stands: ok and findings do; a cap reached stands at the same cap
    (raising the target's memory_mib runs it again); an interrupted run or a failed build
    doesn't."""
    if status == "ok" or status.startswith("FAILED("):
        return True
    return status == "CAP-REACHED" and cap_then == str(cap_now)


def recorded(results: Path) -> dict[tuple[str, str], list[str]]:
    """(seed, target) -> its latest row of results.tsv."""
    latest = {}
    if results.exists():
        for line in results.read_text(encoding="utf-8").splitlines():
            parts = line.split("\t")
            if len(parts) >= 3:
                latest[(parts[0], parts[1])] = parts
    return latest


def summary(results: Path) -> str:
    """Every finding in the table, from this chunk and the chunks before it."""
    found = sorted(((int(seed), target) for (seed, target), row in recorded(results).items()
                    if row[2].startswith("FAILED(")), key=lambda f: f)
    if not found:
        return "0 failures in the sweep so far"
    shown = ", ".join(f"seed {seed} {target}" for seed, target in found[:20])
    return f"{len(found)} failure{'s' if len(found) > 1 else ''} in the sweep so far: {shown}" + \
        (" ..." if len(found) > 20 else "")


def cargo(t: dict, extra: list[str]) -> list[str]:
    selector = ["--lib"] if t.get("lib") else ["--test", t["test"]]
    return ["cargo", "test", "--release", "--locked", "-p", t["crate"], *selector, *extra]


def seeds(spec: str, first: str) -> list[int]:
    out = [int(s) for s in first.split(",") if s]
    low, _, high = spec.partition("-")
    out += [s for s in range(int(low), int(high or low) + 1) if s not in out]
    return out


def build(args) -> int:
    env = {**os.environ, **ENV}
    failed = 0
    for t in targets(args.targets):
        done = subprocess.run(cargo(t, ["--no-run"]), env=env, cwd=Path.cwd())
        print(f"build {name(t)}: exit {done.returncode}", flush=True)
        failed += done.returncode != 0
    return 1 if failed else 0


def run(args) -> int:
    out = Path(args.out)
    (out / "failures").mkdir(parents=True, exist_ok=True)
    results = out / "results.tsv"
    latest = recorded(results)
    deadline = time.monotonic() + args.budget_min * 60
    found = 0
    largest = max(t.get("memory_mib", args.memory_mib) for t in targets(args.targets))
    print(f"each test under its target's cap; the largest is {largest} MiB: the chunk's own cap must hold it",
          flush=True)
    for seed in seeds(args.seeds, args.first):
        for t in targets(args.targets):
            cap = t.get("memory_mib", args.memory_mib)
            row = latest.get((str(seed), name(t)))
            if row and final(row[2], row[4] if len(row) > 4 else "", cap):
                continue
            if time.monotonic() > deadline:
                print(f"budget used: stopped before seed {seed} {name(t)}; {found} failures in this chunk; "
                      f"{summary(results)}; the next chunk resumes", flush=True)
                return 0
            env = {**os.environ, **ENV, "NRESE_FUZZ_SEED": str(seed)}
            started = time.monotonic()
            remaining = (deadline - time.monotonic()) / 60
            r = subprocess.run(capped(t, ["--", *t["filters"]], args.memory_mib, remaining + 5), env=env,
                               cwd=Path.cwd(), capture_output=True, text=True, encoding="utf-8", errors="replace")
            seconds = time.monotonic() - started
            status = status_of(r.returncode, r.stderr)
            with open(results, "a", encoding="utf-8", newline="\n") as f:
                f.write(f"{seed}\t{name(t)}\t{status}\t{seconds:.1f}\t{cap}\n")
            if status in ("BUILD-FAILED", "CAP-REACHED", "INTERRUPTED"):
                log = out / "failures" / f"{name(t).replace(':', '-')}-{seed}-{status.lower()}.log"
                log.write_text(r.stdout + "\n" + r.stderr, encoding="utf-8")
                why = {"BUILD-FAILED": "the sweep's build",
                       "INTERRUPTED": "the budget stopped it; the next chunk runs it again",
                       "CAP-REACHED": f"the target's memory cap ({cap} MiB): raise its memory_mib"}[status]
                print(f"{status}: {name(t)} at seed {seed} ({log.name}): {why}, not a finding", flush=True)
            elif r.returncode != 0:
                found += 1
                log = out / "failures" / f"{name(t).replace(':', '-')}-{seed}.log"
                log.write_text(f"NRESE_FUZZ_SEED={seed} {' '.join(cargo(t, ['--', *t['filters']]))}\n\n"
                               + r.stdout + "\n" + r.stderr, encoding="utf-8")
                print(f"FAILED: seed {seed} {name(t)} ({log.name}): reproduce on another host before acting",
                      flush=True)
    print(f"every seed of the range done; {found} failures in this chunk; {summary(results)}", flush=True)
    return 0


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--targets", type=Path, default=ROOT / "benches" / "seed-sweeps.toml")
    sub = p.add_subparsers(dest="command", required=True)
    sub.add_parser("build")
    r = sub.add_parser("run")
    r.add_argument("out")
    r.add_argument("--seeds", default="1-100000")
    r.add_argument("--first", default="", help="seeds to run before the range, comma-separated")
    r.add_argument("--budget-min", type=float, default=27)
    r.add_argument("--memory-mib", type=int, default=4000, help="a target's cap unless it sets memory_mib")
    args = p.parse_args()
    return {"build": build, "run": run}[args.command](args)


if __name__ == "__main__":
    sys.exit(main())
