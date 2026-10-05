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

SUITE_SYSTEMS = {"qlever", "oxigraph", "virtuoso", "jena", "rdf4j", "nemo", "owlrl",
                 "graphdb", "rdfox", "anzograph", "stardog"}
DL_SYSTEMS = {"hermit", "openllet", "konclude", "elk"}
LICENSED = {"graphdb", "rdfox", "stardog", "anzograph"}
RESULTS = fast.HERE / "results"


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


def suite_run(case: dict, semantics: str, systems: list[str], runs: int, stamp: str) -> list[dict]:
    """The suite's driver on tier CASE~SEMANTICS for NRESE and `systems`; its rows."""
    tier = f"{case['name']}~{semantics}"
    out = RESULTS / stamp / tier.replace("~", "-")
    cap = max(int(case["cap_gb"]), 8)
    env = {**fast.ENV, "NRESE_TARGET_VOLUME": fast.TARGET_VOLUME, "DOCKER_MEMORY": f"{cap}g",
           "JAVA_HEAP": f"{max(cap - 2, 2)}g", "CARGO_BUILD_JOBS": os.environ.get("CARGO_BUILD_JOBS", "4"),
           "NRESE_MEMORY_CAP_GB": os.environ.get("NRESE_MEMORY_CAP_GB", "8")}
    command = [sys.executable, str(fast.ROOT / "benches/suite/suite.py"), "run",
               "--systems", ",".join(["nrese", *systems]), "--workloads", "fast", "--tier", f"fast={tier}",
               "--runs", str(runs), "--query-runs", "3", "--cache", "off", "--skip-build",
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
    file = fast_data_name(case["data"])
    task = "consistency" if case.get("tool", "") == "tableau" else "classify"
    work = f"fast-dl-{case['name']}"
    lines = "\n".join(f"{case['name']}\t{s}\t{task}\t/work/{file}" for s in systems) + "\n"
    manifest_path = f"/work/{work}.manifest.tsv"
    script = (f"printf '%s' '{lines}' > {manifest_path} && rm -f /work/{work}.results.tsv && "
              f"mkdir -p /work/{work}-tax")
    fast.docker(["run", "--rm", "-v", f"{fast.DATA_VOLUME}:/work", "alpine", "sh", "-c", script])
    started = time.monotonic()
    done = fast.docker(["run", "--rm", "--name", f"{work}-{fast.ROOT.name}", f"--memory={case['cap_gb']}g",
                        f"--memory-swap={case['cap_gb']}g", "-v", f"{fast.DATA_VOLUME}:/work",
                        "nrese-bench/dl-reference", "batch", manifest_path, f"/work/{work}.results.tsv",
                        f"/work/{work}-tax", "300"], timeout=1800)
    wall = time.monotonic() - started
    out = fast.docker(["run", "--rm", "-v", f"{fast.DATA_VOLUME}:/work", "alpine", "sh", "-c",
                       f"cat /work/{work}.results.tsv; for f in /work/{work}-tax/*.tax; do "
                       f"echo \"TAX $(basename $f) $(sha256sum < $f | cut -c1-16)\"; done"])
    results = {}
    for line in out.stdout.splitlines():
        parts = line.split("\t")
        if line.startswith("TAX "):
            _, name, digest = line.split()
            reasoner = name.split(".")[-2]
            results.setdefault(reasoner, {})["taxonomy"] = digest
        elif len(parts) >= 5:
            results.setdefault(parts[1], {}).update({"status": parts[3], "ms": float(parts[4]),
                                                     "detail": parts[5] if len(parts) > 5 else ""})
    if done.returncode not in (0, 3):
        print(f"  the DL runner exited {done.returncode}: {done.stderr.strip()[-200:]}")
    return {"results": results, "wall_s": round(wall, 1), "nrese_ms": nrese_ms}


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


def compete(args) -> int:
    cases = fast.select(fast.load_cases(), args)
    only = set(args.systems.split(",")) if args.systems else None
    stamp = datetime.datetime.now().strftime("%Y-%m-%d-%H%M")
    report = {"started": stamp, "git": fast.manifest.git(fast.ROOT), "cases": {}}
    licensed = getattr(args, "licensed", False)
    for case in cases:
        entries = comparators(case, only)
        if not entries:
            continue
        print(f"\n{case['name']} ({case['semantics']})", flush=True)
        fast.ensure_data(case["data"])
        result = {"semantics": case["semantics"], "comparisons": {}, "no_runner": []}
        by_semantics: dict[str, list[str]] = {}
        dl: list[str] = []
        for system, semantics in entries:
            if system in LICENSED and not licensed:
                result["no_runner"].append(f"{system}:{semantics} (licensed; --licensed)")
            elif system in DL_SYSTEMS and semantics in ("dl", "el"):
                dl.append(system)
            elif system == "nemo" and semantics == "custom":
                result["comparisons"]["nemo:custom"] = nemo_custom(case)
            elif system in SUITE_SYSTEMS and semantics in FAST_REGIMES:
                by_semantics.setdefault(semantics, []).append(system)
            else:
                result["no_runner"].append(f"{system}:{semantics}")
        if dl:
            metric, record = nrese_metric(case)
            dl_result = dl_run(case, dl, metric, stamp)
            dl_result["nrese_answer"] = record.get("result", {}).get("answer")
            dl_result["nrese_hash"] = record.get("result", {}).get("hash")
            result["comparisons"]["dl"] = dl_result
        if "nemo:custom" in result["comparisons"]:
            metric, record = nrese_metric(case)
            result["comparisons"]["nemo:custom"]["nrese_reason_ms"] = record.get("metric")
            result["comparisons"]["nemo:custom"]["nrese_pairs"] = \
                next((c["got"] for c in record.get("checks", []) if c["path"].startswith("q.paths")), None)
        if any(s in ("plain", "closure") or s in FAST_REGIMES for s in by_semantics):
            copy_volume_files(case)
        for semantics, systems in by_semantics.items():
            if semantics == "closure" and export_closure(case) is None:
                result["no_runner"].append("closure export failed")
                continue
            rows = suite_run(case, semantics, systems, args.runs, stamp)
            summary = summarise_suite(rows)
            result["comparisons"][semantics] = {"summary": summary, "verdicts": verdicts(summary)}
        report["cases"][case["name"]] = result
        print_case(case["name"], result)
        RESULTS.mkdir(parents=True, exist_ok=True)
        (RESULTS / stamp).mkdir(parents=True, exist_ok=True)
        (RESULTS / stamp / "compete.json").write_text(json.dumps(report, indent=1), encoding="utf-8")
    print(f"\nresults: {(RESULTS / stamp / 'compete.json').relative_to(fast.ROOT)}")
    return 0


def print_case(name: str, result: dict):
    for semantics, comparison in result["comparisons"].items():
        if semantics == "dl":
            r = comparison
            print(f"  DL: NRESE {r['nrese_ms']} ms (answer {r.get('nrese_answer')}, hash {r.get('nrese_hash')})")
            for reasoner, v in r["results"].items():
                print(f"    {reasoner:<9} {v.get('status', '-'):<13} {v.get('ms', '-')} ms  {v.get('taxonomy', '')}")
            continue
        if semantics == "nemo:custom":
            print(f"  nemo (custom rules): {comparison}")
            continue
        for system, v in comparison["verdicts"].items():
            restricted = " (restricted: stays local)" if v["publish"] != "free" else ""
            print(f"  {semantics:<10} {system:<9} {v['status']:<8} answers {v['answers']:<10} "
                  f"queries {v['their_query_ms']} vs NRESE {v['nrese_query_ms']} ms → {v['query_winner']}; "
                  f"load {v['load_ms']} vs {v['nrese_load_ms']} ms → {v.get('load_winner')}{restricted}")
    for missing in result["no_runner"]:
        print(f"  no runner: {missing}")
