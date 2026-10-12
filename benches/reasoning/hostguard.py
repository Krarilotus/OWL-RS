"""Before a kit starts a step on a shared host: wait while the host's owners need it, until
the memory the step may take is free, and for the machine's quiet slot.

Office-PC's guard (~/owlrs/guard.sh) writes ~/owlrs/RETREAT while a colleague is present and
stops benchmark containers then; a benchmark starts no new case until the file is gone. The
memory check keeps a kit from starting a step whose cap the host can't give: on a shared
machine, two large steps at once push others out. The quiet slot (scripts/quiet-slot.sh's
lock, ~/.nrese-quiet-slot) is held per step, never across a whole run, so other threads'
timed measurements and builds take turns with it: a heavy step never runs beside another
thread's measurement, and a timed step also waits until no compiler runs.
"""
from __future__ import annotations

import contextlib
import os
import subprocess
import time
from pathlib import Path

RETREAT = Path.home() / "owlrs" / "RETREAT"
SLOT = Path(os.environ.get("NRESE_QUIET_DIR", str(Path.home() / ".nrese-quiet-slot")))


def alive(pid: str) -> bool:
    try:
        os.kill(int(pid), 0)
        return True
    except (ValueError, OSError):
        return False


def compilers_running() -> bool:
    return subprocess.run(["pgrep", "-x", "rustc|cargo"], capture_output=True).returncode == 0


@contextlib.contextmanager
def slot(what: str, timed: bool, say=print, poll_s: float = 15.0, drain_s: float = 600.0):
    """Holds the quiet slot for one step (the lock protocol of scripts/quiet-slot.sh): waits
    for it, takes it over from a dead owner, and for a timed step waits until no rustc or
    cargo runs (at most `drain_s`, then it releases the slot and raises TimeoutError)."""
    told = False
    while True:
        try:
            SLOT.mkdir()
            break
        except FileExistsError:
            owner = (SLOT / "pid").read_text().strip() if (SLOT / "pid").exists() else ""
            if owner and not alive(owner):
                for f in SLOT.iterdir():
                    f.unlink()
                SLOT.rmdir()
                continue
            if not told:
                who = (SLOT / "who").read_text().strip() if (SLOT / "who").exists() else "?"
                say(f"quiet slot held by {who}: waiting")
                told = True
            time.sleep(poll_s)
    try:
        (SLOT / "pid").write_text(f"{os.getpid()}\n")
        (SLOT / "who").write_text(time.strftime("%H:%M:%S ") + what + "\n")
        if timed:
            deadline = time.monotonic() + drain_s
            while compilers_running():
                if time.monotonic() > deadline:
                    raise TimeoutError("compilers still running; the timed step was not started")
                time.sleep(5)
        yield
    finally:
        for f in SLOT.iterdir():
            f.unlink()
        SLOT.rmdir()


def available_gib() -> float | None:
    """MemAvailable in GiB (Linux); None elsewhere."""
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) / 1024 ** 2
    except OSError:
        return None
    return None


def wait(need_gib: float = 0.0, say=print, poll_s: float = 60.0):
    """Returns when no retreat is in force and `need_gib` of memory is available."""
    told = set()
    while True:
        if RETREAT.exists():
            if "retreat" not in told:
                say(f"the host's owners use it ({RETREAT}): waiting")
                told.add("retreat")
        else:
            free = available_gib()
            if free is None or free >= need_gib:
                return
            if "memory" not in told:
                say(f"{free:.1f} GiB available, the step may take {need_gib:.0f}: waiting")
                told.add("memory")
        time.sleep(poll_s)


def gib(size: str) -> float:
    """A Docker memory size (`28g`, `512m`) in GiB; 0 for none."""
    size = size.strip().lower()
    if not size or size == "0":
        return 0.0
    factor = {"k": 1 / 1024 ** 2, "m": 1 / 1024, "g": 1.0}.get(size[-1])
    return float(size[:-1]) * factor if factor else float(size) / 1024 ** 3
