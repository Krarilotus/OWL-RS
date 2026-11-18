#!/usr/bin/env python3
"""Serving memory of NRESE, sampled finely: loads a dataset into a fresh store, starts the
server, runs the suite's query mix through the harness, and samples the server's resident
memory every 100 ms (psutil). Prints the peak per phase: idle after start, during the
first round of queries (the statistics are built lazily on the first planned query), and
over the whole mix. For telling a transient peak from a resident one, which the suite's
`docker stats` (about one reading a second) can't.

    python benches/probes/serve-memory.py --server target/release/nrese-server.exe \
        --harness target/release/nrese-bench-harness.exe --regime owl2-rl \
        --queries benches/reasoning/queries/lubm --store tmp/probe-store \
        tmp/data/univ-bench.nt tmp/data/lubm-100.nt [--cache on|off] [--keep-store]
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import threading
import time
import urllib.request
from pathlib import Path

import psutil


class Sampler(threading.Thread):
    def __init__(self, pid: int):
        super().__init__(daemon=True)
        self.process = psutil.Process(pid)
        self.samples: list[tuple[float, int]] = []
        self.stopped = threading.Event()

    def run(self):
        while not self.stopped.is_set():
            try:
                self.samples.append((time.monotonic(), self.process.memory_info().rss))
            except psutil.Error:
                return
            time.sleep(0.1)

    def peak(self, since: float = 0.0, until: float = float("inf")) -> float:
        values = [rss for t, rss in self.samples if since <= t <= until]
        return max(values, default=0) / 2**20


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--server", required=True)
    p.add_argument("--harness", required=True)
    p.add_argument("--regime", default="disabled")
    p.add_argument("--queries", required=True)
    p.add_argument("--store", required=True)
    p.add_argument("--cache", choices=["on", "off"], default="on")
    p.add_argument("--port", type=int, default=18960)
    p.add_argument("--keep-store", action="store_true", help="reuse a loaded store")
    p.add_argument("data", nargs="*")
    args = p.parse_args()
    store = Path(args.store)
    env = dict(os.environ, NRESE_STORE_MODE="on-disk", NRESE_DATA_DIR=str(store), RUST_LOG="warn",
               NRESE_REASONING_MODE=args.regime)
    if not (args.keep_store and store.exists()):
        shutil.rmtree(store, ignore_errors=True)
        started = time.monotonic()
        subprocess.run([args.server, "load", *args.data], env=env, check=True, capture_output=True)
        print(f"load {time.monotonic() - started:.1f} s")
    serve_env = dict(env, NRESE_BIND_ADDR=f"127.0.0.1:{args.port}", NRESE_READ_REQUESTS_PER_WINDOW="100000000")
    if args.cache == "off":
        serve_env["NRESE_QUERY_CACHE_BYTES"] = "0"
    server = subprocess.Popen([args.server], env=serve_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    sampler = Sampler(server.pid)
    sampler.start()
    base = f"http://127.0.0.1:{args.port}"
    try:
        for _ in range(600):
            try:
                urllib.request.urlopen(f"{base}/readyz", timeout=2)
                break
            except Exception:
                time.sleep(0.1)
        ready = time.monotonic()
        time.sleep(2)
        idle = time.monotonic()
        mix = [args.harness, "query-mix", "--endpoint", f"{base}/dataset/query", "--queries", args.queries,
               "--label", "probe", "--warmup", "1", "--runs", "3", "--timeout-s", "300",
               "--order", "shuffled", "--seed", "1001"]
        subprocess.run(mix, check=False, capture_output=True)
        done = time.monotonic()
        time.sleep(1)
    finally:
        server.terminate()
        server.wait(timeout=30)
        sampler.stopped.set()
    print(f"idle after start  {sampler.peak(ready, idle):8.0f} MiB")
    print(f"over the mix      {sampler.peak(idle, done):8.0f} MiB")
    print(f"after the mix     {sampler.samples[-1][1] / 2**20 if sampler.samples else 0:8.0f} MiB (last sample)")
    print(f"peak (cache {args.cache})  {sampler.peak():8.0f} MiB  ({len(sampler.samples)} samples)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
