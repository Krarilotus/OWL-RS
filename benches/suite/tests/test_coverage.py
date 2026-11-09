"""`suite.py coverage` (suitekit/coverage.py): whether a catalogue cell's receipt is current. Specified by
Harness notes 2026-11-08-coverage-refresh/p2-design-r5.md with addenda r6 and r7 (design check r6:
SOUND): a cell is current only if its one newest receipt is a complete attempt whose every field a
positive rule accepts against what this clean checkout's owners declare for that receipt's own
runtime; anything missing, unknown, unordered or unrecognised is not current, with its reason, and an
owner read that fails is never an empty inventory. Each fixture below is a receipt the producer
(driver.Suite.receipts) writes, its expected values read here with plain git from a repository of the
test's own, never through the classifier."""
from __future__ import annotations

import contextlib
import copy
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import isolation  # noqa: E402
from suitekit import adapters, coverage, driver, receipts, runtime, summary  # noqa: E402

DECLARED = "ecbf6dde5a5864f088eee3b0836ad6adba1d623b"  # the fixture Dockerfile's MDB_COMMIT
OTHER = "ecbf6dd" + "0" * 33  # its first seven characters, another commit
RUST_PIN = "sha256:" + "1" * 64
QLEVER_PIN = "sha256:" + "2" * 64
IMAGE_ID = "sha256:" + "3" * 64
LUBM_1, UNIV = "4" * 64, "5" * 64
FILES = {
    "rust-toolchain.toml": '[toolchain]\nchannel = "1.98.1"\n',
    ".cargo/config.toml": "[build]\n",
    "Cargo.toml": "[workspace]\n", "Cargo.lock": "# lock\n", "crates/nrese-server/src/main.rs": "fn main() {}\n",
    "scripts/cargo-guarded.sh": "#!/bin/sh\n", "scripts/lib/target-cpu.sh": "#\n", "scripts/lib/disk.sh": "#\n",
    "benches/nrese-bench-harness/Cargo.toml": "[package]\n",
    "benches/competitors/jena/Dockerfile": "FROM a\n",
    "benches/competitors/millenniumdb/Dockerfile": f"FROM a\nARG MDB_COMMIT={DECLARED}\nFROM b\nARG MDB_COMMIT={DECLARED}\n",
    "benches/datasets.toml": (f'[[dataset]]\nfile = "lubm-1.nt"\nsha256 = "{LUBM_1}"\n\n'
                              f'[[dataset]]\nfile = "univ-bench.nt"\nsha256 = "{UNIV}"\n\n'
                              '[[dataset]]\nfile = "made.nt"\nrdfc_sha256 = "' + "6" * 64 + '"\n'),
    "benches/suite/images.toml": (f'[pins]\n"rust:1.98.1-bookworm" = "{RUST_PIN}"\n'
                                  f'"adfreiburg/qlever:commit-0998359ab0" = "{QLEVER_PIN}"\n'),
}
SYSTEMS = {"nrese": {"publish": "free"}, "jena": {"publish": "free"}, "qlever": {"publish": "free"},
           "millenniumdb": {"publish": "free"}, "graphdb": {"publish": "permission"},
           "rdfox": {"publish": "permission", "refused": "no licence"},
           "nrese-oxigraph": {"publish": "free", "variant-of": "nrese"}}


def git(root: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True, check=True,
                          timeout=60).stdout.strip()


class Repo:
    """A repository of the test's own with the files the owners read, three commits: one before the
    reset, the one standing for the reset base, and HEAD."""

    def __init__(self, path: Path):
        for rel, text in FILES.items():
            (path / rel).parent.mkdir(parents=True, exist_ok=True)
            (path / rel).write_text(text, encoding="utf-8", newline="\n")
        self.root = path
        self.pre, self.base, self.head = isolation.repo(path, 3)

    def owners(self, **kw) -> coverage.Owners:
        return coverage.Owners(self.root, reset_base=self.base, **kw)

    def ids(self, paths) -> dict:
        return {p: git(self.root, "rev-parse", f"HEAD:{p}") for p in paths}

    def build(self, program: str, docker: bool) -> dict:
        record = {"commit": self.head, "dirty": 0, "sources": self.ids(receipts.BUILD_SOURCES[program]),
                  "inputs": {p: "no files" for p in receipts.BUILD_INPUTS[program]},
                  "toolchain": {"rustc": "1.98.1", "cargo": "1.98.1", "host": "x86_64-unknown-linux-gnu"},
                  "controlled": True, "target_cpu": "native", "rustflags_env": ""}
        if docker:
            record |= {"rust_image": "rust:1.98.1-bookworm", "rust_image_id": IMAGE_ID,
                       "rustflags": "-C target-cpu=native"}
        return record

    def receipt(self, system: str = "nrese", kind: str = "docker") -> dict:
        """A receipt as driver.Suite.receipts writes it for a complete, qualified lubm/1 cell."""
        plan = coverage.plans(self.root, "lubm", ["1"], {})[0]
        tree = lambda context: git(self.root, "rev-parse", f"HEAD:benches/{context}")  # noqa: E731
        images = {
            "nrese": {"rust:1.98.1-bookworm": f"pulled, pinned {RUST_PIN}"} if kind == "docker" else {},
            "jena": {"nrese-bench/jena:6.2.0": f"built from benches/competitors/jena@{tree('competitors/jena')}"},
            "qlever": {"adfreiburg/qlever:commit-0998359ab0": f"pulled, pinned {QLEVER_PIN}"},
            "millenniumdb": {"nrese-bench/millenniumdb:ecbf6dd": "built from benches/competitors/millenniumdb@"
                             f"{tree('competitors/millenniumdb')}, source commit {DECLARED}"},
        }[system]
        build = {"harness": self.build("harness", False)}
        if system == "nrese":
            build["nrese"] = self.build("nrese", kind == "docker")
        paths = driver.receipt_code_paths(self.root, self.head, adapters.adapter(system), plan)
        return {"attempt": "a" * 32, "started": "2026-11-09T06:00:00.000001", "complete": True,
                "commit": self.head, "dirty": 0, "code": self.ids(paths),
                "images": {tag: {"id": IMAGE_ID, "source": source} for tag, source in images.items()},
                "build": build, "settings": {},
                "protocol": {"runs": 3, "query_runs": 3, "cache": ["off", "on"], "order": "shuffled", "seed": 1,
                             "timeout_s": 3600, "query_timeout_s": 300, "runtime": kind},
                "data": {"/data/univ-bench.nt": UNIV, "/data/lubm-1.nt": LUBM_1},
                "outcome": {"regime": "owl2-rl", "runs": 3, "outcome": "ok"},
                "answers": {"worst": "ok", "verdicts": {"ok": 14}, "qualified": True}}


def unknown(why="why"):
    return {"unknown": why}


class Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.repo = Repo(Path(cls.tmp.name).resolve() / "repo")

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def judge(self, receipt, system="nrese", owners=None):
        return coverage.judge(owners or self.repo.owners(), "lubm", "1", system, receipt)


class Fields(Base):
    def test_a_receipt_every_owner_accepts_is_current(self):
        """Each provenance case and runtime, as the producer writes it: a pulled pinned Rust image and
        a Docker build (NRESE docker), no image (NRESE process), an image built here (Jena), a pulled
        one (QLever) and one images.sh builds at its declared commit (MillenniumDB)."""
        for system, kind in (("nrese", "docker"), ("nrese", "process"), ("jena", "docker"), ("qlever", "docker"),
                             ("millenniumdb", "docker")):
            with self.subTest(system=system, kind=kind):
                self.assertEqual(self.judge(self.repo.receipt(system, kind), system), ("current", []))

    def test_each_field_that_isn_t_the_owner_s_makes_the_cell_not_current_with_its_reason(self):
        """r5/r6/r7 attack groups G2-G9 on one NRESE Docker receipt: (what changes, class, reason)."""
        r = self.repo
        harness_src = "benches/nrese-bench-harness"
        cases = [
            # G2: recognised outcome and answers, then the predicates
            (lambda x: x["outcome"].update(outcome="partial"), "not-ok", "outcome partial"),
            (lambda x: x["outcome"].update(outcome="bogus"), "unqualified", "unrecognised: outcome"),
            (lambda x: x.update(answers="pending"), "unqualified", "answers pending"),
            (lambda x: x.update(answers="not captured"), "unqualified", "answers not captured"),
            (lambda x: x["answers"].update(qualified=False), "unqualified", "answers not qualified"),
            (lambda x: x.update(answers={"worst": "ok"}), "unqualified", "unrecognised: answers"),
            # G3: protocol, and the runtime the receipt names (F13)
            (lambda x: x["protocol"].update(runs=1), "unqualified", "protocol short"),
            (lambda x: x["protocol"].update(cache=["off"]), "unqualified", "result cache"),
            (lambda x: x["protocol"].update(order="fixed"), "unqualified", "order fixed"),
            (lambda x: x["protocol"].pop("runtime"), "unqualified", "no runtime"),
            (lambda x: x["protocol"].update(runtime="kubernetes"), "unqualified", "unrecognised: runtime"),
            (lambda x: x["protocol"].update(runtime="process"), "unqualified", "images: expected"),
            # G4: the invocation
            (lambda x: x.update(complete=False), "unqualified", "incomplete"),
            (lambda x: x.update(commit=unknown("checkout moved during the run: a -> b")), "unqualified",
             "checkout moved"),
            (lambda x: x.update(commit=unknown("checkout changed during the run: clean at its start")),
             "unqualified", "checkout changed"),
            (lambda x: x.update(dirty=2), "unqualified", "dirty"),
            (lambda x: x.update(extra=1), "unqualified", "unrecognised: receipt fields"),
            (lambda x: x["code"].update({"benches/suite": "0" * 40}), "stale", "code: benches/suite changed"),
            (lambda x: x["code"].pop("scripts"), "stale", "definition changed"),
            (lambda x: x["code"].update({"benches/suite": unknown()}), "unqualified", "code benches/suite"),
            # G5: images
            (lambda x: x.update(images={}), "unqualified", "images: expected"),
            (lambda x: x["images"].update({"extra:1": {"id": IMAGE_ID, "source": "pulled, pinned " + RUST_PIN}}),
             "unqualified", "images: expected"),
            (lambda x: x["images"]["rust:1.98.1-bookworm"].update(source="pulled, pinned sha256:" + "9" * 64),
             "stale", "pin moved"),
            (lambda x: x["images"]["rust:1.98.1-bookworm"].update(source=unknown("no build label and no digest pin")),
             "unqualified", "no digest pin"),
            (lambda x: x["images"]["rust:1.98.1-bookworm"].update(id="latest"), "unqualified", "unrecognised: image"),
            # G6: the builds (F10, F14, F15, F16 through `controlled`, r7 export failure)
            (lambda x: x["build"].pop("nrese"), "unqualified", "build: expected"),
            (lambda x: x["build"]["harness"]["sources"].update({harness_src: "0" * 40}), "stale",
             f"harness build sources: {harness_src} changed"),
            (lambda x: x["build"]["nrese"]["sources"].update({"scripts/lib/target-cpu.sh": "0" * 40}), "stale",
             "nrese build sources: scripts/lib/target-cpu.sh changed"),
            (lambda x: x["build"]["nrese"]["sources"].pop("scripts/cargo-guarded.sh"), "stale", "definition changed"),
            (lambda x: x["build"]["nrese"]["inputs"].update({"apps/nrese-console/dist": "sha256:" + "7" * 64}),
             "stale", "apps/nrese-console/dist changed"),
            (lambda x: x["build"]["nrese"]["inputs"].update({"apps/nrese-console/dist": unknown("can't be read")}),
             "unqualified", "can't be read"),
            (lambda x: x["build"]["nrese"].pop("inputs"), "unqualified", "unrecognised: nrese build fields"),
            (lambda x: x["build"]["harness"].update(
                controlled=unknown("rustc 1.97.0 selected, rust-toolchain.toml declares 1.98.1"),
                toolchain={"rustc": "1.97.0", "cargo": "1.97.0", "host": "h"}), "unqualified", "rustc 1.97.0 selected"),
            (lambda x: x["build"]["harness"].update(controlled=unknown("cargo also reads /home/u/.cargo/config.toml")),
             "unqualified", "cargo also reads"),
            (lambda x: x["build"]["harness"].update(controlled=False), "unqualified", "unrecognised: harness build controlled"),
            (lambda x: x["build"]["harness"].update(toolchain={"rustc": "1.98.1"}), "unqualified",
             "unrecognised: harness build toolchain"),
            (lambda x: [x["build"]["harness"].pop(k) for k in ("toolchain", "controlled")], "unqualified",
             "unrecognised: harness build fields"),  # an export that failed: built_from's record only
            (lambda x: x["build"]["harness"].update(rust_image="rust:1.98.1-bookworm"), "unqualified",
             "unrecognised: harness build fields"),
            (lambda x: x["build"]["nrese"].pop("rust_image_id"), "unqualified", "unrecognised: nrese build fields"),
            (lambda x: x["build"]["nrese"].update(rust_image_id="sha256:" + "8" * 64), "unqualified", "pinned"),
            (lambda x: x["build"]["nrese"].update(rust_image="rust:1.97"), "unqualified", "rust_image"),
            (lambda x: x["build"]["nrese"].update(rustflags="-C opt-level=1"), "unqualified", "rustflags"),
            (lambda x: x["build"]["nrese"].update(target_cpu="znver3"), "unqualified", "target_cpu"),
            (lambda x: x["build"]["harness"].update(rustflags_env="-C opt-level=0"), "unqualified", "rustflags_env"),
            (lambda x: x["build"]["nrese"].update(dirty=1), "unqualified", "nrese build: from a dirty checkout"),
            (lambda x: x["build"]["nrese"].update(commit=r.pre), "unqualified", "built before the reset"),
            (lambda x: x["build"]["nrese"].update(commit="f" * 40), "unqualified", "ancestry unreadable"),
            (lambda x: x["build"].update(harness=unknown("--skip-build: the harness that was there")), "unqualified",
             "skip-build"),
            # G7: settings in force
            (lambda x: x.update(settings={"JAVA_HEAP": "8g"}), "unqualified", "settings set: JAVA_HEAP"),
            # G8: data
            (lambda x: x["data"].update({"/data/lubm-1.nt": "0" * 64}), "stale", "data: /data/lubm-1.nt changed"),
            (lambda x: x["data"].pop("/data/univ-bench.nt"), "stale", "definition changed"),
            (lambda x: x["data"].update({"/data/lubm-1.nt": unknown("not there when hashed")}), "unqualified",
             "not there when hashed"),
        ]
        for n, (change, klass, reason) in enumerate(cases):
            receipt = self.repo.receipt()
            change(receipt)
            with self.subTest(n=n, reason=reason):
                got, why = self.judge(receipt)
                self.assertEqual(got, klass, why)
                self.assertTrue(any(reason in w for w in why), why)

    def test_a_scripted_image_is_current_only_at_its_dockerfile_s_declared_commit(self):
        """r7 §4 G5: the recorded source commit equals the declared one exactly; a commit sharing its
        first seven characters (the tag's) is another commit; a declaration that isn't one value is
        unqualified, never an empty or the receipt's own value."""
        tag = "nrese-bench/millenniumdb:ecbf6dd"
        receipt = self.repo.receipt("millenniumdb")
        receipt["images"][tag]["source"] = receipt["images"][tag]["source"].replace(DECLARED, OTHER)
        klass, why = self.judge(receipt, "millenniumdb")
        self.assertEqual(klass, "unqualified")
        self.assertTrue(any(f"built at {OTHER}, declared {DECLARED}" in w for w in why), why)
        no_commit = self.repo.receipt("millenniumdb")
        no_commit["images"][tag]["source"] = no_commit["images"][tag]["source"].split(",")[0]
        self.assertEqual(self.judge(no_commit, "millenniumdb")[0], "unqualified")
        with tempfile.TemporaryDirectory() as d:
            two = Repo(Path(d).resolve() / "repo")
            dockerfile = two.root / "benches/competitors/millenniumdb/Dockerfile"
            dockerfile.write_text(f"ARG MDB_COMMIT={DECLARED}\nARG MDB_COMMIT={OTHER}\n", encoding="utf-8")
            git(two.root, "commit", "-qam", "two values")
            two.head = git(two.root, "rev-parse", "HEAD")
            klass, why = coverage.judge(two.owners(), "lubm", "1", "millenniumdb", two.receipt("millenniumdb"))
            self.assertEqual(klass, "unqualified")
            self.assertTrue(any("dockerfile.declared_source_commit" in w for w in why), why)

    def test_engine_names_nrese_s_build_and_never_the_harness_s(self):
        """r5 §3: under --engine C NRESE's build is C's, by commit and by source trees; the harness is
        still judged against HEAD."""
        r = self.repo
        owners = r.owners(engine=r.base)
        receipt = r.receipt()
        self.assertTrue(any("not --engine" in w for w in self.judge(receipt, owners=owners)[1]))
        receipt["build"]["nrese"]["commit"] = r.base
        receipt["build"]["nrese"]["sources"] = {p: git(r.root, "rev-parse", f"{r.base}:{p}")
                                                for p in receipts.BUILD_SOURCES["nrese"]}
        self.assertEqual(self.judge(receipt, owners=owners), ("current", []))
        receipt["build"]["harness"]["sources"] = {p: git(r.root, "rev-parse", f"{r.pre}:{p}")
                                                  for p in receipts.BUILD_SOURCES["harness"]}
        (r.root / "benches/nrese-bench-harness/Cargo.toml").write_text("[package]\nname = 'h'\n", encoding="utf-8")
        git(r.root, "commit", "-qam", "the harness changes after the engine")
        try:
            later = r.owners(engine=r.base)
            self.assertTrue(any("harness build sources" in w for w in self.judge(receipt, owners=later)[1]))
        finally:
            git(r.root, "reset", "-q", "--hard", r.head)

    def test_the_owners_are_read_with_explicit_inputs_never_the_environment(self):
        """F14, G7: the process environment changes no class and is left as it was."""
        receipt = self.repo.receipt()
        noise = {"NRESE_TARGET_CPU": "znver3", "RUSTFLAGS": "-C opt-level=0", "RUST_IMAGE": "rust:1.97",
                 "QLEVER_IMAGE": "x:1"}
        with patch.dict(os.environ, noise):
            before = dict(os.environ)
            self.assertEqual(self.judge(receipt), ("current", []))
            self.assertEqual(dict(os.environ), before)


class Selection(unittest.TestCase):
    """G1: which receipt is a cell's evidence (r3, F7), before any field is judged."""

    @staticmethod
    def pair(started=None, attempt="a", outcome="ok", **receipt):
        out = {"workload": "lubm", "tier": "1", "system": "nrese", "outcome": outcome}
        if started is not None or receipt:
            out["receipt"] = {"attempt": attempt, "started": started, **receipt}
        return ("record", out)

    def test_the_newest_receipt_decides_and_what_can_t_be_ordered_stands_for_nothing(self):
        old = self.pair("2026-11-08T01:00:00", "old", complete=True)
        new = self.pair("2026-11-09T01:00:00", "new", complete=False)
        self.assertEqual(coverage.evidence([old, new])[1]["attempt"], "new")
        self.assertEqual(coverage.evidence([new, old])[1]["attempt"], "new")
        self.assertEqual(coverage.evidence([self.pair(outcome="ok"), old])[1]["attempt"], "old")  # legacy pair
        lost = ("record", {**old[1], "receipt": unknown("attempts.jsonl lost records")})
        for case, pairs, klass in (
                ("lost journal beside a pass", [old, lost], "unqualified"),
                ("no start", [old, self.pair("", "x", complete=True)], "unqualified"),
                ("a start with an offset", [old, self.pair("2026-11-09T01:00:00+01:00", "x")], "unqualified"),
                ("two attempts, one start", [new, self.pair("2026-11-09T01:00:00", "other")], "unqualified"),
                ("legacy pairs only", [self.pair(outcome="ok")], "unqualified"),
                ("skips only", [self.pair(outcome="skipped")], "no-result"),
                ("no pair", [], "no-result")):
            with self.subTest(case):
                self.assertEqual(coverage.evidence(pairs)[2][0], klass)


class Cells(Base):
    """Which cells there are (excluded, cannot-run, restricted-presence), and the command."""

    def records(self, system="nrese", receipt=None):
        receipt = receipt if receipt is not None else self.repo.receipt(system)
        return [{"id": "r1", "pairs": [{"workload": "lubm", "tier": "1", "system": system, "outcome": "ok",
                                        "receipt": receipt}]}]

    def test_licensed_refused_and_variant_systems_are_never_current(self):
        owners = self.repo.owners()
        workloads = {"lubm": {"tiers": [1]}}
        with patch.object(coverage, "runnable", return_value=None):
            licensed = self.records("graphdb", receipt={"attempt": "g", "started": "2026-11-09T06:00:00"})
            got = {c.system: c for c in coverage.cells(owners, SYSTEMS, workloads, licensed + self.records("nrese"))}
        self.assertEqual(got["graphdb"].klass, "restricted-presence")
        self.assertEqual(got["rdfox"].klass, "excluded")
        self.assertEqual(got["nrese-oxigraph"].klass, "excluded")
        self.assertEqual(got["jena"].klass, "no-result")
        self.assertEqual(got["nrese"].klass, "current")
        with patch.object(coverage, "runnable", return_value="lacks"):
            self.assertEqual({c.klass for c in coverage.cells(owners, {"jena": SYSTEMS["jena"]}, workloads, [])},
                             {"cannot-run"})

    def run_command(self, *argv, records=None, root=None):
        out, err = io.StringIO(), io.StringIO()
        with patch.object(coverage, "load_records", return_value=records if records is not None else self.records()), \
                patch.object(coverage, "runnable", return_value=None), \
                patch.object(coverage, "RESET_BASE", self.repo.base), \
                contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = coverage.main(["--systems", "nrese", *argv], SYSTEMS, {"lubm": {"tiers": [1]}},
                                 root or self.repo.root)
        return code, out.getvalue(), err.getvalue()

    def test_the_command_prints_every_cell_and_writes_nothing(self):
        before = {p: p.read_bytes() for p in self.repo.root.rglob("*") if p.is_file() and ".git" not in p.parts}
        status = git(self.repo.root, "status", "--porcelain")
        code, out, _ = self.run_command("--json")
        self.assertEqual(code, 0)
        cell = json.loads(out)[0]
        self.assertEqual((cell["class"], cell["record"], cell["attempt"]), ("current", "r1", "a" * 32))
        code, out, _ = self.run_command()
        self.assertEqual(code, 0)
        self.assertIn("current 1", out)
        self.assertEqual(git(self.repo.root, "status", "--porcelain"), status)
        self.assertEqual({p: p.read_bytes() for p in before}, before)

    def test_a_dirty_checkout_or_an_unreadable_owner_refuses_and_classifies_nothing(self):
        """r5 §5 and r6 §3: a global read that fails, early or after a valid cell, is a refusal, exit 2,
        with no class on stdout."""
        with tempfile.TemporaryDirectory() as d:
            dirty = Repo(Path(d).resolve() / "repo")
            (dirty.root / "Cargo.toml").write_text("changed\n", encoding="utf-8")
            code, out, err = self.run_command(root=dirty.root)
            self.assertEqual((code, out), (2, ""))
            self.assertIn("dirty", err)
        with patch.object(coverage.driver, "image_pins", side_effect=ValueError("[pins] maps each tag")):
            code, out, err = self.run_command()
        self.assertEqual((code, out), (2, ""))
        self.assertIn("images.toml", err)
        late = self.records() + [{"id": "r2", "pairs": [{"workload": "lubm", "tier": "1", "system": "jena",
                                                         "outcome": "ok", "receipt": self.repo.receipt("jena")}]}]
        real = coverage.judge

        def judge(owners, workload, tier, system, receipt):
            if system == "jena":
                raise coverage.GlobalReadError("expected_build_inputs disagrees with BUILD_INPUTS")
            return real(owners, workload, tier, system, receipt)
        with patch.object(coverage, "judge", judge):
            out, err = io.StringIO(), io.StringIO()
            with patch.object(coverage, "load_records", return_value=late), \
                    patch.object(coverage, "runnable", return_value=None), \
                    contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                code = coverage.main(["--json"], {"nrese": SYSTEMS["nrese"], "jena": SYSTEMS["jena"]},
                                     {"lubm": {"tiers": [1]}}, self.repo.root)
        self.assertEqual((code, out.getvalue()), (2, ""))

    def test_an_owner_inventory_that_contradicts_itself_refuses(self):
        with patch.object(coverage.driver, "expected_build_inputs", return_value={}):
            code, out, err = self.run_command()
        self.assertEqual((code, out), (2, ""))
        self.assertIn("BUILD_INPUTS", err)


class OwnerLines(unittest.TestCase):
    """The declarations P2 reads, each at its owner and used by the producer too (r5 §2, r6 §2)."""

    def test_the_runtime_kinds_are_the_runtimes_the_suite_has(self):
        kinds = {cls.kind for cls in vars(runtime).values()
                 if isinstance(cls, type) and issubclass(cls, runtime.Runtime) and cls.kind not in ("?", None)}
        self.assertEqual(set(runtime.KINDS), kinds)
        self.assertIn(driver.arguments([]).runtime, runtime.KINDS)

    def test_outcomes_are_every_outcome_a_pair_gives(self):
        """A pair's outcome is one of summary.OUTCOMES: every literal Pair.outcome() puts there."""
        import ast
        tree = ast.parse(Path(summary.__file__).read_text(encoding="utf-8"))
        given = set()
        for node in ast.walk(tree):
            values = []
            if isinstance(node, ast.Dict):
                values = [v for k, v in zip(node.keys, node.values) if isinstance(k, ast.Constant) and k.value == "outcome"]
            elif isinstance(node, ast.Assign) and any(isinstance(t, ast.Subscript) and isinstance(t.slice, ast.Constant)
                                                      and t.slice.value == "outcome" for t in node.targets):
                values = [node.value]
            for value in values:
                given |= {n.value for n in ast.walk(value) if isinstance(n, ast.Constant) and isinstance(n.value, str)}
        self.assertEqual(given, set(summary.OUTCOMES))

    def test_the_build_declarations_are_the_producer_s(self):
        nrese, jena = adapters.adapter("nrese"), adapters.adapter("jena")
        self.assertEqual(driver.expected_programs(nrese), ("harness", "nrese"))
        self.assertEqual(driver.expected_programs(jena), ("harness",))
        self.assertEqual([driver.container_build(p, k) for p in ("nrese", "harness") for k in ("docker", "process")],
                         [True, False, False, False])
        self.assertEqual(driver.docker_rustflags("native"), "-C target-cpu=native")
        self.assertEqual(driver.docker_rustflags("portable"), "")

    def test_a_declared_context_says_its_kind_and_no_more(self):
        ctx = adapters.declared_context(Path("."), "docker")
        self.assertEqual((ctx.runtime.kind, ctx.settings, ctx.dry), ("docker", {}, True))
        with self.assertRaises(AttributeError):
            ctx.runtime.new_store("x")


if __name__ == "__main__":
    unittest.main()
