#!/usr/bin/env python3
"""Runs one queue chunk's command under a memory cap that holds and a time budget: on Windows a
Job Object over the whole process tree (the job's memory limit; every process of it killed
when it closes), elsewhere a systemd user scope (MemoryMax) where there is one. A declaration
caps nothing (13 Oct 2026: a Konclude declared at 22 GB took 30 on Office); a host on hold
(hosts.toml queue.hold_mem_gb) needs its jobs to stay under it.

    python scripts/capped-chunk.py --memory-mib 4000 --budget-min 28 -- COMMAND...

The budget ends the chunk, not the work: the command is stopped (its process tree killed)
and the chunk exits 0, the next chunk resumes (the command must be resumable). Exit status:
the command's when it ends by itself; 0 when the budget ended it; 86 when it reached the
memory cap (said, with the peak: the work doesn't fit the cap).
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import time

CAP_REACHED = 86


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


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--memory-mib", type=int, required=True)
    p.add_argument("--budget-min", type=float, required=True)
    p.add_argument("command", nargs=argparse.REMAINDER)
    args = p.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    deadline = time.monotonic() + args.budget_min * 60
    if os.name == "nt":
        assign, peak_mib, kill = windows_job(args.memory_mib)
        # Started suspended would be tighter; the child is assigned at once, before it can
        # start much (its own children inherit the job).
        process = subprocess.Popen(command, creationflags=0x00000200)  # a new process group
        assign(process)
    else:
        unit = None
        if shutil.which("systemd-run"):
            # Under fleet in the step's slice (FLEET_SLICE), so the cap counts against the job.
            unit = f"capped-chunk-{os.getpid()}"
            placed = [f"--slice={os.environ['FLEET_SLICE']}"] if os.environ.get("FLEET_SLICE") else []
            command = ["systemd-run", "--user", "--scope", "-q", *placed, f"--unit={unit}",
                       "-p", f"MemoryMax={args.memory_mib}M", "-p", "MemorySwapMax=0", *command]
        process = subprocess.Popen(command, start_new_session=True)
        peak_mib = lambda: 0.0  # noqa: E731

        def kill():
            import signal
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            end_scope()

        def end_scope():
            # Nothing outlives the chunk: what its scope still holds (a child that left the
            # process group) goes too. A scope already gone is no error.
            if unit:
                subprocess.run(["systemctl", "--user", "kill", "--signal=KILL", f"{unit}.scope"], capture_output=True)
    while True:
        try:
            code = process.wait(timeout=5)
            break
        except subprocess.TimeoutExpired:
            pass
        if time.monotonic() > deadline:
            kill()
            process.wait()
            print(f"capped-chunk: the budget of {args.budget_min:g} min ended this chunk (peak {peak_mib():.0f} MiB); "
                  "the next chunk resumes", flush=True)
            return 0
    peak = peak_mib()
    if os.name != "nt":
        end_scope()
    if peak >= args.memory_mib * 0.98:
        print(f"capped-chunk: the command reached the memory cap ({peak:.0f} of {args.memory_mib} MiB)", flush=True)
        return CAP_REACHED
    print(f"capped-chunk: exit {code}, peak {peak:.0f} MiB", flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())
