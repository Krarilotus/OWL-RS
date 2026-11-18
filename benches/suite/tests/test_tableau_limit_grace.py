"""A DL tool handed the case's time limit stops itself there and says so; the harness's kill is
only the backstop, so the engine's own answer is what the record keeps.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches/suite"))
sys.path.insert(0, str(ROOT / "benches/fast"))

import fast  # noqa: E402
from suitekit import cases  # noqa: E402

DATA = "dl-transitive-400.owl"
# The tableau's own stop on this case, measured from its process start, reading and normalising
# included: 30.21 s to 31.65 s on its 30 s limit (benches/baselines/fast/2026-10-06-*.json and
# 2026-10-12-ab-pooled-*.json, all `gave-up`). The largest is the run modelled here.
OWN_STOP_S = 31.65


def process_ending_at(seconds: float):
    """fast.container() for a tableau process that prints its answer after `seconds`: the
    harness's kill (exit 124) comes first when its limit is shorter."""
    def run(name, cap_gb, command, env=None, timeout=None):
        if timeout is not None and timeout < seconds:
            return 124, "", f"timed out after {timeout} s", timeout
        return 0, f"{DATA}\tgave-up\twhole_ms={seconds * 1000}\tdl.timeout\n", "", seconds
    return run


def measured(case: dict, ends: float) -> dict:
    with patch.object(fast, "native", side_effect=lambda text: text), \
            patch.object(fast, "container", side_effect=process_ending_at(ends)):
        return fast.measure(case, "contract", 1, {}, {}, DATA)


class ToolLimit(unittest.TestCase):
    """Spec: dl-transitive-400 (benches/cases/core.toml) declares `outcome = "gave-up"`: the
    engine stops itself at the case's limit and says so (fast.py: the tool's answer `gave-up`).
    The harness hands the tool that limit; its own kill is a backstop after it, so a tool that
    honours its limit records `gave-up`, and one that doesn't still ends as `timeout`."""

    def test_the_engine_s_own_stop_is_recorded(self):
        case = next(c for c in cases.load() if c["name"] == "dl-transitive-400")
        self.assertEqual((case["outcome"], case["timeout_s"]), ("gave-up", 30))
        record = measured(case, OWN_STOP_S)
        self.assertEqual(record["status"], "gave-up")
        self.assertEqual(fast.not_as_expected([{**record, "expected_outcome": "gave-up"}]), [])

    def test_a_tool_that_ignores_its_limit_is_a_timeout(self):
        case = next(c for c in cases.load() if c["name"] == "dl-transitive-400")
        record = measured(case, float("inf"))
        self.assertEqual(record["status"], "timeout")
        self.assertNotEqual(fast.not_as_expected([{**record, "expected_outcome": "gave-up"}]), [])


if __name__ == "__main__":
    unittest.main()
