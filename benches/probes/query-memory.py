#!/usr/bin/env python3
"""Peak resident memory per query: for each query of a directory, a fresh server on an
existing store runs it three times while its memory is sampled every 20 ms. Several
binaries side by side show which query's memory changed between versions.

    python benches/probes/query-memory.py --store-of BIN=STORE ... --queries DIR \
        [--regime owl2-rl] [--cache off]
"""
from __future__ import annotations

import argparse
import os
import subprocess
import threading
import time
import urllib.parse
import urllib.request
from pathlib import Path

import psutil


def run(binary: str, store: str, query: str, regime: str, cache: str, port: int) -> tuple[float, float, float]:
    env = dict(os.environ, NRESE_STORE_MODE="on-disk", NRESE_DATA_DIR=store, RUST_LOG="warn",
               NRESE_REASONING_MODE=regime, NRESE_BIND_ADDR=f"127.0.0.1:{port}",
               NRESE_READ_REQUESTS_PER_WINDOW="100000000")
    if cache == "off":
        env["NRESE_QUERY_CACHE_BYTES"] = "0"
    p = subprocess.Popen([binary], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    proc = psutil.Process(p.pid)
    base = f"http://127.0.0.1:{port}"
    for _ in range(300):
        try:
            urllib.request.urlopen(base + "/readyz", timeout=1)
            break
        except Exception:
            time.sleep(0.05)
    start_rss = proc.memory_info().rss / 2**20
    peak = [start_rss]
    stop = threading.Event()

    def sample():
        while not stop.is_set():
            try:
                peak[0] = max(peak[0], proc.memory_info().rss / 2**20)
            except psutil.Error:
                return
            time.sleep(0.02)

    t = threading.Thread(target=sample, daemon=True)
    t.start()
    times = []
    for _ in range(3):
        started = time.monotonic()
        urllib.request.urlopen(base + "/dataset/query?" + urllib.parse.urlencode({"query": query}),
                               timeout=300).read()
        times.append(1000 * (time.monotonic() - started))
    time.sleep(0.3)
    stop.set()
    t.join()
    p.terminate()
    p.wait()
    return start_rss, peak[0], min(times)


def main() -> int:
    a = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    a.add_argument("--store-of", action="append", required=True, metavar="BIN=STORE")
    a.add_argument("--queries", required=True, type=Path)
    a.add_argument("--regime", default="disabled")
    a.add_argument("--cache", default="off")
    args = a.parse_args()
    pairs = [s.split("=", 1) for s in args.store_of]
    print(f"{'query':28} " + " ".join(f"{Path(b).stem[:18]:>30}" for b, _ in pairs))
    for q in sorted(args.queries.glob("*.rq")):
        cells = []
        for i, (binary, store) in enumerate(pairs):
            start, peak, ms = run(binary, store, q.read_text(encoding="utf-8"), args.regime, args.cache, 18970 + i)
            cells.append(f"{peak - start:7.0f} MiB +, {ms:8.1f} ms".rjust(30))
        print(f"{q.stem[:28]:28} " + " ".join(cells), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
