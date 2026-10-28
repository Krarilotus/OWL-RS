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

    def test_the_last_input_decides_where_query_user_says_none(self):
        # An active console session's idle column stays "none" however long nobody touches it.
        table = GERMAN.format(idle="Kein")
        self.assertFalse(guard.present(table, input_idle=21.4))
        self.assertTrue(guard.present(table, input_idle=0.5))
        self.assertFalse(guard.present("", input_idle=0.5))

    def test_unknown_is_present(self):
        # Round 14 check-b: a failed or timed-out `query user` (None) or an unreadable idle time kept no RETREAT.
        self.assertTrue(guard.present(None))
        self.assertTrue(guard.present(None, input_idle=60.0))
        unreadable = ENGLISH.format(idle="??")
        self.assertTrue(guard.present(unreadable))
        # Sol r14 C1: unknown stays present whatever the last input says, and a cut-short console row or output
        # that isn't the table is unknown; a valid table without a console session is nobody.
        self.assertTrue(guard.present(unreadable, input_idle=60.0))
        truncated = ENGLISH.splitlines()[0] + "\n>owner                 console             1\n"
        self.assertTrue(guard.present(truncated, input_idle=60.0))
        self.assertTrue(guard.present("Fehler 0x000006BA beim Abrufen\n", input_idle=60.0))
        no_console = ENGLISH.splitlines()[0] + "\n other                 rdp-tcp#1           2  Active      none  10/12/2026 9:02 AM\n"
        self.assertFalse(guard.present(no_console, input_idle=0.5))

    def test_nobody_at_the_console_is_not_present(self):
        disconnected = " BENUTZERNAME   SITZUNGSNAME  ID  STATUS  LEERLAUF  ANMELDEZEIT\n owner  2  Getrennt  1:05  12.10.2026 09:02\n"
        self.assertFalse(guard.present(disconnected))
        self.assertFalse(guard.present(""))


if __name__ == "__main__":
    unittest.main()
