"""A kit's steps on a shared host, under the measurement queue's rules (it replaces
benches/reasoning/hostguard.py, whose own slot lock had no watcher and no fair share).

    require_slot(what)   a timed step runs inside the quiet slot its caller holds, else it refuses
    step(what, timed)    a kit's step: a timed one refused outside the slot
    wait_memory(gib)     waits while the host's owners need it and until the memory is free
    gib("28g")           a Docker memory size in GiB

A timed step needs scripts/quiet-slot.sh around the kit's command, as every timed queue job
has it (benches/suite/queue.py): the slot's watcher pauses other heavy work for the step and
judges it quiet or disturbed, and the queue's chunks share the slot fairly. The kit itself
takes no lock: holding the slot per step inside a command that already holds it would wait
forever, and holding it without the watcher measures beside whatever else runs. Untimed
steps (preparation, qualification) need no slot; they run at low priority beside others.
"""
from __future__ import annotations

import contextlib
import os
import time
from pathlib import Path

from . import hosts


def held() -> str | None:
    """The quiet slot's token when our caller holds it (scripts/quiet-slot.sh exports it)."""
    return os.environ.get("NRESE_QUIET_TOKEN") or None


def require_slot(what: str):
    """Refuses a timed step outside the quiet slot."""
    if not held():
        raise RuntimeError(f"{what}: a timed step runs inside the quiet slot: "
                           "bash scripts/quiet-slot.sh <the kit's command> (every timed queue job does)")


@contextlib.contextmanager
def step(what: str, timed: bool):
    """One step of a kit: a timed one only inside the quiet slot."""
    if timed:
        require_slot(what)
    yield


def retreat_file() -> Path | None:
    """This host's retreat file (hosts.toml queue.retreat): its owners use it while it exists."""
    found = hosts.current()
    path = (found[1].get("queue") or {}).get("retreat") if found else None
    return Path(os.path.expanduser(path)) if path else None


def available_gib() -> float | None:
    """MemAvailable in GiB (Linux); None elsewhere."""
    try:
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemAvailable:"):
                return int(line.split()[1]) / 1024 ** 2
    except OSError:
        return None
    return None


def wait_memory(need_gib: float = 0.0, say=print, poll_s: float = 60.0):
    """Returns when the host's owners don't need it (no retreat file; the queue also stops a
    running job then) and `need_gib` of memory is available: two large steps at once on a
    shared host push others out."""
    retreat, told = retreat_file(), set()
    while True:
        if retreat is not None and retreat.exists():
            if "retreat" not in told:
                say(f"the host's owners use it ({retreat}): waiting")
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
