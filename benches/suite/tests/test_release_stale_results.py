"""The release verdict stands only for the run that wrote it (D16-490).

Spec (D16-490): "a failed step can leave an older PASSED report in place, so a verdict could be
read from a stale file." release-gate.sh's verdict step removes its earlier verdict.md before
anything that can fail, as its report, w3c and guards steps remove theirs: a verdict step that
stops at any point leaves none, never an earlier PASSED one.
"""
from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
PASSED = "# Release verdict\n\n**PASSED.**\n"


class StaleVerdict(unittest.TestCase):

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="stale-verdict-"))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)

    def step(self, *args: str) -> subprocess.CompletedProcess:
        """release-gate.sh verdict ARGS, the step as checked in, its OUT this test's directory."""
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("needs bash")
        body = (ROOT / "scripts/release-gate.sh").read_text(encoding="utf-8").split("\nverdict)\n", 1)[1] \
            .split("\n  ;;", 1)[0]
        script = self.tmp / "verdict-step.sh"
        script.write_text('set -uo pipefail\nOUT="$1"\nlaunch="$2"\nCODE="$2"\nREALPY="$3"\nshift 3\n'
                          'py() { "$REALPY" -B "$@"; }\nset -- verdict "$@"\n' + body + "\n",
                          encoding="utf-8", newline="\n")
        return subprocess.run([bash, script.as_posix(), self.tmp.as_posix(), ROOT.as_posix(),
                               Path(sys.executable).as_posix(), *args],
                              capture_output=True, text=True, encoding="utf-8", timeout=120)

    def test_a_verdict_step_that_fails_leaves_no_earlier_verdict(self):
        timed, checks, bare = self.tmp / "timed", self.tmp / "checks", self.tmp / "bare"
        for d in (timed, checks, bare):
            d.mkdir()
        (timed / "release-gate.json").write_text(json.dumps({"passed": True}), encoding="utf-8")
        for name in ("w3c.json", "guards.json"):
            (checks / name).write_text(json.dumps({"passed": True}), encoding="utf-8")
        for name, args in (("no parts named", ()),
                           ("a timed part without its report", ("HEAD", bare.as_posix(), checks.as_posix())),
                           ("a candidate that is no commit", ("refs/heads/no-such-candidate-d16-490",
                                                              timed.as_posix(), checks.as_posix()))):
            with self.subTest(name):
                out = self.tmp / "verdict.md"
                out.write_text(PASSED, encoding="utf-8")
                done = self.step(*args)
                self.assertFalse(out.exists(), f"SPEC: the earlier PASSED verdict is gone\n{done.stderr[-500:]}")
                self.assertEqual(done.returncode, 2, done.stderr[-500:])


if __name__ == "__main__":
    unittest.main()
