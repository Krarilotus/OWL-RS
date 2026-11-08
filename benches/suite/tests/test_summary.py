"""The evaluation contracts (suitekit/summary.py) on the synthetic cases of the independent
review of 3 October 2026 (B1-B5): lost work stays visible, an intermittent timeout makes an
item incomplete, wrong answers never enter a speed verdict, report and compare estimate the
same way, and the newest record of a day is the latest.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import contextlib
import csv
import io
import itertools
import json
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from suitekit import answers, compare, report, status, versus  # noqa: E402
from suitekit.schema import Result  # noqa: E402
from suitekit.summary import summarise  # noqa: E402
sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "fast"))
import compete  # noqa: E402

FIELDS = list(Result.__dataclass_fields__)


def row(**fields) -> dict:
    base = dict(date="2026-10-03T09:00", host="h", runtime="docker", system="nrese", version="-", publish="free",
                workload="w", tier="t", regime="none", run=1, task="query", item="", repeat="", status="ok", ms="",
                peak_mib="", rows="", bytes="", note="", cache="-", order="-")
    base.update({k: str(v) for k, v in fields.items()})
    return base


def load(run=1, status="ok", system="nrese") -> dict:
    return row(task="load", run=run, status=status, ms=10, rows=5, system=system)


def query(item, ms, run=1, repeat=1, status="ok", rows=1, system="nrese") -> dict:
    return row(task="query", item=item, run=run, repeat=repeat, status=status, ms=ms, rows=rows, system=system)


def write(directory: Path, rows: list[dict]) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "results.csv"
    with open(path, "w", newline="", encoding="utf-8") as f:
        writer = csv.DictWriter(f, fieldnames=FIELDS)
        writer.writeheader()
        writer.writerows(rows)
    # A suite run's manifest, as every run writes one: these fixtures read no /data file (7 Nov 2026:
    # without one, compare can't tell which input a run read, and refuses).
    manifest = directory / "manifest.json"
    if not manifest.exists():
        manifest.write_text(json.dumps({"schema": 1, "invocations": [{"datasets": {}, "dataset_sha256": {}}]}),
                            encoding="utf-8")
    return path


def output(main, argv) -> str:
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        main(argv)
    return out.getvalue()


class LoadedInputs(unittest.TestCase):
    """Review r1 of pkg/stage-data, R1-W1 (7 Nov 2026): lubm-10 and lubm-100 became a new release
    (their sorted distinct lines), and `compare` called a new LUBM result faster than an old one
    without saying the two had loaded other bytes. A case's code-change verdict needs the same
    frozen input files, byte for byte, on both sides; a form pin never stands in for them."""
    OLD, NEW = "66f44fa8" + "0" * 56, "85b6ccc3" + "0" * 56

    def report(self, ms, inputs=None, case="rdfs-full-lubm10"):
        record = {"case": case, "status": "ok", "samples": {"reason.p50_ms": [ms, ms]}, "metric": ms}
        if inputs is not None:
            record["inputs"] = inputs
        return {"reps": 2, "machine": {"host": "fixture", "governor": "performance"},
                "started": "2026-11-07T16:00:00", "records": [dict(record), dict(record)]}

    def compare(self, base, new):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = compare.compare_reports(base, new)
        return code, out.getvalue()

    def test_an_old_release_against_the_new_one_gives_no_code_change_verdict(self):
        old = {"lubm-10.nt": self.OLD, "univ-bench.nt": "u" * 64}
        new = {"lubm-10.nt": self.NEW, "univ-bench.nt": "u" * 64}
        code, text = self.compare(self.report(100.0, old), self.report(50.0, new))
        self.assertIn("different input", text)
        self.assertIn("lubm-10.nt", text)
        self.assertNotIn("faster", text)
        self.assertEqual(code, 2, text)  # nothing left that compares

    def test_a_case_whose_loaded_bytes_aren_t_known_gets_no_verdict(self):
        code, text = self.compare(self.report(100.0), self.report(50.0, {"lubm-10.nt": self.NEW, "univ-bench.nt": "u" * 64}))
        self.assertIn("input unknown", text)
        self.assertNotIn("faster", text)
        self.assertEqual(code, 2, text)
        # A file whose bytes a run couldn't read (the Docker runtime's volume) is unknown on both
        # sides alike: never "the same".
        unread = {"lubm-10.nt": None, "univ-bench.nt": None}
        code, text = self.compare(self.report(100.0, unread), self.report(50.0, unread))
        self.assertIn("input unknown", text)
        self.assertNotIn("faster", text)

    def test_the_same_bytes_compare_as_before(self):
        same = {"lubm-10.nt": self.NEW, "univ-bench.nt": "u" * 64}
        code, text = self.compare(self.report(100.0, same), self.report(50.0, same))
        self.assertIn("faster", text)
        self.assertNotIn("different input", text)
        self.assertEqual(code, 0, text)

    def suite_run(self, directory, ms, datasets=None, digests=None):
        path = write(directory, [load(), query("q1", ms), query("q1", ms, run=2), query("q1", ms, run=3)])
        invocation = {"machine": {"host": "fixture", "governor": "performance"}, "started": "2026-11-07T16:00:00",
                      "datasets": datasets or {}}
        if digests is not None:
            invocation["dataset_sha256"] = digests
        (directory / "manifest.json").write_text(json.dumps({"schema": 1, "invocations": [invocation]}),
                                                 encoding="utf-8")
        return path

    def test_suite_results_of_another_release_give_no_verdict(self):
        """The orchestrator, 7 Nov 2026 (16:55): suite results are compared by their own path
        (compare.main -> compare_pairs), which printed faster or slower across another release of a
        dataset. The same rule holds there: the /data files both runs read, the same bytes, or no
        verdict; without recorded hashes, another size is another input and the same size unknown."""
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            old = self.suite_run(d / "old", 100, {"/data/lubm-10.nt": 231509261})
            new = self.suite_run(d / "new", 50, {"/data/lubm-10.nt": 224996086})
            text = output(compare.main, [str(old), str(new)])
            self.assertIn("different input", text)
            self.assertNotIn("faster", text)
            self.assertEqual(compare.main([str(old), str(new)]), 2)
            same_size = self.suite_run(d / "same-size", 50, {"/data/lubm-10.nt": 231509261})
            self.assertIn("input unknown", output(compare.main, [str(old), str(same_size)]))
            # Recorded hashes decide: the same bytes compare as before.
            a = self.suite_run(d / "a", 100, {"/data/lubm-10.nt": 1}, {"/data/lubm-10.nt": "s" * 64})
            b = self.suite_run(d / "b", 50, {"/data/lubm-10.nt": 1}, {"/data/lubm-10.nt": "s" * 64})
            text = output(compare.main, [str(a), str(b)])
            self.assertNotIn("different input", text)
            self.assertNotIn("input unknown", text)
            self.assertIn("faster", text)

    def test_a_perf_lab_case_compares_on_the_same_bytes_and_is_refused_on_others(self):
        """The orchestrator, 7 Nov 2026 (17:12): leads compare perf-lab runs (the `lab-*` cases, the
        perf lab through fast.py) for counts; their records say what they loaded like every case."""
        same = {"olympics.nt": "o" * 64}
        code, text = self.compare(self.report(100.0, same, case="lab-olympics"),
                                  self.report(50.0, same, case="lab-olympics"))
        self.assertIn("faster", text)
        self.assertEqual(code, 0, text)
        code, text = self.compare(self.report(100.0, same, case="lab-olympics"),
                                  self.report(50.0, {"olympics.nt": "p" * 64}, case="lab-olympics"))
        self.assertIn("different input", text)
        self.assertNotIn("faster", text)
        self.assertEqual(code, 2, text)

    def test_a_case_of_generated_data_alone_needs_no_recorded_inputs(self):
        code, text = self.compare(self.report(100.0, case="rl-hierarchy"), self.report(50.0, case="rl-hierarchy"))
        self.assertIn("faster", text)
        self.assertEqual(code, 0, text)
        # Sol-loop r3 V3: an older record without `inputs` against a new one saying {} (none read).
        code, text = self.compare(self.report(100.0, case="rl-hierarchy"), self.report(50.0, {}, case="rl-hierarchy"))
        self.assertIn("faster", text)
        self.assertEqual(code, 0, text)

    ABSENT = object()  # a record before 7 Nov 2026: no `inputs` key at all

    def reps(self, ms, observations, case="rl-hierarchy"):
        """A report of one case whose records each say what they loaded: a mapping, or ABSENT."""
        records = []
        for rep, seen in enumerate(observations, 1):
            record = {"case": case, "rep": rep, "status": "ok", "samples": {"reason.p50_ms": [ms, ms]}, "metric": ms}
            if seen is not self.ABSENT:
                record["inputs"] = seen
            records.append(record)
        return {"reps": len(records), "machine": {"host": "fixture", "governor": "performance"},
                "started": "2026-11-07T16:00:00", "records": records}

    def test_recorded_external_bytes_that_disagree_are_not_generated_data(self):
        """Fix check r3, R3-W1 (7 Nov 2026): a case that once loaded a frozen file (`volume:history.nt`)
        and now generates its data under the same name (today's rl-hierarchy declares no input). A
        historical run without --prepared recorded other bytes of that file in its two repetitions; its
        records collapsed to "unknown", the generated-only exemption read that as "no input", and the
        new run came out faster, exit 0. Specified (the review's required correction): the exemption
        holds only where no record on either side names an external file; recorded external evidence
        that disagrees, or that only some records state, is unknown and refused before pooling."""
        a, b = {"history.nt": "a" * 64}, {"history.nt": "b" * 64}
        generated = [{}, {}]
        for label, old, new in (("the base's repetitions disagree", [a, b], generated),
                                ("one base repetition records a file, one says nothing", [a, self.ABSENT], generated),
                                ("one base repetition records a file, one records none", [a, {}], generated),
                                ("the base disagrees, the new run says nothing", [a, b], [self.ABSENT, self.ABSENT]),
                                ("both sides disagree", [a, b], [b, a]),
                                ("the new run's repetitions disagree", generated, [a, b])):
            with self.subTest(label):
                code, text = self.compare(self.reps(100.0, old), self.reps(50.0, new))
                self.assertEqual(code, 2, text)
                self.assertNotIn("faster", text)
                self.assertIn("input unknown", text)
        # The public entry point (suite.py compare / fast.py compare BASE.json NEW.json), the review's witness.
        with tempfile.TemporaryDirectory() as d:
            old, new = Path(d) / "history-base.json", Path(d) / "history-new.json"
            old.write_text(json.dumps(self.reps(100.0, [a, b])), encoding="utf-8")
            new.write_text(json.dumps(self.reps(50.0, generated)), encoding="utf-8")
            self.assertEqual(compare.main([str(old), str(new)]), 2)
            text = output(compare.main, [str(old), str(new)])
            self.assertNotIn("faster", text)
            self.assertIn("input unknown", text)

    def test_an_inputs_field_that_isn_t_a_mapping_is_not_none(self):
        """Design note r3w1, invariant 1: a record that has `inputs` but not as a mapping of file to
        hash says something was recorded, not that nothing was loaded; for a case of generated data
        today it is unknown, never the generated-only exemption."""
        for seen in (None, [], "lubm-10.nt", 0):
            with self.subTest(inputs=seen):
                code, text = self.compare(self.reps(100.0, [seen, seen]), self.reps(50.0, [{}, {}]))
                self.assertEqual(code, 2, text)
                self.assertNotIn("faster", text)

    def test_generated_data_alone_compares_whatever_mix_of_old_and_new_records(self):
        """Sol-loop r3 V3, kept by R3-W1's correction: records that name no external file, older ones
        without `inputs` and newer ones saying {} in any mix, on either side, compare."""
        for old, new in (([self.ABSENT, {}], [{}, {}]), ([{}, {}], [self.ABSENT, {}]),
                         ([self.ABSENT, self.ABSENT], [{}, self.ABSENT]), ([{}, {}], [{}, {}])):
            with self.subTest(old=old, new=new):
                code, text = self.compare(self.reps(100.0, old), self.reps(50.0, new))
                self.assertEqual(code, 0, text)
                self.assertIn("faster", text)
                self.assertNotIn("input unknown", text)

    LUBM = {"lubm-10.nt": "85b6ccc3" + "0" * 56, "univ-bench.nt": "u" * 64}

    def two_cases(self, ms, lubm_inputs, status="ok", **fields):
        """A report of an external-input case (rdfs-full-lubm10: its inputs as given, ABSENT for a report
        before 7 Nov 2026) beside an unchanged generated-data one (rl-hierarchy), two repetitions each."""
        records = []
        for rep in (1, 2):
            lubm = {"case": "rdfs-full-lubm10", "rep": rep, "status": status, "samples": {"reason.p50_ms": [ms, ms]},
                    "metric": ms, **fields}
            if lubm_inputs is not self.ABSENT:
                lubm["inputs"] = lubm_inputs
            records += [lubm, {"case": "rl-hierarchy", "rep": rep, "status": "ok", "inputs": {},
                               "samples": {"reason.p50_ms": [10.0, 10.0]}, "metric": 10.0}]
        return {"reps": 2, "machine": {"host": "fixture", "governor": "performance"},
                "started": "2026-11-07T16:00:00", "records": records}

    def test_an_input_refused_case_s_status_is_still_judged(self):
        """The master, 8 Nov 2026 (declared-metrics r4's merge note, as R3-F1 in #135): refusing a
        case's verdict for its input dropped all its records, so a report from before 7 Nov (no
        `inputs`) against a new one hid the external-input case's status: ok -> failed, beside an
        unchanged generated-data case, came out exit 0. Specified: an input refusal withholds the
        case's measures, never its status, checks or answers; a failure found is 1."""
        base, new = self.two_cases(100.0, self.ABSENT), self.two_cases(50.0, self.LUBM, status="failed")
        with tempfile.TemporaryDirectory() as d:  # the public entry point (fast.py / suite.py compare)
            old_path, new_path = Path(d) / "base.json", Path(d) / "new.json"
            old_path.write_text(json.dumps(base), encoding="utf-8")
            new_path.write_text(json.dumps(new), encoding="utf-8")
            self.assertEqual(compare.main([str(old_path), str(new_path)]), 1)
            text = output(compare.main, [str(old_path), str(new_path)])
        self.assertIn("rdfs-full-lubm10", text)
        self.assertIn("CHANGED", text)
        self.assertIn("input unknown", text)
        # Every case of a side refused for its input, one failing: the failure is found, 1, not 2.
        code, text = self.compare(self.reps(100.0, [self.ABSENT, self.ABSENT], "rdfs-full-lubm10"),
                                  {**self.reps(50.0, [self.LUBM, self.LUBM], "rdfs-full-lubm10"),
                                   "records": [{**r, "status": "failed"} for r in
                                               self.reps(50.0, [self.LUBM, self.LUBM], "rdfs-full-lubm10")["records"]]})
        self.assertEqual(code, 1, text)
        self.assertIn("CHANGED", text)

    def test_an_input_refused_case_s_answers_are_still_compared(self):
        """As above: rows that differ from the baseline's are a finding about the new run, whatever the
        input; the case's time stays uncompared."""
        code, text = self.compare(self.two_cases(100.0, self.ABSENT, answers={"q1": 5}),
                                  self.two_cases(50.0, self.LUBM, answers={"q1": 7}))
        self.assertEqual(code, 1, text)
        self.assertIn("ANSWERS DIFFER", text)
        self.assertNotIn("faster", text)

    def test_an_input_refused_case_gets_no_measure_verdict(self):
        """Review r1 of pkg/stage-data, R1-W1 (7 Nov 2026): time AND memory verdicts across other input
        bytes are no evidence of a code change; work counters and throughput alike. A case refused for
        its input is judged in status, checks and answers only: a peak twice as high or more work over
        other bytes is no regression. (A guard: on the record filter's code these records never reached
        the comparison, so this held there too.)"""
        code, text = self.compare(
            self.two_cases(100.0, self.ABSENT, peak_mib=1000, memory_kind="lifetime", counters={"rows": 10}),
            self.two_cases(300.0, self.LUBM, peak_mib=2000, memory_kind="lifetime", counters={"rows": 100}))
        self.assertEqual(code, 0, text)
        self.assertNotIn("MORE MEMORY", text)
        self.assertNotIn("MORE WORK", text)
        self.assertNotIn("SLOWER", text)

    def known_issue(self, ms, history, frontier_status, frontier_metric=50.0, frontier_peak=1000):
        """mem-load-20m-2g stopping at its declared memory limit on every repetition (a known issue), its
        run past the limit as given, its records loading `history.nt` as `history` (an older definition
        of the case: stage-data r3/r4), beside an unchanged generated-data case (rl-hierarchy)."""
        records = []
        for rep in (1, 2):
            frontier = {"status": frontier_status, "metric": frontier_metric, "peak_mib": frontier_peak,
                        "checks": [{"path": "loaded", "expected": 20000000, "got": 20000000 if frontier_status == "ok"
                                    else 0, "ok": frontier_status == "ok"}]}
            records += [{"case": "mem-load-20m-2g", "rep": rep, "status": "memory-limit",
                         "expected_outcome": "memory-limit", "inputs": {"history.nt": history}, "metric": None,
                         "samples": {}, "checks": [], "routes": [], **({"frontier": frontier} if rep == 1 else {})},
                        {"case": "rl-hierarchy", "rep": rep, "status": "ok", "inputs": {}, "metric": 10.0,
                         "samples": {"reason.p50_ms": [10.0, 10.0]}, "checks": [], "routes": []}]
        return {"reps": 2, "machine": {"host": "fixture", "governor": "performance"},
                "started": "2026-11-07T16:00:00", "records": records}

    def compare_paths(self, base, new, compact=False):
        """compare.main over the two reports as files: the full reports, or their compact records whose
        full report isn't here (load_report()'s fallback)."""
        sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "fast"))
        import fast
        with tempfile.TemporaryDirectory() as d:
            paths = []
            for name, report in (("base", base), ("new", new)):
                if compact:
                    report = {**{k: v for k, v in report.items() if k != "records"},
                              "full_report": {"path": str(Path(d) / f"{name}-gone.json"), "host": "elsewhere",
                                              "bytes": 1, "sha256": "0" * 64},
                              "records": [fast.compact_record(r) for r in report["records"]]}
                path = Path(d) / f"{name}.json"
                path.write_text(json.dumps(report), encoding="utf-8")
                paths.append(str(path))
            code = compare.main(paths)
            return code, output(compare.main, paths)

    def test_an_input_refused_known_issue_s_run_past_its_limit_is_still_judged(self):
        """Review r1 of pkg/compare-input-status, R1-W1 (8 Nov 2026): the frontier of a known issue
        (its run past the declared limit) went ok -> wrong over other input bytes, and compare exited 0:
        the input refusal withheld the frontier's status with its metric and peak. Specified: its status
        is a finding about the new run whatever the input, 1; only its metric and peak go uncompared."""
        for compact in (False, True):
            with self.subTest(compact=compact):
                code, text = self.compare_paths(self.known_issue(100.0, "a" * 64, "ok"),
                                                self.known_issue(100.0, "b" * 64, "wrong"), compact)
                self.assertEqual(code, 1, text)
                self.assertIn("mem-load-20m-2g", text)
                self.assertIn("WORSE", text)

    def test_an_input_refused_frontier_s_measures_alone_are_no_finding(self):
        """The control: the same frontier ok on both sides, its metric and peak far worse over other
        input bytes, is no regression (no evidence of a code change), and its measures aren't shown."""
        for compact in (False, True):
            with self.subTest(compact=compact):
                code, text = self.compare_paths(
                    self.known_issue(100.0, "a" * 64, "ok", frontier_metric=50.0, frontier_peak=1000),
                    self.known_issue(100.0, "b" * 64, "ok", frontier_metric=500.0, frontier_peak=4000), compact)
                self.assertEqual(code, 0, text)
                self.assertNotIn("WORSE", text)
                self.assertNotIn("4000", text)

    def test_a_case_s_agreeing_records_compare_and_one_silent_record_doesn_t(self):
        """A case that loads frozen files: every record stating the same bytes on both sides compares;
        one record that doesn't say makes its side unknown (R3-W1's correction: no record's
        observation is dropped on the way to the decision)."""
        same = {"lubm-10.nt": self.NEW, "univ-bench.nt": "u" * 64}
        code, text = self.compare(self.reps(100.0, [same, same], "rdfs-full-lubm10"),
                                  self.reps(50.0, [same, same], "rdfs-full-lubm10"))
        self.assertEqual(code, 0, text)
        self.assertIn("faster", text)
        code, text = self.compare(self.reps(100.0, [same, self.ABSENT], "rdfs-full-lubm10"),
                                  self.reps(50.0, [same, same], "rdfs-full-lubm10"))
        self.assertEqual(code, 2, text)
        self.assertNotIn("faster", text)
        self.assertIn("input unknown", text)

    def test_a_concatenation_s_order_is_part_of_its_input(self):
        """Sol-loop r3 V2: the same parts concatenated in another order are other bytes loaded."""
        a = {"lubm-1.nt": "l" * 64, "univ-bench.nt": "u" * 64, "concat 1": "a" * 64}
        b = {"lubm-1.nt": "l" * 64, "univ-bench.nt": "u" * 64, "concat 1": "b" * 64}
        code, text = self.compare(self.report(100.0, a, case="dl-abox-lubm1"), self.report(50.0, b, case="dl-abox-lubm1"))
        self.assertIn("different input", text)
        self.assertEqual(code, 2, text)

    def test_an_ore_case_s_converted_ontology_is_its_input(self):
        """Fix check r2, R2-W2: an `ore:` case loads a converted external ontology, not generated data;
        it was taken as input-free, and the registry's shortcut even dropped two different recorded
        hashes. Recorded identities come first; without them the input is unknown."""
        a, b = {"ore:ore_ont_9724": "a" * 64}, {"ore:ore_ont_9724": "b" * 64}
        code, text = self.compare(self.report(100.0, a, case="ctx-ore-9724"), self.report(50.0, b, case="ctx-ore-9724"))
        self.assertEqual(code, 2, text)
        self.assertNotIn("faster", text)
        self.assertIn("different input", text)
        code, text = self.compare(self.report(100.0, case="ctx-ore-9724"), self.report(50.0, case="ctx-ore-9724"))
        self.assertEqual(code, 2, text)  # an older report: unknown, never "none"
        self.assertIn("input unknown", text)
        code, text = self.compare(self.report(100.0, a, case="ctx-ore-9724"), self.report(50.0, a, case="ctx-ore-9724"))
        self.assertEqual(code, 0, text)
        self.assertIn("faster", text)

    def test_suite_results_hashing_a_file_they_don_t_list_by_size_hold_to_it(self):
        """Fix check r2, R2-W1: a hash in `dataset_sha256` for a file outside `datasets` was dropped,
        and two runs of other bytes compared (commit-latency loads LUBM its plan doesn't declare)."""
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            old = self.suite_run(d / "old", 100, {}, {"/data/lubm-10.nt": "o" * 64})
            new = self.suite_run(d / "new", 50, {}, {"/data/lubm-10.nt": "n" * 64})
            self.assertEqual(compare.main([str(old), str(new)]), 2)
            text = output(compare.main, [str(old), str(new)])
            self.assertNotIn("faster", text)
            self.assertIn("different input", text)
            same = self.suite_run(d / "same", 50, {}, {"/data/lubm-10.nt": "o" * 64})
            self.assertIn("faster", output(compare.main, [str(old), str(same)]))

    def test_invocations_agreeing_on_a_hash_hold_whatever_size_each_lists(self):
        """Sol-loop r5 X1: two invocations hashing a file alike, one listing its size and one not, were
        taken as disagreeing, and a same-bytes comparison was refused."""
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            runs = []
            for name, ms in (("old", 100), ("new", 50)):
                path = self.suite_run(d / name, ms)
                invocation = {"machine": {"host": "fixture", "governor": "performance"}, "started": "2026-11-07T16:00:00",
                              "dataset_sha256": {"/data/lubm-10.nt": "s" * 64}}
                (d / name / "manifest.json").write_text(json.dumps({"schema": 1, "invocations": [
                    {**invocation, "datasets": {}}, {**invocation, "datasets": {"/data/lubm-10.nt": 224996086}}]}),
                    encoding="utf-8")
                runs.append(path)
            self.assertEqual(compare.main([str(runs[0]), str(runs[1])]), 0)
            self.assertIn("faster", output(compare.main, [str(runs[0]), str(runs[1])]))

    def test_a_case_s_declared_input_missing_from_a_record_is_unknown(self):
        """The orchestrator, 7 Nov 2026 (18:12): a declared input a record doesn't hash, or a hash for
        an input the case doesn't declare, is unknown, never none."""
        code, text = self.compare(self.report(100.0, {"lubm-10.nt": "x" * 64}),
                                  self.report(50.0, {"lubm-10.nt": "x" * 64}))  # univ-bench.nt unhashed
        self.assertEqual(code, 2, text)
        self.assertIn("input unknown", text)
        extra = {"lubm-10.nt": "x" * 64, "univ-bench.nt": "u" * 64, "lubm-100.nt": "y" * 64}
        code, text = self.compare(self.report(100.0, extra), self.report(50.0, extra))
        self.assertEqual(code, 2, text)
        self.assertIn("input unknown", text)

    def test_suite_results_that_don_t_list_their_inputs_hashes_are_unknown(self):
        """Sol-loop r3 V1: `datasets: {}` doesn't prove a run read no frozen file (commit-latency
        loads LUBM outside its declared inputs, maintenance.py). Only a manifest that states its
        inputs' hashes (`dataset_sha256`, `{}` included) establishes them."""
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            old, new = self.suite_run(d / "old", 100, {}), self.suite_run(d / "new", 50, {})
            text = output(compare.main, [str(old), str(new)])
            self.assertIn("input unknown", text)
            self.assertNotIn("faster", text)
            stated_old = self.suite_run(d / "so", 100, {}, {})
            stated_new = self.suite_run(d / "sn", 50, {}, {})
            self.assertIn("faster", output(compare.main, [str(stated_old), str(stated_new)]))


class Contracts(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.dir = Path(temporary.name)

    def test_b1_a_failed_load_and_lost_queries_stay_visible(self):
        base = write(self.dir / "base", [load(), query("q1", 10), query("q2", 20)])
        failed = write(self.dir / "failed", [load(status="failed")])
        text = output(compare.main, [str(base), str(failed)])
        self.assertIn("1 of 1 loads not ok", text)
        self.assertIn("lost queries", text)
        fewer = write(self.dir / "fewer", [load(), query("q1", 10)])
        text = output(compare.main, [str(base), str(fewer)])
        self.assertIn("lost queries:** q2", text)
        gone = write(self.dir / "gone", [load(system="other")])
        text = output(compare.main, [str(base), str(gone)])
        self.assertIn("Lost: pairs the baseline ran", text)

    def test_b2_an_intermittent_timeout_makes_the_item_incomplete(self):
        rows = [load(1), load(2), query("q1", 100, run=1), query("q1", 300000, run=2, status="timeout", rows="")]
        pair = summarise(rows)[("w", "t", "nrese")]
        outcome = pair.outcome()
        self.assertEqual(outcome["outcome"], "partial")
        self.assertEqual(outcome["items"], "0/1")
        self.assertIn("1 timeout of 2", outcome["note"])
        text = output(report.main, [str(write(self.dir / "r", rows))])
        self.assertIn("| 0/1 |", text)
        self.assertIn("| q1 |", text)

    def test_b3_wrong_answers_never_enter_a_speed_verdict(self):
        base = write(self.dir / "base", [load(), query("q1", 100)])
        new = write(self.dir / "new", [load(), query("q1", 1, status="wrong", rows=2)])
        text = output(compare.main, [str(base), str(new)])
        self.assertIn("wrong answer", text)
        self.assertIn("0 faster", text)
        self.assertIn("1 not comparable", text)
        text = output(report.main, [str(new)])
        self.assertNotIn("| 1.0 |", text)

    def test_an_answer_the_contract_doesnt_time_enters_no_speed_verdict(self):
        """T8 PR #56 (Sol's fix review, P2): the run comparison took a query's time on its CSV
        status alone and read answers.json only for a verdict that changed, so a query wrong in
        both runs, by the same row count, came out faster. answers.timed decides, as for versus
        and compete: a wrong or unchecked answer earns no time."""
        # Sol, PR #56 compare r1: an item is its query's name already; a dotted one (geographica2's
        # synthetic tier) keeps its dots, or no verdict is found and the time counts.
        for (verdict, note), item in itertools.product((("wrong", ""), ("incomplete", "no reference answered")),
                                                       ("q1", "00-Synthetic_Selection_Intersects_1_1.0")):
            with self.subTest(verdict=verdict, item=item):
                runs = []
                for name, ms in (("base", 100), ("new", 1)):
                    path = write(self.dir / verdict / item / name, [load(), query(item, ms)])
                    (path.parent / "answers.json").write_text(json.dumps({"verdicts": [
                        {"workload": "w", "tier": "t", "regime": "none", "query": f"queries/{item}.rq",
                         "system": "nrese", "verdict": verdict, "note": note}]}), encoding="utf-8")
                    runs.append(str(path))
                text = output(compare.main, runs)
                self.assertIn("0 faster", text, "SPEC answer contract: no time from an answer it doesn't time")
                self.assertIn("1 not comparable", text)

    def test_b4_report_and_compare_estimate_alike(self):
        rows = [load(1), load(2), load(3)]
        for run, values in ((1, [1, 1, 1000]), (2, [2, 2, 1000]), (3, [100, 1000, 1000])):
            rows += [query("q1", v, run=run, repeat=i + 1) for i, v in enumerate(values)]
        pair = summarise(rows)[("w", "t", "nrese")]
        self.assertEqual(pair.items["-"]["q1"].estimate()[0], 2)
        text = output(report.main, [str(write(self.dir / "r", rows))])
        self.assertIn("| 2.0 |", text)

    def test_distinct_records_paths_preserve_unequal_run_medians(self):
        first = write(self.dir / "first" / "records",
                      [load()] + [query("q1", 1, repeat=i) for i in range(1, 6)])
        second = write(self.dir / "second" / "records", [load(), query("q1", 101)])
        pair = compare.collect([first, second], None, False)[("w", "t", "nrese")]
        item = pair.items["-"]["q1"]
        self.assertEqual(len(pair.ok_runs()), 2)
        self.assertEqual(sorted(map(len, item.repeats.values())), [1, 5])
        self.assertEqual(item.estimate(), (51, 100 / 51))

    def test_duplicate_resolved_csv_is_read_once(self):
        path = write(self.dir / "records", [load(), query("q1", 1), query("q1", 3, repeat=2)])
        alias = path.parent / ".." / "records" / path.name
        paths = compare.files(f"{path.parent},{path},{alias}")
        pair = compare.collect(paths, None, False)[("w", "t", "nrese")]
        self.assertEqual(len(pair.ok_runs()), 1)
        self.assertEqual(list(pair.items["-"]["q1"].repeats.values()), [[1, 3]])

    def test_b5_the_newest_of_a_day_is_the_latest(self):
        early = {"id": "early", "started": "2026-10-03T09:00",
                 "pairs": [{"workload": "w", "tier": "t", "system": "nrese", "outcome": "ok"}]}
        later = {"id": "later", "started": "2026-10-03T18:00",
                 "pairs": [{"workload": "w", "tier": "t", "system": "nrese", "outcome": "failed"}]}
        for records in ([early, later], [later, early]):
            self.assertEqual(status.newest(records)[("w", "t", "nrese")][1]["outcome"], "failed")

    def test_completed_client_requests_are_no_answers(self):
        rows = [load(system="a"), load(system="b"), query("clients-32", 5, rows=1000, system="a"),
                query("clients-32", 5, rows=1200, system="b")]
        pairs = summarise(rows)
        for system in ("a", "b"):
            self.assertEqual(pairs[("w", "t", system)].outcome()["outcome"], "ok")

    def test_answers_that_differ_between_systems_are_disputed(self):
        rows = [load(system="a"), load(system="b"), query("q1", 5, rows=1, system="a"),
                query("q1", 5, rows=2, system="b")]
        pairs = summarise(rows)
        for system in ("a", "b"):
            self.assertEqual(pairs[("w", "t", system)].outcome()["outcome"], "disputed")

    def test_validations_are_items_whose_result_counts_are_cross_checked(self):
        validate = lambda shapes, results, system: row(task="validate", item=shapes, ms=7, rows=results,
                                                       system=system)
        rows = [load(system="a"), load(system="b"), validate("core", 3, "a"), validate("core", 4, "b"),
                validate("tds", 1, "a"), validate("tds", 1, "b")]
        pairs = summarise(rows)
        for system in ("a", "b"):
            items = pairs[("w", "t", system)].items["-"]
            self.assertEqual(sorted(items), ["core", "tds"])
            self.assertEqual(pairs[("w", "t", system)].disputed, {"core"})

    def test_an_adjudication_settles_a_dispute(self):
        rows = [load(system="a"), load(system="b"), query("q1", 5, rows=1, system="a"),
                query("q1", 5, rows=2, system="b")]
        pairs = summarise(rows, settled={("w", "t", "q1"): "1"})
        self.assertEqual(pairs[("w", "t", "a")].outcome()["outcome"], "ok")
        self.assertEqual(pairs[("w", "t", "b")].outcome()["outcome"], "wrong")
        self.assertIsNone(pairs[("w", "t", "b")].items["-"]["q1"].estimate())

    def test_a_workload_without_a_load_counts_its_runs_from_its_steps(self):
        rows = [row(task="update", run=r, ms=100) for r in (1, 2, 3)]
        outcome = summarise(rows)[("w", "t", "nrese")].outcome()
        self.assertEqual((outcome["outcome"], outcome["runs"]), ("ok", 3))


class CompetitorCompleteness(unittest.TestCase):
    def verdict(self, rows):
        # Pin the fixture's adjudications independently of the repository's datasets.
        with patch("suitekit.summary.adjudications", return_value={}):
            summary = compete.summarise_suite(rows)
        return summary, compete.verdicts(summary)["other"]

    def test_no_winner_from_an_incomplete_shared_intersection(self):
        rows = [load(), load(system="other"), query("q1", 100), query("q2", 100),
                query("q1", 1, system="other")]
        _, verdict = self.verdict(rows)
        self.assertIsNone(verdict["query_winner"])
        self.assertNotIn("load_winner", verdict)
        self.assertIn("incomplete", verdict["answers"])

    def test_failure_wrong_answer_and_cold_timeout_exclude_winners(self):
        for status, repeat in [("failed", 1), ("wrong", 1), ("timeout", 0)]:
            rows = [load(), load(system="other"), query("q1", 100),
                    query("q1", 1, system="other"),
                    query("q1", 0.1, system="other", status=status, repeat=repeat)]
            with self.subTest(status=status, repeat=repeat):
                _, verdict = self.verdict(rows)
                self.assertIsNone(verdict["query_winner"])
                self.assertNotIn("load_winner", verdict)

    def test_nrese_failure_or_count_dispute_also_excludes_winners(self):
        for bad in (load(status="failed"), query("q1", 1, rows=2)):
            rows = [load(), load(system="other"), query("q1", 100),
                    query("q1", 1, system="other"), bad]
            _, verdict = self.verdict(rows)
            self.assertIsNone(verdict["query_winner"])

    def test_missing_query_run_or_repeat_excludes_winners(self):
        for missing in [(2, 1), (1, 2), (2, None)]:
            rows = []
            for system in ("nrese", "other"):
                for run in (1, 2):
                    rows.append(load(run=run, system=system))
                    for repeat in (1, 2):
                        if system == "other" and run == missing[0] and (missing[1] is None or repeat == missing[1]):
                            continue
                        rows.append(query("q1", 1, run=run, repeat=repeat, system=system))
            _, verdict = self.verdict(rows)
            self.assertIsNone(verdict["query_winner"])

    def test_estimator_and_noise_are_the_suite_contract(self):
        rows = []
        for system in ("nrese", "other"):
            for run, values in ((1, [1, 1, 1000]), (2, [2, 2, 1000]), (3, [100, 1000, 1000])):
                rows.append(load(run=run, system=system))
                rows += [query("q1", ms, run=run, repeat=i + 1, system=system)
                         for i, ms in enumerate(values)]
        summary, _ = self.verdict(rows)
        self.assertEqual(summary["other"]["queries"]["q1"]["median_ms"], 2)
        self.assertEqual(summary["other"]["queries"]["q1"]["noise"], 499.5)

    def test_complete_answer_only_comparator_keeps_counts_without_query_winner(self):
        rows = [load(), load(system="other"), query("q1", 10), query("q1", "", system="other")]
        summary, verdict = self.verdict(rows)
        self.assertTrue(summary["other"]["queries"]["q1"]["complete"])
        self.assertEqual(verdict["answers"], "match")
        self.assertIsNone(verdict["query_winner"])

    def test_client_completion_counts_are_not_answer_disputes(self):
        rows = [load(), load(system="other"), query("q1", 10), query("q1", 1, system="other"),
                query("clients-8", 10, rows=100), query("clients-8", 1, rows=1000, system="other")]
        _, verdict = self.verdict(rows)
        self.assertEqual(verdict["query_winner"], "other")

    def test_publication_permission_is_preserved_separately_from_completeness(self):
        rows = [load(), load(system="other"), query("q1", 10), query("q1", 1, system="other")]
        for r in rows:
            if r["system"] == "other":
                r["publish"] = "permission"
        summary, verdict = self.verdict(rows)
        self.assertTrue(summary["other"]["complete"])
        self.assertEqual(verdict["query_winner"], "other")
        self.assertEqual(verdict["publish"], "permission")

    def test_an_answer_the_contract_doesnt_time_excludes_winners(self):
        """T8 Sol r1 S6: a COUNT in xsd:int for xsd:integer has the same row count, but its answer
        deviates (answers.json): no time comes from it, nor from a wrong or undecided one. Sol r2
        S7: nor from one whose answer capture failed after its timed runs (incomplete). PR #56 R3:
        nor from one no reference checked (unchecked): one rule (answers.timed) for every report."""
        rows = [load(), load(system="other"), query("q1", 10), query("q1", 1, system="other")]
        for verdict, winner in (("ok", "other"), ("deviates", None), ("wrong", None), ("unqualified", None),
                                ("incomplete", None), ("unchecked", None)):
            record = {"workload": "w", "tier": "t", "regime": "none", "query": "queries/q1.rq", "system": "other",
                      "verdict": "incomplete" if verdict == "unchecked" else verdict,
                      "note": "no reference answered" if verdict == "unchecked" else ""}
            with patch("suitekit.summary.adjudications", return_value={}):
                summary = compete.summarise_suite(rows, answers.TimingVerdicts([record]))
            got = compete.verdicts(summary)["other"]
            with self.subTest(verdict=verdict):
                self.assertEqual(got["query_winner"], winner, f"SPEC answer contract: {got['answers']}")
                if winner is None:
                    self.assertIn(verdict, got["answers"])


def recorded(path: Path, records: list[tuple[str, str]], systems=("nrese",)) -> Path:
    """answers.json beside a results file: each (verdict, query file) for each system; an
    incomplete one because no reference answered (unchecked)."""
    (path.parent / "answers.json").write_text(json.dumps({"verdicts": [
        {"workload": "w", "tier": "t", "regime": "none", "query": query_file, "system": system, "verdict": verdict,
         "note": "no reference answered" if verdict == "incomplete" else ""}
        for verdict, query_file in records for system in systems]}), encoding="utf-8")
    return path


class OneVerdictPerQuery(unittest.TestCase):
    """T8 PR #56 R3 (Sol's fix check 1): the reports that time (compare, versus, compete) read one
    verdict per query from the contract: the worst of every record of it, in any order; joined by
    the query's result id, its file's stem (the driver's: `q1.rq.rq` is `q1.rq`); and a query its
    run recorded no verdict for, where the run recorded others, isn't timed."""

    REPORTS = ("compare", "versus", "compete")

    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.dir = Path(temporary.name)

    def timed(self, name: str, item: str, runs: list[list[tuple[str, str]]]) -> dict[str, bool]:
        """Whether each report times `item` at 100 ms against 1 ms (compare: a base run against a
        new one; versus and compete: nrese against `other` in one run), over runs whose answers.json
        holds `runs[i]`'s (verdict, query file) records."""
        sides, batch, rows = {"base": [], "new": []}, self.dir / name / "batch", []
        for i, records in enumerate(runs):
            for side, ms in (("base", 100), ("new", 1)):
                sides[side].append(str(recorded(write(self.dir / name / side / str(i), [load(), query(item, ms)]),
                                                records)))
            rows = [load(), load(system="other"), query(item, 100), query(item, 1, system="other")]
            recorded(write(batch / str(i), rows), records, ("nrese", "other"))
        text = output(compare.main, [",".join(sides["base"]), ",".join(sides["new"])])
        entry = {e["system"]: e for e in versus.versus(batch)}["other"]
        with patch("suitekit.summary.adjudications", return_value={}):
            summary = compete.summarise_suite(rows, versus.answer_verdicts(batch))
        return {"compare": "1 faster" in text, "versus": entry["timed"] == "1/1",
                "compete": compete.verdicts(summary)["other"]["query_winner"] == "other"}

    def test_the_worst_record_of_a_query_decides_in_either_order(self):
        for failure, order in itertools.product(("wrong", "incomplete"), ("bad first", "bad last")):
            runs = [[(failure, "queries/q1.rq")], [("ok", "queries/q1.rq")]]
            with self.subTest(failure=failure, order=order):
                got = self.timed(f"{failure}-{order}", "q1", runs if order == "bad first" else runs[::-1])
                self.assertEqual(got, dict.fromkeys(self.REPORTS, False), "SPEC a later ok hides no fault")

    def test_a_verdict_the_contract_doesnt_know_is_not_timed(self):
        # answers.json before 12 Oct named an answer match or mismatch (a wrong one): timing fails
        # closed on a name it doesn't know, alone or beside an ok record (PR #56 fix round 2, F6).
        mismatch, ok = ("mismatch", "queries/q1.rq"), ("ok", "queries/q1.rq")
        for name, runs in (("mismatch", [[mismatch]]), ("mismatch-then-ok", [[mismatch], [ok]]),
                           ("ok-then-mismatch", [[ok], [mismatch]]), ("match", [[("match", "queries/q1.rq")]])):
            with self.subTest(records=name):
                self.assertEqual(self.timed(name, "q1", runs), dict.fromkeys(self.REPORTS, False),
                                 "SPEC a verdict the contract doesn't know earns no time")

    def test_a_query_file_ending_in_rq_twice_joins_its_result_id(self):
        for verdict, timed in (("wrong", False), ("incomplete", False), ("ok", True)):
            with self.subTest(verdict=verdict):
                got = self.timed(f"suffix-{verdict}", "q1.rq", [[(verdict, "queries/q1.rq.rq")]])
                self.assertEqual(got, dict.fromkeys(self.REPORTS, timed), "SPEC the item is the file's stem")

    def test_a_query_its_run_recorded_no_verdict_for_is_not_timed(self):
        got = self.timed("unjoined", "q1", [[("ok", "queries/q2.rq")]])
        self.assertEqual(got, dict.fromkeys(self.REPORTS, False), "SPEC a failed join isn't counts only")
        # A run that recorded no answers at all is timed by its counts, as before.
        self.assertEqual(self.timed("unrecorded", "q1", [[]]), dict.fromkeys(self.REPORTS, True))

    def test_a_client_level_has_no_answer_to_join(self):
        runs = []
        for name, ms in (("base", 100), ("new", 1)):
            path = write(self.dir / name, [load(), query("q1", ms), query("clients-8", ms, rows=100)])
            runs.append(str(recorded(path, [("ok", "queries/q1.rq")])))
        self.assertIn("2 faster", output(compare.main, runs))


if __name__ == "__main__":
    unittest.main()
