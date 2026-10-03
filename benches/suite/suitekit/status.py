"""`suite.py status`: what has been measured and what hasn't, from the registries
(systems.toml, workloads.toml) and the run records (benches/runs/*.toml).

    suite.py status            print it
    suite.py status --write    write benches/STATUS.md (commit it with the records)

A pair is a workload tier and a system. For each, the status says whether it can run (the
system has the capabilities, the suite or a kit can start it), and the newest outcome a run
record holds, with the record's date. The standard tiers of a workload are its `tiers` in
workloads.toml: the sizes a full comparison runs.
"""
from __future__ import annotations

import argparse
import datetime
from pathlib import Path

from .adapters import adapter
from .ledger import load_records
from .workloads import DEFINITIONS, plans

MARK = {"ok": "ok", "partial": "part", "wrong": "WRONG", "failed": "FAIL", "timeout": "T/O",
        "skipped": "skip", "restricted": "ran*", "disputed": "≠?"}
LEGEND = ("`ok` every repetition loaded, every step and query execution ok · `part` a load, step or query execution failed or timed out in some repetition · `WRONG` an answer count "
          "differs from the expected one · `FAIL`/`T/O` the load failed or timed out · `skip` the "
          "suite skipped it (reason in the record) · `≠?` its answers differ from another system's, "
          "not yet adjudicated · `ran*` ran; a licensed system whose outcome "
          "stays local · `·` can run, never run · `–` lacks a capability · `n/a` no adapter or kit "
          "for it")


def verdict(system: dict, workload: dict) -> str | None:
    have = set(system.get("capabilities", []))
    missing = [c for c in workload.get("needs", []) if c not in have]
    alternatives = workload.get("any", [])
    if alternatives and not have & set(alternatives):
        missing.append(" or ".join(alternatives))
    return ", ".join(missing) or None


def standard_tiers(name: str, workload: dict) -> list[str]:
    if workload.get("tiers"):
        return [str(t) for t in workload["tiers"]]
    if name in DEFINITIONS:
        return DEFINITIONS[name][1]
    return ["-"]


def runnable(root: Path, key: str, system: dict, name: str, workload: dict) -> str | None:
    """None if the pair can run, else "lacks" or "n/a"."""
    if verdict(system, workload):
        return "lacks"
    if name not in DEFINITIONS:
        # Outside the suite's driver: a kit runs it (or will); the capabilities decide, and
        # a system nothing can start (runs = []) can't run it.
        return None if system.get("runs") else "n/a"
    plan = plans(root, name, [standard_tiers(name, workload)[0]], {})[0]
    if plan.systems is not None:
        return None if key in plan.systems else "n/a"
    instance = adapter(key)
    if instance is None:
        return "n/a"
    if not any(r in instance.regimes for r in plan.regimes):
        return "n/a"
    return None


def newest(records: list[dict]) -> dict[tuple, tuple[str, dict, str]]:
    """(workload, tier, system) -> (date, pair, record id) of the newest record that isn't a skip,
    or the newest skip if there is nothing else."""
    best: dict[tuple, tuple[str, dict, str]] = {}
    for record in records:
        # The full start time: two records of one day are ordered by it, not by file name.
        date = str(record.get("started", ""))
        for pair in record.get("pairs", []):
            key = (pair["workload"], str(pair["tier"]), pair["system"])
            current = best.get(key)
            rank = (pair["outcome"] != "skipped", date)
            if current is None or rank > (current[1]["outcome"] != "skipped", current[0]):
                best[key] = (date, pair, record["id"])
    return best


def render(root: Path, systems: dict, workloads: dict) -> str:
    records = load_records(root / "benches/runs")
    evidence = newest(records)
    today = datetime.date.today().isoformat()
    out = ["# Benchmark status", "",
           f"Generated {today} by `python benches/suite/suite.py status --write` from "
           "[suite/systems.toml](suite/systems.toml), [suite/workloads.toml](suite/workloads.toml) and the run "
           "records in [runs/](runs/README.md). Don't edit it by hand: change those and regenerate. How to read "
           "and extend it: [README.md](README.md).", ""]

    # --- at a glance
    states: dict[str, int] = {}
    for w in workloads.values():
        states[w["state"]] = states.get(w["state"], 0) + 1
    pairs_total = pairs_run = pairs_ok = 0
    nrese_tiers = nrese_ok = 0
    for name, w in workloads.items():
        for key, s in systems.items():
            if runnable(root, key, s, name, w) is not None or s.get("variant-of"):
                continue
            for tier in standard_tiers(name, w):
                pairs_total += 1
                e = evidence.get((name, tier, key))
                if e and e[1]["outcome"] != "skipped":
                    pairs_run += 1
                    pairs_ok += e[1]["outcome"] in ("ok", "restricted")
                if key == "nrese":
                    nrese_tiers += 1
                    nrese_ok += bool(e and e[1]["outcome"] == "ok")
    out += ["## At a glance", "",
            f"- **Workloads:** {len(workloads)} ({', '.join(f'{v} {k}' for k, v in sorted(states.items()))})",
            f"- **Systems:** {len(systems)} ({sum(s['publish'] == 'free' for s in systems.values())} free to publish, "
            f"{sum(s['publish'] != 'free' for s in systems.values())} only with the vendor's permission)",
            f"- **Pairs** (standard tier × system that can run it; build variants apart): {pairs_total}; run at least once: {pairs_run}; "
            f"ok on their newest run: {pairs_ok}",
            f"- **NRESE:** {nrese_ok} of its {nrese_tiers} standard tiers ok on their newest run",
            f"- **Run records:** {len(records)}, newest {max((str(r.get('started', ''))[:10] for r in records), default='-')}",
            ""]

    # --- the matrix
    names = list(systems)
    out += ["## Coverage", "",
            "Per workload and system: standard tiers with an `ok` (or `ran*`) newest outcome / standard tiers. "
            "A `!` marks a newest outcome that is `FAIL`, `T/O`, `WRONG` or `part` on some tier.", "",
            "| Workload | State | " + " | ".join(names) + " |",
            "|---|---|" + "---|" * len(names)]
    for name, w in workloads.items():
        cells = []
        tiers = standard_tiers(name, w)
        for key in names:
            why = runnable(root, key, systems[key], name, w)
            if why == "lacks":
                cells.append("–")
                continue
            if why == "n/a":
                cells.append("n/a")
                continue
            got = [evidence.get((name, t, key)) for t in tiers]
            good = sum(1 for e in got if e and e[1]["outcome"] in ("ok", "restricted"))
            bad = any(e and e[1]["outcome"] in ("failed", "timeout", "wrong", "partial", "disputed") for e in got)
            ran = any(e and e[1]["outcome"] != "skipped" for e in got)
            cells.append(f"{good}/{len(tiers)}{' !' if bad else ''}" if ran else "·")
        out.append(f"| {name} | {w['state']} | " + " | ".join(cells) + " |")
    out += ["", LEGEND, ""]

    # --- per workload
    out += ["## Workloads, tier by tier", ""]
    for name, w in workloads.items():
        tiers = standard_tiers(name, w)
        able = [k for k in names if runnable(root, k, systems[k], name, w) is None]
        kit = w.get("kit", "-")
        driver = "suite driver" if name in DEFINITIONS else "outside the driver"
        out += [f"### {name}: {w['title']}", "",
                f"{w['state']}, {w['kind']}; tasks {', '.join(w.get('tasks', []))}; kit `{kit}` ({driver}); "
                f"checked by: {w['check']}.", ""]
        if not able:
            out += ["No system can run it yet.", ""]
            continue
        extra = sorted({t for (wl, t, _s) in evidence if wl == name} - set(tiers))
        out += ["| Tier | " + " | ".join(able) + " |", "|---|" + "---|" * len(able)]
        for tier in tiers + extra:
            row = []
            for key in able:
                e = evidence.get((name, tier, key))
                row.append(f"{MARK[e[1]['outcome']]} {e[0][5:10]}" if e else "·")
            label = tier if tier in tiers else f"{tier} (extra)"
            out.append(f"| {label} | " + " | ".join(row) + " |")
        out.append("")

    # --- gaps
    def gaps(keys: list[str]) -> list[str]:
        lines = []
        for name, w in workloads.items():
            missing = []
            for key in keys:
                if runnable(root, key, systems[key], name, w) is not None:
                    continue
                tiers = [t for t in standard_tiers(name, w)
                         if not (e := evidence.get((name, t, key))) or e[1]["outcome"] == "skipped"]
                if tiers:
                    missing.append(f"{key} ({', '.join(tiers)})")
            if missing:
                lines.append(f"- **{name}:** {'; '.join(missing)}")
        return lines

    variants = [k for k in names if systems[k].get("variant-of")]
    out += ["## Gaps", "", "### Pairs that can run and have no outcome yet", ""]
    out += gaps([k for k in names if k not in variants])
    if variants:
        out += ["", f"### Build variants without an outcome ({', '.join(variants)}; optional)", ""]
        out += gaps(variants)
    out += ["", "### Problems on the newest run", ""]
    for (name, tier, key), (date, pair, rid) in sorted(evidence.items()):
        if pair["outcome"] in ("failed", "timeout", "wrong", "partial", "disputed"):
            out.append(f"- {name} {tier} on {key}: {pair['outcome']} ({rid}){': ' + pair['note'] if pair.get('note') else ''}")
    out += ["", "### Workloads not ready", ""]
    for name, w in workloads.items():
        if w["state"] != "ready":
            extra = f" Waits for {w['waits-for']}." if w.get("waits-for") else ""
            out.append(f"- **{name}** ({w['state']}): {w['title']}. {w.get('note', '')}{extra}".rstrip())
    nrese = systems.get("nrese", {})
    lacking = nrese.get("missing", {})
    if lacking:
        out += ["", "### Capabilities NRESE lacks", ""] + [f"- {c}: {step}" for c, step in lacking.items()]
    out.append("")

    # --- runs
    out += ["## Runs", "", "Newest first; the records are in [runs/](runs/).", "",
            "| Run | Started | Kit | Host | Commit | Pairs | Purpose |", "|---|---|---|---|---|---|---|"]
    for r in sorted(records, key=lambda r: str(r.get("started", "")), reverse=True):
        commit = str(r.get("commit", "?"))
        commit = commit[:10] if len(commit) >= 40 else commit
        pairs = [p for p in r.get("pairs", []) if p["outcome"] != "skipped"]
        out.append(f"| [{r['id']}](runs/{r['id']}.toml) | {str(r.get('started', ''))[:16]} | {r.get('kit', 'suite')} | "
                   f"{r.get('host', '')} | {commit} | {len(pairs)} | {r.get('purpose', '')} |")
    out.append("")
    return "\n".join(out)


def main(argv: list[str], systems: dict, workloads: dict, root: Path) -> int:
    p = argparse.ArgumentParser(prog="suite.py status", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--write", action="store_true", help="write benches/STATUS.md")
    args = p.parse_args(argv)
    text = render(root, systems, workloads)
    if args.write:
        (root / "benches/STATUS.md").write_text(text, encoding="utf-8", newline="\n")
        print("benches/STATUS.md")
    else:
        print(text)
    return 0
