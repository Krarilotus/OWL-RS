#!/usr/bin/env python3
"""The ORE 2015 development subset on the reference reasoners (docs/design/owl2-dl.md §11).

    python ore.py subset ZIP CASES [--track dl/classification] [--count 40] [--max-bytes 5000000]
    python ore.py strata ZIP CASES [--seed 1] [--holdout 0.2] [--max-bytes 20000000]
    python ore.py manifest CASES REASONER... [--split dev|holdout|all] [--shuffle-seed N]
    python reference.py run CASES --timeout 300
    python ore.py compare CASES [--out results/ore-dev-DATE.tsv]
    python ore.py stats CASES [--limit-s 300] [--by stratum] [--split dev]

- The corpus: Zenodo record 18578 (`ore2015_sample.zip`, MD5
  109f04cf8f124eb551d33c100e549730, CC BY-NC-ND 4.0: used here, never committed or
  redistributed), under `.cache/ore2015/`.
- The subset: from a track's `fileorder.txt` (the competition's order), the ontologies up
  to `--max-bytes`, sorted by size, every k-th, so that it spans the sizes; deterministic.
- The stratified development set (`strata`, DL lab 3.0; docs/design/owl2-dl-performance.md
  §5): per track (DL, EL and pure DL classification, DL consistency, DL realisation), the
  ontologies up to `--max-bytes`, in strata of size (logical axioms: under 1k, 1k-10k,
  10k-100k, more) and expressivity family (nominals, numbers, disjunctive, chains, inverse,
  horn: the first of these whose constructors the ontology uses, from the competition's
  `metadata.csv`). Each stratum contributes in proportion to its size (at least one), drawn
  with `--seed`; a seeded `--holdout` share of each stratum is the blind holdout, never
  looked at while tuning (`split` in index.tsv).
- `manifest --shuffle-seed N` runs the ontologies with their axioms in a shuffled order
  (copies under `shuffled-N/`): an engine whose time depends on the order shows it.
- The comparison: per ontology the canonical taxonomies' hashes (classification) or the
  answers (consistency). Reference reasoners that disagree are listed: such a case is
  settled by hand and recorded in `disputed.tsv`, never by a vote.
- `stats`: per reasoner the solved count, PAR-2 (an unsolved task counts twice the limit),
  the median, p90, p95 and p99 over all tasks (unsolved ones at the limit), and the sum
  over the solved; `--by` stratum, track, family or size gives them per group.
"""

import argparse
import collections
import csv
import io
import pathlib
import random
import re
import sys
import zipfile

TASKS = {"classification": "classify", "consistency": "consistency", "instantiation": "realise"}
EL_ONLY = {"elk"}
# The development set: tasks per track.
DEV_TRACKS = {"dl/classification": 100, "el/classification": 60, "pure_dl/classification": 60,
              "dl/consistency": 50, "dl/instantiation": 50}
FAMILIES = [("nominals", {"O"}), ("numbers", {"N", "Q", "F"}), ("disjunctive", {"C", "U"}),
            ("chains", {"R"}), ("inverse", {"I"})]
SOLVED = ("classified", "consistent", "inconsistent", "realised")
AXIOM = re.compile(r"^[A-Z][A-Za-z]*\(")


def family(constructs: str) -> str:
    tokens = set(constructs.split())
    return next((name for name, marks in FAMILIES if tokens & marks), "horn")


def size_class(axioms: int) -> str:
    return "xs" if axioms < 1_000 else "s" if axioms < 10_000 else "m" if axioms < 100_000 else "l"


def allocate(sizes: dict, total: int) -> dict:
    """Largest-remainder allocation of `total` over strata in proportion to their sizes,
    at least one per non-empty stratum."""
    n = sum(sizes.values())
    if n <= total:
        return dict(sizes)
    quota = {k: max(1.0, v * total / n) for k, v in sizes.items()}
    out = {k: min(sizes[k], int(q)) for k, q in quota.items()}
    order = sorted(sizes, key=lambda k: quota[k] - int(quota[k]), reverse=True)
    i = 0
    while sum(out.values()) < total and i < 10 * len(order):
        k = order[i % len(order)]
        if out[k] < sizes[k]:
            out[k] += 1
        i += 1
    return out


def strata(args):
    z = zipfile.ZipFile(args.zip)
    cases = pathlib.Path(args.cases)
    (cases / "files").mkdir(parents=True, exist_ok=True)
    sizes = {i.filename.rsplit("/", 1)[-1]: i.file_size for i in z.infolist()
             if i.filename.startswith("pool_sample/files/")}
    rows = []
    for track, count in DEV_TRACKS.items():
        meta = list(csv.DictReader(io.StringIO(z.read(f"pool_sample/{track}/metadata.csv").decode())))
        groups = collections.defaultdict(list)
        for m in meta:
            f = m["ore2015_filename"]
            if f not in sizes or sizes[f] > args.max_bytes:
                continue
            axioms = int(m["logical_axiom_count_incl"] or 0)
            key = f"{size_class(axioms)}-{family(m['constructs_incl'])}"
            groups[key].append((f, axioms, m["expressivity_incl"]))
        quota = allocate({k: len(v) for k, v in groups.items()}, count)
        rng = random.Random(f"{args.seed}:{track}")
        for key in sorted(groups):
            members = sorted(groups[key])
            rng.shuffle(members)
            chosen = members[: quota[key]]
            holdout = round(len(chosen) * args.holdout)
            for i, (f, axioms, expressivity) in enumerate(chosen):
                target = cases / "files" / f
                if not target.exists():
                    target.write_bytes(z.read(f"pool_sample/files/{f}"))
                profile, task = track.split("/")
                rows.append([f.removesuffix(".owl"), track, profile, TASKS[task], f"files/{f}", sizes[f], key,
                             "holdout" if i < holdout else "dev", axioms, expressivity])
    with open(cases / "index.tsv", "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh, delimiter="\t", lineterminator="\n")
        w.writerow(["id", "track", "profile", "task", "file", "bytes", "stratum", "split", "axioms", "expressivity"])
        w.writerows(rows)
    split = collections.Counter(r[7] for r in rows)
    print(f"{len(rows)} tasks ({split['dev']} dev, {split['holdout']} holdout) in "
          f"{len({(r[1], r[6]) for r in rows})} track strata, up to {args.max_bytes} bytes, seed {args.seed}")


def is_axiom(line: str) -> bool:
    return bool(AXIOM.match(line)) and not line.startswith(("Import(", "Annotation("))


def shuffled(source: pathlib.Path, target: pathlib.Path, seed: int):
    """A functional-syntax ontology with its axiom lines in a seeded random order (prefixes,
    the ontology header, imports and annotations on the ontology stay first)."""
    lines = source.read_text(encoding="utf-8", errors="replace").splitlines()
    starts = [i for i, l in enumerate(lines) if l.startswith("Ontology(")]
    ends = [i for i, l in enumerate(lines) if l.strip() == ")"]
    target.parent.mkdir(parents=True, exist_ok=True)
    if not starts or not ends:
        target.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return
    start, end = starts[0], ends[-1]
    body = lines[start + 1:end]
    axioms = [l for l in body if is_axiom(l)]
    rest = [l for l in body if not is_axiom(l)]
    random.Random(seed).shuffle(axioms)
    target.write_text("\n".join(lines[: start + 1] + rest + axioms + lines[end:]) + "\n", encoding="utf-8")


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
    cases = pathlib.Path(args.cases)
    for r in read_index(args.cases):
        if args.split != "all" and r.get("split", "dev") != args.split:
            continue
        file, suffix = r["file"], ""
        if args.shuffle_seed is not None:
            file = f"shuffled-{args.shuffle_seed}/{r['file']}"
            if not (cases / file).exists():
                shuffled(cases / r["file"], cases / file, args.shuffle_seed)
            suffix = f"~s{args.shuffle_seed}"
        for reasoner in args.reasoners:
            if reasoner in EL_ONLY and r["profile"] != "el":
                continue
            tid = f"{r['id']}-{r['track'].replace('/', '-')}{suffix}"
            lines.append("\t".join([tid, reasoner, r["task"], f"/work/{file}"]))
    path = pathlib.Path(args.cases) / "manifest.tsv"
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"{len(lines)} tasks in {path}")


def compare(args):
    by_task = collections.defaultdict(dict)
    times = {}
    for line in open(pathlib.Path(args.cases) / "results.tsv", encoding="utf-8"):
        tid, reasoner, task, status, ms, detail = (line.rstrip("\n").split("\t") + [""])[:6]
        answer = detail if status in ("classified", "realised") else status
        by_task[tid][reasoner] = (status, answer)
        times[(tid, reasoner)] = int(ms)
    tally = collections.Counter()
    solved = collections.Counter()
    out = []
    for tid, answers in sorted(by_task.items()):
        decided = {r: a for r, (s, a) in answers.items() if s in SOLVED}
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


def percentile(values, q):
    values = sorted(values)
    if not values:
        return float("nan")
    k = (len(values) - 1) * q
    lo, hi = int(k), min(int(k) + 1, len(values) - 1)
    return values[lo] + (values[hi] - values[lo]) * (k - lo)


def stats(args):
    """Per reasoner (and group): solved, PAR-2, median/p90/p95/p99 with unsolved at the limit."""
    index = {f"{r['id']}-{r['track'].replace('/', '-')}": r for r in read_index(args.cases)}
    limit = args.limit_s * 1000
    groups = collections.defaultdict(list)  # (group, reasoner) -> [(solved, ms)]
    for line in open(pathlib.Path(args.cases) / "results.tsv", encoding="utf-8"):
        tid, reasoner, _task, status, ms = (line.rstrip("\n").split("\t") + [""] * 5)[:5]
        meta = index.get(tid.split("~")[0], {})
        if args.split != "all" and meta.get("split", "dev") != args.split:
            continue
        stratum = meta.get("stratum", "-")
        group = {"stratum": f"{meta.get('track', '-')} {stratum}", "track": meta.get("track", "-"),
                 "family": stratum.split("-", 1)[-1], "size": stratum.split("-", 1)[0]}.get(args.by, "all")
        solved = status in SOLVED
        groups[(group, reasoner)].append((solved, min(float(ms or limit), limit) if solved else limit))
    print("| group | reasoner | solved | PAR-2 s | median ms | p90 | p95 | p99 | sum solved s |")
    print("|---|---|---|---|---|---|---|---|---|")
    for (group, reasoner), rs in sorted(groups.items()):
        times = [ms for _, ms in rs]
        solved = [ms for s, ms in rs if s]
        par2 = sum(ms if s else 2 * limit for s, ms in rs) / 1000
        print(f"| {group} | {reasoner} | {len(solved)}/{len(rs)} | {par2:,.1f} | {percentile(times, .5):,.0f} | "
              f"{percentile(times, .9):,.0f} | {percentile(times, .95):,.0f} | {percentile(times, .99):,.0f} | "
              f"{sum(solved) / 1000:,.1f} |")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="command", required=True)
    s = sub.add_parser("subset")
    s.add_argument("zip")
    s.add_argument("cases")
    s.add_argument("--track", default="dl/classification")
    s.add_argument("--count", type=int, default=40)
    s.add_argument("--max-bytes", type=int, default=5_000_000)
    st = sub.add_parser("strata")
    st.add_argument("zip")
    st.add_argument("cases")
    st.add_argument("--seed", type=int, default=1)
    st.add_argument("--holdout", type=float, default=0.2)
    st.add_argument("--max-bytes", type=int, default=20_000_000)
    m = sub.add_parser("manifest")
    m.add_argument("cases")
    m.add_argument("reasoners", nargs="+")
    m.add_argument("--split", choices=["dev", "holdout", "all"], default="all")
    m.add_argument("--shuffle-seed", type=int)
    sa = sub.add_parser("stats")
    sa.add_argument("cases")
    sa.add_argument("--limit-s", type=int, default=300)
    sa.add_argument("--by", choices=["all", "stratum", "track", "family", "size"], default="all")
    sa.add_argument("--split", choices=["dev", "holdout", "all"], default="dev")
    c = sub.add_parser("compare")
    c.add_argument("cases")
    c.add_argument("--out")
    args = p.parse_args()
    if args.command == "subset":
        subset(args)
    elif args.command == "strata":
        strata(args)
    elif args.command == "manifest":
        manifest(args)
    elif args.command == "stats":
        stats(args)
    else:
        sys.exit(compare(args))


if __name__ == "__main__":
    main()
