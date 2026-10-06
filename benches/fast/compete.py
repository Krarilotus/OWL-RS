"""`fast.py compete`: each case on NRESE and on the comparators it declares, each under
the semantics it names (cases.toml `systems`), so that a case added once runs for NRESE
and for whoever supports it.

Who runs what:
- SPARQL stores and rule reasoners (QLever, Oxigraph, Virtuoso, Jena, RDF4J, Nemo's
  OWL 2 RL encoding, owlrl, and the licensed ones) go through the suite's driver
  (benches/suite, workload `fast`, tier `CASE~SEMANTICS`), beside an NRESE server on the
  same data and semantics: loads, statement counts and the case's queries, cache off.
  `closure` loads the closure NRESE exported (perf_lab --export); `plain` the data as it
  is.
- The DL reasoners (HermiT, Openllet, Konclude, ELK) run in the DL kit's image
  (benches/reasoning/dl): consistency for `tableau` cases, classification for the
  `classify-*` ones, taxonomies compared by their canonical hash.
- Nemo under `custom` runs the case's Datalog program (rules/*.rls).
- Licensed systems run only with `--licensed` and their licence files; their results stay
  in benches/fast/results/ (git-ignored) and are printed as restricted.

Answer counts must match NRESE's before a time counts: a differing count is reported as
`differs`, never as a time. Results: benches/fast/results/<stamp>/compete.json.
"""
from __future__ import annotations

import csv
import datetime
import hashlib
import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

import fast
from suitekit.workloads import FAST_REGIMES, fast_data_name

DL_TIMEOUT_S = 120  # per reference reasoner and task in the comparisons

SUITE_SYSTEMS = {"qlever", "oxigraph", "virtuoso", "jena", "rdf4j", "nemo", "owlrl",
                 "graphdb", "rdfox", "anzograph", "stardog"}
DL_SYSTEMS = {"hermit", "openllet", "konclude", "elk"}
LICENSED = {"graphdb", "rdfox", "stardog", "anzograph"}
RESULTS = fast.HERE / "results"


# How far the stores without reasoning can be given a reasoning case's queries as SPARQL
# (benches/PROTOCOL.md §3): RDFS, OWL 2 QL and property axioms between named individuals
# (sub-properties, inverse, symmetric, transitive, regular chains) as property paths; RL
# and EL only per query (paths are NLogSpace, RL and EL PTime-complete); sameAs and DL not.
REWRITE_BY_SEMANTICS = {"rdfs": "expressible", "rdfs-full": "expressible", "owl2-ql": "expressible",
                        "rdfs-plus": "per-query", "owl-horst": "per-query", "owl2-rl": "per-query",
                        "custom": "per-query", "el": "not-expressible", "dl": "not-expressible"}


def rewrite_class(case: dict) -> str:
    """`expressible`, `per-query` or `not-expressible`: the case's own `rewrite`, else its
    semantics'."""
    return case.get("rewrite") or REWRITE_BY_SEMANTICS.get(case["semantics"], "expressible")


def unsupported_mode(case: dict) -> str | None:
    """What a case measures that the suite's cycle (load, count, queries, clients) doesn't."""
    args = case.get("args", [])
    for flag, what in (("--commits", "a commit series"), ("--shapes", "SHACL validation"),
                       ("--canonicalize", "canonicalisation")):
        if flag in args:
            return what
    return None


def comparators(case: dict, only: set[str] | None) -> list[tuple[str, str]]:
    out = []
    for entry in case.get("systems", []):
        system, _, semantics = entry.partition(":")
        if only and system not in only:
            continue
        out.append((system, semantics))
    return out


def copy_volume_files(case: dict):
    """Files of the dataset volume a case reads, copied into the fast volume (the suite
    mounts one volume)."""
    for spec in [case["data"], *case.get("extra", [])]:
        if spec.startswith("volume:"):
            name = fast_data_name(spec)
            fast.docker(["run", "--rm", "-v", f"{fast.BENCH_VOLUME}:/data:ro", "-v", f"{fast.DATA_VOLUME}:/fast",
                         "alpine", "sh", "-c", f"test -s /fast/{name} || cp /data/{name} /fast/{name}"])


def export_closure(case: dict) -> str | None:
    """NRESE's closure of the case's data (asserted and inferred), for the stores without
    reasoning; None if it fails."""
    target = f"/fast/{case['name']}.closure.nt"
    found = fast.docker(["run", "--rm", "-v", f"{fast.DATA_VOLUME}:/fast", "alpine", "test", "-s", target])
    if found.returncode == 0:
        return target
    loads = [x for f in [case["data"], *case.get("extra", [])] for x in ("--load", fast.data_file(f))]
    reason = ["--reason", case["semantics"]] if case["semantics"] not in ("none",) else []
    code, _, err, _ = fast.container(f"fast-export-{fast.ROOT.name}", max(case["cap_gb"], 8),
                                     ["/target/release/examples/perf_lab", *loads, *reason, "--export", target])
    if code != 0:
        print(f"  export of {case['name']}'s closure failed: {err.strip()[-300:]}")
        return None
    return target


def suite_run(case: dict, semantics: str, systems: list[str], runs: int, stamp: str,
              queries: Path | None = None, track: str = "", settings: dict | None = None) -> list[dict]:
    """The suite's driver on tier CASE~SEMANTICS for NRESE and `systems`; its rows. A track
    (the case's `tracks`) adds its settings, such as how many queries a store answers at
    once."""
    tier = f"{case['name']}~{semantics}"
    out = RESULTS / stamp / (tier.replace("~", "-") + (f"-{track}" if track else ""))
    cap = max(int(case["cap_gb"]), 8)
    env = {**fast.ENV, "NRESE_TARGET_VOLUME": fast.TARGET_VOLUME, "DOCKER_MEMORY": f"{cap}g",
           "JAVA_HEAP": f"{max(cap - 2, 2)}g", "CARGO_BUILD_JOBS": os.environ.get("CARGO_BUILD_JOBS", "4"),
           "NRESE_MEMORY_CAP_GB": os.environ.get("NRESE_MEMORY_CAP_GB", "8"),
           "CARGO_TARGET_DIR": str(fast.ROOT / "target")}
    if queries:
        env["FAST_QUERIES"] = str(queries)
    env.update(settings or {})
    command = [sys.executable, str(fast.ROOT / "benches/suite/suite.py"), "run",
               "--systems", ",".join(["nrese", *systems]), "--workloads", "fast", "--tier", f"fast={tier}",
               "--runs", str(runs), "--query-runs", "2", "--cache", "off", "--skip-build",
               "--data", f"volume:{fast.DATA_VOLUME}", "--results", str(out), "--query-timeout-s", "120",
               "--timeout-s", "900"]
    print(f"  suite: {tier} on nrese, {', '.join(systems)}", flush=True)
    log = out.with_suffix(".log")
    out.parent.mkdir(parents=True, exist_ok=True)
    with open(log, "w", encoding="utf-8") as f:
        subprocess.run(command, stdout=f, stderr=subprocess.STDOUT, env=env, cwd=fast.ROOT)
    path = out / "results.csv"
    if not path.exists():
        print(f"  no results; see {log}")
        return []
    return list(csv.DictReader(open(path, encoding="utf-8")))


def summarise_suite(rows: list[dict]) -> dict[str, dict]:
    """Per system: load (with reasoning), statements, per-query median ms and rows, status."""
    out: dict[str, dict] = {}
    for r in rows:
        s = out.setdefault(r["system"], {"queries": {}, "status": "ok", "notes": [], "publish": r["publish"]})
        if r["status"] not in ("ok", ""):
            s["status"] = r["status"] if s["status"] == "ok" else s["status"]
            if r.get("note"):
                s["notes"].append(f"{r['task']} {r['item']}: {r['note'][:120]}")
        if r["task"] == "load" and r["ms"]:
            s["load_ms"] = float(r["ms"])
            s["load_peak_mib"] = float(r["peak_mib"]) if r["peak_mib"] else None
        if r["task"] == "reason" and r["ms"]:
            s["reason_ms"] = float(r["ms"])
        if r["task"] == "count" and r["rows"]:
            s["statements"] = int(r["rows"])
        if r["task"] == "query" and r["item"].startswith("clients-") and r["ms"]:
            s.setdefault("clients", {})[r["item"]] = {"p99_ms": float(r["ms"]), "completed": r["rows"],
                                                       "note": r.get("note", "")}
            continue
        if r["task"] == "query" and r["status"] in ("ok", "wrong") and r["repeat"] not in ("0", "") and r["ms"]:
            q = s["queries"].setdefault(r["item"], {"ms": [], "rows": r["rows"]})
            q["ms"].append(float(r["ms"]))
    for s in out.values():
        for q in s["queries"].values():
            q["median_ms"] = statistics.median(q["ms"])
            del q["ms"]
    return out


def verdicts(summary: dict[str, dict]) -> dict[str, dict]:
    """Per system against NRESE: answers match?, query sum ratio, load ratio."""
    nrese = summary.get("nrese")
    out = {}
    for system, s in summary.items():
        if system == "nrese" or nrese is None:
            continue
        shared = [q for q in s["queries"] if q in nrese["queries"]]
        differ = [q for q in shared if str(s["queries"][q]["rows"]) != str(nrese["queries"][q]["rows"])]
        same = [q for q in shared if q not in differ]
        theirs = sum(s["queries"][q]["median_ms"] for q in same)
        ours = sum(nrese["queries"][q]["median_ms"] for q in same)
        v = {"status": s["status"], "answers": "differ: " + ",".join(differ) if differ else "match",
             "queries_compared": len(same), "their_query_ms": round(theirs, 2), "nrese_query_ms": round(ours, 2),
             "query_winner": None, "load_ms": s.get("load_ms"), "nrese_load_ms": nrese.get("load_ms"),
             "statements": s.get("statements"), "nrese_statements": nrese.get("statements"),
             "publish": s["publish"], "notes": s["notes"][:3]}
        if same:
            v["query_winner"] = "nrese" if ours < theirs else system
        if v["load_ms"] and v["nrese_load_ms"]:
            v["load_winner"] = "nrese" if v["nrese_load_ms"] < v["load_ms"] else system
        out[system] = v
    return out


def dl_run(case: dict, systems: list[str], nrese_ms: float | None, stamp: str) -> dict:
    """The DL kit's reference reasoners on the case's ontology."""
    # The ontology as the fast suite's volume holds it, mounted at /work here.
    file = fast.data_file(case["data"]).removeprefix("/fast/")
    task = "consistency" if case.get("tool", "") == "tableau" else "classify"
    work = f"fast-dl-{case['name']}"
    lines = "\n".join(f"{case['name']}\t{s}\t{task}\t/work/{file}" for s in systems) + "\n"
    manifest_path = f"/work/{work}.manifest.tsv"
    script = (f"printf '%s' '{lines}' > {manifest_path} && rm -f /work/{work}.results.tsv /work/{work}.part.tsv && "
              f"mkdir -p /work/{work}-tax")
    fast.docker(["run", "--rm", "-v", f"{fast.DATA_VOLUME}:/work", "alpine", "sh", "-c", script])
    started = time.monotonic()
    # One JVM per batch; after a timeout the runner exits (3) for a fresh one, so the rest
    # of the manifest runs again until every reasoner has a result (as reference.py does).
    timeout_s = DL_TIMEOUT_S
    for _ in systems:
        rest = (f"cut -f1,2 /work/{work}.results.tsv 2>/dev/null > /tmp/done; "
                f"grep -v -F -f /tmp/done {manifest_path} > /work/{work}.rest.tsv || true; "
                f"test -s /tmp/done || cp {manifest_path} /work/{work}.rest.tsv; wc -l < /work/{work}.rest.tsv")
        left = fast.docker(["run", "--rm", "-v", f"{fast.DATA_VOLUME}:/work", "alpine", "sh", "-c", rest])
        if left.stdout.strip() in ("", "0"):
            break
        done = fast.docker(["run", "--rm", "--name", f"{work}-{fast.ROOT.name}", f"--memory={case['cap_gb']}g",
                            f"--memory-swap={case['cap_gb']}g", "-v", f"{fast.DATA_VOLUME}:/work",
                            "nrese-bench/dl-reference", "batch", f"/work/{work}.rest.tsv", f"/work/{work}.part.tsv",
                            f"/work/{work}-tax", str(timeout_s)], timeout=timeout_s * len(systems) + 300)
        # Results so far, and a timeout for the task the runner stopped at.
        merge = (f"cat /work/{work}.part.tsv >> /work/{work}.results.tsv 2>/dev/null; rm -f /work/{work}.part.tsv; "
                 f"cut -f1,2 /work/{work}.results.tsv > /tmp/done; "
                 f"next=$(grep -v -F -f /tmp/done /work/{work}.rest.tsv | head -1); "
                 f"if [ {done.returncode} -eq 3 ] && [ -n \"$next\" ]; then "
                 f"echo \"$next\" | awk -F'\\t' -v OFS='\\t' '{{print $1,$2,$3,\"timeout\",{timeout_s * 1000},\"\"}}' "
                 f">> /work/{work}.results.tsv; fi")
        fast.docker(["run", "--rm", "-v", f"{fast.DATA_VOLUME}:/work", "alpine", "sh", "-c", merge])
    wall = time.monotonic() - started
    out = fast.docker(["run", "--rm", "-v", f"{fast.DATA_VOLUME}:/work", "alpine", "sh", "-c",
                       f"cat /work/{work}.results.tsv; for f in /work/{work}-tax/*.tax; do [ -e \"$f\" ] || continue; "
                       f"echo \"TAX $(basename $f) $(sha256sum < $f | cut -c1-16)\"; done"])
    results = {}
    for line in out.stdout.splitlines():
        parts = line.split("\t")
        if line.startswith("TAX "):
            fields = line.split()
            if len(fields) != 3:
                continue
            _, name, digest = fields
            reasoner = name.split(".")[-2]
            results.setdefault(reasoner, {})["taxonomy"] = digest
        elif len(parts) >= 5:
            results.setdefault(parts[1], {}).update({"status": parts[3], "ms": float(parts[4]),
                                                     "detail": parts[5] if len(parts) > 5 else ""})
    out = {"results": results, "wall_s": round(wall, 1), "nrese_ms": nrese_ms}
    if task == "classify":
        out["nrese_taxonomy"] = nrese_taxonomy(case, work, results)
        # A reference taxonomy for the fast run's check (`reference` in cases.toml): ELK's
        # for EL cases, else the first reasoner's that classified.
        keep = next((r for r in ("elk", "konclude", "hermit", "openllet") if results.get(r, {}).get("status") == "classified"), None)
        if keep and case.get("reference"):
            fast.docker(["run", "--rm", "-v", f"{fast.DATA_VOLUME}:/work", "alpine", "sh", "-c",
                         f"mkdir -p /work/ref && cp /work/{work}-tax/{case['name']}.{keep}.tax "
                         f"/work/{case['reference'].removeprefix('/fast/')}"])
    return out


def nrese_taxonomy(case: dict, work: str, results: dict) -> str | None:
    """NRESE's closure (the fast run's `--out`) as the canonical taxonomy over a reference
    run's signature (benches/reasoning/dl/canonical.py): its SHA-256, which equals a
    reference's when the taxonomies are the same."""
    sys.path.insert(0, str(fast.ROOT / "benches" / "reasoning" / "dl"))
    import canonical
    closure = fast.SCRATCH / f"{case['name']}.tsv"
    reference = next((r for r, v in results.items() if v.get("status") == "classified"), None)
    if reference is None or not closure.exists():
        return None
    tax = fast.docker(["run", "--rm", "-v", f"{fast.DATA_VOLUME}:/work", "alpine", "cat",
                       f"/work/{work}-tax/{case['name']}.{reference}.tax"])
    if tax.returncode != 0:
        return None
    signature_file = fast.SCRATCH / f"{case['name']}.{reference}.tax"
    signature_file.write_text(tax.stdout, encoding="utf-8")
    pairs = [tuple(line.rstrip("\n").split("\t")[:2]) for line in open(closure, encoding="utf-8") if "\t" in line]
    text = canonical.canonical(canonical.signature(signature_file), pairs)
    return hashlib.sha256(text.encode()).hexdigest()


def nemo_custom(case: dict) -> dict:
    """Nemo on the case's Datalog program (rules/<name>.rls, the same rules as the N3)."""
    rules = fast.HERE / "rules" / "tc.rls"
    data = fast.data_file(case["data"])
    script = (f"mkdir -p /tmp/in /tmp/nemo && cp {data} /tmp/in/input.nt && "
              "nmo -I /tmp/in -D /tmp/nemo -o --report short /rules/tc.rls 2>&1 && wc -l < /tmp/nemo/path.csv")
    started = time.monotonic()
    done = fast.docker(["run", "--rm", f"--memory={case['cap_gb']}g", f"--memory-swap={case['cap_gb']}g",
                        "-v", f"{fast.DATA_VOLUME}:/fast", "-v", f"{rules.parent.as_posix()}:/rules:ro",
                        "nrese-bench/nemo", "sh", "-c", script], timeout=900)
    wall = time.monotonic() - started
    text = done.stdout
    import re
    reasoning = re.search(r"Reasoning: +(\d+)ms", text)
    pairs = text.strip().splitlines()[-1] if text.strip() else ""
    return {"status": "ok" if done.returncode == 0 else "failed", "reason_ms": int(reasoning.group(1)) if reasoning else None,
            "wall_s": round(wall, 2), "pairs": int(pairs) if pairs.isdigit() else None}


def nrese_metric(case: dict) -> tuple[float | None, dict]:
    """NRESE's own number for the case, from one fast-suite run (perf lab or DL tool)."""
    record = fast.run_case(case, "compete", 1)
    return record.get("metric"), record


def merged_queries(cases: list[dict], key: str) -> Path | None:
    """The queries of `cases` in one directory, each named CASE--QUERY.rq; None if a case
    has none."""
    directory = fast.SCRATCH / "queries" / "merged" / key
    if directory.exists():
        for old in directory.glob("*.rq"):
            old.unlink()
    directory.mkdir(parents=True, exist_ok=True)
    for case in cases:
        name = case.get("queries", "")
        if not name or name.startswith("@"):
            return None
        source = (fast.ROOT / "benches/reasoning/queries" / name if name in ("lubm", "owl2bench")
                  else fast.HERE / "queries" / name)
        for query in sorted(source.glob("*.rq")):
            (directory / f"{case['name']}--{query.name}").write_text(query.read_text(encoding="utf-8"),
                                                                      encoding="utf-8")
    return directory


def split(summary: dict[str, dict], case: str) -> dict[str, dict]:
    """A merged run's summary restricted to one case's queries (renamed back)."""
    out = {}
    for system, s in summary.items():
        queries = {q.split("--", 1)[1]: v for q, v in s["queries"].items() if q.startswith(f"{case}--")}
        out[system] = {**s, "queries": queries}
    return out


def compete(args) -> int:
    cases = fast.select(fast.load_cases(), args)
    only = set(args.systems.split(",")) if args.systems else None
    stamp = datetime.datetime.now().strftime("%Y-%m-%d-%H%M")
    report = {"started": stamp, "git": fast.manifest.git(fast.ROOT), "cases": {}}
    licensed = getattr(args, "licensed", False)
    (RESULTS / stamp).mkdir(parents=True, exist_ok=True)

    def save():
        (RESULTS / stamp / "compete.json").write_text(json.dumps(report, indent=1), encoding="utf-8")

    # Suite jobs, grouped: the cases on the same data and semantics load it once per system.
    groups: dict[tuple, dict] = {}
    for case in cases:
        entries = comparators(case, only)
        if not entries:
            continue
        print(f"\n{case['name']} ({case['semantics']})", flush=True)
        fast.ensure_data(case["data"])
        result = report["cases"].setdefault(case["name"], {"semantics": case["semantics"], "comparisons": {},
                                                            "no_runner": []})
        dl: list[str] = []
        for system, semantics in entries:
            if system in LICENSED and not licensed:
                result["no_runner"].append(f"{system}:{semantics} (licensed; --licensed)")
            elif system in DL_SYSTEMS and semantics in ("dl", "el"):
                dl.append(system)
            elif semantics == "rewritten":
                # The case's queries as NRESE's OWL 2 QL rewriter prints them (headline:
                # property paths; secondary: VALUES lists of the hierarchy NRESE computed),
                # never written by hand; NRESE runs the path form with reasoning off too.
                verdict = rewrite_class(case)
                if verdict == "not-expressible":
                    result["comparisons"][f"{system}:rewritten"] = {
                        "status": "not expressible",
                        "why": "needs sameAs, DL or recursive RL/EL rules: reasoning in the store wins here"}
                else:
                    result["no_runner"].append(
                        f"{system}:rewritten ({verdict}; waits for the OWL 2 QL rewriter's SPARQL printer, ql/tree-witness)")
            elif system == "nemo" and semantics == "custom":
                result["comparisons"]["nemo:custom"] = nemo_custom(case)
            elif system in SUITE_SYSTEMS and semantics in FAST_REGIMES and unsupported_mode(case):
                result["no_runner"].append(f"{system}:{semantics} ({unsupported_mode(case)} has no suite runner yet)")
            elif system in SUITE_SYSTEMS and semantics in FAST_REGIMES:
                clients = "--clients" in case.get("args", [])
                data = (case["name"],) if semantics == "closure" else (case["data"], *case.get("extra", []))
                group = groups.setdefault((data, semantics, clients), {"cases": [], "systems": set()})
                if case not in group["cases"]:
                    group["cases"].append(case)
                group["systems"].add(system)
            else:
                result["no_runner"].append(f"{system}:{semantics}")
        if dl:
            metric, record = nrese_metric(case)
            dl_result = dl_run(case, dl, metric, stamp)
            dl_result["nrese_answer"] = record.get("result", {}).get("answer")
            dl_result["nrese_hash"] = record.get("result", {}).get("hash")
            dl_result["nrese_status"] = record.get("status")
            result["comparisons"]["dl"] = dl_result
        if "nemo:custom" in result["comparisons"]:
            metric, record = nrese_metric(case)
            result["comparisons"]["nemo:custom"]["nrese_reason_ms"] = record.get("metric")
            result["comparisons"]["nemo:custom"]["nrese_pairs"] =                 next((c["got"] for c in record.get("checks", []) if c["path"].startswith("q.paths")), None)
        print_case(case["name"], result)
        save()
    for (data, semantics, clients), group in groups.items():
        first = group["cases"][0]
        copy_volume_files(first)
        if semantics == "closure" and export_closure(first) is None:
            for case in group["cases"]:
                report["cases"][case["name"]]["no_runner"].append("closure export failed")
            continue
        key = f"{first['name']}-{semantics}" + ("-clients" if clients else "")
        queries = merged_queries(group["cases"], key) if len(group["cases"]) > 1 else None
        # A case's tracks (defaults, best configuration) each run with their own settings.
        tracks = first.get("tracks") or {"": {}}
        for track, settings in tracks.items():
            rows = suite_run(first, semantics, sorted(group["systems"]), args.runs, stamp, queries,
                             track, {k: str(v) for k, v in settings.items()})
            summary = summarise_suite(rows)
            label = f"{semantics}@{track}" if track else semantics
            for case in group["cases"]:
                mine = split(summary, case["name"]) if queries else summary
                report["cases"][case["name"]]["comparisons"][label] = {"summary": mine, "verdicts": verdicts(mine)}
                print(f"\n{case['name']} ({label})")
                print_case(case["name"], {"comparisons": {label: report["cases"][case["name"]]["comparisons"][label]},
                                          "no_runner": []})
        save()
    save()
    print(f"\nresults: {(RESULTS / stamp / 'compete.json').relative_to(fast.ROOT)}")
    return 0


def print_case(name: str, result: dict):
    for semantics, comparison in result["comparisons"].items():
        if semantics == "dl":
            r = comparison
            print(f"  DL: NRESE {r['nrese_ms']} ms (answer {r.get('nrese_answer')}, hash {r.get('nrese_hash')})")
            ours = r.get("nrese_taxonomy")
            if ours:
                print(f"    NRESE's taxonomy {ours[:16]}")
            for reasoner, v in r["results"].items():
                same = ""
                if ours and v.get("status") == "classified":
                    same = "taxonomy equal" if v.get("detail", "").startswith(ours) else "TAXONOMY DIFFERS"
                print(f"    {reasoner:<9} {v.get('status', '-'):<13} {v.get('ms', '-')} ms  {same}")
            continue
        if semantics == "nemo:custom":
            print(f"  nemo (custom rules): {comparison}")
            continue
        if semantics.endswith(":rewritten"):
            print(f"  rewritten  {semantics.split(':')[0]:<9} {comparison['status']}: {comparison['why']}")
            continue
        for system, v in comparison["verdicts"].items():
            restricted = " (restricted: stays local)" if v["publish"] != "free" else ""
            print(f"  {semantics:<10} {system:<9} {v['status']:<8} answers {v['answers']:<10} "
                  f"queries {v['their_query_ms']} vs NRESE {v['nrese_query_ms']} ms → {v['query_winner']}; "
                  f"load {v['load_ms']} vs {v['nrese_load_ms']} ms → {v.get('load_winner')}{restricted}")
    for missing in result["no_runner"]:
        print(f"  no runner: {missing}")
