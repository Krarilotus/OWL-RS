"""Fast report counters accept structured metadata without changing numeric work rules."""
import importlib.util
import contextlib
import copy
import io
import json
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


def report(ms=1.0, status="ok", qps=None):
    records = []
    for rep in range(1, 4):
        records.append({"case": "micro", "rep": rep, "status": status, "metric": ms,
                        "samples": {"q": [ms] * 4}, "checks": [], "routes": [],
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
