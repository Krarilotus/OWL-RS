#!/usr/bin/env python3
"""Khaleesi's guard (the owner's rule, 12 Oct 2026): while the owner uses the machine, no timed
job runs. Task Scheduler starts it every minute (pythonw, no window); it is a script, not an
agent, like the office PC's ~/owlrs/guard.sh.

The owner counts as present while someone is at the console and its last input (mouse,
keyboard) is less than IDLE_MINUTES ago. The last input is the session's own
(GetLastInputInfo: the task runs in the owner's interactive session); `query user` only says
whether someone is at the console, because its idle column stays "none" for an active
console session however long nobody touches it (13 Oct 2026: "none" for a console idle 21
minutes by its last input; the guard never released in ten hours). Where the last input
can't be read, `query user`'s idle time decides.
Then:
- the queue's retreat file (benches/hosts.toml khaleesi queue.retreat) is written, with the
  time: the measurement queue starts no timed job while it exists;
- every running timed job is stopped, its whole process tree (taskkill /T /F), by the pid
  file the runner keeps (queue/running/<id>.pid); the runner sees it stopped with the owner
  present, keeps its partial results and queues it again.
The retreat file goes once nobody has been present for IDLE_MINUTES. Nothing is restarted
here; the runner starts the job again in the next window.

The owner can free the machine whatever the console shows (13 Oct 2026): while the free file
(hosts.toml khaleesi queue.free, S:/nrese-scratch/FREE-UNTIL) holds a time that hasn't passed
(HH:MM today, or ISO; empty: until it's removed), nobody counts as present, and the runner
takes timed jobs outside its windows too. A passed time removes the file.

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


def idle_minutes(text: str) -> int | None:
    """`query user`'s idle time as minutes ("none" or "Kein" or ".": 0; "5"; "1:02";
    "1+02:03"), or None if it isn't one."""
    value = text.strip().lower()
    if value in ("none", "kein", ".", "keine"):
        return 0
    m = re.fullmatch(r"(?:(\d+)\+)?(?:(\d+):)?(\d+)", value)
    if not m:
        return None
    days, hours, minutes = (int(x) if x else 0 for x in m.groups())
    return days * 1440 + hours * 60 + minutes


# A console idle time that can't be read: it counts as present (round 14 check-b).
UNREADABLE = -1


# `query user`'s header, English or German: output without it isn't its table.
HEADERS = ("username", "benutzername")


def console_idle(output: str) -> int | None:
    """The console session's idle minutes from `query user`'s table, UNREADABLE if they can't be read
    (its idle time isn't one, its row is cut short, or the output isn't the table), or None if no one
    is at the console ("" : nobody logged on). The columns: user, session, id, state, idle time, logon
    time."""
    lines = [l for l in output.splitlines() if l.strip()]
    if not lines:
        return None
    if not lines[0].strip().lower().startswith(HEADERS):
        return UNREADABLE
    for line in lines[1:]:
        fields = line.lstrip(">").split()
        if len(fields) >= 2 and fields[1].lower() == "console":
            idle = idle_minutes(fields[4]) if len(fields) >= 5 else None
            return UNREADABLE if idle is None else idle
    return None


def query_user() -> str | None:
    """`query user`'s table ("" when nobody is logged on), or None when it couldn't say (no answer
    within its deadline, another failure): None counts as present (round 14 check-b)."""
    try:
        done = subprocess.run(["query", "user"], capture_output=True, text=True, encoding="oem", errors="replace",
                              creationflags=HIDDEN, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return None
    if done.returncode == 0:
        return done.stdout
    said = f"{done.stdout}\n{done.stderr}".lower()
    # Nobody logged on: it says so and exits 1 ("No User exists for *", "Kein Benutzer ...").
    if done.returncode == 1 and ("no user exists" in said or "kein benutzer" in said):
        return ""
    return None


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


def present(output: str | None, input_idle: float | None = None) -> bool:
    """Someone at the console (`query user`), and their last input less than IDLE_MINUTES ago
    (`input_idle`, minutes; else `query user`'s idle column). Unknown is present: `query user`
    couldn't say (None), or the console's idle time can't be read."""
    if output is None:
        return True
    idle = console_idle(output)
    if idle is None:
        return False
    # Unknown before the last input's word (Sol r14 C1: it may speak of another session).
    if idle == UNREADABLE:
        return True
    return (input_idle if input_idle is not None else idle) < IDLE_MINUTES


def free_until(settings: dict) -> str | None:
    """The time the owner freed the machine until, while it hasn't passed (the free file)."""
    import datetime
    path = Path(settings.get("free", "")) if settings.get("free") else None
    if path is None or not path.exists():
        return None
    text = path.read_text(encoding="utf-8").strip()
    if not text:
        return "the file's removal"
    now = datetime.datetime.now()
    try:
        until = (datetime.datetime.combine(now.date(), datetime.time.fromisoformat(text)) if len(text) <= 5
                 else datetime.datetime.fromisoformat(text))
    except ValueError:
        return None
    if now >= until:
        path.unlink(missing_ok=True)
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


def main() -> int:
    settings = queue_settings()
    directory = Path(settings["dir"])
    retreat = Path(settings["retreat"])
    log = open(directory / "guard.log", "a", encoding="utf-8")

    def say(text: str):
        log.write(f"{time.strftime('%F %T')} {text}\n")

    query = query_user()
    now = int(time.time())
    freed = free_until(settings)
    if freed:
        if retreat.exists():
            retreat.unlink()
            say(f"back: the owner freed the machine until {freed}")
        return 0
    if present(query, last_input_minutes()):
        if not retreat.exists():
            say("retreat: " + ("the owner is at the console" if query is not None
                               else "query user couldn't say: counted as present"))
        retreat.write_text(str(now), encoding="utf-8")
        stop_timed(directory, say)
    elif retreat.exists():
        try:
            last = int(retreat.read_text(encoding="utf-8").split()[0])
        except (ValueError, IndexError):
            last = 0
        if now - last >= IDLE_MINUTES * 60:
            retreat.unlink()
            say(f"back: nobody at the console for {IDLE_MINUTES} minutes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
