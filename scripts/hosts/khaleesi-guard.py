#!/usr/bin/env python3
"""Khaleesi's guard (the owner's rule, 12 Oct 2026): while the owner uses the machine, no timed
job runs. Task Scheduler starts it every minute (pythonw, no window); it is a script, not an
agent, like the office PC's ~/owlrs/guard.sh.

The owner counts as present while someone is at the console and its last input (mouse,
keyboard) is less than IDLE_MINUTES ago. The last input is the session's own
(GetLastInputInfo: the task runs in the owner's interactive session); Windows' session API
(WTSGetActiveConsoleSessionId, WTSQuerySessionInformationW) says whether a user is at the
console. Where the last input can't be read, a user at the console counts as present.
Then:
- the queue's retreat file (benches/hosts.toml khaleesi queue.retreat) is written: its first
  line `present`, or `unknown: <why>` where the session API couldn't say (the fleet's format,
  fleet-issues.md F31), then the time. The measurement queue starts no timed job while it exists;
- every running timed job is stopped, its whole process tree (taskkill /T /F), by the pid
  file the runner keeps (queue/running/<id>.pid); the runner sees it stopped with the owner
  present, keeps its partial results and queues it again.
The retreat file goes once nobody has been present for IDLE_MINUTES. Nothing is restarted
here; the runner starts the job again in the next window.

The owner can free the machine whatever the console shows (13 Oct 2026): while the free file
(hosts.toml khaleesi queue.free, S:/nrese-scratch/FREE-UNTIL) holds a time that hasn't passed
(HH:MM today, or ISO; empty: until it's removed), nobody counts as present, and the runner
takes timed jobs outside its windows too. A passed time removes the file. The release lifts a
known reading only: while the session API can't say, RETREAT stays.

    pythonw scripts/hosts/khaleesi-guard.py          once (Task Scheduler: every minute)
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
import time
import tomllib
from pathlib import Path

IDLE_MINUTES = 15
HIDDEN = getattr(subprocess, "CREATE_NO_WINDOW", 0)
HOSTS = Path(__file__).resolve().parents[2] / "benches" / "hosts.toml"


# Windows' session API (WtsApi32): the console session's state and user, as data (the orchestrator, 28 Oct 2026,
# PR #52's outer view: three rounds of `query user` text parsing each certified "nobody" wrongly, F1, F4, F5).
CONNECT_STATE, USER_NAME = 8, 5  # WTS_INFO_CLASS
ACTIVE, CONNECTED, DISCONNECTED, IDLE = 0, 1, 4, 5  # WTS_CONNECTSTATE_CLASS
NO_CONSOLE = 0xFFFFFFFF  # WTSGetActiveConsoleSessionId: no session attached to the console


def console_session_id() -> int:
    """The session attached to the physical console, NO_CONSOLE if none; raises OSError where it can't say."""
    import ctypes
    from ctypes import wintypes

    get = ctypes.WinDLL("kernel32").WTSGetActiveConsoleSessionId
    get.restype = wintypes.DWORD
    return get()


def session_info(session: int, info: int) -> int | str:
    """WTSQuerySessionInformationW of `session` on this PC: CONNECT_STATE as a number, USER_NAME as text ("" for
    none); raises OSError where the call fails."""
    import ctypes
    from ctypes import wintypes

    wts = ctypes.WinDLL("wtsapi32", use_last_error=True)
    query = wts.WTSQuerySessionInformationW
    query.argtypes = [wintypes.HANDLE, wintypes.DWORD, ctypes.c_int, ctypes.POINTER(ctypes.c_void_p),
                      ctypes.POINTER(wintypes.DWORD)]
    query.restype = wintypes.BOOL
    buffer, size = ctypes.c_void_p(), wintypes.DWORD()
    if not query(None, session, info, ctypes.byref(buffer), ctypes.byref(size)):
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        if not buffer.value:
            raise OSError("an empty answer")
        return ctypes.wstring_at(buffer.value) if info == USER_NAME else ctypes.c_int.from_address(buffer.value).value
    finally:
        wts.WTSFreeMemory(buffer)


def own_session_id() -> int:
    """The session this guard runs in, whose last input GetLastInputInfo reads; raises OSError where it can't say."""
    import ctypes
    from ctypes import wintypes

    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    session = wintypes.DWORD()
    if not kernel.ProcessIdToSessionId(kernel.GetCurrentProcessId(), ctypes.byref(session)):
        raise ctypes.WinError(ctypes.get_last_error())
    return session.value


def console() -> tuple[bool | None, str | None]:
    """Whether a user is at the console (True), nobody is (False), or None and why it can't say. Nobody: no
    session at the console, or one without a user that is connected (the logon screen, as `query user` said:
    no console row), disconnected or idle. Any failed call or other state is unknown, and so is a user at the
    console in another session than the guard's, whose last input it can't read (fast user switching; sol-loop
    r1 S1). A remote (RDP) session is never the console's."""
    try:
        session = console_session_id()
        if session == NO_CONSOLE:
            return False, None
        state, user = session_info(session, CONNECT_STATE), session_info(session, USER_NAME)
        someone = state in (ACTIVE, CONNECTED) and bool(user)
        own = own_session_id() if someone else session
    except (OSError, AttributeError, ValueError) as e:  # AttributeError: not Windows
        return None, " ".join(f"the console session can't be read ({e})".split())
    if someone and own != session:
        return None, f"the console session ({session}) isn't the guard's ({own}): its last input can't be read"
    if someone:
        return True, None
    if not user and state in (CONNECTED, DISCONNECTED, IDLE):
        return False, None
    return None, f"the console session is in state {state}, {'with' if user else 'without'} a user"


def last_input_minutes() -> float | None:
    """Minutes since this session's last mouse or keyboard input (Windows), else None."""
    try:
        import ctypes
        from ctypes import wintypes

        class LASTINPUTINFO(ctypes.Structure):
            _fields_ = [("cbSize", wintypes.UINT), ("dwTime", wintypes.DWORD)]

        info = LASTINPUTINFO()
        info.cbSize = ctypes.sizeof(info)
        if not ctypes.windll.user32.GetLastInputInfo(ctypes.byref(info)):
            return None
        # Both tick counts wrap at 2**32 ms together.
        return ((ctypes.windll.kernel32.GetTickCount() - info.dwTime) % 2 ** 32) / 60000
    except (AttributeError, OSError):
        return None


def present(someone: bool | None, input_idle: float | None) -> bool:
    """A user at the console (`someone`, from console()) whose last input was less than IDLE_MINUTES ago
    (`input_idle`, minutes; where it can't be read, present). Unknown is present."""
    if someone is None:
        return True
    return someone and (input_idle is None or input_idle < IDLE_MINUTES)


def free_until(settings: dict) -> str | None:
    """The time the owner freed the machine until, while it hasn't passed (the free file). A file that
    can't be read, or a time it can't compare (one with an offset), is no release (PR #52 review F3)."""
    import datetime
    path = Path(settings.get("free", "")) if settings.get("free") else None
    if path is None:
        return None
    try:
        text = path.read_text(encoding="utf-8").strip()
    except (OSError, ValueError):  # no file is no release, and so is one that can't be read
        return None
    if not text:
        return "the file's removal"
    now = datetime.datetime.now()
    try:
        until = (datetime.datetime.combine(now.date(), datetime.time.fromisoformat(text)) if len(text) <= 5
                 else datetime.datetime.fromisoformat(text))
    except ValueError:
        return None
    if until.tzinfo is not None:
        return None
    if now >= until:
        try:
            path.unlink(missing_ok=True)
        except OSError:
            pass
        return None
    return text


def queue_settings() -> dict:
    with open(HOSTS, "rb") as f:
        return tomllib.load(f)["host"]["khaleesi"]["queue"]


def stop_timed(directory: Path, say) -> None:
    """Stops every running timed job by its pid file, its whole process tree."""
    for job in (directory / "running").glob("*.toml"):
        text = job.read_text(encoding="utf-8")
        if re.search(r'^window = "any"', text, re.M):
            continue
        pidfile = job.with_suffix(".pid")
        if not pidfile.exists():
            continue
        pid = pidfile.read_text(encoding="utf-8").strip()
        done = subprocess.run(["taskkill", "/T", "/F", "/PID", pid], capture_output=True, text=True,
                              creationflags=HIDDEN)
        say(f"stopped {job.stem} (pid {pid}): {done.returncode}")


def publish(retreat: Path, text: str, say) -> None:
    """RETREAT's new text: written beside it and renamed, so no reader sees half of it. Where the rename is
    denied (Windows: a reader holds RETREAT open), written in place; a reader then sees at worst a partial first
    line, which the fleet takes as unknown. Never raises: timed jobs are stopped either way (sol-loop r4 S1)."""
    written = retreat.with_name(retreat.name + ".new")
    try:
        written.write_text(text, encoding="utf-8")
        os.replace(written, retreat)
        return
    except OSError:
        pass
    try:
        written.unlink(missing_ok=True)
    except OSError:
        pass
    try:
        retreat.write_text(text, encoding="utf-8")
    except OSError as e:
        say(f"RETREAT couldn't be written ({e}): the next run tries again")


def main() -> int:
    settings = queue_settings()
    directory = Path(settings["dir"])
    retreat = Path(settings["retreat"])
    log = open(directory / "guard.log", "a", encoding="utf-8")

    def say(text: str):
        log.write(f"{time.strftime('%F %T')} {text}\n")

    someone, why = console()
    now = int(time.time())
    # The owner's release lifts a known reading only: an unknown one keeps RETREAT (PR #52 review F2).
    freed = None if why else free_until(settings)
    if freed:
        if retreat.exists():
            retreat.unlink()
            say(f"back: the owner freed the machine until {freed}")
        return 0
    if present(someone, last_input_minutes()):
        # Its first line is the reading's state, the fleet's to read (fleet-issues.md F31); then the time. Written
        # beside it and renamed, so no reader sees half of it.
        state = f"unknown: {why}" if why else "present"
        if not retreat.exists():
            say("retreat: " + ("the owner is at the console" if not why else f"{why}: counted as present"))
        publish(retreat, f"{state}\n{now}\n", say)
        stop_timed(directory, say)
    elif retreat.exists():
        try:
            last = next((int(l) for l in retreat.read_text(encoding="utf-8").split() if l.isdigit()), 0)
        except (OSError, ValueError):
            last = 0
        if now - last >= IDLE_MINUTES * 60:
            retreat.unlink()
            say(f"back: nobody at the console for {IDLE_MINUTES} minutes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
