"""Real output and dispatch entry points, with synthetic host settings."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import isolation
sys.path.insert(0, str(isolation.ROOT / "benches/suite"))
from suitekit import driver, hosts, schema
from suitekit.workloads import Plan

REGISTRY = isolation.OVERLAY_HOSTS
ROW = schema.Result("2026-01-01T00:00", "private-test-host", "process", "nrese", "-",
                    "free", "fixture", "tiny", "custom", 1, "load")


class BoundaryRuntime(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory(dir=isolation.OWN)))
        self.public = isolation.checkout(self.root / "public", "benches/suite/suitekit/hosts.py")
        self.inputs = self.root / "inputs"
        isolation.overlay_repo(self.inputs, REGISTRY)
        self.enterContext(patch.dict(os.environ, isolation.inherit("NRESE_", "BENCH_INPUTS_", "FLEET_"), clear=True))
        self.overlay = self.inputs / "overlay"
        self.identity = hosts.content_id(REGISTRY, "")
        self.scratch = self.root / "scratch"
        os.environ["NRESE_BENCH_SCRATCH"] = str(self.scratch)

    def link(self, path, target):
        try:
            path.symlink_to(target)
        except OSError as error:
            if getattr(error, "winerror", None) != 1314:
                raise
            self.skipTest("this Windows account cannot create symbolic links")

    def test_private_csv_checks_the_file_and_numbered_siblings(self):
        # Spec: a private CSV never writes into a checkout through a link, and ordinary private CSVs still append.
        os.environ["NRESE_BENCH_PRIVATE"] = str(self.overlay)
        for form in ("normal", "linked", "numbered", "link after opening"):
            with self.subTest(form=form):
                directory = self.root / form
                directory.mkdir()
                target = directory / "results.csv"
                public_file = self.public / f"{form}.csv"
                # A compatible header exercises the append path, including the numbered candidate.
                public_file.write_text(",".join(schema.FIELDS) + "\n", encoding="utf-8")
                before = public_file.read_bytes()
                if form == "numbered":
                    target.write_text("different,header\n", encoding="utf-8")
                    self.link(target.with_name("results-2.csv"), public_file)
                elif form == "linked":
                    self.link(target, public_file)
                before_write = directory / "results.csv"
                writer = None
                if form == "link after opening":
                    writer = schema.Writer(before_write)
                    before_write.unlink()
                    self.link(before_write, public_file)
                if form == "normal":
                    schema.Writer(target).write(ROW)
                    self.assertEqual([row["host"] for row in schema.read(target)], [ROW.host])
                else:
                    refused = False
                    try:
                        (writer or schema.Writer(target)).write(ROW)
                    except ValueError:
                        refused = True
                    self.assertEqual(public_file.read_bytes(), before)
                    self.assertTrue(refused)

    def test_each_overlay_source_reaches_a_real_kit_child(self):
        # Spec: the suite imports a child's successful row and the child saves precisely the parent's overlay.
        child = self.root / "record.py"
        child.write_text(
            "import json, sys\nfrom pathlib import Path\n"
            f"sys.path.insert(0, {str(isolation.ROOT / 'benches/suite')!r})\n"
            "from suitekit import hosts, manifest, schema\n"
            "@hosts.run_snapshot\n"
            "def run():\n"
            "    out = Path(sys.argv[1])\n"
            "    schema.Writer(out / 'results.csv').write(schema.Result("
            "'2026-01-01T00:00', 'fixture', 'process', 'nrese', '-', 'free', "
            "'fixture', 'tiny', 'custom', 1, 'load'))\n"
            "    (out / 'manifest.json').write_text(json.dumps(manifest.machine('process')))\n"
            "run()\n",
            encoding="utf-8",
        )
        sources = ({"NRESE_BENCH_PRIVATE": str(self.overlay)}, {"BENCH_INPUTS_GIT": str(self.inputs)},
                   dict(zip(hosts.CHANNELS, (REGISTRY, "", self.identity))))
        for n, source in enumerate(sources):
            with self.subTest(source=tuple(source)), patch.dict(os.environ, source):
                results = self.root / f"run-{n}"
                args = driver.arguments(["--results", str(results), "--runtime", "process",
                                         "--data", str(self.root), "--skip-build"])
                plan = Plan("fixture", "tiny", driver=[[sys.executable, "-I", str(child), "{results}"]])

                @hosts.run_snapshot
                def execute():
                    made = driver.Suite(args, {"nrese": {"publish": "free"}}, {}, self.public)
                    made.driver(plan, ["nrese"])

                execute()
                logs = list(results.glob("logs/*/step-1.txt"))
                self.assertEqual([(row["task"], row["status"]) for row in schema.read(results / "results.csv")],
                                 [("load", "ok")], "\n".join(log.read_text(encoding="utf-8") for log in logs))
                receipts = [json.loads(path.read_text(encoding="utf-8")) for path in results.glob("driver/*/manifest.json")]
                self.assertEqual([receipt.get("overlay") for receipt in receipts], [self.identity])
