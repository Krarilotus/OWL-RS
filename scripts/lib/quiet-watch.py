#!/usr/bin/env python3
"""Keeps a timing measurement's host quiet while it runs (scripts/quiet-slot.sh starts it;
release-gate.sh per block and the measurement queue for every timed job). It suspends only work
positively known to be the measurement owner's own, and says when anything else disturbed it.

Owned (and so suspended while busy, resumed at the end) is only:
- a process outside the measurement whose environment carries the slot's FLEET_JOB, or that
  runs in the fleet step's own cgroup slice (FLEET_SLICE);
- a Docker container labelled with the slot's job namespace (nrese.ns, suitekit/ns.py) and not
  as the measurement's own (nrese.quiet, the slot's token).
Nothing is taken as owned by its user id (20 Oct 2026, COT-13: every busy process of the
user and every running container were stopped, other agents' and unlabelled ones included). A
process whose environment can't be read, an unlabelled container, another job's work, another
user's: never touched. What they use counts as unowned load: when it is above a core in more
than DISTURBED_SHARE of the samples, the measurement was disturbed, and says so.

The journal comes first (COT-14): each suspension is recorded, flushed and synced in
STATE_DIR/frozen before the signal or the pause, a process with its start time and the boot it
started in, so a later resume never continues a pid that has been reused. A resume that can't
settle a line (a container that won't unpause) keeps it and fails: the next slot holder doesn't
overwrite it.

    quiet-watch.py STATE_DIR ROOT_PID TOKEN      until SIGTERM (or ROOT_PID ends), then everything resumes
    quiet-watch.py STATE_DIR --resume            resume what a dead watcher left; exit 1 if any remains

ROOT_PID is the measurement: it, all it runs and its ancestors are never stopped. STATE_DIR/
watch.log says what happened, its last line the verdict; STATE_DIR/verdict.json the same for
the gate's report. Linux only (/proc); on another system it watches nothing and says so.
"""
from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

PERIOD_S = 2.0
# A process using more than this many cores is busy (a build, an unpack, a load).
HEAVY_CORES = 0.2
# Samples in which unowned work used more than a core: beyond this share, the measurement was
# disturbed.
DISTURBED_SHARE = 0.10
TICKS = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100
# Never stopped, even when owned: what keeps the host and the queue running, and a desktop
# session's compositor (phuoc-yu's console: gnome-shell stopped would freeze its display).
SPARED = ("guard.sh", "sshd", "systemd", "queue.py", "quiet-watch.py", "dbus-daemon",
          "gnome-shell", "Xwayland", "gnome-session", "gdm")
ROOT = Path(__file__).resolve().parents[2]


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


def started(pid: int) -> str | None:
    """The process's start time (clock ticks since boot, /proc/PID/stat field 22); None when it is
    gone. Any other failure to read it (permission, an I/O error) raises: that is not gone."""
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
    except (FileNotFoundError, ProcessLookupError):
        return None
    return stat[stat.rindex(")") + 2:].split()[19]


def boot_id() -> str:
    try:
        return Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    except OSError:
        return "?"


def environ_of(pid: int) -> dict[str, str]:
    """The process's environment, or nothing when it can't be read (another user's)."""
    try:
        raw = Path(f"/proc/{pid}/environ").read_bytes()
    except OSError:
        return {}
    return dict(item.split("=", 1) for item in raw.decode("utf-8", "replace").split("\0") if "=" in item)


def cgroup_of(pid: int) -> str:
    try:
        return Path(f"/proc/{pid}/cgroup").read_text()
    except OSError:
        return ""


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


def job_namespace(environ=os.environ) -> str | None:
    """The job namespace (suitekit/ns.py) of this checkout and job, or None without the kit."""
    try:
        sys.path.insert(0, str(ROOT / "benches" / "suite"))
        from suitekit import ns
        return ns.namespace(ROOT, environ)
    except Exception:  # noqa: BLE001 (no kit, no namespace: no container is owned)
        return None


class Owner:
    """Whose work the slot may suspend: its fleet job (FLEET_JOB), the step's slice (FLEET_SLICE)
    and the job namespace its containers carry (nrese.ns). A slot with neither a job nor a slice
    (a manual run) owns nothing, containers included: its namespace would be the checkout's,
    which every manual run from it shares (Sol, 20 Oct 2026)."""

    def __init__(self, environ=os.environ, namespace: str | None = None):
        self.job = environ.get("FLEET_JOB", "")
        self.slice = environ.get("FLEET_SLICE", "")
        if not (self.job or self.slice):
            self.namespace = None
        else:
            self.namespace = namespace if namespace is not None else job_namespace(environ)

    def owns(self, pid: int, environ_of=environ_of, cgroup_of=cgroup_of) -> bool:
        if self.job and environ_of(pid).get("FLEET_JOB") == self.job:
            return True
        if self.slice and any(self.slice in line.split(":", 2)[-1].strip().split("/")
                              for line in cgroup_of(pid).splitlines()):
            return True
        return False

    def owns_container(self, ns_label: str) -> bool:
        return bool(self.namespace) and ns_label == self.namespace


def busy(before: dict, now: dict, interval_s: float, keep: set[int], owner: Owner,
         environ_of=environ_of, cgroup_of=cgroup_of) -> tuple[list[int], float, list[int]]:
    """Busy processes outside `keep`: the owned ones (to suspend), the cores the unowned used,
    and the busy unowned ones (to name)."""
    owned, unowned_cores, unowned = [], 0.0, []
    for pid, (_ppid, _uid, ticks, _name) in now.items():
        if pid not in before or pid in keep:
            continue
        cores = max((ticks - before[pid][2]) / TICKS / interval_s, 0.0)
        if cores > HEAVY_CORES and owner.owns(pid, environ_of, cgroup_of):
            owned.append(pid)
            continue
        unowned_cores += cores
        if cores > HEAVY_CORES:
            unowned.append(pid)
    return owned, unowned_cores, unowned


def containers(token: str) -> list[tuple[str, str]]:
    """Running Docker containers other than the measurement's: (id, their nrese.ns label)."""
    try:
        done = subprocess.run(["docker", "ps", "--filter", "status=running", "--format",
                               '{{.ID}}\t{{.Label "nrese.quiet"}}\t{{.Label "nrese.ns"}}'],
                              capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.TimeoutExpired):
        return []
    found = []
    for line in done.stdout.splitlines():
        cid, quiet, ns_label = (line.split("\t") + ["", "", ""])[:3]
        if cid and quiet.strip() != token:
            found.append((cid, ns_label.strip()))
    return found


def fsync_dir(directory: Path):
    """Makes a directory's entries durable (a file created, renamed or removed in it)."""
    try:
        fd = os.open(directory, os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
    except OSError:
        return
    try:
        os.fsync(fd)
    except OSError:
        pass
    finally:
        os.close(fd)


def journal(frozen: Path, line: str):
    """Records a suspension before it is made: appended, flushed, synced (and the directory too
    when the journal is new, so its entry survives a power loss)."""
    created = not frozen.exists()
    with open(frozen, "a", encoding="utf-8") as f:
        f.write(line + "\n")
        f.flush()
        os.fsync(f.fileno())
    if created:
        fsync_dir(frozen.parent)


def rewrite(frozen: Path, lines: list[str]):
    """Replaces the journal with `lines` whole or not at all: a synced temporary file renamed over
    it, then the directory synced. Truncating it in place lost the records of a crash midway
    (Sol, 20 Oct 2026)."""
    temporary = frozen.with_name(f"{frozen.name}.{os.getpid()}.new")
    try:
        with open(temporary, "w", encoding="utf-8") as f:
            f.write("".join(f"{line}\n" for line in lines))
            f.flush()
            os.fsync(f.fileno())
        os.replace(temporary, frozen)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise
    fsync_dir(frozen.parent)


def alive(fd: int) -> bool:
    """Whether a pidfd's process still exists (signal 0 through the handle)."""
    try:
        signal.pidfd_send_signal(fd, 0)
    except ProcessLookupError:
        return False
    return True


def suspend(pid: int, frozen: Path, boot: str, recorded: set[int], owner: Owner,
            environ_of=environ_of, cgroup_of=cgroup_of) -> bool:
    """Stops an owned process through a handle on it (a pidfd): its identity (start time) and
    ownership are read while the handle holds it, the handle's process is then proven alive (so
    those reads were of it: a live process keeps its pid), the journal line (pid, start time,
    boot) is written once, and the signal goes through the handle, never to a pid that may since
    name another process (Sol, 20 Oct 2026). No pidfd (an older kernel or Python): not stopped."""
    try:
        fd = os.pidfd_open(pid)
    except (AttributeError, OSError):
        return False
    try:
        start = started(pid)
        if start is None or not owner.owns(pid, environ_of, cgroup_of) or not alive(fd):
            return False
        if pid not in recorded:
            journal(frozen, f"pid {pid} {start} {boot}")
            recorded.add(pid)
        signal.pidfd_send_signal(fd, signal.SIGSTOP)
        return True
    except (AttributeError, OSError):
        return False
    finally:
        os.close(fd)


def resume(state: Path) -> list[str]:
    """Continues and unpauses what STATE/frozen records; a process only if it is still the one
    recorded (same start time, same boot). A line is settled only when its process is continued,
    or proven gone (no such pid, another start time, another boot), or its container unpaused or
    gone; one that couldn't be inspected or signalled is not (Sol, 20 Oct 2026: an EPERM on
    SIGCONT dropped the record of a process left stopped). The unsettled lines stay in the file
    (it goes when none remain); they are returned."""
    path = state / "frozen"
    if not path.exists():
        return []
    boot, left = boot_id(), []
    for line in path.read_text(encoding="utf-8").splitlines():
        kind, _, what = line.partition(" ")
        if kind == "pid":
            pid, _, rest = what.partition(" ")
            start, _, booted = rest.partition(" ")
            if not pid.isdigit() or not start:
                left.append(line)  # no start time (an older watcher's line): which process is unknown
                continue
            if booted != boot:
                continue  # recorded in another boot: that process is gone
            # Through a handle on the process, as suspend(): identity read while it holds it,
            # proven alive after, the signal through it. No handle: not continued blindly.
            try:
                fd = os.pidfd_open(int(pid))
            except ProcessLookupError:
                continue  # gone
            except (AttributeError, OSError):
                left.append(line)
                continue
            try:
                now = started(int(pid))
                if not alive(fd) or now != start:
                    continue  # gone, or another process under a reused pid: nothing of ours to continue
                signal.pidfd_send_signal(fd, signal.SIGCONT)
            except ProcessLookupError:
                pass  # gone meanwhile
            except (AttributeError, OSError):
                left.append(line)  # can't be inspected or signalled: still stopped, still ours, kept
            finally:
                os.close(fd)
        elif kind == "container":
            try:
                done = subprocess.run(["docker", "unpause", what], capture_output=True, text=True)
            except OSError:
                left.append(line)
                continue
            gone_or_running = done.returncode == 0 or "No such container" in done.stderr or "not paused" in done.stderr
            if not gone_or_running:
                left.append(line)
        elif line.strip():
            left.append(line)
    if left:
        rewrite(path, left)
    else:
        path.unlink()
        fsync_dir(path.parent)
    return left


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
    me, boot = os.getpid(), boot_id()
    owner = Owner()
    frozen = state / "frozen"
    stopped: set[int] = set()
    paused: set[str] = set()
    named: set[int] = set()
    unowned_containers: set[str] = set()
    samples = disturbed = 0
    before, last = processes(), time.monotonic()
    say(f"owner: job {owner.job or '-'}, slice {owner.slice or '-'}, namespace {owner.namespace or '-'}")
    try:
        while not stop["now"]:
            time.sleep(PERIOD_S)
            now, at = processes(), time.monotonic()
            if root not in now:
                break
            mine, ancestors = family(now, root)
            # A stopped process uses no CPU; one another watcher resumed is stopped again.
            owned, unowned_cores, unowned = busy(before, now, at - last, mine | ancestors | {me} | stopped, owner)
            for pid in owned:
                if not spared(pid) and suspend(pid, frozen, boot, stopped, owner):
                    say(f"stopped {pid} {now[pid][3]} (ours)")
            for pid in unowned:
                if pid not in named:
                    named.add(pid)
                    say(f"busy, not ours (not suspended): {pid} {now[pid][3]}")
            for cid, ns_label in containers(token):
                if not owner.owns_container(ns_label):
                    if cid not in unowned_containers:
                        unowned_containers.add(cid)
                        say(f"a running container that isn't ours (not paused): {cid} {ns_label or 'unlabelled'}")
                    continue
                if cid not in paused:
                    journal(frozen, f"container {cid}")
                    paused.add(cid)
                    if subprocess.run(["docker", "pause", cid], capture_output=True).returncode == 0:
                        say(f"paused container {cid} (ours)")
            samples += 1
            disturbed += unowned_cores > 1.0
            before, last = now, at
    finally:
        left = resume(state)
        share = disturbed / samples if samples else 0.0
        verdict = "disturbed" if share > DISTURBED_SHARE else "quiet"
        (state / "verdict.json").write_text(json.dumps({
            "verdict": verdict, "samples": samples, "unowned_share": round(share, 3),
            "stopped": len(stopped), "paused": len(paused), "unowned_busy": len(named),
            "unowned_containers": sorted(unowned_containers), "not_resumed": left}), encoding="utf-8")
        say(f"{verdict}: {len(stopped)} of our processes stopped and {len(paused)} of our containers paused; "
            f"unowned work above a core in {share:.0%} of {samples} samples ({len(named)} busy processes and "
            f"{len(unowned_containers)} containers not ours, left running)"
            + (f"; NOT resumed: {left}" if left else ""))
    # Anything left suspended fails the watch, and its callers with it (the record stays).
    return 1 if left else 0


def main() -> int:
    state = Path(sys.argv[1])
    state.mkdir(parents=True, exist_ok=True)
    if sys.argv[2:] == ["--resume"]:
        left = resume(state)
        if left:
            print(f"quiet-watch: {len(left)} suspensions in {state / 'frozen'} could not be resumed: {left}",
                  file=sys.stderr)
        return 1 if left else 0
    return watch(state, int(sys.argv[2]), sys.argv[3])


if __name__ == "__main__":
    sys.exit(main())
