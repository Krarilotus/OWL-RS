"""What a checkout the release gate makes may hold: its revision's files as stored, and besides
them nothing but the gate's scratch (release-gate.sh worktree(); release_gate.py imports it). It
imports nothing of the repository, so release-gate.sh runs it with `python3 -I`, which puts none of
a checkout on Python's path: it checks the checkout of the gate's own code before any Python runs
from there (Sol, PR #60 fix checks 1 and 2: files in the checkout the gate was launched from ran in
the validator before anything was checked).

    python3 -I benches/suite/suitekit/checkout.py REV DIR [--nothing-else]
"""
from __future__ import annotations

import argparse
import hashlib
import os
import stat
import subprocess
import sys
from pathlib import Path

# The objects as stored, for every git call that decides what a side is built from or whether its
# measurement ref may go: no replace ref stands another object in, no graft another parent.
# release-gate.sh exports the same for a whole gate, so the check and the checkout it builds read
# the same objects (PR #54 review: D1-D3, and fix check 1: a replace ref made after the check).
STORED_OBJECTS = {"GIT_NO_REPLACE_OBJECTS": "1", "GIT_GRAFT_FILE": os.devnull}
# Where the gate's own scratch lives in a harness checkout: git-ignored directories at its root,
# on no import path.
SCRATCH = ("tmp", ".cache")


def git(checkout: str, *args: str) -> bytes:
    return subprocess.run(["git", "-C", checkout, *args], capture_output=True, check=True, timeout=120,
                          env={**os.environ, **STORED_OBJECTS}).stdout


def checkout_differs(checkout: str, rev: str, nothing_else: bool = False) -> list[str]:
    """The files of `rev`'s tree that the checkout at `checkout` doesn't hold as stored (missing, not a
    regular file, of other bytes, or of a mode the gate doesn't build from), and every file and
    linked directory besides them, whatever its name: in a harness checkout outside the gate's
    scratch (SCRATCH, the checkout's own directories, not links), with `nothing_else` (a checkout to
    be built) anywhere. HEAD and a clean status trust the index and the attributes: a smudge
    filter, or a change a stat cache can't see (core.checkStat=minimal), leaves other bytes behind
    them (Sol, PR #54 fix check 1 S1), and an ignored file isn't shown at all: one a build reads
    (nrese-server's build.rs embeds apps/nrese-console/dist/; Sol, S1 r1), a module the harness
    imports (benches/suite/csv.pyc before the standard library's; Sol, S1 r3; csv.PYC on Windows),
    or a file it reads (the catalogue loads every benches/cases/*.toml; Sol, PR #60 r1 R1). The
    harness's checkout keeps the gate's scratch (Sol, S1 r2). Each file is hashed here as git
    hashes a blob, so no filter applies. A linked directory (a junction on Windows) isn't walked,
    so it counts itself, a scratch root's link too: it's looked at before the scratch is left out.
    So does a directory the walk can't list: Python opens a file there by its name all the same
    (Sol, PR #60 fix 2 r1: a planted cases/ of mode 0111)."""
    algorithm = git(checkout, "rev-parse", "--show-object-format").decode().strip()
    differ, tracked = [], set()
    for record in git(checkout, "ls-tree", "-r", "-z", "--full-tree", rev).split(b"\0"):
        if not record:
            continue
        meta, path = record.split(b"\t", 1)
        mode, _, blob = meta.split()
        name = path.decode("utf-8", "surrogateescape")
        tracked.add(name)
        if mode not in (b"100644", b"100755"):
            differ.append(f"{name} (mode {mode.decode()})")
            continue
        file = os.path.join(checkout, os.fsdecode(path))
        try:
            regular = stat.S_ISREG(os.lstat(file).st_mode)
            data = Path(file).read_bytes() if regular else b""
        except FileNotFoundError:
            differ.append(f"{name} (missing)")
            continue
        digest = hashlib.new(algorithm, b"blob %d\0" % len(data) + data).hexdigest()
        if not regular or digest != blob.decode():
            differ.append(name)
    junction = getattr(os.path, "isjunction", lambda _: False)
    scratch = () if nothing_else else SCRATCH

    def unlisted(error: OSError):
        where = os.path.relpath(error.filename, checkout).replace(os.sep, "/")
        differ.append(f"{where} (not listed: {error.strerror})")
    for top, directories, files in os.walk(checkout, onerror=unlisted):
        here = os.path.relpath(top, checkout)
        relative = lambda entry: os.path.normpath(os.path.join(here, entry)).replace(os.sep, "/")
        if here == ".":
            directories[:] = [d for d in directories if d != ".git"]
            files = [f for f in files if f != ".git"]  # a worktree's link to its repository
        links = [d for d in directories if os.path.islink(os.path.join(top, d)) or junction(os.path.join(top, d))]
        differ += [f"{relative(d)} (a linked directory)" for d in links]
        directories[:] = [d for d in directories if d not in links and not (here == "." and d in scratch)]
        differ += [f"{extra} (not in the revision)" for extra in map(relative, files) if extra not in tracked]
    return differ


def main() -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("rev", help="the revision the checkout must hold")
    p.add_argument("dir")
    p.add_argument("--nothing-else", action="store_true", help="and no other file: a checkout to be built")
    args = p.parse_args()
    differ = checkout_differs(args.dir, args.rev, args.nothing_else)
    for name in differ[:20]:
        print(f"{args.dir}: {name} isn't {args.rev[:12]}'s as stored", file=sys.stderr)
    if len(differ) > 20:
        print(f"... and {len(differ) - 20} more", file=sys.stderr)
    return 1 if differ else 0


if __name__ == "__main__":
    sys.exit(main())
