#!/usr/bin/env python3
"""NRESE's executed plans for a set of queries: the evidence behind a loss.

    python3 -I explain.py --endpoint URL --queries DIR [--only STEM,...] --out DIR

For each query (the files of DIR, or those named), `explain=true` runs it and returns every
operator with its estimate, actual rows, time (inputs included) and cache use
(docs/ops/http-api.md). Writes OUT/STEM.json and prints per query its total time and its
three costliest operators by their own time (an operator's time less its inputs'), with
the q-error of their estimates: where the time goes and whether the plan was chosen on
wrong estimates.
"""
from __future__ import annotations

import argparse
import json
import sys
import urllib.parse
import urllib.request
from pathlib import Path


def explain(endpoint: str, query: str, timeout_s: float) -> dict:
    data = urllib.parse.urlencode({"query": query, "explain": "true"}).encode()
    with urllib.request.urlopen(urllib.request.Request(endpoint, data=data), timeout=timeout_s) as response:
        return json.loads(response.read())


def own_times(steps: list[dict]) -> list[tuple[float, dict]]:
    """Each step's own time: its micros less those of its direct inputs (the next deeper steps
    until one at its depth or above)."""
    out = []
    for i, step in enumerate(steps):
        children = 0
        for later in steps[i + 1:]:
            if later["depth"] <= step["depth"]:
                break
            if later["depth"] == step["depth"] + 1:
                children += later.get("micros") or 0
        out.append((max((step.get("micros") or 0) - children, 0) / 1000, step))
    return out


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--endpoint", required=True)
    p.add_argument("--queries", type=Path, required=True)
    p.add_argument("--only", default="")
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--timeout-s", type=float, default=600)
    args = p.parse_args(argv)
    args.out.mkdir(parents=True, exist_ok=True)
    only = {s for s in args.only.split(",") if s}
    for path in sorted(args.queries.glob("*.rq")):
        if only and path.stem not in only:
            continue
        try:
            body = explain(args.endpoint, path.read_text(encoding="utf-8"), args.timeout_s)
        except Exception as e:  # a failing query is evidence too
            print(f"{path.stem}: {e}")
            continue
        (args.out / f"{path.stem}.json").write_text(json.dumps(body, indent=1), encoding="utf-8")
        steps = body.get("steps") or []
        print(f"{path.stem}: {(body.get('micros') or 0) / 1000:.1f} ms, {body.get('rows')} rows, rewrites {body.get('rewrites')}")
        for ms, step in sorted(own_times(steps), key=lambda x: -x[0])[:3]:
            e, r = step.get("estimated_rows"), step.get("rows") or 0
            q = (max(e, 1, r) / max(min(max(e, 1), max(r, 1)), 1)) if e is not None else None
            print(f"    {ms:10.1f} ms  {step['operator']:<14} rows {r:<12} est {e!s:<12} q-error {q if q is None else round(q, 1)!s:<8} {step.get('detail', '')[:90]}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
