"""`suite.py data`: the frozen benchmark datasets (benches/datasets.toml).

Comparisons need byte-identical data: a "latest" dump that changes between two runs
changes their answers (the Wikidata lexemes of 2 October 2026). The datasets the suite's
standard tiers read are therefore kept in a reserved Docker volume (or directory) within a
budget, each with its size and SHA-256, and checked before a comparison.

    suite.py data                      the catalogue: present, size, frozen, budget
    suite.py data freeze [FILE ...]    hash present files into the catalogue (all present
                                       standard-tier inputs without FILE)
    suite.py data verify [--quick]     every catalogued file against its hash (or size)
    suite.py data files                the catalogued file names (for scripts/bench-cleanup.sh)

FILE is a path under /data (`lubm-100.nt`, `real/wikidata-lexemes.nt.gz`). Uses the volume
`nrese-bench-data` unless --data DIR.
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import subprocess
import tomllib
from pathlib import Path

from .ledger import toml_value
from .runtime import run_quiet

HEADER = """# The frozen benchmark datasets (benches/PROTOCOL.md §6; `suite.py data`). Comparisons run
# on byte-identical data: every file the standard tiers read, with its size and SHA-256, the
# command that made it and the release of its source. The reserved space holds them for
# good (budget_gb); scripts/bench-cleanup.sh never removes a catalogued file. Regenerating a
# file must give the same hash, or it is a new release: freeze it again and say so in the
# run records that use it.
"""


def load(root: Path) -> dict:
    path = root / "benches/datasets.toml"
    if not path.exists():
        return {"budget_gb": 300, "volume": "nrese-bench-data", "dataset": []}
    with open(path, "rb") as f:
        return tomllib.load(f)


def save(root: Path, catalog: dict):
    lines = [HEADER, f"budget_gb = {catalog['budget_gb']}", f"volume = {toml_value(catalog['volume'])}", ""]
    for d in sorted(catalog["dataset"], key=lambda d: d["file"]):
        lines.append("[[dataset]]")
        for key in ("file", "workload", "tier", "role", "made_by", "source", "bytes", "sha256", "frozen"):
            if d.get(key) not in (None, ""):
                lines.append(f"{key} = {toml_value(d[key])}")
        lines.append("")
    (root / "benches/datasets.toml").write_text("\n".join(lines), encoding="utf-8", newline="\n")


class Store:
    """The dataset files: in the Docker volume, or in a directory."""

    def __init__(self, volume: str | None, directory: Path | None):
        self.volume, self.directory = volume, directory

    def _sh(self, script: str, *args: str) -> str:
        out = run_quiet(["docker", "run", "--rm", "-v", f"{self.volume}:/data:ro", "alpine", "sh", "-c", script,
                         "sh", *args])
        return out.stdout

    def sizes(self, files: list[str]) -> dict[str, int]:
        if not files:
            return {}
        if self.directory:
            return {f: (self.directory / f).stat().st_size for f in files if (self.directory / f).is_file()}
        out = self._sh('cd /data && for f in "$@"; do [ -f "$f" ] && stat -c "%s %n" "$f"; done', *files)
        return {name: int(size) for size, name in (line.split(" ", 1) for line in out.splitlines() if " " in line)}

    def hashes(self, files: list[str]) -> dict[str, str]:
        if not files:
            return {}
        if self.directory:
            result = {}
            for f in files:
                h = hashlib.sha256()
                with open(self.directory / f, "rb") as fh:
                    for chunk in iter(lambda: fh.read(1 << 22), b""):
                        h.update(chunk)
                result[f] = h.hexdigest()
            return result
        out = self._sh('cd /data && sha256sum "$@"', *files)
        return {name.strip().lstrip("*"): digest for digest, name in
                (line.split(None, 1) for line in out.splitlines() if line.strip())}

    def listing(self) -> dict[str, int]:
        if self.directory:
            return {p.relative_to(self.directory).as_posix(): p.stat().st_size
                    for p in self.directory.rglob("*") if p.is_file()}
        out = self._sh('cd /data && find . -type f -exec stat -c "%s %n" {} +')
        return {name[2:]: int(size) for size, name in (line.split(" ", 1) for line in out.splitlines() if " " in line)}


def gib(n: int) -> str:
    return f"{n / 2**30:,.1f}"


def standard_inputs(root: Path, workloads: dict) -> dict[str, tuple[str, str, list[str] | None]]:
    """/data file -> (workload, tier, prepare command) for every standard tier the driver
    defines."""
    from .workloads import DEFINITIONS, plans
    out = {}
    for name, workload in workloads.items():
        if name not in DEFINITIONS:
            continue
        tiers = [str(t) for t in workload.get("tiers", [])] or DEFINITIONS[name][1]
        for plan in plans(root, name, tiers, {}) or []:
            for i in plan.inputs:
                if i.startswith("/data/"):
                    out.setdefault(i[len("/data/"):], (name, plan.tier, plan.prepare))
    return out


def main(argv: list[str], workloads: dict, root: Path) -> int:
    p = argparse.ArgumentParser(prog="suite.py data", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("command", nargs="?", default="list", choices=["list", "freeze", "verify", "files"])
    p.add_argument("files", nargs="*")
    p.add_argument("--data", type=Path, help="a directory instead of the Docker volume")
    p.add_argument("--quick", action="store_true", help="verify sizes only")
    args = p.parse_args(argv)
    catalog = load(root)
    store = Store(None if args.data else catalog["volume"], args.data)
    by_file = {d["file"]: d for d in catalog["dataset"]}
    if args.command == "files":
        print("\n".join(sorted(by_file)))
        return 0
    if args.command == "freeze":
        standard = standard_inputs(root, workloads)
        present = store.sizes(sorted(set(args.files) or set(standard) | set(by_file)))
        if not present:
            print("nothing to freeze: no file present")
            return 1
        print(f"hashing {len(present)} files, {gib(sum(present.values()))} GiB ...", flush=True)
        hashes = store.hashes(sorted(present))
        sources = {}
        listing = store.listing()
        for f in listing:
            if f.endswith(".source"):
                text = store._sh('cat "/data/$1"', f) if not args.data else (args.data / f).read_text()
                sources[f[:-len(".source")]] = text.strip()
        today = datetime.date.today().isoformat()
        for f, size in present.items():
            entry = by_file.setdefault(f, {"file": f})
            if f in standard:
                entry["workload"], entry["tier"] = standard[f][0], standard[f][1]
                entry["made_by"] = " ".join(standard[f][2] or []) or "the driver (generated per run)"
                entry["role"] = "core"
            entry.setdefault("role", "source" if f.startswith("real/") else "core")
            if f in sources:
                entry["source"] = sources[f]
            if entry.get("sha256") != hashes.get(f):
                entry["frozen"] = today
            entry["bytes"], entry["sha256"] = size, hashes.get(f, "")
        catalog["dataset"] = list(by_file.values())
        save(root, catalog)
        print(f"{len(present)} files frozen into benches/datasets.toml")
        return 0
    if args.command == "verify":
        files = sorted(by_file)
        sizes = store.sizes(files)
        hashes = {} if args.quick else store.hashes([f for f in files if f in sizes])
        bad = 0
        for f in files:
            d = by_file[f]
            if f not in sizes:
                state = "missing"
            elif sizes[f] != d.get("bytes"):
                state = f"size {sizes[f]} != {d.get('bytes')}"
            elif not args.quick and hashes.get(f) != d.get("sha256"):
                state = "hash differs"
            else:
                continue
            bad += 1
            print(f"{f}: {state}")
        print(f"{len(files) - bad} of {len(files)} catalogued files {'present with their size' if args.quick else 'verified'}")
        return 1 if bad else 0
    # list
    listing = store.listing()
    standard = standard_inputs(root, workloads)
    total = sum(listing.values())
    catalogued = sum(listing.get(f, 0) for f in by_file)
    print(f"budget {catalog['budget_gb']} GB; volume {catalog['volume']}: {gib(total)} GiB in {len(listing)} files, "
          f"{gib(catalogued)} GiB catalogued\n")
    print(f"{'file':44} {'GiB':>7}  {'state':10} workload/tier")
    for f in sorted(set(listing) | set(by_file) | set(standard)):
        d = by_file.get(f)
        state = ("frozen" if d and listing.get(f) == d.get("bytes") else "CHANGED" if d and f in listing
                 else "missing" if f not in listing else "not frozen")
        where = d and f"{d.get('workload', '')} {d.get('tier', '')}" or (
            f"{standard[f][0]} {standard[f][1]}" if f in standard else "")
        print(f"{f:44} {gib(listing.get(f, 0)):>7}  {state:10} {where}")
    return 0
