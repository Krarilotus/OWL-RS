"""Khaleesi's guard (scripts/hosts/khaleesi-guard.py): when the owner counts as present.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import contextlib
import importlib.util
import shutil
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

spec = importlib.util.spec_from_file_location(
    "khaleesi_guard", Path(__file__).resolve().parents[3] / "scripts" / "hosts" / "khaleesi-guard.py")
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

OTHER_STATES = (2, 3, 6, 7, 8, 9, 42)  # ConnectQuery, Shadow, Listen, Reset, Down, Init, and one Windows doesn't name


@contextlib.contextmanager
def api(session=1, state=guard.ACTIVE, user="owner", fails=None, own=None):
    """The session API answering so; `own`: the guard's own session (default: the console's). `fails` names the call
    that raises ("id", "state", "user", "own" or "windows")."""
    def session_id():
        if fails == "id":
            raise OSError("denied")
        if fails == "windows":
            raise AttributeError("module 'ctypes' has no attribute 'WinDLL'")
        return session

    def info(_, which):
        if fails == ("state" if which == guard.CONNECT_STATE else "user"):
            raise OSError("[WinError 7022] The specified session cannot be found.")
        return state if which == guard.CONNECT_STATE else user

    def own_session():
        if fails == "own":
            raise OSError("denied")
        return session if own is None else own

    with mock.patch.object(guard, "console_session_id", side_effect=session_id), \
            mock.patch.object(guard, "session_info", side_effect=info), \
            mock.patch.object(guard, "own_session_id", side_effect=own_session):
        yield


AT_CONSOLE = {"state": guard.ACTIVE, "user": "owner"}
NOBODY = {"session": guard.NO_CONSOLE}
UNKNOWN = ({"fails": "id"}, {"fails": "state"}, {"fails": "user"}, {"fails": "windows"}, {"fails": "own"},
           {"session": 2, "own": 1},  # sol-loop r1 S1: fast user switching, the guard's last input isn't the console's
           {"state": guard.ACTIVE, "user": ""}, {"state": guard.DISCONNECTED, "user": "owner"},
           *({"state": s, "user": u} for s in OTHER_STATES for u in ("owner", "")))


class Console(unittest.TestCase):
    def test_a_user_in_an_active_or_connected_console_session_is_someone(self):
        for state in (guard.ACTIVE, guard.CONNECTED):
            with api(state=state, user="other user"):
                self.assertEqual(guard.console(), (True, None), state)

    def test_no_console_session_or_one_without_a_user_is_nobody(self):
        # The logon screen (connected, no user) is nobody, as `query user` said (no console row).
        with api(**NOBODY):
            self.assertEqual(guard.console(), (False, None))
        for state in (guard.CONNECTED, guard.DISCONNECTED, guard.IDLE):
            with api(state=state, user=""):
                self.assertEqual(guard.console(), (False, None), state)

    def test_a_failed_call_or_any_other_state_is_unknown(self):
        # PR #52's outer view: only a successful call and a known state say anything; nothing else is ever nobody.
        for reading in UNKNOWN:
            with api(**reading):
                someone, why = guard.console()
            self.assertIsNone(someone, reading)
            self.assertTrue(why and "\n" not in why, (reading, why))

    def test_the_last_input_decides_for_someone_at_the_console(self):
        self.assertTrue(guard.present(True, 0.5))
        self.assertFalse(guard.present(True, 15.0))
        self.assertTrue(guard.present(True, None))  # the last input can't be read: present
        self.assertFalse(guard.present(False, 0.5))
        self.assertTrue(guard.present(None, 60.0))  # unknown is present whatever the last input says


class Main(unittest.TestCase):
    """main() over real files: a stale RETREAT (written at epoch 0) and an optional FREE-UNTIL."""

    def run_main(self, reading, free=None, input_idle=60.0, retreat_text="0"):
        root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, root, ignore_errors=True)
        retreat, free_file = root / "RETREAT", root / "FREE-UNTIL"
        retreat.write_text(retreat_text, encoding="utf-8")
        self.retreat = retreat
        if free is not None:
            free_file.write_text(free, encoding="utf-8")
        settings = {"dir": str(root), "retreat": str(retreat), "free": str(free_file)}
        with mock.patch.object(guard, "queue_settings", return_value=settings), api(**reading), \
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
        for reading, first in (
                (AT_CONSOLE, "present"),
                ({"fails": "id"}, "unknown: the console session can't be read (denied)"),
                ({"state": 7, "user": "owner"}, "unknown: the console session is in state 7, with a user")):
            self.assertEqual(self.run_main(reading, input_idle=0.5), (True, True), first)
            self.assertEqual(self.lines()[0], first)
            self.assertTrue(self.lines()[1].isdigit(), self.lines())
            self.assertEqual(list(self.retreat.parent.glob("RETREAT.*")), [], "written beside it, then renamed")

    def test_an_unknown_reading_keeps_and_refreshes_retreat_through_a_release(self):
        # PR #52 review F1 and F2 (and fix checks 1-2's F4, F5): no reading the API can't vouch for is nobody, and
        # the owner's release lifts a known reading only.
        for reading in UNKNOWN:
            for free in (None, "", "2999-01-01T00:00:00"):
                self.assertEqual(self.run_main(reading, free=free), (True, True), (reading, free))
        self.assertEqual(self.run_main(AT_CONSOLE, free="", input_idle=0.5), (False, False))

    def test_nobody_at_the_console_lets_a_stale_retreat_go(self):
        for reading in (NOBODY, {"state": guard.CONNECTED, "user": ""}, AT_CONSOLE):  # the last: idle 60 minutes
            self.assertEqual(self.run_main(reading), (False, False), reading)
            self.assertFalse(self.retreat.exists(), reading)

    def test_a_retreat_that_cant_be_replaced_is_written_in_place_and_timed_jobs_still_stop(self):
        # sol-loop r4 S1: on Windows a reader holding RETREAT open denies os.replace (WinError 5) but not a write in
        # place; the denial crashed main() before stop_timed and left RETREAT.new behind.
        for reading, first in ((AT_CONSOLE, "present"), ({"fails": "id"}, "unknown: the console session can't be read (denied)")):
            with mock.patch.object(guard.os, "replace", side_effect=PermissionError(13, "denied")):
                self.assertEqual(self.run_main(reading, input_idle=0.5, retreat_text="present\n0\n"), (True, True))
            self.assertEqual(self.lines()[0], first)
            self.assertEqual(list(self.retreat.parent.glob("RETREAT.*")), [])

    def test_the_quiet_time_is_read_from_retreats_time_line(self):
        # Nobody at the console: a RETREAT written just now stays its 15 minutes, one from long ago goes.
        recent = f"present\n{int(time.time())}\n"
        self.run_main(NOBODY, retreat_text=recent)
        self.assertEqual(self.retreat.read_text(encoding="utf-8"), recent)
        self.run_main(NOBODY, retreat_text="present\n0\n")
        self.assertFalse(self.retreat.exists())

    def test_a_release_time_with_an_offset_is_no_release_and_never_a_crash(self):
        # PR #52 review F3: an ISO time with an offset raised TypeError before RETREAT was written.
        for until in ("2999-01-01T00:00:00+01:00", "2999-01-01T00:00:00Z"):
            self.assertEqual(self.run_main(AT_CONSOLE, free=until, input_idle=0.5), (True, True))

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
            self.assertEqual(self.run_main(AT_CONSOLE, free="", input_idle=0.5), (True, True))


if __name__ == "__main__":
    unittest.main()
