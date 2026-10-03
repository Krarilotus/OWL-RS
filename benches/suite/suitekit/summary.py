"""The one interpretation of a suite results CSV (PROTOCOL.md §2 and §5): what counts as
complete, which timings are eligible, and the estimator. `report`, `compare` and `ledger`
present it for their readers and decide nothing of their own.

- **Complete.** A query item is complete when every execution the run asked for came back
  `ok`: a timeout or failure in any repetition makes it incomplete, even if another
  repetition succeeded. A pair is complete when every repetition loaded and every item is
  complete.
- **Eligible.** Only `ok` executions count as timings. A wrong answer is a correctness
  finding, never a time; failures and timeouts aren't times either.
- **Estimator.** Per query: the median of the eligible measured repeats of each run, then
  the median over the runs; noise is the spread of those run medians relative to their
  median, with one run the interquartile range of its repeats (`None` with fewer than four).
  The first execution on a fresh server (`repeat` 0) is estimated apart, the same way.
- **Disputed.** An item on which systems of one regime report different answer counts
  (`ok` rows) is disputed until adjudicated: which system is wrong isn't known, so neither
  timing is eligible for a speed claim, and every reader shows it. An adjudication
  (`benches/suite/adjudications.tsv`: the correct count and the evidence) settles it: the
  systems with that count are undisputed, the others' executions count as wrong.
"""
from __future__ import annotations

import csv
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path
from statistics import median, quantiles

ADJUDICATIONS = Path(__file__).resolve().parent.parent / "adjudications.tsv"

FAILED = ("failed", "timeout")


def number(text) -> float | None:
    try:
        return float(text)
    except (TypeError, ValueError):
        return None


def estimate(per_run: dict[str, list[float]]) -> tuple[float, float | None] | None:
    """(median over runs of the per-run medians, relative noise); `None` without values."""
    medians = [median(v) for v in per_run.values() if v]
    if not medians:
        return None
    m = median(medians)
    if m <= 0:
        return m, 0.0
    if len(medians) >= 2:
        return m, (max(medians) - min(medians)) / m
    only = next(v for v in per_run.values() if v)
    if len(only) >= 4:
        q = quantiles(only, n=4)
        return m, (q[2] - q[0]) / m
    return m, None


@dataclass
class Item:
    """One query of one pair under one cache mode."""
    statuses: Counter = field(default_factory=Counter)
    repeats: dict = field(default_factory=lambda: defaultdict(list))  # run -> eligible measured ms
    first: dict = field(default_factory=lambda: defaultdict(list))  # run -> eligible first ms
    answers: set = field(default_factory=set)  # answer counts of ok rows
    wrong_answers: set = field(default_factory=set)

    @property
    def complete(self) -> bool:
        return bool(self.statuses) and set(self.statuses) == {"ok"}

    @property
    def wrong(self) -> bool:
        return self.statuses["wrong"] > 0

    def estimate(self) -> tuple[float, float | None] | None:
        return estimate(self.repeats)

    def estimate_first(self) -> tuple[float, float | None] | None:
        return estimate(self.first)

    def problems(self) -> str:
        """What made it incomplete: `1 timeout of 12`."""
        total = sum(self.statuses.values())
        bad = [f"{n} {s}" for s, n in sorted(self.statuses.items()) if s != "ok"]
        return f"{', '.join(bad)} of {total}" if bad else ""


@dataclass
class Pair:
    """One workload tier on one system in one regime."""
    workload: str
    tier: str
    system: str
    regime: str = "-"
    publish: str = "free"
    skipped_note: str = ""
    loads: dict = field(default_factory=dict)  # run -> (status, note)
    step_runs: set = field(default_factory=set)  # runs with an ok step (workloads without a load)
    other: Counter = field(default_factory=Counter)  # (task, status) of further steps
    steps: dict = field(default_factory=lambda: defaultdict(lambda: defaultdict(list)))  # metric -> run -> values
    counts: set = field(default_factory=set)  # (task, rows) of load, reason, count
    items: dict = field(default_factory=lambda: defaultdict(lambda: defaultdict(Item)))  # cache -> item -> Item
    disputed: set = field(default_factory=set)  # items with answer counts that differ across systems

    @property
    def key(self) -> tuple:
        return (self.workload, self.tier, self.system)

    @property
    def live(self) -> bool:
        return bool(self.loads) or bool(self.items) or bool(self.other)

    def ok_runs(self) -> list[str]:
        return [run for run, (status, _) in self.loads.items() if status == "ok"]

    def all_items(self) -> dict[str, list[Item]]:
        """item -> its Items over the cache modes."""
        out: dict[str, list[Item]] = defaultdict(list)
        for by_item in self.items.values():
            for name, item in by_item.items():
                out[name].append(item)
        return out

    def outcome(self) -> dict:
        """The pair's outcome, as the ledger records it (runs/README.md)."""
        if not self.live:
            return {"regime": self.regime, "outcome": "skipped", "note": self.skipped_note[:160]}
        if self.publish == "permission":
            return {"regime": self.regime, "outcome": "restricted",
                    "note": "ran; results stay local until the vendor permits publishing"}
        result: dict = {"regime": self.regime,
                        "runs": len(self.ok_runs()) if self.loads else len(self.step_runs)}
        items = self.all_items()
        complete = [name for name, its in items.items() if all(i.complete for i in its)]
        if items:
            result["items"] = f"{len(complete)}/{len(items)}"
        if self.loads and not self.ok_runs():
            statuses = {s for s, _ in self.loads.values()}
            result["outcome"] = "timeout" if statuses == {"timeout"} else "failed"
            note = next((n for s, n in self.loads.values() if s != "ok" and n), "")
            if note:
                result["note"] = note[:160]
            return result
        wrong = sorted(name for name, its in items.items() if any(i.wrong for i in its))
        incomplete = sorted(name for name in items if name not in complete and name not in wrong)
        failed_runs = [run for run, (status, _) in self.loads.items() if status != "ok"]
        failed_steps = sorted({f"{task} {status}" for (task, status), n in self.other.items() if status != "ok"})
        notes = []
        if wrong:
            result["outcome"] = "wrong"
            notes.append("wrong answer counts: " + ", ".join(wrong))
        elif incomplete or failed_runs or failed_steps:
            result["outcome"] = "partial"
        elif self.disputed:
            result["outcome"] = "disputed"
        else:
            result["outcome"] = "ok"
        if incomplete:
            first = incomplete[0]
            how = "; ".join(i.problems() for i in items[first] if i.problems())
            notes.append(f"incomplete: {', '.join(incomplete)} ({first}: {how})")
        if failed_runs:
            notes.append(f"{len(failed_runs)} of {len(self.loads)} loads not ok")
        if failed_steps:
            notes.append("steps: " + ", ".join(failed_steps))
        if self.disputed:
            notes.append("answers differ from other systems': " + ", ".join(sorted(self.disputed)))
        if notes:
            result["note"] = "; ".join(notes)[:200]
        return result


def adjudications(path: Path = ADJUDICATIONS) -> dict[tuple, str]:
    """(workload, tier, item) -> the correct answer count, from the adjudications file."""
    if not path.exists():
        return {}
    lines = [line for line in path.read_text(encoding="utf-8").splitlines()
             if line.strip() and not line.startswith("#")]
    return {(r[0], r[1], r[2]): r[3] for r in csv.reader(lines, delimiter="	") if len(r) >= 4}


def summarise(rows: list[dict], source: str = "", settled: dict[tuple, str] | None = None) -> dict[tuple, Pair]:
    """(workload, tier, system) -> Pair, from result rows; `source` separates the runs of
    several files read together."""
    pairs: dict[tuple, Pair] = {}
    for r in rows:
        key = (r["workload"], r["tier"], r["system"])
        pair = pairs.get(key)
        if pair is None:
            pair = pairs[key] = Pair(r["workload"], r["tier"], r["system"], publish=r["publish"])
        if r["regime"] not in ("", "-"):
            pair.regime = r["regime"]
        if r["status"] == "skipped":
            pair.skipped_note = pair.skipped_note or r["note"]
            continue
        run = f"{source}:{r['run']}"
        task, status, ms = r["task"], r["status"], number(r["ms"])
        if task == "query":
            item = pair.items[r.get("cache") or "-"][r["item"]]
            item.statuses[status] += 1
            if status == "ok" and ms is not None:
                (item.first if str(r["repeat"]) == "0" else item.repeats)[run].append(ms)
            if r["rows"] != "":
                (item.answers if status == "ok" else item.wrong_answers if status == "wrong" else set()).add(r["rows"])
            continue
        if task in ("load", "conformance"):
            pair.loads[run] = (status, r["note"])
        else:
            pair.other[(task, status)] += 1
        if status != "ok":
            continue
        pair.step_runs.add(run)
        if ms is not None and task in ("load", "reason", "restart", "count"):
            pair.steps[f"{task} ms"][run].append(ms)
        if task in ("load", "serve") and number(r["peak_mib"]) is not None:
            pair.steps[f"{task} peak MiB"][run].append(number(r["peak_mib"]))
        if task == "size" and number(r["bytes"]) is not None:
            pair.steps["store MiB"][run].append(number(r["bytes"]) / 2**20)
        if task in ("load", "reason", "count") and r["rows"] != "":
            pair.counts.add((task, r["rows"]))
    mark_disputes(pairs, adjudications() if settled is None else settled)
    return pairs


def mark_disputes(pairs: dict[tuple, Pair], settled: dict[tuple, str]):
    """Items whose `ok` answer counts differ between the systems of one workload tier and
    regime: each such pair gets the item in `disputed`, unless `settled` gives the correct
    count; then a pair with another count has its executions counted as wrong."""
    seen: dict[tuple, dict[str, set]] = defaultdict(lambda: defaultdict(set))
    for pair in pairs.values():
        for by_item in pair.items.values():
            for name, item in by_item.items():
                seen[(pair.workload, pair.tier, pair.regime, name)][pair.system] |= item.answers
    for (workload, tier, regime, name), by_system in seen.items():
        values = {v for answers in by_system.values() for v in answers}
        if len(values) <= 1:
            continue
        correct = settled.get((workload, tier, name))
        for system, answers in by_system.items():
            pair = pairs.get((workload, tier, system))
            if pair is None:
                continue
            if correct is None:
                pair.disputed.add(name)
            elif answers - {correct}:
                for item in (by_item[name] for by_item in pair.items.values() if name in by_item):
                    item.statuses["wrong"] += item.statuses.pop("ok", 0)
                    item.wrong_answers |= item.answers - {correct}
                    item.answers &= {correct}
                    item.repeats.clear()
                    item.first.clear()
