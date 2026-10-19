"""The host lock every benchmark tool takes (shared dataset preparation, a gate's builds, a job's
image pins): an OS lock on an open file, which the kernel releases when its holder dies, however
it dies. fcntl.flock on Linux (the same lock as `flock(1)` in shell scripts), msvcrt.locking on
Windows. Standard library only, Python 3.8 on: scripts/lib/ns.sh runs this file where `flock`
is missing.

    with lock(path): ...
    python lock.py FILE -- COMMAND...    COMMAND under the lock, its exit code
"""
from __future__ import annotations

import contextlib
import os
import subprocess
import sys
import time
from pathlib import Path


@contextlib.contextmanager
def lock(path):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(str(path), os.O_RDWR | os.O_CREAT, 0o666)
    try:
        if os.name == "nt":
            import msvcrt
            while True:  # LK_LOCK gives up after ten tries of a second: wait on
                try:
                    os.lseek(fd, 0, os.SEEK_SET)
                    msvcrt.locking(fd, msvcrt.LK_LOCK, 1)
                    break
                except OSError:
                    time.sleep(0.05)
            try:
                yield
            finally:
                os.lseek(fd, 0, os.SEEK_SET)
                msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
        else:
            import fcntl
            fcntl.flock(fd, fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(fd, fcntl.LOCK_UN)
    finally:
        os.close(fd)


def main(argv: list) -> int:
    if len(argv) < 3 or argv[1] != "--":
        print("usage: lock.py FILE -- COMMAND...", file=sys.stderr)
        return 2
    with lock(argv[0]):
        return subprocess.call(argv[2:])


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
