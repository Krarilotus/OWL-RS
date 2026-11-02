"""Stopping a process the tools started, with what it started: the one tree stop of the
measurement queue's chunks (benches/suite/queue.py) and of /verify's server
(scripts/verify/verifykit/server.py). A process stopped alone leaves its children running, and a
child keeps what it holds: a Windows venv's python.exe is a launcher whose child is the
interpreter, so a server started through it kept its port after its launcher was ended
(windows-job-python, 31 Oct 2026).

    stop(process, grace_s)   SIGTERM to its process group, SIGKILL to what of the group is left
                             after grace_s (Windows: taskkill /T /F). On POSIX the process must
                             lead its own group: started with start_new_session=True. It stops
                             the group also when the process itself has already gone. A
                             descendant that leaves the group (a session of its own) isn't
                             reached: what moved it out stops it. On Windows a tree is found
                             through its live parent: what a parent that has gone left behind
                             isn't found.
"""
from __future__ import annotations

import os
import subprocess

# On Windows, a process the tools start opens no console window (the queue's runner is a
# scheduled task in the owner's session).
QUIET_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)


def stop(process: subprocess.Popen, grace_s: float = 30):
    """Stops a process and its group: SIGTERM to its process group, SIGKILL after `grace_s` to
    whatever of the group is left (Windows: taskkill /T /F). The grace ends when the group is
    gone, not the process: a launcher that SIGTERM ends leaves its child, which may ignore it
    (Sol, windows-job-python r3)."""
    if os.name == "nt":
        subprocess.run(["taskkill", "/T", "/F", "/PID", str(process.pid)], capture_output=True,
                       creationflags=QUIET_WINDOW)
        return
    import signal
    import time
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return  # the group is gone
    deadline = time.monotonic() + grace_s
    while time.monotonic() < deadline:
        process.poll()  # reaped, so that its exit isn't a member still there
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            return
        time.sleep(0.05)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
