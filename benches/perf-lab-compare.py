"""Compares two perf lab reports query by query: rows must match, and p50 is shown with the
speedup. Exit status 1 if any row count differs.

    python benches/perf-lab-compare.py BASELINE.json NEW.json
"""

import json
import sys


def load(path):
    with open(path, encoding="utf-8") as f:
        report = json.load(f)
    return report, {q["name"]: q for q in report["queries"]}


def main() -> int:
    (base_report, base), (new_report, new) = load(sys.argv[1]), load(sys.argv[2])
    print(f"{'query':<28} {'rows':>10} {base_report['label']:>12} {new_report['label']:>12} {'speedup':>9}")
    mismatch = False
    total_base = total_new = 0.0
    for name in sorted(set(base) | set(new)):
        b, n = base.get(name, {}), new.get(name, {})
        if "error" in b or "error" in n or not b or not n:
            print(f"{name:<28} {'':>10} {b.get('p50_ms', b.get('error', '-'))!s:>12.12} {n.get('p50_ms', n.get('error', '-'))!s:>12.12}")
            continue
        rows_note = str(n["rows"]) if b["rows"] == n["rows"] else f"{b['rows']}!={n['rows']}"
        mismatch |= b["rows"] != n["rows"]
        speedup = b["p50_ms"] / n["p50_ms"] if n["p50_ms"] > 0 else float("inf")
        total_base += b["p50_ms"]
        total_new += n["p50_ms"]
        print(f"{name:<28} {rows_note:>10} {b['p50_ms']:>12.2f} {n['p50_ms']:>12.2f} {speedup:>8.1f}x")
    print(f"{'sum (answered by both)':<28} {'':>10} {total_base:>12.1f} {total_new:>12.1f} {total_base / max(total_new, 1e-9):>8.1f}x")
    if mismatch:
        print("ROW COUNT MISMATCH")
    return 1 if mismatch else 0


if __name__ == "__main__":
    sys.exit(main())
