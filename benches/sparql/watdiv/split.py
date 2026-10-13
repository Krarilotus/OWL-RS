#!/usr/bin/env python3
"""WatDiv's instantiated templates (queries.sh: one TEMPLATE.txt each, queries separated by a
blank line) as the suite's query set: TEMPLATE-N.rq, numbered in the generator's order,
an instance identical to an earlier one of its template left out.

    python3 -I split.py RAW-DIR TARGET-DIR
"""
from __future__ import annotations

import sys
from pathlib import Path


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__)
        return 2
    raw, target = Path(argv[1]), Path(argv[2])
    target.mkdir(parents=True, exist_ok=True)
    for old in target.glob("*.rq"):
        old.unlink()
    kept = dropped = 0
    for source in sorted(raw.glob("*.txt")):
        seen: set[str] = set()
        blocks = [b.strip() for b in source.read_text(encoding="utf-8").replace("\r\n", "\n").split("\n\n") if b.strip()]
        for block in blocks:
            if block in seen:
                dropped += 1
                continue
            seen.add(block)
            name = f"{source.stem}-{len(seen)}.rq"
            with open(target / name, "w", encoding="utf-8", newline="\n") as f:
                f.write(f"# WatDiv v0.6 basic testing, template {source.stem}, instance {len(seen)}\n{block}\n")
            kept += 1
    print(f"{kept} queries into {target}; {dropped} repeated instances left out")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
