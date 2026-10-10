"""Linux quiet-slot contracts, without observing or waiting for real compilers.

Run: python3 -m unittest discover -s benches/suite/tests -p test_quiet_slot.py
"""

import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


GUARD = Path(__file__).resolve().parents[3] / "scripts" / "quiet-slot.sh"


@unittest.skipUnless(sys.platform.startswith("linux"), "Linux process-probe contract")
class QuietSlot(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="nrese-quiet-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        # A private PATH excludes tasklist and any host-specific process wrappers.
        for name in ("mkdir", "rm", "cat", "date", "grep"):
            (self.bin / name).symlink_to(shutil.which(name))
        self.bash = shutil.which("bash")
        self.script("pgrep", """
case $(cat "$PROBE_STATE") in
  quiet) [ "$1" != -c ] || echo 0; exit 1 ;;
  busy) echo 1; exit 0 ;;
  error) exit 2 ;;
esac
""")
        self.script("sleep", 'echo quiet > "$PROBE_STATE"; echo slept > "$SLEPT"')
        self.lock = self.root / "slot"
        self.marker = self.root / "measurement"
        self.state = self.root / "probe-state"

    def script(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)

    def run_guard(self, state, drain=0, command_exit=0):
        self.state.write_text(state)
        result = subprocess.run(
            [self.bash, str(GUARD), "/bin/sh", "-c",
             'printf %s "$NRESE_QUIET_HOLDER" > "$MARKER"; exit "$COMMAND_EXIT"'],
            env={**os.environ, "PATH": str(self.bin), "PROBE_STATE": str(self.state),
                 "SLEPT": str(self.root / "slept"), "MARKER": str(self.marker),
                 "COMMAND_EXIT": str(command_exit), "NRESE_QUIET_DIR": str(self.lock),
                 "NRESE_QUIET_DRAIN_S": str(drain)},
            capture_output=True, text=True, timeout=5,
        )
        self.assertFalse(self.lock.exists(), "quiet slot must be released")
        return result

    def test_no_compilers_runs_without_count_warning(self):
        result = self.run_guard("quiet")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(self.marker.read_text(), "1")

    def test_drain_timeout_never_starts_measurement(self):
        result = self.run_guard("busy")
        self.assertEqual(result.returncode, 124, result.stderr)
        self.assertIn("measurement not started", result.stderr)
        self.assertFalse(self.marker.exists())
        self.assertFalse((self.root / "slept").exists())

    def test_compilers_can_drain_before_measurement(self):
        result = self.run_guard("busy", drain=600)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertTrue((self.root / "slept").exists())
        self.assertEqual(self.marker.read_text(), "1")

    def test_inspection_error_never_starts_measurement(self):
        result = self.run_guard("error")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("cannot inspect", result.stderr)
        self.assertFalse(self.marker.exists())

    def test_measurement_exit_status_is_preserved(self):
        result = self.run_guard("quiet", command_exit=37)
        self.assertEqual(result.returncode, 37, result.stderr)
        self.assertEqual(self.marker.read_text(), "1")
