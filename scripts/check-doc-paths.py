#!/usr/bin/env python3
"""Checks that the repository paths the documentation names exist: every relative Markdown link,
and every path in backticks that starts at a top-level directory (`scripts/check.sh`,
`benches/abba/README.md`, `crates/nrese-dl/src/limits.rs:12`). A doc that names a tool, test or
file that moved or never landed sends its reader looking for nothing, or building it again.

    python3 scripts/check-doc-paths.py            every tracked *.md
    python3 scripts/check-doc-paths.py FILE...    these files (the changed ones)

A path resolves from the document's directory, from the repository root, or, for a path such as
`tests/it/x.rs` or `src/lib.rs` written inside a crate's or the console's prose, from any crate's
or app's directory. A
`:LINE` suffix and a `#anchor` are ignored; anchors aren't checked. Paths with placeholders or
patterns (`<`, `{`, `*`, `…`, `$`, `NAME`, `HOST`) are skipped, as are links to URLs.

A reference that is meant not to exist (a removed directory named as removed, an output a run
writes) goes into scripts/doc-paths.allow, one per line: `DOC TARGET  # why`.

Exit status: 0 when every path exists, 1 when one doesn't (each printed as DOC:LINE: TARGET),
2 on a usage error. Stdlib only; no build.
"""
import glob
import os
import re
import subprocess
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), os.pardir))
ALLOW = os.path.join(ROOT, "scripts", "doc-paths.allow")
TOP = ("scripts/", "docs/", "crates/", "benches/", "apps/", "tests/", "fixtures/", "ops/",
       ".githooks/", ".github/", "fuzz/", ".claude/")
SKIPPED_DIRS = ("literature/", "fixtures/")
LINK = re.compile(r"\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
TICK = re.compile(r"`([^`\n]+)`(\])?")
PLACEHOLDER = re.compile(r"[<>{}*…$|]|\b(NAME|HOST|ID|REV|DIR)\b")


def package_dirs():
    crates = glob.glob(os.path.join(ROOT, "crates", "*")) + glob.glob(os.path.join(ROOT, "crates", "*", "*"))
    apps = glob.glob(os.path.join(ROOT, "apps", "*"))
    return [d for d in crates if os.path.isfile(os.path.join(d, "Cargo.toml"))] + \
        [d for d in apps if os.path.isfile(os.path.join(d, "package.json"))]


CRATES = package_dirs()


def tracked_docs():
    out = subprocess.run(["git", "-C", ROOT, "ls-files", "-z", "--", "*.md"], capture_output=True,
                         check=True).stdout.decode("utf-8")
    return [p for p in out.split("\0") if p and not p.startswith(SKIPPED_DIRS)]


def allowed():
    pairs = set()
    if os.path.exists(ALLOW):
        with open(ALLOW, encoding="utf-8") as fh:
            for line in fh:
                line = line.split("#", 1)[0].split()
                if len(line) == 2:
                    pairs.add((line[0], line[1]))
    return pairs


def clean(path):
    path = path.split("#", 1)[0]
    path = re.sub(r":[0-9][0-9,–-]*$", "", path)
    return path.rstrip(".,;:)")


def exists(doc_dir, path, crate_relative):
    bases = [doc_dir, ROOT] + (CRATES if crate_relative else [])
    return any(os.path.exists(os.path.join(base, path)) for base in bases)


def missing(rel):
    doc_dir = os.path.dirname(os.path.join(ROOT, rel))
    with open(os.path.join(ROOT, rel), encoding="utf-8") as fh:
        for no, line in enumerate(fh, 1):
            for m in LINK.finditer(line):
                target = m.group(1)
                if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("#"):
                    continue
                path = clean(target)
                if path and not PLACEHOLDER.search(path) and not exists(doc_dir, path, False):
                    yield no, target
            for m in TICK.finditer(line):
                if m.group(2):  # link text: the link itself is checked above
                    continue
                words = m.group(1).split()
                token = words[0] if words else ""
                if not token.startswith(TOP + ("src/",)) or PLACEHOLDER.search(token):
                    continue
                path = clean(token)
                crate_relative = path.startswith(("tests/", "src/"))
                if path and not exists(doc_dir, path, crate_relative):
                    yield no, token


def main(argv):
    if any(a.startswith("-") for a in argv):
        print(__doc__.strip().split("\n\n")[1], file=sys.stderr)
        return 2
    docs = [os.path.relpath(os.path.abspath(a), ROOT).replace(os.sep, "/") for a in argv] or tracked_docs()
    allow = allowed()
    bad = 0
    for rel in docs:
        if not rel.endswith(".md") or not os.path.exists(os.path.join(ROOT, rel)):
            continue
        for no, target in missing(rel):
            if (rel, clean(target)) in allow:
                continue
            bad += 1
            print(f"{rel}:{no}: {target}")
    if bad:
        print(f"check-doc-paths: {bad} path(s) the docs name don't exist; fix the doc, or list an "
              "intended one in scripts/doc-paths.allow", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
