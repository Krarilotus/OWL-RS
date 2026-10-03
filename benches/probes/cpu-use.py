#!/usr/bin/env python3
"""How many cores a command keeps busy, over time: samples the process tree's CPU time
every 100 ms and prints the busy cores per interval, plus the totals. For finding the
serial stretches of a bulk load (a phase at 1–2 cores on a 16-thread machine is the next
thing to fix).

    python benches/probes/cpu-use.py [--every 0.25] -- COMMAND [ARGS...]
"""
from __future__ import annotations

import argparse
import subprocess
import time

import psutil


def tree_cpu(process: psutil.Process) -> float:
    total = 0.0
    for p in [process, *process.children(recursive=True)]:
        try:
            t = p.cpu_times()
            total += t.user + t.system
        except psutil.Error:
            pass
    return total


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--every", type=float, default=0.25)
    p.add_argument("command", nargs=argparse.REMAINDER)
    args = p.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    started = time.monotonic()
    child = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    process = psutil.Process(child.pid)
    last_t, last_cpu = started, 0.0
    line = []
    while child.poll() is None:
        time.sleep(args.every)
        try:
            cpu = tree_cpu(process)
        except psutil.Error:
            break
        now = time.monotonic()
        busy = (cpu - last_cpu) / (now - last_t)
        line.append(f"{busy:4.1f}")
        last_t, last_cpu = now, cpu
    wall = time.monotonic() - started
    print(f"busy cores every {args.every}s: " + " ".join(line))
    print(f"wall {wall:.2f} s, cpu {last_cpu:.2f} s, average {last_cpu / wall:.1f} cores of {psutil.cpu_count()}")
    return child.returncode or 0


if __name__ == "__main__":
    raise SystemExit(main())
