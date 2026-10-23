"""What the breadth benchmarks share (benches/breadth/README.md): where their sources, data and
results live, how NRESE is built for their containers, how an engine image is built from a
pinned upstream checkout, the interleaved plan, and the result rows (the suite's one schema,
benches/suite/suitekit/schema.py).

The work root (NRESE_BREADTH, default ~/nrese-breadth) holds, outside git:
  src/<owner>_<repo>/   upstream checkouts, each at the commit its benchmark pins
  data/<benchmark>/     downloads and the files made from them (frozen: benches/datasets.toml)
  results/<run>/        results.csv, manifest.json, logs, the answers the checks read
  scratch/              per-step scratch, removed after each step
"""
from __future__ import annotations

import hashlib
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "benches" / "suite"))

from suitekit import manifest, ns  # noqa: E402
from suitekit.runtime import DockerRuntime, Mount, Spec  # noqa: E402,F401
from suitekit.schema import Result, Writer  # noqa: E402,F401

WORK = Path(os.environ.get("NRESE_BREADTH") or Path.home() / "nrese-breadth")
# The datasets are shared by every job on the host; the rest is this job's own (suitekit/ns.py):
# upstream sources a job patches and builds images from, its binaries, results and scratch.
DATA = WORK / "data"
SRC, RESULTS, SCRATCH = (ns.directory(WORK, d, ROOT) for d in ("src", "results", "scratch"))
MIRROR = WORK / "mirror"
# NRESE's binaries for the containers: built in the pinned Rust image into this checkout's own
# target volume, then copied out with their SHA-256 (the fast suite's build, benches/fast/fast.py).
TARGET_VOLUME = ns.name("target", ROOT)
CARGO_VOLUME = ns.shared("cargo")
OWNER = "breadth"
BIN = ns.directory(WORK, "bin", ROOT)


def run(command: list[str], **kw) -> subprocess.CompletedProcess:
    print("+", " ".join(command), flush=True)
    return subprocess.run(command, check=True, **kw)


def rust_image() -> str:
    channel = re.search(r'channel = "(.*)"', (ROOT / "rust-toolchain.toml").read_text()).group(1)
    return os.environ.get("RUST_IMAGE", f"rust:{channel}-bookworm")


def wait_for_quiet_slot():
    """Waits while another process holds the host's quiet slot (scripts/quiet-slot.sh): a
    build inside a container doesn't see the slot, which cargo-guarded.sh honours on the host."""
    lock = Path(os.environ.get("NRESE_QUIET_DIR") or Path.home() / ".nrese-quiet-slot")
    said = False
    # At most NRESE_QUIET_WAIT_S, as cargo-guarded.sh waits.
    deadline = time.monotonic() + float(os.environ.get("NRESE_QUIET_WAIT_S", "1800"))
    while lock.is_dir() and time.monotonic() < deadline:
        try:
            owner = int((lock / "pid").read_text().strip())
            os.kill(owner, 0)
        except (OSError, ValueError):
            break  # its owner died; quiet-slot.sh takes such a lock over
        if owner in (os.getpid(), os.getppid()):
            break
        if not said:
            print(f"a measurement holds the quiet slot ({lock}): the build waits", flush=True)
            said = True
        time.sleep(10)


RETREAT = Path(os.environ.get("NRESE_RETREAT_FILE") or Path.home() / "owlrs" / "RETREAT")


def wait_until_free():
    """Waits while the host's owners use it (the office PC's guard writes RETREAT and stops
    our containers then): no step starts during a retreat."""
    said = False
    while RETREAT.exists():
        if not said:
            print(f"the host is in use by its owners ({RETREAT}): waiting", flush=True)
            said = True
        time.sleep(60)


def retreated_since(started: float) -> bool:
    """Whether a retreat began after `started` (time.time()): the step it overlapped was
    stopped or disturbed and runs again."""
    try:
        return RETREAT.stat().st_mtime >= started or RETREAT.exists()
    except FileNotFoundError:
        return False


def build_nrese(targets: list[list[str]]) -> dict[str, str]:
    """Builds `targets` (cargo package/example arguments) in the pinned Rust image and copies
    the binaries to this job's BIN; returns name -> SHA-256."""
    jobs = os.environ.get("CARGO_BUILD_JOBS", "8")
    cap = os.environ.get("NRESE_MEMORY_CAP_GB", "12")
    run([ns.executable(), *ns.volume("create", TARGET_VOLUME, ROOT, OWNER)], capture_output=True)
    for packages in targets:
        wait_for_quiet_slot()
        run(["docker", "run", "--rm", f"--memory={cap}g", f"--memory-swap={cap}g", f"--cpus={jobs}",
             "-v", f"{ROOT}:/src:ro", "-v", f"{TARGET_VOLUME}:/target",
             "-v", f"{CARGO_VOLUME}:/usr/local/cargo/registry", "-e", "CARGO_TARGET_DIR=/target",
             "-e", f"CARGO_BUILD_JOBS={jobs}", "-e", f"NRESE_MEMORY_CAP_GB={cap}",
             # Portable code: the image may run on another measure host (benches/hosts.toml).
             "-e", "NRESE_TARGET_CPU=x86-64-v3", "-w", "/src", rust_image(),
             "bash", "scripts/cargo-guarded.sh", "build", "--release", "--locked", "--quiet", *packages])
    BIN.mkdir(parents=True, exist_ok=True)
    wanted = {"perf_lab": "release/examples/perf_lab", "nrese-server": "release/nrese-server",
              "nrese-bench-harness": "release/nrese-bench-harness"}
    made = {}
    for name, path in wanted.items():
        if not any(name in " ".join(p) for p in targets):
            continue
        run(["docker", "run", "--rm", "-v", f"{TARGET_VOLUME}:/target:ro", "-v", f"{BIN}:/out",
             "debian:bookworm-slim", "cp", f"/target/{path}", f"/out/{name}"])
        made[name] = sha256(BIN / name)
    return made


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def checkout(repo: str, commit: str) -> Path:
    """This job's checkout of upstream `owner/repo` at `commit` (a job patches it and builds from
    it), cloned from the host's mirror of the repository, which is made once: cloned aside and
    renamed into place only if no other job did it first."""
    mirror = MIRROR / (repo.replace("/", "_") + ".git")
    if not mirror.exists():
        MIRROR.mkdir(parents=True, exist_ok=True)
        aside = mirror.with_name(f"{mirror.name}.{ns.name('part', ROOT)}")
        shutil.rmtree(aside, ignore_errors=True)
        run(["git", "clone", "-q", "--mirror", f"https://github.com/{repo}", str(aside)])
        try:
            aside.rename(mirror)
        except OSError:
            shutil.rmtree(aside, ignore_errors=True)  # another job's mirror came first
    if subprocess.run(["git", "-C", str(mirror), "cat-file", "-e", f"{commit}^{{commit}}"],
                      capture_output=True).returncode != 0:
        run(["git", "-C", str(mirror), "fetch", "-q", "origin"], capture_output=True)
    path = SRC / repo.replace("/", "_")
    if not path.exists():
        run(["git", "clone", "-q", "--no-checkout", str(mirror), str(path)])
    run(["git", "-C", str(path), "checkout", "-q", "--detach", "--force", commit])
    return path


def image(tag: str, dockerfile: Path, context: Path, args: dict[str, str] | None = None) -> str:
    """Builds `tag` from `dockerfile` over `context` (an upstream directory or ours)."""
    wait_for_quiet_slot()
    command = ["docker", "build", "-q", "-t", tag, "-f", str(dockerfile)]
    for key, value in (args or {}).items():
        command += ["--build-arg", f"{key}={value}"]
    run(command + [str(context)], capture_output=True)
    return tag


def interleaved(pairs: list, runs: int) -> list[tuple[int, object]]:
    """Every pair's first repetition, then every pair's second, each round rotated by one
    (benches/PROTOCOL.md §4)."""
    order, plan = list(pairs), []
    for rep in range(1, runs + 1):
        if rep > 1:
            order = order[1:] + order[:1]
        plan += [(rep, p) for p in order]
    return plan


def fleet_container_step(spec: Spec, case: dict) -> tuple[list[str], dict]:
    """Bind the canonical create argv in Fleet's typed per-step Docker plan.

    Fleet resolves its three placeholders once. The driver receives the resolved
    argv in the agent-written request; successful completion requires consumption.
    """
    import json
    argv = DockerRuntime(SCRATCH, port=0).args(spec, detached=False, bound=True)
    step = ["python3", "-B", "benches/breadth/era-shacl/run.py", "consume", "--case",
            json.dumps(case, separators=(",", ":"))]
    return step, dict(cores=1, ram_gb=16, docker=dict(ram_gb=12, argv=argv))


def stamp(results: Path, argv: list[str], images: list[str], datasets: list[Path], protocol: dict) -> dict:
    """The run's manifest entry (benches/suite/suitekit/manifest.py's fields)."""
    return {
        "started": manifest.now(), "argv": argv, "protocol": protocol, "git": manifest.git(ROOT),
        "machine": manifest.machine("docker", ROOT), "images": manifest.image_ids(images),
        "datasets": {str(p): p.stat().st_size for p in datasets if p.exists()},
        "binaries": {p.name: sha256(p) for p in sorted(BIN.glob("*")) if p.is_file()},
    }
