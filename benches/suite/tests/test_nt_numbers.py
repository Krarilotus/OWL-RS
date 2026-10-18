"""benches/sparql/nt-numbers.py: Turtle shorthand objects become typed literals of the same
lexical form; every other line is copied unchanged; anything else stops the conversion."""
from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "benches/sparql/nt-numbers.py"
spec = importlib.util.spec_from_file_location("nt_numbers", SCRIPT)
nt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nt)
S, P = b"<http://e/s>", b"<http://e/p>"
XSD = "http://www.w3.org/2001/XMLSchema#"


class Shorthand(unittest.TestCase):
    def test_bsbm_prices_keep_their_lexical_form_as_doubles(self):
        line, kind = nt.convert_line(S + b" " + P + b" 2685.34e0 .\n")
        self.assertEqual(kind, b"double")
        self.assertEqual(line, S + b" " + P + f' "2685.34e0"^^<{XSD}double> .\n'.encode())

    def test_integers_decimals_and_booleans(self):
        for token, kind in ((b"-12", b"integer"), (b"+0.5", b"decimal"), (b".5", b"decimal"),
                            (b"1E3", b"double"), (b"true", b"boolean")):
            line, found = nt.convert_line(S + b" " + P + b" " + token + b" .\n")
            self.assertEqual(found, kind, token)
            self.assertIn(b'"' + token + b'"^^<', line)

    def test_n_triples_lines_are_copied_byte_for_byte(self):
        for obj in (b"<http://e/o>", b'"a b"', b'"x"@en-GB', b'"1"^^<http://www.w3.org/2001/XMLSchema#int>',
                    b"_:b1", b'"say \\"1\\""'):
            line = S + b" " + P + b" " + obj + b" .\n"
            self.assertEqual(nt.convert_line(line), (line, None), obj)

    def test_anything_else_stops_the_conversion(self):
        for bad in (S + b" " + P + b" ex:o .\n", S + b" " + P + b" <http://e/o>\n", S + b" " + P + b" 12abc .\n"):
            with self.assertRaises(ValueError):
                nt.convert_line(bad)

    def test_a_file_is_converted_with_counts_and_hashes(self):
        with tempfile.TemporaryDirectory() as tmp:
            source, target = Path(tmp) / "in.nt", Path(tmp) / "out.nt"
            source.write_bytes(S + b" " + P + b" 1.5e0 .\n" + S + b" " + P + b' "x" .\n')
            done = subprocess.run([sys.executable, str(SCRIPT), str(source), str(target)],
                                  capture_output=True, text=True)
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertIn("lines 2; shorthand rewritten: 1 double", done.stderr)
            self.assertEqual(target.read_bytes().count(b"^^<" + XSD.encode() + b"double>"), 1)
            self.assertFalse(Path(str(target) + ".partial").exists())
            source.write_bytes(S + b" " + P + b" ex:o .\n")
            done = subprocess.run([sys.executable, str(SCRIPT), str(source), str(target)],
                                  capture_output=True, text=True)
            self.assertEqual(done.returncode, 2)
            self.assertIn("in.nt:1:", done.stderr)


if __name__ == "__main__":
    unittest.main()
