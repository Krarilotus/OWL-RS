"""The suite paths other tests replace wholesale, each run as it is down to what it launches: only
the launch itself (subprocess.run) is a stand-in, and the test checks what was launched. A path
replaced whole is never run by any test, so a fault inside one (an undefined `ROOT` after #132's
merge, 11 Nov 2026) passed every test and failed only on a fleet host (D16-472; the toolchain-root
fix's review r1 listed these paths).

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_real_launches.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import contextlib
import csv
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
import abba  # noqa: E402
import suite  # noqa: E402
from suitekit import driver, fleet_cells, manifest, ns, schema  # noqa: E402
from suitekit.workloads import Plan  # noqa: E402

A = "a" * 40


def done(command, code=0, out=""):
    return subprocess.CompletedProcess(command, code, out, "")


class Temp(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="nrese-real-launches-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.launched = []
        # The host's locks (ns.lock_dir) stay the host's: this test's are its own.
        held = patch.dict(os.environ, {"NRESE_LOCK_DIR": str(self.root / "locks")})
        held.start()
        self.addCleanup(held.stop)

    def suite(self, *args):
        made = driver.Suite(driver.arguments([*args, "--results", str(self.root / "results"), "--systems", "x",
                                              "--workloads", "w", "--skip-build"]),
                            {"x": {"capabilities": ["load"], "publish": "free"}},
                            {"w": {"needs": ["load"], "state": "ready", "kit": ""}}, self.root)
        made.say = Mock()
        return made


class EnsureData(Temp):
    """Suite.ensure_data with the datasets in a Docker volume: it looks for the inputs in the volume,
    prepares what is missing, and looks again."""

    def ensure(self, present_after: bool):
        found = {"prepared": False}

        def run(command, **kwargs):
            self.launched.append((command, kwargs.get("cwd")))
            if command[-3:-1] == ["sh", "-c"]:
                return done(command, out="" if found["prepared"] and present_after else "/data/a.nt\n")
            if command[0] == "python":
                found["prepared"] = True
            return done(command)
        made = self.suite("--runtime", "docker", "--data", "volume:vol-x")
        plan = Plan("w", "1", inputs=["/data/a.nt"], prepare=["python", "prepare.py", "a"])
        with patch.object(subprocess, "run", side_effect=run):
            return made, made.ensure_data(plan)

    def test_missing_inputs_are_prepared_where_the_volume_is_and_looked_for_again(self):
        made, why = self.ensure(present_after=True)
        look = ["--rm", "-v", "vol-x:/data:ro", "alpine", "sh", "-c", "test -s /data/a.nt || echo /data/a.nt"]
        self.assertEqual(why, None, "SPEC ensure-data-volume: inputs made by their preparation are there")
        self.assertEqual([(c[0], c[1], c[-len(look):]) for c, _ in self.launched[::2]],
                         [(ns.executable(), "run", look)] * 2,
                         "SPEC ensure-data-volume: the inputs are looked for in the volume, read-only, before and after")
        self.assertEqual(self.launched[1], (["python", "prepare.py", "a"], self.root),
                         "SPEC ensure-data-volume: the plan's preparation runs on the host, from the repository")
        self.assertEqual(made.made_files, ["/data/a.nt"], "SPEC ensure-data-volume: what the run made is recorded")

    def test_inputs_still_missing_after_their_preparation_fail_the_plan(self):
        _, why = self.ensure(present_after=False)
        self.assertRegex(why or "", r"^preparing the inputs failed; see .*prepare-w-1-.*\.log$",
                         "SPEC ensure-data-volume: a preparation that made nothing is no data")


class SifMissing(Temp):
    """Suite.sif_missing: under apptainer and rootfs, every image a system runs or builds must be
    there, as a SIF file or an unpacked root file system."""

    def test_the_images_not_there_are_named(self):
        system = Mock(images=Mock(return_value=["vendor/a:1", "b/c:2"]), builds=Mock(return_value=[("own/d:3", "ctx")]))
        (self.root / "data").mkdir()
        (self.root / "images" / "b_c_2" / "rootfs").mkdir(parents=True)
        (self.root / "images" / "vendor_a_1.sif").write_bytes(b"")
        for runtime, where, expected in (
                ("apptainer", "--sif-dir", "no SIF image for b/c:2, own/d:3 in --sif-dir (benches/cluster/build-sif.sh)"),
                ("rootfs", "--rootfs-dir",
                 "no unpacked image for vendor/a:1, own/d:3 in --rootfs-dir (benches/sparql/images.sh export)")):
            with self.subTest(runtime):
                made = self.suite("--runtime", runtime, where, str(self.root / "images"), "--data", str(self.root / "data"))
                self.assertEqual(made.sif_missing(system, made.context_for_images()), expected,
                                 "SPEC sif-missing: each image not there, by name, and where it comes from")

    def test_with_every_image_there_nothing_is_missing(self):
        system = Mock(images=Mock(return_value=["vendor/a:1"]), builds=Mock(return_value=[]))
        (self.root / "data").mkdir()
        (self.root / "images").mkdir()
        (self.root / "images" / "vendor_a_1.sif").write_bytes(b"")
        made = self.suite("--runtime", "apptainer", "--sif-dir", str(self.root / "images"), "--data", str(self.root / "data"))
        self.assertIsNone(made.sif_missing(system, made.context_for_images()), "SPEC sif-missing: none missing")


def build(directory: Path, revision: str = A) -> Path:
    """A build as scripts/release-gate.sh leaves it: its REVISION and the sha256sum of every file
    under release/ (SHA256SUMS, `<sha256>  <path>`)."""
    files = {"release/nrese-server": b"server", "release/examples/perf_lab": b"lab"}
    for name, data in files.items():
        (directory / name).parent.mkdir(parents=True, exist_ok=True)
        (directory / name).write_bytes(data)
    (directory / "REVISION").write_text(revision + "\n")
    (directory / "SHA256SUMS").write_text("".join(f"{hashlib.sha256(data).hexdigest()}  {name}\n"
                                                  for name, data in sorted(files.items())), newline="\n")
    (directory / "IDENTITY").write_text("build", encoding="utf-8")
    return directory


class Binaries(Temp):
    """manifest.binaries: a build is verified when its files are those its record lists, of the
    revision asked for."""

    def test_a_build_is_verified_only_as_recorded(self):
        self.assertTrue(manifest.binaries(build(self.root / "as-built"), A)["verified"],
                        "SPEC binaries-verified: the build as recorded, of that revision")
        changed = build(self.root / "changed")
        (changed / "release/nrese-server").write_bytes(b"other")
        unrecorded = build(self.root / "unrecorded")
        (unrecorded / "REVISION").unlink()
        for name, target, revision in (("a binary changed", changed, A), ("another revision", build(self.root / "b"), "b" * 40),
                                       ("no REVISION", unrecorded, A)):
            with self.subTest(name):
                self.assertFalse(manifest.binaries(target, revision)["verified"], "SPEC binaries-verified: not as recorded")


class Member(Temp):
    """fleet_cells.member, the fleet step of one suite cell: its child commands, each the repository's
    and each as its program takes it (suite.py run's arguments through its own parser)."""

    def test_a_cell_launches_the_repositorys_commands_and_lands_its_results(self):
        checkout, gate, out = self.root / "checkout", self.root / "gate", self.root / "out"
        bins = build(self.root / "bin")
        for directory in (checkout, gate, out, self.root / "suite-client-target" / "release"):
            directory.mkdir(parents=True, exist_ok=True)
        (gate / "inputs.txt").write_text("inputs", encoding="utf-8")
        (self.root / "suite-client-target" / "release" / "nrese-bench-harness").write_bytes(b"client")
        parsed = []

        def run(command, **kwargs):
            if command[0] == "git":  # manifest.git: the checkout here is no repository
                return done(command, 128)
            self.launched.append(command)
            if "bin_of" in " ".join(command):
                return done(command, out=str(bins))
            if "benches/suite/suite.py" in command:
                at = command.index("benches/suite/suite.py")
                self.assertEqual(command[at + 1], "run", "SPEC fleet-cell-member: the suite's run")
                args = command[at + 2:]
                with contextlib.redirect_stderr(io.StringIO()) as said:
                    try:
                        parsed.append(driver.arguments(args))
                    except SystemExit:
                        self.fail(f"suite.py run refuses the cell's arguments {args}: {said.getvalue()}")
                results = Path(args[args.index("--results") + 1])
                with open(results / "results.csv", "w", newline="", encoding="utf-8") as f:
                    rows = csv.DictWriter(f, schema.FIELDS)
                    rows.writeheader()
                    rows.writerow({"system": "qlever", "task": "load", "status": "ok"})
                for name in ("manifest.json", "answers.json"):
                    (results / name).write_text("{}", encoding="utf-8")
            return done(command, out="x" if kwargs.get("stdout") == subprocess.PIPE else None)
        payload = {"group": {"systems": ["qlever"], "references": ["jena", "oxigraph"],
                             "case": {"workload": "sparqloscope", "tier": "sparqloscope-dblp"}},
                   "identity": {}, "result_root": str(self.root / "private"), "member": "cell", "nrese": A,
                   "harness": A, "deadline_min": 1}
        with patch.object(subprocess, "run", side_effect=run), patch.object(abba, "git", return_value="t" * 40), \
                patch.dict(os.environ, {"SUITE_CELL": json.dumps(payload), "FLEET_OUT": str(out),
                                        "NRESE_GATE_DIR": str(gate)}), contextlib.redirect_stderr(io.StringIO()):
            code = fleet_cells.member(checkout, suite.load()[0])
        landing = self.root / "private" / "cell"
        self.assertEqual(code, 0, "SPEC fleet-cell-member: the cell measured:\n"
                         + (landing / "stderr.log").read_text(encoding="utf-8"))
        self.assertEqual([c[:3] if c[0] == "bash" else c[:1] for c in self.launched],
                         [["bash", "scripts/release-gate.sh", "build"], ["bash", "scripts/release-gate.sh", "prepare"],
                          ["bash", "-c", ". scripts/lib/gate-publish.sh; bin_of \"$1\""],
                          ["bash", "scripts/cargo-guarded.sh", "build"], ["rustc"],
                          ["bash", "scripts/cargo-guarded.sh", "--print-rustflags"], ["uv"],
                          ["bash", "scripts/release-gate.sh", "clean"]],
                         "SPEC fleet-cell-member: the builds, the data, the harness, the suite run, the cleanup, in order")
        named = {part.split(";")[0].removeprefix(". ") for c in self.launched for part in c
                 if part.startswith(("scripts/", "benches/", ". scripts/"))}
        self.assertEqual(sorted(p for p in named if not (ROOT / p).is_file()), [],
                         "SPEC fleet-cell-member: every script it launches is the repository's")
        self.assertEqual([(p.systems, p.tier, p.answers_reference) for p in parsed],
                         [("qlever", ["sparqloscope=sparqloscope-dblp"], "jena,oxigraph")],
                         "SPEC fleet-cell-member: the suite runs the cell's system and case against its references")
        identity = json.loads((landing / "identity.json").read_text(encoding="utf-8"))
        self.assertEqual((identity["source_tree"], identity["build_identity"]), ("t" * 40, "build"),
                         "SPEC fleet-cell-member: the cell names the source and build it measured")


if __name__ == "__main__":
    unittest.main()
