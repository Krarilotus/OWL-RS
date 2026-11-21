"""The crate scan (P2): every compile-time variable our Rust sources read is one cargo sets for the
build, or one the suite's controlled build environment (suitekit/driver.py controlled_environment)
never passes. A variable a source reads at compile time (env!, option_env!) or a build script reads
(env::var) changes the binary; a new one fails here until it is one of the two.

The gate's own step, "crate scan" (scripts/check.sh): where a change reaches it (READS, see
contracts.py), and in every run of all the contracts. Not a test_*.py, so a run of all the contracts
by discovery doesn't run it a second time.

Run: python benches/suite/tests/isolation.py crate_scan
"""
from __future__ import annotations

import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
from suitekit import driver  # noqa: E402

READS = ("crates/**/src/**/*.rs", "crates/**/build.rs", "benches/nrese-bench-harness/src/**/*.rs",
         "benches/suite/suitekit/")
CARGO_SETS = re.compile(r"^(CARGO_.*|OUT_DIR|TARGET|HOST|PROFILE|OPT_LEVEL|DEBUG|NUM_JOBS|RUSTC|RUSTDOC)$")


class CrateScan(unittest.TestCase):
    def test_every_compile_time_variable_our_sources_read_is_set_by_cargo_or_never_passed(self):
        names = set()
        sources = [*ROOT.glob("crates/**/src/**/*.rs"), *ROOT.glob("benches/nrese-bench-harness/src/**/*.rs")]
        for path in sources:
            names |= set(re.findall(r'(?:option_env|env)!\(\s*"([A-Za-z0-9_]+)"', path.read_text(encoding="utf-8")))
        for path in ROOT.glob("crates/**/build.rs"):
            names |= set(re.findall(r'env::var(?:_os)?\(\s*"([A-Za-z0-9_]+)"', path.read_text(encoding="utf-8")))
        self.assertIn("NRESE_BUILD_REVISION", names)  # the scan finds today's one (draft_check.rs)
        for name in sorted(names):
            if not CARGO_SETS.match(name):
                with self.subTest(name):
                    self.assertNotIn(name, driver.controlled_environment({name: "x"}))


if __name__ == "__main__":
    unittest.main()
