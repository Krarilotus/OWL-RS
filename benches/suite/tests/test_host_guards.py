"""Khaleesi's guard (scripts/hosts/khaleesi-guard.py): when the owner counts as present.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import importlib.util
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

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

    def test_a_table_that_is_not_whole_is_unknown(self):
        # PR #52 review F1: only `query user`'s "no user" answer or a whole table says nobody is at the console.
        header = ENGLISH.splitlines()[0]
        for name, table in (("header only", header + "\n"), ("cut before the session", header + "\n>owner\n"),
                            ("a row that isn't one", header + "\n<unreadable>\n"),
                            ("a non-console row cut short", header + "\n other   rdp-tcp#1   2  Active\n")):
            self.assertTrue(guard.present(table, input_idle=60.0), name)

    def test_an_empty_answer_with_success_is_unknown_and_no_user_is_nobody(self):
        # PR #52 review F1: exit 0 with nothing on stdout is a missing reading, not "nobody logged on".
        cases = ((0, " \n", "", None), (1, "", "No User exists for *", ""), (1, "", "Kein Benutzer vorhanden", ""))
        for code, out, err, said in cases:
            with mock.patch.object(guard.subprocess, "run", return_value=subprocess.CompletedProcess([], code, out, err)):
                self.assertEqual(guard.query_user(), said, (code, out, err))


class Main(unittest.TestCase):
    """main() over real files: a stale RETREAT (written at epoch 0) and an optional FREE-UNTIL."""

    def run_main(self, query, free=None, input_idle=60.0, retreat_text="0"):
        root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, root, ignore_errors=True)
        retreat, free_file = root / "RETREAT", root / "FREE-UNTIL"
        retreat.write_text(retreat_text, encoding="utf-8")
        self.retreat = retreat
        if free is not None:
            free_file.write_text(free, encoding="utf-8")
        settings = {"dir": str(root), "retreat": str(retreat), "free": str(free_file)}
        with mock.patch.object(guard, "queue_settings", return_value=settings), \
                mock.patch.object(guard, "query_user", return_value=query), \
                mock.patch.object(guard, "last_input_minutes", return_value=input_idle), \
                mock.patch.object(guard, "stop_timed") as stop:
            self.assertEqual(guard.main(), 0)
        kept = retreat.exists() and retreat.read_text(encoding="utf-8") != retreat_text
        return kept, stop.called

    def lines(self):
        return self.retreat.read_text(encoding="utf-8").splitlines()

    def test_retreat_says_its_readings_state_on_its_first_line(self):
        # The fleet's F31 (fleet-issues.md): `present` for a known reading, `unknown: <why>` otherwise; then the time.
        # Each run starts from a stale RETREAT ("0"), so the unknown cases are the refreshed-unknown path.
        for query, input_idle, first in (
                (ENGLISH.format(idle="none"), 0.5, "present"),
                (None, 60.0, "unknown: query user gave no answer"),
                ("Fehler 0x000006BA beim Abrufen\n", 60.0, "unknown: query user's answer isn't a whole table"),
                (ENGLISH.format(idle="??"), 60.0, "unknown: query user's answer isn't a whole table")):
            self.assertEqual(self.run_main(query, input_idle=input_idle), (True, True), first)
            self.assertEqual(self.lines()[0], first)
            self.assertTrue(self.lines()[1].isdigit(), self.lines())
            self.assertEqual(list(self.retreat.parent.glob("RETREAT.*")), [], "written beside it, then renamed")

    def test_a_retreat_that_cant_be_replaced_is_written_in_place_and_timed_jobs_still_stop(self):
        # sol-loop r4 S1: on Windows a reader holding RETREAT open denies os.replace (WinError 5) but not a write in
        # place; the denial crashed main() before stop_timed and left RETREAT.new behind.
        for query, first in ((ENGLISH.format(idle="none"), "present"), (None, "unknown: query user gave no answer")):
            with mock.patch.object(guard.os, "replace", side_effect=PermissionError(13, "denied")):
                self.assertEqual(self.run_main(query, input_idle=0.5, retreat_text="present\n0\n"), (True, True))
            self.assertEqual(self.lines()[0], first)
            self.assertEqual(list(self.retreat.parent.glob("RETREAT.*")), [])

    # PR #52 fix check 1 F4: a Windows account name may hold a space; the fix read rows by token count, so whole
    # tables with `other user` became unknown and RETREAT stayed forever. The reviewer's tables
    # (notes/review-pr52-guard-port-fix1/complete_names.py), English and German.
    LOCALES = ((ENGLISH.splitlines()[0], "Active", "Disc", "10/12/2026 9:02 AM"),
               (GERMAN.splitlines()[0], "Aktiv", "Getrennt", "12.10.2026 09:02"))

    def test_a_user_name_with_a_space_in_another_session_is_nobody_at_the_console(self):
        for header, active, disconnected, logon in self.LOCALES:
            for user, session, state in (("other user", "rdp-tcp#1", active), ("other user", None, disconnected),
                                         ("x console", None, disconnected)):  # a name's word is no session
                table = f"{header}\n {user:<22}{session or '':<19}2  {state:<8} none  {logon}\n"
                self.assertEqual(self.run_main(table, input_idle=0.5), (False, False), table)

    def test_a_user_name_with_a_space_at_the_console_is_known_presence(self):
        for header, active, _, logon in self.LOCALES:
            console = f"{header}\n>{'other user':<22}{'console':<19}2  {active:<8} none  {logon}\n"
            self.assertEqual(self.run_main(console, input_idle=0.5), (True, True), console)
            self.assertEqual(self.lines()[0], "present", console)
            self.assertEqual(self.run_main(console, free="2999-01-01T00:00:00", input_idle=0.5), (False, False))
        # A row off the header's columns (the reviewer's mixed table): a console session is still one, never a
        # user called "owner console".
        mixed = ENGLISH.splitlines()[0] + "\n other 3 Disc 1:05 10/12/2026 9:02 PM\n>owner console 1 Active none 10/12/2026 9:02 AM\n"
        self.assertEqual(self.run_main(mixed, input_idle=0.5), (True, True), mixed)

    def test_the_quiet_time_is_read_from_retreats_time_line(self):
        # Nobody at the console: a RETREAT written just now stays its 15 minutes, one from long ago goes.
        nobody = ENGLISH.splitlines()[0] + "\n other   rdp-tcp#1   2  Active  none  10/12/2026 9:02 AM\n"
        recent = f"present\n{int(time.time())}\n"
        self.run_main(nobody, retreat_text=recent)
        self.assertEqual(self.retreat.read_text(encoding="utf-8"), recent)
        self.run_main(nobody, retreat_text="present\n0\n")
        self.assertFalse(self.retreat.exists())

    def test_an_incomplete_table_keeps_and_refreshes_retreat(self):
        # PR #52 review F1: a header-only table deleted a stale RETREAT; sol-loop r2 S1: so did a row whose logon
        # time is cut short. Whole rows of every form still say nobody is at the console.
        header = ENGLISH.splitlines()[0]
        for table in (header + "\n", header + "\n other   rdp-tcp#1   2  Active  none  10/12/2026 9:\n"):
            self.assertEqual(self.run_main(table), (True, True), table)
        for table in (header + "\n other   rdp-tcp#1   2  Active  none  10/12/2026 9:02 AM\n",
                      header + "\n other   2  Disc  1:05  10/12/2026 9:02 PM\n",
                      GERMAN.splitlines()[0] + "\n owner  2  Getrennt  1:05  12.10.2026 09:02\n"):
            self.assertEqual(self.run_main(table), (False, False), table)

    def test_a_release_lifts_known_presence_only(self):
        # PR #52 review F2: FREE-UNTIL deleted RETREAT whatever the reading; an unknown one keeps it.
        unknown = (None, "Fehler 0x000006BA beim Abrufen\n", ENGLISH.format(idle="??"))
        for query in unknown:
            self.assertEqual(self.run_main(query, free=""), (True, True), query)
        self.assertEqual(self.run_main(ENGLISH.format(idle="none"), free="", input_idle=0.5), (False, False))

    def test_a_release_time_with_an_offset_is_no_release_and_never_a_crash(self):
        # PR #52 review F3: an ISO time with an offset raised TypeError before RETREAT was written.
        for until in ("2999-01-01T00:00:00+01:00", "2999-01-01T00:00:00Z"):
            self.assertEqual(self.run_main(ENGLISH.format(idle="none"), free=until, input_idle=0.5), (True, True))

    def test_a_release_file_that_cant_be_read_or_removed_is_no_release(self):
        # PR #52 review F3: a failed read or a failed removal of a passed release must not stop the protection.
        with mock.patch.object(guard.Path, "read_text", side_effect=PermissionError("denied")):
            self.assertIsNone(guard.free_until({"free": __file__}))
        root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, root, ignore_errors=True)
        (root / "FREE-UNTIL").write_text("2000-01-01T00:00", encoding="utf-8")
        with mock.patch.object(guard.Path, "unlink", side_effect=PermissionError("denied")):
            self.assertIsNone(guard.free_until({"free": str(root / "FREE-UNTIL")}))

    def test_a_release_file_whose_existence_cant_be_checked_still_lets_retreat_be_written(self):
        # sol-loop r2 S2: Path.exists() raised PermissionError past the boundary and main() wrote no RETREAT. A file
        # whose stat is denied can't be read either.
        def denied(real):
            def call(path, *args, **kwargs):
                if path.name == "FREE-UNTIL":
                    raise PermissionError("denied")
                return real(path, *args, **kwargs)
            return call

        with mock.patch.object(guard.Path, "exists", autospec=True, side_effect=denied(guard.Path.exists)), \
                mock.patch.object(guard.Path, "read_text", autospec=True, side_effect=denied(guard.Path.read_text)):
            self.assertEqual(self.run_main(ENGLISH.format(idle="none"), free="", input_idle=0.5), (True, True))


if __name__ == "__main__":
    unittest.main()
