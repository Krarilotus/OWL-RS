#!/usr/bin/env python3
"""Two runs' captured answers, system by system and query by query: whether a runtime (or a
host) changes what a system answers (benches/sparql/README.md, the rootfs runtime's
qualification against Docker).

    python3 benches/sparql/same_answers.py RUN-A RUN-B

RUN-A and RUN-B are suite result directories with answers.json (`suite.py run --answers
full`). Per system and query: `same` where the captured results are identical, else the
suite's own comparison of the two as bags (suitekit/answers.py, with the query's operators
as the run recorded them); a capture missing or failed on one side is listed. Exit status 1
when any answer differs.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "suite"))
from suitekit.answers import compare, parse  # noqa: E402


def captures(run: Path) -> dict[tuple[str, str], dict]:
    data = json.loads((run / "answers.json").read_text(encoding="utf-8"))
    out = {}
    for c in data.get("captures", []):
        if c.get("cache", "off") != "off" or c.get("unlimited"):
            continue
        out.setdefault((c["system"], c["item"][-1]), c)
    return out


def operators(run: Path) -> dict[str, dict[str, str]]:
    """Each query's COUNT and division variables as the run recorded them, by query file; a
    query whose run didn't record them (before T8) is left out: unknown, not none."""
    data = json.loads((run / "answers.json").read_text(encoding="utf-8"))
    return {i["item"][-1]: i["operators"] for i in data.get("items", []) if i.get("operators") is not None}


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        print(__doc__)
        return 2
    a_dir, b_dir = Path(argv[1]), Path(argv[2])
    a, b = captures(a_dir), captures(b_dir)
    ops = operators(b_dir) | operators(a_dir)
    same = differ = missing = 0
    for key in sorted(set(a) | set(b)):
        system, query = key
        x, y = a.get(key), b.get(key)
        if x is None or y is None or x.get("error") or y.get("error"):
            missing += 1
            print(f"missing  {system:<10} {query}: "
                  f"{'absent' if x is None else x.get('error') or 'ok'} | {'absent' if y is None else y.get('error') or 'ok'}")
            continue
        if x["sha256"] == y["sha256"]:
            same += 1
            continue
        answer_a = parse((a_dir / x["path"]).read_text(encoding="utf-8"))
        answer_b = parse((b_dir / y["path"]).read_text(encoding="utf-8"))
        result = compare(answer_a, answer_b, "bag", ops=ops.get(query))
        if result["verdict"] == "ok":
            same += 1
            if not result["strict_match"]:
                print(f"values   {system:<10} {query}: {result['note']}")
        else:
            differ += 1
            print(f"DIFFERS  {system:<10} {query}: {result['verdict']} {result['note']}")
    print(f"{same} equal, {differ} differ, {missing} missing or failed on a side")
    return 1 if differ else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
