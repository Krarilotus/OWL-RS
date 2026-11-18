#!/usr/bin/env python3
"""Names Python can't resolve, found before a host runs the code: pyflakes' undefined-name findings
(and syntax errors) over the given files, nothing else it reports. A suite test that replaces a
whole function never runs that function's body, so a name missing there (`ROOT` after #132's
merge, 11 Nov 2026) passed every test and the review, and failed only on a fleet host.

    python scripts/check-python-names.py FILE...

What it finds is what pyflakes decides from one file: a name read that no assignment, import,
definition or builtin in scope provides (`from x import *` hides its module's names from it). It
doesn't find a missing attribute or a name another module stopped exporting. Exit 1 with one line
per finding, 0 with none, 2 without pyflakes (pinned in benches/suite/requirements.txt, the gate's
suite environment). Paths that don't exist (deleted in the change) are skipped.
"""
import sys
from pathlib import Path


def main(paths: list[str]) -> int:
    try:
        from pyflakes import api, messages, reporter
    except ImportError:
        print("check-python-names: needs pyflakes (pinned in benches/suite/requirements.txt)", file=sys.stderr)
        return 2
    kinds = (messages.UndefinedName, messages.UndefinedLocal, messages.UndefinedExport)
    found: list[str] = []

    class Undefined(reporter.Reporter):
        def flake(self, message):
            if isinstance(message, kinds):
                found.append(str(message))

        def syntaxError(self, filename, msg, lineno, offset, text):
            found.append(f"{filename}:{lineno}: {msg}")

        def unexpectedError(self, filename, msg):
            found.append(f"{filename}: {msg}")

    report = Undefined(sys.stdout, sys.stderr)
    for path in paths:
        if Path(path).is_file():
            api.checkPath(path, report)
    for line in found:
        print(line)
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
