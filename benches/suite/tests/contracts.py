"""The suite contracts a change reaches, by what each contract reads.

A contract (benches/suite/tests/test_*.py, and crate_scan.py, the crate scan) declares the repository
paths it reads in a module-level literal:

    READS = ("scripts/check.sh", "scripts/lib/*.sh", "crates/**/src/**/*.rs", "benches/fast/")

each a path, a glob (`*` and `?` within one path segment, `**/` any directories) or a directory
ending in `/` (everything under it). A change reaches a contract when a changed file is one of its
reads, its own file, or a file every contract reads (EVERY: the runner, this selector, the suite's
Python and its packages). A contract that declares nothing, or whose READS isn't a literal of
strings, is reached by every change: it always runs. The gate's push and Windows tiers run the
contracts a change reaches (scripts/check.sh); the release gate runs them all.

    python benches/suite/tests/contracts.py select [--commit COMMIT] < FILES

prints the contracts the changed files (one per line on stdin) reach, one module name per line:
nothing when that is every contract, `none` when it is none. A caller runs every contract where it
prints nothing or fails. With COMMIT the declarations are COMMIT's own (git), else the work tree's.
It imports nothing of the suite's, so any python runs it.

benches/suite/tests/test_contract_reads.py keeps the declarations honest.
"""
from __future__ import annotations

import ast
import re
import subprocess
import sys
from pathlib import Path

TESTS = "benches/suite/tests"
EVERY = frozenset({f"{TESTS}/isolation.py", f"{TESTS}/conftest.py", f"{TESTS}/contracts.py",
                   "benches/suite/requirements.txt", "scripts/lib/gate-tools.txt"})
CONTRACT = re.compile(rf"{re.escape(TESTS)}/(test_\w+|crate_scan)\.py")


def declared(source: str) -> tuple[str, ...] | None:
    """The reads SOURCE declares (READS, a literal tuple or list of non-empty strings), or None."""
    try:
        tree = ast.parse(source)
    except SyntaxError:
        return None
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "READS" for t in node.targets):
            try:
                value = ast.literal_eval(node.value)
            except ValueError:
                return None
            if isinstance(value, (tuple, list)) and all(isinstance(v, str) and v for v in value):
                return tuple(value)
            return None
    return None


def pattern(read: str) -> re.Pattern:
    """READ as a regular expression over repository paths."""
    if read.endswith("/"):
        return re.compile(re.escape(read) + ".*")
    out, i = "", 0
    while i < len(read):
        if read.startswith("**/", i):
            out, i = out + "(?:[^/]+/)*", i + 3
        elif read.startswith("**", i):
            out, i = out + ".*", i + 2
        elif read[i] == "*":
            out, i = out + "[^/]*", i + 1
        elif read[i] == "?":
            out, i = out + "[^/]", i + 1
        else:
            out, i = out + re.escape(read[i]), i + 1
    return re.compile(out)


def reaches(own: str, reads: tuple[str, ...] | None, path: str) -> bool:
    """Whether a change to PATH reaches the contract at OWN that declares READS."""
    return reads is None or path == own or path in EVERY or any(pattern(r).fullmatch(path) for r in reads)


def select(contracts: dict[str, tuple[str, tuple[str, ...] | None]], changed: list[str]) -> list[str] | None:
    """The module names of CONTRACTS (name -> (its file, its reads)) the CHANGED files reach, or None
    when that is every one."""
    picked = sorted(name for name, (own, reads) in contracts.items() if any(reaches(own, reads, f) for f in changed)
                    or reads is None)
    return None if len(picked) == len(contracts) else picked


def work_tree(top: Path) -> dict[str, str]:
    return {p.relative_to(top).as_posix(): p.read_text(encoding="utf-8")
            for p in sorted((top / TESTS).glob("*.py")) if CONTRACT.fullmatch(p.relative_to(top).as_posix())}


def committed(commit: str) -> dict[str, str]:
    """The contracts' sources at COMMIT, read from git in one batch."""
    # NUL-ended names, as they are: one outside ASCII is never quoted out of the contracts.
    names = subprocess.run(["git", "ls-tree", "-z", "--name-only", commit, f"{TESTS}/"], capture_output=True,
                           check=True, timeout=60).stdout.decode("utf-8").split("\0")
    names = [n for n in names if CONTRACT.fullmatch(n)]
    batch = subprocess.run(["git", "cat-file", "--batch"], input="".join(f"{commit}:{n}\n" for n in names).encode(),
                           capture_output=True, check=True, timeout=60).stdout
    sources, at = {}, 0
    for name in names:
        header_end = batch.index(b"\n", at)
        size = int(batch[at:header_end].split()[2])
        sources[name] = batch[header_end + 1:header_end + 1 + size].decode("utf-8")
        at = header_end + 1 + size + 1
    return sources


def main(argv: list[str]) -> int:
    if not argv or argv[0] != "select" or len(argv) not in (1, 3) or (len(argv) == 3 and argv[1] != "--commit"):
        print("usage: contracts.py select [--commit COMMIT] < FILES", file=sys.stderr)
        return 2
    sources = committed(argv[2]) if len(argv) == 3 else work_tree(Path(__file__).resolve().parents[3])
    if not sources:
        print("no contracts found", file=sys.stderr)
        return 2
    contracts = {Path(path).stem: (path, declared(text)) for path, text in sources.items()}
    # The gate's list as git gives it unquoted (scripts/check.sh gate_files): UTF-8 whatever the console's code page.
    lines = sys.stdin.buffer.read().decode("utf-8", "surrogateescape").split("\n")
    changed = [line.strip("\r") for line in lines if line.strip("\r")]
    picked = select(contracts, changed)
    if picked is not None:
        print("\n".join(picked) if picked else "none")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
