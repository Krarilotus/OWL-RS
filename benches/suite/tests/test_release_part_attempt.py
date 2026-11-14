"""A timed part's report stands only for its part's newest run (D16-490).

Spec (D16-490): "a failed step can leave an older PASSED report in place, so a verdict could be
read from a stale file." A job submitted again under its id keeps its directory, and a fetch copies
over an earlier fetch, so a part's directory can hold an earlier run's release-gate.json when this
run stopped before its report step. Each run of a timed part records its attempt at its first step
(release-gate.sh begin; a confirmation's plan-cases), its report carries the newest attempt beside
it, and the verdict takes a part's report only of that attempt, as it takes the checks' results
(release_gate.py attempt_errors()).
"""
from __future__ import annotations

import contextlib
import io
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import test_release_acceptance as acceptance

ROOT = acceptance.ROOT
gate_module = acceptance.gate_module
A, B = "a1" * 16, "b2" * 16  # two runs' attempt ids (release_gate.py attempt: 32 hex digits)


class PartAttempt(acceptance.Scratch):

    def part(self, name: str, report: dict, attempt: str, records: list[str]) -> Path:
        """A fetched part's directory: its report, of run ATTEMPT, beside the attempt records RECORDS."""
        where = self.tmp / name
        where.mkdir()
        acceptance.write_attempts(where, "timed", [acceptance.attempt_record("timed", r, "t") for r in records])
        (where / "release-gate.json").write_text(json.dumps({**report, "attempt": attempt}), encoding="utf-8")
        return where / "release-gate.json"

    def test_the_verdict_takes_a_part_s_report_only_of_its_newest_run(self):
        a = self.repo.commit(message="candidate")
        tree = self.repo.tree(a)
        w3c, guards = acceptance.checks(a, tree)
        for kind in ("w3c", "guards"):
            acceptance.write_attempts(self.tmp, kind, [acceptance.attempt_record(kind, acceptance.ATTEMPT, tree)])
        first, confirmation = acceptance.rounds(a)
        for name, records, expected in (
                ("this run's report", [A], 0),
                ("a report an earlier run left, this run stopped before its report", [A, B], 1),
                ("a report beside no attempt record", [], 1)):
            with self.subTest(name):
                parts = [self.part(f"first-{len(records)}", first, A, [A]),
                         self.part(f"confirmation-{len(records)}", confirmation, A, records)]
                done = acceptance.verdict_on(self.repo.path, a, self.tmp, parts, w3c, guards)
                self.assertEqual(done.returncode, expected, f"SPEC {name}\n{done.stdout}{done.stderr}")
                if expected:
                    self.assertIn("stale report from an earlier attempt" if records else "no attempt record",
                                  done.stdout)

    def test_a_report_carries_its_part_s_newest_run(self):
        out = self.tmp / "out"
        out.mkdir()
        acceptance.write_attempts(out, "timed", [acceptance.attempt_record("timed", r, "t") for r in (A, B)])
        (self.tmp / "passes").mkdir()
        argv = ["release_gate.py", "report", "--passes", str(self.tmp / "passes"), "--candidate", "cand",
                "--side", "base=" + "b" * 40, "--side", "cand=" + "c" * 40, "--require", "base",
                "--harness-revision", "c" * 40, "--out", str(out / "release-gate.md"),
                "--json", str(out / "release-gate.json")]
        with patch.object(sys, "argv", argv), \
                contextlib.redirect_stdout(io.TextIOWrapper(io.BytesIO(), encoding="utf-8")):
            gate_module.main()
        self.assertEqual(json.loads((out / "release-gate.json").read_text(encoding="utf-8")).get("attempt"), B)

    def test_every_timed_part_s_first_step_records_its_run_first(self):
        with patch.object(gate_module, "harness_tables", acceptance.fixtures.LOCAL_TABLES, create=True):  # a fake harness
            found = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None)
        timed = [job for job in found.values() if job["kind"] == "owlrs.timed"]
        self.assertTrue(timed)
        for job in timed:
            first = job["steps"][0][2]
            self.assertIn("release-gate.sh begin", first, job["id"])
            self.assertLess(first.index("release-gate.sh begin"), first.index("release-gate.sh build"), job["id"])

    def test_a_part_s_first_step_records_its_run(self):
        # begin (a timed part's first step, above) and plan-cases (a confirmation's first step,
        # test_release_gate's confirmations) each record the run's attempt in OUT.
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("needs bash")
        text = (ROOT / "scripts/release-gate.sh").read_text(encoding="utf-8")
        for name in ("begin", "plan-cases"):
            with self.subTest(name):
                cut = text.split(f"\n{name})\n", 1)
                body = cut[1].split("\n  ;;", 1)[0] if len(cut) == 2 else ""
                out = Path(tempfile.mkdtemp(prefix="part-attempt-", dir=self.tmp))
                script = out / "step.sh"
                script.write_text('set -uo pipefail\nOUT="$1"\nREALPY="$2"\nlog() { echo "$*"; }\n'
                                  'py() { "$REALPY" -B "$@"; }\n' f"set -- {name}\n" + body + "\n",
                                  encoding="utf-8", newline="\n")
                # plan-cases without GATE_PLAN refuses after its record: the run began.
                subprocess.run([bash, script.as_posix(), out.as_posix(), Path(sys.executable).as_posix()], cwd=ROOT,
                               capture_output=True, text=True, timeout=120,
                               env=acceptance.isolation.inherit("NRESE_", "FLEET_", "GATE_", PYTHONIOENCODING="utf-8"))
                self.assertEqual([p.name for p in out.glob("timed.attempt.*.json")], ["timed.attempt.000000001.json"],
                                 f"SPEC the {name} step records its run's attempt")


if __name__ == "__main__":
    unittest.main()
