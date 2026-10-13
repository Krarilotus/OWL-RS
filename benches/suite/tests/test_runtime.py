"""The runtime's helpers (suitekit/runtime.py).

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from suitekit.runtime import run_quiet  # noqa: E402


class Scopes(unittest.TestCase):
    """The kit rule of 14 Oct 2026 (a whole-host OOM): a scope the kit starts joins the
    fleet's step slice where FLEET_SLICE is set, and the user's default slice where not."""

    def test_a_scope_joins_the_fleet_slice(self):
        import os
        from unittest.mock import patch
        from suitekit.runtime import scope_argv
        with patch.dict(os.environ, {"FLEET_SLICE": "fleet-step-7.slice"}):
            argv = scope_argv("case-1", "6g")
        self.assertIn("--slice=fleet-step-7.slice", argv)
        self.assertLess(argv.index("--slice=fleet-step-7.slice"), argv.index("--"))
        self.assertIn("MemoryMax=6G", argv)
        with patch.dict(os.environ, {}, clear=False):
            os.environ.pop("FLEET_SLICE", None)
            argv = scope_argv("case-1", "6g")
        self.assertFalse(any(a.startswith("--slice") for a in argv))


class RunQuiet(unittest.TestCase):
    def test_output_is_read_as_utf8_whatever_the_console_code_page(self):
        # 0x9d has no character in Windows' cp1252: read with the console's code page, the
        # reader thread failed and stdout was None (a container log of Jena's, 12 Oct 2026).
        script = "import sys; sys.stdout.buffer.write('– ok'.encode() + bytes([0x9d]))"
        done = run_quiet([sys.executable, "-c", script])
        self.assertIsInstance(done.stdout, str)
        self.assertTrue(done.stdout.startswith("– ok"))


if __name__ == "__main__":
    unittest.main()
