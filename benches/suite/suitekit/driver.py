"""`suite.py run`: walks the matrix of workloads and systems, runs every pair that can run,
and writes one result file (schema.py).

For each workload tier, system and repetition, from a fresh store: load (reasoning where
the system reasons at load), store size, restart, a count of every statement (where lazy
reasoners do their work), the queries (one warm-up, then the measured repetitions, through
the harness's `query-mix`), the server's peak memory. After every repetition its
containers, store and scratch go; after the whole run, what the run itself made (datasets,
the NRESE build volume, images it pulled or built) goes too, unless kept for a batch
(--keep, or NRESE_BENCH_KEEP=1; installed tools always stay).
"""
from __future__ import annotations

import argparse
import atexit
import datetime
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path

from .adapters import ADAPTERS, Context, Endpoint, Nrese, adapter, count_all
from .runtime import (PREFIX, ApptainerRuntime, DockerRuntime, DryRuntime, Mount, ProcessRuntime,
                      run_quiet, shell)
from .schema import Result, Writer
from .workloads import plans

EXE = ".exe" if os.name == "nt" else ""
# Systems a default run leaves out: AnzoGraph needs registration beyond 8 GB and takes
# minutes to start; name it to run it.
EXPLICIT_ONLY = {"anzograph"}


def arguments(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="suite.py run", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--systems", help="comma-separated systems.toml keys (default: every system with an adapter)")
    p.add_argument("--workloads", help="comma-separated workloads.toml keys (default: all)")
    p.add_argument("--tier", action="append", default=[], metavar="WORKLOAD=TIER",
                   help="a tier to run (repeatable), e.g. lubm=10, owl2bench=ql-1, basics-mix=yago-tiny")
    p.add_argument("--runs", type=int, default=3, help="repetitions, each from a fresh store (default 3)")
    p.add_argument("--query-runs", type=int, default=3, help="measured runs per query after one warm-up (default 3)")
    p.add_argument("--timeout-s", type=int, default=3600, help="per load (default 3600)")
    p.add_argument("--query-timeout-s", type=int, default=300, help="per query run (default 300)")
    p.add_argument("--runtime", choices=["docker", "apptainer", "process"], default="docker")
    p.add_argument("--sif-dir", help="Apptainer: the directory of the SIF images")
    p.add_argument("--data", help="the datasets: a directory, or volume:NAME with Docker (default volume:nrese-bench-data)")
    p.add_argument("--results", help="the result directory (default benches/suite/results/<date>)")
    p.add_argument("--port", type=int, default=18950, help="the port the systems are reached on")
    p.add_argument("--dry-run", action="store_true", help="print every step instead of running it")
    p.add_argument("--keep", action="store_true",
                   help="keep datasets, the NRESE build and images for the next run of a batch")
    p.add_argument("--skip-build", action="store_true", help="use the existing NRESE and harness builds")
    return p.parse_args(argv)


def bash() -> str:
    """The bash the repository's scripts run in. On Windows that is Git's: a bare `bash`
    starts System32's, which is WSL's (Windows searches System32 before PATH), and WSL has
    neither the toolchain nor these paths."""
    if os.name != "nt":
        return "bash"
    if os.environ.get("NRESE_BASH"):
        return os.environ["NRESE_BASH"]
    found = shutil.which("bash")  # PATH only
    if found and "system32" not in found.lower():
        return found
    git = shutil.which("git")
    if git:
        for parent in Path(git).parents:
            if (parent / "usr/bin/bash.exe").exists():
                return str(parent / "usr/bin/bash.exe")
    sys.exit("no Git Bash found: set NRESE_BASH to its bash.exe")


def verdict(system: dict, workload: dict) -> str | None:
    have = set(system.get("capabilities", []))
    missing = [c for c in workload.get("needs", []) if c not in have]
    alternatives = workload.get("any", [])
    if alternatives and not have & set(alternatives):
        missing.append(" or ".join(alternatives))
    return ", ".join(missing) or None


def slug(text: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "-", text).strip("-").lower()


class Suite:
    def __init__(self, args, systems: dict, workloads: dict, root: Path):
        self.args = args
        self.systems = systems
        self.workloads = workloads
        self.root = root
        self.settings = dict(os.environ)
        now = datetime.datetime.now()
        self.date = now.isoformat(timespec="minutes")
        self.stamp = now.strftime("%Y%m%d-%H%M%S")
        self.host = socket.gethostname()
        scratch_root = Path(self.settings.get("NRESE_BENCH_SCRATCH") or root / "tmp")
        self.scratch = (scratch_root / f"{PREFIX}{self.stamp}").resolve()
        self.results = Path(args.results or root / "benches/suite/results" / now.strftime("%Y-%m-%d")).resolve()
        if args.dry_run:
            # A dry run writes nothing that stays: its rows and logs go with the scratch.
            self.results = self.scratch / "dry-run-results"
        self.keep = args.keep or bool(self.settings.get("NRESE_BENCH_KEEP"))
        self.tools_installed = (root / ".cache/bench-toolbox").exists()
        if args.runtime == "docker":
            runtime = DockerRuntime(self.scratch, args.port)
        elif args.runtime == "apptainer":
            if not args.sif_dir:
                sys.exit("--runtime apptainer needs --sif-dir")
            runtime = ApptainerRuntime(self.scratch, args.port, Path(args.sif_dir).resolve())
        else:
            runtime = ProcessRuntime(self.scratch, args.port)
        self.runtime = DryRuntime(runtime) if args.dry_run else runtime
        data = args.data or ("volume:nrese-bench-data" if args.runtime == "docker" else None)
        if data is None:
            sys.exit(f"--runtime {args.runtime} needs --data DIR (the datasets)")
        if data.startswith("volume:"):
            if args.runtime != "docker":
                sys.exit("a data volume needs --runtime docker")
            self.data_volume = data[len("volume:"):]
            self.data = Mount(self.data_volume, "/data")
        else:
            self.data_volume = None
            self.data = Mount(str(Path(data).resolve()), "/data")
        self.writer = Writer(self.results / "results.csv", echo=print)
        self.made_files: list[str] = []  # datasets this run made, as /data paths
        self.made_volumes: list[str] = []
        self.made_images: list[str] = []
        self.written: list[Result] = []
        self.skips: list[str] = []

    # --- helpers ---
    def say(self, text: str):
        print(text, flush=True)

    def host_command(self, command: list[str], log: Path | None = None, timeout: float | None = None,
                     cwd: Path | None = None) -> int:
        """Runs a command on the host, from the repository root unless `cwd` says otherwise
        (echoed in a dry run)."""
        if command[0] == "bash":
            command = [bash(), *command[1:]]
        if self.args.dry_run:
            self.say(f"    [host] {shell(command)}")
            return 0
        if log is None:
            return subprocess.run(command, cwd=cwd or self.root, timeout=timeout).returncode
        log.parent.mkdir(parents=True, exist_ok=True)
        with open(log, "w", encoding="utf-8", errors="replace") as f:
            return subprocess.run(command, cwd=cwd or self.root, stdout=f, stderr=subprocess.STDOUT,
                                  timeout=timeout).returncode

    def emit(self, base: dict, **fields):
        result = Result(**{**base, **fields})
        self.writer.write(result)
        self.written.append(result)

    def publish(self, key: str) -> str:
        return self.systems.get(key, {}).get("publish", "free")

    # --- selection ---
    def selected_systems(self) -> list[str]:
        if self.args.systems:
            return [s.strip() for s in self.args.systems.split(",") if s.strip()]
        return [k for k in self.systems if k in ADAPTERS and k not in EXPLICIT_ONLY]

    def selected_workloads(self) -> list[str]:
        if self.args.workloads:
            return [w.strip() for w in self.args.workloads.split(",") if w.strip()]
        return list(self.workloads)

    def tiers(self, workload: str) -> list[str] | None:
        chosen = [t.split("=", 1)[1] for t in self.args.tier if t.split("=", 1)[0] == workload]
        return chosen or None

    # --- preparation ---
    def ensure_images(self, needed: dict[str, str | None]):
        """needed: image -> build context under benches/ (None: pulled)."""
        if self.args.runtime != "docker":
            return
        for image, context in needed.items():
            if self.runtime.image_present(image):
                continue
            if context:
                self.say(f"building {image}")
                self.host_command(["docker", "build", "-q", "-t", image, str(self.root / "benches" / context)])
            else:
                self.say(f"pulling {image}")
                self.host_command(["docker", "pull", "-q", image])
            self.made_images.append(image)

    def build_nrese(self, nrese) -> bool:
        """Builds an NRESE server (`nrese` or `nrese-oxigraph`) from its source tree."""
        if self.args.skip_build:
            return True
        ctx = self.context_for_images()
        source = nrese.source(ctx)
        self.say(f"building {nrese.key} (release) from {source}")
        log = self.results / "logs" / f"{nrese.key}-build-{self.stamp}.log"
        if self.args.runtime == "docker":
            for volume in (nrese.target_volume, "nrese-cargo"):
                if run_quiet(["docker", "volume", "inspect", volume]).returncode != 0:
                    self.made_volumes.append(volume)
            # Built in the pinned Rust image, as the scorecards do: the binary runs in it.
            # Half the cores, like scripts/cargo-guarded.sh: the machine stays usable.
            cpus = str(max(1, (os.cpu_count() or 2) // 2))
            rc = self.host_command(["docker", "run", "--rm", "--cpus", cpus, "-v", f"{source.as_posix()}:/src:ro",
                                    "-v", f"{nrese.target_volume}:/target", "-v", "nrese-cargo:/usr/local/cargo/registry",
                                    "-w", "/src", nrese.rust_image(ctx),
                                    "cargo", "build", "--release", "--locked", "-j", cpus, "-p", "nrese-server",
                                    "--target-dir", "/target"], log)
        else:
            # From the source tree: its own toolchain, budget and target directory.
            rc = self.host_command(["bash", "scripts/cargo-guarded.sh", "build", "--release", "--locked",
                                    "-p", "nrese-server"], log, cwd=source)
        if rc != 0:
            self.say(f"the {nrese.key} build failed; see {log}")
        return rc == 0

    def harness(self) -> str:
        target = Path(self.settings.get("CARGO_TARGET_DIR") or self.root / "target")
        return self.settings.get("HARNESS") or str(target / "release" / f"nrese-bench-harness{EXE}")

    def build_harness(self) -> bool:
        if self.args.skip_build and Path(self.harness()).exists():
            return True
        self.say("building the harness (release)")
        log = self.results / "logs" / f"harness-build-{self.stamp}.log"
        rc = self.host_command(["bash", "scripts/cargo-guarded.sh", "build", "--release", "--locked", "--quiet",
                                "--manifest-path", "benches/nrese-bench-harness/Cargo.toml"], log)
        return rc == 0

    def data_missing(self, inputs: list[str]) -> list[str]:
        files = [i for i in inputs if i.startswith("/data/")]
        if not files:
            return []
        if self.data_volume:
            if self.args.dry_run:
                return []
            script = "; ".join(f"test -s {f} || echo {f}" for f in files)
            out = run_quiet(["docker", "run", "--rm", "-v", f"{self.data_volume}:/data:ro", "alpine", "sh", "-c", script])
            return out.stdout.split()
        return [f for f in files if not (Path(self.data.source) / f[len("/data/"):]).is_file()]

    def ensure_data(self, plan) -> str | None:
        # Looking for the inputs mounts the volume, which creates it: whether this run made
        # it (and so removes it) is decided before.
        if (self.data_volume and not self.args.dry_run and self.data_volume not in self.made_volumes
                and run_quiet(["docker", "volume", "inspect", self.data_volume]).returncode != 0):
            self.made_volumes.append(self.data_volume)
        missing = self.data_missing(plan.inputs)
        if not missing:
            return None
        if not (self.data_volume and plan.prepare):
            return f"missing inputs: {', '.join(missing)} (prepare them on a machine with Docker)"
        self.say(f"preparing {plan.workload} {plan.tier}: {shell(plan.prepare)}")
        log = self.results / "logs" / f"prepare-{slug(plan.workload)}-{slug(plan.tier)}-{self.stamp}.log"
        rc = self.host_command(plan.prepare, log)
        self.made_files += missing
        still = self.data_missing(plan.inputs)
        return f"preparing the inputs failed; see {log}" if rc != 0 or still else None

    def sif_missing(self, system, ctx: Context) -> str | None:
        if self.args.runtime != "apptainer":
            return None
        images = system.images(ctx) + [image for image, _ in system.builds()]
        missing = [i for i in images if not self.runtime.image_present(i)]
        if missing:
            return f"no SIF image for {', '.join(missing)} in --sif-dir (benches/cluster/build-sif.sh)"
        return None

    def context_for_images(self) -> Context:
        return Context(self.runtime, self.root, self.scratch, self.results, self.data, [], self.args.timeout_s,
                       self.settings, "images")

    # --- runs ---
    def cycle(self, plan, key: str, system, regime: str, run: int):
        run_id = slug(f"{self.stamp}-{key}-{plan.workload}-{plan.tier}-r{run}")
        work = self.scratch / run_id
        logs = self.results / "logs" / run_id
        logs.mkdir(parents=True, exist_ok=True)
        work.mkdir(parents=True, exist_ok=True)
        ctx = Context(self.runtime, self.root, work, logs, self.data, plan.mounts, self.args.timeout_s,
                      {**self.settings, "QUERY_TIMEOUT_S": str(self.args.query_timeout_s)}, run_id, plan.small)
        base = dict(date=self.date, host=self.host, runtime=self.runtime.kind, system=key, version="-",
                    publish=self.publish(key), workload=plan.workload, tier=plan.tier, regime=regime, run=run)
        self.say(f"\n{key} / {plan.workload} {plan.tier} / {regime} / run {run}")
        store = None
        endpoint = None
        try:
            store = self.runtime.new_store(ctx.name("store"))
            if plan.kind == "writes":
                endpoint = system.serve(ctx, store, regime)
                self.writes(ctx, endpoint, base)
                return
            step = system.load(ctx, store, plan.inputs, regime)
            m = step.measured
            status = "ok" if m.ok else ("timeout" if m.timed_out else "failed")
            self.emit(base, task="load", status=status, ms=m.ms, peak_mib=m.peak_mib if m.peak_mib is not None else "",
                      rows=step.asserted if step.asserted is not None else "", note=step.note)
            if status != "ok":
                return
            in_memory = system.in_memory(regime)
            if step.inferred is not None or step.reason_ms is not None:
                self.emit(base, task="reason", ms=step.reason_ms if step.reason_ms is not None else "",
                          rows=step.inferred if step.inferred is not None else "",
                          note="" if step.reason_ms is not None else "time included in the load")
            if system.persistent and not in_memory:
                size = self.runtime.store_bytes(store)
                self.emit(base, task="size", bytes=size if size is not None else "")
            endpoint = system.serve(ctx, store, regime)
            base["version"] = system.version(ctx, endpoint)
            if system.persistent and not in_memory:
                self.emit(base, task="restart", ms=endpoint.restart_ms if endpoint.restart_ms is not None else "")
            n, ms, error = count_all(ctx, endpoint)
            self.emit(base, task="count", status="failed" if error else "ok",
                      ms="" if system.answers_only else ms, rows=n if n is not None else "",
                      note=error or ("the rules run at this first query" if system.lazy(regime) else ""))
            if plan.queries:
                self.queries(ctx, plan, key, system, endpoint, base)
        except Exception as e:  # a system that fails mustn't stop the suite
            self.emit(base, task="load" if endpoint is None else "serve", status="failed", note=str(e)[:300])
        finally:
            peak = system.stop(ctx)
            if endpoint is not None and peak is not None:
                self.emit(base, task="serve", peak_mib=peak, note="server peak over the run")
            if store is not None:
                self.runtime.remove_store(store)
            shutil.rmtree(work, ignore_errors=True)

    def queries(self, ctx: Context, plan, key: str, system, endpoint: Endpoint, base: dict):
        report = ctx.logs / "queries.json"
        command = [self.harness(), "query-mix", "--endpoint", endpoint.query, "--queries", str(plan.queries),
                   "--label", key, "--warmup", "1", "--runs", str(self.args.query_runs),
                   "--timeout-s", str(self.args.query_timeout_s), "--report-json", str(report)]
        self.host_command(command, ctx.logs / "queries.txt")
        if self.args.dry_run:
            return
        try:
            queries = json.loads(report.read_text(encoding="utf-8"))["queries"]
        except (OSError, ValueError, KeyError) as e:
            self.emit(base, task="query", item="*", status="failed", note=f"no query report: {e}")
            return
        for q in queries:
            if q.get("error"):
                timeout = "timed out" in q["error"].lower() or "timeout" in q["error"].lower()
                self.emit(base, task="query", item=q["id"], status="timeout" if timeout else "failed",
                          note=q["error"][:300])
                continue
            expected = (plan.expected or {}).get(q["id"])
            wrong = expected is not None and q.get("rows") != expected
            note = f"expected {expected}" if wrong else ""
            if system.answers_only:
                note = (note + "; " if note else "") + "answered by Oxigraph over the closure"
            for i, latency in enumerate(q.get("latencies_ms") or [None], start=1):
                self.emit(base, task="query", item=q["id"], repeat=i, status="wrong" if wrong else "ok",
                          ms="" if system.answers_only or latency is None else latency,
                          rows=q.get("rows") if q.get("rows") is not None else "", note=note)

    def writes(self, ctx: Context, endpoint: Endpoint, base: dict):
        report = ctx.logs / "write-scaling.json"
        steps = {"100k": "10000,100000", "1m": "10000,100000,500000,1000000",
                 "10m": "1000000,10000000"}.get(base["tier"], base["tier"])
        started = time.monotonic()
        rc = self.host_command([self.harness(), "write-scaling", "--nrese-base-url",
                                endpoint.query.rsplit("/dataset/", 1)[0], "--steps", steps,
                                "--report-json", str(report)], ctx.logs / "write-scaling.txt")
        self.emit(base, task="update", status="ok" if rc == 0 else "failed", ms=(time.monotonic() - started) * 1000,
                  note=f"steps {steps}; per-step figures in {report.name}")

    def kit(self, plan, key: str):
        base = dict(date=self.date, host=self.host, runtime="process", system=key, version="-",
                    publish=self.publish(key), workload=plan.workload, tier=plan.tier, regime="-", run=1)
        log = self.results / "logs" / slug(f"{self.stamp}-{key}-{plan.workload}") / "kit.log"
        self.say(f"\n{key} / {plan.workload}: {shell(plan.command)}")
        started = time.monotonic()
        rc = self.host_command(plan.command, log)
        self.emit(base, task="conformance", item=plan.workload, status="ok" if rc == 0 else "failed",
                  ms=(time.monotonic() - started) * 1000, note=f"log: {log.relative_to(self.results)}")

    def skip(self, key: str, plan, reason: str, regime: str = "-"):
        base = dict(date=self.date, host=self.host, runtime=self.runtime.kind, system=key, version="-",
                    publish=self.publish(key), workload=plan.workload, tier=plan.tier, regime=regime, run=0)
        self.emit(base, task="load", status="skipped", note=reason[:300])

    # --- the walk ---
    def run(self) -> int:
        systems = self.selected_systems()
        unknown = [s for s in systems if s not in self.systems]
        if unknown:
            self.say(f"unknown systems: {', '.join(unknown)}; known: {', '.join(self.systems)}")
            return 2
        chosen = []  # (plan, system key, adapter, regime)
        for name in self.selected_workloads():
            workload = self.workloads.get(name)
            if workload is None:
                self.say(f"unknown workload {name}")
                return 2
            tier_plans = plans(self.root, name, self.tiers(name), self.settings)
            if tier_plans is None:
                self.skips.append(f"{name}: {workload['state']}, no kit in the suite yet (completion plan 3.4)")
                continue
            for plan in tier_plans:
                if plan.unavailable:
                    self.skips.append(f"{name} {plan.tier}: {plan.unavailable}")
                    continue
                for key in systems:
                    if plan.systems is not None and key not in plan.systems:
                        continue
                    missing = verdict(self.systems[key], workload)
                    if missing:
                        self.skip(key, plan, f"lacks {missing}")
                        continue
                    if plan.kind == "kit":
                        chosen.append((plan, key, None, "-"))
                        continue
                    system = adapter(key)
                    if system is None:
                        self.skip(key, plan, "no adapter (systems.toml: runs = [])")
                        continue
                    regime = next((r for r in plan.regimes if r in system.regimes), None)
                    if regime is None:
                        self.skip(key, plan, f"runs none of the regimes {', '.join(plan.regimes)}")
                        continue
                    ctx = self.context_for_images()
                    ctx.tier_small = plan.small
                    reason = system.supports(ctx, plan.inputs, regime) or self.sif_missing(system, ctx)
                    if reason:
                        self.skip(key, plan, reason, regime)
                        continue
                    chosen.append((plan, key, system, regime))
        if not chosen:
            self.say("nothing to run")
            self.report_skips()
            return 0
        # Preparation: images, builds, data.
        ctx = self.context_for_images()
        images: dict[str, str | None] = {}
        for _, _, system, _ in chosen:
            if system is None:
                continue
            for image in system.images(ctx):
                images.setdefault(image, None)
            for image, context in system.builds():
                images[image] = context
        if self.args.runtime == "docker":
            images.setdefault("alpine:latest", None)
        self.ensure_images(images)
        for key in dict.fromkeys(key for _, key, system, _ in chosen if isinstance(system, Nrese)):
            if not self.build_nrese(adapter(key)):
                return 1
        if any(system is not None and plan.kind != "kit" for plan, _, system, _ in chosen):
            if not self.build_harness():
                self.say("the harness build failed")
                return 1
        unavailable: dict[int, str] = {}
        for plan, _, system, _ in chosen:
            if system is not None and plan.kind == "cycle" and id(plan) not in unavailable:
                unavailable[id(plan)] = self.ensure_data(plan) or ""
        for plan, key, system, regime in chosen:
            if unavailable.get(id(plan)):
                self.skip(key, plan, unavailable[id(plan)], regime)
                continue
            if plan.kind == "kit":
                self.kit(plan, key)
                continue
            for run in range(1, self.args.runs + 1):
                self.cycle(plan, key, system, regime, run)
        self.cross_check()
        self.report_skips()
        self.say(f"\nresults: {self.writer.path}")
        return 0

    def cross_check(self):
        """Answer counts per query side by side, within each regime."""
        counts: dict[tuple, dict[str, set]] = defaultdict(lambda: defaultdict(set))
        for r in self.written:
            if r.task == "query" and r.rows != "":
                counts[(r.workload, r.tier, r.regime, r.item)][r.system].add(r.rows)
        disagreements = [(k, v) for k, v in sorted(counts.items())
                         if len({x for s in v.values() for x in s}) > 1]
        if counts:
            self.say("\nanswer counts: " + ("all systems agree within each regime" if not disagreements else
                                            f"{len(disagreements)} queries differ"))
        for (workload, tier, regime, item), by_system in disagreements:
            shown = ", ".join(f"{s} {'/'.join(map(str, sorted(v)))}" for s, v in sorted(by_system.items()))
            self.say(f"  {workload} {tier} {regime} {item}: {shown}")

    def report_skips(self):
        if self.skips:
            self.say("\nnot run:")
            for line in self.skips:
                self.say(f"  {line}")

    def cleanup(self):
        """What this run made goes: containers, stores, scratch; and unless kept for a
        batch, datasets, build volumes and images (installed tools stay)."""
        self.runtime.cleanup()
        shutil.rmtree(self.scratch, ignore_errors=True)
        if self.args.dry_run:
            return
        if not self.keep:
            if self.made_files and self.data_volume and self.data_volume not in self.made_volumes:
                run_quiet(["docker", "run", "--rm", "-v", f"{self.data_volume}:/data", "alpine", "rm", "-f",
                           *self.made_files])
            for volume in self.made_volumes:
                run_quiet(["docker", "volume", "rm", "-f", volume])
            if not self.tools_installed:
                for image in self.made_images:
                    run_quiet(["docker", "image", "rm", image])
        self.made_files, self.made_volumes, self.made_images = [], [], []


def images(argv: list[str], systems: dict, root: Path) -> int:
    """`suite.py images [systems]`: the images the systems need, one per line, with the
    directory under benches/ an image is built from ("-": pulled from its registry)."""
    keys = argv or [k for k in systems if k in ADAPTERS]
    ctx = Context(None, root, root, root, Mount("-", "/data"), [], 0, dict(os.environ), "images")
    seen = {}
    for key in keys:
        system = adapter(key)
        if system is None:
            continue
        for image in system.images(ctx):
            seen.setdefault(image, "-")
        for image, context in system.builds():
            seen[image] = context
    for image, context in seen.items():
        print(f"{image}\t{context}")
    return 0


def main(argv: list[str], systems: dict, workloads: dict, root: Path) -> int:
    args = arguments(argv)
    suite = Suite(args, systems, workloads, root)
    atexit.register(suite.cleanup)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        return suite.run()
    except KeyboardInterrupt:
        print("\ninterrupted: cleaning up", file=sys.stderr)
        return 130
    finally:
        suite.cleanup()
