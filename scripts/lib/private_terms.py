"""Refuses publishing a private bench host's names: at commit (the staged change) and at push (the commits the
push publishes), each called by scripts/check.sh private_terms, the one rule both read.

The terms come only from the private overlay NRESE_BENCH_PRIVATE names (benches/hosts.toml's header):
- derived from its hosts.toml's [host.NAME] tables, each field by its verdict in HOST_FIELDS; a string in a field
  not listed there is derived whole, so a new field is checked before anyone has looked at it;
- and the patterns of the optional publish-deny.txt beside it (one regular expression per line, matched ignoring
  case), which may stay git-ignored there, so a private name need be committed nowhere.
A derived term matches as a whole word, ignoring case. One shorter than SHORTEST can't be matched safely that
way: it is counted and named by its field, never matched alone (a pattern in the list can cover it).

It prints where a term was found, never the term or the line. Exit 0: none found; 1: found; 2: could not check
(an overlay that is set and can't be read, a list that can't be used, a failed git read: never "clean").

    python scripts/lib/private_terms.py commit
    python scripts/lib/private_terms.py push BASE HEAD [REF [REMOTE]]
        the commits in HEAD that neither BASE ("-" for none) nor REMOTE's branches (origin's) already hold
"""
from __future__ import annotations

import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

PRIVATE = "NRESE_BENCH_PRIVATE"
LIST = "publish-deny.txt"
SHORTEST = 3

WHOLE, SSH, HOMES, NOT = "whole", "the user and host of user@host[:port]", "home-directory user names", "none"
# Each [host.NAME] field (a subfield as table.field; a list's items share its name) and what of it is a term.
HOST_FIELDS = {
    "hostname": WHOLE,
    "ssh": SSH,  # the user can be a person's name
    "assets": NOT, "cpus": NOT, "has": NOT, "roles": NOT, "runtime": NOT,  # flags, counts, capability names
    # A configuration's description: what changes timings, no names.
    "config.since": NOT, "config.toolchain": NOT, "config.governor": NOT, "config.cold_cache": NOT,
    "config.cpus": NOT, "config.perf_event_paranoid": NOT,
    "config.note": HOMES,  # free text: its home directories; anything else in it only through the list
    # The host's queue: paths and commands (their home directories) and its numbers and windows.
    "queue.dir": HOMES, "queue.repo": HOMES, "queue.results": HOMES, "queue.path": HOMES,
    "queue.receive_pack": HOMES, "queue.python": HOMES, "queue.free": HOMES, "queue.keep": HOMES,
    "queue.retreat": HOMES,
    "queue.chunk_min": NOT, "queue.max_job_min": NOT, "queue.timing": NOT, "queue.windows": NOT,
    "queue.weekend_windows": NOT,
}
HOME = re.compile(r"(?:/home/|/Users/|[A-Za-z]:[/\\]Users[/\\])([^/\\\s'\"]+)")
# Literal pathspecs whatever the caller's environment sets; a conflicting mode makes git fail (could not check).
GIT_ENV = {**os.environ, "GIT_LITERAL_PATHSPECS": "1"}
DIFF = ("--no-color", "--no-ext-diff", "--no-textconv", "--text", "--no-renames")


class Unreadable(Exception):
    pass


def git(*args: str) -> bytes:
    try:
        done = subprocess.run(["git", *args], capture_output=True, env=GIT_ENV, timeout=300)
    except (OSError, subprocess.SubprocessError) as error:
        raise Unreadable(f"git {args[0]}") from error
    if done.returncode:
        raise Unreadable(f"git {args[0]}")
    return done.stdout


def text(data: bytes) -> str:
    return data.decode("utf-8", "replace")


def derive(hosts: dict) -> tuple[set[str], list[str]]:
    """The host table's terms, and the fields of the terms too short to match as a word."""
    terms, short = set(), []

    def add(term: str, field: str):
        term = term.strip()
        if term:
            (terms.add(term.lower()) if len(term) >= SHORTEST else short.append(field))

    def walk(value, field: str):
        if isinstance(value, dict):
            for key, item in value.items():
                walk(item, f"{field}.{key}" if field else key)
        elif isinstance(value, list):
            for item in value:
                walk(item, field)
        elif isinstance(value, str):
            verdict = HOST_FIELDS.get(field, WHOLE)
            if verdict == WHOLE:
                add(value, field)
            elif verdict == SSH:
                user, _, machine = value.rpartition("@")
                add(user, field + " (user)")
                add(machine.split(":")[0], field + " (host)")
            elif verdict == HOMES:
                for found in HOME.finditer(value):
                    add(found.group(1), field)

    for key, entry in hosts.items():
        add(key, "the host's key")
        walk(entry, "")
    return terms, short


def matcher(folder: str) -> tuple[re.Pattern, str]:
    """One pattern for every term of the overlay, and what it holds, by count."""
    try:
        overlay = Path(folder)
        hosts = tomllib.loads((overlay / "hosts.toml").read_text(encoding="utf-8")).get("host", {})
    except (OSError, ValueError) as error:
        raise Unreadable(f"the overlay's hosts.toml ({type(error).__name__})") from error
    if not isinstance(hosts, dict):
        raise Unreadable("the overlay's [host] table")
    terms, short = derive(hosts)
    parts = [rf"(?<![a-z0-9]){re.escape(t)}(?![a-z0-9])" for t in sorted(terms, key=len, reverse=True)]
    listed = overlay / LIST
    patterns = 0
    if listed.exists():
        try:
            lines = listed.read_text(encoding="utf-8").splitlines()
        except (OSError, ValueError) as error:
            raise Unreadable(f"the overlay's {LIST}") from error
        for number, line in enumerate(lines, 1):
            if not line.strip():
                continue
            try:
                re.compile(line, re.IGNORECASE)
            except re.error as error:
                raise Unreadable(f"line {number} of the overlay's {LIST}, which is no pattern") from error
            parts.append(f"(?:{line})")
            patterns += 1
    held = f"{len(terms)} derived terms, {patterns} patterns from {LIST}"
    if short:
        by_field = ", ".join(sorted(set(short)))
        held += (f"; {len(short)} derived terms too short to match as a word, not checked alone (fields: "
                 f"{by_field}): give each a pattern in {LIST}")
    return re.compile("|".join(parts) or r"(?!)", re.IGNORECASE), held


def where(path: str, terms: re.Pattern) -> str:
    """A changed path's directory, unless the directory's own name holds a term."""
    directory = path.rpartition("/")[0] or "."
    return "a directory whose name matches (not shown)" if terms.search(directory) else directory


def added(patch: str) -> str:
    """A patch's added lines (one marker column per parent in a merge's combined diff). Only a hunk's lines are
    content: a file's headers come before its first hunk header."""
    lines, columns, in_hunk = [], 1, False
    for line in patch.splitlines():
        if line.startswith("diff "):
            in_hunk = False
        elif line.startswith("@@"):
            in_hunk, columns = True, len(line) - len(line.lstrip("@")) - 1
        elif in_hunk and "+" in line[:columns] and set(line[:columns]) <= {"+", " "}:
            lines.append(line[columns:])
    return "\n".join(lines)


def scan(label: str, paths: list[str], patch: str, per_file, terms: re.Pattern) -> list[str]:
    found = []
    for path in paths:
        if terms.search(path):
            found.append(f"{label}path matches: a file under {where(path, terms)}")
    if terms.search(added(patch)):
        located = [f"{label}added lines match in a file under {where(path, terms)}"
                   for path in paths if terms.search(added(per_file(path)))]
        found += located or [f"{label}added lines match in a file that could not be located"]
    return found


def staged(terms: re.Pattern) -> tuple[list[str], int]:
    paths = [p for p in text(git("diff", "--cached", "--name-only", "-z", "--no-renames", "--diff-filter=d"))
             .split("\0") if p]
    patch = text(git("diff", "--cached", *DIFF))
    return scan("", paths, patch, lambda p: text(git("diff", "--cached", *DIFF, "--", p)), terms), 0


def pushed(terms: re.Pattern, base: str, head: str, ref: str, remote: str) -> tuple[list[str], int]:
    found = []
    if ref and terms.search(ref.removeprefix("refs/heads/")):
        found.append("the branch name matches")
    not_in = ([] if base == "-" else [f"^{base}"]) + ["--not", f"--remotes={remote}"]
    commits = [line.split() for line in text(git("rev-list", "--reverse", "--parents", head, *not_in, "--"))
               .splitlines()]
    for commit, *parents in commits:
        short = commit[:9]
        if terms.search(text(git("show", "-s", "--format=%B", commit))):
            found.append(f"commit {short}: message matches")
        shape = ("--cc",) if len(parents) > 1 else ()
        paths = [p for p in text(git("diff-tree", "-r", "-z", "--name-only", "--no-commit-id", "--root",
                                     "--no-renames", "--diff-filter=d", *shape, commit)).split("\0") if p]
        patch = text(git("diff-tree", "-p", "--no-commit-id", "--root", *DIFF, *shape, commit))
        found += scan(f"commit {short}: ", paths, patch,
                      lambda p: text(git("diff-tree", "-p", "--no-commit-id", "--root", *DIFF, *shape, commit,
                                         "--", p)), terms)
    return found, len(commits)


def main(argv: list[str]) -> int:
    folder = os.environ.get(PRIVATE, "")
    if not folder:
        print(f"private terms: no overlay configured ({PRIVATE}), nothing to check")
        return 0
    try:
        terms, held = matcher(folder)
        if argv[:1] == ["commit"]:
            found, commits = staged(terms)
        elif argv[:1] == ["push"] and len(argv) >= 3:
            found, commits = pushed(terms, argv[1], argv[2], (argv[3:] or [""])[0], (argv[4:] or ["origin"])[0])
        else:
            print("usage: private_terms.py commit | push BASE HEAD [REF [REMOTE]]", file=sys.stderr)
            return 2
    except Unreadable as error:
        print(f"private terms: could not check: {error}: refused")
        return 2
    print(f"private terms: {held}" + (f"; {commits} commits" if argv[0] == "push" else ""))
    for line in found:
        print(f"private terms: {line}")
    if found:
        print("private terms: refused: a private bench name would be published")
        return 1
    print("private terms: none found")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
