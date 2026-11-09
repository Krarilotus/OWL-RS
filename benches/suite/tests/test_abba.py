"""The targeted A-B-B-A (benches/suite/abba.py): a family declared once, its rounds on the gate's
blocks (A A' B B A' A), fail-closed completeness, the A-A' arm, the verdict's exit codes.

20 Oct 2026: three agents wrote their own A-B-B-A scripts, and each failed open on missing runs,
had no interleaved A-A' arm, or checked counts instead of content.
"""
from __future__ import annotations

import contextlib
import io
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import abba  # noqa: E402
import isolation  # noqa: E402
import test_release_gate as fixture  # noqa: E402  (the gate's pass fixtures)

A, B = "a" * 40, "b" * 40
# The real reader of a harness's dataset metadata; the tests' fake harnesses (A, B) read as this tree's
# (Abba's patch), as PR #139's tests patched makers_at().
HARNESS_TABLES = getattr(abba.rg, "harness_tables", None)
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


@patch.object(abba.rg, "harness_tables", fixture.LOCAL_TABLES, create=True)
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

    def round_(self, records_of, name="r1", drop=lambda p: False, manifest=True, host="phuoc-yu", k=1,
               ran_on=None):
        """Round k's results as its job leaves them: the manifest of its check, naming `host`, its
        passes (after round k - 1's), run on `ran_on` (default: that host; None in it: unsaid)."""
        out = Path(self.tmp.name) / name
        (out / "passes").mkdir(parents=True)
        for p in fixture.passes(records_of, blocks=6, sides=abba.SIDES):
            later = (k - 1) * 10 ** 9
            for item in [p["report"], *p["report"]["records"]]:
                item.update(started_ns=item["started_ns"] + later, finished_ns=item["finished_ns"] + later)
            machine = {**p["report"]["machine"], "fleet_host": host if ran_on is None else ran_on(p)}
            p["report"]["machine"] = {k_: v for k_, v in machine.items() if v is not None}
            if not drop(p):
                (out / "passes" / p["path"]).write_text(json.dumps(p["report"]), encoding="utf-8")
        if manifest:
            (out / "abba-manifest.json").write_text(json.dumps(abba.manifest_of(self.family, fixture.HARNESS, k, host)),
                                                    encoding="utf-8")
        return out

    def verdict(self, *rounds):
        code, text, _ = abba.report(self.family, list(rounds))
        return code, text

    def test_baseline_routes_keep_answers_and_the_aa_control(self):
        """Master 9 Nov21:38: a baseline may use its own optimizer, with checked answers;
        B's route and the interleaved A-A' control remain required (PROTOCOL section5)."""
        for name, side, scale, wrong, expected in (
            ("baseline", "a", 1.0, False, abba.NO_LOSS),
            ("aa-differs", "a", 1.8, False, abba.NO_VERDICT),
            ("candidate", "b", 1.0, False, abba.NO_VERDICT),
            ("wrong-answer", "a", 1.0, True, abba.NO_VERDICT),
        ):
            with self.subTest(name=name):
                base = records(a2=scale)

                def own_route(s, n):
                    out = base(s, n)
                    if s == side or (side == "a" and s == "a2"):
                        out[0].update(status="off-route", routes=[{
                            "path": "q.q01-point-lookup.operators", "expected": "has group count",
                            "got": ["project", "bind", "group"], "ok": False}])
                        if wrong:
                            for check in out[0]["checks"]:
                                check["ok"] = False
                            for o in out[0]["observations"]:
                                for check in o["checks"]:
                                    check["ok"] = False
                    return out

                code, _ = self.verdict(self.round_(own_route, name=name))
                self.assertEqual(code, expected)

    def test_hypotheses_nothing_compared_never_read_as_no_loss(self):
        """SHACL lead, 8 Nov 2026 (orchestrator 04:31): A's answers differ from the pinned ones and B's
        match, so the gate takes neither side's time there (release_gate.compare_case, not_compared);
        the report decided over no test at all and said "No loss shown (exit 0)", its rows `- | 1 | -`.
        It names each hypothesis not compared and why, with B's own numbers, and never exits 0."""
        pinned = {"q01-point-lookup": 5, "q06-text-filter": 1033}
        for wrong in ({"q01-point-lookup": 4, "q06-text-filter": 1000}, {"q01-point-lookup": 4}):
            with self.subTest(wrong=sorted(wrong)):
                base = records()

                def records_of(side, n):
                    out = base(side, n)
                    for o in out[0]["observations"]:
                        o["rows"] = dict(pinned) if side == "b" else {**pinned, **wrong}
                    return out
                code, text = self.verdict(self.round_(records_of, name=f"r-{len(wrong)}"))
                self.assertNotEqual(code, abba.NO_LOSS)
                self.assertNotIn("No loss shown", text)
                self.assertEqual(code, abba.NOT_COMPARED)
                self.assertIn(f"{len(wrong)} of 2 hypotheses not compared", text)
                for s in wrong:
                    row = next(line for line in text.splitlines() if line.startswith(f"| lab-olympics / time/{s} |"))
                    self.assertIn("not compared: answers differ from a's", row)
                    self.assertRegex(row, r"B \d+(\.\d+)? ms")
                for s in set(pinned) - set(wrong):  # the other one tested, as before: its ratio and p
                    row = next(line for line in text.splitlines() if line.startswith(f"| lab-olympics / time/{s} |"))
                    self.assertNotIn("not compared", row)
                    self.assertNotIn("| - | - |", row)

    def test_a_fourth_round_that_compared_nothing_is_never_read_as_no_loss(self):
        """Review release-not-compared r1 W1: four rounds in order, the first three comparable, the
        fourth's A answering what the pinned rows don't (B's match), so it compared nothing; the
        decision took reasons from three slots only and the report said no loss."""
        pinned = {"q01-point-lookup": 5, "q06-text-filter": 1033}
        base = records()

        def wrong_a(side, n):
            out = base(side, n)
            for o in out[0]["observations"]:
                o["rows"] = dict(pinned) if side == "b" else {"q01-point-lookup": 4, "q06-text-filter": 1000}
            return out
        rounds = [self.round_(records() if k < 4 else wrong_a, name=f"r{k}", k=k) for k in range(1, 5)]
        code, text = self.verdict(*rounds)
        self.assertNotEqual(code, abba.NO_LOSS)
        self.assertEqual(code, abba.NOT_COMPARED)
        self.assertIn("2 of 2 hypotheses not compared", text)

    def test_a_plan_the_fleet_refuses_exits_as_refused(self):
        """STAGE2's lead, 8 Nov 2026: `abba.py run --plan` exited 0 when the fleet check it ran
        refused the set ("(exit 1)" in its text only), so a script read a refused plan as taken. It
        exits 1, as a run that wasn't submitted does; a plan the fleet takes still exits 0."""
        catalog = abba.cases_registry.load()
        for code, expected in ((1, 1), (0, 0)):
            with self.subTest(fleet_check=code), tempfile.TemporaryDirectory(dir=self.repo) as out:
                args = SimpleNamespace(checkout=self.repo.as_posix(), a=self.commits[0], b=self.commits[-1],
                                       harness=None, cases="lab-olympics", areas="", blocks=3, min_samples=3,
                                       name=None, out=Path(out), host=None, round=1, priority=790, reason="",
                                       plan=True)
                with patch.object(abba.cases_registry, "load_at", return_value=catalog), \
                        patch.object(abba.rg, "fleet_call", return_value=(code, "refused: a reason")) as fleet, \
                        contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(abba.run(args), expected)
                self.assertEqual([c.args[0] for c in fleet.call_args_list], ["check"])  # never submitted

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

    def test_passes_from_another_fleet_host_than_the_manifest_s_have_no_verdict(self):
        """Review r1 R1-F1 (HOST_IDENTITY): passes all timed on phuoc-yu, a manifest naming
        office-pc, got "no loss shown" against office-pc's floor. The fleet may move a timed job
        between steps when a host's window closes; such a round has no verdict, and so has one whose
        passes don't say which fleet host ran them."""
        read = []
        with patch.object(abba.rg, "read_floor", side_effect=lambda path: read.append(path.stem)):
            code, text = self.verdict(self.round_(records(), name="moved", host="office-pc",
                                                  ran_on=lambda p: "phuoc-yu"))
        self.assertEqual(code, abba.NO_VERDICT, text)
        self.assertIn("passes ran on phuoc-yu, not on office-pc", text)
        self.assertNotIn("office-pc", read)  # no floor is read for a round that has no verdict
        code, text = self.verdict(self.round_(records(), name="part-moved",
                                              ran_on=lambda p: "office-pc" if p["report"]["block"] == 6 else "phuoc-yu"))
        self.assertEqual(code, abba.NO_VERDICT, text)
        code, text = self.verdict(self.round_(records(), name="unsaid", ran_on=lambda p: None))
        self.assertEqual(code, abba.NO_VERDICT, text)
        self.assertIn("don't say which fleet host ran them", text)
        self.assertEqual(self.verdict(self.round_(records(), name="stayed"))[0], abba.NO_LOSS)

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

    def test_a_job_asks_for_what_making_its_data_takes(self):
        """Orchestrator, 7 Nov 2026: a set makes the datasets its cases read where its host lacks
        them (LUBM's in Docker), so it asks for Docker and the disk to make them in, not the bench
        assets; asked for nothing, it could land where its data can be neither found nor made."""
        self.path.write_bytes(abba.family_text("t", {"a": A, "b": B}, ["dlmode-lubm1"], 6).encode("utf-8"))
        with patch.object(abba.rg, "harness_tables", fixture.LOCAL_TABLES, create=True):  # its harness: this tree
            job = next(iter(abba.jobs(self.load(), B, self.repo.as_posix()).values()))
        self.assertEqual(job["requires"].get("has"), ["docker"])
        self.assertGreaterEqual(job["requires"].get("disk_gb", 0), 1)

    def test_a_job_making_its_data_in_containers_declares_them(self):
        """The master, 8 Nov 2026 (#129's regression): `abba.py --plan` of shacl-lubm on engine-v2 was
        refused by the fleet ("a Docker job a shared host could take: its steps start no container the
        fleet can see"). The job declares its maker's container (F61, spec 9) from the one data ask
        (release_gate.data_job()), asks that memory on top of what its blocks ask, and tells its
        makers the limits; a job making nothing in containers declares nothing."""
        self.path.write_bytes(abba.family_text("t", {"a": A, "b": B}, ["shacl-lubm"], 6).encode("utf-8"))
        with patch.object(abba.rg, "harness_tables", fixture.LOCAL_TABLES, create=True):  # its harness: this tree
            job = next(iter(abba.jobs(self.load(), B, self.repo.as_posix()).values()))
            set_id = next(iter(abba.jobs(self.load(), B, self.repo.as_posix())))
        self.assertEqual(job["spec"], 9)
        [maker] = job["containers"]
        plain = next(iter(abba.jobs(self.family, B, self.repo.as_posix()).values()))  # FAMILY's cases make nothing
        self.assertNotIn("containers", plain)
        self.assertNotIn("spec", plain)
        # What its blocks ask (at least 12 GB, more for a case's cap), plus the maker's container.
        blocks = max(12, *(c["cap_gb"] for c in self.load()["cases"]))
        self.assertEqual(job["requires"]["ram_gb"], blocks + maker["memory_gb"])
        self.assertEqual((job["env"]["NRESE_MAKER_MEMORY_GB"], job["env"]["NRESE_MAKER_CPUS"]),
                         (f"{maker['memory_gb']:g}", f"{maker['cpus']:g}"))
        self.assertNotIn("NRESE_MAKER_MEMORY_GB", plain["env"])
        import tomllib
        read = tomllib.loads(abba.set_file(set_id, [job]))["job"][0]
        self.assertEqual((read["spec"], read["containers"]), (9, [maker]))

    def test_a_harness_that_can_t_make_its_family_s_data_is_refused_before_planning(self):
        """Orchestrator, 8 Nov 2026: REAS's and C5's sets (abba-run-055fe886-f3da61f9) were planned by a
        submitter that makes support-stress data, so asked for no assets, but prepared by a harness
        from before the dataset makers (838cd7eb, PR #129), which didn't make them: "7 not as pinned"
        on a host without them, after the builds. Refused at the plan, saying why. Fix check r3 R3-W1
        of pkg/maker-containers (the master, 8 Nov 2026): a harness's makers are its catalogue's
        [makers], so one without them is refused as such, whichever of the cases' files it would make."""
        old = self.commits[-1]  # a real harness commit whose tree has no dataset makers
        sides = {"a": self.commits[0], "b": old}
        self.path.write_bytes(abba.family_text("t", sides, ["support-copies-17"], 6).encode("utf-8"))
        with patch.object(abba.rg, "harness_tables", HARNESS_TABLES):  # its real metadata (pkg/maker-containers)
            with self.assertRaises(abba.Invalid) as refused:  # planned unrefused, it asks for no assets
                abba.jobs(self.load(), old, self.repo.as_posix())
        text = "\n".join(refused.exception.args[0])
        self.assertIn("[makers]", text)
        self.assertIn("dataset makers", text)
        # The same family, a harness that makes it (this tree's metadata; the class's patch).
        job = next(iter(abba.jobs(self.load(), old, self.repo.as_posix()).values()))
        self.assertNotIn("owlrs-bench-assets", job["requires"].get("has", []))

    def test_whether_a_harness_makes_data_is_read_from_its_own_catalogue(self):
        """The harness's own catalogue, at that commit, names its dataset makers ([makers]) or doesn't
        (harness_tables(): PR #139's makers_at(), unified into the one reader of a harness's metadata).
        Fix check r3 R3-W1 (the master, 8 Nov 2026): one without [makers] is refused, whatever its
        release_gate.py says, as is a commit the checkout doesn't have."""
        with tempfile.TemporaryDirectory() as d:
            git = lambda *a: abba.subprocess.run(["git", "-C", d, *a], capture_output=True, text=True,  # noqa: E731
                                                 check=True).stdout.strip()
            git("init", "-q")
            Path(d, "benches/fast").mkdir(parents=True)
            Path(d, "benches/fast/release_gate.py").write_text("DATASET_MAKERS = {}\n", encoding="utf-8")

            def commit(makers: str) -> str:
                Path(d, "benches/datasets.toml").write_text(f'budget_gb = 1\n{makers}\n[[dataset]]\nfile = "x.nt"\n',
                                                            encoding="utf-8")
                git("add", "-A")
                git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "x")
                return git("rev-parse", "HEAD")
            before, after = commit("# no makers"), commit("[makers]")
            for name, harness in (("no [makers]", before), ("not there", "0" * 40)):
                with self.subTest(name):
                    with self.assertRaises(abba.rg.HarnessRefused) as refused:
                        HARNESS_TABLES(d, harness)
                    self.assertIn("[makers]", str(refused.exception))
            self.assertEqual(HARNESS_TABLES(d, after).makers, {})

    def test_a_job_asks_what_its_harness_makes_and_how(self):
        """Review r1 R1 of pkg/maker-containers (8 Nov 2026): a job's data is made by its harness's own
        prepare, from that harness's catalogue and makers (release-gate.sh:356, 401-406), so its
        declaration, its makers' env and its memory are the harness's, not the planning tree's: a
        harness whose LUBM maker takes 6 GB and 4 CPUs is declared so; one whose catalogue has LUBM from
        the bench assets declares nothing and asks the assets; one whose makers start containers it has
        no size for (before this package) is refused."""
        with tempfile.TemporaryDirectory() as tmp:
            for name, kw in (("sized", {"lubm_size": (6, 4)}), ("assets", {"lubm_made_by": "the bench assets' copy"}),
                             ("unsized", {"lubm_size": None})):
                repo = Path(tmp) / name
                harness = fixture.harness_commit(repo, **kw)
                self.path.write_bytes(abba.family_text("t", {"a": harness, "b": harness}, ["shacl-lubm"], 6)
                                      .encode("utf-8"))
                with self.subTest(name), patch.object(abba.rg, "harness_tables", HARNESS_TABLES, create=True):
                    if name == "unsized":
                        with self.assertRaises(abba.Invalid) as refused:
                            abba.jobs(self.load(), harness, repo.as_posix())
                        self.assertIn("container", "\n".join(refused.exception.args[0]))
                        continue
                    job = next(iter(abba.jobs(self.load(), harness, repo.as_posix()).values()))
                    blocks = max(12, *(c["cap_gb"] for c in self.load()["cases"]))
                    if name == "sized":
                        self.assertEqual(job["containers"], [{"name": "maker", "memory_gb": 6, "cpus": 4}])
                        self.assertEqual((job["env"]["NRESE_MAKER_MEMORY_GB"], job["env"]["NRESE_MAKER_CPUS"]),
                                         ("6", "4"))
                        self.assertEqual(job["requires"]["ram_gb"], blocks + 6)
                    else:
                        self.assertNotIn("containers", job)
                        self.assertEqual(job["requires"]["ram_gb"], blocks)
                        self.assertIn("owlrs-bench-assets", job["requires"]["has"])

    def test_one_job_asks_by_capability_and_builds_where_it_times(self):
        """The brief of 2 Nov 2026: no host names. A build job and a timed job pinned to one host
        were the only way to time where the binaries were built; one job does both, its host held
        alone from its first step to its last."""
        found = abba.jobs(self.family, B, self.repo.as_posix())
        self.assertEqual(list(found), ["abba-t-bbbbbbbb"])
        job = found["abba-t-bbbbbbbb"]
        self.assertNotIn("hosts", job["requires"])
        self.assertNotIn("GATE_HOST", job["env"])
        self.assertEqual((job["kind"], job["requires"]["roles"], job["requires"]["os"]),
                         ("owlrs.timed", ["measure", "build"], "linux"))
        # The fleet keeps an exclusive job's host only before an exclusive step (planner
        # held_between_steps; Sol r1 S1) and while the host takes exclusive work: every step is
        # exclusive, none untimed, so the job starts only where it can time (2 Nov 2026: an untimed
        # first step built on office-pc outside its timing window, and the blocks were offered
        # another host). The check, every build and the preparation are its first step.
        self.assertNotIn("untimed_steps", job)
        self.assertEqual(job["class"], "exclusive")
        first = job["steps"][0]
        for word in ("abba.py check", "release-gate.sh build", "release-gate.sh prepare"):
            self.assertIn(word, first[2])
        self.assertEqual(first[4:], [abba.ENV_FAMILY, A, B])  # the family, then every revision built
        self.assertEqual([s[2] for s in job["steps"][1:7]], ["block"] * 6)
        # Its gate state is its family's own: another A at the same B and name is another directory
        # (review r1 R1-F3); a family's rounds share it.
        self.path.write_text(FAMILY.replace(A, "c" * 40), encoding="utf-8")
        other = next(iter(abba.jobs(self.load(), B, self.repo.as_posix()).values()))
        self.assertNotEqual(other["env"]["GATE_NAME"], job["env"]["GATE_NAME"])
        self.path.write_text(FAMILY, encoding="utf-8")
        round2 = next(iter(abba.jobs(self.load(), B, self.repo.as_posix(), round_=2).values()))
        self.assertEqual(round2["env"]["GATE_NAME"], job["env"]["GATE_NAME"])
        # The first step asks no less memory than the blocks (Sol r2 S1): a case past 12 GB, the
        # builds' own figure before, so that a lower first-step ask would show (Sol r3 U1).
        big = dict(self.family, cases=[{**self.family["cases"][0], "cap_gb": 24}])
        heavy = next(iter(abba.jobs(big, B, self.repo.as_posix()).values()))
        self.assertEqual(heavy["requires"]["ram_gb"], 24)
        self.assertTrue(all(n.get("ram_gb", 24) >= 24 for n in heavy.get("step_needs", [])))
        self.assertGreater(job["est_min"], 0)
        import tomllib
        whole = tomllib.loads(abba.set_file("abba-t-bbbbbbbb", list(found.values())))
        self.assertEqual((whole["set"]["id"], whole["set"]["fail"], len(whole["job"])), ("abba-t-bbbbbbbb", "stop-rest", 1))
        self.assertNotIn("untimed_steps", whole["job"][0])

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

    def test_a_job_resolves_its_family_at_the_declared_harness(self):
        """Review r1 R1-F4: a job's check, run in B's checkout, took a case from B's registry that
        the declared harness doesn't have (dl-number-fragment, absent at 313e5a64), and wrote a
        manifest claiming that harness. Check, jobs and report resolve the family's cases at the
        declared harness; a case absent there is refused before anything is timed."""
        old = "313e5a640639bdc81a25dcbdc1b39f68fef352ef"  # engine-v2 before the registry's additions
        known = abba.subprocess.run(["git", "-C", str(ROOT), "cat-file", "-e", f"{old}^{{commit}}"]).returncode == 0
        if not known:
            self.skipTest(f"{old[:8]} isn't in this checkout's history")
        self.assertNotIn("dl-number-fragment", {c["name"] for c in abba.cases_registry.load_at(ROOT, old)})
        head = abba.git(ROOT, "rev-parse", "HEAD")
        family = abba.family_text("t", {"a": A, "b": B}, ["dl-number-fragment"], 3)

        def check(harness):
            with tempfile.TemporaryDirectory() as out:
                inherited = {k: v for k, v in abba.os.environ.items() if k not in ("GATE_HOST", "NRESE_FAMILY")}
                env = {**inherited, "FLEET_OUT": out, "FLEET_HOST": "office-pc", "ABBA_FAMILY": family}
                if harness:
                    env["ABBA_HARNESS"] = harness
                else:
                    env.pop("ABBA_HARNESS", None)
                done = abba.subprocess.run([sys.executable, "-B", str(ROOT / "benches/suite/abba.py"), "check",
                                            abba.ENV_FAMILY], env=env, capture_output=True, text=True, cwd=ROOT)
                written = (Path(out) / "abba-manifest.json").is_file()
            return done, written

        refused, written = check(old)
        self.assertEqual(refused.returncode, abba.INVALID, refused.stdout + refused.stderr)
        self.assertIn("dl-number-fragment", refused.stderr)
        self.assertFalse(written)  # no manifest claiming a harness it doesn't hold the cases of
        taken, written = check(head)
        self.assertEqual(taken.returncode, 0, taken.stderr)
        self.assertTrue(written)
        unnamed, _ = check(None)  # a job's check without its harness is refused, never this tree's
        self.assertEqual(unnamed.returncode, abba.INVALID, unnamed.stderr)
        self.assertIn("ABBA_HARNESS", unnamed.stderr)
        # The same family loaded at each revision: refused at the older one, resolved at the head.
        with patch.dict(abba.os.environ, {"ABBA_FAMILY": family}):
            with self.assertRaises(abba.Invalid):
                abba.load_family(abba.ENV_FAMILY, ROOT, harness=old)
            self.assertEqual([c["name"] for c in abba.load_family(abba.ENV_FAMILY, ROOT, harness=head)["cases"]],
                             ["dl-number-fragment"])

    def test_a_family_s_own_cases_are_the_declared_harness_s(self):
        """Sol-loop of review r1's fix (S1): a family's own cases were read from the launching
        checkout while the harness's fast suite times them from its own copy of the family file
        (NRESE_FAMILY in the harness's tree). With a declared harness, the family file must be at it
        byte for byte, line endings aside; a file absent there, or changed since, is refused before anything runs."""
        own_case = ('\n[[case]]\nname = "t-own"\narea = "query"\nwhat = "a case of its own"\nsemantics = "none"\n'
                    'data = "social people={n}"\nmetric = "sum_p50_ms"\ncap_gb = 1\nsystems = []\n'
                    'check = {{ "loaded" = ">= 1" }}\n')
        with tempfile.TemporaryDirectory() as root:
            repo = Path(root)
            git = lambda *a: abba.subprocess.run(["git", "-C", root, *a], capture_output=True, text=True,  # noqa: E731
                                                 check=True).stdout.strip()
            git("init", "-q")
            commit = lambda m: (git("add", "-A"), git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q",  # noqa: E731
                                                      "--allow-empty", "-m", m), git("rev-parse", "HEAD"))[2]
            before = commit("no family yet")
            path = repo / "benches" / "abba" / "own.toml"
            path.parent.mkdir(parents=True)
            path.write_bytes((FAMILY + own_case.format(n=1000)).encode("utf-8"))
            harness = commit("the family")
            catalog = abba.cases_registry.load()
            self.assertEqual([c["name"] for c in abba.load_family(path, repo, catalog, harness=harness)["cases"]],
                             ["lab-olympics", "t-own"])
            path.write_bytes((FAMILY + own_case.format(n=2000)).encode("utf-8"))  # B changes the case's data
            changed = commit("its case changed")
            for at, why in ((harness, "differs"), (before, "isn't at")):
                with self.subTest(why):
                    with self.assertRaises(abba.Invalid) as refused:
                        abba.load_family(path, repo, catalog, harness=at)
                    self.assertIn(why, "\n".join(refused.exception.args[0]))
            self.assertEqual(len(abba.load_family(path, repo, catalog, harness=changed)["cases"]), 2)
            # Line endings aren't a change (sol-loop r2, T1): a file stored with CRLF, read in a
            # checkout that normalises line endings, is the harness's file as it is.
            path.write_bytes((FAMILY + own_case.format(n=2000)).replace("\n", "\r\n").encode("utf-8"))
            git("-c", "core.autocrlf=false", "add", "-A")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "stored with CRLF")
            git("config", "core.autocrlf", "true")
            self.assertEqual(len(abba.load_family(path, repo, catalog, harness=git("rev-parse", "HEAD"))["cases"]), 2)
            git("config", "core.autocrlf", "false")
            # A value equal in Python but not as the suite passes it on (sol-loop r3, U1): the fast
            # suite stringifies env values ("268435456.0" vs "268435456"), and perf_lab reads the
            # former as no limit. A family that differs so is refused.
            typed = FAMILY + own_case.format(n=2000) + "env = { NRESE_BULK_LOAD_MEMORY = VALUE }\n"
            path.write_bytes(typed.replace("VALUE", "268435456.0").encode("utf-8"))
            as_float = commit("its case's buffer as a float")
            path.write_bytes(typed.replace("VALUE", "268435456").encode("utf-8"))
            with self.assertRaises(abba.Invalid) as refused:
                abba.load_family(path, repo, catalog, harness=as_float)
            self.assertIn("differs", "\n".join(refused.exception.args[0]))
            # Line endings alone, both committed, no conversion (fix check r1, V1): the harness's
            # suite times the same cases, so the family is valid, in either direction.
            lf = (FAMILY + own_case.format(n=3000)).encode("utf-8")
            crlf = lf.replace(b"\n", b"\r\n")
            for stored, here, which in ((lf, crlf, "LF at the harness, CRLF here"), (crlf, lf, "CRLF at the harness, LF here")):
                with self.subTest(which):
                    path.write_bytes(stored)
                    at = commit("the harness's line endings")
                    path.write_bytes(here)
                    commit("B's line endings")
                    self.assertEqual(len(abba.load_family(path, repo, catalog, harness=at)["cases"]), 2)

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
            # The harness is a commit of this checkout: the job's check reads its registry (R1-F4).
            env = {"FLEET_OUT": out, "FLEET_HOST": "office-pc", "ABBA_HARNESS": abba.git(ROOT, "rev-parse", "HEAD"),
                   "ABBA_FAMILY": FAMILY}
            inherited = {k: v for k, v in abba.os.environ.items() if k not in ("GATE_HOST", "NRESE_FAMILY")}
            done = abba.subprocess.run([sys.executable, "-B", str(ROOT / "benches/suite/abba.py"), "check", abba.ENV_FAMILY],
                                       env={**inherited, **env}, capture_output=True, text=True, cwd=ROOT)
            self.assertEqual(done.returncode, 0, done.stderr)
            manifest = json.loads((Path(out) / "abba-manifest.json").read_text(encoding="utf-8"))
        self.assertEqual((manifest["host"], manifest["path"]), ("office-pc", abba.ENV_FAMILY))


if __name__ == "__main__":
    unittest.main()
