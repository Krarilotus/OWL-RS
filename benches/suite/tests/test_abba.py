"""The targeted A-B-B-A (benches/suite/abba.py): a family declared once, its rounds on the gate's
blocks (A A' B B A' A), fail-closed completeness, the A-A' arm, the verdict's exit codes.

20 Oct 2026: three agents wrote their own A-B-B-A scripts, and each failed open on missing runs,
had no interleaved A-A' arm, or checked counts instead of content.
"""
from __future__ import annotations

import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import abba  # noqa: E402
import test_release_gate as fixture  # noqa: E402  (the gate's pass fixtures)

A, B = "a" * 40, "b" * 40
CHECKS = next(c for c in abba.cases_registry.load() if c["name"] == "lab-olympics")["check"]
fixture.REVS.update(a=A, a2=A, b=B)
SERIES = ("q01-point-lookup", "q06-text-filter")
FAMILY = f'''name = "t"
host = "phuoc-yu"
blocks = 6
[sides]
a = "{A}"
b = "{B}"
[[case]]
catalog = "lab-olympics"
dimensions = ["time/q01-point-lookup", "time/q06-text-filter"]
min_samples = 3
'''


def records(b=1.0, a2=1.0, broken=None, checked=True):
    """records_of for fixture.passes: B `b` times A, A' `a2` times A (each a little jitter), every
    declared content check passing (`checked`), and `broken(side, n)` a record that failed."""
    def records_of(side, n):
        jitter = 1 + 0.004 * ((n * 7) % 5 - 2)
        scale = {"b": fixture.worse(b, n) if b != 1 else 1.0, "a2": a2 * jitter}.get(side, 1.0)
        rec = fixture.record(samples={s: [v * scale for v in (4.8, 4.85, 4.8)] for s in SERIES})
        if checked:
            rec["checks"] = [{"path": p, "expected": v, "got": v, "ok": True} for p, v in CHECKS.items()]
            for o in rec["observations"]:  # perf_lab checks each invocation's answer
                o["checks"] = [dict(c) for c in rec["checks"]]
        if broken and broken(side, n):
            rec.update(status="failed", notes=["a content check failed"])
        return [rec]
    return records_of


class Abba(unittest.TestCase):
    def setUp(self):
        (ROOT / "tmp").mkdir(exist_ok=True)
        self.tmp = tempfile.TemporaryDirectory(dir=ROOT / "tmp")  # a family lives in the repository
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "t.toml"
        self.path.write_text(FAMILY, encoding="utf-8")
        self.family = abba.load_family(self.path)

    def round_(self, records_of, name="r1", drop=lambda p: False, manifest=True, host="phuoc-yu", k=1):
        """Round k's results as its job leaves them: the manifest of its check, its passes (after
        round k - 1's)."""
        out = Path(self.tmp.name) / name
        (out / "passes").mkdir(parents=True)
        for p in fixture.passes(records_of, blocks=6, sides=abba.SIDES):
            later = (k - 1) * 10 ** 9
            for item in [p["report"], *p["report"]["records"]]:
                item.update(started_ns=item["started_ns"] + later, finished_ns=item["finished_ns"] + later)
            if not drop(p):
                (out / "passes" / p["path"]).write_text(json.dumps(p["report"]), encoding="utf-8")
        if manifest:
            (out / "abba-manifest.json").write_text(json.dumps(abba.manifest_of(self.family, fixture.HARNESS, k, host)),
                                                    encoding="utf-8")
        return out

    def verdict(self, *rounds):
        code, text, _ = abba.report(self.family, list(rounds))
        return code, text

    def test_missing_evidence_is_no_verdict_never_a_pass(self):
        self.assertEqual(self.verdict(self.round_(records()))[0], abba.NO_LOSS)
        cases = [("a slot missing", dict(drop=lambda p: p["n"] == 9)),
                 ("a block missing", dict(drop=lambda p: p["report"]["block"] == 6)),
                 ("no manifest", dict(manifest=False))]
        for k, (case, kwargs) in enumerate(cases):
            with self.subTest(case):
                code, text = self.verdict(self.round_(records(), name=f"m{k}", **kwargs))
                self.assertEqual(code, abba.NO_VERDICT, text)

    def test_the_a_a_arm_runs_interleaved_and_a_copy_that_differs_voids_the_round(self):
        code, text = self.verdict(self.round_(records(), drop=lambda p: p["side"] == "a2"))
        self.assertEqual(code, abba.NO_VERDICT, text)
        code, text = self.verdict(self.round_(records(a2=1.10), name="slow-copy"))
        self.assertEqual(code, abba.NO_VERDICT, text)
        self.assertIn("the A-A' arm itself differs", text)
        # The order of a block: A A' B B A' A (or mirrored), every case.
        self.assertEqual([s for s, _ in fixture.fast.palindrome(list(abba.SIDES), 1)], ["a", "a2", "b", "b", "a2", "a"])

    def test_a_failed_content_check_or_too_few_samples_on_any_side_voids_the_round(self):
        code, text = self.verdict(self.round_(records(broken=lambda side, n: side == "a2" and n == 2)))
        self.assertEqual(code, abba.NO_VERDICT, text)
        self.assertIn("lab-olympics on a2", text)
        # Sol r1: a run with no check results counted as checked (exit 0).
        code, text = self.verdict(self.round_(records(checked=False), name="no-results"))
        self.assertEqual(code, abba.NO_VERDICT, text)
        self.assertIn("no passing result for its content checks q.q01-point-lookup.rows", text)
        # Sol r2: a perf_lab invocation with no check results borrowed its sibling's (exit 0).
        def two_invocations(side, n):
            out = records()(side, n)
            for rec in out:
                first, last = (json.loads(json.dumps(rec["observations"][0])) for _ in range(2))
                first["identity"]["repeat"], first["checks"] = 0, []
                last["identity"]["repeat"] = 1
                rec["observations"] = [first, last]
            return out
        code, text = self.verdict(self.round_(two_invocations, name="one-unchecked-invocation"))
        self.assertEqual(code, abba.NO_VERDICT, text)
        self.family["cases"][0]["min_samples"] = 4  # every series has 3 samples a pass
        self.assertEqual(self.verdict(self.round_(records(), name="few"))[0], abba.NO_VERDICT)

    def test_a_family_may_name_its_sides_by_short_ids(self):
        """lcp-passes (21 Oct 2026): sides named by 8 digits got "no verdict" on every pass of a
        complete run, the passes recording the full revisions. The sides are revisions every
        checkout has (test_fixed_revisions.py)."""
        a, b = (abba.git("rev-parse", rev) for rev in ("HEAD", "HEAD^"))
        self.path.write_text(FAMILY.replace(A, a[:8]).replace(B, b[:8]), encoding="utf-8")
        family = abba.load_family(self.path)
        self.assertEqual(family["sides"], {"a": a, "b": b})
        self.family = family
        with patch.dict(fixture.REVS, {"a": a, "a2": a, "b": b}):
            self.assertEqual(self.verdict(self.round_(records(), name="short"))[0], abba.NO_LOSS)
        self.path.write_text(FAMILY.replace(A, "abcdef1"), encoding="utf-8")
        with self.assertRaises(abba.Invalid) as refused:
            abba.load_family(self.path)
        self.assertIn("side a: abcdef1 names no single commit", "\n".join(refused.exception.args[0]))

    def test_a_family_with_a_case_without_a_content_check_is_refused(self):
        self.path.write_text(FAMILY + '''
[[case]]
name = "t-unchecked"
area = "query"
what = "a case of its own, without a check"
semantics = "none"
data = "social people=1000"
metric = "sum_p50_ms"
cap_gb = 1
systems = []
''', encoding="utf-8")
        with self.assertRaises(abba.Invalid) as refused:
            abba.load_family(self.path)
        self.assertIn("no content check", "\n".join(refused.exception.args[0]))

    def test_a_loss_is_shown_in_one_round_and_confirmed_by_another(self):
        one = self.round_(records(b=1.08), name="loss1")
        self.assertEqual(self.verdict(one)[0], abba.LOSS_SHOWN)
        two = self.round_(records(b=1.08), name="loss2", k=2)
        self.assertEqual(self.verdict(one, two)[0], abba.CONFIRMED)
        # Sol r1: the same round given twice confirmed its loss. Neither it, nor a copy of it
        # renamed round 2, nor the rounds out of order is a confirmation.
        copy = Path(self.tmp.name) / "copy"
        shutil.copytree(one, copy)
        manifest = json.loads((copy / "abba-manifest.json").read_text(encoding="utf-8"))
        manifest.update(round=2, session="another")
        (copy / "abba-manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
        for case, rounds, why in (("twice", (one, one), "round 1's directory again"),
                                  ("a copy", (one, copy), "round 1's evidence again"),
                                  ("out of order", (two, one), "its manifest says round 2")):
            with self.subTest(case):
                code, text = self.verdict(*rounds)
                self.assertEqual(code, abba.NO_VERDICT, text)
                self.assertIn(why, text)

    def test_the_jobs_time_every_block_on_one_host_with_the_a_a_arm(self):
        self.path.write_text(FAMILY.replace("blocks = 6", "blocks = 8"), encoding="utf-8")
        eight = abba.load_family(self.path)  # Sol r1: whole squares of A, A' and B
        self.assertEqual(sum("block" in s for s in abba.jobs(eight, B, ROOT.as_posix(), "office-pc")["abba-t-bbbbbbbb"]["steps"]), 9)
        # One whole square is the least a family may ask (a long, flat case needs no more), fewer
        # is refused: N11 found 6 hard-coded.
        self.path.write_text(FAMILY.replace("blocks = 6", "blocks = 3"), encoding="utf-8")
        self.assertEqual(abba.load_family(self.path)["blocks"], 3)
        self.path.write_text(FAMILY.replace("blocks = 6", "blocks = 2"), encoding="utf-8")
        with self.assertRaises(abba.Invalid):
            abba.load_family(self.path)
        self.path.write_text(FAMILY, encoding="utf-8")
        found = abba.jobs(self.family, B, ROOT.as_posix(), "office-pc")
        timed = found["abba-t-bbbbbbbb"]
        self.assertEqual((timed["requires"]["hosts"], timed["class"], timed["steps"][0][-2:]),
                         (["office-pc"], "exclusive", ["check", self.family["path"]]))
        blocks = [s for s in timed["steps"] if "block" in s]
        self.assertEqual(len(blocks), 6)
        self.assertEqual(blocks[0][-3:], [f"a={A}", f"a2={A}", f"b={B}"])
        # A round its manifest says ran elsewhere takes that host's floor, not the family's.
        read = []
        with patch.object(abba.rg, "read_floor", side_effect=lambda path: read.append(path.stem)):
            self.verdict(self.round_(records(), host="office-pc"))
        self.assertEqual((timed["env"]["GATE_HOST"], read), ("office-pc", ["office-pc"]))


if __name__ == "__main__":
    unittest.main()
