"""A runtime-only benchmark host must never build a missing client implicitly."""
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit.driver import Suite


class PrebuiltHarness(unittest.TestCase):
    def test_skip_build_never_launches_a_build(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / 'harness'
            binary.write_bytes(b'prebuilt fixture')
            for path, expected in [(root / 'missing', False), (root, False), (binary, True)]:
                with self.subTest(path=path):
                    suite = Suite.__new__(Suite)
                    suite.args = SimpleNamespace(skip_build=True)
                    suite.harness = lambda: str(path)
                    suite.say = Mock()
                    suite.host_command = Mock(side_effect=AssertionError('unexpected build'))
                    self.assertEqual(suite.build_harness(), expected)
                    suite.host_command.assert_not_called()
                    if not expected:
                        self.assertIn('--skip-build requires an existing harness', suite.say.call_args.args[0])
