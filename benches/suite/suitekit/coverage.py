"""`suite.py coverage`: which catalogue cells have a current receipt (Harness notes
2026-11-08-coverage-refresh/p2-design-r5.md, addenda r6 and r7).

    suite.py coverage [--systems S,..] [--workloads W,..] [--engine REV] [--json]

A cell is a workload's standard tier on one system. It is current only if its one newest receipt
(receipts.py) is a complete attempt whose every field a positive rule accepts against what this clean
checkout's owners declare for that receipt's own runtime. Anything missing, unknown, unordered or
unrecognised makes it not current, with its reason; an owner that can't be read is never an empty
inventory. Read-only: it prints. Exit 0; 2 when it refuses (a dirty checkout, an owner every cell
depends on that can't be read), and then it classifies nothing.

Classes, the first that applies: excluded (refused, or a build variant), cannot-run, no-result,
restricted-presence (a licensed system: presence only), unqualified (the evidence can't show the
cell's definition ran), not-ok, stale (a readable identity that isn't this checkout's), current.
"""
from __future__ import annotations

import argparse
import datetime
import json
import re
import subprocess
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path

from . import driver, receipts
from .adapters import adapter, declared_context
from .datasets import load as load_datasets
from .dockerfile import declared_source_commit
from .ledger import RESET_BASE, load_records
from .manifest import git as checkout_state
from .runtime import KINDS
from .status import runnable, standard_tiers
from .summary import OUTCOMES
from .workloads import plans

CLASSES = ("current", "stale", "not-ok", "unqualified", "restricted-presence", "no-result", "cannot-run", "excluded")
HEX40 = re.compile(r"[0-9a-f]{40}")
HEX64 = re.compile(r"[0-9a-f]{64}")
INPUT = re.compile(r"no files|sha256:[0-9a-f]{64}")
IMAGE_ID = re.compile(r"sha256:[0-9a-f]{64}")
BUILT = re.compile(r"built from benches/([^@\s]+)@([0-9a-f]{40})(?:, source commit (\S+))?")
PULLED = re.compile(r"pulled, pinned (sha256:[0-9a-f]{64})")
RECEIPT = {"attempt", "started", "complete", "commit", "dirty", "code", "images", "build", "settings", "protocol",
           "data", "outcome", "answers"}
BUILD = {"commit", "dirty", "sources", "inputs", "toolchain", "controlled"}  # with build_settings({})'s keys
CONTAINER = {"rust_image", "rust_image_id", "rustflags"}
TOOLCHAIN = {"rustc", "cargo", "host"}


class GlobalReadError(Exception):
    """An owner every cell depends on can't be read: the command refuses and classifies nothing."""


class Unreadable(Exception):
    """A per-cell owner read failed: the cell is unqualified, with the owner named."""


def owned(owner: str, read, *args):
    """A per-cell owner read: its value, or Unreadable naming the owner; never a default. Global
    failures (GlobalReadError, a git process that failed to run) pass through to the command."""
    try:
        return read(*args)
    except (GlobalReadError, Unreadable, subprocess.SubprocessError):
        raise
    except Exception as error:
        raise Unreadable(f"could not read {owner}: {error}") from error


def is_unknown(value) -> bool:
    return isinstance(value, dict) and set(value) == {"unknown"}


class Owners:
    """This clean checkout's owners, each read once: what every cell is judged against."""

    def __init__(self, root: Path, engine: str | None = None, reset_base: str | None = None):
        self.root = root
        state = checkout_state(root)
        if state.get("dirty") is None or not HEX40.fullmatch(state.get("commit") or ""):
            raise GlobalReadError(f"the checkout's commit or state can't be read ({state})")
        if state["dirty"]:
            raise GlobalReadError(f"the checkout is dirty ({state['dirty']} changed paths): coverage judges a commit")
        self.head = state["commit"]
        self.reset_base = reset_base or RESET_BASE
        self.ids: dict[tuple[str, str], object] = {}
        self.ancestry: dict[str, bool | None] = {}
        self.engine = self.git_commit(engine) if engine else None
        self.settings = global_read("driver.build_settings({})", driver.build_settings, {})
        self.pins = global_read("benches/suite/images.toml", driver.image_pins, root)
        datasets = global_read("benches/datasets.toml", load_datasets, root)
        self.datasets = {d["file"]: d for d in datasets.get("dataset", []) if isinstance(d, dict) and "file" in d}
        programs = set(receipts.BUILD_SOURCES)
        if not self.settings or programs != {"nrese", "harness"} or set(receipts.BUILD_INPUTS) != programs \
                or not all(receipts.BUILD_SOURCES.values()):
            raise GlobalReadError("receipts.BUILD_SOURCES, BUILD_INPUTS or driver.build_settings({}) declare no "
                                  "build for nrese and harness")
        for program in programs:
            if set(driver.expected_build_inputs(program)) != set(receipts.BUILD_INPUTS[program]):
                raise GlobalReadError(f"driver.expected_build_inputs({program!r}) disagrees with receipts.BUILD_INPUTS")

    def git_commit(self, revision: str) -> str:
        done = subprocess.run(["git", "-C", str(self.root), "rev-parse", "--verify", "--quiet", f"{revision}^{{commit}}"],
                              capture_output=True, text=True, timeout=60)
        if done.returncode != 0 or not HEX40.fullmatch(done.stdout.strip()):
            raise GlobalReadError(f"--engine {revision}: no such commit here")
        return done.stdout.strip()

    def object_id(self, commit: str, path: str):
        """`path`'s git id at `commit`, or unknown (receipts.object_ids, the producer's reader)."""
        if (commit, path) not in self.ids:
            self.ids[(commit, path)] = receipts.object_ids(self.root, commit, (path,))[path]
        return self.ids[(commit, path)]

    def after_reset(self, commit: str) -> bool | None:
        """Whether `commit` descends from the reset base; None where git can't tell (not in this clone)."""
        if commit not in self.ancestry:
            done = subprocess.run(["git", "-C", str(self.root), "merge-base", "--is-ancestor", self.reset_base, commit],
                                  capture_output=True, timeout=60)
            self.ancestry[commit] = {0: True, 1: False}.get(done.returncode)
        return self.ancestry[commit]

    def data_pin(self, path: str):
        """The SHA-256 datasets.toml pins `path` (a /data input) to, or unknown."""
        if path == "external":
            return receipts.unknown("an external driver's programs and data are not recorded")
        if not path.startswith("/data/"):
            return receipts.unknown("outside the datasets: not pinned")
        entry = self.datasets.get(path[len("/data/"):])
        if entry is None:
            return receipts.unknown("not in datasets.toml")
        sha = entry.get("sha256")
        return sha if isinstance(sha, str) else receipts.unknown("pinned by form only (rdfc_sha256), not by its bytes")


def global_read(owner: str, read, *args):
    try:
        return read(*args)
    except Exception as error:
        raise GlobalReadError(f"{owner}: {error}") from error


@dataclass
class Cell:
    workload: str
    tier: str
    system: str
    klass: str
    reasons: list[str] = field(default_factory=list)
    record: str | None = None
    attempt: str | None = None
    started: str | None = None


def started_at(receipt) -> datetime.datetime | None:
    """A receipt's start (the producer's naive local isoformat), or None where it can't be ordered."""
    if not isinstance(receipt, dict) or is_unknown(receipt) or not isinstance(receipt.get("started"), str):
        return None
    try:
        when = datetime.datetime.fromisoformat(receipt["started"])
    except ValueError:
        return None
    return when if when.tzinfo is None else None


def evidence(pairs: list[tuple[str, dict]]):
    """(record id, receipt, verdict) of a cell's record pairs: its one newest receipt, verdict None;
    else no receipt and the verdict (class, reason). Evidence that can't be ordered stands for
    nothing, and no older pass stands beside it (r3, F7)."""
    if not pairs:
        return None, None, ("no-result", "never run")
    held = [(rid, pair["receipt"]) for rid, pair in pairs if "receipt" in pair]
    if not held:
        if all(pair.get("outcome") == "skipped" for _, pair in pairs):
            return None, None, ("no-result", "skips only")
        return None, None, ("unqualified", "no receipt: recorded before receipts")
    times = [started_at(r) for _, r in held]
    if None in times:
        return None, None, ("unqualified", "evidence that can't be ordered: a receipt without a readable start")
    newest = max(times)
    top = [(rid, r) for (rid, r), t in zip(held, times) if t == newest]
    if any(r != top[0][1] for _, r in top):
        return None, None, ("unqualified", f"evidence that can't be ordered: {len(top)} receipts start at {newest}")
    return top[0][0], top[0][1], None


def identity(name: str, got, want: dict, shape: re.Pattern, bad: list, stale: list):
    """An identity map (path or input -> id) against the owner's: an unknown on either side is
    unqualified, a readable difference (key set or value) stale."""
    if is_unknown(got):
        bad.append(f"{name}: {got['unknown']}")
        return
    if not isinstance(got, dict):
        bad.append(f"unrecognised: {name}")
        return
    if set(got) != set(want):
        added, removed = sorted(set(got) - set(want)), sorted(set(want) - set(got))
        stale.append(f"{name}: definition changed: +{added} -{removed}")
    for path in sorted(set(got) & set(want)):
        g, w = got[path], want[path]
        if is_unknown(w):
            bad.append(f"{name} {path}: nothing to compare with: {w['unknown']}")
        elif is_unknown(g):
            bad.append(f"{name} {path}: {g['unknown']}")
        elif not (isinstance(g, str) and shape.fullmatch(g)):
            bad.append(f"unrecognised: {name} {path}")
        elif g != w:
            stale.append(f"{name}: {path} changed")


def judge(owners: Owners, workload: str, tier: str, key: str, receipt) -> tuple[str, list[str]]:
    """A receipt's class for the cell (workload, tier, key) and its reasons: unqualified, not-ok and
    stale in that order, so a cell is never stale for what it can't prove."""
    bad: list[str] = []
    failing: list[str] = []
    stale: list[str] = []
    try:
        fields(owners, workload, tier, key, receipt, bad, failing, stale)
    except Unreadable as error:
        return "unqualified", [str(error)]
    for klass, reasons in (("unqualified", bad), ("not-ok", failing), ("stale", stale)):
        if reasons:
            return klass, reasons
    return "current", []


def fields(owners, workload, tier, key, receipt, bad, failing, stale):
    if is_unknown(receipt):
        bad.append(f"receipt unknown: {receipt['unknown']}")
        return
    if not isinstance(receipt, dict) or receipt.get("complete") is not True:
        bad.append("attempt incomplete" if isinstance(receipt, dict) and receipt.get("complete") is False
                   else "unrecognised: complete")
        return
    if set(receipt) != RECEIPT:
        bad.append(f"unrecognised: receipt fields {sorted(set(receipt) ^ RECEIPT)}")
        return
    system = owned("adapters.adapter", adapter, key)
    if system is None:
        bad.append(f"no suite adapter for {key}: its kit's evidence isn't recorded")
        return
    found = owned("workloads.plans", plans, owners.root, workload, [tier], {})
    if not found:
        bad.append(f"no suite plan for {workload}")
        return
    plan = found[0]
    if plan.unavailable:
        bad.append(f"plan unavailable here: {plan.unavailable}")
        return
    protocol = receipt["protocol"]
    if not isinstance(protocol, dict):
        bad.append("unrecognised: protocol")
        return
    if "runtime" not in protocol:
        bad.append("no runtime")
        return
    kind = protocol["runtime"]
    if kind not in KINDS:
        bad.append(f"unrecognised: runtime {kind!r}")
        return
    check_protocol(protocol, system, bad)
    ctx = declared_context(owners.root, kind)

    commit = receipt["commit"]
    if is_unknown(commit):
        bad.append(f"commit: {commit['unknown']}")
    elif not (isinstance(commit, str) and HEX40.fullmatch(commit)):
        bad.append("unrecognised: commit")
    if type(receipt["dirty"]) is not int or receipt["dirty"]:
        bad.append(f"run from a dirty checkout ({receipt['dirty']})")
    paths = owned("driver.receipt_code_paths", driver.receipt_code_paths, owners.root, owners.head, system, plan)
    identity("code", receipt["code"], {p: owners.object_id(owners.head, p) for p in paths}, HEX40, bad, stale)
    images = check_images(owners, system, ctx, receipt["images"], bad, stale)
    check_build(owners, system, kind, ctx, receipt["build"], images, bad, stale)
    settings = receipt["settings"]
    if is_unknown(settings):
        bad.append(f"settings: {settings['unknown']}")
    elif not isinstance(settings, dict):
        bad.append("unrecognised: settings")
    elif settings:
        bad.append(f"settings set: {', '.join(sorted(settings))} (only the defaults are the standard cell)")
    inputs = owned("Plan.inputs_of", plan.inputs_of, key) + (["external"] if plan.external else [])
    identity("data", receipt["data"], {i: owners.data_pin(i) for i in inputs}, HEX64, bad, stale)

    outcome = receipt["outcome"]
    verdict = outcome.get("outcome") if isinstance(outcome, dict) else None
    if verdict not in OUTCOMES:
        bad.append(f"unrecognised: outcome {verdict!r}")
    elif verdict != "ok":
        failing.append(f"outcome {verdict}" + (f": {outcome['note']}" if outcome.get("note") else ""))
    answers = receipt["answers"]
    if answers in ("pending", "not captured"):
        bad.append(f"answers {answers}")
    elif isinstance(answers, dict) and set(answers) == {"worst", "verdicts", "qualified"} \
            and isinstance(answers["qualified"], bool):
        if not answers["qualified"]:
            bad.append(f"answers not qualified: {answers['verdicts']}")
    else:
        bad.append("unrecognised: answers")


def check_protocol(protocol: dict, system, bad: list):
    """The standard cell's protocol (PROTOCOL.md §4): three runs and three measured query runs,
    shuffled, and both cache modes for a system with a result cache."""
    runs, query_runs, cache = protocol.get("runs"), protocol.get("query_runs"), protocol.get("cache")
    if not all(type(n) is int for n in (runs, query_runs)) or not isinstance(cache, list) \
            or not all(m in ("off", "on") for m in cache) or protocol.get("order") not in ("shuffled", "fixed"):
        bad.append("unrecognised: protocol")
        return
    if runs < 3 or query_runs < 3:
        bad.append(f"protocol short of the standard: {runs} runs, {query_runs} query runs (PROTOCOL.md §4)")
    if protocol["order"] != "shuffled":
        bad.append("protocol: query order fixed, the standard shuffles")
    if system.result_cache and set(cache) != {"off", "on"}:
        bad.append(f"protocol: cache {','.join(cache) or 'none'} for a system with a result cache; the standard "
                   "runs off and on")


def check_images(owners, system, ctx, got, bad, stale) -> dict:
    """Each image the cell's adapter declares for its runtime, by its provenance (r3): built here
    from its context, built by images.sh at its Dockerfile's declared commit, or pulled at its pin."""
    built = dict(owned(f"{system.key}.builds", system.builds))
    scripted = {tag: (context, arg) for tag, context, arg in owned(f"{system.key}.script_builds",
                                                                    system.script_builds, ctx)}
    expected = set(owned(f"{system.key}.images", system.images, ctx)) | set(built)
    if is_unknown(got):
        bad.append(f"images: {got['unknown']}")
        return {}
    if not isinstance(got, dict):
        bad.append("unrecognised: images")
        return {}
    if set(got) != expected:
        bad.append(f"images: expected {sorted(expected)}, has {sorted(got)}")
    for tag in sorted(set(got) & expected):
        entry = got[tag]
        if not isinstance(entry, dict) or set(entry) != {"id", "source"}:
            bad.append(f"unrecognised: image {tag}")
            continue
        image_id, source = entry["id"], entry["source"]
        if is_unknown(image_id) or is_unknown(source):
            bad.append(f"image {tag}: {(image_id if is_unknown(image_id) else source)['unknown']}")
            continue
        if not (isinstance(image_id, str) and IMAGE_ID.fullmatch(image_id)) or not isinstance(source, str):
            bad.append(f"unrecognised: image {tag} id or source")
            continue
        if tag in built or tag in scripted:
            context, arg = scripted.get(tag, (built.get(tag), None))
            m = BUILT.fullmatch(source)
            if not m or m.group(1) != context or (m.group(3) is None) != (arg is None):
                bad.append(f"unrecognised: image {tag} source {source!r} (built from benches/{context})")
                continue
            if arg is not None:
                declared = owned("dockerfile.declared_source_commit", declared_source_commit,
                                 owners.root / "benches" / context / "Dockerfile", arg)
                if m.group(3) != declared:
                    bad.append(f"image {tag}: built at {m.group(3)}, declared {declared} ({arg})")
            tree = owners.object_id(owners.head, f"benches/{context}")
            if is_unknown(tree):
                bad.append(f"image {tag}: nothing to compare with: {tree['unknown']}")
            elif m.group(2) != tree:
                stale.append(f"image {tag}: context benches/{context} changed")
            continue
        m = PULLED.fullmatch(source)
        pin = owners.pins.get(tag)
        if not m:
            bad.append(f"unrecognised: image {tag} source {source!r} (a pulled image)")
        elif pin is None:
            bad.append(f"image {tag}: no digest pin at HEAD (benches/suite/images.toml)")
        elif m.group(1) != pin:
            stale.append(f"image {tag}: pin moved")
    return got


def check_build(owners, system, kind, ctx, got, images, bad, stale):
    """Each program the cell runs that the suite builds (driver.expected_programs), by the producer's
    proofs: its source trees and non-git inputs by content, its controlled build (environment,
    config, declared toolchain), its default settings and, in a container, its pinned Rust image."""
    programs = driver.expected_programs(system)
    if is_unknown(got):
        bad.append(f"build: {got['unknown']}")
        return
    if not isinstance(got, dict):
        bad.append("unrecognised: build")
        return
    if set(got) != set(programs):
        bad.append(f"build: expected {sorted(programs)}, has {sorted(got)}")
    for program in sorted(set(got) & set(programs)):
        record, name = got[program], f"{program} build"
        if is_unknown(record):
            bad.append(f"{name}: {record['unknown']}")
            continue
        container = driver.container_build(program, kind)
        keys = BUILD | set(owners.settings) | (CONTAINER if container else set())
        if not isinstance(record, dict) or set(record) != keys:
            bad.append(f"unrecognised: {name} fields {sorted(set(record) ^ keys) if isinstance(record, dict) else ''}")
            continue
        commit = record["commit"]
        if not (isinstance(commit, str) and HEX40.fullmatch(commit)):
            bad.append(f"{name}: commit {commit!r}")
        if type(record["dirty"]) is not int or record["dirty"]:
            bad.append(f"{name}: from a dirty checkout ({record['dirty']})")
        engine = owners.engine if program == "nrese" else None
        if program == "nrese" and isinstance(commit, str) and HEX40.fullmatch(commit):
            after = owners.after_reset(commit)
            if after is None:
                bad.append(f"{name}: ancestry unreadable: {commit[:12]} isn't in this clone")
            elif not after:
                bad.append(f"{name}: built before the reset ({owners.reset_base})")
            if engine and commit != engine:
                bad.append(f"{name}: at {commit[:12]}, not --engine {engine[:12]}")
        base = engine or owners.head
        identity(f"{name} sources", record["sources"],
                 {p: owners.object_id(base, p) for p in receipts.BUILD_SOURCES[program]}, HEX40, bad, stale)
        identity(f"{name} inputs", record["inputs"], driver.expected_build_inputs(program), INPUT, bad, stale)
        toolchain = record["toolchain"]
        readable = isinstance(toolchain, dict) and set(toolchain) == TOOLCHAIN \
            and all(isinstance(v, str) for v in toolchain.values())
        if not (readable or is_unknown(toolchain)):
            bad.append(f"unrecognised: {name} toolchain")
        if is_unknown(record["controlled"]):
            ran = f" (ran rustc {toolchain['rustc']}, cargo {toolchain['cargo']})" if readable else ""
            bad.append(f"{name} not controlled: {record['controlled']['unknown']}{ran}")
        elif record["controlled"] is not True:
            bad.append(f"unrecognised: {name} controlled")
        for setting, default in owners.settings.items():
            if record[setting] != default:
                bad.append(f"{name}: {setting} {record[setting]!r}, the default is {default!r}")
        if container:
            image = owned(f"{system.key}.rust_image", system.rust_image, ctx)
            ran = images.get(record["rust_image"]) if isinstance(images, dict) else None
            if record["rust_image"] != image:
                bad.append(f"{name}: rust_image {record['rust_image']!r}, the owner's is {image!r}")
            elif not isinstance(ran, dict) or ran.get("id") != record["rust_image_id"]:
                bad.append(f"{name}: compiled in {record['rust_image_id']}, not the pinned image its containers ran")
            flags = driver.docker_rustflags(owners.settings["target_cpu"])
            if record["rustflags"] != flags:
                bad.append(f"{name}: rustflags {record['rustflags']!r}, the default is {flags!r}")


def cells(owners: Owners, systems: dict, workloads: dict, records: list[dict]) -> list[Cell]:
    """Every cell of the registries, classified; a global read that fails raises before any is shown."""
    found: dict[tuple, list] = {}
    for record in records:
        for pair in record.get("pairs", []):
            found.setdefault((pair["workload"], str(pair["tier"]), pair["system"]), []).append((record["id"], pair))
    out = []
    for name, workload in workloads.items():
        tiers = standard_tiers(name, workload)
        for key, system in systems.items():
            if system.get("refused") or system.get("variant-of"):
                why = system.get("refused") or f"a build variant of {system['variant-of']}"
                out += [Cell(name, t, key, "excluded", [why]) for t in tiers]
                continue
            try:
                why = owned("status.runnable", runnable, owners.root, key, system, name, workload)
            except Unreadable as error:
                out += [Cell(name, t, key, "unqualified", [str(error)]) for t in tiers]
                continue
            if why:
                out += [Cell(name, t, key, "cannot-run", [why]) for t in tiers]
                continue
            out += [classify(owners, name, t, key, system, found.get((name, t, key), [])) for t in tiers]
    return out


def classify(owners, workload, tier, key, system, pairs) -> Cell:
    record, receipt, verdict = evidence(pairs)
    if verdict and verdict[0] == "no-result":
        return Cell(workload, tier, key, verdict[0], [verdict[1]])
    if system.get("publish") == "permission":
        return Cell(workload, tier, key, "restricted-presence", ["a licensed system: presence only, never current"])
    if verdict:
        return Cell(workload, tier, key, verdict[0], [verdict[1]])
    klass, reasons = judge(owners, workload, tier, key, receipt)
    return Cell(workload, tier, key, klass, reasons, record, receipt.get("attempt"), receipt.get("started"))


def text(owners: Owners, out: list[Cell]) -> str:
    counts = {k: sum(c.klass == k for c in out) for k in CLASSES}
    lines = [f"coverage at {owners.head[:12]}" + (f", engine {owners.engine[:12]}" if owners.engine else "")
             + f": {len(out)} cells", "  " + " · ".join(f"{k} {n}" for k, n in counts.items() if n), ""]
    for key in dict.fromkeys(c.system for c in out):
        mine = [c for c in out if c.system == key and c.klass not in ("excluded", "cannot-run")]
        if mine:
            lines.append(f"  {key}: current {sum(c.klass == 'current' for c in mine)}/{len(mine)}")
    for klass in ("stale", "not-ok", "unqualified", "restricted-presence", "no-result"):
        listed = [c for c in out if c.klass == klass]
        if listed:
            lines += ["", f"{klass}:"] + [f"  {c.workload} {c.tier} {c.system}: {'; '.join(c.reasons)}" for c in listed]
    return "\n".join(lines)


def main(argv: list[str], systems: dict, workloads: dict, root: Path) -> int:
    p = argparse.ArgumentParser(prog="suite.py coverage", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--systems", help="comma-separated systems.toml keys (default: all)")
    p.add_argument("--workloads", help="comma-separated workloads.toml keys (default: all)")
    p.add_argument("--engine", metavar="REV", help="NRESE's build must be this commit (default: any after the reset)")
    p.add_argument("--json", action="store_true", help="every cell as JSON")
    args = p.parse_args(argv)
    for flag, names, known in (("--systems", args.systems, systems), ("--workloads", args.workloads, workloads)):
        if names and (unknown := [n for n in names.split(",") if n not in known]):
            p.error(f"{flag}: unknown {', '.join(unknown)}")
    chosen = {k: v for k, v in systems.items() if not args.systems or k in args.systems.split(",")}
    wanted = {k: v for k, v in workloads.items() if not args.workloads or k in args.workloads.split(",")}
    try:
        owners = Owners(root, args.engine)
        records = global_read("benches/runs", load_records, root / "benches/runs")
        out = cells(owners, chosen, wanted, records)
    except (GlobalReadError, OSError, subprocess.SubprocessError) as error:
        print(f"refused: {error}", file=sys.stderr)
        return 2
    if args.json:
        print(json.dumps([{**{k: v for k, v in asdict(c).items() if k != "klass"}, "class": c.klass} for c in out],
                         indent=1))
    else:
        print(text(owners, out))
    return 0
