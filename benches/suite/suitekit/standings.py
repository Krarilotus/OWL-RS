"""`suite.py standings [--write]`: where NRESE stands against every contender, from the run
records' `versus` entries (suitekit/versus.py), into benches/STANDINGS.md.

Per contender (each system of systems.toml but NRESE and its variants): its home-turf cases
first, then the others it ran; per case the newest comparison (a case run on several hosts
shows each host's newest), its answers, NRESE's time against its own with the interval, and
the verdict. A licensed system shows only that it ran: its results stay local.
"""
from __future__ import annotations

import argparse
import datetime
import tomllib
from collections import defaultdict
from pathlib import Path

from .ledger import load_records

VERDICTS = ("win", "tie", "loss", "no-verdict")


def newest(records: list[dict]) -> dict[tuple, list[tuple[dict, dict]]]:
    """(system, case, semantics, host) -> the entries of the newest record with them."""
    out: dict[tuple, tuple[str, list]] = {}
    for record in records:
        started = str(record.get("started", ""))
        for entry in record.get("versus", []):
            key = (entry["system"], entry["case"], entry.get("semantics", "-"), str(record.get("host", "")))
            held = out.get(key)
            if held is None or started > held[0]:
                out[key] = (started, [])
            if out[key][0] == started:
                out[key][1].append((entry, record))
    return {k: v for k, (_, v) in out.items()}


def cell_time(entry: dict) -> str:
    time = entry.get("time")
    if not time:
        return "-"
    low, high = time["ci"]
    return f"{time['ratio']:.3g} [{low:.3g}, {high:.3g}]"


def render(records: list[dict], systems: dict, root: Path) -> str:
    table = newest(records)
    restricted = {k for k, s in systems.items() if s.get("publish") == "permission"}
    contenders = [k for k, s in systems.items() if k != "nrese" and not k.startswith("nrese") and "variant-of" not in s]
    ran_restricted = defaultdict(set)
    for record in records:
        for pair in record.get("pairs", []):
            if pair.get("system") in restricted and pair.get("outcome") == "restricted":
                ran_restricted[pair["system"]].add(record["id"])
    lines = [
        "# Standings: NRESE against every contender",
        "",
        f"Generated {datetime.date.today().isoformat()} by `python benches/suite/suite.py standings --write` from the "
        "run records' `versus` entries ([runs/](runs/README.md)). Don't edit it by hand.",
        "",
        "Per case and contender, the newest comparison on each host: answers first (each system against the "
        "reference systems; a wrong, missing or unchecked answer is never timed), then NRESE's time over the "
        "queries both answered right, as NRESE / contender with its 95 % bootstrap interval (below 1: NRESE "
        "faster). **win**: faster beyond max(10 %, the spread) and 2 ms, the interval below 1; **loss**: the "
        "other way round; **tie**: neither; **no-verdict**: nothing both answered right. Load and memory "
        "(serve peak, with its kind) as the same ratio. A case marked ★ is the contender's home turf "
        "([cases/](cases/README.md) `home_turf`).",
        "",
        "## Summary",
        "",
        "| Contender | Home turf: win / tie / loss / no verdict | All cases: win / tie / loss / no verdict | Answers not ok | Newest |",
        "|---|---|---|---|---|",
    ]
    sections = []
    from . import reported as published
    by_system: dict[str, list[dict]] = defaultdict(list)
    for entry in published.load():
        by_system[entry["system"]].append(entry)
    for system in contenders:
        if systems[system].get("refused"):
            # Never measured by us: its published numbers, as reported, on their hardware.
            found = by_system.get(system, [])
            lines.append(f"| {system} | refused | refused | - | {systems[system]['refused']}; "
                         f"{len(found)} published numbers below |")
            if found:
                sections += ["", f"## {system}: reported, not measured by us", "",
                             "As the source reports it, on the source's hardware: not comparable with our "
                             "measurements as if it were one ([competitors/reported.toml](competitors/reported.toml)).",
                             "", "| Benchmark | Tier | Metric | Value | Threads | Hardware | Source | Date |",
                             "|---|---|---|---|---|---|---|---|"]
                for e in found:
                    sections.append(f"| {e['benchmark']} | {e.get('tier', '-')} | {e['metric']} | "
                                    f"{e['value']} {e['unit']} | {e.get('threads', '-')} | {e['hardware']} | "
                                    f"{e['source']}" + (f" (reported by {e['reported_by']})" if e.get('reported_by') else "")
                                    + f" | {e['date']} |")
            continue
        if system in restricted:
            runs = sorted(ran_restricted.get(system, set()))
            lines.append(f"| {system} | restricted | restricted | - | "
                         + (f"ran in {len(runs)} runs; results stay local" if runs else "not run") + " |")
            continue
        rows = sorted(((e, r) for (s, *_), entries in table.items() if s == system for e, r in entries),
                      key=lambda er: (er[0].get("home_turf") != system, er[0]["case"], er[0].get("run", "")))
        if not rows:
            lines.append(f"| {system} | - | - | - | not measured yet |")
            continue
        def counts(subset):
            return " / ".join(str(sum(1 for e, _ in subset if e["verdict"] == v)) for v in VERDICTS)
        turf = [er for er in rows if er[0].get("home_turf") == system]
        wrong = sum(1 for e, _ in rows if e["answers"] != "ok")
        latest = max(str(r.get("started", ""))[:10] for _, r in rows)
        lines.append(f"| {system} | {counts(turf) if turf else '-'} | {counts(rows)} | {wrong} | {latest} |")
        sections += ["", f"## {system}", "",
                     "| Case | Semantics | Answers | Timed | Time NRESE/them [95 %] | Verdict | Load | Memory | Run |",
                     "|---|---|---|---|---|---|---|---|---|"]
        repeated = {e["case"] for e, _ in rows if sum(1 for f, _ in rows if f["case"] == e["case"]) > 1}
        for e, r in rows:
            star = " ★" if e.get("home_turf") == system else ""
            if e["case"] in repeated:
                star += f" ({e.get('run', '?')})"
            memory = e.get("memory")
            memory = f"{memory['ratio']:.3g} ({memory['kind']})" if memory else "-"
            sections.append(f"| {e['case']}{star} | {e.get('semantics', '-')} | {e['answers']} | {e.get('timed', '-')} | "
                            f"{cell_time(e)} | **{e['verdict']}** | {e.get('load', '-')} | {memory} | "
                            f"[{r['id']}](runs/{r['id']}.toml) ({r.get('host', '?')}, {str(r.get('commit', '?'))[:7]}) |")
    return "\n".join(lines + sections) + "\n"


def main(argv: list[str], systems: dict, root: Path) -> int:
    p = argparse.ArgumentParser(prog="suite.py standings", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--write", action="store_true", help="write benches/STANDINGS.md")
    args = p.parse_args(argv)
    text = render(load_records(root / "benches/runs"), systems, root)
    if args.write:
        path = root / "benches/STANDINGS.md"
        path.write_text(text, encoding="utf-8", newline="\n")
        print(f"wrote {path.relative_to(root).as_posix()}")
    else:
        print(text)
    return 0
