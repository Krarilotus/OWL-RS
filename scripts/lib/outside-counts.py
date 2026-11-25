"""The outside check's counts and verdict in the nightly tier: how many cases reached the final comparison.

    python scripts/lib/outside-counts.py OUT FIXED_DRAWN FIRST_FRESH FRESH_DRAWN

OUT holds fixed/ and fresh/, each a benches/reasoning/dl/outside.sh run (its summary.tsv: seed, shape,
tags, name, class, outcome, detail; its exit code in `exit`, scripts/check.sh), over seeds
0..FIXED_DRAWN-1 and FIRST_FRESH.. (FRESH_DRAWN of them). A case where the references split on a key
control (control-split) is dropped whole: its other rows rest on that control. Any other case is compared
when one of its rows reached pass, pass-one, FAIL or FAIL-one, and is otherwise dropped by its first
reason: a reference gap (a key over a property that isn't simple, set aside whole), the references split
(set-aside, control-split, control-undecided), no reference answer (reference-undecided, not-judged), the
store declined every write (declined-*), or missing (no row: it never reached the summary). A compared
case's FAIL and FAIL-one rows, both references against the store, are its disagreements.

A block's run ended cleanly when outside.sh exited 0, or 101 (its test found failing rows) with a
FAIL, FAIL-one or control-split row in its summary. Anything else (a seed missing, a planted fault
missed, a key control not established, a crash, a timeout) is an error in either block.

Writes OUT/outside.json and prints one gate-log line per block. The fixed block decides: an error, a
disagreement, or fewer than half of its drawn cases compared (an empty run) is a failure (exit 1). The
fresh block's disagreements never are; they are listed as new_disagreements, for a reader to file; its
error is a failure.
Exit: 0, 1 (a failure), 2 (a usage error).
"""
import json
import os
import sys
from collections import Counter

COMPARED = {"pass", "pass-one", "FAIL", "FAIL-one"}
DROPPED = [("reference gap", lambda o: o == "reference-gap"),
           ("references split", lambda o: o in {"set-aside", "control-split", "control-undecided"}),
           ("no reference answer", lambda o: o in {"reference-undecided", "not-judged"}),
           ("store declined", lambda o: o.startswith("declined-"))]
FLOOR = 0.5  # of the fixed block's drawn cases, compared


def block(directory, seeds):
    """Counts of the cases of SEEDS in DIRECTORY's summary.tsv, its disagreements, and its run's error."""
    rows = {}
    try:
        with open(os.path.join(directory, "summary.tsv"), encoding="utf-8") as f:
            for line in f:
                cols = line.rstrip("\n").split("\t")
                if len(cols) >= 6 and cols[0].isdigit():
                    rows.setdefault(int(cols[0]), []).append(cols)
    except OSError:
        pass
    reasons, compared, differ = Counter(), 0, []
    for seed in seeds:
        outcomes = {cols[5] for cols in rows.get(seed, [])}
        if outcomes & COMPARED and "control-split" not in outcomes:
            compared += 1
            differ += [{"seed": seed, "query": c[3], "verdict": c[5]} for c in rows[seed] if c[5].startswith("FAIL")]
            continue
        why = next((name for name, test in DROPPED if any(map(test, outcomes))), "missing" if not outcomes else
                   "other: " + ",".join(sorted(outcomes)))
        reasons[why] += 1
    try:
        with open(os.path.join(directory, "exit"), encoding="utf-8") as f:
            code = f.read().strip()
    except OSError:
        code = "none"
    failing = any(c[5].startswith("FAIL") or c[5] == "control-split" for r in rows.values() for c in r)
    error = None if code == "0" or (code == "101" and failing) else f"outside.sh exit {code}"
    return {"drawn": len(seeds), "first": seeds[0] if seeds else None, "compared": compared,
            "dropped": dict(reasons), "error": error}, differ


if len(sys.argv) != 5 or not all(a.isdigit() for a in sys.argv[2:]):
    sys.exit(__doc__.split("\n\n")[1])
out, fixed_n, first, fresh_n = sys.argv[1], *map(int, sys.argv[2:])
fixed, failures = block(os.path.join(out, "fixed"), list(range(fixed_n)))
fresh, new = block(os.path.join(out, "fresh"), list(range(first, first + fresh_n)))
empty = fixed["compared"] < FLOOR * fixed["drawn"]
result = {"fixed": {**fixed, "floor": FLOOR, "empty": empty, "disagreements": failures},
          "fresh": fresh, "new_disagreements": new}
with open(os.path.join(out, "outside.json"), "w", encoding="utf-8") as f:
    json.dump(result, f, indent=1)


def said(counts):
    dropped = ", ".join(f"{k} {v}" for k, v in sorted(counts["dropped"].items())) or "none"
    return dropped, f"; ERROR: {counts['error']}" if counts["error"] else ""


dropped, error = said(fixed)
print(f"outside: seeds 0..{fixed_n - 1}, drawn {fixed['drawn']}, compared {fixed['compared']}, dropped: "
      f"{dropped}; disagreements {len(failures)}" + ("; EMPTY RUN: fewer than half compared" if empty else "") + error)
dropped, error = said(fresh)
print(f"outside discovery: seeds {first}..{first + fresh_n - 1}, compared {fresh['compared']}, dropped: "
      f"{dropped}; new disagreements {len(new)}{error}")
sys.exit(1 if empty or failures or fixed["error"] or fresh["error"] else 0)
