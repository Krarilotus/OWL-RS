#!/usr/bin/env python3
"""Incremental maintenance under reasoning, over the SPARQL protocol: what GraphDB and RDFox
document as their strength, on NRESE and on them, each through its server's SPARQL Update.

    maintenance.py prepare --tier lubm-10                  the change sets (seeded), from the frozen data
    maintenance.py run --systems nrese,graphdb --tier lubm-10 --runs 3 --results DIR
        [--sizes 1,100,10000] [--readers 0,1,8,64] [--duration-s 60]

Per system and repetition, from a fresh store (the suite's adapters load and serve it:
NRESE from NRESE_BIN, GraphDB with its licence, RDFox once licensed):
1. **load** the base (univ-bench and LUBM(N)) under OWL 2 RL, and take the closure's
   fingerprint: its size and an order-independent hash of every triple it answers with;
2. **batches**: for each size, commits that add a new university's triples and remove them
   again (`insert-new-K`, `delete-new-K`), and that remove existing triples and put them back
   (`delete-base-K`, `reinsert-base-K`); one commit is one SPARQL Update request, timed from
   the request to the response;
3. **schema**: one subclass axiom removed and restored (`tbox-delete`, `tbox-insert`);
4. **readers**: for each reader count R, R clients run LUBM's queries (the harness's
   `query-mix --clients R`) while one writer commits single-triple changes back to back:
   commit latency under R readers (`commit@readers-R`) and the readers' latency and
   throughput (`read@readers-R`);
5. **check**: every change was undone, so the closure must have the base's fingerprint
   again. Otherwise the run is `wrong`, and its times don't count.

Rows follow the suite's schema (benches/suite/suitekit/schema.py): workload
`commit-latency`, tier = the base, regime `owl2-rl`; `update` rows per commit (`repeat` =
the commit's number within its item), `query` rows for the readers, a `count` row for the
check. Restricted systems' rows carry publish = permission and stay on the machine.

Settings: BENCH_DATA (the frozen datasets: univ-bench.nt, lubm-N.nt), NRESE_BIN (+ NRESE_IMAGE,
default ubuntu:24.04), HARNESS (the harness binary, for the readers), NRESE_LICENSES,
DOCKER_MEMORY (default 28g; a repetition starts only when that much memory is available
and the host's owners don't need it: benches/suite/suitekit/quiet.py; timed, so only under
scripts/quiet-slot.sh), DOCKER_CPUS, MAINTENANCE_DATA (the change sets; default
~/nrese-reasoning-suite/data/maintenance).
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import os
import platform
import random
import subprocess
import sys
import threading
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(ROOT / "benches/suite"))
sys.path.insert(0, str(HERE.parent))

from suitekit import quiet  # noqa: E402
from suitekit.adapters import Context, Nrese, adapter, images_for  # noqa: E402
from suitekit.manifest import Manifest  # noqa: E402
from suitekit.runtime import DockerRuntime, Mount, Spec  # noqa: E402
from suitekit.schema import Result, Writer  # noqa: E402

WORKLOAD = "commit-latency"
REGIME = "owl2-rl"
SEED = 20261012
UB = "http://www.lehigh.edu/~zhp2/2004/0401/univ-bench.owl#"
SUBCLASS = "<http://www.w3.org/2000/01/rdf-schema#subClassOf>"
# The schema change: Professor ⊑ Faculty, removed and restored. Every professor's
# Faculty, Employee and Person memberships depend on it (LUBM's queries 1-14 read them).
TBOX_AXIOM = f"<{UB}Professor> {SUBCLASS} <{UB}Faculty> ."


def setting(key: str, default: str = "") -> str:
    return os.environ.get(key) or default


def changes_dir(tier: str) -> Path:
    return Path(setting("MAINTENANCE_DATA", str(Path.home() / "nrese-reasoning-suite/data/maintenance"))) / tier


def bench_data() -> Path:
    if setting("BENCH_DATA"):
        return Path(setting("BENCH_DATA"))
    hosts = tomllib.loads((ROOT / "benches/hosts.toml").read_text(encoding="utf-8"))
    return Path(hosts["assets"]["root"]) / hosts["inputs"]["data"]["path"]


# --- prepare: the change sets ---------------------------------------------------------------------

def prepare(args):
    """New facts: one more university (UBA's University<N>, from the pinned generator); base
    facts to remove: a seeded sample of the base's instance triples."""
    n = int(args.tier.removeprefix("lubm-"))
    out = changes_dir(args.tier)
    out.mkdir(parents=True, exist_ok=True)
    new = out / "new-university.nt"
    if not new.exists():
        gen = out / "gen"
        # UBA writes its log into the working directory.
        subprocess.run(["docker", "run", "--rm", "-u", f"{os.getuid()}:{os.getgid()}", "-v", f"{out}:/out", "-w", "/out",
                        "nrese-bench/lubm-uba", "-u", str(n + 1), "-f", "NTRIPLES", "--consolidate", "Maximal",
                        "-t", "8", "-q", "-o", "/out/gen"], check=True)
        base = set(open_lines(bench_data() / f"lubm-{n}.nt"))
        lines = []
        for part in sorted(gen.iterdir()):
            lines += [line for line in open_lines(part) if not line.startswith("<> ") and line not in base]
        # The new university's own triples: whatever LUBM(N+1) has beyond LUBM(N).
        new.write_text("".join(lines), encoding="utf-8", newline="\n")
        for part in gen.iterdir():
            part.unlink()
        gen.rmdir()
    sample = out / "base-sample.nt"
    if not sample.exists():
        rng = random.Random(SEED)
        lines = list(open_lines(bench_data() / f"lubm-{n}.nt"))
        sample.write_text("".join(rng.sample(lines, min(len(lines), 20 * 10000))), encoding="utf-8", newline="\n")
    ontology = list(open_lines(bench_data() / "univ-bench.nt"))
    if TBOX_AXIOM + "\n" not in ontology:
        raise SystemExit(f"univ-bench.nt doesn't state {TBOX_AXIOM}")
    manifest = {p.name: {"lines": sum(1 for _ in open_lines(p)), "sha256": sha256(p)} for p in (new, sample)}
    (out / "changes.json").write_text(json.dumps(manifest, indent=1) + "\n", encoding="utf-8")
    print(json.dumps(manifest, indent=1))


def open_lines(path: Path):
    with open(path, encoding="utf-8") as f:
        for line in f:
            if line.strip():
                yield line


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


# --- NRESE from a prebuilt binary ---------------------------------------------------------------------

class NreseBinary(Nrese):
    """The suite's NRESE adapter, its server the binary NRESE_BIN in NRESE_IMAGE (the binary's
    C library decides the image), its store a Docker volume as every other system's."""

    def supports(self, ctx, inputs, regime):
        return None if Path(setting("NRESE_BIN")).is_file() else "set NRESE_BIN"

    def images(self, ctx):
        return [setting("NRESE_IMAGE", "ubuntu:24.04")]

    def spec(self, ctx, name, store, command, env):
        env = {"NRESE_STORE_MODE": "on-disk", "NRESE_DATA_DIR": "/store", "RUST_LOG": "info", **env}
        binary = Path(setting("NRESE_BIN"))
        return Spec(name, setting("NRESE_IMAGE", "ubuntu:24.04"), [f"/nrese/{binary.name}", *command], env,
                    ctx.mounts(Mount(str(binary.parent), "/nrese"), Mount(store, "/store", readonly=False)),
                    memory=ctx.memory)

    def version(self, ctx, endpoint):
        return setting("NRESE_VERSION") or super().version(ctx, endpoint)


def system(key: str):
    return NreseBinary() if key == "nrese" else adapter(key)


# --- the SPARQL side ----------------------------------------------------------------------------------

def update(url: str, text: str, timeout: float) -> tuple[float, int, str]:
    """One commit: ms from request to response, HTTP status, the body's start."""
    body = urllib.parse.urlencode({"update": text}).encode()
    request = urllib.request.Request(url, data=body, headers={"Content-Type": "application/x-www-form-urlencoded"})
    started = time.perf_counter()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            response.read()
            status, detail = response.status, ""
    except urllib.error.HTTPError as e:
        status, detail = e.code, e.read()[:300].decode("utf-8", "replace")
    except (urllib.error.URLError, OSError) as e:
        status, detail = 0, str(e)[:300]
    return (time.perf_counter() - started) * 1000, status, detail


def fingerprint(url: str, timeout: float) -> tuple[int, int]:
    """(triples, an order-independent hash of them): every triple the endpoint answers with,
    each line of the TSV result hashed, the hashes summed modulo 2**64."""
    body = urllib.parse.urlencode({"query": "SELECT ?s ?p ?o WHERE { ?s ?p ?o }"}).encode()
    request = urllib.request.Request(url, data=body, headers={"Accept": "text/tab-separated-values"})
    count, total = 0, 0
    with urllib.request.urlopen(request, timeout=timeout) as response:
        response.readline()  # the header
        for line in response:
            line = line.rstrip(b"\r\n")
            if line:
                count += 1
                total = (total + int.from_bytes(hashlib.blake2b(line, digest_size=8).digest(), "little")) % 2**64
    return count, total


def data_block(lines: list[str]) -> str:
    return "".join(lines)


# --- one system, one repetition --------------------------------------------------------------------------

class Run:
    def __init__(self, args, key: str, rep: int, runtime: DockerRuntime, writer: Writer, results: Path):
        self.args, self.key, self.rep, self.runtime, self.writer = args, key, rep, runtime, writer
        self.system = system(key)
        self.n = int(args.tier.removeprefix("lubm-"))
        run_id = f"{args.stamp}-{key}-{args.tier}-r{rep}"
        self.work = Path(setting("MAINTENANCE_SCRATCH", str(Path.home() / "nrese-reasoning-suite/scratch"))) / run_id
        self.work.mkdir(parents=True, exist_ok=True)
        self.logs = results / "logs" / run_id
        self.logs.mkdir(parents=True, exist_ok=True)
        settings = dict(os.environ)
        settings.setdefault("DOCKER_MEMORY", "28g")
        self.ctx = Context(runtime, ROOT, self.work, self.logs, Mount(str(bench_data()), "/data"), [],
                           args.timeout_s, settings, run_id)
        self.publish = args.publish.get(key, "free")
        self.version = "-"

    def row(self, task: str, **fields) -> Result:
        return Result(date=self.args.date, host=self.args.host, runtime="docker", system=self.key,
                      version=self.version, publish=self.publish, workload=WORKLOAD, tier=self.args.tier,
                      regime=REGIME, run=self.rep, task=task, **fields)

    def go(self):
        inputs = ["/data/univ-bench.nt", f"/data/lubm-{self.n}.nt"]
        why = self.system.supports(self.ctx, inputs, REGIME)
        if why:
            self.writer.write(self.row("load", status="skipped", note=why))
            return
        store = self.runtime.new_store(self.ctx.name("store"))
        try:
            step = self.system.load(self.ctx, store, inputs, REGIME)
            measured = step.measured
            self.writer.write(self.row("load", status="ok" if measured.ok else "timeout" if measured.timed_out else "failed",
                                       ms=measured.ms, peak_mib=measured.peak_mib or "", note=step.note))
            if not measured.ok:
                return
            endpoint = self.system.endpoint if self.system.in_memory(REGIME) else self.system.serve(self.ctx, store, REGIME)
            self.version = self.system.version(self.ctx, endpoint)
            try:
                self.maintain(endpoint)
            finally:
                peak = self.system.stop(self.ctx)
                self.writer.write(self.row("serve", peak_mib=peak or "", note="the server's peak over the maintenance run"))
        finally:
            self.runtime.remove_store(store)

    def maintain(self, endpoint):
        timeout = self.args.timeout_s
        base = fingerprint(endpoint.query, timeout)
        self.writer.write(self.row("count", rows=base[0], note="the closure after the load: the base fingerprint"))
        changes = changes_dir(self.args.tier)
        new = list(open_lines(changes / "new-university.nt"))
        sample = list(open_lines(changes / "base-sample.nt"))
        failures = 0
        offset_new, offset_base = 0, 0
        for size in self.args.sizes:
            reps = self.args.batch_reps if size < 10000 else max(1, self.args.batch_reps // 2)
            for i in range(1, reps + 1):
                block_new = new[offset_new:offset_new + size]
                block_base = sample[offset_base:offset_base + size]
                offset_new, offset_base = offset_new + size, offset_base + size
                for item, verb, block in ((f"insert-new-{size}", "INSERT", block_new),
                                          (f"delete-new-{size}", "DELETE", block_new),
                                          (f"delete-base-{size}", "DELETE", block_base),
                                          (f"reinsert-base-{size}", "INSERT", block_base)):
                    failures += self.commit(endpoint, item, i, f"{verb} DATA {{\n{data_block(block)}}}", size)
        for i in range(1, self.args.batch_reps + 1):
            failures += self.commit(endpoint, "tbox-delete", i, f"DELETE DATA {{ {TBOX_AXIOM} }}", 1)
            failures += self.commit(endpoint, "tbox-insert", i, f"INSERT DATA {{ {TBOX_AXIOM} }}", 1)
        for readers in self.args.readers:
            failures += self.under_readers(endpoint, readers, new)
        after = fingerprint(endpoint.query, timeout)
        same = after == base
        self.writer.write(self.row("count", status="ok" if same and not failures else "wrong", rows=after[0],
                                   note=("the closure after every change was undone equals the base"
                                         if same else f"the closure differs from the base: {after[0]} triples "
                                                      f"against {base[0]}, hash {'equal' if after[1] == base[1] else 'differs'}")
                                        + (f"; {failures} commits failed" if failures else "")))

    def commit(self, endpoint, item: str, repeat: int, text: str, size: int) -> int:
        ms, status, detail = update(endpoint.update, text, self.args.timeout_s)
        ok = 200 <= status < 300
        self.writer.write(self.row("update", item=item, repeat=repeat, status="ok" if ok else "failed", ms=ms,
                                   rows=size, note="" if ok else f"HTTP {status}: {detail}"))
        return 0 if ok else 1

    def under_readers(self, endpoint, readers: int, new: list[str]) -> int:
        """Single-triple commits back to back for the duration, while `readers` clients run
        LUBM's queries; each commit is undone by the next (insert, then delete)."""
        stop = threading.Event()
        harness = None
        if readers:
            report = self.logs / f"readers-{readers}.json"
            harness = subprocess.Popen(
                [setting("HARNESS"), "query-mix", "--endpoint", endpoint.query,
                 "--queries", str(ROOT / "benches/reasoning/queries/lubm"), "--label", self.key,
                 "--warmup", "0", "--runs", "1", "--clients", str(readers), "--duration-s", str(self.args.duration_s),
                 "--interactive-ms", "1000", "--timeout-s", str(self.args.timeout_s), "--report-json", str(report)],
                stdout=open(self.logs / f"readers-{readers}.log", "w"), stderr=subprocess.STDOUT)
        times, failures, i = [], 0, 0
        deadline = time.monotonic() + self.args.duration_s
        item = f"commit@readers-{readers}"
        while time.monotonic() < deadline or (harness and harness.poll() is None and time.monotonic() < deadline + 120):
            triple = new[i % len(new)]
            for verb in ("INSERT", "DELETE"):
                ms, status, detail = update(endpoint.update, f"{verb} DATA {{ {triple.strip()} }}", self.args.timeout_s)
                if 200 <= status < 300:
                    times.append(ms)
                else:
                    failures += 1
            i += 1
            if harness is None and time.monotonic() >= deadline:
                break
        if harness:
            harness.wait()
        times.sort()
        for repeat, ms in enumerate(times, 1):
            self.writer.write(self.row("update", item=item, repeat=repeat, ms=ms, rows=1))
        if failures:
            self.writer.write(self.row("update", item=item, repeat=len(times) + 1, status="failed",
                                       note=f"{failures} commits failed"))
        if harness:
            self.reader_rows(readers, self.logs / f"readers-{readers}.json")
        return failures

    def reader_rows(self, readers: int, report: Path):
        """One `query` row per LUBM query under `readers` clients: its p50 as the time, p99 and
        the clients' throughput in the note (the harness's throughput phase)."""
        try:
            throughput = json.loads(report.read_text(encoding="utf-8"))["throughput"]
            summary = (f"{readers} readers: {throughput['queries_per_s']:.1f} queries/s over "
                       f"{throughput['duration_s']:.0f} s, {throughput['completed']} done, {throughput['errors']} errors")
            status = "ok" if not throughput["errors"] else "failed"
            for query in throughput["per_query"]:
                self.writer.write(self.row("query", item=f"read@readers-{readers}:{query['id']}", repeat=1,
                                           status=status, ms=query["p50_ms"] if query["p50_ms"] is not None else "",
                                           note=f"p99 {query['p99_ms']} ms, {query['completed']} runs; {summary}"))
        except (OSError, ValueError, KeyError, TypeError) as e:
            self.writer.write(self.row("query", item=f"read@readers-{readers}", repeat=1, status="failed",
                                       note=f"no reader report: {e}"))


def cmd_run(args):
    results = Path(args.results)
    results.mkdir(parents=True, exist_ok=True)
    args.date = datetime.datetime.now().isoformat(timespec="minutes")
    args.stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    args.host = platform.node()
    args.sizes = [int(s) for s in args.sizes.split(",")]
    args.readers = [int(r) for r in args.readers.split(",")]
    systems = args.systems.split(",")
    registry = tomllib.loads((ROOT / "benches/suite/systems.toml").read_text(encoding="utf-8"))
    args.publish = {k: registry.get(k, {}).get("publish", "free") for k in systems}
    if not changes_dir(args.tier).joinpath("changes.json").exists():
        raise SystemExit(f"no change sets for {args.tier}: run `maintenance.py prepare --tier {args.tier}`")
    if any(r > 0 for r in args.readers) and not Path(setting("HARNESS")).is_file():
        raise SystemExit("set HARNESS to the nrese-bench-harness binary (the readers)")
    protocol = argparse.Namespace(runs=args.runs, query_runs=0, cache="-", order="rotated", seed=SEED,
                                  timeout_s=args.timeout_s, query_timeout_s=args.timeout_s)
    manifest = Manifest(results, ROOT, sys.argv[1:], protocol, "docker", dict(os.environ))
    writer = CountingWriter(results / "results.csv", echo=print)
    runtime = DockerRuntime(Path(setting("MAINTENANCE_SCRATCH", str(Path.home() / "nrese-reasoning-suite/scratch"))),
                            args.port)
    runtime.pin([i for key in systems for i in images_for(system(key), ROOT)])
    code = 0
    try:
        for rep in range(1, args.runs + 1):
            k = (rep - 1) % len(systems)
            for key in systems[k:] + systems[:k]:
                quiet.wait_memory(quiet.gib(setting("DOCKER_MEMORY", "28g")))
                print(f"\n{key} / {args.tier} / run {rep}")
                with quiet.step(f"maintenance {key} {args.tier} r{rep}", timed=True):
                    Run(args, key, rep, runtime, writer, results).go()
    except KeyboardInterrupt:
        code = 130
        raise
    finally:
        runtime.cleanup()
        manifest.finish(code)
    print(f"results: {results / 'results.csv'}")
    if writer.bad:
        print(f"{writer.bad} rows wrong, failed or timed out")
        return 1
    return 0


class CountingWriter(Writer):
    """The suite's writer, counting the rows that went wrong: a queue job stops after one."""

    def __init__(self, path: Path, echo=None):
        super().__init__(path, echo)
        self.bad = 0

    def write(self, result: Result):
        super().write(result)
        if result.status in ("wrong", "failed", "timeout"):
            self.bad += 1


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    pp = sub.add_parser("prepare")
    pp.add_argument("--tier", default="lubm-10")
    pr = sub.add_parser("run")
    pr.add_argument("--systems", default="nrese,graphdb")
    pr.add_argument("--tier", default="lubm-10")
    pr.add_argument("--runs", type=int, default=3)
    pr.add_argument("--sizes", default="1,100,10000")
    pr.add_argument("--batch-reps", type=int, default=6, help="commits of each kind and size (10,000: half as many)")
    pr.add_argument("--readers", default="0,1,8,64")
    pr.add_argument("--duration-s", type=int, default=60)
    pr.add_argument("--timeout-s", type=int, default=3600)
    pr.add_argument("--port", type=int, default=19352)
    pr.add_argument("--results", required=True)
    args = p.parse_args(argv)
    sys.exit({"prepare": prepare, "run": cmd_run}[args.command](args) or 0)


if __name__ == "__main__":
    main()
