#!/usr/bin/env python3
"""The ORE 2015 development subset on the reference reasoners (docs/design/owl2-dl.md §11).

    python ore.py subset ZIP CASES [--track dl/classification] [--count 40] [--max-bytes 5000000]
    python ore.py manifest CASES REASONER...
    python reference.py run CASES --timeout 300
    python ore.py compare CASES [--out results/ore-dev-DATE.tsv]

- The corpus: Zenodo record 18578 (`ore2015_sample.zip`, MD5
  109f04cf8f124eb551d33c100e549730, CC BY-NC-ND 4.0: used here, never committed or
  redistributed), under `.cache/ore2015/`.
- The subset: from a track's `fileorder.txt` (the competition's order), the ontologies up
  to `--max-bytes`, sorted by size, every k-th, so that it spans the sizes; deterministic.
- The comparison: per ontology the canonical taxonomies' hashes (classification) or the
  answers (consistency). Reference reasoners that disagree are listed: such a case is
  settled by hand and recorded in `disputed.tsv`, never by a vote.
"""

import argparse
import collections
import csv
import pathlib
import sys
import zipfile

TASKS = {"classification": "classify", "consistency": "consistency"}
EL_ONLY = {"elk"}


def subset(args):
    z = zipfile.ZipFile(args.zip)
    base = f"pool_sample/{args.track}"
    order = z.read(f"{base}/fileorder.txt").decode().split()
    sizes = {i.filename.rsplit("/", 1)[-1]: i.file_size for i in z.infolist()
             if i.filename.startswith("pool_sample/files/")}
    pool = sorted((sizes[f], f) for f in order if f in sizes and sizes[f] <= args.max_bytes)
    step = max(1, len(pool) // args.count)
    chosen = [f for _, f in pool[::step]][: args.count]
    cases = pathlib.Path(args.cases)
    (cases / "files").mkdir(parents=True, exist_ok=True)
    task = TASKS[args.track.split("/")[1]]
    profile = args.track.split("/")[0]
    rows = []
    for f in chosen:
        target = cases / "files" / f
        if not target.exists():
            target.write_bytes(z.read(f"pool_sample/files/{f}"))
        rows.append((f.removesuffix(".owl"), args.track, profile, task, f"files/{f}", sizes[f]))
    index = cases / "index.tsv"
    old = []
    if index.exists():
        old = [r for r in csv.reader(open(index, encoding="utf-8"), delimiter="\t")][1:]
        old = [r for r in old if r[1] != args.track]
    with open(index, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh, delimiter="\t", lineterminator="\n")
        w.writerow(["id", "track", "profile", "task", "file", "bytes"])
        w.writerows(old + [list(map(str, r)) for r in rows])
    print(f"{len(rows)} ontologies of {len(pool)} up to {args.max_bytes} bytes from {args.track}")


def read_index(cases):
    with open(pathlib.Path(cases) / "index.tsv", encoding="utf-8") as f:
        return list(csv.DictReader(f, delimiter="\t"))


def manifest(args):
    lines = []
    for r in read_index(args.cases):
        for reasoner in args.reasoners:
            if reasoner in EL_ONLY and r["profile"] != "el":
                continue
            tid = f"{r['id']}-{r['track'].replace('/', '-')}"
            lines.append("\t".join([tid, reasoner, r["task"], f"/work/{r['file']}"]))
    path = pathlib.Path(args.cases) / "manifest.tsv"
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{len(lines)} tasks in {path}")


def compare(args):
    by_task = collections.defaultdict(dict)
    times = {}
    for line in open(pathlib.Path(args.cases) / "results.tsv", encoding="utf-8"):
        tid, reasoner, task, status, ms, detail = (line.rstrip("\n").split("\t") + [""])[:6]
        answer = detail if status == "classified" else status
        by_task[tid][reasoner] = (status, answer)
        times[(tid, reasoner)] = int(ms)
    tally = collections.Counter()
    solved = collections.Counter()
    out = []
    for tid, answers in sorted(by_task.items()):
        decided = {r: a for r, (s, a) in answers.items()
                   if s in ("classified", "consistent", "inconsistent")}
        for r in decided:
            solved[r] += 1
        distinct = set(decided.values())
        if len(distinct) > 1:
            verdict = "DISAGREE"
            groups = collections.defaultdict(list)
            for r, a in decided.items():
                groups[a[:12]].append(r)
            print("DISAGREE", tid, dict(groups), sep="\t")
        elif distinct:
            verdict = f"agree({len(decided)})"
        else:
            verdict = "undecided"
        tally[verdict.split("(")[0]] += 1
        for r, (s, a) in sorted(answers.items()):
            out.append([tid, r, s, times[(tid, r)], a[:16], verdict])
    print(f"{len(by_task)} tasks: " + ", ".join(f"{k} {v}" for k, v in sorted(tally.items())))
    print("solved: " + ", ".join(f"{k} {v}" for k, v in sorted(solved.items())))
    if args.out:
        with open(args.out, "w", newline="", encoding="utf-8") as f:
            w = csv.writer(f, delimiter="\t", lineterminator="\n")
            w.writerow(["task", "reasoner", "status", "ms", "answer", "verdict"])
            w.writerows(out)
    return 1 if tally["DISAGREE"] else 0


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    s = sub.add_parser("subset")
    s.add_argument("zip")
    s.add_argument("cases")
    s.add_argument("--track", default="dl/classification")
    s.add_argument("--count", type=int, default=40)
    s.add_argument("--max-bytes", type=int, default=5_000_000)
    m = sub.add_parser("manifest")
    m.add_argument("cases")
    m.add_argument("reasoners", nargs="+")
    c = sub.add_parser("compare")
    c.add_argument("cases")
    c.add_argument("--out")
    args = p.parse_args()
    if args.command == "subset":
        subset(args)
    elif args.command == "manifest":
        manifest(args)
    else:
        sys.exit(compare(args))


if __name__ == "__main__":
    main()
