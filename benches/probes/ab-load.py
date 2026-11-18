#!/usr/bin/env python3
"""Interleaved A/B comparison of bulk loads: two (or more) nrese-server binaries load the
same file into fresh on-disk stores in alternating order (A B A B ...), so that drift on
the machine (background I/O, scanning of new files, heat) falls on every binary alike. A
bisect over single runs at different times was misled by that drift (3 October 2026).

    python benches/probes/ab-load.py --runs 8 --data tmp/data/yago-tiny.nt A.exe B.exe
        [--env NAME=VALUE ...] [--scratch tmp/ab-store]

Prints, per binary, the median and spread of the load's own phases (NRESE's load log:
total, index build, checkpoint, and the rest: parse, encode, sort) and of the wall time.
"""
from __future__ import annotations

import argparse
import os
import re
import shutil
import statistics
import subprocess
import time
from pathlib import Path

PHASES = ("seconds", "index_build_ms", "checkpoint_ms")


def load(binary: str, data: list[str], store: Path, env: dict) -> dict[str, float]:
    shutil.rmtree(store, ignore_errors=True)
    started = time.monotonic()
    out = subprocess.run([binary, "load", *data], capture_output=True, text=True,
                         env=dict(os.environ, NRESE_STORE_MODE="on-disk", NRESE_DATA_DIR=str(store),
                                  RUST_LOG="info", NO_COLOR="1", **env))
    wall = time.monotonic() - started
    text = re.sub(r"\x1b\[[0-9;]*m", "", out.stdout + out.stderr)
    row = {"wall": wall}
    for phase in PHASES:
        m = re.findall(rf"{phase}=([0-9.]+)", text)
        if m:
            row[phase] = float(m[-1])
    if "seconds" in row:
        row["rest"] = row["seconds"] - (row.get("index_build_ms", 0) + row.get("checkpoint_ms", 0)) / 1000
    return row


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("binaries", nargs="+")
    p.add_argument("--data", action="append", required=True)
    p.add_argument("--runs", type=int, default=8)
    p.add_argument("--env", action="append", default=[])
    p.add_argument("--scratch", type=Path, default=Path("tmp/ab-store"))
    args = p.parse_args()
    env = dict(e.split("=", 1) for e in args.env)
    rows: dict[str, list[dict]] = {b: [] for b in args.binaries}
    for run in range(args.runs):
        order = args.binaries if run % 2 == 0 else list(reversed(args.binaries))
        for binary in order:
            rows[binary].append(load(binary, args.data, args.scratch, env))
            r = rows[binary][-1]
            print(f"run {run + 1} {Path(binary).name:28} " +
                  " ".join(f"{k}={v:.3f}" for k, v in r.items()), flush=True)
    shutil.rmtree(args.scratch, ignore_errors=True)
    print()
    keys = ["wall", "seconds", "rest", "index_build_ms", "checkpoint_ms"]
    print(f"{'binary':28} " + " ".join(f"{k:>22}" for k in keys))
    for binary, rs in rows.items():
        cells = []
        for k in keys:
            values = [r[k] for r in rs if k in r]
            if not values:
                cells.append(f"{'-':>22}")
                continue
            m = statistics.median(values)
            cells.append(f"{m:>10.3f} [{min(values):.2f}-{max(values):.2f}]".rjust(22))
        print(f"{Path(binary).name:28} " + " ".join(cells))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
