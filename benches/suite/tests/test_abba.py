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
import isolation  # noqa: E402
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
    @classmethod
    def setUpClass(cls):
        # The commits a family's sides name and the family itself: a repository of the tests' own
        # (isolation.repo).
        own = tempfile.TemporaryDirectory()
        cls.addClassCleanup(own.cleanup)
        cls.repo = Path(own.name)
        cls.commits = isolation.repo(cls.repo, 2)

    def setUp(self):
        # A family lives in the checkout it names its sides in: the tests' own, with its rounds'
        # results beside it (PR #57 r1 R2: in ROOT/tmp, a read-only checkout failed every test).
        self.tmp = tempfile.TemporaryDirectory(dir=self.repo)
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / "t.toml"
        self.path.write_text(FAMILY, encoding="utf-8")
        self.family = self.load()

    def load(self):
        return abba.load_family(self.path, self.repo)

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
        complete run, the passes recording the full revisions. The sides are commits of the tests'
        own repository: fixed ids from before the 26 Oct rewrite were gone from pruned hosts, and a
        shallow checkout has no HEAD^."""
        a, b = self.commits
        self.path.write_text(FAMILY.replace(A, a[:8]).replace(B, b[:8]), encoding="utf-8")
        family = self.load()
        self.assertEqual(family["sides"], {"a": a, "b": b})
        self.family = family
        with patch.dict(fixture.REVS, {"a": a, "a2": a, "b": b}):
            self.assertEqual(self.verdict(self.round_(records(), name="short"))[0], abba.NO_LOSS)
        self.path.write_text(FAMILY.replace(A, "abcdef1"), encoding="utf-8")
        with self.assertRaises(abba.Invalid) as refused:
            self.load()
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
            self.load()
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
        eight = self.load()  # Sol r1: whole squares of A, A' and B
        self.assertEqual(sum("block" in s for s in abba.jobs(eight, B, self.repo.as_posix(), "office-pc")["abba-t-bbbbbbbb"]["steps"]), 9)
        # One whole square is the least a family may ask (a long, flat case needs no more), fewer
        # is refused: N11 found 6 hard-coded.
        self.path.write_text(FAMILY.replace("blocks = 6", "blocks = 3"), encoding="utf-8")
        self.assertEqual(self.load()["blocks"], 3)
        self.path.write_text(FAMILY.replace("blocks = 6", "blocks = 2"), encoding="utf-8")
        with self.assertRaises(abba.Invalid):
            self.load()
        self.path.write_text(FAMILY, encoding="utf-8")
        found = abba.jobs(self.family, B, self.repo.as_posix(), "office-pc")  # its sides resolve there too
        timed = found["abba-t-bbbbbbbb"]
        first = timed["steps"][0]
        self.assertEqual((timed["requires"]["hosts"], timed["class"], ["check" if "check" in first[2] else "?", first[4]]),
                         (["office-pc"], "exclusive", ["check", abba.ENV_FAMILY]))
        blocks = [s for s in timed["steps"] if "block" in s]
        self.assertEqual(len(blocks), 6)
        self.assertEqual(blocks[0][-3:], [f"a={A}", f"a2={A}", f"b={B}"])
        # A round its manifest says ran elsewhere takes that host's floor, not the family's.
        read = []
        with patch.object(abba.rg, "read_floor", side_effect=lambda path: read.append(path.stem)):
            self.verdict(self.round_(records(), host="office-pc"))
        self.assertEqual((timed["env"]["GATE_HOST"], read), ("office-pc", ["office-pc"]))
        self.assertIn("pin_reason", timed)

    def test_one_job_asks_by_capability_builds_untimed_and_times_alone(self):
        """The brief of 2 Nov 2026: no host names. A build job and a timed job pinned to one host
        were the only way to time where the binaries were built; one job does both, its builds
        shareable (untimed steps), its host kept between steps, alone for its blocks."""
        found = abba.jobs(self.family, B, self.repo.as_posix())
        self.assertEqual(list(found), ["abba-t-bbbbbbbb"])
        job = found["abba-t-bbbbbbbb"]
        self.assertNotIn("hosts", job["requires"])
        self.assertNotIn("GATE_HOST", job["env"])
        self.assertEqual((job["kind"], job["requires"]["roles"], job["requires"]["os"]),
                         ("owlrs.timed", ["measure", "build"], "linux"))
        # The fleet keeps an exclusive job's host only before an exclusive step (planner
        # held_between_steps; Sol r1 S1): the check, every build and the preparation are the first
        # step, the only untimed one, and every later step is exclusive, so it runs where they ran.
        self.assertEqual(job["untimed_steps"], [0])
        first = job["steps"][0]
        for word in ("abba.py check", "release-gate.sh build", "release-gate.sh prepare"):
            self.assertIn(word, first[2])
        self.assertEqual(first[4:], [abba.ENV_FAMILY, A, B])  # the family, then every revision built
        self.assertEqual([s[2] for s in job["steps"][1:7]], ["block"] * 6)
        self.assertGreater(job["est_min"], 0)
        import tomllib
        whole = tomllib.loads(abba.set_file("abba-t-bbbbbbbb", list(found.values())))
        self.assertEqual((whole["set"]["id"], whole["set"]["fail"], len(whole["job"])), ("abba-t-bbbbbbbb", "stop-rest", 1))
        self.assertEqual(whole["job"][0]["untimed_steps"], job["untimed_steps"])

    def test_a_family_of_registry_cases_runs_from_anywhere_without_a_commit(self):
        """PR #91's timing (2 Nov 2026): a family had to be committed on a throwaway ref to be read in
        the job's checkout. A family of registry cases travels in the job (ABBA_FAMILY): read from a
        file outside the repository or from the environment, it is the same family (its SHA-256 and
        hypotheses), and its job names env:ABBA_FAMILY, not a path."""
        text = self.path.read_bytes().decode("utf-8")  # the family file's own bytes, its identity
        with tempfile.TemporaryDirectory() as outside:
            path = Path(outside) / "f.toml"
            path.write_bytes(text.encode("utf-8"))
            there = abba.load_family(path, self.repo)
            with patch.dict(abba.os.environ, {"ABBA_FAMILY": text}):
                carried = abba.load_family(abba.ENV_FAMILY, self.repo)
        for family in (there, carried):
            self.assertEqual((family["sha256"], family["hypotheses"]), (self.family["sha256"], self.family["hypotheses"]))
        job = next(iter(abba.jobs(there, B, self.repo.as_posix()).values()))
        self.assertEqual(job["env"]["ABBA_FAMILY"], text)
        self.assertNotIn("NRESE_FAMILY", job["env"])
        self.assertEqual(job["steps"][0][4], abba.ENV_FAMILY)
        # Its rounds are judged against the family whichever way it was read.
        self.family = carried
        self.assertEqual(self.verdict(self.round_(records(), name="carried"))[0], abba.NO_LOSS)
        # An empty variable is no family; a family with cases of its own stays in the repository.
        with patch.dict(abba.os.environ, {"ABBA_FAMILY": ""}), self.assertRaises(abba.Invalid):
            abba.load_family(abba.ENV_FAMILY, self.repo)
        own = FAMILY + '[[case]]\nname = "t-own"\narea = "query"\nwhat = "x"\nsemantics = "none"\n' \
                       'data = "social people=1000"\nmetric = "sum_p50_ms"\ncap_gb = 1\nsystems = []\ncheck = { "loaded" = ">= 1" }\n'
        with patch.dict(abba.os.environ, {"ABBA_FAMILY": own}), self.assertRaises(abba.Invalid) as refused:
            abba.load_family(abba.ENV_FAMILY, self.repo)
        self.assertIn("lives in the repository", "\n".join(refused.exception.args[0]))

    def test_run_builds_the_family_from_the_registry(self):
        """`run --cases/--areas`: registry cases only, each once, the timeable ones of an area; an
        unknown name or area, a case that can't be timed, and an empty selection are refused with
        the reason. The family it writes is a family file: loaded, its hypotheses are those of the
        same family written by hand."""
        catalog = abba.cases_registry.load()
        self.assertEqual(abba.select(catalog, ["lab-olympics", "lab-olympics"], []), ["lab-olympics"])
        reasoning = abba.select(catalog, [], ["reasoning"])
        self.assertIn("rl-sameas", reasoning)
        self.assertTrue(all(c["check"] and c.get("outcome", "ok") == "ok" for c in catalog if c["name"] in reasoning))
        self.assertNotIn("rl-single-thread", reasoning)  # no content check: not timed
        for cases, areas, why in ((["no-such-case"], [], "no case 'no-such-case'"),
                                  ([], ["no-such-area"], "no area 'no-such-area'"),
                                  (["rl-single-thread"], [], "rl-single-thread: no content check"),
                                  (["dl-transitive-400"], [], "a declared outcome (gave-up)"),
                                  ([], [], "no case: name --cases or --areas")):
            with self.subTest(why), self.assertRaises(abba.Invalid) as refused:
                abba.select(catalog, cases, areas)
            self.assertIn(why, "\n".join(refused.exception.args[0]))
        text = abba.family_text("t", {"a": A, "b": B}, ["lab-olympics"], 6)
        self.assertNotIn("\r", text)
        self.path.write_bytes(text.encode("utf-8"))
        built = self.load()
        self.path.write_text(FAMILY.replace('dimensions = ["time/q01-point-lookup", "time/q06-text-filter"]\n'
                                            "min_samples = 3\n", ""), encoding="utf-8")
        self.assertEqual(built["hypotheses"], self.load()["hypotheses"])
        # The run's id is its request's: the same request is the same set, another one another (Sol r1 S2).
        same = abba.run_name(None, {"a": A, "b": B}, B, ["lab-olympics"], 3, 1)
        self.assertEqual(same, abba.run_name(None, {"a": A, "b": B}, B, ["lab-olympics"], 3, 1))
        for other in (abba.run_name(None, {"a": B, "b": B}, B, ["lab-olympics"], 3, 1),
                      abba.run_name(None, {"a": A, "b": B}, B, ["lab-olympics"], 6, 1),
                      abba.run_name("named", {"a": A, "b": B}, B, ["lab-olympics"], 3, 1)):
            self.assertNotEqual(other, same)
        # Effort follows spread: blocks from the calibration, in whole squares, at least one.
        self.assertEqual(abba.suggested_blocks(["x"], {}), 3)
        self.assertEqual(abba.suggested_blocks(["x"], {"x": {"passes": 8}}), 6)
        self.assertEqual(abba.suggested_blocks(["x"], {"x": {"blocks": 7}}), 9)

    def test_a_job_s_check_records_the_host_the_fleet_chose(self):
        """Placed by capability, the round's host is known only where it runs: the check writes
        FLEET_HOST into the manifest (GATE_HOST, a pin, wins), and the report reads that host's floor."""
        with tempfile.TemporaryDirectory() as out:
            env = {"FLEET_OUT": out, "FLEET_HOST": "office-pc", "ABBA_HARNESS": B, "ABBA_FAMILY": FAMILY}
            inherited = {k: v for k, v in abba.os.environ.items() if k not in ("GATE_HOST", "NRESE_FAMILY")}
            done = abba.subprocess.run([sys.executable, "-B", str(ROOT / "benches/suite/abba.py"), "check", abba.ENV_FAMILY],
                                       env={**inherited, **env}, capture_output=True, text=True, cwd=ROOT)
            self.assertEqual(done.returncode, 0, done.stderr)
            manifest = json.loads((Path(out) / "abba-manifest.json").read_text(encoding="utf-8"))
        self.assertEqual((manifest["host"], manifest["path"]), ("office-pc", abba.ENV_FAMILY))


if __name__ == "__main__":
    unittest.main()
