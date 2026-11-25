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
from test_release_gate import CONTEXT, HARNESS, REVS, gate_module, gatestats, passes, record, run_gate  # noqa: E402

CASE = "lab-olympics"
MEASURED = f"base|{CASE}|time/q06"
MISSING = f"base|{CASE}|memory/live"
ANON = f"base|{CASE}|memory/anon_kernel"
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


def summary(contrasts, family=(MEASURED,), floor=0.0, baseline=10.123, **kw):
    return run_gate(measured_passes(contrasts, baseline), sides=SIDES, require=("base",),
                    family=list(family) if family is not None else None, **kw,
                    floor={"source": "test floor", "cases": {CASE: {"time/q06": floor}}} if floor else None)


def lines_for(report, name=MEASURED):
    return report.get("lines", {}).get(name, {})


def sampled_p(rng, loss=0.0, floor=0.0):
    reading = gatestats.loss_test([rng.gauss(loss, SIGMA) for _ in range(UNITS)], floor=floor)
    return gatestats.landing_lines(reading, len(FAMILY), reading["p"])["p"]


def three_se(rate):
    return 3 * math.sqrt(rate * (1 - rate) / TRIALS)


def reference_power(loss_percent):
    """Midpoint integration over sample sd / true sd for the review's eight-unit case."""
    df = 7
    critical = gatestats.t_ppf(1 - 0.045 / 15, df)
    shift = math.log1p(loss_percent / 100) * math.sqrt(8) / 0.025
    denominator, numerator = 0.0, 0.0
    for i in range(20_000):
        sd = (i + 0.5) / 5_000
        mass = sd ** (df - 1) * math.exp(-df * sd * sd / 2)
        denominator += mass
        numerator += mass * math.erfc((critical * max(sd, 0.75) - shift) / math.sqrt(2)) / 2
    return numerator / denominator


class LandingWords(unittest.TestCase):
    # Spec: landing timing design §3 D1–D2 says 15 independent normal null dimensions,
    # eight blocks each -> at most 10 % screened runs and 5 % confirmed runs.
    @patch.object(gatestats, "sign_flip_p", new=lambda _: None)  # an ancillary diagnostic, unused by the screen
    def test_normal_null_rates(self):
        rng = random.Random(61723)
        first_words, paired_words = Counter(), Counter()
        for _ in range(TRIALS):
            first = {h: sampled_p(rng) for h in FAMILY}
            second = {h: sampled_p(rng) for h in FAMILY}
            first_words[gatestats.landing_words(FAMILY, first)["word"]] += 1
            paired_words[gatestats.landing_words(FAMILY, first, second)["word"]] += 1

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
        self.assertEqual(gatestats.landing_words(family, first, second)["confirmed"], family[:-1])

    # Spec: fix pass 1 F1 and design D1–D2 say the catalogue's planned dimensions
    # include unread memory and set the screen cutoff before measurements arrive.
    def test_catalogue_family_keeps_the_unread_memory_and_screen_cutoff(self):
        contrasts = [0.011 + 0.004 * i for i in (-4, -3, -2, -1, 1, 2, 3, 4)]
        series = [f"q{i:02}" for i in range(6, 20)]
        catalogue = [{"name": CASE, "metric": "fixture", "sweep": [[s] for s in series]}]
        report = summary(contrasts, family=None, catalog=catalogue)
        with self.subTest(answer="unread memory"):
            reading = lines_for(report, ANON)
            self.assertEqual(reading.get("clearance"), "not resolved: not measured")
            self.assertEqual(reading.get("p"), 1)
            self.assertEqual(set(report["lines"]), {ANON, *(f"base|{CASE}|time/{s}" for s in series)})
        with self.subTest(answer="screen cutoff"):
            self.assertEqual(report.get("word"), "cleared")
            self.assertEqual(summary(contrasts)["word"], "needs a second round")

    # Spec: fix pass 1 F1 says an unavailable plan family leaves the report's opening
    # and fields in their legacy form.
    def test_without_a_plan_the_report_keeps_its_legacy_opening(self):
        report = summary([0.05 + i * 0.001 for i in range(UNITS)], family=None)
        text = gate_module.render(report)
        self.assertTrue(text.startswith("# Release gate round "), text.splitlines()[0])
        self.assertIn("## Flagged: a loss shown this round, a confirmation due", text)
        self.assertNotIn("word", report)

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

    # Spec: fix pass 1 F2 and design D1 say a quantized null inside one counter step
    # receives neither a screen nor clearance, even with four distinct values.
    def test_a_quantized_null_below_one_step_cannot_screen(self):
        offsets = (0, 0, 0, 0, 1, 1, 2, 3)  # a one-sided draw from a zero-centred, whole-step counter
        report = summary([math.log1p(x / 100) for x in offsets], baseline=100)
        self.assertEqual(report.get("screened"), [])
        self.assertEqual(lines_for(report).get("clearance"), "not resolved to 3 % (below the counter's step)")

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
        # The report has already selected the loss. Skip recomputing ancillary diagnostics
        # for every simulated sample while running the actual test and eligibility path.
        with patch.object(gatestats, "sign_flip_p", new=lambda _: None), \
                patch.object(gatestats, "_sensitivity_shift", new=lambda *_: 0.0):
            for _ in range(TRIALS):
                first = {MEASURED: sampled_p(rng, loss, floor=SIGMA)}
                second = {MEASURED: sampled_p(rng, loss, floor=SIGMA)}
                wins += gatestats.landing_words(FAMILY, first, second)["word"] == "confirmed loss"
        rate = wins / TRIALS
        print(f"Sensitivity {loss_percent:.2f} % loss, confirmed {rate:.2%}, stated floor 80 %, "
              f"with 3 SE {0.80 - three_se(0.80):.2%}", flush=True)
        self.assertGreaterEqual(rate, 0.80 - three_se(0.80))

    # Spec: fix pass 1 F5 and design D4 say the clamped eight-unit, 15-dimension
    # report's smallest displayed sensitivity must reach 90 % per-round power.
    def test_clamped_sensitivity_reaches_ninety_percent(self):
        offsets = (-4, -3, -2, -1, 1, 2, 3, 4)
        scale = 0.025 * math.sqrt(7 / 60)
        reading = gatestats.loss_test([0.05 + scale * x for x in offsets], floor=0.75 * 0.025)
        line = gatestats.landing_lines(reading, 15, reading["p"])["sensitivity"]
        percent = float(line.split()[0])
        power = reference_power(percent)
        print(f"Clamped sensitivity {percent:.2f} % loss, per-round power {power:.4%}, target 90 %", flush=True)
        self.assertGreaterEqual(power, 0.90)
        self.assertLess(reference_power(percent - 0.01), 0.90)


class LandingReport(unittest.TestCase):
    def report(self, root, contrasts, round_number=0, first=None, reuse_pass=None, fleet_host=None):
        passes_dir = root / "passes"
        passes_dir.mkdir(parents=True, exist_ok=True)
        for p in measured_passes(contrasts):
            p["report"]["timing_inputs"] = {"overlay": None, "floor": None}
            if fleet_host is None:
                p["report"]["machine"].pop("fleet_host")
            elif p["n"] == 1:
                p["report"]["machine"]["fleet_host"] = fleet_host
            p["report"]["machine"]["overlay"] = None
            for row in [p["report"], *p["report"]["records"]]:
                for key in ("started_ns", "finished_ns"):
                    row[key] += round_number * 1_000_000
            path = passes_dir / p["path"]
            reading = json.loads(reuse_pass.read_bytes()) if reuse_pass and path.name == reuse_pass.name else p["report"]
            path.write_text(json.dumps(reading, indent=2 if reuse_pass else None), encoding="utf-8")
        family = root / "plan.json"
        family.write_text(json.dumps({"family": FAMILY}), encoding="utf-8")
        md, data = root / "release-gate.md", root / "release-gate.json"
        argv = ["release_gate.py", "report", "--passes", str(passes_dir), "--candidate", "cand",
                "--side", f"cand={REVS['cand']}", "--side", f"base={REVS['base']}", "--require", "base",
                "--harness-revision", HARNESS, "--context", f"round={round_number}", "--family", str(family),
                "--out", str(md), "--json", str(data)]
        if first is not None:
            argv += ["--round-one", str(first)]
        if fleet_host is not None:
            argv += ["--context", f"host={CONTEXT['host']}"]
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

    # Spec: fix pass 1 F3 and design D3 say even one reused pass makes a pair
    # ineligible for confirmation, including a copy written with different formatting.
    def test_a_reused_pass_refuses_the_pair(self):
        loss = [0.05 + i * 0.001 for i in range(UNITS)]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            first = root / "first"
            self.report(first, loss)
            reused = min((first / "passes").glob("pass-*.json"))
            _, error = self.report(root / "second", loss, round_number=1, first=first / "release-gate.json",
                                   reuse_pass=reused)
            self.assertIn("share a pass", str(error))
            self.assertFalse((root / "second" / "release-gate.json").exists())

    # Spec: fix pass 1 F4 and design D3 say a wrong fleet host on either round
    # refuses the pair, even when both rounds name the same host in their context.
    def test_a_host_provenance_error_in_either_round_refuses_the_pair(self):
        loss = [0.05 + i * 0.001 for i in range(UNITS)]
        for wrong in (0, 1):
            with self.subTest(round=wrong + 1), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                hosts = [CONTEXT["host"]] * 2
                hosts[wrong] = "other-host"
                first = root / "first"
                self.report(first, loss, fleet_host=hosts[0])
                _, error = self.report(root / "second", loss, round_number=1, first=first / "release-gate.json",
                                       fleet_host=hosts[1])
                self.assertIn(f"round {wrong + 1} has a host provenance error", str(error))
                self.assertFalse((root / "second" / "release-gate.json").exists())


if __name__ == "__main__":
    unittest.main()
