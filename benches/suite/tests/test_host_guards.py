"""Khaleesi's guard (scripts/hosts/khaleesi-guard.py): when the owner counts as present.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "khaleesi_guard", Path(__file__).resolve().parents[3] / "scripts" / "hosts" / "khaleesi-guard.py")
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

GERMAN = """ BENUTZERNAME          SITZUNGSNAME       ID  STATUS  LEERLAUF   ANMELDEZEIT
 krarilotus            console             1  Aktiv       {idle}   12.10.2026 09:02
"""
ENGLISH = """ USERNAME              SESSIONNAME        ID  STATE   IDLE TIME  LOGON TIME
>owner                 console             1  Active      {idle}  10/12/2026 9:02 AM
"""


class Guard(unittest.TestCase):
    def test_idle_times_in_every_form(self):
        for text, minutes in (("Kein", 0), ("none", 0), (".", 0), ("5", 5), ("1:02", 62), ("1+02:03", 1563)):
            self.assertEqual(guard.idle_minutes(text), minutes, text)

    def test_the_owner_is_present_while_the_console_is_not_idle(self):
        for table in (GERMAN, ENGLISH):
            self.assertTrue(guard.present(table.format(idle="Kein" if table is GERMAN else "none")))
            self.assertTrue(guard.present(table.format(idle="14")))
            self.assertFalse(guard.present(table.format(idle="15")))
            self.assertFalse(guard.present(table.format(idle="2:10")))

    def test_nobody_at_the_console_is_not_present(self):
        disconnected = " BENUTZERNAME   SITZUNGSNAME  ID  STATUS  LEERLAUF  ANMELDEZEIT\n owner  2  Getrennt  1:05  12.10.2026 09:02\n"
        self.assertFalse(guard.present(disconnected))
        self.assertFalse(guard.present(""))


if __name__ == "__main__":
    unittest.main()
