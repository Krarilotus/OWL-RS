#!/usr/bin/env python3
"""Sparqloscope's DBLP benchmark as the suite's query set, its published answers as known
answers, and its published times as the bar (benches/sparql/README.md).

    python3 benches/sparql/sparqloscope/import.py SPARQLOSCOPE-CHECKOUT

SPARQLOSCOPE-CHECKOUT is github.com/ad-freiburg/sparqloscope at the pinned commit (PINNED;
Apache-2.0). It writes, from `benchmarks/dblp.benchmark.tsv` and the six engines' results
of the ISWC 2025 paper (`results/iswc-25/dblp.ENGINE.results.yaml`, plus Oxigraph's in
`results/`):

- benches/competitors/queries/sparqloscope-dblp/NNN-NAME.rq: one query per file, numbered
  in the benchmark's order (the order its authors ran them in), its name and description
  in a comment;
- benches/sparql/sparqloscope/dblp-published.tsv: per query, the published answer (the
  value of a one-row answer, else the row count), how many engines published that same
  answer, and each engine's published time in seconds (`fail` where it failed or timed out);
- benches/sparql/sparqloscope/dblp-checks.toml: the known answers as a case's `check`
  table (an answer every engine that answered published alike, at least two), for benches/cases/sparql.toml.

It prints each engine's geometric mean with failures counted as twice the 180 s timeout,
the figure of the paper's Table 2 (QLever 0.18 s).
"""
from __future__ import annotations

import math
import re
import subprocess
import sys
from pathlib import Path

import yaml

PINNED = "61ad92f"
TIMEOUT_S = 180.0
ENGINES = ["qlever", "virtuoso", "mdb", "graphdb", "blazegraph", "jena", "oxigraph"]
REPO = Path(__file__).resolve().parents[3]
QUERIES = REPO / "benches/competitors/queries/sparqloscope-dblp"
HERE = Path(__file__).resolve().parent


def published(checkout: Path) -> dict[str, dict[str, dict]]:
    """engine -> query name -> its published entry."""
    out = {}
    for engine in ENGINES:
        path = checkout / "results/iswc-25" / f"dblp.{engine}.results.yaml"
        if not path.exists():
            path = checkout / "results" / f"dblp.{engine}.results.yaml"
        out[engine] = {q["query"]: q for q in yaml.safe_load(path.read_text(encoding="utf-8"))["queries"]}
    return out


def answer(entry: dict | None) -> str | None:
    """A published answer: `value=V` for one row and column (a literal's lexical form), else
    `rows=N`; None where the engine failed."""
    if not entry or not isinstance(entry.get("results"), list):
        return None
    rows = entry.get("result_size")
    if rows == 1 and len(entry["results"]) == 1 and len(entry["results"][0]) == 1:
        term = str(entry["results"][0][0])
        m = re.fullmatch(r'"(.*)"(\^\^<[^>]+>)?', term, re.S)
        return f"value={m.group(1) if m else term}"
    return f"rows={rows}"


def seconds(entry: dict | None) -> str:
    if not entry or not isinstance(entry.get("results"), list):
        return "fail"
    return f"{entry['runtime_info']['client_time']:.4f}"


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__)
        return 2
    checkout = Path(argv[1]).resolve()
    head = subprocess.run(["git", "-C", str(checkout), "rev-parse", "--short=7", "HEAD"],
                          capture_output=True, text=True).stdout.strip()
    if head != PINNED:
        sys.exit(f"{checkout} is at {head or '?'}, not the pinned {PINNED}")
    lines = (checkout / "benchmarks/dblp.benchmark.tsv").read_text(encoding="utf-8").splitlines()
    results = published(checkout)
    QUERIES.mkdir(parents=True, exist_ok=True)
    for old in QUERIES.glob("*.rq"):
        old.unlink()
    table = ["# Sparqloscope DBLP (ad-freiburg/sparqloscope " + PINNED + ", ISWC 2025): the answer every engine "
             "that answered published alike (value= one value, rows= a row count; else the distinct answers, "
             "disputed), how many published it, and their times in seconds (fail: failed or timed out at 180 s). "
             "Written by import.py.",
             "\t".join(["query", "answer", "agree", *ENGINES])]
    checks = ["# Known answers of the Sparqloscope DBLP queries: published alike by every engine of the",
              "# ISWC 2025 paper that answered, at least two (dblp-published.tsv). Written by import.py.",
              "[check]"]
    for n, line in enumerate(lines, 1):
        head_text, query = line.split("\t", 1)
        name, _, description = head_text.partition(" [")
        name, description = name.strip(), description.rstrip("]")
        stem = f"{n:03d}-{name}"
        (QUERIES / f"{stem}.rq").write_text(
            f"# Sparqloscope {PINNED}, DBLP, query {n}: {name}\n# {description}\n{query.strip()}\n", encoding="utf-8")
        # No majority and no favourite: an answer is known only where every engine that
        # answered published the same one (QLever's own are wrong on several queries, its
        # MIN and MAX of strings follow a locale's collation). Else the query is disputed
        # until the reference systems settle it on our run (answers.py, adjudications.tsv).
        given = [a for a in (answer(results[e].get(name)) for e in ENGINES) if a is not None]
        reference = given[0] if len(given) >= 2 and len(set(given)) == 1 else None
        shown = reference or ("disputed: " + " | ".join(sorted(set(given))) if given else "-")
        table.append("\t".join([stem, shown[:200], str(len(given) if reference else 0),
                                *(seconds(results[e].get(name)) for e in ENGINES)]))
        if reference:
            kind, _, value = reference.partition("=")
            key = f'"q.{stem}.{"value" if kind == "value" else "rows"}"'
            checks.append(f"{key} = {value if re.fullmatch(r'-?[0-9]+', value) else repr(value)}")
    (HERE / "dblp-published.tsv").write_text("\n".join(table) + "\n", encoding="utf-8")
    (HERE / "dblp-checks.toml").write_text("\n".join(checks) + "\n", encoding="utf-8")
    print(f"{len(lines)} queries into {QUERIES.relative_to(REPO)}")
    for engine in ENGINES:
        times = []
        for name_line in lines:
            name = name_line.split("\t", 1)[0].partition(" [")[0].strip()
            t = seconds(results[engine].get(name))
            times.append(2 * TIMEOUT_S if t == "fail" else max(float(t), 1e-3))
        failed = sum(1 for t in times if t == 2 * TIMEOUT_S)
        gm = math.exp(sum(math.log(t) for t in times) / len(times))
        print(f"{engine:<11} geometric mean {gm:7.3f} s, {failed} failed (penalty 2 x {TIMEOUT_S:.0f} s)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
