"""`suite.py run`: walks the matrix of workloads and systems, runs every pair that can run,
and writes one result file (schema.py).

For each workload tier, system and repetition, from a fresh store: load (reasoning where
the system reasons at load), store size, restart, a count of every statement (where lazy
reasoners do their work), the queries (one warm-up, then the measured repetitions, through
the harness's `query-mix`), the server's peak memory. After every repetition its
containers, store and scratch go; after the whole run, what the run itself made (datasets,
the NRESE build volume, images it pulled or built) goes too, unless kept for a batch
(--keep, or NRESE_BENCH_KEEP=1; installed tools always stay). What the run was measured
with (commit, machine, protocol, images, datasets) goes to manifest.json (manifest.py).
"""
from __future__ import annotations

import argparse
import atexit
import contextlib
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
import uuid
from collections import defaultdict
from pathlib import Path

from .adapters import ADAPTERS, Context, Endpoint, Nrese, adapter, count_all, post
from . import hosts, ns
from .answers import Recorder, capped_post
from .runtime import (ApptainerRuntime, DockerRuntime, DryRuntime, Mount, ProcessRuntime, RootfsRuntime,
                      run_quiet, shell)
from . import receipts
from .datasets import Store
from .manifest import OPERATIONAL_SETTINGS, Manifest, ManifestError, append_attempts, image_ids
from .manifest import git as checkout_state
from .schema import FIELDS, Result, Writer, problems, read
from .summary import adjudications, summarise
from .workloads import plans

EXE = ".exe" if os.name == "nt" else ""
# Systems a default run leaves out: AnzoGraph needs registration beyond 8 GB and takes
# minutes to start; name it to run it.
EXPLICIT_ONLY = {"anzograph"}
IMAGE_LABEL = "owlrs.built-from"  # a suite-built image's context and its tree id (receipts: images)
QUALIFIED_ANSWERS = {"ok", "deviates"}  # a cell's answers qualify when every query's verdict is one of these


def arguments(argv: list[str]) -> argparse.Namespace:
    p = argparse.ArgumentParser(prog="suite.py run", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--systems", help="comma-separated systems.toml keys (default: every system with an adapter)")
    p.add_argument("--workloads", help="comma-separated workloads.toml keys (default: all)")
    p.add_argument("--tier", action="append", default=[], metavar="WORKLOAD=TIER",
                   help="a tier to run (repeatable), e.g. lubm=10, owl2bench=ql-1, basics-mix=yago-tiny")
    p.add_argument("--runs", type=int, default=3, help="repetitions, each from a fresh store (default 3)")
    p.add_argument("--query-runs", type=int, default=3, help="measured runs per query after one warm-up (default 3)")
    p.add_argument("--cache", default="off,on",
                   help="result-cache modes for systems that have a result cache, in this order: "
                        "off, on (the system's default) or both (default off,on; the server restarts "
                        "between them, which empties the cache)")
    p.add_argument("--order", choices=["shuffled", "fixed"], default="shuffled",
                   help="query order: rounds over all queries, each round shuffled (default), or each "
                        "query's runs back to back in file order")
    p.add_argument("--seed", type=int, default=1,
                   help="the shuffle seed; repetition r uses seed*1000+r, the same for every system")
    p.add_argument("--timeout-s", type=int, default=3600, help="per load (default 3600)")
    p.add_argument("--query-timeout-s", type=int, default=300, help="per query run (default 300)")
    p.add_argument("--runtime", choices=["docker", "apptainer", "rootfs", "process"], default="docker")
    p.add_argument("--rootfs-dir", help="rootfs: the images unpacked for bubblewrap (benches/sparql/images.sh export)")
    p.add_argument("--sif-dir", help="Apptainer: the directory of the SIF images")
    p.add_argument("--data", help="the datasets: a directory, or volume:NAME with Docker (default volume:nrese-bench-data)")
    p.add_argument("--results", help="the result directory (default benches/suite/results/<date>)")
    p.add_argument("--port", type=int, default=18950, help="the port the systems are reached on")
    p.add_argument("--dry-run", action="store_true", help="print every step instead of running it")
    p.add_argument("--keep", action="store_true",
                   help="keep datasets, the NRESE build and images for the next run of a batch")
    p.add_argument("--skip-build", action="store_true", help="use the existing NRESE and harness builds")
    p.add_argument("--answers", choices=["off", "full"], default="off",
                   help="full: capture every query's complete answer, untimed, and compare each system "
                        "with --answers-reference (answers.py; writes answers.json)")
    p.add_argument("--answers-reference", metavar="SYSTEM[,SYSTEM]",
                   help="the reference systems the answers are compared with, in order: per query the first "
                        "with a complete, stable answer, confirmed by the next (answers.py)")
    args = p.parse_args(argv)
    if args.answers == "full" and not args.answers_reference:
        p.error("--answers full needs --answers-reference SYSTEM")
    args.cache = [m.strip() for m in args.cache.split(",") if m.strip()]
    if not args.cache or any(m not in ("off", "on") for m in args.cache) or len(set(args.cache)) != len(args.cache):
        p.error("--cache is off, on, or both in some order (off,on)")
    return args


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


def kit_env(environ) -> dict:
    """A kit's environment: the W3C suites require their test data (a missing suite fails, never
    passes by skipping), unless NRESE_W3C_OPTIONAL opts out (a local run without the data)."""
    return {} if environ.get("NRESE_W3C_OPTIONAL") else {"NRESE_W3C_REQUIRED": "1"}


def kit_counts(log: Path) -> str:
    """The counts a conformance kit printed (its run with --nocapture), one clause per suite,
    so a row says what ran and passed, not only that the command exited 0; a skip shows too."""
    try:
        lines = log.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError:
        return ""
    out, title = [], None
    for line in lines:
        header = re.match(r"W3C (.+) on engine v2: passed / failed / skipped", line)
        total = re.match(r"\s+total: (\d+) / (\d+) / (\d+)", line)
        if header:
            title = header.group(1)
        elif total and title:
            out.append(f"{title}: {total.group(1)} passed, {total.group(2)} failed, {total.group(3)} skipped")
            title = None
        elif re.match(r"(\S.*: \d+ of \d+ pass$|W3C .*: \d+ passed, |GeoSPARQL .*: \d+ of \d+ |skipped: )", line):
            out.append(line.strip())
    return "; ".join(out)


def external_rows(rows: list[dict], captured: bool) -> list[dict]:
    """An external driver's rows as the suite's. Answers before time: its timings count only
    after its own qualification passed (a `conformance` row `...-qualification`, ok), or with
    the plan's qualification queries captured by the answer recorder (`captured`); otherwise
    its query rows are `failed`, without a time."""
    allowed = {"task", "item", "repeat", "status", "ms", "rows", "note", "peak_mib"}
    qualified = captured or any(
        r.get("task") == "conformance" and str(r.get("item", "")).endswith("qualification") and r.get("status") == "ok"
        for r in rows)
    out = []
    for row in rows:
        row = {k: v for k, v in row.items() if k in allowed}
        if not qualified and row.get("task") == "query" and row.get("status") == "ok":
            row.update(status="failed", ms="",
                       note=("answers unqualified: no time; " + str(row.get("note", ""))).strip("; "))
        out.append(row)
    return out


class Suite:
    def __init__(self, args, systems: dict, workloads: dict, root: Path):
        self.args = args
        self.systems = systems
        self.workloads = workloads
        self.root = root
        self.settings = dict(os.environ)
        now = datetime.datetime.now()
        self.date = now.isoformat(timespec="minutes")
        self.stamp = now.strftime("%Y%m%d-%H%M%S-%f")
        self.host = socket.gethostname()
        scratch_root = Path(self.settings.get("NRESE_BENCH_SCRATCH") or root / "tmp")
        self.scratch = scratch_root.resolve() / f"{ns.name('suite', root)}-{self.stamp}"
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
        elif args.runtime == "rootfs":
            if not args.rootfs_dir:
                sys.exit("--runtime rootfs needs --rootfs-dir")
            runtime = RootfsRuntime(self.scratch, args.port, Path(args.rootfs_dir).resolve())
        else:
            runtime = ProcessRuntime(self.scratch, args.port)
        self.runtime = DryRuntime(runtime) if args.dry_run else runtime
        data = args.data or (f"volume:{hosts.volume('data')}" if args.runtime == "docker" else None)
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
        # Claim this invocation's scratch before creating anything we may later remove.
        self.scratch.mkdir(parents=True, exist_ok=False)
        self.results.mkdir(parents=True, exist_ok=True)  # the directory; its results.csv waits (writer)
        self._writer: Writer | None = None
        self.made_files: list[str] = []  # datasets this run made, as /data paths
        self.made_volumes: list[str] = []
        self.made_images: list[str] = []
        self.written: list[Result] = []
        self.hashed: dict[str, str | None] = {}  # /data input -> its SHA-256 (data_hashes)
        self.built: dict[str, dict] = {}  # program -> the checkout it was built from in this run
        self.image_sources: dict[str, object] = {}  # image id -> where it came from (image_source)
        self.hash_s = 0.0
        self.skips: list[str] = []
        self.manifest: Manifest | None = None
        self.answers = (Recorder(self.results, args.answers_reference, adjudications())
                        if args.answers == "full" and not args.dry_run else None)

    @property
    def writer(self) -> Writer:
        """The run's results.csv, created at its first row: in a run, after main took the results
        directory (Manifest's lock), never before, so a second starter, refused there, never opens
        it (review cell-receipts r5 F1: Writer's exists check, then open "w")."""
        if getattr(self, "_writer", None) is None:
            self._writer = Writer(self.results / "results.csv", echo=print)
        return self._writer

    @writer.setter
    def writer(self, writer: Writer):
        self._writer = writer

    # --- helpers ---
    def say(self, text: str):
        print(text, flush=True)

    def host_command(self, command: list[str], log: Path | None = None, timeout: float | None = None,
                     cwd: Path | None = None, env: dict | None = None) -> int:
        """Runs a command on the host, from the repository root unless `cwd` says otherwise
        (echoed in a dry run)."""
        if command[0] == "bash":
            command = [bash(), *command[1:]]
        if self.args.dry_run:
            self.say(f"    [host] {shell(command)}")
            return 0
        environment = {**os.environ, **env} if env else None
        if log is None:
            return subprocess.run(command, cwd=cwd or self.root, timeout=timeout, env=environment).returncode
        log.parent.mkdir(parents=True, exist_ok=True)
        with open(log, "w", encoding="utf-8", errors="replace") as f:
            return subprocess.run(command, cwd=cwd or self.root, stdout=f, stderr=subprocess.STDOUT,
                                  timeout=timeout, env=environment).returncode

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
                # What it is built from, on the image itself: its context's git tree id in this run's
                # commit, when that is clean (P2 design review F5). The receipt reads it off the id
                # that runs (image_source), never off this tag (P1b review r1 F1).
                state = checkout_state(self.root)
                tree = (receipts.object_ids(self.root, state["commit"] or "", [f"benches/{context}"])[f"benches/{context}"]
                        if state.get("dirty") == 0 else None)
                made = f"benches/{context}@{tree}" if isinstance(tree, str) else ""
                self.say(f"building {image}")
                self.host_command(["docker", "build", "-q", "-t", image,
                                   *(["--label", f"{IMAGE_LABEL}={made}"] if made else []),
                                   str(self.root / "benches" / context)])
            else:
                self.say(f"pulling {image}")
                self.host_command(["docker", "pull", "-q", image])
            self.made_images.append(image)

    def image_source(self, image_id: str):
        """Where the image with this id came from, read off that id (the one the run's containers
        run, its pin), never off a tag that may name another by now (P1b review r1 F1): its build
        label, or unknown (no label: present before, built from an unclean checkout, or pulled with
        no digest pin to hold it to)."""
        if image_id not in self.image_sources:
            # Unreadable is unknown with its reason, never an exception: a receipt is written on a
            # host without docker too (#161's Windows gate, gate-windows-59532fff57-1794140734).
            try:
                label = run_quiet(["docker", "image", "inspect", "--format",
                                   f'{{{{index .Config.Labels "{IMAGE_LABEL}"}}}}', image_id])
            except OSError as error:
                self.image_sources[image_id] = receipts.unknown(f"docker not available: {error}")
                return self.image_sources[image_id]
            if label.returncode != 0:
                said = (label.stderr or label.stdout or "").strip().splitlines()
                self.image_sources[image_id] = receipts.unknown(
                    f"docker image inspect failed (exit {label.returncode}): {said[-1] if said else 'no output'}")
                return self.image_sources[image_id]
            found = label.stdout.strip().replace("<no value>", "")
            self.image_sources[image_id] = (f"built from {found}" if found else
                                            receipts.unknown("no build label on the image that ran"))
        return self.image_sources[image_id]

    def build_nrese(self, nrese) -> bool:
        """Builds an NRESE server (`nrese` or `nrese-oxigraph`) from its source tree."""
        if self.args.skip_build:
            return True
        ctx = self.context_for_images()
        source = nrese.source(ctx)
        # What it is built from and with (P2 design review F4): its checkout when the build starts, and
        # the build's own configuration, never an environment list taken for its defaults.
        self.built[nrese.key] = {**{k: v for k, v in checkout_state(Path(source)).items() if k in ("commit", "dirty")},
                                 **build_settings()}
        self.say(f"building {nrese.key} (release) from {source}")
        log = self.results / "logs" / f"{nrese.key}-build-{self.stamp}.log"
        if self.args.runtime == "docker":
            # Its own build volume, labelled, removed with the run; the Cargo registry is the
            # host's shared cache, never removed by a run.
            if nrese.target_volume not in self.made_volumes and \
                    ns.docker(ns.volume("inspect", nrese.target_volume)).returncode != 0:
                ns.docker(ns.volume("create", nrese.target_volume, self.root, "suite"))
                self.made_volumes.append(nrese.target_volume)
            # Built in the pinned Rust image, as the scorecards do: the binary runs in it.
            # Half the cores, like scripts/cargo-guarded.sh: the machine stays usable.
            cpus = str(max(1, (os.cpu_count() or 2) // 2))
            # Code for this machine's CPU, where the containers run (NRESE_TARGET_CPU, as
            # scripts/lib/target-cpu.sh reads it; `portable` for none).
            cpu = os.environ.get("NRESE_TARGET_CPU", "native")
            rustflags = "" if cpu in ("", "portable") else f"-C target-cpu={cpu}"
            self.built[nrese.key].update(rust_image=nrese.rust_image(ctx), rustflags=rustflags)
            rc = self.host_command([ns.executable(), *ns.run_args(self.root, "suite"), "--rm", "--cpus", cpus,
                                    "-v", f"{source.as_posix()}:/src:ro",
                                    "-e", f"RUSTFLAGS={rustflags}",
                                    "-v", f"{nrese.target_volume}:/target",
                                    "-v", f"{ns.shared('cargo')}:/usr/local/cargo/registry",
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
        if self.args.skip_build:
            if Path(self.harness()).is_file():
                return True
            self.say(f"--skip-build requires an existing harness: {self.harness()} (set HARNESS)")
            return False
        self.say("building the harness (release)")
        self.built["harness"] = {**{k: v for k, v in checkout_state(self.root).items() if k in ("commit", "dirty")},
                                 **build_settings()}
        log = self.results / "logs" / f"harness-build-{self.stamp}.log"
        rc = self.host_command(["bash", "scripts/cargo-guarded.sh", "build", "--release", "--locked", "--quiet",
                                "--manifest-path", "benches/nrese-bench-harness/Cargo.toml"], log)
        return rc == 0

    def in_data(self, *command: str):
        """`command` in a container over the datasets' volume, read-only."""
        return run_quiet([ns.executable(), *ns.run_args(self.root, "suite"), "--rm", "-v", f"{self.data_volume}:/data:ro",
                          "alpine", *command])

    def data_missing(self, inputs: list[str]) -> list[str]:
        files = [i for i in inputs if i.startswith("/data/")]
        if not files:
            return []
        if self.data_volume:
            if self.args.dry_run:
                return []
            return self.in_data("sh", "-c", "; ".join(f"test -s {f} || echo {f}" for f in files)).stdout.split()
        return [f for f in files if not (Path(self.data.source) / f[len("/data/"):]).is_file()]

    def data_sizes(self, inputs: list[str]) -> dict[str, int]:
        """The sizes of the /data inputs, for the manifest (a changed dataset shows)."""
        files = sorted({i for i in inputs if i.startswith("/data/")})
        if not files or self.args.dry_run:
            return {}
        if self.data_volume:
            out = self.in_data("stat", "-c", "%n %s", *files)
            pairs = (line.rsplit(" ", 1) for line in out.stdout.splitlines() if " " in line)
            return {name: int(size) for name, size in pairs if size.isdigit()}
        sizes = {}
        for f in files:
            path = Path(self.data.source) / f[len("/data/"):]
            if path.is_file():
                sizes[f] = path.stat().st_size
        return sizes

    def data_sources(self) -> list[str]:
        """Where the downloaded sources came from, and their release (the `.source` files
        benches/competitors/prepare-datasets.sh leaves beside each download)."""
        if self.args.dry_run:
            return []
        if self.data_volume:
            out = self.in_data("sh", "-c", "cat /data/real/*.source 2>/dev/null")
            return [line for line in out.stdout.splitlines() if line.strip()]
        real = Path(self.data.source) / "real"
        return [line for f in sorted(real.glob("*.source")) for line in f.read_text().splitlines()
                if line.strip()] if real.is_dir() else []

    def ensure_data(self, plan) -> str | None:
        # The datasets' volume is the host's, shared by every job: a run that creates it (by
        # looking for its inputs) never removes it, nor the files it fetched into it (another
        # job may read them by then; scripts/bench-cleanup.sh --datasets, by hand).
        missing = self.data_missing(plan.all_inputs())
        if not missing:
            return None
        if not (self.data_volume and plan.prepare):
            return f"missing inputs: {', '.join(missing)} (prepare them on a machine with Docker)"
        self.say(f"preparing {plan.workload} {plan.tier}: {shell(plan.prepare)}")
        log = self.results / "logs" / f"prepare-{slug(plan.workload)}-{slug(plan.tier)}-{self.stamp}.log"
        # Shared datasets are prepared one at a time per host (the preparation scripts take the
        # same lock: held here, they don't wait for themselves).
        with ns.lock(ns.lock_dir() / "datasets.lock"), held_lock("datasets"):
            rc = self.host_command(plan.prepare, log)
        self.made_files += missing
        still = self.data_missing(plan.all_inputs())
        return f"preparing the inputs failed; see {log}" if rc != 0 or still else None

    def sif_missing(self, system, ctx: Context) -> str | None:
        if self.args.runtime not in ("apptainer", "rootfs"):
            return None
        images = system.images(ctx) + [image for image, _ in system.builds()]
        missing = [i for i in images if not self.runtime.image_present(i)]
        if missing and self.args.runtime == "rootfs":
            return f"no unpacked image for {', '.join(missing)} in --rootfs-dir (benches/sparql/images.sh export)"
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
        self.runtime.owned_path(str(work)).mkdir(parents=True)
        ctx = Context(self.runtime, self.root, work, logs, self.data, plan.mounts, self.args.timeout_s,
                      {**self.settings, "QUERY_TIMEOUT_S": str(self.args.query_timeout_s)}, run_id, plan.small)
        ctx.services = tuple(plan.services)
        base = dict(date=self.date, host=self.host, runtime=self.runtime.kind, system=key, version="-",
                    publish=self.publish(key), workload=plan.workload, tier=plan.tier, regime=regime, run=run)
        self.say(f"\n{key} / {plan.workload} {plan.tier} / {regime} / run {run}")
        store = None
        endpoint = None
        try:
            store = self.runtime.new_store(ctx.name("store"))
            if plan.kind == "writes":
                if key == "graphdb":
                    # Serving a fresh GraphDB home does not create its repository.
                    # Reuse its configured loader; setup is outside write timings.
                    (ctx.work / "empty.nt").write_bytes(b"")
                    setup = system.load(ctx, store, ["/work/empty.nt"], regime)
                    self.emit(base, task="load", status="ok" if setup.measured.ok else "failed",
                              ms=setup.measured.ms, note="empty repository setup; " + setup.note)
                    if not setup.measured.ok:
                        return
                endpoint = system.serve(ctx, store, regime)
                base["version"] = system.version(ctx, endpoint)
                self.writes(ctx, endpoint, base)
                return
            step = system.load(ctx, store, plan.inputs_of(key), regime)
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
            modes = self.args.cache if system.result_cache else ["-"]
            ctx.cache = modes[0] if modes[0] != "-" else "off"
            endpoint = system.serve(ctx, store, regime)
            base["version"] = system.version(ctx, endpoint)
            if endpoint.index_ms is not None:
                self.emit(base, task="index", ms=endpoint.index_ms, note=endpoint.index_note)
            if system.persistent and not in_memory:
                self.emit(base, task="restart", ms=endpoint.restart_ms if endpoint.restart_ms is not None else "")
            if self.settings.get("SKIP_COUNT") == "1":
                # A lazy reasoner's count of everything forces its whole closure (Jena's
                # rules on LUBM 10 ran out of heap there); the queries alone then decide.
                self.emit(base, task="count", status="skipped", note="SKIP_COUNT=1")
            else:
                n, ms, error = count_all(ctx, endpoint)
                self.emit(base, task="count", status="failed" if error else "ok",
                          ms="" if system.answers_only else ms, rows=n if n is not None else "",
                          note=error or ("the rules run at this first query" if system.lazy(regime) else ""))
            if plan.queries or plan.external:
                for i, mode in enumerate(modes):
                    if i > 0:
                        # A restart in the next mode: the cache starts empty, so each
                        # query's first execution is a miss in every mode.
                        if not system.persistent or in_memory:
                            break
                        peak = system.stop(ctx)
                        endpoint = None
                        if peak is not None:
                            self.emit({**base, "cache": modes[i - 1]}, task="serve", peak_mib=peak,
                                      note="server peak over the run")
                        ctx.cache = mode
                        endpoint = system.serve(ctx, store, regime)
                    if plan.external:
                        self.external(ctx, plan, key, endpoint, {**base, "cache": mode}, run)
                    else:
                        self.queries(ctx, plan, key, system, endpoint, {**base, "cache": mode}, run)
                base["cache"] = modes[-1]
        except Exception as e:  # a system that fails mustn't stop the suite
            self.emit(base, task="load" if endpoint is None else "serve", status="failed", note=str(e)[:300])
        finally:
            def stop():
                peak = system.stop(ctx)
                if endpoint is not None and peak is not None:
                    self.emit(base, task="serve", peak_mib=peak, note="server peak over the run")

            self.runtime.attempt(stop)
            # The runtime also owns interrupted loaders and partially started servers.
            self.runtime.cleanup()
            if not self.runtime.workers():
                if store is not None and (self.args.dry_run or store in self.runtime.created_stores):
                    self.runtime.attempt(self.runtime.remove_store, store)
                self.runtime.attempt(lambda: shutil.rmtree(self.runtime.owned_path(str(work))))

    def external(self, ctx: Context, plan, key: str, endpoint: Endpoint, base: dict, run: int):
        """A benchmark's own driver (Plan.external) against the endpoint; its rows as ours."""
        suffix = "" if base["cache"] == "-" else f"-cache-{base['cache']}"
        out = ctx.logs / f"external{suffix}.json"
        values = {"endpoint": endpoint.query, "update": endpoint.update or "", "out": str(out),
                  "system": key, "run": str(run)}
        command = [part.format(**values) for part in plan.external]
        rc = self.host_command(command, ctx.logs / f"external{suffix}.txt")
        if self.args.dry_run:
            return
        try:
            rows = json.loads(out.read_text(encoding="utf-8"))
        except (OSError, ValueError) as e:
            self.emit(base, task="query", item="*", status="failed", note=f"driver exit {rc}, no rows: {e}")
            return
        for row in external_rows(rows, plan.queries is not None):
            self.emit(base, **row)
        if self.answers and plan.queries and not plan.replay:
            for query in sorted(plan.queries.glob("*.rq")):
                item = (plan.workload, plan.tier, base["regime"], query.name)
                self.answers.expect(item, key, tuple(plan.inputs))
                self.answers.capture(capped_post, endpoint.query, item,
                                     key, base["cache"], run, query, ctx.logs, self.args.query_timeout_s)

    def queries(self, ctx: Context, plan, key: str, system, endpoint: Endpoint, base: dict, run: int):
        suffix = "" if base["cache"] == "-" else f"-cache-{base['cache']}"
        report = ctx.logs / f"queries{suffix}.json"
        selected = sorted(plan.queries.glob("*.rq"))
        # A log is replayed as it is: each entry once, in order, without a warm-up.
        warmup, runs = ("0", "1") if plan.replay else ("1", str(self.args.query_runs))
        if self.answers and not plan.replay:
            for query in selected:
                item = (plan.workload, plan.tier, base["regime"], query.name)
                self.answers.expect(item, key, tuple(plan.all_inputs()))
                self.answers.check(item, plan.answers.get(query.stem))
        command = [self.harness(), "query-mix", "--endpoint", endpoint.query, "--queries", str(plan.queries),
                   "--label", key, "--warmup", warmup, "--runs", runs,
                   "--timeout-s", str(self.args.query_timeout_s), "--report-json", str(report)]
        if self.args.order == "shuffled" and not plan.replay:
            seed = self.args.seed * 1000 + run
            command += ["--order", "shuffled", "--seed", str(seed)]
            base = {**base, "order": f"shuffled:{seed}"}
        else:
            base = {**base, "order": "fixed"}
        try:
            # Each invocation owns a fresh report. Preserve old evidence rather
            # than allowing a failed launcher to reuse it as this run's success.
            if not self.args.dry_run and report.exists():
                raise ValueError(f"report already exists: {report}")
            self.host_command(command, ctx.logs / f"queries{suffix}.txt")
            if self.args.dry_run:
                return
            mix = json.loads(report.read_text(encoding="utf-8"))
            queries = mix["queries"]
            if sorted(q["id"] for q in queries) != [q.stem for q in selected]:
                raise ValueError("query report must contain each selected case exactly once")
            if any(not q.get("error") and (q.get("first_ms") is None or q.get("rows") is None
                   or len(q.get("latencies_ms") or []) != int(runs)) for q in queries):
                raise ValueError("query report contains unfinished measurements")
        except (OSError, ValueError, KeyError, TypeError) as e:
            error = f"invalid query report: {e}"
            self.emit(base, task="query", item="*", status="failed", note=error)
            if self.answers and not plan.replay:
                for query in selected:
                    self.answers.capture(capped_post, endpoint.query,
                                         (plan.workload, plan.tier, base["regime"], query.name),
                                         key, base["cache"], run, query, ctx.logs, self.args.query_timeout_s,
                                         prior_error=error)
            return
        for clients in plan.clients:
            # Concurrent clients, one level at a time: the slowest query's p99 as the time,
            # completed queries as rows, queries per second in the note.
            level = ctx.logs / f"clients-{clients}{suffix}.json"
            self.host_command([self.harness(), "query-mix", "--endpoint", endpoint.query, "--queries",
                               str(plan.queries), "--label", key, "--warmup", "0", "--runs", "1",
                               "--timeout-s", str(self.args.query_timeout_s), "--clients", str(clients),
                               "--duration-s", str(plan.duration_s), "--report-json", str(level)],
                              ctx.logs / f"clients-{clients}{suffix}.txt")
            try:
                throughput = json.loads(level.read_text(encoding="utf-8")).get("throughput")
            except (OSError, ValueError):
                throughput = None
            if not throughput:
                self.emit(base, task="query", item=f"clients-{clients}", status="failed", note="no throughput report")
                continue
            p99 = max((q["p99_ms"] for q in throughput["per_query"] if q.get("p99_ms") is not None), default=None)
            self.emit(base, task="query", item=f"clients-{clients}", repeat=1,
                      status="ok" if not throughput["errors"] else "failed", ms=p99 if p99 is not None else "",
                      rows=throughput["completed"],
                      note=f"{throughput['queries_per_s']:.1f} queries/s; {throughput['errors']} errors; "
                           f"time = the slowest query's p99")
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
            if q.get("first_ms") is not None and not system.answers_only:
                self.emit(base, task="query", item=q["id"], repeat=0, status="wrong" if wrong else "ok",
                          ms=q["first_ms"], rows=q.get("rows") if q.get("rows") is not None else "",
                          note=(note + "; " if note else "") + "first execution")
            for i, latency in enumerate(q.get("latencies_ms") or [None], start=1):
                self.emit(base, task="query", item=q["id"], repeat=i, status="wrong" if wrong else "ok",
                          ms="" if system.answers_only or latency is None else latency,
                          rows=q.get("rows") if q.get("rows") is not None else "", note=note)
        if self.answers and not plan.replay:
            failures = {q["id"]: q["error"] for q in queries if q.get("error")}
            for query in selected:
                self.answers.capture(capped_post, endpoint.query, (plan.workload, plan.tier, base["regime"], query.name),
                                     key, base["cache"], run, query, ctx.logs, self.args.query_timeout_s,
                                     prior_error=failures.get(query.stem))

    def writes(self, ctx: Context, endpoint: Endpoint, base: dict):
        report = ctx.logs / "write-scaling.json"
        steps = {"100k": "10000,100000", "1m": "10000,100000,500000,1000000",
                 "10m": "1000000,10000000"}.get(base["tier"], base["tier"])
        if base["system"] == "nrese":
            connection = ["--nrese-base-url", endpoint.query.rsplit("/dataset/", 1)[0]]
        elif base["system"] == "graphdb":
            connection = ["--reference-kind", "graphdb", "--reference-base-url", endpoint.query]
        else:
            raise ValueError("write-scaling has no route for " + base["system"])
        started = time.monotonic()
        rc = self.host_command([self.harness(), "write-scaling", *connection, "--steps", steps,
                                "--reset", "true",
                                "--report-json", str(report)], ctx.logs / "write-scaling.txt")
        checked = False
        try:
            result = json.loads(report.read_text(encoding="utf-8"))
            services = result["services"]
            records = services[0]["steps"]
            requested = [int(n) for n in steps.split(",")]
            samples = result["samples_per_step"]
            checked = (result["mode"] == "write-scaling" and len(services) == 1
                       and services[0]["label"].lower() == base["system"] and services[0]["reset"] is True
                       and type(samples) is int and samples > 0 and len(records) == len(requested))
            previous = 0
            for i, (row, size) in enumerate(zip(records, requested)):
                actual = size // 4 * 4
                expected = dict(triples=size, asserted_entity_triples=actual, loaded_triples=actual - previous,
                                expected_persons=size // 4, observed_persons=size // 4,
                                expected_probes=(i + 1) * samples, observed_probes=(i + 1) * samples)
                checked &= all(type(row[k]) is int and row[k] == v for k, v in expected.items())
                previous = actual
        except (OSError, ValueError, KeyError, IndexError, TypeError):
            checked = False
        self.emit(base, task="update", status="ok" if rc == 0 and checked else "failed",
                  ms=(time.monotonic() - started) * 1000,
                  note=f"steps {steps}; numeric checks {'passed' if checked else 'missing/failed'}; per-step figures in {report.name}")

    def kit(self, plan, key: str):
        base = dict(date=self.date, host=self.host, runtime="process", system=key, version="-",
                    publish=self.publish(key), workload=plan.workload, tier=plan.tier, regime="-", run=1)
        log = self.results / "logs" / slug(f"{self.stamp}-{key}-{plan.workload}") / "kit.log"
        if plan.prepare:
            # Its test data (the W3C kits: scripts/fetch-w3c-tests.sh, pinned).
            prepared = log.with_name("prepare.log")
            self.say(f"\n{key} / {plan.workload}: preparing: {shell(plan.prepare)}")
            if self.host_command(plan.prepare, prepared) != 0:
                self.emit(base, task="conformance", item=plan.workload, status="failed",
                          note=f"preparing its data failed: log {prepared.relative_to(self.results)}")
                return
        self.say(f"\n{key} / {plan.workload}: {shell(plan.command)}")
        started = time.monotonic()
        rc = self.host_command(plan.command, log, env=kit_env(os.environ))
        counts = "" if self.args.dry_run else kit_counts(log)
        self.emit(base, task="conformance", item=plan.workload, status="ok" if rc == 0 else "failed",
                  ms=(time.monotonic() - started) * 1000,
                  note=f"{counts + '; ' if counts else ''}log: {log.relative_to(self.results)}")

    def driver(self, plan, keys: list[str]):
        """A kit's own driver (Plan.driver) for `keys` on one tier: its steps in order, then
        every results.csv they wrote below {results} into the run's results.csv. {results} is
        this run's own (its stamp): kits append, and an earlier run into the same results
        directory would be imported again."""
        out = self.results / "driver" / slug(f"{self.stamp}-{plan.workload}-{plan.tier}")
        logs = self.results / "logs" / slug(f"{self.stamp}-driver-{plan.workload}-{plan.tier}")
        values = {"results": str(out), "systems": ",".join(keys), "tier": plan.tier, "runs": str(self.args.runs)}
        self.say(f"\n{','.join(keys)} / {plan.workload} {plan.tier}: the kit's driver")
        failed = None
        for n, step in enumerate(plan.driver, start=1):
            rc = self.host_command([part.format(**values) for part in step], logs / f"step-{n}.txt")
            if rc != 0:
                failed = f"step {n} exit {rc}: log {(logs / f'step-{n}.txt').relative_to(self.results)}"
                break
        if self.args.dry_run:
            return
        imported = 0
        for path in sorted(out.rglob("results.csv")) if out.exists() else []:
            for row in read(path):
                result = Result(**{k: row.get(k, "") for k in FIELDS})
                if problems(result):
                    self.say(f"    {path}: a row the schema refuses ({'; '.join(problems(result))}): left out")
                    continue
                self.writer.write(result)
                self.written.append(result)
                imported += 1
        if failed or not imported:
            for key in keys:
                base = dict(date=self.date, host=self.host, runtime="process", system=key, version="-",
                            publish=self.publish(key), workload=plan.workload, tier=plan.tier, regime="custom", run=1)
                self.emit(base, task="conformance", item=plan.workload, status="failed",
                          note=failed or "the kit's driver wrote no rows")

    def skip(self, key: str, plan, reason: str, regime: str = "-", unsupported: bool = True):
        """A pair that doesn't run: `unsupported` when the system can't run it, else (a missing
        input) incomplete, for the answers as for the row."""
        if self.answers and plan.queries and not plan.replay:
            answered = regime if regime != "-" else plan.regimes[0]
            for query in plan.queries.glob("*.rq"):
                self.answers.skip((plan.workload, plan.tier, answered, query.name), key, reason,
                                  "unsupported" if unsupported else "incomplete", tuple(plan.all_inputs()))
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
                    # A kit runs its own systems; one that lacks the workload's capabilities is
                    # still reported as unable (below), never dropped.
                    if plan.systems is not None and key not in plan.systems and not verdict(self.systems[key], workload):
                        continue
                    # A vendor that refused us an evaluation is never measured (systems.toml refused).
                    if self.systems[key].get("refused"):
                        self.skip(key, plan, self.systems[key]["refused"])
                        continue
                    missing = verdict(self.systems[key], workload)
                    if missing:
                        self.skip(key, plan, f"lacks {missing}")
                        continue
                    if plan.kind in ("kit", "driver"):
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
                    reason = system.supports(ctx, plan.inputs_of(key), regime)
                    if reason:
                        self.skip(key, plan, reason, regime)
                        continue
                    missing = self.sif_missing(system, ctx)
                    if missing:
                        self.skip(key, plan, missing, regime, unsupported=False)
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
        # Every image the run's containers use, resolved to its ID now, before anything is timed
        # (ns.pin_images): one that can't be stops the run; none is pulled or re-resolved later.
        if self.args.runtime == "docker" and not self.args.dry_run:
            try:
                ns.pin_images(list(images), self.root)
            except ns.ImageError as error:
                self.say(f"images: {error}")
                return 1
        if self.manifest and self.args.runtime == "docker":
            self.manifest.update(images=image_ids(list(images)), pinned_images=ns.pinned(self.root))
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
        if self.manifest:
            self.manifest.update(datasets=self.data_sizes(
                [i for plan, key, system, _ in chosen if system is not None for i in plan.inputs_of(key)]),
                sources=self.data_sources())
        # Per workload tier, the repetitions are interleaved across the systems: every
        # system's first run, then every system's second, each round in an order rotated by
        # one, so that drift on the machine (background I/O, heat) falls on every system
        # alike instead of on whichever ran last (PROTOCOL.md §4).
        groups: dict[int, list] = {}
        for entry in chosen:
            groups.setdefault(id(entry[0]), []).append(entry)
        for group in groups.values():
            plan = group[0][0]
            runnable, drivers, kits = [], [], []
            for _, key, system, regime in group:
                if unavailable.get(id(plan)):
                    self.skip(key, plan, unavailable[id(plan)], regime, unsupported=False)
                elif plan.kind == "kit":
                    kits.append(key)
                elif plan.kind == "driver":
                    drivers.append(key)
                else:
                    runnable.append((key, system, regime))
            # Each cell's attempt, before any of its work: a run stopped from here on leaves the
            # cell incomplete, never an older attempt's receipt in its place (review r1 R2).
            cells = [(key, None) for key in kits + drivers] + [(key, system) for key, system, _ in runnable]
            attempts = self.attempts(plan, cells)
            for key in kits:
                self.kit(plan, key)
            if drivers:
                self.driver(plan, drivers)
            for run in range(1, self.args.runs + 1):
                shift = (run - 1) % max(1, len(runnable))
                for key, system, regime in runnable[shift:] + runnable[:shift]:
                    self.cycle(plan, key, system, regime, run)
                    if self.runtime.workers():
                        return 1
            # The tier's cells ran their whole protocol: their receipts, now, so that a run
            # stopped later keeps them (an interrupted tier's attempts stay incomplete).
            self.receipts(plan, cells, attempts)
        self.cross_check()
        self.report_skips()
        self.say(f"\nresults: {self.writer.path}")
        return 0

    def attempts(self, plan, cells: list[tuple[str, object]]) -> dict[str, dict]:
        """Each cell's attempt (receipts.py), appended to the results' attempt journal and on disk
        (manifest.append_attempts) before its work starts; if it can't be, that raises here, and the
        run stops before the work."""
        if not self.manifest or self.args.dry_run or not cells:
            return {}
        started = datetime.datetime.now().isoformat(timespec="microseconds")
        out = {receipts.cell_key(plan.workload, plan.tier, key): {"attempt": uuid.uuid4().hex, "started": started,
                                                                    "complete": False} for key, _ in cells}
        append_attempts(self.manifest.path.parent, out)
        return out

    def receipts(self, plan, cells: list[tuple[str, object]], attempts: dict[str, dict]):
        """The receipts of a tier's cells that ran their whole protocol, each under its attempt:
        what it ran, its own rows' outcome; its answers once the run has written them."""
        if not attempts:
            return
        entry = self.manifest.entry
        commit = entry["git"].get("commit") or ""
        ctx = self.context_for_images()
        pins = ns.pinned(self.root)  # the ids the containers ran (runtime.py), never a tag looked up again
        child = plan.kind in ("kit", "driver")
        out = {}
        for key, system in cells:
            cell = receipts.cell_key(plan.workload, plan.tier, key)
            rows = [receipts.csv_row(r) for r in self.written
                    if (r.workload, r.tier, r.system) == (plan.workload, plan.tier, key)]
            pair = summarise(rows).get((plan.workload, plan.tier, key))
            compiled = isinstance(system, Nrese) or plan.kind == "cycle"
            mine = receipts.unknown(f"a {plan.kind} plan's: its child's, not recorded here")
            out[cell] = {
                **attempts[cell], "complete": True, "commit": commit, "dirty": entry["git"].get("dirty"),
                "code": receipts.object_ids(self.root, commit, receipts.code_paths(self.root, commit)
                                            + (receipts.BUILD_PATHS if compiled or child else ())),
                "images": mine if child else {
                    i: {"id": pins[i], "source": self.image_source(pins[i])} if pins.get(i) else
                       {"id": receipts.unknown("not in the run's pin map"),
                        "source": receipts.unknown("no image pinned: none known to have run")}
                    for i in sorted({*system.images(ctx), *(i for i, _ in system.builds())})},
                "build": mine if child else self.build_receipt(system),
                # The settings that change a measurement; where a run keeps its files doesn't (F8).
                "settings": mine if child else {k: v for k, v in (entry.get("settings") or {}).items()
                                                if k not in OPERATIONAL_SETTINGS},
                "protocol": dict(entry.get("protocol") or {}),  # the attempt's own (F3), not the record's
                "data": mine if child else self.data_receipt(plan, key),
                "outcome": pair.outcome() if pair else {"outcome": "skipped"},
                "answers": "pending" if self.answers else "not captured"}
        self.manifest.update(cells=out, data_hash_s=round(self.hash_s, 1))

    def build_receipt(self, system) -> dict:
        """What the cell's compiled programs were: built in this run from a checkout (its state when
        the build started), or unknown (--skip-build: whatever binary was there)."""
        out = {"harness": receipts.unknown("--skip-build: the harness that was there") if self.args.skip_build
               else self.built.get("harness") or receipts.unknown("not built in this run")}
        if isinstance(system, Nrese):
            out["nrese"] = (receipts.unknown("--skip-build: the binary that was there") if self.args.skip_build
                            else self.built.get(system.key) or receipts.unknown("not built in this run"))
        return out

    def data_receipt(self, plan, key: str) -> dict:
        inputs = plan.inputs_of(key)
        out = {i: receipts.unknown("outside the datasets: not hashed") for i in inputs if not i.startswith("/data/")}
        out.update({i: digest or receipts.unknown("not there when hashed")
                    for i, digest in self.data_hashes([i for i in inputs if i.startswith("/data/")]).items()})
        if plan.external:
            out["external"] = receipts.unknown("an external driver's programs and data: not recorded")
        return out

    def answer_receipts(self):
        """The run's answers into its cells' receipts: each cell's worst verdict this run, after
        answers.json is written (Recorder: this invocation's captures only)."""
        if not self.manifest or not self.answers:
            return
        from .answers import PRECEDENCE
        counts: dict[str, dict[str, int]] = {}
        for v in self.answers.verdicts():  # one per query the cell was to answer (captured or not)
            cell = receipts.cell_key(v["workload"], str(v["tier"]), v["system"])
            counts.setdefault(cell, {}).setdefault(v["verdict"], 0)
            counts[cell][v["verdict"]] += 1

        def of(cell: str):
            # Not one worst verdict: `deviates` ranks above `incomplete` (PRECEDENCE, the reports'), so it
            # could stand for a query never captured (P2 design review F2). Qualified: every query ok or
            # deviating only across datatypes (rule.owl-rs-answer-comparison), none without its answer.
            found = counts.get(cell)
            if not found:
                return "not captured"
            return {"worst": min(found, key=PRECEDENCE.index), "verdicts": dict(sorted(found.items())),
                    "qualified": set(found) <= QUALIFIED_ANSWERS}
        cells = self.manifest.entry.get("cells") or {}
        done = {cell: {**r, "answers": of(cell)} for cell, r in cells.items() if r.get("complete")}
        if done:
            self.manifest.update(cells=done)

    def data_hashes(self, inputs: list[str]) -> dict[str, str | None]:
        """/data input -> the SHA-256 of its bytes here (None: not there), each file hashed once a run:
        nothing on the suite's path checks them against their pins (`suite.py data verify` is by hand)."""
        todo = sorted({i for i in inputs if i not in self.hashed})
        if todo:
            start = time.monotonic()
            store = Store(self.data_volume, None if self.data_volume else Path(self.data.source))
            names = {i: i[len("/data/"):] for i in todo}
            present = store.sizes(list(names.values()))
            found = store.hashes([n for n in names.values() if n in present])
            self.hashed.update({i: found.get(n) for i, n in names.items()})
            self.hash_s += time.monotonic() - start
        return {i: self.hashed[i] for i in sorted(set(inputs))}

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
        batch, its build volume and images (installed tools stay). The shared datasets stay."""
        self.runtime.cleanup()
        if self.args.dry_run and self.results.exists() and self.results == self.scratch / "dry-run-results":
            self.runtime.attempt(shutil.rmtree, self.runtime.owned_path(str(self.results)))
        if self.scratch.exists() and not self.runtime.workers() and not self.runtime.created_stores:
            self.runtime.attempt(self.scratch.rmdir)
        if self.args.dry_run:
            return
        if not self.keep and not self.runtime.workers():
            def checked(command):
                run_quiet(command).check_returncode()

            for volume in list(self.made_volumes):
                if self.runtime.attempt(DockerRuntime.remove_resource, self.runtime, "volume", volume):
                    self.made_volumes.remove(volume)
            if not self.tools_installed:
                for image in list(self.made_images):
                    if self.runtime.attempt(checked, ["docker", "image", "rm", image]):
                        self.made_images.remove(image)


def build_settings() -> dict[str, str]:
    """A build's configuration from the environment it runs in: the CPU it compiles for
    (NRESE_TARGET_CPU, scripts/lib/target-cpu.sh's; `native` unset) and the RUSTFLAGS it inherits.
    The toolchain is the checkout's (rust-toolchain.toml, in the receipt's code)."""
    return {"target_cpu": os.environ.get("NRESE_TARGET_CPU", "native"), "rustflags_env": os.environ.get("RUSTFLAGS", "")}


@contextlib.contextmanager
def held_lock(name: str):
    """The scripts this process runs see the host lock `name` as held (scripts/lib/ns.sh)."""
    key = "NS_HELD_" + re.sub(r"[^a-z0-9]", "_", name)
    os.environ[key] = "1"
    try:
        yield
    finally:
        os.environ.pop(key, None)


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
    if not args.dry_run:
        try:
            suite.manifest = Manifest(suite.results, root, argv, args, args.runtime, suite.settings)
        except ManifestError as error:  # nothing has run: its own scratch goes, the manifest stays
            suite.runtime.attempt(suite.cleanup)
            print(f"refused: {error}", file=sys.stderr)
            return 2
    atexit.register(suite.cleanup)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    code = 1
    try:
        code = suite.run()
    except KeyboardInterrupt:
        print("\ninterrupted: cleaning up", file=sys.stderr)
        code = 130
    except SystemExit as error:
        code = error.code if isinstance(error.code, int) else 1 if error.code else 0
        if code:
            raise
    finally:
        if suite.answers and suite.runtime.attempt(lambda: print(f"answers: {suite.answers.write()}")):
            suite.runtime.attempt(suite.answer_receipts)  # written: the receipts' answers are this run's
        suite.runtime.attempt(suite.cleanup)
        atexit.unregister(suite.cleanup)
        if suite.runtime.cleanup_errors and code == 0:
            code = 1
        if suite.manifest:
            def record_cleanup():
                suite.manifest.update(cleanup={"errors": suite.runtime.cleanup_errors,
                                              "residuals": {**suite.runtime.residuals(),
                                                            "scratch": str(suite.scratch) if suite.scratch.exists() else None,
                                                            "files": suite.made_files, "volumes": suite.made_volumes,
                                                            "images": suite.made_images}})

            if not suite.runtime.attempt(record_cleanup) and code == 0:
                code = 1
            if not suite.runtime.attempt(suite.manifest.finish, code) and code == 0:
                code = 1
    return code
