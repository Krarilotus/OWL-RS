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
- the queue's retreat file (benches/hosts.toml khaleesi queue.retreat) is written: its first
  line `present`, or `unknown: <why>` where `query user` couldn't say (the fleet's format,
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
known reading only: while `query user` can't say, RETREAT stays.

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


# A row's logon time: a date and a time, with AM or PM where the locale has them.
LOGON = re.compile(r"\d{1,4}[./-]\d{1,2}[./-]\d{1,4} \d{1,2}:\d{2}(?: [AaPp][Mm])?")


# A session name by its form (where the columns don't say): the console, services, or a protocol's `name#N`.
SESSION = re.compile(r"console|services|[\w.-]+#\d+", re.IGNORECASE)


def row(line: str, session_at: int, id_at: int) -> tuple[str | None, int] | None:
    """One session of `query user`'s table: its session name (None for a disconnected one, which has none)
    and its idle minutes; None if the line isn't a whole row. The columns: user, session, id, state, idle
    time, logon time. From the right each is one word, the logon time two or three (AM or PM). Left of the
    id, a user name may hold spaces (PR #52 fix check 1 F4) and a session name has none:
    - a row whose id sits in the header's id column (`id_at`) is read by the header's columns: the session
      starts at `session_at`, and a user name reaching into that column isn't a row;
    - in any other row the word before the id is the session only if it has a session name's form, so
      that a name read wrongly can only make a console session, never hide one."""
    words = [(m.start(), m.group()) for m in re.finditer(r"\S+", line)]
    logon = 3 if words and words[-1][1].upper() in ("AM", "PM") else 2
    if len(words) < logon + 4:
        return None
    tail = words[-logon - 3:]
    (id_from, sid), (_, state), (_, idle) = tail[:3]
    if not sid.isdigit() or not state.isalpha() or not LOGON.fullmatch(" ".join(w for _, w in tail[3:])):
        return None
    left = words[:-logon - 3]
    if abs(id_from - id_at) <= 2:
        session = left[-1][1] if left and left[-1][0] >= session_at else None
        user = left[:-1] if session else left
        if any(at >= session_at for at, _ in user):
            return None
    else:
        session = left[-1][1] if len(left) >= 2 and SESSION.fullmatch(left[-1][1]) else None
        user = left[:-1] if session else left
    if not user:
        return None
    minutes = idle_minutes(idle)
    return None if minutes is None else (session, minutes)


def console_idle(output: str) -> int | None:
    """The console session's idle minutes from `query user`'s table, UNREADABLE if the output isn't a
    whole table (its header, then one or more whole rows), or None if no one is at the console (""
    : nobody logged on, or a whole table without a console row). PR #52 review F1: a header alone, a
    row cut short or a row that isn't one says nothing about the console."""
    lines = [l for l in output.splitlines() if l.strip()]
    if not lines:
        return None
    header = [m.start() for m in re.finditer(r"\S+", lines[0])]
    if not lines[0].strip().lower().startswith(HEADERS) or len(header) < 3 or len(lines) < 2:
        return UNREADABLE
    sessions = [row(line, header[1], header[2]) for line in lines[1:]]
    if None in sessions:
        return UNREADABLE
    return next((idle for session, idle in sessions if (session or "").lower() == "console"), None)


def query_user() -> str | None:
    """`query user`'s table ("" when nobody is logged on), or None when it couldn't say (no answer
    within its deadline, another failure): None counts as present (round 14 check-b)."""
    try:
        done = subprocess.run(["query", "user"], capture_output=True, text=True, encoding="oem", errors="replace",
                              creationflags=HIDDEN, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return None
    # Success with nothing on stdout is a missing reading, not "nobody logged on" (PR #52 review F1).
    if done.returncode == 0:
        return done.stdout if done.stdout.strip() else None
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


def unknown(output: str | None) -> str | None:
    """Why `query user` couldn't say who is at the console (no answer, or not a whole table), or None if it
    could."""
    if output is None:
        return "query user gave no answer"
    if console_idle(output) == UNREADABLE:
        return "query user's answer isn't a whole table"
    return None


def present(output: str | None, input_idle: float | None = None) -> bool:
    """Someone at the console (`query user`), and their last input less than IDLE_MINUTES ago
    (`input_idle`, minutes; else `query user`'s idle column). Unknown is present."""
    # Unknown before the last input's word (Sol r14 C1: it may speak of another session).
    if unknown(output):
        return True
    idle = console_idle(output)
    if idle is None:
        return False
    return (input_idle if input_idle is not None else idle) < IDLE_MINUTES


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

    query = query_user()
    now = int(time.time())
    why = unknown(query)
    # The owner's release lifts a known reading only: an unknown one keeps RETREAT (PR #52 review F2).
    freed = None if why else free_until(settings)
    if freed:
        if retreat.exists():
            retreat.unlink()
            say(f"back: the owner freed the machine until {freed}")
        return 0
    if present(query, last_input_minutes()):
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
