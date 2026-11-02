#!/usr/bin/env python3
"""Checks that the repository paths the documentation names exist: every relative Markdown link,
and every path in backticks that starts at a top-level directory (`scripts/check.sh`,
`benches/abba/README.md`, `crates/nrese-dl/src/limits.rs:12`). A doc that names a tool, test or
file that moved or never landed sends its reader looking for nothing, or building it again.

    python3 scripts/check-doc-paths.py            every tracked *.md
    python3 scripts/check-doc-paths.py FILE...    these files (the changed ones)

Links: what a CommonMark parser (markdown-it-py, CommonMark mode) makes a link or an image, from
its tokens: inline links, autolinks, images, and every reference definition it records (in lists,
blockquotes, with escaped labels), plus the `href` and `src` of raw HTML. What it doesn't make a
link (text in code, a broken bracket) isn't one. A link resolves from its document's directory
only, as a renderer resolves it.

Mentions: a backticked path resolves from the document's directory, from the repository root, or,
for a path such as `tests/it/x.rs` or `src/lib.rs` written inside a crate's or the console's prose,
from any crate's or app's directory.

A `:LINE` suffix and a `#anchor` are ignored; anchors aren't checked. Paths with placeholders or
patterns (`<`, `{`, `*`, `…`, `$`, `NAME`, `HOST`) are skipped, as are links to URLs.

Without FILEs, every tracked doc must be readable: one that isn't (deleted, unreadable) fails. A
named FILE that doesn't exist, or isn't Markdown, is skipped, so a changed-files list may carry
deleted docs; one that exists but can't be read fails.

A mention that is meant not to exist (a directory named as removed, an output a run writes) goes
into scripts/doc-paths.allow, one per line: `DOC TARGET  # why`. The allow list exempts mentions
only: a clickable link to a missing file always fails.

Exit status: 0 when every path exists, 1 when one doesn't or a doc can't be read (each printed as
DOC:LINE: KIND TARGET), 2 on a usage error or without markdown-it-py. Needs markdown-it-py and its
mdurl (pinned in benches/suite/requirements.txt, the gate's suite environment); no build.
"""
import glob
import os
import re
import subprocess
import sys

try:
    from markdown_it import MarkdownIt
except ImportError:
    MarkdownIt = None

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), os.pardir))
ALLOW = os.path.join(ROOT, "scripts", "doc-paths.allow")
TOP = ("scripts/", "docs/", "crates/", "benches/", "apps/", "tests/", "fixtures/", "ops/",
       ".githooks/", ".github/", "fuzz/", ".claude/")
SKIPPED_DIRS = ("literature/", "fixtures/")
TICK = re.compile(r"`([^`\n]+)`(\])?")
HTML_LINK = re.compile(r"""\b(?:href|src)\s*=\s*(?:"([^"]*)"|'([^']*)')""", re.I)
PLACEHOLDER = re.compile(r"[<>{}*…$|]|\b(NAME|HOST|ID|REV|DIR)\b")
URL = re.compile(r"^[a-z][a-z0-9+.-]*:", re.I)


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


def doc_key(path):
    """A doc's name in output and in the allow list: relative to the root, else absolute."""
    path = os.path.abspath(path)
    try:
        return os.path.relpath(path, ROOT).replace(os.sep, "/")
    except ValueError:  # another drive
        return path.replace(os.sep, "/")


def clean(path):
    path = path.split("#", 1)[0]
    path = re.sub(r":[0-9][0-9,–-]*$", "", path)
    return path.rstrip(".,;:)")


def exists(doc_dir, path, crate_relative):
    bases = [doc_dir, ROOT] + (CRATES if crate_relative else [])
    return any(os.path.exists(os.path.join(base, path)) for base in bases)


def parser():
    md = MarkdownIt("commonmark")
    # The destination as written (escapes and entities resolved), not percent-encoded for a URL:
    # it is looked up as a file.
    md.normalizeLink = lambda url: url
    return md


def links(text):
    """(line number, destination) of every link the parser finds in TEXT."""
    env = {}
    tokens = parser().parse(text, env)
    for token in tokens:
        line = token.map[0] + 1 if token.map else 0
        if token.type in ("html_block", "html_inline"):
            for m in HTML_LINK.finditer(token.content):
                yield line, m.group(1) if m.group(1) is not None else m.group(2)
        for child in token.children or ():
            if child.type == "link_open":
                yield line, child.attrGet("href")
            elif child.type == "image":
                yield line, child.attrGet("src")
            elif child.type == "html_inline":
                for m in HTML_LINK.finditer(child.content):
                    yield line, m.group(1) if m.group(1) is not None else m.group(2)
    for reference in env.get("references", {}).values():
        yield reference["map"][0] + 1, reference["href"]


def missing(path):
    """(line, kind, target) of each reference in the doc at PATH whose target doesn't exist:
    kind `link` (clickable: a link, image or reference definition) or `mention` (backticked)."""
    doc_dir = os.path.dirname(os.path.abspath(os.path.join(ROOT, path)))
    with open(os.path.join(ROOT, path), encoding="utf-8") as fh:
        text = fh.read()
    for no, dest in sorted(set(links(text))):
        if not dest or URL.match(dest) or dest.startswith("#"):
            continue
        target = clean(dest)
        if target and not PLACEHOLDER.search(target) and not os.path.exists(os.path.join(doc_dir, target)):
            yield no, "link", dest
    for no, line in enumerate(text.splitlines(), 1):
        for m in TICK.finditer(line):
            if m.group(2):  # link text: the link itself is checked above
                continue
            words = m.group(1).split()
            token = words[0] if words else ""
            if not token.startswith(TOP + ("src/",)) or PLACEHOLDER.search(token):
                continue
            target = clean(token)
            crate_relative = target.startswith(("tests/", "src/"))
            if target and not exists(doc_dir, target, crate_relative):
                yield no, "mention", token


def main(argv):
    if any(a.startswith("-") for a in argv):
        print(__doc__.strip().split("\n\n")[1], file=sys.stderr)
        return 2
    if MarkdownIt is None:
        print("check-doc-paths: needs markdown-it-py (pinned in benches/suite/requirements.txt)", file=sys.stderr)
        return 2
    explicit = bool(argv)
    docs = [os.path.abspath(a) for a in argv] if explicit else [os.path.join(ROOT, p) for p in tracked_docs()]
    allow = allowed()
    bad = 0
    for path in docs:
        key = doc_key(path)
        if explicit and (not key.endswith(".md") or not os.path.exists(path)):
            continue  # a deleted doc in a changed-files list
        try:
            found = list(missing(path))
        except (OSError, UnicodeDecodeError) as error:
            bad += 1
            print(f"{key}: can't be read: {error}")
            continue
        for no, kind, target in found:
            if kind == "mention" and (key, clean(target)) in allow:
                continue
            bad += 1
            print(f"{key}:{no}: {kind} {target}")
    if bad:
        print(f"check-doc-paths: {bad} problem(s): a path the docs name doesn't exist, or a doc can't be "
              "read; fix the doc, or list an intended mention in scripts/doc-paths.allow", file=sys.stderr)
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
