#!/usr/bin/env python3
"""HTTP soak (G7): a server under a mixed load for a while, watched for errors, growth and
lost writes. Starts a release server on a fresh on-disk store (optionally loaded first),
then WORKERS threads send queries, updates, Graph Store requests, transactions (sessions),
EXPLAINs and listings for MINUTES. Every second it samples the server's resident memory,
handles (or file descriptors) and threads; every window it prints requests, errors and
latency percentiles by kind. At the end:

- **errors**: answers other than the expected ones (any 5xx, a refused valid request);
- **growth**: resident memory's slope over the run after its first fifth, in MiB per hour,
  and handles and threads at the end against after the warm-up;
- **writes**: what the workers committed to the soak graph (a ledger of inserted and
  deleted triples) against what the server holds there, exactly.

    python benches/probes/soak.py --server target/release/nrese-server[.exe] \
        [--minutes 30] [--workers 8] [--store tmp/soak-store] [--port 18970] \
        [--out tmp/soak] [--max-growth-mib-per-hour 50] [--soak-triples 20000] [DATA.nt ...]

Writes `samples.csv` (per second) and `windows.csv` (per window) into --out. Exits 1 on
errors, lost writes, or growth past the limit. Needs psutil; works on Linux and Windows.
"""
from __future__ import annotations

import argparse
import csv
import json
import os
import random
import shutil
import statistics
import subprocess
import threading
import time
import http.client
import urllib.parse
import urllib.request
from pathlib import Path

import psutil

SOAK = "urn:soak:g"
REPO = "nrese"

QUERIES = [
    "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
    "SELECT ?p (COUNT(*) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p ORDER BY DESC(?n) LIMIT 10",
    "SELECT * WHERE { ?s ?p ?o } LIMIT 100",
    "SELECT * WHERE { ?s a ?c . OPTIONAL { ?s ?p ?o } } LIMIT 50",
    "ASK { ?s ?p ?o }",
    "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o } LIMIT 200",
    f"SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{SOAK}> {{ ?s ?p ?o }} }}",
    "SELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 20",
    "SELECT ?s WHERE { ?s ?p ?o FILTER(isIRI(?o)) } ORDER BY ?s LIMIT 30",
]


class Ledger:
    """The soak graph's triples as the workers committed them."""

    def __init__(self, pool: int):
        self.lock = threading.Lock()
        self.pool = pool
        self.reserved: set[str] = set()
        self.live: set[str] = set()
        # Triples a failed request may or may not have changed.
        self.ambiguous: set[str] = set()

    def fresh(self, n: int, worker: int) -> list[str]:
        """`n` triples not in the graph, from a bounded pool (so that the dictionary of
        terms stops growing and resident memory can level off), not handed out twice."""
        with self.lock:
            out = []
            for _ in range(n * 4):
                i = random.randrange(self.pool)
                triple = f"<urn:soak:s{i}> <urn:soak:p> {i} ."
                if triple not in self.live and triple not in self.reserved:
                    self.reserved.add(triple)
                    out.append(triple)
                    if len(out) == n:
                        break
            return out

    def settle(self, triples: list[str], committed: bool):
        with self.lock:
            self.reserved.difference_update(triples)
            if committed:
                self.live.update(triples)

    def take(self, n: int) -> list[str]:
        with self.lock:
            return random.sample(sorted(self.live), min(n, len(self.live))) if self.live else []


class Stats:
    def __init__(self):
        self.lock = threading.Lock()
        self.latency: dict[str, list[float]] = {}
        self.errors: list[str] = []
        self.error_count = 0
        self.requests = 0

    def record(self, kind: str, seconds: float, error: str | None):
        with self.lock:
            self.requests += 1
            self.latency.setdefault(kind, []).append(seconds)
            if error:
                self.error_count += 1
                if len(self.errors) < 2000:
                    self.errors.append(f"{kind}: {error}")

    def drain(self):
        with self.lock:
            latency, self.latency = self.latency, {}
            requests, self.requests = self.requests, 0
        return requests, latency


class Connection:
    """One kept-alive connection to the server (as real clients use), reopened after a
    failure."""

    def __init__(self, base: str, timeout: float = 60):
        url = urllib.parse.urlsplit(base)
        self.host, self.port, self.timeout = url.hostname, url.port, timeout
        self.conn: http.client.HTTPConnection | None = None

    def __call__(self, method: str, path: str, body: bytes | None = None, ctype: str | None = None,
                 accept: str | None = None) -> tuple[int, bytes]:
        if self.conn is None:
            self.conn = http.client.HTTPConnection(self.host, self.port, timeout=self.timeout)
        headers = {}
        if ctype:
            headers["Content-Type"] = ctype
        if accept:
            headers["Accept"] = accept
        try:
            self.conn.request(method, path, body=body, headers=headers)
            response = self.conn.getresponse()
            data = response.read()
        except Exception:
            self.conn.close()
            self.conn = None
            raise
        if (response.getheader("connection") or "").lower() == "close":
            self.conn.close()
            self.conn = None
        return response.status, data


def worker(index: int, base: str, stop: threading.Event, ledger: Ledger, stats: Stats, seed: int, cap: int):
    rng = random.Random(seed + index)
    call = Connection(base)
    kinds = ["query"] * 6 + ["insert"] * 2 + ["delete", "gsp", "session", "explain", "listing"]
    while not stop.is_set():
        kind = rng.choice(kinds)
        # The soak graph stays about `cap` triples: past it, inserts become deletes.
        if kind == "insert" and len(ledger.live) >= cap:
            kind = "delete"
        started = time.monotonic()
        error = None
        step = kind
        pending: list[str] = []
        try:
            if kind == "query":
                q = urllib.parse.quote(rng.choice(QUERIES))
                status, body = call("GET", f"/dataset/query?query={q}", accept="*/*")
                error = None if status == 200 else f"{status} {body[:200]!r}"
            elif kind == "insert":
                triples = ledger.fresh(rng.randint(1, 20), index)
                pending = triples
                update = f"INSERT DATA {{ GRAPH <{SOAK}> {{ {' '.join(triples)} }} }}"
                status, body = call("POST", "/dataset/update", update.encode(), "application/sparql-update")
                ledger.settle(triples, status in (200, 204))
                if status not in (200, 204):
                    error = f"{status} {body[:200]!r}"
            elif kind == "delete":
                triples = ledger.take(rng.randint(1, 10))
                if triples:
                    with ledger.lock:
                        # Claimed before the request: no other worker deletes them too, and
                        # none inserts them again until the delete has settled (else an
                        # insert the server orders first would be deleted, while the
                        # ledger, settling it last, would count it live).
                        claimed = [t for t in triples if t in ledger.live]
                        ledger.live.difference_update(claimed)
                        ledger.reserved.update(claimed)
                    if claimed:
                        pending = claimed
                        update = f"DELETE DATA {{ GRAPH <{SOAK}> {{ {' '.join(claimed)} }} }}"
                        status, body = call("POST", "/dataset/update", update.encode(),
                                            "application/sparql-update")
                        with ledger.lock:
                            ledger.reserved.difference_update(claimed)
                            if status not in (200, 204):
                                ledger.live.update(claimed)
                        if status not in (200, 204):
                            error = f"{status} {body[:200]!r}"
            elif kind == "gsp":
                graph = urllib.parse.quote(f"urn:soak:gsp:{rng.randint(0, 9)}")
                op = rng.choice(["PUT", "GET", "DELETE", "POST"])
                if op in ("PUT", "POST"):
                    data = "\n".join(f'<urn:soak:x{rng.randint(0, 99)}> <urn:soak:q> "{i}" .'
                                     for i in range(rng.randint(1, 30)))
                    status, body = call(op, f"/dataset/data?graph={graph}", data.encode(),
                                        "application/n-triples")
                    error = None if status in (200, 201, 204) else f"{op} {status} {body[:200]!r}"
                else:
                    status, _ = call(op, f"/dataset/data?graph={graph}", accept="application/n-triples")
                    error = None if status in (200, 204, 404) else f"{op} {status}"
            elif kind == "session":
                step = "begin"
                status, body = call("POST", f"/api/v1/repositories/{REPO}/sessions", b"{}", "application/json")
                if status not in (200, 201):
                    error = f"begin {status} {body[:200]!r}"
                else:
                    session = json.loads(body).get("session") or json.loads(body).get("id")
                    triples = ledger.fresh(rng.randint(1, 5), index)
                    update = f"INSERT DATA {{ GRAPH <{SOAK}> {{ {' '.join(triples)} }} }}"
                    step = "session update"
                    status, body = call("POST", f"/api/v1/repositories/{REPO}/sessions/{session}/update",
                                        update.encode(), "application/sparql-update")
                    if status not in (200, 204):
                        ledger.settle(triples, False)
                        error = f"session update {status} {body[:200]!r}"
                    elif rng.random() < 0.7:
                        step = "commit"
                        pending = triples
                        status, body = call("POST", f"/api/v1/repositories/{REPO}/sessions/{session}/commit")
                        ledger.settle(triples, status in (200, 204))
                        if status not in (200, 204, 409):  # a conflicting commit is refused, as it should be
                            error = f"commit {status} {body[:200]!r}"
                    else:
                        ledger.settle(triples, False)
                        step = "rollback"
                        status, _ = call("DELETE", f"/api/v1/repositories/{REPO}/sessions/{session}")
                        error = None if status in (200, 204) else f"rollback {status}"
            elif kind == "explain":
                q = urllib.parse.quote(rng.choice(QUERIES))
                status, body = call("GET", f"/dataset/query?query={q}&explain=plan", accept="*/*")
                error = None if status == 200 else f"{status} {body[:200]!r}"
            else:
                path = rng.choice([f"/api/v1/repositories/{REPO}/graphs", f"/api/v1/repositories/{REPO}/namespaces",
                                   "/readyz", "/api/v1/health", f"/api/v1/repositories/{REPO}/queries"])
                status, _ = call("GET", path)
                error = None if status == 200 else f"{path} {status}"
        except Exception as exc:  # a refused connection, a timeout
            error = f"{step} {type(exc).__name__}: {exc}"
            with ledger.lock:
                ledger.ambiguous.update(pending)
                ledger.reserved.difference_update(pending)
        stats.record(kind, time.monotonic() - started, error)


def slope_mib_per_hour(samples: list[tuple[float, int]]) -> float:
    if len(samples) < 3:
        return 0.0
    xs = [t for t, _ in samples]
    ys = [rss / 2**20 for _, rss in samples]
    mx, my = statistics.fmean(xs), statistics.fmean(ys)
    var = sum((x - mx) ** 2 for x in xs)
    return 0.0 if var == 0 else sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / var * 3600


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--server", required=True)
    p.add_argument("--minutes", type=float, default=30)
    p.add_argument("--workers", type=int, default=8)
    p.add_argument("--store", default="tmp/soak-store")
    p.add_argument("--port", type=int, default=18970)
    p.add_argument("--out", default="tmp/soak")
    p.add_argument("--window", type=float, default=60, help="seconds per printed window")
    p.add_argument("--max-growth-mib-per-hour", type=float, default=50)
    p.add_argument("--seed", type=int, default=20261005)
    p.add_argument("--soak-triples", type=int, default=20000,
                   help="the soak graph's size the workers keep it near")
    p.add_argument("data", nargs="*")
    args = p.parse_args()
    store, out = Path(args.store).resolve(), Path(args.out)
    server_bin = str(Path(args.server).resolve())
    shutil.rmtree(store, ignore_errors=True)
    out.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, NRESE_STORE_MODE="on-disk", NRESE_DATA_DIR=str(store), RUST_LOG="warn",
               NRESE_BIND_ADDR=f"127.0.0.1:{args.port}", NRESE_READ_REQUESTS_PER_WINDOW="1000000000",
               NRESE_WRITE_REQUESTS_PER_WINDOW="1000000000")
    if args.data:
        subprocess.run([server_bin, "load", *args.data], env=env, check=True, capture_output=True)
    log = open(out / "server.log", "wb")
    server = subprocess.Popen([server_bin], env=env, stdout=log, stderr=subprocess.STDOUT)
    process = psutil.Process(server.pid)
    base = f"http://127.0.0.1:{args.port}"
    stop, ledger, stats = threading.Event(), Ledger(5 * args.soak_triples), Stats()
    samples: list[tuple[float, int, int, int]] = []
    windows = []
    try:
        for _ in range(600):
            try:
                urllib.request.urlopen(f"{base}/readyz", timeout=2)
                break
            except Exception:
                time.sleep(0.1)
        else:
            print("the server didn't get ready")
            return 1
        threads = [threading.Thread(target=worker, args=(i, base, stop, ledger, stats, args.seed, args.soak_triples), daemon=True)
                   for i in range(args.workers)]
        start = time.monotonic()
        for t in threads:
            t.start()
        end = start + args.minutes * 60
        next_window = start + args.window
        while time.monotonic() < end:
            time.sleep(1)
            now = time.monotonic() - start
            handles = process.num_handles() if hasattr(process, "num_handles") else process.num_fds()
            samples.append((now, process.memory_info().rss, handles, process.num_threads()))
            if time.monotonic() >= next_window or time.monotonic() >= end:
                next_window += args.window
                requests, latency = stats.drain()
                row = {"t_s": round(now), "requests": requests, "errors": stats.error_count,
                       "rss_mib": round(samples[-1][1] / 2**20), "handles": handles, "threads": samples[-1][3]}
                for kind, values in sorted(latency.items()):
                    values.sort()
                    row[f"{kind}_p50_ms"] = round(values[len(values) // 2] * 1000, 1)
                    row[f"{kind}_p99_ms"] = round(values[min(len(values) - 1, int(len(values) * 0.99))] * 1000, 1)
                windows.append(row)
                print(json.dumps(row), flush=True)
        stop.set()
        for t in threads:
            t.join(timeout=120)
        # The soak graph holds exactly what was committed.
        q = urllib.parse.quote(f"SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{SOAK}> {{ ?s ?p ?o }} }}")
        call = Connection(base)
        status, body = call("GET", f"/dataset/query?query={q}", accept="application/sparql-results+json")
        held = int(json.loads(body)["results"]["bindings"][0]["n"]["value"]) if status == 200 else -1
    finally:
        stop.set()
        server.terminate()
        try:
            server.wait(timeout=60)
        except subprocess.TimeoutExpired:
            server.kill()
    with open(out / "samples.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["t_s", "rss_bytes", "handles", "threads"])
        w.writerows(samples)
    with open(out / "windows.csv", "w", newline="") as f:
        keys = sorted({k for row in windows for k in row})
        w = csv.DictWriter(f, fieldnames=keys)
        w.writeheader()
        w.writerows(windows)
    after_warmup = [(t, rss) for t, rss, _, _ in samples if t >= samples[-1][0] / 5] if samples else []
    growth = slope_mib_per_hour(after_warmup)
    warm = next((s for s in samples if s[0] >= samples[-1][0] / 5), samples[0]) if samples else None
    expected = len(ledger.live)
    ambiguous = len(ledger.ambiguous - ledger.live)
    print(f"errors: {stats.error_count}")
    kinds: dict[str, int] = {}
    for e in stats.errors:
        key = e.split(" b'")[0][:120]
        kinds[key] = kinds.get(key, 0) + 1
    for key, n in sorted(kinds.items(), key=lambda kv: -kv[1])[:15]:
        print(f"  {n:5d}  {key}")
    print(f"soak graph: {held} triples held, {expected} committed"
          + (f", {ambiguous} after failed requests either way" if ambiguous else ""))
    if samples and warm:
        print(f"resident memory: {warm[1] / 2**20:.0f} MiB after the warm-up, {samples[-1][1] / 2**20:.0f} MiB at the end, "
              f"slope {growth:+.1f} MiB/h; handles {warm[2]} -> {samples[-1][2]}, threads {warm[3]} -> {samples[-1][3]}")
    shutil.rmtree(store, ignore_errors=True)
    lost = not expected <= held <= expected + ambiguous
    failed = stats.error_count > 0 or lost or growth > args.max_growth_mib_per_hour
    print("FAILED" if failed else "passed")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
