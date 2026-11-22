"""Refuses publishing a private bench host's names: at commit (the staged change) and at push (the commits the
push publishes), each called by scripts/check.sh private_terms, the one rule both read.

The terms come only from the private overlay NRESE_BENCH_PRIVATE names (benches/hosts.toml's header):
- derived from its hosts.toml's [host.NAME] tables, each field by its verdict in HOST_FIELDS; a value in a field
  not listed there is derived whole, so a new field is checked before anyone has looked at it;
- and the patterns of the optional publish-deny.txt beside it (one extended regular expression per line, matched
  ignoring case by `grep -E -i` itself, the push guard's matcher, so a pattern means here what it means there),
  which may stay git-ignored there, so a private name need be committed nowhere.
A derived term matches as a whole word. One shorter than SHORTEST can't be matched safely that way: it is counted
and named by its field, never matched alone (a pattern in the list can cover it). Every term is matched line by
line: a pattern's ^ and $ are a line's ends.

It prints where a term was found, never the term or the line. Exit 0: none found; 1: found; 2: could not check
(an overlay that is set and can't be read, a list that can't be used, a failed git read: never "clean").

    python scripts/lib/private_terms.py commit
    python scripts/lib/private_terms.py push BASE HEAD [REF]
        the commits in HEAD that BASE doesn't hold (the hook's BASE: the remote's own head for a branch it has)
"""
from __future__ import annotations

import os
import re
import signal
import subprocess
import sys
import tempfile
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
UNKNOWN = "a field the check doesn't know"  # its name may itself be private: never printed
HOME = re.compile(r"(?:/home/|/Users/|[A-Za-z]:[/\\](?i:users)[/\\])([^/\\\s'\"]+)")
# Literal pathspecs whatever the caller's environment sets (a conflicting mode makes git fail: could not check),
# and the objects as stored: no replace ref or graft stands in for what a push sends (suitekit/checkout.py's
# STORED_OBJECTS).
GIT_ENV = {**os.environ, "GIT_LITERAL_PATHSPECS": "1", "GIT_NO_REPLACE_OBJECTS": "1", "GIT_GRAFT_FILE": os.devnull}
DIFF = ("--no-color", "--no-ext-diff", "--no-textconv", "--text", "--no-renames")


class Unreadable(Exception):
    pass


# A stop by SIGTERM unwinds as an exit (128 + 15, a refusal to check.sh) through the cleanup; while a pattern file
# exists it waits until close() has removed it, so no stop leaves one behind (grep starts only then too).
STOP = {"files": 0, "asked": False}


def stop(*_):
    if STOP["files"]:
        STOP["asked"] = True
    else:
        sys.exit(128 + signal.SIGTERM)


class Terms:
    """The overlay's terms, matched against each line (Git's records end at LF only): the derived words here, the
    list's patterns by grep, from a file of its own that close() removes."""

    def __init__(self, words: re.Pattern | None, patterns: list[str]):
        self.words, self.listed, self.held = words, None, bool(patterns)
        if patterns:
            STOP["files"] += 1  # before the file exists: a stop from here on waits for close()
            try:
                handle, self.listed = tempfile.mkstemp(prefix="nrese-private-terms-", suffix=".txt")
                with os.fdopen(handle, "w", encoding="utf-8", newline="\n") as out:
                    out.write("\n".join(patterns) + "\n")
                grep(self.listed, [""])  # a pattern grep can't use refuses here, before anything is read
            except BaseException:
                self.close()
                raise

    def close(self):
        if not self.held:
            return
        self.held = False
        removed = True
        try:
            if self.listed:
                os.remove(self.listed)
        except OSError:
            removed = False
        finally:
            STOP["files"] -= 1
        why = "removing its own pattern file"  # never the path: it names the user
        if STOP["asked"] and not STOP["files"]:
            if not removed:  # the stop's exit, with what it couldn't do
                print(f"private terms: could not check: {why}: refused")
            sys.exit(128 + signal.SIGTERM)
        if not removed:
            raise Unreadable(why)

    def hits(self, values: list[str]) -> set[int]:
        """The indexes of the values a term matches, line by line."""
        rows = [(i, line) for i, value in enumerate(values) for line in value.split("\n")]
        found = {i for i, line in rows if self.words and self.words.search(line)}
        if self.listed and rows:
            found |= {rows[n][0] for n in grep(self.listed, [line for _, line in rows])}
        return found

    def search(self, value: str) -> bool:
        return bool(self.hits([value]))


def grep(patterns: str, lines: list[str]) -> list[int]:
    """The indexes of the lines one of the file's patterns matches, as `grep -E -i` reads them."""
    try:
        done = subprocess.run(["grep", "-a", "-n", "-E", "-i", "-f", patterns],
                              input=("\n".join(lines) + "\n").encode("utf-8"), capture_output=True, timeout=300)
    except (OSError, subprocess.SubprocessError) as error:
        raise Unreadable(f"grep with the overlay's {LIST}") from error
    if done.returncode not in (0, 1):  # 1 is no match; an error or a signal is never "no match"
        raise Unreadable(f"grep with the overlay's {LIST}")
    # 0 is a match, so records, each ending at LF (a matched line may hold a CR) and numbered within the input;
    # 1 is none, so none. Anything else is no answer, and never the line itself in a message.
    records = done.stdout.split(b"\n")
    if records.pop() or (done.returncode == 0) != bool(records):
        raise Unreadable(f"grep's output with the overlay's {LIST}")
    found = []
    for line in records:
        number = re.match(rb"([1-9][0-9]*):", line)
        # its digits counted before it is read: a number past Python's int limit would raise, path and all
        if not number or len(number.group(1)) > len(str(len(lines))) or int(number.group(1)) > len(lines):
            raise Unreadable(f"grep's output with the overlay's {LIST}")
        found.append(int(number.group(1)) - 1)
    return found


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

    def add(value: str, field: str):
        for term in value.split("\n"):  # matched line by line: a value over several lines is a term per line
            term = term.strip()
            if term:
                (terms.add(term.lower()) if len(term) >= SHORTEST else short.append(field))

    def walk(value, field: str):
        verdict = HOST_FIELDS.get(field, WHOLE)
        label = field if field in HOST_FIELDS else UNKNOWN
        if isinstance(value, dict):
            for key, item in value.items():
                walk(item, f"{field}.{key}" if field else key)
        elif isinstance(value, list):
            for item in value:
                walk(item, field)
        elif verdict == NOT:
            return
        elif verdict == WHOLE:  # a number, a date or a text: as it is written; a flag names nothing
            if not isinstance(value, bool):
                add(str(value), label)
        elif not isinstance(value, str):
            raise Unreadable(f"the overlay's {label}: not text")
        elif verdict == SSH:
            user, _, machine = value.rpartition("@")
            add(user, field + " (user)")
            if machine.startswith("["):  # [an IPv6 address]:port
                machine = machine[1:].partition("]")[0]
            elif machine.count(":") == 1:  # host:port
                machine = machine.partition(":")[0]
            add(machine, field + " (host)")
        else:
            for found in HOME.finditer(value):
                add(found.group(1), label)

    for key, entry in hosts.items():
        add(key, "the host's key")
        walk(entry, "")
    return terms, short


def matcher(folder: str) -> tuple[Terms, str]:
    """Every term of the overlay, and what it holds, by count."""
    try:
        overlay = Path(folder)
        hosts = tomllib.loads((overlay / "hosts.toml").read_text(encoding="utf-8")).get("host", {})
    except (OSError, ValueError) as error:
        raise Unreadable(f"the overlay's hosts.toml ({type(error).__name__})") from error
    if not isinstance(hosts, dict):
        raise Unreadable("the overlay's [host] table")
    terms, short = derive(hosts)
    words = "|".join(rf"(?<![a-z0-9]){re.escape(t)}(?![a-z0-9])" for t in sorted(terms, key=len, reverse=True))
    listed, patterns = overlay / LIST, []
    if listed.exists() or listed.is_symlink():  # a link to nothing is a list that can't be read, not no list
        try:
            lines = listed.read_text(encoding="utf-8").splitlines()
        except (OSError, ValueError) as error:
            raise Unreadable(f"the overlay's {LIST}") from error
        patterns = [line for line in lines if line]  # an empty line is no pattern (to grep, one matching all)
    held = f"{len(terms)} derived terms, {len(patterns)} patterns from {LIST}"
    if short:
        by_field = ", ".join(sorted(set(short)))
        held += (f"; {len(short)} derived terms too short to match as a word, not checked alone (fields: "
                 f"{by_field}): give each a pattern in {LIST}")
    return Terms(re.compile(words, re.IGNORECASE) if words else None, patterns), held


def added(patch: str) -> str:
    """A patch's added lines. Only a hunk's lines are content: a file's headers come before its first hunk."""
    lines, in_hunk = [], False
    for line in patch.split("\n"):
        if line.startswith("diff "):
            in_hunk = False
        elif line.startswith("@@"):
            in_hunk = True
        elif in_hunk and line.startswith("+"):
            lines.append(line[1:])
    return "\n".join(lines)


def scan(label: str, paths: list[str], patch: str, per_file, terms: Terms) -> list[str]:
    directories = [path.rpartition("/")[0] for path in paths]
    hit = terms.hits(paths + directories)  # one read for every path and directory

    def where(i: int) -> str:
        """A changed path's directory, unless the path or the directory matches."""
        if not directories[i]:
            return "."
        hidden = i in hit or len(paths) + i in hit
        return "a directory whose name matches (not shown)" if hidden else directories[i]

    found = [f"{label}path matches: a file under {where(i)}" for i in range(len(paths)) if i in hit]
    if terms.search(added(patch)):
        located = [f"{label}added lines match in a file under {where(i)}"
                   for i, path in enumerate(paths) if terms.search(added(per_file(path)))]
        found += located or [f"{label}added lines match in a file that could not be located"]
    return found


def staged(terms: Terms) -> tuple[list[str], int]:
    paths = [p for p in text(git("diff", "--cached", "--name-only", "-z", "--no-renames", "--diff-filter=d"))
             .split("\0") if p]
    patch = text(git("diff", "--cached", *DIFF))
    return scan("", paths, patch, lambda p: text(git("diff", "--cached", *DIFF, "--", p)), terms), 0


def pushed(terms: Terms, base: str, head: str, ref: str) -> tuple[list[str], int]:
    found = []
    if ref and terms.search(ref.removeprefix("refs/heads/")):
        found.append("the branch name matches")
    commits = [line.split() for line in text(git("rev-list", "--reverse", "--parents", head, f"^{base}", "--"))
               .splitlines()]
    for commit, *parents in commits:
        short = commit[:9]
        if terms.search(text(git("show", "-s", "--format=%B", commit))):
            found.append(f"commit {short}: message matches")
        # What the commit brings to its branch: against its first parent, so a merge's lines from its other
        # parents are read too.
        trees = (parents[0], commit) if parents else ("--root", commit)
        paths = [p for p in text(git("diff-tree", "-r", "-z", "--name-only", "--no-commit-id", "--no-renames",
                                     "--diff-filter=d", *trees)).split("\0") if p]
        patch = text(git("diff-tree", "-r", "-p", "--no-commit-id", *DIFF, *trees))
        found += scan(f"commit {short}: ", paths, patch,
                      lambda p: text(git("diff-tree", "-r", "-p", "--no-commit-id", *DIFF, *trees, "--", p)), terms)
    return found, len(commits)


def main(argv: list[str]) -> int:
    folder = os.environ.get(PRIVATE, "")
    if not folder:
        print(f"private terms: no overlay configured ({PRIVATE}), nothing to check")
        return 0
    if argv[:1] != ["commit"] and not (argv[:1] == ["push"] and len(argv) in (3, 4)):
        print("usage: private_terms.py commit | push BASE HEAD [REF]", file=sys.stderr)
        return 2
    try:
        terms, held = matcher(folder)
        try:
            found, commits = (staged(terms) if argv[0] == "commit"
                              else pushed(terms, argv[1], argv[2], (argv[3:] or [""])[0]))
        finally:
            terms.close()
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
    signal.signal(signal.SIGTERM, stop)
    sys.exit(main(sys.argv[1:]))
