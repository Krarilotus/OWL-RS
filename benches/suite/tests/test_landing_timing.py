"""The after-landing word and resolution lines, independently of the release verdict."""
import contextlib
import io
import json
import math
import random
import sys
import tempfile
import unittest
from collections import Counter
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_release_gate import HARNESS, REVS, gate_module, gatestats, passes, record, run_gate  # noqa: E402

CASE = "lab-olympics"
MEASURED = f"base|{CASE}|time/q06"
MISSING = f"base|{CASE}|memory/live"
FAMILY = [MEASURED, MISSING, *(f"base|{CASE}|time/q{i:02}" for i in range(7, 20))]
SIDES = ("base", "cand")
TRIALS = 2000
UNITS = 8
SIGMA = 0.01


def measured_passes(contrasts, baseline=10.123):
    def records_of(side, n):
        contrast = contrasts[(n - 1) // (2 * len(SIDES))] if side == "cand" else 0
        return [record(samples={"q06": [baseline * math.exp(contrast)]})]

    return passes(records_of, blocks=len(contrasts), sides=SIDES)


def summary(contrasts, family=(MEASURED,), floor=0.0, baseline=10.123):
    return run_gate(measured_passes(contrasts, baseline), sides=SIDES, require=("base",), family=list(family),
                    floor={"source": "test floor", "cases": {CASE: {"time/q06": floor}}} if floor else None)


def lines_for(report, name=MEASURED):
    return report.get("lines", {}).get(name, {})


def word_for(family, first, second=None):
    report = getattr(gatestats, "landing_words", None)
    if report is not None:
        return report(family, first, second)
    # On the base tree, observe the old round decision so the red check reaches the answer.
    rounds = [{"family": family, "pvalues": first}]
    if second is not None:
        rounds.append({"family": family, "pvalues": second})
    return gate_module.decide([rounds])


def sampled_p(rng, loss=0.0, floor=0.0):
    # Generate the actual block contrasts independently of the sensitivity simulation.
    blocks = [rng.gauss(loss, SIGMA) for _ in range(UNITS)]
    mean = sum(blocks) / UNITS
    sampled_sd = math.sqrt(sum((x - mean) ** 2 for x in blocks) / (UNITS - 1))
    return gatestats.t_sf(mean * math.sqrt(UNITS) / max(sampled_sd, floor), UNITS - 1)


def three_se(rate):
    return 3 * math.sqrt(rate * (1 - rate) / TRIALS)


class LandingWords(unittest.TestCase):
    # Spec: landing timing design §3 D1–D2 says 15 independent normal null dimensions,
    # eight blocks each -> at most 10 % screened runs and 5 % confirmed runs.
    def test_normal_null_rates(self):
        rng = random.Random(61723)
        first_words, paired_words = Counter(), Counter()
        for _ in range(TRIALS):
            first = {h: sampled_p(rng) for h in FAMILY}
            second = {h: sampled_p(rng) for h in FAMILY}
            first_words[word_for(FAMILY, first).get("word")] += 1
            paired_words[word_for(FAMILY, first, second).get("word")] += 1

        with self.subTest(round="first"):
            self.assertEqual(first_words["cleared"] + first_words["needs a second round"], TRIALS, first_words)
            rate = first_words["needs a second round"] / TRIALS
            print(f"Normal null screen {rate:.2%}, cap 10 %, with 3 SE {0.10 + three_se(0.10):.2%}", flush=True)
            self.assertLessEqual(rate, 0.10 + three_se(0.10))
        with self.subTest(round="paired"):
            self.assertEqual(paired_words["cleared"] + paired_words["confirmed loss"], TRIALS, paired_words)
            rate = paired_words["confirmed loss"] / TRIALS
            print(f"Normal null confirmation {rate:.2%}, cap 5 %, with 3 SE {0.05 + three_se(0.05):.2%}", flush=True)
            self.assertLessEqual(rate, 0.05 + three_se(0.05))

    # Spec: landing timing design §3 D2 says a member above the round-one screen gets
    # p2 = 1 -> even a tiny second p cannot confirm it in a later Holm step.
    def test_only_screened_members_can_confirm(self):
        family = FAMILY[:5]
        first = dict.fromkeys(family, 0.001)
        first[family[-1]] = 0.023
        second = dict.fromkeys(family, 0.001)
        self.assertEqual(word_for(family, first, second).get("confirmed"), family[:-1])

    # Spec: landing timing design §3 D1 and D4 say a planned dimension without readings
    # stays in the family with p = 1 and "not resolved: not measured".
    def test_missing_reading(self):
        reading = lines_for(summary([0.01] * UNITS, FAMILY), MISSING)
        self.assertEqual(reading.get("clearance"), "not resolved: not measured")
        self.assertEqual(reading.get("p"), 1)

    # Spec: landing timing design §3 D4 says one unit has no clearance measurement.
    def test_one_unit(self):
        self.assertEqual(lines_for(summary([0.01])).get("clearance"), "not resolved: not measured")

    # Spec: landing timing design §3 D1 says a difference within the counter's step
    # -> "not resolved to 3 % (below the counter's step)".
    def test_below_the_step(self):
        contrasts = [math.log1p(offset / 10.123) for offset in (-0.0004, 0, 0.0004, 0.0008)] * 2
        self.assertEqual(lines_for(summary(contrasts)).get("clearance"),
                         "not resolved to 3 % (below the counter's step)")

    # Spec: landing timing design §3 D1 says fewer than four distinct unit values
    # -> no screen and "not resolved to 3 % (discrete counter)".
    def test_discrete_counter(self):
        contrasts = [math.log1p(x / 100) for x in (5, 6, 7, 5, 6, 7, 5, 6)]
        report = summary(contrasts, baseline=100)
        self.assertEqual(lines_for(report).get("clearance"), "not resolved to 3 % (discrete counter)")
        self.assertEqual(report["word"], "cleared")
        self.assertEqual(report["screened"], [])

    # Spec: landing timing design §3 D4 says all-equal units without a floor
    # -> "not resolved: no spread".
    def test_no_spread(self):
        self.assertEqual(lines_for(summary([0.05] * UNITS)).get("clearance"), "not resolved: no spread")

    # Spec: landing timing design §3 D4 says clearance uses the one-sided t upper
    # bound on the units' own df and the clamped spread, with 3 % as the precision bar.
    def test_clearance_bound(self):
        offsets = (-0.003, -0.002, -0.001, 0, 0, 0.001, 0.002, 0.003)
        # The offsets have zero mean and their spread is below 0.02. t(7, 0.95) is 1.89458.
        for mean, label in [(0.005, "no loss above"), (0.018, "not resolved to 3 %")]:
            with self.subTest(mean=mean):
                line = lines_for(summary([mean + x for x in offsets], floor=0.02)).get("clearance", "")
                self.assertTrue(line.startswith(label), line)
                bound = 100 * math.expm1(mean + 1.89458 * 0.02 / math.sqrt(8))
                reading = line.split("(")[1] if "(" in line else line.removeprefix("no loss above ")
                self.assertAlmostEqual(float(reading.split()[0]), bound, delta=0.011)

    # Spec: landing timing design §3 D4 says a loss at the reported sensitivity, with the
    # floor clamped in each round -> at least 90 % power per round and 80 % over two.
    def test_sensitivity_holds_over_independent_rounds(self):
        offsets = (-0.008, -0.006, -0.004, -0.002, 0.002, 0.004, 0.006, 0.008)
        line = lines_for(summary([0.01 + x for x in offsets], FAMILY, floor=SIGMA)).get("sensitivity", "")
        self.assertIn("two-round power ≥ 80 %", line)
        loss_percent = float(line.split()[0])
        rng = random.Random(90070)
        loss = math.log1p(loss_percent / 100)
        wins = 0
        for _ in range(TRIALS):
            first = {MEASURED: sampled_p(rng, loss, floor=SIGMA)}
            second = {MEASURED: sampled_p(rng, loss, floor=SIGMA)}
            wins += word_for(FAMILY, first, second).get("word") == "confirmed loss"
        rate = wins / TRIALS
        print(f"Sensitivity {loss_percent:.2f} % loss, confirmed {rate:.2%}, stated floor 80 %, "
              f"with 3 SE {0.80 - three_se(0.80):.2%}", flush=True)
        self.assertGreaterEqual(rate, 0.80 - three_se(0.80))


class LandingReport(unittest.TestCase):
    def report(self, root, contrasts, round_number=0, first=None):
        passes_dir = root / "passes"
        passes_dir.mkdir(parents=True, exist_ok=True)
        for p in measured_passes(contrasts):
            p["report"]["timing_inputs"] = {"overlay": None, "floor": None}
            p["report"]["machine"].pop("fleet_host")
            p["report"]["machine"]["overlay"] = None
            (passes_dir / p["path"]).write_text(json.dumps(p["report"]), encoding="utf-8")
        family = root / "plan.json"
        family.write_text(json.dumps({"family": FAMILY}), encoding="utf-8")
        md, data = root / "release-gate.md", root / "release-gate.json"
        argv = ["release_gate.py", "report", "--passes", str(passes_dir), "--candidate", "cand",
                "--side", f"cand={REVS['cand']}", "--side", f"base={REVS['base']}", "--require", "base",
                "--harness-revision", HARNESS, "--context", f"round={round_number}", "--family", str(family),
                "--out", str(md), "--json", str(data)]
        if first is not None:
            argv += ["--round-one", str(first)]
        out, err = io.StringIO(), io.StringIO()
        with patch.object(sys, "argv", argv), contextlib.redirect_stdout(out), contextlib.redirect_stderr(err), \
                patch.object(out, "reconfigure", create=True):
            try:
                gate_module.main()
            except SystemExit as exit_:
                return str(exit_), err.getvalue()
        return md.read_text(encoding="utf-8"), json.loads(data.read_text(encoding="utf-8"))

    # Spec: landing timing design §3 D2 says the Markdown opens with the word and its
    # measured dimensions -> a screened loss names the dimension with its ratio and interval.
    def test_report_opens_with_the_word(self):
        with tempfile.TemporaryDirectory() as tmp:
            md, data = self.report(Path(tmp), [0.05 + i * 0.001 for i in range(UNITS)])
        first, reading = md.splitlines()[0], md.splitlines()[2]
        self.assertEqual(first.removeprefix("# "), "needs a second round")
        self.assertIn("time/q06", reading)
        self.assertIn("ratio", reading)
        self.assertIn("interval", reading)
        self.assertEqual(data["screened"], [MEASURED])
        self.assertIn("## nominal p below 0.05: not findings", md)

    # Spec: landing timing design §3 D2–D3 says a second strong reading on the same
    # pair confirms; the slice brief requires refusal for a different candidate, base, host or family.
    def test_second_report_requires_the_same_pair_and_family(self):
        loss = [0.05 + i * 0.001 for i in range(UNITS)]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _, first = self.report(root / "first", loss)
            path = root / "first" / "release-gate.json"
            md, paired = self.report(root / "paired", loss, round_number=1, first=path)
            self.assertEqual(md.splitlines()[0].removeprefix("# "), "confirmed loss")
            self.assertEqual(paired["confirmed"], [MEASURED])
            for field, other in {
                "candidate": {"name": "cand", "revision": "d" * 40},
                "baselines": {"base": "a" * 40},
                "host": ["other-host"],
                "family": FAMILY[:-1],
            }.items():
                with self.subTest(field=field):
                    path.write_text(json.dumps({**first, field: other}), encoding="utf-8")
                    _, error = self.report(root / ("other-" + field), loss, round_number=1, first=path)
                    self.assertIn("round 1", error)
                    self.assertIn("does not match", error)
                    self.assertFalse((root / ("other-" + field) / "release-gate.json").exists())


if __name__ == "__main__":
    unittest.main()
