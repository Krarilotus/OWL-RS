#!/usr/bin/env python3
"""Keeps a timing measurement's host quiet while it runs (scripts/quiet-slot.sh starts it;
the measurement queue too, for every timed job). Every few seconds:

- a process of this user that isn't the measurement (nor one of its ancestors) and used more
  than a fifth of a core since the last look is stopped (SIGSTOP): a build, an unpack, a
  load, another run that didn't ask for the slot; it continues (SIGCONT) when the
  measurement ends;
- a Docker container that was running when the measurement began and isn't the
  measurement's (its label nrese.quiet isn't the slot's token) is paused, and unpaused at
  the end; one started meanwhile without the token is recorded (a kit that starts its own
  containers labels them with NRESE_QUIET_TOKEN, or they count as a disturbance);
- the host's guard, sshd, systemd, the measurement queue and this watcher are never stopped;
- processes of other users (the owners', whom the host's guard looks after) and of root are
  never touched: their CPU is recorded, and a measurement they disturbed says so.

    quiet-watch.py STATE_DIR ROOT_PID TOKEN      until SIGTERM (or ROOT_PID ends), then everything resumes

ROOT_PID is the measurement: the process that asked for the slot; it and all it runs are
never stopped, nor are its ancestors.
    quiet-watch.py STATE_DIR --resume            resume what a dead watcher left stopped

STATE_DIR/frozen lists what is stopped or paused (so that a dead watcher's are resumed by the
next slot holder); STATE_DIR/watch.log what happened, and its last line the verdict: quiet,
or disturbed with the share of samples in which other users' processes used more than a core.
Linux only (/proc); on another system it watches nothing and says so.
"""
from __future__ import annotations

import os
import signal
import subprocess
import sys
import time
from pathlib import Path

PERIOD_S = 2.0
# A process using more than this many cores is heavy (a busy build, an unpack, a load).
HEAVY_CORES = 0.2
# Samples in which other users' processes used more than a core: beyond this share, the
# measurement was disturbed.
DISTURBED_SHARE = 0.10
TICKS = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100
# Never stopped, whatever they use: what keeps the host and the queue running, and a desktop
# session's compositor (phuoc-yu's console: gnome-shell stopped would freeze its display).
SPARED = ("guard.sh", "sshd", "systemd", "queue.py", "quiet-watch.py", "dbus-daemon",
          "gnome-shell", "Xwayland", "gnome-session", "gdm")


def spared(pid: int) -> bool:
    try:
        command = Path(f"/proc/{pid}/cmdline").read_bytes().replace(bytes(1), b" ").decode("utf-8", "replace")
    except OSError:
        return True
    return any(name in command for name in SPARED)


def processes() -> dict[int, tuple[int, int, int, str]]:
    """pid -> (ppid, uid, cpu ticks, name), from /proc."""
    out = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            stat = Path(f"/proc/{entry}/stat").read_text()
            uid = os.stat(f"/proc/{entry}").st_uid
        except OSError:
            continue
        name = stat[stat.index("(") + 1:stat.rindex(")")]
        fields = stat[stat.rindex(")") + 2:].split()
        out[int(entry)] = (int(fields[1]), uid, int(fields[11]) + int(fields[12]), name)
    return out


def family(table: dict, root: int) -> tuple[set[int], set[int]]:
    """The measurement: `root` and its descendants; and root's ancestors (who started it)."""
    children: dict[int, list[int]] = {}
    for pid, (ppid, *_rest) in table.items():
        children.setdefault(ppid, []).append(pid)
    mine, todo = set(), [root]
    while todo:
        pid = todo.pop()
        if pid not in mine:
            mine.add(pid)
            todo += children.get(pid, [])
    ancestors, pid = set(), table.get(root, (0,))[0]
    while pid and pid in table and pid not in ancestors:
        ancestors.add(pid)
        pid = table[pid][0]
    return mine, ancestors


def heavy(before: dict, now: dict, interval_s: float, uid: int, keep: set[int]) -> tuple[list[int], float]:
    """This user's heavy processes outside `keep`, and the cores other users used."""
    ours, others = [], 0.0
    for pid, (_, owner, ticks, _name) in now.items():
        if pid not in before:
            continue
        cores = (ticks - before[pid][2]) / TICKS / interval_s
        if owner == uid and pid not in keep and cores > HEAVY_CORES:
            ours.append(pid)
        elif owner not in (uid, 0):
            others += max(cores, 0.0)
    return ours, others


def containers(token: str) -> list[str]:
    """Running Docker containers that aren't labelled as the measurement's."""
    try:
        done = subprocess.run(["docker", "ps", "--filter", "status=running", "--format",
                               '{{.ID}} {{.Label "nrese.quiet"}}'], capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return []
    found = []
    for line in done.stdout.splitlines():
        cid, _, label = line.partition(" ")
        if cid and label.strip() != token:
            found.append(cid)
    return found


def resume(state: Path):
    path = state / "frozen"
    if not path.exists():
        return
    for line in path.read_text().splitlines():
        kind, _, what = line.partition(" ")
        if kind == "pid":
            try:
                os.kill(int(what), signal.SIGCONT)
            except (OSError, ValueError):
                pass
        elif kind == "container":
            subprocess.run(["docker", "unpause", what], capture_output=True)
    path.unlink()


def watch(state: Path, root: int, token: str) -> int:
    log = open(state / "watch.log", "a", encoding="utf-8")

    def say(text: str):
        log.write(f"{time.strftime('%H:%M:%S')} {text}\n")
        log.flush()
    if not Path("/proc").is_dir():
        say("not Linux: nothing watched")
        return 0
    stop = {"now": False}
    signal.signal(signal.SIGTERM, lambda *_: stop.update(now=True))
    uid, me = os.getuid(), os.getpid()
    frozen = state / "frozen"
    stopped: set[int] = set()
    paused: set[str] = set()
    samples = disturbed = 0
    before, last = processes(), time.monotonic()
    # Containers running when the measurement began are someone else's work: paused. Those
    # started meanwhile without the token may be the measurement's own: recorded.
    at_start, started_meanwhile = set(containers(token)), set()
    while not stop["now"]:
        time.sleep(PERIOD_S)
        now, at = processes(), time.monotonic()
        mine, ancestors = family(now, root)
        if root not in now:
            break
        # A stopped process uses no CPU; one another watcher resumed is stopped again.
        pids, others = heavy(before, now, at - last, uid, mine | ancestors | {me})
        for pid in pids:
            if spared(pid):
                continue
            try:
                os.kill(pid, signal.SIGSTOP)
            except OSError:
                continue
            if pid not in stopped:
                stopped.add(pid)
                with open(frozen, "a") as f:
                    f.write(f"pid {pid}\n")
                say(f"stopped {pid} {now[pid][3]}")
        for cid in containers(token):
            if cid not in at_start:
                if cid not in started_meanwhile:
                    started_meanwhile.add(cid)
                    say(f"a container without the slot's token started meanwhile: {cid}")
                continue
            if cid not in paused and subprocess.run(["docker", "pause", cid], capture_output=True).returncode == 0:
                paused.add(cid)
                with open(frozen, "a") as f:
                    f.write(f"container {cid}\n")
                say(f"paused container {cid}")
        samples += 1
        disturbed += others > 1.0
        before, last = now, at
    resume(state)
    share = disturbed / samples if samples else 0.0
    verdict = "disturbed" if share > DISTURBED_SHARE else "quiet"
    say(f"{verdict}: {len(stopped)} processes stopped and {len(paused)} containers paused while it ran; "
        f"other users above a core in {share:.0%} of {samples} samples; "
        f"{len(started_meanwhile)} containers without the token started meanwhile")
    return 0


def main() -> int:
    state = Path(sys.argv[1])
    state.mkdir(parents=True, exist_ok=True)
    if sys.argv[2:] == ["--resume"]:
        resume(state)
        return 0
    return watch(state, int(sys.argv[2]), sys.argv[3])


if __name__ == "__main__":
    sys.exit(main())
