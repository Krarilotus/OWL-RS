"""A classifier process's whole run (fast.py PROCESS_WALL, the series `wall`), timed by the case's
shell, not by the tool: a phase the tool can't resolve, left out of the case's comparison, still
counts (master, 14 Nov 2026 08:12: nothing unobserved after el-classify's prepare was left out)."""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from test_runtime_lifetime import load_fast

CASE = dict(name="w", data="none", args=[], queries="", metric="whole_ms", cap_gb=2, tool="classify-el")
PROFILE = "profile read=10.0 normalise=10.0 prepare=0.0 saturate=60.0 assemble=10.0 write=10.0"


def bash_with_clock() -> bool:
    bash = shutil.which("bash")
    return bool(bash) and bool(subprocess.run([bash, "-c", 'echo "$EPOCHREALTIME"'], capture_output=True,
                                              text=True).stdout.strip())


@unittest.skipUnless(os.name == "posix" and bash_with_clock(), "bash 5 (EPOCHREALTIME) on POSIX")
class Shell(unittest.TestCase):
    def measure(self, tool_body: str, processes: int = 2) -> dict:
        """The case measured by fast.measure with its real shell command, the classifier replaced
        by a script with `tool_body` (its arguments: --out PATH --repeat N DATA)."""
        fast = load_fast()
        with tempfile.TemporaryDirectory() as temporary:
            where = Path(temporary)
            tool = where / "classify"
            tool.write_text("#!/bin/sh\n" + tool_body + "\n")
            tool.chmod(0o755)

            def run(_name, _cap, command, **_):
                script = command[2].replace("/target/release/examples/classify", str(tool)) \
                    .replace("/out/", f"{where}/")
                done = subprocess.run(command[:2] + [script], capture_output=True, text=True)
                return done.returncode, done.stdout, done.stderr, 0.0

            with patch.object(fast, "container", side_effect=run):
                return fast.measure({**CASE, "processes": processes}, "contract", 1, {}, {}, "none")

    def test_the_wall_holds_time_no_printed_phase_does(self):
        # Spec: a process that spends 300 ms, 200 ms of them in no phase it prints (as a prepare
        # below the tool's 0.1 ms step), has a wall of at least 300 ms; its printed phases sum to 100.
        record = self.measure(f'sleep 0.3; echo "{PROFILE}" >&2; printf "a\\tb\\n" > "$2"')
        self.assertEqual(record["status"], "ok", record["notes"])
        walls = record["observations"][0]["samples"]["wall"]
        self.assertEqual(len(walls), 2, walls)
        self.assertTrue(all(300 <= w < 2300 for w in walls), walls)
        self.assertEqual(record["result"]["whole_ms"], 100.0)

    def test_a_killed_classifier_keeps_its_exit_and_its_last_words(self):
        # Spec (fast.py classify_failure): exit 137 is a kill at the memory cap; the record's note
        # is the tool's last line, not the harness's clock.
        record = self.measure('echo "allocation failed" >&2; exit 137', processes=1)
        self.assertEqual(record["status"], "memory-limit")
        self.assertIn("allocation failed", record["notes"][-1])
        self.assertNotIn("fast-process-wall", record["notes"][-1])


class Required(unittest.TestCase):
    def test_a_process_without_its_wall_fails_the_case_at_once(self):
        # Spec: the series is measured on every process, or the case fails loudly (as a scope
        # without an anon + kernel reading, 13 Nov 2026), never an incomplete series in the report.
        fast = load_fast()
        with patch.object(fast, "container", return_value=(0, "abcd\n0\n", PROFILE + "\n", 0.1)) as run:
            record = fast.measure({**CASE, "processes": 3}, "contract", 1, {}, {}, "none")
        self.assertEqual(record["status"], "failed")
        self.assertEqual(run.call_count, 1)
        self.assertIn("no wall time of the classifier process", record["notes"][-1])

    def test_the_locale_s_decimal_comma_is_read(self):
        # Spec (bash(1), EPOCHREALTIME): the decimal point is the locale's.
        self.assertEqual(load_fast().process_wall_ms("x\nfast-process-wall 1,500000 1,750250\n"), ("x\n", 250.25))


if __name__ == "__main__":
    unittest.main()
