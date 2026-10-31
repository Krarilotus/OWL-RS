"""The one `compare` (benches/README.md, "One model"): a run against a baseline run, in this
order, so that nothing lost hides behind a time:

1. **Missing:** cases, pairs, repetitions, series and queries one side has and the other
   doesn't.
2. **Answers and completeness:** status changes, wrong answers, answers that differ from the
   baseline's (counts; with answers.json beside the results, each system's verdict), work
   complete before and incomplete now, and known issues with their checked values and
   their run past the limit.
3. **Time:** what got slower or faster beyond the thresholds; work counters; throughput.
4. **Memory, by kind:** peaks compared only with peaks of their kind, and only kinds that
   measure memory in use, never cache (the owner's decision, 13 Oct 2026): `lifetime`, a
   process's peak resident memory (NRESE's own A/Bs); `anon-kernel`, a container's or
   scope's anonymous plus kernel memory from memory.stat, sampled (cross-system: a contender
   runs several processes in its container); `sampled`, `docker stats` readings about a
   second apart; `phase`, a peak reset at each phase. `cgroup`, the cgroup's `memory.peak`,
   counts the page cache its reads were charged (rl-lubm100: 2.3 GB of 4.7 by what the host
   had cached): recorded, never compared.
5. **Disk:** store sizes.

    suite.py compare BASE NEW [--min-ms 2] [--threshold 0.10] [--systems S] [--include-restricted]
    fast.py compare BASE.json NEW.json [--min-ms 2] [--allow-host-change]

Runs of different hosts, or of one host under different configurations (benches/hosts.toml
`config`, or a different CPU governor read by the runs), are not compared: compare exits 2
unless --allow-host-change, and then flags every verdict as no evidence of a code change.

BASE and NEW are fast-suite reports (`.json`: the full report, or a compact record that
names it), perf-lab reports (`.json` with `queries`), or suite results (directories or
results.csv files, several comma-separated). The thresholds have one owner,
benches/fast/README.md#comparing; the suite's completeness, eligibility and estimator are
summary.py's. Exit status 1 when anything regressed or can't be compared.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import random
import re
import statistics
from pathlib import Path

from . import hosts
from .answers import TimingVerdicts, timed as answer_timed
from .schema import read
from .summary import Item, Pair, estimate, summarise

SECTIONS = ("Missing", "Answers and completeness", "Time", "Memory, by kind", "Disk")
# Order statistics per series in a compact fast record (fast.py write_compact; every 2.5th
# percentile): compare's verdicts on the two runs of 6 Oct 2026 were the full reports' with
# 41; with 21, two of 80 borderline cases flipped.
KEPT_POINTS = 41


class Out:
    """The comparison's lines, by section, printed in SECTIONS' order."""

    def __init__(self):
        self.lines: dict[str, list[str]] = {s: [] for s in SECTIONS}
        self.regressions = 0

    def add(self, section: str, line: str, regression: bool = False):
        self.lines[section].append(line)
        self.regressions += int(regression)

    def print(self, header: dict[str, str] | None = None):
        for section in SECTIONS:
            if self.lines[section]:
                print(f"## {section}\n")
                if header and section in header:
                    print(header[section])
                print("\n".join(self.lines[section]) + "\n")


# --- statistics ------------------------------------------------------------------------------

def thin(values: list[float], at_most: int = 200) -> list[float]:
    """At most `at_most` evenly spaced order statistics of sorted `values`: the same
    distribution, small enough to resample quickly."""
    if len(values) <= at_most:
        return values
    step = (len(values) - 1) / (at_most - 1)
    return [values[round(i * step)] for i in range(at_most)]


def statistic(reps: list[dict[str, list[float]]], series: list[str]) -> float:
    """The sum over series of the median of their samples, pooled over `reps`."""
    total = 0.0
    for s in series:
        values = [v for rep in reps for v in rep.get(s, [])]
        if values:
            total += statistics.median(values)
    return total


def spread(entry: dict) -> float:
    """The range of the per-repetition metrics relative to their median (PROTOCOL.md §5)."""
    m = [v for v in entry["metrics"] if v]
    if len(m) < 2:
        return 0.0
    return (max(m) - min(m)) / statistics.median(m)


# A process's peak varies between repetitions by what the allocator still holds of memory
# freed shortly before (up to 32 % in a small parallel load, 12 Oct 2026); beyond this,
# something not the process's is counted (the page cache in a cgroup's peak: 49-98 %).
UNSTABLE_PEAK = 0.40


def peak_spread(peaks: list) -> float:
    """The range of the per-repetition peaks relative to the smallest."""
    return (max(peaks) - min(peaks)) / min(peaks) if len(peaks) > 1 and min(peaks) > 0 else 0.0


def process_peak(peak, phase_peaks: dict):
    """A perf-lab run's lifetime peak: the process's since it started. A perf lab before
    12 Oct 2026 reported as `peak_mib` the last phase's alone, so the highest phase peak
    counts too. The cgroup's (a container's or native scope's `memory.peak`) is not it: that
    counts the page cache the run's reads were charged, which depends on what the host had
    cached (rl-lubm100: 2.3 GB of 4.7)."""
    values = [v for v in (peak, *(phase_peaks or {}).values()) if isinstance(v, (int, float)) and v > 0]
    return max(values) if values else None


def bootstrap_ratio(base: list[dict], new: list[dict], rounds: int = 1000, seed: int = 1,
                    level: float = 0.95) -> tuple[float, float, float]:
    """The ratio new/base of the summed medians and its 95 % interval, by a two-level
    bootstrap: repetitions drawn with replacement, then samples within each, so that the
    spread between runs widens the interval as much as the spread within them."""
    rng = random.Random(seed)
    series = sorted({s for rep in base for s in rep} & {s for rep in new for s in rep})
    if not series or not base or not new:
        return float("nan"), float("nan"), float("nan")

    def draw(reps):
        chosen = [rng.choice(reps) for _ in reps]
        return statistic([{s: rng.choices(rep[s], k=len(rep[s])) for s in series if rep.get(s)}
                          for rep in chosen], series)

    point = statistic(new, series) / max(statistic(base, series), 1e-9)
    ratios = sorted(draw(new) / max(draw(base), 1e-9) for _ in range(rounds))
    tail = (1 - level) / 2
    return point, ratios[int(tail * rounds)], ratios[min(rounds - 1, int((1 - tail) * rounds) - 1)]


# The release gate's statistics are suitekit/gatestats.py's (benches/PROTOCOL.md §5).


def counter_changes(base: dict, new: dict) -> tuple[bool, list[str]]:
    """Whether a work counter grew beyond 2 %, and a note per changed counter. A counter
    whose repetitions differ within one run is not deterministic and isn't judged."""
    grew, notes = False, []
    for path in sorted(set(base) & set(new)):
        b, n = base[path], new[path]
        if any(value != b[0] for value in b[1:]) or any(value != n[0] for value in n[1:]):
            continue
        before, after = b[0], n[0]
        if before == after:
            continue
        if not all(isinstance(value, (int, float)) and not isinstance(value, bool)
                   for value in (before, after)):
            notes.append(f"{path}: {json.dumps(before, sort_keys=True)} -> "
                         f"{json.dumps(after, sort_keys=True)} (metadata changed)")
            continue
        change = (after - before) / before if before else float("inf")
        mark = ""
        if change > 0.02:
            grew, mark = True, "  MORE WORK"
        elif change < -0.02:
            mark = "  less work"
        notes.append(f"{path}: {before:g} -> {after:g} ({change:+.1%}){mark}")
    return grew, notes


def series_ratios(base: list[dict], new: list[dict], series: list[str]) -> list[str]:
    """The series of a case sorted by how far their median moved, as `name ratio`."""
    out = []
    for s in series:
        b, n = statistic(base, [s]), statistic(new, [s])
        if b > 0:
            out.append((abs(n / b - 1), f"{s} {n / b:.2f}x"))
    return [text for _, text in sorted(out, reverse=True)]


# --- fast-suite reports ----------------------------------------------------------------------

def client_throughput(result: dict) -> list[dict]:
    """One client level and a sweep share the same throughput contract.

    These peaks include the in-process harness's latency samples; they are not
    isolated engine memory. Unknown memory must not become a synthetic QPS/GB win.
    """
    levels = result.get("client_sweep") or ([result["clients"]] if result.get("clients") else [])
    out = []
    for level in levels:
        peak = (level.get("memory") or {}).get("peak_mib")
        qps = level.get("qps")
        out.append({"clients": level.get("clients"), "qps": qps, "p99_ms": level.get("p99_ms"),
                    "peak_mib": peak,
                    "qps_per_gb": round(qps / peak * 1024, 1)
                    if isinstance(qps, (int, float)) and isinstance(peak, (int, float)) and peak > 0 else None})
    return out


def load_report(path: Path) -> dict:
    """A run's report: the full one when a compact record names it and it is here with its
    hash, else the compact record (its KEPT_POINTS points per series). A perf-lab report
    (one run on a dataset, `queries` with rows and p50) becomes a one-case report."""
    report = json.loads(path.read_text(encoding="utf-8"))
    if "records" not in report and "queries" in report:
        return perf_lab_report(report)
    full = report.get("full_report")
    if full:
        candidate = Path(full["path"].replace("~", str(Path.home()), 1))
        if candidate.exists() and hashlib.sha256(candidate.read_bytes()).hexdigest() == full["sha256"]:
            return json.loads(candidate.read_text(encoding="utf-8"))
        print(f"note: {path.name}: the full report isn't here ({full['path']} on {full['host']}); "
              f"comparing its {KEPT_POINTS} points per series")
    return report


def perf_lab_report(report: dict) -> dict:
    """A perf-lab report (benches/baselines/perf-lab/) as a one-repetition fast report: each
    query a series of its p50, its rows the answers."""
    queries = [q for q in report["queries"] if "error" not in q]
    return {"reps": 1, "records": [{
        "case": "perf-lab", "rep": 1, "status": "ok", "metric": report.get("sum_p50_ms"),
        "samples": {q["name"]: [q["p50_ms"]] for q in queries},
        "answers": {q["name"]: q.get("rows") for q in report["queries"]},
        "peak_mib": process_peak(report.get("peak_mib"),
                                 {k: v.get("peak_mib") for k, v in (report.get("phases") or {}).items()}),
        "memory_kind": "lifetime",
        "checks": [], "routes": []}]}


def valid_series(samples: dict) -> bool:
    return bool(isinstance(samples, dict) and samples and all(isinstance(vs, list) and vs and
                all(type(v) in (int, float) and math.isfinite(v) and v >= 0 for v in vs) for vs in samples.values()))


def complete_timing(*entries: dict, series=None) -> bool:
    reps = [rep for e in entries for rep in e["reps"]]
    return bool(reps and all(len(e["reps"]) == len(e["status"]) == len(e["metrics"]) for e in entries)
                and all(type(v) in (int, float) and math.isfinite(v) and v >= 0 for e in entries for v in e["metrics"])
                and all(valid_series(rep) and set(rep) == set(series if series is not None else reps[0]) for rep in reps))


def pooled(report: dict) -> dict[str, dict]:
    """Per case: its samples per repetition, statuses, peak memory, the metric per repetition."""
    out: dict[str, dict] = {}
    for r in report["records"]:
        entry = out.setdefault(r["case"], {"reps": [], "status": [], "peaks": [], "metrics": [],
                                           "counters": {}, "phase_peaks": {}, "qps": [], "client_peaks": [],
                                           "cache_peaks": [], "client_case": False,
                                           "expected": [], "invalid": False, "expected_reps": report.get("reps", 0),
                                           "checks": {}, "frontiers": [], "answers": {}, "memory_kind": set()})
        entry["expected"].append(r.get("expected_outcome", "ok"))
        # ok None: a check the tool couldn't report (fast.py run --no-routes), unchecked, not failed.
        entry["invalid"] |= any(c.get("ok") is False for c in r.get("checks", []) + r.get("routes", []))
        # Older full reports retain single-level clients only in their raw result.
        # An explicitly empty throughput list remains missing evidence, not a fallback.
        raw = r.get("result") or {}
        levels = r["throughput"] if "throughput" in r else client_throughput(raw)
        # Old compact client reports lost the raw QPS. Their case/series still
        # identifies the missing evidence; a latency-only pass would be misleading.
        entry["client_case"] |= ("throughput" in r or "clients" in raw or "client_sweep" in raw
                                 or r["case"] in ("clients-lookups", "clients-sweep")
                                 or any(s.endswith(" clients p99") for s in r.get("samples", {})))
        widths, client_peaks = {}, {}
        for level in levels or []:
            width = str(level.get("clients"))
            # A duplicate in this repetition must not replace a missing observation
            # in another repetition. Keep invalid observations explicitly missing.
            widths[width] = None if width in widths else level.get("qps")
            client_peaks[width] = None if width in client_peaks else level.get("peak_mib")
        raw_levels = raw.get("client_sweep") or ([raw["clients"]] if raw.get("clients") else [])
        for level in raw_levels:
            widths.setdefault(str(level.get("clients")), None)
        for series in r.get("samples", {}):
            if match := re.fullmatch(r"(\d+) clients p99", series):
                widths.setdefault(match[1], None)
        entry["qps"].append(widths)
        entry["client_peaks"].append(client_peaks)
        entry["cache_peaks"].append(r.get("cache_peaks") or {})
        for path, value in (r.get("counters") or {}).items():
            entry["counters"].setdefault(path, []).append(value)
        for phase, peak in (r.get("phase_peaks") or {}).items():
            if peak:
                entry["phase_peaks"].setdefault(phase, []).append(peak)
        entry["status"].append(r["status"])
        for c in r.get("checks", []):
            entry["checks"].setdefault(c.get("path", "?"), []).append(c.get("got"))
        for name, rows in (r.get("answers") or {}).items():
            entry["answers"].setdefault(name, []).append(rows)
        if r.get("frontier"):
            entry["frontiers"].append(r["frontier"])
        # A fast record before 13 Oct 2026 holds the cgroup's peak, or no kind: the process's
        # is recovered from its perf-lab result where it has one.
        peak, kind = r.get("peak_mib"), r.get("memory_kind")
        if kind != "lifetime" and (recovered := process_peak((r.get("result") or {}).get("peak_mib"),
                                                             r.get("phase_peaks") or {})):
            peak, kind = recovered, "lifetime"
        if peak:
            entry["peaks"].append(peak)
            # Records before the kind was kept: unknown, compared with any kind.
            entry["memory_kind"].add(kind or "unknown")
        if isinstance(r.get("metric"), (int, float)):
            entry["metrics"].append(r["metric"])
        samples = {k: thin(sorted(v for v in vs if v is not None)) for k, vs in (r.get("samples") or {}).items()}
        if any(samples.values()):
            entry["reps"].append(samples)
    return out


# Kinds that count page cache: recorded beside a peak, never compared.
CACHE_KINDS = {"cgroup"}


def kind_of(entry: dict) -> str:
    kinds = entry["memory_kind"] - {"unknown"}
    return "/".join(sorted(kinds)) if kinds else "unknown"


def compare_cases(base: dict[str, dict], new: dict[str, dict], min_ms: float, out: Out) -> list[float]:
    """Fast-suite cases (or a perf-lab report as one), case by case; the spreads seen."""
    spreads = []
    for case in sorted(set(base) | set(new)):
        b, n = base.get(case), new.get(case)
        if not b or not n:
            out.add("Missing", f"{case:<26} {'LOST case' if b else 'new case without baseline'}", b is not None)
            continue
        for name in sorted(set(b["answers"]) | set(n["answers"])):
            before, after = b["answers"].get(name), n["answers"].get(name)
            if before is None or after is None:
                out.add("Missing", f"{case:<26} {name}: {'lost' if after is None else 'new'}", after is None)
            elif before != after:
                out.add("Answers and completeness", f"{case:<26} {name}: rows {'/'.join(map(str, before))} -> "
                        f"{'/'.join(map(str, after))}  ANSWERS DIFFER", True)
        if set(n["status"]) != set(b["status"]):
            out.add("Answers and completeness",
                    f"{case:<26} status {','.join(sorted(set(b['status'])))} -> {','.join(sorted(set(n['status'])))}  CHANGED",
                    True)
            continue
        if any(len(e["status"]) < e["expected_reps"] for e in (b, n)):
            out.add("Missing", f"{case:<26} NOT COMPARABLE: missing/invalid timing series or repetitions", True)
            continue
        if any(e["invalid"] or set(e["status"]) != {"ok"} for e in (b, n)):
            # A known product issue (the status the case declares, on both sides) tracks its
            # values and its frontier; anything else is no valid comparison.
            known = all(e["status"] == e["expected"] and "ok" not in e["expected"] for e in (b, n))
            out.add("Answers and completeness",
                    f"{case:<26} {'known issue' if known else 'NOT COMPARABLE: failed checks/status'}; no timing verdict",
                    not known)
            for path in sorted(set(b["checks"]) & set(n["checks"])):
                if b["checks"][path] != n["checks"][path]:
                    out.add("Answers and completeness", f"{'':<28}{path}: {'/'.join(map(str, b['checks'][path]))} -> "
                            f"{'/'.join(map(str, n['checks'][path]))}")
            if b["frontiers"] and n["frontiers"]:
                # Every repetition's run past the limit: its statuses, the medians of its metric and
                # peak (one run alone is one sample; the A/B of 12 Oct 2026 had a 4,032 MiB first
                # run beside three of 5,112-5,153).
                def past(entry):
                    runs = entry["frontiers"]
                    numbers = [[f.get(k) for f in runs if isinstance(f.get(k), (int, float))] for k in ("metric", "peak_mib")]
                    return ({f["status"] for f in runs}, *(statistics.median(v) if v else None for v in numbers))
                (sb, mb, pb), (sn, mn, pn) = past(b), past(n)
                worse = "ok" in sb and sn != {"ok"}
                worse |= bool(pb and pn and pn > pb * 1.10 and pn - pb > 64)
                out.add("Answers and completeness",
                        f"{'':<28}past its limit ({len(b['frontiers'])} and {len(n['frontiers'])} runs, medians): "
                        f"{','.join(sorted(sb))} -> {','.join(sorted(sn))}, metric {mb} -> {mn}, peak {pb} -> {pn} MiB"
                        + ("  WORSE" if worse else ""), worse)
            continue
        if not complete_timing(b, n):
            out.add("Missing", f"{case:<26} NOT COMPARABLE: missing/invalid timing series or repetitions", True)
            continue
        ratio, low, high = bootstrap_ratio(b["reps"], n["reps"])
        series = sorted({s for rep in b["reps"] for s in rep} & {s for rep in n["reps"] for s in rep})
        bm, nm = statistic(b["reps"], series), statistic(n["reps"], series)
        # A change counts beyond max(10 %, the measured spread of both sides), the chosen floor,
        # and with the interval excluding 1.
        threshold = max(0.10, spread(b), spread(n))
        spreads.append(max(spread(b), spread(n)))
        verdict, slower = "same", False
        if low > 1.0 and ratio >= 1 + threshold:
            verdict = "SLOWER" if nm - bm >= min_ms else "slower, below absolute floor"
            slower = nm - bm >= min_ms
        elif high < 1.0 and ratio <= 1 / (1 + threshold):
            verdict = "faster" if bm - nm >= min_ms else "faster, below absolute floor"
        work, notes = counter_changes(b["counters"], n["counters"])
        if work:
            verdict += ", MORE WORK"
        out.add("Time", f"{case:<26} {bm:>11.4g} {nm:>11.4g} {ratio:>7.2f} {f'[{low:.2f}, {high:.2f}]':>15} "
                f"{threshold:>6.0%}  {verdict}", slower)
        out.regressions += int(work)
        widths = {width for e in (b, n) for rep in e["qps"] for width in rep}
        if not widths and any(e["client_case"] for e in (b, n)):
            out.add("Time", "  clients QPS: NOT COMPARABLE (throughput missing; retrieve the verified full reports)", True)
        for clients in sorted(widths, key=lambda w: (len(w), w)):
            values = [[rep.get(clients) for rep in e["qps"]] for e in (b, n)]
            if not clients.isdecimal() or int(clients) <= 0 or any(len(vs) != len(e["status"]) or len(vs) < 2
                   or any(not isinstance(v, (int, float)) or not math.isfinite(v) or v < 0 for v in vs)
                   for e, vs in zip((b, n), values)) or statistics.median(values[0]) <= 0:
                out.add("Time", f"  {clients} clients QPS: NOT COMPARABLE (missing/invalid samples or fewer than two "
                        "repetitions)", True)
                continue
            qr, ql, qh = bootstrap_ratio(*[[{"qps": [v]} for v in vs] for vs in values])
            qt = max(0.10, *(spread({"metrics": vs}) for vs in values))
            qv = "LOWER QPS" if qh < 1 and qr <= 1 / (1 + qt) else (
                "higher QPS" if ql > 1 and qr >= 1 + qt else "within noise")
            out.add("Time", f"  {clients} clients QPS: {statistics.median(values[0]):.4g} -> "
                    f"{statistics.median(values[1]):.4g}; ratio {qr:.3f}, 95% CI [{ql:.3f}, {qh:.3f}], "
                    f"spread threshold {qt:.1%}; {qv}", qv == "LOWER QPS")
        for note in notes:
            out.add("Time", f"{'':<28}{note}")
        if verdict.startswith(("SLOWER", "faster")) and len(series) > 1:
            out.add("Time", f"{'':<28}by series: " + ", ".join(series_ratios(b["reps"], n["reps"], series)[:3]))
        # Memory: the case's peak (its kind kept with it) and each phase's peak apart.
        bp, np_ = (max(b["peaks"]) if b["peaks"] else None), (max(n["peaks"]) if n["peaks"] else None)
        bk, nk = kind_of(b), kind_of(n)
        # A peak counts beyond either side's spread of peaks too, as a time beyond its spread.
        memory_spread = max(peak_spread(b["peaks"]), peak_spread(n["peaks"]))
        if bp and np_:
            if CACHE_KINDS & {bk, nk}:
                out.add("Memory, by kind", f"{case:<26} peak {bp} ({bk}) -> {np_} ({nk}) MiB: counts page cache, "
                        "recorded, not compared", False)
            elif "unknown" not in (bk, nk) and bk != nk:
                out.add("Memory, by kind", f"{case:<26} peak {bp} ({bk}) -> {np_} ({nk}) MiB: other kinds, not compared",
                        True)
            elif memory_spread > UNSTABLE_PEAK:
                out.add("Memory, by kind", f"{case:<26} peak ({nk if nk != 'unknown' else bk}) {bp} -> {np_} MiB: "
                        f"NOT COMPARABLE (peaks vary {memory_spread:.0%} between repetitions)", True)
            else:
                more = np_ > bp * (1 + max(0.10, threshold, memory_spread)) and np_ - bp > 64
                out.add("Memory, by kind", f"{case:<26} peak ({nk if nk != 'unknown' else bk}) {bp} -> {np_} MiB"
                        + ("  MORE MEMORY" if more else ""), more)
        for phase in sorted(set(b["phase_peaks"]) & set(n["phase_peaks"])):
            before, after = max(b["phase_peaks"][phase]), max(n["phase_peaks"][phase])
            phase_spread = max(peak_spread(b["phase_peaks"][phase]), peak_spread(n["phase_peaks"][phase]))
            if after > before * (1 + max(0.10, threshold, phase_spread)) and after - before > 64:
                out.add("Memory, by kind", f"{case:<26} peak (phase {phase}) {before} -> {after} MiB  MORE MEMORY", True)
    return spreads


# --- the hosts a comparison spans ------------------------------------------------------------

def identity(machine: dict | None, started: str | None) -> dict | None:
    """A run's host, the configuration in force when it started (hosts.toml `config`), and
    the governor it read; None for a report that doesn't say (a perf-lab report)."""
    if not machine or not started:
        return None
    return hosts.run_identity(machine, started)


def host_check(base: list[dict | None], new: list[dict | None], allow: bool) -> int | None:
    """Prints what differs between the hosts and configurations of the two sides (and within
    a side). A difference refuses the comparison (exit 2) unless `allow`: then it is flagged
    and the comparison goes on. None when nothing stands in the way."""
    problems = []
    known = [i for i in base + new if i is not None]
    if len(known) < len(base) + len(new):
        problems.append("unknown host for some runs (a report without its machine): not checked")
    hosts_seen = {(i["host"], i["config"]) for i in known}
    if len({h for h, _ in hosts_seen}) > 1:
        problems.append("different hosts: " + ", ".join(sorted({h for h, _ in hosts_seen})))
    elif len(hosts_seen) > 1:
        problems.append(f"{known[0]['host']} changed its configuration between the runs (hosts.toml config since "
                        + " and ".join(sorted(c for _, c in hosts_seen)) + ")")
    governors = {i["governor"] for i in known if i.get("governor")}
    if len(governors) > 1:
        problems.append("the runs read different CPU governors: " + ", ".join(sorted(governors)))
    blocking = [p for p in problems if not p.startswith("unknown host")]
    if not problems:
        return None
    print("## Host configuration\n")
    for problem in problems:
        print(f"- {problem}")
    if blocking and not allow:
        print("\nREFUSED: timings of different hosts or host configurations don't compare "
              "(--allow-host-change compares them anyway, flagged)")
        return 2
    if blocking:
        print("\nFLAGGED: compared across a host change on request; no time or memory verdict below is "
              "evidence of a code change")
    print()
    return None


def compare_reports(base_report: dict, new_report: dict, min_ms: float = 2.0, allow_host_change: bool = False) -> int:
    """Two fast-suite (or perf-lab) reports; the exit status."""
    if not math.isfinite(min_ms) or min_ms < 0:
        raise ValueError("--min-ms must be finite and nonnegative")
    refused = host_check([identity(base_report.get("machine"), base_report.get("started"))],
                         [identity(new_report.get("machine"), new_report.get("started"))], allow_host_change)
    if refused:
        return refused
    for name, report in (("base", base_report), ("new", new_report)):
        if report.get("reps", 2) < 2:
            print(f"warning: the {name} run has one repetition: its spread is unknown, so a verdict "
                  "rests on the 10 % floor alone (run --reps 2 or more)")
    out = Out()
    spreads = compare_cases(pooled(base_report), pooled(new_report), min_ms, out)
    out.print({"Time": f"{'case':<26} {'base ms':>11} {'new ms':>11} {'ratio':>7} {'95% CI':>15} {'spread':>7}  verdict"})
    if spreads:
        print(f"spread between repetitions: median {statistics.median(spreads):.0%}, max {max(spreads):.0%}")
    print(f"{out.regressions} regression(s)/acceptance failure(s): latency floor {min_ms:g} ms (default 2); "
          "changes require max(10 %, spread) and a 95 % ratio interval excluding 1; "
          "QPS is higher-is-better, judged per client width without a millisecond floor")
    return 1 if out.regressions else 0


# --- suite results ---------------------------------------------------------------------------

def files(spec: str) -> list[Path]:
    out = []
    for part in spec.split(","):
        path = Path(part)
        out.append(path / "results.csv" if path.is_dir() else path)
    return out


def collect(paths: list[Path], systems: set[str] | None, restricted: bool) -> dict[tuple, Pair]:
    rows = []
    # Distinct files can both live under records/; aliases of one file are not new runs.
    for path in dict.fromkeys(p.resolve() for p in paths):
        for r in read(path):
            if systems and r["system"] not in systems:
                continue
            if r["publish"] == "permission" and not restricted:
                continue
            rows.append(r | {"run": f"{path.as_posix()}/{r['run']}"})
    return summarise(rows)


def memory_kinds(paths: list[Path]) -> dict[tuple, str]:
    """(workload, tier, system, task) -> the kind of its peaks: the rows' own `memory_kind`,
    else the runtime's (Docker's are sampled from `docker stats`; processes' are lifetime)."""
    out: dict[tuple, str] = {}
    for path in dict.fromkeys(p.resolve() for p in paths):
        for r in read(path):
            if r.get("peak_mib") in ("", None):
                continue
            kind = r.get("memory_kind") or ("sampled" if r.get("runtime") == "docker" else "lifetime")
            out[(r["workload"], r["tier"], r["system"], r["task"])] = kind
    return out


def manifest_identities(paths: list[Path]) -> list[dict | None]:
    """The host identity of every invocation in the manifest.json beside each results file."""
    out = []
    for path in dict.fromkeys(p.resolve() for p in paths):
        try:
            invocations = json.loads((path.parent / "manifest.json").read_text(encoding="utf-8"))["invocations"]
        except (OSError, ValueError, KeyError):
            out.append(None)
            continue
        out += [identity(i.get("machine"), i.get("started")) for i in invocations] or [None]
    return out


def verdicts(paths: list[Path]) -> TimingVerdicts:
    """The answer verdicts of the answers.json beside each results file, all of them, as timing
    reads them (answers.TimingVerdicts)."""
    records = []
    for path in dict.fromkeys(p.resolve() for p in paths):
        answers = path.parent / "answers.json"
        if answers.exists():
            records += json.loads(answers.read_text(encoding="utf-8")).get("verdicts", [])
    return TimingVerdicts(records)


def verdict(base, new, threshold: float, min_abs: float) -> str:
    (b, nb), (n, nn) = base, new
    if b <= 0:
        return "="
    ratio = n / b
    bound = max(threshold, (nb or 0) + (nn or 0))
    if ratio > 1 + bound and n - b > min_abs:
        return "slower"
    if ratio < 1 / (1 + bound) and b - n > min_abs:
        return "faster"
    return "="


def fmt(x: float) -> str:
    return f"{x:,.0f}" if abs(x) >= 100 else f"{x:,.1f}"


def eligible(item: Item, name: str, pair: Pair) -> bool:
    return item.complete and name not in pair.disputed


def compare_pairs(base: dict[tuple, Pair], new: dict[tuple, Pair], threshold: float, min_ms: float, out: Out,
                  kinds: tuple[dict, dict] = ({}, {}),
                  answers: tuple[TimingVerdicts, TimingVerdicts] | None = None) -> dict[str, int]:
    """Suite pairs, pair by pair, with summary.py's estimator; the query verdicts' totals."""
    answers = answers or (TimingVerdicts(), TimingVerdicts())
    totals = {"slower": 0, "faster": 0, "=": 0, "not comparable": 0}
    for k in sorted(k for k, pair in base.items() if pair.live and (k not in new or not new[k].live)):
        out.add("Missing", f"**Lost: pairs the baseline ran and the new run didn't:** {' '.join(k)}", True)
    for k in sorted(k for k, pair in new.items() if pair.live and (k not in base or not base[k].live)):
        out.add("Missing", f"New pair without a baseline: {' '.join(k)}")
    for key in sorted(set(base) & set(new)):
        b, n = base[key], new[key]
        if not (b.live and n.live):
            continue
        title = f"{key[0]} {key[1]}, {key[2]} ({n.regime})"
        failed = len(n.loads) - len(n.ok_runs())
        if failed:
            out.add("Answers and completeness", f"- {title}: **{failed} of {len(n.loads)} loads not ok in the new run**",
                    True)
        failed_steps = sorted({f"{t} {s}" for (t, s), c in n.other.items() if s != "ok"})
        if failed_steps:
            out.add("Answers and completeness", f"- {title}: **steps not ok in the new run:** {', '.join(failed_steps)}",
                    True)
        if b.counts and n.counts and b.counts != n.counts:
            out.add("Answers and completeness",
                    f"- {title}: **statement counts differ:** {sorted(b.counts)} -> {sorted(n.counts)}", True)
        # Steps: times under Time, peaks under Memory with their kind, the store under Disk.
        for metric in sorted(set(b.steps) & set(n.steps)):
            sb, sn = estimate(b.steps[metric]), estimate(n.steps[metric])
            if not (sb and sn and sb[0] > 0):
                continue
            v = verdict(sb, sn, threshold, min_ms if metric.endswith("ms") else 0)
            if metric.endswith("MiB"):
                v = {"slower": "larger", "faster": "smaller"}.get(v, v)
            mark = f", **{v}**" if v != "=" else ""
            line = f"{fmt(sb[0])} -> {fmt(sn[0])} ({sn[0] / sb[0]:.2f}x{mark})"
            worse = v in ("slower", "larger")
            if metric == "store MiB":
                out.add("Disk", f"- {title}: store MiB {line}", worse)
            elif metric.endswith("peak MiB"):
                task = metric.split()[0]
                kb = kinds[0].get((*key, task), "unknown")
                kn = kinds[1].get((*key, task), "unknown")
                if CACHE_KINDS & {kb, kn}:
                    out.add("Memory, by kind", f"- {title}: {task} peak MiB ({kb} -> {kn}): counts page cache, "
                            "recorded, not compared", False)
                elif "unknown" not in (kb, kn) and kb != kn:
                    out.add("Memory, by kind", f"- {title}: {task} peak MiB ({kb} -> {kn}): other kinds, not compared",
                            True)
                else:
                    out.add("Memory, by kind", f"- {title}: {task} peak MiB ({kn if kn != 'unknown' else kb}) {line}",
                            worse)
            else:
                out.add("Time", f"- {title}: {metric} {line}", worse)
        for cache in sorted(set(b.items) | set(n.items)):
            bi, ni = b.items.get(cache, {}), n.items.get(cache, {})
            label = "" if cache == "-" else f" (cache {cache})"
            missing = sorted(set(bi) - set(ni))
            if missing:
                out.add("Missing", f"- {title}: **lost queries{label}:** {', '.join(missing)}", True)
            changed, ratios, sums = [], [], [0.0, 0.0]
            for name in sorted(set(bi) & set(ni)):
                ib, inn = bi[name], ni[name]
                if ib.answers and inn.answers and ib.answers != inn.answers:
                    out.add("Answers and completeness", f"- {title}: **{name}{label}: answers differ** "
                            f"{'/'.join(sorted(ib.answers))} -> {'/'.join(sorted(inn.answers))}", True)
                if inn.wrong:
                    out.add("Answers and completeness", f"- {title}: **{name}{label}: wrong answer** ({inn.problems()})",
                            True)
                elif ib.complete and not inn.complete:
                    out.add("Answers and completeness",
                            f"- {title}: **{name}{label}: was complete, now {inn.problems()}**", True)
                if name in n.disputed:
                    out.add("Answers and completeness",
                            f"- {title}: {name}{label}: answers differ between systems in the new run (disputed)")
                if not (eligible(ib, name, b) and eligible(inn, name, n)):
                    totals["not comparable"] += 1
                    continue
                # An answer the contract doesn't time earns no time, in either run: wrong,
                # unchecked or another fault, also where the verdict didn't change (T8 PR #56).
                answer = {run: found.of(key[0], key[1], pair.regime, name, key[2])
                          for run, pair, found in (("base", b, answers[0]), ("new", n, answers[1]))}
                untimed = [f"{a} in the {run} run" for run, a in answer.items() if not answer_timed(a)]
                if untimed:
                    out.add("Answers and completeness", f"- {title}: {name}{label}: not timed, its answer "
                            + " and ".join(untimed))
                    totals["not comparable"] += 1
                    continue
                sb, sn = ib.estimate(), inn.estimate()
                if not (sb and sn):
                    totals["not comparable"] += 1
                    continue
                sums[0] += sb[0]
                sums[1] += sn[0]
                if sb[0] > 0 and sn[0] > 0:
                    ratios.append(sn[0] / sb[0])
                v = verdict(sb, sn, threshold, min_ms)
                totals[v] += 1
                if v != "=":
                    changed.append((sn[0] / sb[0], name, sb, sn, v))
            if ratios:
                geo = math.exp(sum(math.log(r) for r in ratios) / len(ratios))
                out.add("Time", f"- {title}: queries{label}: sum of medians {fmt(sums[0])} -> {fmt(sums[1])} ms "
                        f"({sums[1] / sums[0] if sums[0] else float('nan'):.2f}x), geometric mean of ratios "
                        f"{geo:.2f}x over {len(ratios)} comparable queries of {len(set(bi) & set(ni))}")
            for ratio, name, sb, sn, v in sorted(changed, reverse=True):
                noise = "/".join("?" if x is None else f"{x:.0%}" for x in (sb[1], sn[1]))
                out.add("Time", f"  - {name}{label}: {fmt(sb[0])} -> {fmt(sn[0])} ms ({ratio:.2f}x, {v}; noise {noise})",
                        v == "slower")
    # Each system's answers against its reference (answers.json), where both runs have them.
    recorded = [found.verdicts() for found in answers]
    for k in sorted(set(recorded[0]) & set(recorded[1])):
        before, after = recorded[0][k], recorded[1][k]
        if before != after:
            worse = before == "ok" and after != "ok"
            out.add("Answers and completeness", f"- {k[4]} {k[0]} {k[1]} {k[3]}: answer verdict {before} -> {after}"
                    + ("  WORSE" if worse else ""), worse)
    return totals


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(prog="suite.py compare", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("base", help="baseline: a fast or perf-lab report (.json), or suite results (directories or CSV "
                                "files, comma-separated)")
    p.add_argument("new", help="the run to check, same form")
    p.add_argument("--systems", help="suite results: comma-separated systems (default: all in both)")
    p.add_argument("--threshold", type=float, default=0.10, help="relative change that counts (default 0.10)")
    p.add_argument("--min-ms", type=float, default=2.0, help="absolute change that counts, ms (default 2)")
    p.add_argument("--include-restricted", action="store_true")
    p.add_argument("--allow-host-change", action="store_true",
                   help="compare runs of different hosts or host configurations anyway, flagged")
    args = p.parse_args(argv)
    if args.base.endswith(".json") and args.new.endswith(".json"):
        return compare_reports(load_report(Path(args.base)), load_report(Path(args.new)), args.min_ms,
                               args.allow_host_change)
    systems = set(args.systems.split(",")) if args.systems else None
    base_files, new_files = files(args.base), files(args.new)
    refused = host_check(manifest_identities(base_files), manifest_identities(new_files), args.allow_host_change)
    if refused:
        return refused
    base = collect(base_files, systems, args.include_restricted)
    new = collect(new_files, systems, args.include_restricted)
    print(f"# {args.new} against {args.base}\n")
    print(f"threshold {args.threshold:.0%} or the measured noise, whichever is larger; at least {args.min_ms} ms; "
          "only complete, correct, undisputed queries enter the speed verdict\n")
    out = Out()
    totals = compare_pairs(base, new, args.threshold, args.min_ms, out,
                           (memory_kinds(base_files), memory_kinds(new_files)), (verdicts(base_files), verdicts(new_files)))
    out.print()
    print(f"**Queries:** {totals['slower']} slower, {totals['faster']} faster, {totals['=']} within noise, "
          f"{totals['not comparable']} not comparable (incomplete, wrong, disputed or without timings).")
    print(f"**{out.regressions} regression(s)** (lost work, changed answers, worse beyond noise).")
    return 1 if out.regressions else 0
