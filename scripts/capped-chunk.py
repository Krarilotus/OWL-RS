#!/usr/bin/env python3
"""Runs one queue chunk's command under a memory cap that holds and a time budget: on Windows a
Job Object over the whole process tree (the job's memory limit; every process of it killed
when it closes), elsewhere a systemd user scope (MemoryMax, no swap, in the fleet step's
slice) where there is one. A declaration caps nothing (13 Oct 2026: a Konclude declared at
22 GB took 30 on Office); a host on hold (hosts.toml queue.hold_mem_gb) needs its jobs to
stay under it.

    python scripts/capped-chunk.py --memory-mib 4000 --budget-min 28 [--budget-ok] -- COMMAND...

Exit status: the command's when it ends by itself; 86 (CAP_REACHED) when it failed at the
memory cap (the kernel's OOM kill in its scope, or the job's peak at the limit on Windows:
the work doesn't fit the cap); 75 (BUDGET_ENDED) when the budget ended it: the command is
stopped (its process tree and scope killed), and the work isn't done. `--budget-ok` makes
that 0, for a resumable command whose next chunk continues where this one stopped. Ending
the scope is verified; if it can't be, the exit status is 1 (or the command's failure).
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "benches" / "suite"))
from suitekit.runtime import end_scope, run_quiet, scope_argv  # noqa: E402

CAP_REACHED = 86
BUDGET_ENDED = 75


def windows_job(memory_mib: int):
    """A Job Object with a memory limit for all its processes, killed on close."""
    import ctypes
    from ctypes import wintypes
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)

    class IO(ctypes.Structure):
        _fields_ = [(n, ctypes.c_ulonglong) for n in ("a", "b", "c", "d", "e", "f")]

    class BASIC(ctypes.Structure):
        _fields_ = [("PerProcessUserTimeLimit", ctypes.c_int64), ("PerJobUserTimeLimit", ctypes.c_int64),
                    ("LimitFlags", wintypes.DWORD), ("MinimumWorkingSetSize", ctypes.c_size_t),
                    ("MaximumWorkingSetSize", ctypes.c_size_t), ("ActiveProcessLimit", wintypes.DWORD),
                    ("Affinity", ctypes.c_size_t), ("PriorityClass", wintypes.DWORD),
                    ("SchedulingClass", wintypes.DWORD)]

    class EXTENDED(ctypes.Structure):
        _fields_ = [("BasicLimitInformation", BASIC), ("IoInfo", IO), ("ProcessMemoryLimit", ctypes.c_size_t),
                    ("JobMemoryLimit", ctypes.c_size_t), ("PeakProcessMemoryUsed", ctypes.c_size_t),
                    ("PeakJobMemoryUsed", ctypes.c_size_t)]

    kernel32.CreateJobObjectW.restype = wintypes.HANDLE
    job = kernel32.CreateJobObjectW(None, None)
    info = EXTENDED()
    info.BasicLimitInformation.LimitFlags = 0x00000200 | 0x00002000  # job memory, kill on close
    info.JobMemoryLimit = memory_mib * 2**20
    if not kernel32.SetInformationJobObject(job, 9, ctypes.byref(info), ctypes.sizeof(info)):
        raise OSError(ctypes.get_last_error(), "SetInformationJobObject")

    def assign(process: subprocess.Popen):
        kernel32.OpenProcess.restype = wintypes.HANDLE
        handle = kernel32.OpenProcess(0x1F0FFF, False, process.pid)
        if not kernel32.AssignProcessToJobObject(job, handle):
            raise OSError(ctypes.get_last_error(), "AssignProcessToJobObject")
        kernel32.CloseHandle(handle)

    def peak_mib() -> float:
        query = EXTENDED()
        kernel32.QueryInformationJobObject(job, 9, ctypes.byref(query), ctypes.sizeof(query), None)
        return query.PeakJobMemoryUsed / 2**20

    def kill():
        kernel32.TerminateJobObject(job, 1)

    return assign, peak_mib, kill


# Inside the scope, after the command: its cgroup's peak and OOM kills, also when the kernel
# killed the command at the cap (OOMPolicy=continue keeps the wrapper).
WRAPPER = ('g=/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup); "$@"; rc=$?; '
           '{ cat "$g/memory.peak" 2>/dev/null || echo -; '
           'sed -n "s/^oom_kill //p" "$g/memory.events" 2>/dev/null || echo -; } > "$CAPPED_CHUNK_REPORT"; exit $rc')


class Scope:
    """The command's systemd user scope: its cgroup's memory.events read while it runs (the
    wrapper's report may be lost with it), the wrapper's report after."""

    def __init__(self, memory_mib: int):
        self.unit = f"capped-chunk-{os.getpid()}-{int(time.monotonic() * 1000)}"
        self.report = Path(tempfile.mkdtemp(prefix="capped-chunk-")) / "report"
        self.events: Path | None = None
        self.oom_kills: int | None = None
        self.peak_bytes: int | None = None
        self.prefix = [*scope_argv(self.unit, f"{memory_mib}m"), "sh", "-c", WRAPPER, "sh"]

    def poll(self):
        if self.events is None:
            shown = run_quiet(["systemctl", "--user", "show", "-p", "ControlGroup", "--value",
                               f"{self.unit}.scope"], 10)
            group = shown.stdout.strip() if shown.returncode == 0 else ""
            if group.startswith("/"):
                self.events = Path("/sys/fs/cgroup" + group)
        if self.events is not None:
            try:
                for line in (self.events / "memory.events").read_text().splitlines():
                    if line.startswith("oom_kill "):
                        self.oom_kills = max(self.oom_kills or 0, int(line.split()[1]))
                self.peak_bytes = max(self.peak_bytes or 0, int((self.events / "memory.peak").read_text()))
            except (OSError, ValueError):
                pass

    def finish(self):
        try:
            peak, kills = (self.report.read_text().split() + ["-", "-"])[:2]
            if peak.isdigit():
                self.peak_bytes = max(self.peak_bytes or 0, int(peak))
            if kills.isdigit():
                self.oom_kills = max(self.oom_kills or 0, int(kills))
        except OSError:
            pass
        shutil.rmtree(self.report.parent, ignore_errors=True)

    @property
    def peak_mib(self) -> float | None:
        return None if self.peak_bytes is None else self.peak_bytes / 2**20


def outcome(code: int, *, oom_kills: int | None, peak_mib: float | None, cap_mib: int) -> int:
    """The chunk's exit status for a command that ended by itself: CAP_REACHED when it failed
    at the cap (an OOM kill in its scope; on Windows, where only the job's peak is known, a
    peak at the limit), else its own status."""
    if code == 0:
        return 0
    if oom_kills is not None:
        return CAP_REACHED if oom_kills > 0 else code
    return CAP_REACHED if peak_mib is not None and peak_mib >= cap_mib * 0.98 else code


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--memory-mib", type=int, required=True)
    p.add_argument("--budget-min", type=float, required=True)
    p.add_argument("--budget-ok", action="store_true",
                   help="exit 0 when the budget ends a resumable command (its next chunk continues)")
    p.add_argument("command", nargs=argparse.REMAINDER)
    args = p.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    deadline = time.monotonic() + args.budget_min * 60
    scope = None
    if os.name == "nt":
        assign, peak_mib, kill = windows_job(args.memory_mib)
        # Started suspended would be tighter; the child is assigned at once, before it can
        # start much (its own children inherit the job).
        process = subprocess.Popen(command, creationflags=0x00000200)  # a new process group
        assign(process)
    else:
        env = None
        if shutil.which("systemd-run"):
            scope = Scope(args.memory_mib)
            command = [*scope.prefix, *command]
            env = {**os.environ, "CAPPED_CHUNK_REPORT": str(scope.report)}
        process = subprocess.Popen(command, start_new_session=True, env=env)

        def peak_mib():
            return scope.peak_mib if scope else None

        def kill():
            import signal
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    budget_ended = False
    while True:
        try:
            code = process.wait(timeout=1 if scope else 5)
            break
        except subprocess.TimeoutExpired:
            if scope:
                scope.poll()
        if time.monotonic() > deadline:
            kill()
            code = process.wait()
            budget_ended = True
            break
    if scope:
        scope.poll()
        scope.finish()
        try:
            # Nothing outlives the chunk: what its scope still holds (a child that left the
            # process group) goes too, and that it went is checked.
            end_scope(scope.unit)
        except RuntimeError as error:
            print(f"capped-chunk: {error}", flush=True)
            return code if code and not budget_ended else 1
    peak = peak_mib()
    shown = "unknown" if peak is None else f"{peak:.0f} MiB"
    if budget_ended:
        ending = 0 if args.budget_ok else BUDGET_ENDED
        print(f"capped-chunk: the budget of {args.budget_min:g} min ended this chunk (peak {shown}); "
              f"the work isn't done, the next chunk resumes (exit {ending})", flush=True)
        return ending
    status = outcome(code, oom_kills=scope.oom_kills if scope else None, peak_mib=peak, cap_mib=args.memory_mib)
    if status == CAP_REACHED:
        print(f"capped-chunk: the command reached the memory cap ({shown} of {args.memory_mib} MiB, "
              f"exit {code})", flush=True)
    else:
        print(f"capped-chunk: exit {code}, peak {shown}", flush=True)
    return status


if __name__ == "__main__":
    sys.exit(main())
