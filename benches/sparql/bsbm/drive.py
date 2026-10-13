#!/usr/bin/env python3
"""BSBM's own test driver (BIBM 0.7.8) against one endpoint, as the suite's external driver
(workloads Plan.external): the procedure of the BSBM V3.1 results (April 2013), whose
Virtuoso 7 figures are the bar, and its qualification before any time.

    drive.py --bibm DIR --td DIR --mix explore|bi --endpoint URL --out ROWS.json
             --system NAME [--qualify DIR] [--clients 1,4,8,64] [--timeout-ms 300000]

The published procedure (results/V7/index.html, sections 4 and 5), per client level:
- 1 client: seed 9834533, explore 2000 warm-up mixes and 500 measured, BI 25 and 10;
- 4, 8 and 64 clients: seeds 8188326, 9175932 and 4187411, two warm-up mixes per client,
  as many measured mixes as clients (the driver's default);
- drill-down mode throughout.

Qualification (`--qualify DIR`): first a single-client run with the driver's qualification
output (DIR/SYSTEM-MIX.qual, the answers of every query of 20 mixes, seed 9834533); then
BIBM's comparator checks this system's answers against every other reference system's
(BSBM_REFERENCES, default jena,virtuoso) that has qualified (its .qual in DIR). Agreeing
with all of them is `ok`; differing from the first is `wrong`; when the references disagree
among themselves the answers are unqualified. Without a reference's answers they are
unchecked. Only `ok` answers are timed: otherwise the client levels' rows are `wrong` or
`failed`, with the reason. `--clients ""` runs the qualification alone (a job's first chunk:
the references qualify, untimed, before the timed chunks).

Rows (the suite's schema): per client level, task `query`, item `MIX-cN`, ms the wall time
per mix (3,600,000 / QMpH, so that less is better like every other time), rows the measured
mixes, the QMpH in the note. The per-query figures stay in the driver's XML beside ROWS.json.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

SEEDS = {1: 9834533, 4: 8188326, 8: 9175932, 64: 4187411}
SINGLE = {"explore": (2000, 500), "bi": (25, 10)}  # warm-up mixes, measured mixes
QUALIFY_RUNS = 20


def bibm(args, extra: list[str], log: Path) -> int:
    command = ["java", "-Xmx2g", "-cp", "bin:lib/*", "com.openlinksw.bibm.bsbm.TestDriver",
               "-qrd", str(args.bibm), "-idir", str(args.td), "-uc", f"bsbm/{args.mix}", "-drill",
               "-t", str(args.timeout_ms), *extra, args.endpoint]
    with open(log, "a", encoding="utf-8") as f:
        f.write("$ " + " ".join(command) + "\n")
        f.flush()
        return subprocess.run(command, cwd=args.bibm, stdout=f, stderr=subprocess.STDOUT).returncode


def element(text: str, name: str) -> str | None:
    m = re.search(rf"<{name}>([^<]*)</{name}>", text)
    return m.group(1).strip() if m else None


def same(args, theirs: Path, mine: Path, log: Path, label: str) -> bool:
    """Whether BIBM's comparator finds two qualification files' answers equal."""
    done = subprocess.run(["java", "-Xmx2g", "-cp", "bin:lib/*", "com.openlinksw.bibm.qualification.Comparator",
                           str(theirs), str(mine)], cwd=args.bibm, capture_output=True, text=True)
    report = (done.stdout + done.stderr).strip()
    with open(log, "a", encoding="utf-8") as f:
        f.write(f"qualification, {label}:\n{report}\n")
    return not (done.returncode != 0 or re.search(r"(?i)differ|mismatch|error|incorrect", report))


def qualify(args, out_dir: Path, log: Path) -> tuple[str, str]:
    """(status, note): this system's answers against every other reference's that qualified:
    ok (all agree), wrong (it differs from the first), unqualified (the references disagree),
    unchecked (no reference has qualified)."""
    mine = args.qualify / f"{args.system}-{args.mix}.qual"
    rc = bibm(args, ["-seed", str(SEEDS[1]), "-runs", str(QUALIFY_RUNS), "-qf", str(mine),
                     "-o", str(out_dir / f"qualify-{args.mix}.xml")], log)
    if rc != 0 or not mine.exists():
        return "failed", f"qualification run exit {rc}"
    references = [r for r in os.environ.get("BSBM_REFERENCES", "jena,virtuoso").split(",") if r and r != args.system]
    found = [r for r in references if (args.qualify / f"{r}-{args.mix}.qual").exists()]
    if not found:
        return "unchecked", f"no reference's answers to check against (BSBM_REFERENCES {','.join(references)})"
    agree = {r: same(args, args.qualify / f"{r}-{args.mix}.qual", mine, log, f"{args.system} against {r}") for r in found}
    if all(agree.values()):
        return "ok", f"answers equal to {', '.join(found)}'s"
    if len(found) > 1 and not same(args, args.qualify / f"{found[0]}-{args.mix}.qual",
                                   args.qualify / f"{found[1]}-{args.mix}.qual", log, f"{found[0]} against {found[1]}"):
        return "unqualified", f"the references {found[0]} and {found[1]} disagree: adjudicate"
    return "wrong", f"answers differ from {', '.join(r for r, ok in agree.items() if not ok)}'s"


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--bibm", type=Path, required=True)
    p.add_argument("--td", type=Path, required=True)
    p.add_argument("--mix", choices=sorted(SINGLE), required=True)
    p.add_argument("--endpoint", required=True)
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--system", required=True)
    p.add_argument("--qualify", type=Path)
    p.add_argument("--clients", default="1,4,8,64")
    p.add_argument("--timeout-ms", type=int, default=300000)
    args = p.parse_args(argv)
    args.bibm, args.td = args.bibm.resolve(), args.td.resolve()
    out_dir = args.out.parent
    log = out_dir / f"bibm-{args.mix}.log"
    rows: list[dict] = []
    verdict, note = "ok", "unchecked"
    if args.qualify:
        args.qualify.mkdir(parents=True, exist_ok=True)
        verdict, note = qualify(args, out_dir, log)
        rows.append({"task": "conformance", "item": f"{args.mix}-qualification",
                     "status": verdict if verdict in ("ok", "wrong", "failed") else "failed", "note": f"{verdict}: {note}"})
    for clients in [int(c) for c in args.clients.split(",") if c.strip()]:
        warmups, runs = SINGLE[args.mix] if clients == 1 else (2 * clients, clients)
        xml = out_dir / f"{args.mix}-c{clients}.xml"
        rc = bibm(args, ["-seed", str(SEEDS[clients]), "-w", str(warmups), "-runs", str(runs),
                         "-mt", str(clients), "-o", str(xml)], log)
        text = xml.read_text(encoding="utf-8", errors="replace") if xml.exists() else ""
        qmph = element(text, "qmph")
        timeouts = sum(int(t) for t in re.findall(r"<timeoutcount>(\d+)</timeoutcount>", text))
        item = f"{args.mix}-c{clients}"
        if rc != 0 or not qmph or float(qmph) <= 0:
            rows.append({"task": "query", "item": item, "repeat": 1, "status": "failed",
                         "note": f"driver exit {rc}; see {log.name}"})
            continue
        # Answers before time: a wrong answer is wrong, unchecked or unqualified ones aren't timed.
        if verdict == "wrong":
            status = "wrong"
        elif verdict != "ok":
            status = "failed"
        else:
            status = "timeout" if timeouts else "ok"
        rows.append({"task": "query", "item": item, "repeat": 1, "status": status,
                     "ms": round(3600000 / float(qmph), 3), "rows": int(element(text, "querymixruns") or runs),
                     "note": f"{float(qmph):.1f} QMpH; {clients} clients; seed {SEEDS[clients]}; "
                             f"{warmups} warm-up mixes; {timeouts} query timeouts; answers {note}"})
    args.out.write_text(json.dumps(rows, indent=1), encoding="utf-8")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
