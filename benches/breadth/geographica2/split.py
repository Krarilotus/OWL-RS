#!/usr/bin/env python3
"""Geographica 2's query lists (geographica2.di.uoa.gr/queries/*.lst, the instances the
benchmark's runtime prints; Apache-2.0) as one file per query, grouped by the data they
read (the suite's tiers): OUT_DIR/<tier>/<list>--<NN>-<name>.rq, the text unchanged.
Strabon's dialect stays as published (`strdf:` functions, `geo:` used without a PREFIX): a
system that rejects a query is reported, the query is not rewritten.

    split.py LIST_DIR OUT_DIR
"""
import re
import sys
from pathlib import Path

HEADER = re.compile(r"^(?:QUERY|Query) (\d+) - ([A-Za-z0-9_.]+):\s*$", re.M)
# The data each list reads: the real-world sets in their graphs, New York's streets, the
# generated set, the scalability slice.
TIER = {"macro_geocoding": "census", "synthetic": "synthetic", "scalability": "scalability"}


def main(source: Path, target: Path) -> int:
    for lst in sorted(source.glob("*.lst")):
        text = lst.read_text(encoding="utf-8")
        heads = list(HEADER.finditer(text))
        tier = TIER.get(lst.stem, "real")
        out = target / tier
        out.mkdir(parents=True, exist_ok=True)
        prefix = "" if tier in ("synthetic", "scalability") else f"{lst.stem}--"
        for i, head in enumerate(heads):
            body = text[head.end(): heads[i + 1].start() if i + 1 < len(heads) else len(text)].strip()
            (out / f"{prefix}{int(head.group(1)):02d}-{head.group(2)}.rq").write_text(body + "\n", encoding="utf-8")
        print(f"{lst.stem}: {len(heads)} queries -> {tier}")
    return 0


if __name__ == "__main__":
    sys.exit(main(Path(sys.argv[1]), Path(sys.argv[2])))
