#!/usr/bin/env python3
"""Khaleesi's guard (the owner's rule, 12 Oct 2026): while the owner uses the machine, no timed
job runs. Task Scheduler starts it every minute (pythonw, no window); it is a script, not an
agent, like the office PC's ~/owlrs/guard.sh.

The owner counts as present while the console session isn't idle: `query user` shows its
idle time, and under IDLE_MINUTES (or a session started less than that ago) is present.
Then:
- the queue's retreat file (benches/hosts.toml khaleesi queue.retreat) is written, with the
  time: the measurement queue starts no timed job while it exists;
- every running timed job is stopped, its whole process tree (taskkill /T /F), by the pid
  file the runner keeps (queue/running/<id>.pid); the runner sees it stopped with the owner
  present, keeps its partial results and queues it again.
The retreat file goes once nobody has been present for IDLE_MINUTES. Nothing is restarted
here; the runner starts the job again in the next window.

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


def console_idle(output: str) -> int | None:
    """The console session's idle minutes from `query user`'s table, or None if no one is at
    the console. The columns: user, session, id, state, idle time, logon time."""
    for line in output.splitlines()[1:]:
        fields = line.lstrip(">").split()
        if len(fields) >= 5 and fields[1].lower() == "console":
            return idle_minutes(fields[4])
    return None


def present(output: str) -> bool:
    idle = console_idle(output)
    return idle is not None and idle < IDLE_MINUTES


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

    query = subprocess.run(["query", "user"], capture_output=True, text=True, encoding="oem", errors="replace",
                           creationflags=HIDDEN)
    now = int(time.time())
    if present(query.stdout):
        if not retreat.exists():
            say("retreat: the owner is at the console")
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
