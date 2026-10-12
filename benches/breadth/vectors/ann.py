#!/usr/bin/env python3
"""ann-benchmarks (github.com/erikbern/ann-benchmarks) with NRESE among its algorithms, each
in its own container as upstream runs them (benches/breadth/README.md, "Vector search").

    ann.py setup                       the checkout at COMMIT, NRESE's module and server,
                                       a virtual environment for upstream's driver, the datasets
    ann.py images [--algorithms ...]   upstream's images (base first), versions pinned below
    ann.py run --dataset sift-128-euclidean [--algorithms nrese,qdrant,pgvector,hnswlib]
               [--runs 5] [--batch]
    ann.py export [--label NAME]       upstream's metrics to RESULTS/<label>/: its CSV, the
                                       suite's rows, and QPS at recall 0.90/0.95/0.99 per system

Upstream's protocol stands: k = 10, each container on one CPU (batch mode: all), every
query argument of the algorithm's config, the best of --runs passes. Upstream's pgvector
image builds pgvector's unpinned master and its Qdrant module pins 1.5.1 (2023); here both
are pinned to the releases recorded in VERSIONS, and the version used is in each row.
"""
from __future__ import annotations

import argparse
import csv
import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import kit  # noqa: E402

REPO, COMMIT = "erikbern/ann-benchmarks", "2e081ad32c1eccab72dcb739ad886c310b90f715"
VENV = kit.WORK / "venv-ann"
DATA = kit.DATA / "ann"
# Build arguments that pin what upstream leaves floating or old (docker build --build-arg).
VERSIONS = {"qdrant": {"QDRANT_VERSION": "1.19.2", "QDRANT_CLIENT_VERSION": "1.19.1"}}
# pgvector's image clones master; the release instead.
PGVECTOR = "v0.8.7"
SYSTEM = {"nrese": "nrese", "qdrant": "qdrant", "pgvector": "pgvector", "milvus-hnsw": "milvus",
          "milvus-flat": "milvus", "milvus-ivfflat": "milvus", "milvus-ivfsq8": "milvus", "milvus-ivfpq": "milvus"}


def checkout() -> Path:
    src = kit.checkout(REPO, COMMIT)
    target = src / "ann_benchmarks/algorithms/nrese"
    shutil.rmtree(target, ignore_errors=True)
    shutil.copytree(HERE / "ann-benchmarks/nrese", target)
    dockerfile = src / "ann_benchmarks/algorithms/pgvector/Dockerfile"
    text = dockerfile.read_text(encoding="utf-8")
    pinned = text.replace("git clone https://github.com/pgvector/pgvector /tmp/pgvector",
                          f"git clone --branch {PGVECTOR} --depth 1 https://github.com/pgvector/pgvector /tmp/pgvector")
    if pinned != text:
        dockerfile.write_text(pinned, encoding="utf-8")
    return src


def python() -> str:
    return str(VENV / "bin/python")


def setup(args) -> int:
    src = checkout()
    if not VENV.exists():
        kit.run([sys.executable, "-m", "venv", str(VENV)])
        kit.run([python(), "-m", "pip", "install", "-q", "-r", str(src / "requirements.txt")])
    (src / "data").mkdir(exist_ok=True)
    for f in DATA.glob("*.hdf5"):
        link = src / "data" / f.name
        if not link.exists():
            link.symlink_to(f)
    made = kit.build_nrese([["-p", "nrese-server"]])
    shutil.copy(kit.BIN / "nrese-server", src / "ann_benchmarks/algorithms/nrese/nrese-server")
    print(f"nrese-server {made['nrese-server']}")
    return 0


def images(args) -> int:
    src = checkout()
    shutil.copy(kit.BIN / "nrese-server", src / "ann_benchmarks/algorithms/nrese/nrese-server")
    kit.wait_for_quiet_slot()
    kit.run(["docker", "build", "-q", "-t", "ann-benchmarks", "-f", "ann_benchmarks/algorithms/base/Dockerfile", "."],
            cwd=src)
    for algorithm in args.algorithms.split(","):
        command = ["docker", "build", "-q", "-t", f"ann-benchmarks-{algorithm}",
                   "-f", f"ann_benchmarks/algorithms/{algorithm}/Dockerfile"]
        for key, value in VERSIONS.get(algorithm, {}).items():
            command += ["--build-arg", f"{key}={value}"]
        kit.wait_for_quiet_slot()
        kit.run(command + ["."], cwd=src)
    return 0


def run(args) -> int:
    src = checkout()
    for algorithm in args.algorithms.split(","):
        kit.wait_until_free()
        command = [python(), "run.py", "--dataset", args.dataset, "--algorithm", algorithm, "--runs", str(args.runs),
                   "--timeout", str(args.timeout_s)]
        if args.batch:
            command.append("--batch")
        if args.force:
            command.append("--force")
        kit.run(command, cwd=src)
    return 0


def frontier(points: list[tuple[float, float]], target: float) -> float | None:
    """The highest QPS among the configurations whose recall reaches `target`."""
    reached = [qps for recall, qps in points if recall >= target]
    return max(reached) if reached else None


def export(args) -> int:
    src = checkout()
    out = kit.RESULTS / args.label
    out.mkdir(parents=True, exist_ok=True)
    table = out / "ann-benchmarks.csv"
    kit.run([python(), "data_export.py", "--output", str(table)], cwd=src)
    rows = list(csv.DictReader(open(table, encoding="utf-8")))
    writer = kit.Writer(out / "results.csv")
    git = kit.manifest.git(kit.ROOT)
    host = kit.manifest.machine("docker").get("host", "")
    by: dict[tuple, list] = {}
    for r in rows:
        recall, qps = float(r["k-nn"]), float(r["qps"])
        by.setdefault((r["dataset"], r["algorithm"]), []).append((recall, qps))
        writer.write(kit.Result(
            date=kit.manifest.now()[:16], host=host, runtime="docker", system=SYSTEM.get(r["algorithm"], r["algorithm"]),
            version=r.get("library", "-"), publish="free", workload="vector-ann", tier=r["dataset"], regime="none",
            run=1, task="query", item=r["parameters"], repeat=1, status="ok", ms=1000.0 / qps if qps else "",
            rows=10, note=f"recall@10 {recall:.4f}; {qps:.1f} QPS; build {float(r.get('build', 0)):.1f} s; "
                          f"index {float(r.get('indexsize', 0)) / 1024:.0f} MiB; commit {git.get('commit', '')[:10]}"))
    print(f"\n| dataset | algorithm | QPS at recall 0.90 | 0.95 | 0.99 |\n|---|---|---:|---:|---:|")
    for (dataset, algorithm), points in sorted(by.items()):
        cells = [frontier(points, t) for t in (0.90, 0.95, 0.99)]
        print(f"| {dataset} | {algorithm} | " + " | ".join("-" if c is None else f"{c:,.0f}" for c in cells) + " |")
    return 0


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    sub.add_parser("setup")
    i = sub.add_parser("images")
    i.add_argument("--algorithms", default="nrese,qdrant,pgvector,hnswlib,milvus")
    r = sub.add_parser("run")
    r.add_argument("--dataset", required=True)
    r.add_argument("--algorithms", default="nrese,qdrant,pgvector,hnswlib")
    r.add_argument("--runs", type=int, default=5)
    r.add_argument("--timeout-s", type=int, default=2 * 3600)
    r.add_argument("--batch", action="store_true")
    r.add_argument("--force", action="store_true")
    e = sub.add_parser("export")
    e.add_argument("--label", default="vector-ann")
    args = p.parse_args(argv)
    return {"setup": setup, "images": images, "run": run, "export": export}[args.command](args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
