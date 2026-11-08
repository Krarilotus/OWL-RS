"""Fast report counters accept structured metadata without changing numeric work rules."""
import importlib.util
import contextlib
import copy
import io
import json
import re
import tempfile
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "fast_counter_contract", Path(__file__).resolve().parents[2] / "fast" / "fast.py"
)
fast = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fast)


class FastCounters(unittest.TestCase):
    def test_unchanged_nested_metadata(self):
        values = {"q.a.rewrites": [[{"rules": ["r1"]}]] * 3}
        self.assertEqual(fast.counter_changes(values, values), (False, []))

    def test_changed_nested_metadata_is_preserved(self):
        before, after = [{"rules": ["r1"]}], [{"rules": ["r1", "r2"]}]
        grew, notes = fast.counter_changes({"rewrite": [before] * 3}, {"rewrite": [after] * 3})
        self.assertFalse(grew)
        self.assertEqual(notes, [f"rewrite: {json.dumps(before, sort_keys=True)} -> "
                                 f"{json.dumps(after, sort_keys=True)} (metadata changed)"])

    def test_variable_repetitions_are_not_judged(self):
        for b, n in [([[1], [2]], [[3], [3]]), ([[1], [1]], [[2], [3]]),
                     ([100, 101], [150, 150])]:
            with self.subTest(b=b, n=n):
                self.assertEqual(fast.counter_changes({"work": b}, {"work": n}), (False, []))

    def test_numeric_thresholds_are_unchanged(self):
        for before, after, expected in [
            (100, 100, (False, [])),
            (100, 102, (False, ["work: 100 -> 102 (+2.0%)"])),
            (100, 103, (True, ["work: 100 -> 103 (+3.0%)  MORE WORK"])),
            (100, 98, (False, ["work: 100 -> 98 (-2.0%)"])),
            (100, 97, (False, ["work: 100 -> 97 (-3.0%)  less work"])),
            (0, 1, (True, ["work: 0 -> 1 (+inf%)  MORE WORK"])),
        ]:
            with self.subTest(before=before, after=after):
                self.assertEqual(fast.counter_changes({"work": [before] * 3},
                                                     {"work": [after] * 3}), expected)

    def test_scalar_metadata_and_type_changes(self):
        for before, after in [("a", "b"), (False, True), (None, "x"), ([1], 2)]:
            with self.subTest(before=before, after=after):
                grew, notes = fast.counter_changes({"metadata": [before]}, {"metadata": [after]})
                self.assertFalse(grew)
                self.assertEqual(len(notes), 1)
                self.assertIn("metadata changed", notes[0])

    def test_metadata_cannot_hide_numeric_growth(self):
        grew, notes = fast.counter_changes({"metadata": [[1]], "work": [100]},
                                           {"metadata": [[2]], "work": [103]})
        self.assertTrue(grew)
        self.assertEqual(len(notes), 2)
        self.assertIn("MORE WORK", notes[1])


SHAPES_CASE = {"name": "shapes", "data": "none", "args": ["--shapes", "/src/shapes.ttl"], "queries": "",
               "metric": "shacl.p50_ms", "cap_gb": 2,
               "check": {"shacl.results": 3, "shacl.values": 3, "shacl.paths": 2}}


def shapes_run(scratch, report="", json_text='{"shacl": {"results": 3, "p50_ms": 1.0}}'):
    """A container that runs perf_lab with shapes: writes its JSON (None: none) and its SHACL
    report where the command says (--shacl-report /out/NAME); the commands it was given."""
    commands = []

    def measured(name, cap, command, env=None, timeout=None):
        commands.append(command[-1])
        if json_text is not None:
            (scratch / "shapes.json").write_text(json_text)
        found = re.search(r"'--shacl-report' '/out/([^']+)'", command[-1])
        if found and isinstance(report, bytes):
            (scratch / found.group(1)).write_bytes(report)
        elif found:
            (scratch / found.group(1)).write_text(report, encoding="utf-8")
        return 0, "", "", 1
    return measured, commands


def report(ms=1.0, status="ok", qps=None):
    records = []
    for rep in range(1, 4):
        # `inputs`: the frozen files it loaded, as every record says since 7 Nov 2026 (none here).
        records.append({"case": "micro", "rep": rep, "status": status, "metric": ms,
                        "samples": {"q": [ms] * 4}, "checks": [], "routes": [], "inputs": {},
                        **({"throughput": [{"clients": n, "qps": v} for n, v in qps.items()]}
                           if qps is not None else {})})
    return {"reps": 3, "records": records}


def comparison(base, new, **options):
    out = io.StringIO()
    with patch.object(fast, "load_report", side_effect=[base, new]), contextlib.redirect_stdout(out):
        code = fast.compare(SimpleNamespace(base="base", new="new", **options))
    return code, out.getvalue()


class FastComparison(unittest.TestCase):
    def test_missing_later_json_fails_even_after_both_cache_modes_were_observed(self):
        case = {"name": "partial", "data": "none", "args": [], "queries": "",
                "metric": "cold_ms", "cap_gb": 2, "repeat": 2}
        with tempfile.TemporaryDirectory() as directory:
            scratch, calls = Path(directory), []

            def measured(*args, **kwargs):
                calls.append(1)
                if len(calls) <= 2:
                    (scratch / "partial.json").write_text('{"open_s":1,"peak_mib":100}')
                return 0, "", "", 1

            with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", measured), \
                    patch.object(fast, "page_cache_modes", return_value=(["warm", "cold"], "")), \
                    patch.object(fast, "drop_page_cache", return_value=True):
                record = fast.measure(case, "partial", 1, {}, {}, "none")
        self.assertEqual(record["status"], "failed")
        self.assertEqual(set(record["cache_peaks"]), {"warm", "cold"})
        self.assertIn("invocation did not produce its JSON report", record["notes"])

    def test_cache_mode_memory_survives_measurement_and_compaction(self):
        case = {"name": "cache", "data": "none", "args": [], "queries": "",
                "metric": "cold_ms", "cap_gb": 2, "repeat": 2, "page_cache": ["warm", "cold"]}
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            peaks = iter([100, 200, 110, 210])

            def measured(*args, **kwargs):
                (scratch / "cache.json").write_text(json.dumps({"open_s": 1, "peak_mib": next(peaks),
                    "queries": [{"name": "q", "first_ms": 5, "p50_ms": 2}]}))
                return 0, "", "", 1

            with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", measured), \
                    patch.object(fast, "page_cache_modes", return_value=(["warm", "cold"], "")), \
                    patch.object(fast, "drop_page_cache", return_value=True):
                record = fast.measure(case, "cache", 1, {}, {}, "none")
        self.assertEqual(record["cache_peaks"], {"warm": [100, 110], "cold": [200, 210]})
        self.assertEqual(fast.compact_record(record)["cache_peaks"], record["cache_peaks"])
        self.assertEqual([o["identity"] for o in record["observations"]], fast.invocation_plan(case))
        self.assertEqual([o["peak_mib"] for o in record["observations"]], [100, 200, 110, 210])
        self.assertEqual(fast.compact_record(record)["observations"], record["observations"])
        self.assertEqual(record["metric"], 2010)  # original descriptive aggregate is unchanged
        for mode in ("warm", "cold"):
            self.assertEqual(record["samples"][f"{mode}/open"], [1000, 1000])
            self.assertEqual(record["samples"][f"{mode}/q/first"], [5, 5])
            self.assertEqual(record["samples"][f"{mode}/q/repeat"], [2, 2])
        self.assertEqual(fast.case_manifest([case])["cache_modes"], {"cache": ["warm", "cold"]})

    def test_a_shacl_case_counts_its_report_s_values_and_paths(self):
        """SHACL lead, 8 Nov 2026 (shacl-lubm-computed): the specified answer has 40,944 sh:value and
        20,472 sh:resultPath (SHACL 1.0 §5.3.2), which perf_lab doesn't count; its --shacl-report
        writes the report as N-Triples. A case with --shapes asks for it, counts them as
        shacl.values and shacl.paths, so a check can pin them, and leaves no report behind."""
        sh = "http://www.w3.org/ns/shacl#"
        report = "".join([f"_:r{i} <{sh}value> \"v{i}\" .\n" for i in range(3)]
                         + [f"_:r{i} <{sh}resultPath> <http://x/p{i}> .\n" for i in range(2)]
                         + [f"_:r0 <{sh}focusNode> <<( _:b <{sh}value> <http://x/o> )>> .\n",
                            f"_:v <{sh}conforms> \"false\"^^<http://www.w3.org/2001/XMLSchema#boolean> .\n"])
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            measured, commands = shapes_run(scratch, report=report)
            with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", measured):
                record = fast.measure({**SHAPES_CASE, "repeat": 2}, "shapes", 1, {}, {}, "none")
            left = sorted(p.name for p in scratch.iterdir())
        for observation in record["observations"]:
            self.assertEqual([(c["path"], c["got"], c["ok"]) for c in observation["checks"]],
                             [("shacl.results", 3, True), ("shacl.values", 3, True), ("shacl.paths", 2, True)])
        named = [re.search(r"'--shacl-report' '/out/([^']+)'", c).group(1) for c in commands]
        self.assertEqual(len(set(named)), 2)  # each invocation its own report
        self.assertEqual(left, [])
        self.assertEqual({k: record["counters"][k] for k in ("shacl.values", "shacl.paths")},
                         {"shacl.values": 3, "shacl.paths": 2})  # a triple term's sh:value isn't one

    def test_only_a_record_s_own_subject_and_predicate_are_counted(self):
        """Fix check r1 W (review-shacl-report-counts-r1): the writer escapes a literal's control
        characters but not U+2028, U+2029 or U+0085, where str.splitlines() also splits, so text
        inside a literal counted as a record of its own. Records end at a physical newline only;
        only their subject and predicate terms are read; a record that isn't N-Triples counts nothing."""
        sh = "http://www.w3.org/ns/shacl#"
        report = "".join(f"{line}\n" for line in [
            f'_:r0 <{sh}value> "a _:fake <{sh}value> y" .',
            f'_:r1 <{sh}value> "b <{sh}value> z" .',
            f'_:r2 <{sh}resultMessage> "c\u0085_:f <{sh}resultPath> <http://x/p> ." .',
            f'_:r3 <{sh}resultMessage> " <{sh}value> <{sh}resultPath> " .',
            f"_:r4 <{sh}focusNode> <<( _:b <{sh}value> <http://x/o> )>> .",
            f"_:r5 <{sh}resultPath> <http://x/p> .",
            "# a comment", ""])
        self.assertEqual(fast.shacl_report_counts(report), {"values": 2, "paths": 1})
        for broken in (f'"a literal" <{sh}value> "x" .', f"_:r0 {sh}value <x> .", f"<<( _:a <p> <o> )>> <{sh}value> <x> ."):
            with self.subTest(record=broken):
                self.assertIsNone(fast.shacl_report_counts(report + broken + "\n"))

    def test_counts_its_report_couldn_t_give_fail_their_checks_with_or_without_routes(self):
        """Fix check r2 (review-shacl-report-counts-r2): without --routes check() leaves an absent
        value unchecked (ok None), and measure() fails a case only on ok False, so a report that
        couldn't be parsed, with its values and paths pinned, ended ok. Couldn't be read isn't
        "not reported": the verdict is wrong, whatever --routes, and the note says why. A report
        the invocation didn't write is never read, though one is where an older harness put its own."""
        sh = "http://www.w3.org/ns/shacl#"
        unparseable = f'_:r0 <{sh}value> "x" .\n"a literal" <{sh}value> "y" .\n'
        stale = "".join([f"_:s{i} <{sh}value> \"v\" .\n" for i in range(3)]
                        + [f"_:s{i} <{sh}resultPath> <http://x/p> .\n" for i in range(2)])
        for routes in (False, True):
            for kind in ("unparseable", "not written"):
                with self.subTest(routes=routes, report=kind), tempfile.TemporaryDirectory() as directory:
                    scratch = Path(directory)
                    (scratch / "shapes.shacl.nt").write_text(stale, encoding="utf-8")  # an older harness's path
                    measured, _ = shapes_run(scratch, report=unparseable)
                    if kind == "not written":
                        def measured(name, cap, command, env=None, timeout=None):  # noqa: F811
                            (scratch / "shapes.json").write_text('{"shacl": {"results": 3, "p50_ms": 1.0}}')
                            return 0, "", "", 1
                    with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", measured), \
                            patch.dict(fast.OPTIONS, routes=routes):
                        record = fast.measure(SHAPES_CASE, "shapes", 1, {}, {}, "none")
                    self.assertEqual(fast.verdict(SHAPES_CASE, record)["verdict"], "wrong")
                    failed = {c["path"]: c for c in record["observations"][0]["checks"] if c["ok"] is not True}
                    self.assertEqual(sorted(failed), ["shacl.paths", "shacl.values"])
                    self.assertTrue(all(c["ok"] is False and "not counted" in c["note"] for c in failed.values()))

    def test_a_report_that_can_t_be_read_or_decoded_fails_its_counts_and_aborts_nothing(self):
        """Fix check r3 (review-shacl-report-counts-r3): a report that isn't UTF-8, or whose read
        fails, raised out of measure() before its counts were marked unreadable. Every way of not
        getting the report is one state: with values and paths pinned the verdict is wrong whatever
        --routes, each count's check fails with the reason; a case that pins neither is unaffected,
        ok; and the report is removed either way."""
        sh = "http://www.w3.org/ns/shacl#"
        undecodable = f'_:r0 <{sh}value> "'.encode() + b'\xff\xfe' + f'" .\n_:r0 <{sh}resultPath> <http://x/p> .\n'.encode()
        unpinned = {**SHAPES_CASE, "check": {"shacl.results": 3}}
        opened = Path.open

        def failing_read(path, mode="r", *args, **kwargs):  # the disk, not the report, fails
            if path.name.endswith(".shacl.nt") and "r" in mode:
                raise OSError(5, "Input/output error", str(path))
            return opened(path, mode, *args, **kwargs)

        for routes in (False, True):
            for failure, reason in (("invalid UTF-8", "isn't UTF-8"), ("read error", "couldn't be read")):
                for case, verdict in ((SHAPES_CASE, "wrong"), (unpinned, "ok")):
                    pinned = case is SHAPES_CASE
                    with self.subTest(routes=routes, failure=failure, pinned=pinned), \
                            tempfile.TemporaryDirectory() as directory:
                        scratch = Path(directory)
                        measured, _ = shapes_run(scratch, report=undecodable if failure == "invalid UTF-8" else "")
                        with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", measured), \
                                patch.dict(fast.OPTIONS, routes=routes), \
                                (patch.object(Path, "open", failing_read) if failure == "read error"
                                 else contextlib.nullcontext()):
                            try:
                                record = fast.measure(case, "shapes", 1, {}, {}, "none")
                                got = fast.verdict(case, record)["verdict"]
                            except Exception as error:  # noqa: BLE001 - the defect is that it raises
                                record, got = None, f"raised {type(error).__name__}"
                        self.assertEqual(got, verdict)
                        self.assertEqual(sorted(p.name for p in scratch.iterdir() if p.name.endswith(".nt")), [])
                        checks = {c["path"]: c for c in record["observations"][0]["checks"]}
                        if pinned:
                            failed = {p: c for p, c in checks.items() if c["ok"] is not True}
                            self.assertEqual(sorted(failed), ["shacl.paths", "shacl.values"])
                            self.assertTrue(all(c["ok"] is False and reason in c["note"] for c in failed.values()))
                        else:
                            self.assertEqual([(p, c["ok"]) for p, c in checks.items()], [("shacl.results", True)])
                        self.assertTrue(any(reason in note for note in record["notes"]))

    def test_a_report_is_removed_however_its_invocation_ends_and_a_stale_one_refused(self):
        """Fix check r1 (lifecycle): the report's path was one per checkout and stayed behind when an
        invocation wrote no JSON or a broken one; it is the invocation's own, removed in every case,
        and one already there is refused, never counted or removed."""
        for ending in ("no json", "broken json"):
            with self.subTest(ending=ending), tempfile.TemporaryDirectory() as directory:
                scratch = Path(directory)
                measured, _ = shapes_run(scratch, json_text=None if ending == "no json" else "{")
                with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", measured):
                    if ending == "no json":
                        self.assertEqual(fast.measure(SHAPES_CASE, "shapes", 1, {}, {}, "none")["status"], "failed")
                    else:
                        with self.assertRaises(json.JSONDecodeError):
                            fast.measure(SHAPES_CASE, "shapes", 1, {}, {}, "none")
                self.assertEqual(sorted(p.name for p in scratch.iterdir() if p.name.endswith(".nt")), [])
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            measured, commands = shapes_run(scratch)
            fixed = SimpleNamespace(hex="0123456789abcdef")
            stale = scratch / "shapes-0123456789ab-0.shacl.nt"
            stale.write_text("_:old <http://www.w3.org/ns/shacl#value> 1 .\n", encoding="utf-8")
            with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", measured), \
                    patch("uuid.uuid4", return_value=fixed):
                record = fast.measure(SHAPES_CASE, "shapes", 1, {}, {}, "none")
            self.assertEqual(record["status"], "failed")
            self.assertEqual(commands, [])
            self.assertTrue(stale.exists())

    def test_single_client_level_survives_measurement_and_compaction(self):
        clients = {"clients": 32, "qps": 100, "p99_ms": 1, "memory": {"peak_mib": 64}}
        case = {"name": "single", "data": "none", "args": ["--clients", "32"],
                "queries": "", "metric": "clients.p99_ms", "cap_gb": 2}
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)

            def measured(*args, **kwargs):
                (scratch / "single.json").write_text(json.dumps({"clients": clients}))
                return 0, "", "", 1

            with patch.object(fast, "SCRATCH", scratch), patch.object(fast, "container", measured):
                record = fast.measure(case, "single", 1, {}, {}, "none")
        self.assertEqual(record["status"], "ok")
        self.assertEqual(record["throughput"][0]["qps"], 100)
        self.assertEqual(fast.compact_record(record)["throughput"], record["throughput"])
        self.assertEqual(record["throughput"], fast.client_throughput({"client_sweep": [clients]}))
        clients.pop("memory")
        self.assertIsNone(fast.client_throughput({"clients": clients})[0]["qps_per_gb"])

    def test_old_full_single_level_report_cannot_hide_lower_qps_behind_faster_tail(self):
        base, new = report(1), report(0.5)
        for data, qps in ((base, 100), (new, 50)):
            for record in data["records"]:
                record["result"] = {"clients": {"clients": 32, "qps": qps, "p99_ms": record["metric"]}}
        code, text = comparison(base, new)
        self.assertEqual(code, 1)
        self.assertIn("32 clients QPS: 100 -> 50", text)
        self.assertIn("LOWER QPS", text)
        self.assertIn("faster", text)
        new["records"][1]["throughput"] = []
        code, text = comparison(base, new)
        self.assertEqual(code, 1)
        self.assertIn("QPS: NOT COMPARABLE", text)

    def test_lost_case_fails_but_added_case_is_not_a_regression(self):
        empty = {"reps": 3, "records": []}
        code, text = comparison(report(), empty)
        self.assertEqual(code, 1)
        self.assertIn("LOST case", text)
        code, text = comparison(empty, report())
        self.assertEqual(code, 0)
        self.assertIn("without baseline", text)

    def test_missing_client_evidence_on_both_sides_is_unqualified(self):
        for raw, explicit in ((True, True), (True, False), (False, False)):
            with self.subTest(raw=raw, explicit=explicit):
                data = report()
                for record in data["records"]:
                    record["case"] = "clients-lookups"
                    if raw:
                        record["result"] = {"clients": {"clients": 32}}
                    if explicit:
                        record["throughput"] = []
                code, text = comparison(data, copy.deepcopy(data))
                self.assertEqual(code, 1)
                self.assertIn("QPS: NOT COMPARABLE", text)

    def test_explicit_throughput_owns_incomplete_raw_and_failed_status(self):
        data = report(qps={32: 100})
        for record in data["records"]:
            record["result"] = {"clients": {"clients": 32}}
        self.assertEqual(comparison(data, copy.deepcopy(data))[0], 0)
        for record in data["records"]:
            record["status"] = "failed"
            record["throughput"] = []
        code, text = comparison(data, copy.deepcopy(data))
        self.assertEqual(code, 1)
        self.assertIn("failed checks/status", text)

    def test_duplicate_width_cannot_replace_a_missing_repetition(self):
        data = report(qps={32: 100})
        data["records"][0]["throughput"] *= 2
        data["records"][1]["throughput"] = []
        code, text = comparison(data, copy.deepcopy(data))
        self.assertEqual(code, 1)
        self.assertIn("QPS: NOT COMPARABLE", text)

    def test_same_missing_width_on_both_sides_is_not_intersected_away(self):
        for raw in (True, False):
            data = report(qps={1: 100})
            for record in data["records"]:
                if raw:
                    record["result"] = {"client_sweep": [{"clients": 1}, {"clients": 8}]}
                else:
                    record["samples"] = {"1 clients p99": [1], "8 clients p99": [1]}
            code, text = comparison(data, copy.deepcopy(data))
            self.assertEqual(code, 1)
            self.assertIn("8 clients QPS: NOT COMPARABLE", text)

    def test_matching_failures_never_acquire_a_timing_verdict(self):
        for status in ("failed", "wrong", "off-route", "timeout", "unsupported"):
            with self.subTest(status=status):
                code, text = comparison(report(100, status), report(1, status))
                self.assertEqual(code, 1)
                self.assertIn("NOT COMPARABLE", text)
                self.assertNotIn("faster", text)

    def test_expected_frontiers_are_recorded_without_performance_credit(self):
        base = report(100, "memory-limit")
        for r in base["records"]:
            r["expected_outcome"] = "memory-limit"
        code, text = comparison(base, copy.deepcopy(base))
        self.assertEqual(code, 0)
        self.assertIn("known issue; no timing verdict", text)

    def test_failed_check_or_route_excludes_an_ok_record(self):
        for key in ("checks", "routes"):
            new = report(0.1)
            new["records"][1][key] = [{"ok": False}]
            code, text = comparison(report(100), new)
            self.assertEqual(code, 1)
            self.assertIn("NOT COMPARABLE", text)

    def test_missing_samples_and_series_do_not_get_intersected_away(self):
        for samples in ({}, {"other": [1]}, {"q": []}, {"q": [float("nan")]}, {"q": ["bad"]}):
            new = report()
            new["records"][1]["samples"] = samples
            code, text = comparison(report(), new)
            self.assertEqual(code, 1)
            self.assertIn("missing/invalid timing", text)

    def test_missing_declared_repetition_fails(self):
        new = report()
        new["records"].pop()
        code, text = comparison(report(), new)
        self.assertEqual(code, 1)
        self.assertIn("missing/invalid timing series or repetitions", text)

    def test_missing_expected_frontier_repetition_also_fails(self):
        base = report(status="unsupported")
        for r in base["records"]:
            r["expected_outcome"] = "unsupported"
        new = copy.deepcopy(base)
        new["records"].pop()
        self.assertEqual(comparison(base, new)[0], 1)

    def test_microcase_floor_is_explicit_and_keeps_the_default(self):
        code, text = comparison(report(0.1), report(0.2))
        self.assertEqual(code, 0)
        self.assertIn("slower, below absolute floor", text)
        self.assertIn("latency floor 2 ms", text)
        code, text = comparison(report(0.1), report(0.2), min_ms=0)
        self.assertEqual(code, 1)
        self.assertIn("SLOWER", text)
        self.assertIn("latency floor 0 ms", text)
        for floor in (-1, float("nan"), float("inf")):
            with self.assertRaises(ValueError):
                comparison(report(), report(), min_ms=floor)

    def test_latency_noise_still_prevents_a_microcase_verdict(self):
        new = report(0.2)
        for r, value in zip(new["records"], (0.05, 0.2, 0.5)):
            r["metric"], r["samples"] = value, {"q": [value] * 4}
        self.assertEqual(comparison(report(0.1), new, min_ms=0)[0], 0)

    def test_qps_is_higher_is_better_per_width_independent_of_latency_floor(self):
        base = report(qps={1: 100, 8: 100})
        code, text = comparison(base, report(qps={1: 200, 8: 50}))
        self.assertEqual(code, 1)
        self.assertIn("1 clients QPS: 100 -> 200", text)
        self.assertIn("higher QPS", text)
        self.assertIn("8 clients QPS: 100 -> 50", text)
        self.assertIn("LOWER QPS", text)
        self.assertIn("95% CI", text)
        self.assertEqual(comparison(base, report(qps={1: 200, 8: 200}))[0], 0)

    def test_lost_width_or_missing_repetition_is_not_a_throughput_win(self):
        base = report(qps={1: 100, 8: 100})
        for new in (report(qps={1: 100}), report(qps={1: 100, 8: 100})):
            new["records"][1]["throughput"] = []
            code, text = comparison(base, new)
            self.assertEqual(code, 1)
            self.assertIn("QPS: NOT COMPARABLE", text)

    def test_runs_across_a_host_configuration_change_are_refused_unless_flagged(self):
        # A host registry of the test's own: hosts.toml's epochs are operational records.
        registry = {"host": {"phuoc-yu": {"hostname": "Phuoc-Yu", "config": [{"since": "2026-10-09"},
                                                                             {"since": "2026-10-12T13:00"}]}}}
        self.enterContext(patch.object(fast.hosts, "load", return_value=registry))

        def on(started, governor=None):
            data = report(1)
            data["machine"] = {"host": "Phuoc-Yu", **({"governor": governor} if governor else {})}
            data["started"] = started
            return data
        before, after = on("2026-10-12T10:51:30"), on("2026-10-12T14:00:00")
        code, text = comparison(before, after)
        self.assertEqual(code, 2)
        self.assertIn("changed its configuration", text)
        self.assertIn("REFUSED", text)
        code, text = comparison(before, after, allow_host_change=True)
        self.assertEqual(code, 0)
        self.assertIn("FLAGGED", text)
        # One configuration: compared; a governor the runs read differently: refused.
        self.assertEqual(comparison(on("2026-10-12T14:00:00"), after)[0], 0)
        code, text = comparison(on("2026-10-12T14:00:00", "powersave"), on("2026-10-12T15:00:00", "performance"))
        self.assertEqual(code, 2)
        self.assertIn("governors", text)

    def test_qps_noise_is_taken_from_qps_not_latency(self):
        new = report(qps={1: 50})
        for r, value in zip(new["records"], (20, 50, 150)):
            r["throughput"][0]["qps"] = value
        code, text = comparison(report(qps={1: 100}), new)
        self.assertEqual(code, 0)
        self.assertIn("within noise", text)


class FastVerdicts(unittest.TestCase):
    def test_every_run_has_a_verdict_and_names_whose_failure(self):
        case = {"name": "c", "known": "the budget bounds the quads"}
        def of(status, expected="ok", notes=()):
            return fast.verdict(case, {"status": status, "expected_outcome": expected, "notes": list(notes)})
        self.assertEqual(of("ok"), {"verdict": "ok", "whose": "-"})
        self.assertEqual(of("off-route")["verdict"], "wrong")
        # A known limit is the product's issue, named, though it fails no gate.
        limit = of("memory-limit", "memory-limit")
        self.assertEqual((limit["verdict"], limit["whose"], limit["issue"]),
                         ("incomplete", "product", "the budget bounds the quads"))
        missing = of("failed", notes=["missing input: ore_ont_9724.owl"])
        self.assertEqual((missing["verdict"], missing["whose"]), ("incomplete", "harness"))
        self.assertEqual(of("failed", notes=["exit 101"])["whose"], "unknown")
        self.assertNotIn("issue", of("wrong", "ok"))


class FastNative(unittest.TestCase):
    DIRS = {"/target": Path("/bin/t"), "/fast": Path("/g"), "/data": Path("/assets/data"), "/src": Path("/repo"),
            "/out": Path("/repo/tmp/fast"), "/inputs": Path("/assets/fast"), "/ore": Path("/assets/ore")}

    def test_container_paths_map_to_the_hosts_directories_and_nothing_else(self):
        self.assertEqual(fast.native("/target/release/examples/perf_lab --load /data/lubm-10.nt --json /out/c.json",
                                     self.DIRS),
                         "/bin/t/release/examples/perf_lab --load /assets/data/lubm-10.nt --json /repo/tmp/fast/c.json")
        script = "rm -rf /fast/scratch/c && mkdir -p /fast/scratch/c && cp '/inputs/x.nt' /fast/"
        self.assertEqual(fast.native(script, self.DIRS),
                         "rm -rf /g/scratch/c && mkdir -p /g/scratch/c && cp '/assets/fast/x.nt' /g/")
        # Not a container path: inside another path, a URL, or a longer name.
        for text in ("/home/u/data/x", "http://example.org/data/x", "/database", "a/fast/b"):
            self.assertEqual(fast.native(text, self.DIRS), text)

    def test_the_scopes_peak_is_read_from_the_wrappers_line(self):
        self.assertEqual(fast.scope_peak_mib("warning\nfast-scope-peak 2147483648\n"), 2048)
        self.assertIsNone(fast.scope_peak_mib("Killed\n"))


class FastPageCache(unittest.TestCase):
    def test_invocation_plan_distinguishes_processes_from_internal_repeats(self):
        case = {"name": "s", "metric": "open_s", "repeat": 2, "sweep": [["--x", "1"], ["--x", "2"]],
                "page_cache": ["warm", "cold"]}
        plan = fast.invocation_plan(case)
        self.assertEqual(len(plan), 8)
        self.assertEqual(plan[-1], {"sweep": 1, "repeat": 1, "cache": "cold"})
        self.assertEqual(fast.case_manifest([case])["invocation_series"]["s"], [["_"]] * 8)
        self.assertEqual(len(fast.invocation_plan({**case, "tool": "tableau"})), 1)

    def test_each_invocation_keeps_its_counters_clients_and_scope_peak(self):
        case = {"metric": "clients.p99_ms"}
        result = {"nodes_created": 7, "clients": {"clients": 4, "qps": 30, "p99_ms": 2},
                  "phases": {"query": {"peak_mib": 16}}}
        observation = fast.invocation_observation(case, result, "fast-scope-peak 33554432\n", {})
        self.assertEqual(observation["counters"]["nodes_created"], 7)
        self.assertEqual(observation["throughput"][0]["qps"], 30)
        self.assertEqual(observation["cgroup_peak_mib"], 32)
        self.assertEqual(observation["phase_peaks"], {"query": 16})

    def test_warm_and_cold_are_series_of_their_own_and_the_metric_sums_their_medians(self):
        runs = [{"open_s": 0.010, "queries": [{"name": "q", "first_ms": 1.0, "p50_ms": 0.5}], "_page_cache": mode}
                for mode in ("warm", "cold", "warm", "cold", "warm", "cold")]
        for run, extra in zip(runs, (0, 30, 0, 50, 0, 40)):
            run["queries"][0]["first_ms"] += extra
        result = {**runs[-1], "_repeats": runs}
        self.assertEqual(fast.cold_series(result), {"warm": [11.0] * 3, "cold": [41.0, 61.0, 51.0]})
        self.assertEqual(fast.metric_value({"metric": "cold_ms"}, result), 11.0 + 51.0)
        one = {**runs[0], "_repeats": [runs[0], runs[2]]}
        self.assertEqual(fast.samples({"metric": "cold_ms"}, one),
                         {"warm/open": [10.0, 10.0], "warm/q/first": [1.0, 1.0], "warm/q/repeat": [0.5, 0.5]})

    def test_a_host_without_a_cold_cache_command_runs_warm_and_says_so(self):
        with patch.object(fast, "cold_cache_command", lambda: None):
            modes, why = fast.page_cache_modes({"page_cache": ["warm", "cold"]})
        self.assertEqual(modes, ["warm"])
        self.assertIn("cold_cache", why)
        with patch.object(fast, "cold_cache_command", lambda: "true"):
            self.assertEqual(fast.page_cache_modes({"page_cache": ["warm", "cold"]}), (["warm", "cold"], ""))


class AbBase(unittest.TestCase):
    """An A/B's base side: off its case's route is a measured run there (`--off-route-ok`),
    a failed run never is (sparql-memory-ab-phuoc-2: every base pass failed in 0.0 s, the
    job finished done)."""

    def test_off_route_is_measured_on_the_base_only(self):
        records = [{"case": "op-group", "status": "off-route"}, {"case": "q-star", "status": "ok"}]
        self.assertEqual([r["case"] for r in fast.not_as_expected(records)], ["op-group"])
        self.assertEqual(fast.not_as_expected(records, off_route_ok=True), [])

    def test_failed_runs_fail_either_side(self):
        records = [{"case": "q-star", "status": "failed"}, {"case": "q-chain", "status": "wrong"},
                   {"case": "q-limit", "status": "failed", "expected_outcome": "failed"}]
        for off_route_ok in (False, True):
            bad = fast.not_as_expected(records, off_route_ok=off_route_ok)
            self.assertEqual([r["case"] for r in bad], ["q-star", "q-chain"])


if __name__ == "__main__":
    unittest.main()
