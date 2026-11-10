"""The release gate (benches/fast/release_gate.py, suitekit/gatestats.py) and the block runs of
fast.py it relies on.

Each defect of the audits has a test here. 18 Oct 2026: a gate passed with one baseline and
overlapping timestamps; main's perf lab reports no samples (its side compared by per-process
p50s); the cgroup's peak counts page cache. Research round 8 (19 Oct 2026): a palindrome of
whole passes isn't time-balanced; the layout rotation skipped layouts; layouts weren't the
statistical unit; a loss first seen while confirming another was dropped; a difference within
the counter's step was neutral, untested; the Windows peak was mislabelled; the big host was
native Windows. lks (20 Oct 2026): blocks timing one cached instance of a build share its
offset, so a round's blocks weren't independent; the order was fixed in advance; no test knew the
host's noise between instances.
"""
from __future__ import annotations

import argparse
import contextlib
import copy
import importlib.util
import io
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
sys.path.insert(0, str(ROOT / "benches" / "fast"))
spec = importlib.util.spec_from_file_location("release_gate", ROOT / "benches/fast/release_gate.py")
gate_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gate_module)
import fast  # noqa: E402
import isolation  # noqa: E402
from suitekit import gatestats  # noqa: E402

def LOCAL_TABLES(checkout, harness):  # noqa: N802
    """A fake harness (a test's "c" * 40 in "C:/repo") reads as this tree's dataset metadata."""
    return gate_module.local_tables()


def commit_all(repo: Path) -> str:
    """Everything at `repo` committed in a repository of the test's own; the commit."""
    git = lambda *a: subprocess.run(["git", "-C", str(repo), *a], capture_output=True, text=True,  # noqa: E731
                                    check=True, timeout=60).stdout.strip()
    if not (repo / ".git").exists():
        git("init", "-q")
    git("add", "-A")
    git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "harness")
    return git("rev-parse", "HEAD")


LUBM_MAKER = "bash benches/reasoning/prepare-lubm.sh"


MAKER_SCRIPTS = (*isolation.NS_SH, "benches/reasoning/prepare-lubm.sh", "benches/reasoning/prepare-owl2bench.sh",
                 "benches/sparql/prepare.sh")  # ns.sh with its lock (isolation.NS_SH)


def harness_commit(repo: Path, lubm_size: tuple | None = (4, 8), lubm_made_by: str | None = None,
                   tables: str | None = None, catalogue: bool = True, helper: bool = True,
                   sparql_size: bool = True, drop: tuple = (), legacy: bool = False, after: str = "") -> str:
    """A harness of the test's own whose dataset metadata (its catalogue's [makers], its catalogue,
    its makers' scripts) differs from this tree's as asked: its LUBM maker's container size (None:
    none), the LUBM files made otherwise (`lubm_made_by`), no catalogue, makers without ns_maker_args
    (`helper`), no SPARQL size, catalogue entries dropped, or more release_gate.py source (`after`).
    `legacy` (or `tables`, its text whole; or no catalogue): a harness from before [makers], its
    tables as code in its release_gate.py, its LUBM size as MAKER_CONTAINERS there (as this package's
    commits before [makers] had it)."""
    (repo / "benches/fast").mkdir(parents=True)
    for rel in MAKER_SCRIPTS:
        (repo / rel).parent.mkdir(parents=True, exist_ok=True)
        text = (ROOT / rel).read_text(encoding="utf-8")
        (repo / rel).write_text(text if helper else text.replace("ns_maker_args", "ns_run_args"), encoding="utf-8")
    legacy = legacy or tables is not None or not catalogue
    sizes = {LUBM_MAKER: lubm_size, "bash benches/reasoning/prepare-owl2bench.sh": (4, 8),
             "bash benches/sparql/prepare.sh": (8, 8) if sparql_size else None}
    if legacy:
        table = "{" + ", ".join(f'"{k}": {v!r}' for k, v in sizes.items() if v) + "}"
        code = (f'DATASET_MAKERS = {{"{LUBM_MAKER}": ["docker"], "bash benches/reasoning/prepare-owl2bench.sh": '
                '["docker"], "perf_lab generate support-stress": []}\n'
                'FORM_ONLY = {"bash benches/reasoning/prepare-owl2bench.sh"}\nSPARQL_PREPARE = "benches/sparql/prepare.sh"\n'
                + (f"MAKER_CONTAINERS = {table}\n" if lubm_size else ""))
    else:
        code = "# a harness's release_gate.py: its makers are its catalogue's [makers]\n"
    (repo / "benches/fast/release_gate.py").write_text((tables if tables is not None else code) + after,
                                                       encoding="utf-8")
    if catalogue:
        import tomllib
        entries = tomllib.loads((ROOT / "benches/datasets.toml").read_text(encoding="utf-8"))["dataset"]
        lines = []
        if not legacy:
            makers = {LUBM_MAKER: {"needs": ["docker"]}, "bash benches/reasoning/prepare-owl2bench.sh":
                      {"needs": ["docker"], "form_only": True}, "bash benches/sparql/prepare.sh": {"sparql": True},
                      "perf_lab generate support-stress": {"needs": []}}
            for start, maker in makers.items():
                if sizes.get(start):
                    maker["container"] = list(sizes[start])
                lines += [f"[makers.{gate_module.toml_value(start)}]"] + [
                    f"{k} = {gate_module.toml_value(v)}" for k, v in maker.items()] + [""]
        for entry in entries:
            if entry["file"] in drop:
                continue
            if lubm_made_by and entry.get("made_by", "").startswith(LUBM_MAKER + " "):
                entry = {**entry, "made_by": lubm_made_by}
            lines += ["[[dataset]]"] + [f"{k} = {gate_module.toml_value(v)}" for k, v in entry.items()] + [""]
        (repo / "benches/datasets.toml").write_text("\n".join(lines), encoding="utf-8")
    return commit_all(repo)


HARNESS = "h" * 40
REVS = {"cand": "c" * 40, "base": "b" * 40, "main": "m" * 40}
SIDES = ("base", "main", "cand")
MACHINE = {"host": "phuoc-yu", "cpu": "Ryzen", "governor": "performance", "fleet_host": "phuoc-yu"}
CONTEXT = {"host": "phuoc-yu", "round": 0, "prefix": "rg-cccccccc", "max_gb": 24.0, "ram_gb": 24.0}


def remove_checkout(tree, repo=ROOT) -> subprocess.CompletedProcess:
    """A checkout a test made, removed by its path alone (never a `prune`). With core.longpaths: a
    job's temp root on a Windows host can put the repository's deepest files past 260 characters,
    where git without it can't delete them (khaleesi's Windows tier, 2 Nov 2026: "Filename too long";
    other platforms ignore the setting)."""
    return subprocess.run(["git", "-c", "core.longpaths=true", "worktree", "remove", "--force", Path(tree).as_posix()],
                          cwd=repo, capture_output=True, text=True)


def remove_checkouts_under(root, repo=ROOT) -> list[str]:
    """Every checkout registered under `root`, a test's own temporary directory, removed by its path
    (remove_checkout()); those that stayed, with git's reason. Never a `git worktree prune`: in the
    repository the suite runs in it drops any registration whose directory this process can't see,
    other agents' live checkouts among them (rule.delete-only-what-you-created; 2 Nov 2026)."""
    listed = subprocess.run(["git", "worktree", "list", "--porcelain"], cwd=repo, capture_output=True, text=True)
    base = Path(root).resolve()
    left = []
    for line in listed.stdout.splitlines():
        if not line.startswith("worktree "):
            continue
        tree = Path(line[len("worktree "):]).resolve()
        if tree == base or base in tree.parents:
            removed = remove_checkout(tree, repo)
            if removed.returncode:
                left.append(f"{tree}: {removed.stderr.strip()}")
    return left


class Checkouts(unittest.TestCase):
    def test_a_test_s_checkout_past_the_windows_path_limit_is_removed(self):
        """khaleesi's Windows tier, 2 Nov 2026: a test's harness checkout under the job's deep temp
        root stayed registered ("Filename too long"), failing the test that made it."""
        if os.name != "nt":
            self.skipTest("the 260-character limit is Windows'")
        with tempfile.TemporaryDirectory() as tmp:
            git = lambda *a, cwd=tmp: subprocess.run(["git", "-c", "core.longpaths=true", *a], cwd=cwd,  # noqa: E731
                                                       capture_output=True, text=True, check=True)
            repo = Path(tmp, "r")
            repo.mkdir()
            git("init", "-q", cwd=repo)
            deep = Path(*(["d" * 40] * 6), "f.txt")  # 250 characters inside the checkout
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base", cwd=repo)
            tree = Path(tmp, "w" * 30)
            git("worktree", "add", "-q", tree.as_posix(), cwd=repo)
            (tree / deep).parent.mkdir(parents=True)
            (tree / deep).write_text("x", encoding="utf-8")
            git("add", "-A", cwd=tree)
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "deep", cwd=tree)
            self.assertGreater(len(str((tree / deep).resolve())), 260)
            removed = remove_checkout(tree, repo)
            self.assertEqual(removed.returncode, 0, removed.stderr)
            self.assertFalse(tree.exists())

    def test_a_test_removes_only_its_own_checkouts_never_prunes(self):
        """Orchestrator, 2 Nov 2026 (rule.delete-only-what-you-created): three tests ran `git worktree
        prune` in the repository the suite runs in, which drops every registration whose directory
        this process can't see, another agent's live checkout among them. A test removes the
        checkouts under its own temporary directory, by path, and nothing else."""
        with tempfile.TemporaryDirectory() as tmp:
            git = lambda *a, cwd=tmp: subprocess.run(["git", *a], cwd=cwd, capture_output=True, text=True,  # noqa: E731
                                                       check=True).stdout
            repo = Path(tmp, "r")
            repo.mkdir()
            git("init", "-q", cwd=repo)
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base", cwd=repo)
            own, other = Path(tmp, "own"), Path(tmp, "elsewhere")
            git("worktree", "add", "-q", "--detach", (own / "gate" / "src-1").as_posix(), cwd=repo)
            git("worktree", "add", "-q", "--detach", (other / "src-2").as_posix(), cwd=repo)
            shutil.rmtree(other)  # another space's checkout: registered, its directory not visible here
            self.assertEqual(remove_checkouts_under(own, repo), [])
            listed = git("worktree", "list", "--porcelain", cwd=repo)
            self.assertNotIn("src-1", listed)
            self.assertIn("src-2", listed, "another's registration was dropped")


def observation(samples, peak=500, cgroup=900, estimator="samples", rows=None, phases=None, anon=None):
    out = {"identity": {"sweep": 0, "repeat": 0, "cache": "warm"}, "exit": 0, "samples": samples,
           "estimator": estimator, "peak_mib": peak, "phase_peaks": phases or {}, "cgroup_peak_mib": cgroup,
           "rows": rows or {q: 5 for q in samples}, "checks": [], "routes": []}
    if anon is not None:
        out["anon_kernel_peak_bytes"] = anon
    return out


def record(case="lab-olympics", samples=None, status="ok", expected="ok", **kw):
    samples = samples if samples is not None else {"q01": [10.0, 10.1, 9.9, 10.0, 10.2], "q06": [4.8, 4.9, 4.8]}
    obs = observation(samples, **kw)
    return {"case": case, "rep": 1, "status": status, "expected_outcome": expected, "checks": [], "routes": [],
            "samples": samples, "observations": [obs], "peak_mib": obs["peak_mib"], "notes": [], "counters": {}}


def report(side, n, records, options=None):
    rev = REVS[side]
    binaries = {"revision": rev, "verified": True, "sha256": {"release/examples/perf_lab": side * 8}}
    return {"label": f"gate-{n}-{side}", "started": "2026-10-20T01:00:00", "finished": "2026-10-20T01:10:00",
            "reps": 1, "planned_cases": sorted({r["case"] for r in records}), "records": records,
            "git": {"commit": HARNESS, "dirty": 0}, "git_finished": {"commit": HARNESS, "dirty": 0},
            "binaries": binaries, "binaries_finished": copy.deepcopy(binaries), "machine": MACHINE,
            "instance": {"built": f"/gate/bin-{side}", "inode": n},
            "runtime": "native", "prepared": {"manifest_sha256": "p" * 64, "files": 3, "changed": []},
            "options": options or {"prepared": True, "routes": True, "empty_queries": False}}


def passes(records_of=None, blocks=2, layouts=None, sides=SIDES, order_of=None):
    """A round as fast.py block writes it: per block one report per side and run, each case's six
    runs back to back as a palindrome (`order_of(block)` another order). records_of(side, n) gives
    a run's records, n its pass number (6 (block - 1) + its place)."""
    records_of = records_of or (lambda side, n: [record()])
    out, t = [], 1000
    for b in range(1, blocks + 1):
        layout = layouts[b - 1] if layouts else 1
        order = order_of(b) if order_of else fast.palindrome(list(sides), b)
        runs = {}
        for i, (side, run) in enumerate(order):
            n = len(order) * (b - 1) + i + 1
            runs[(side, run)] = (n, report(side, n, records_of(side, n)))
        for case in dict.fromkeys(r["case"] for n, rep in runs.values() for r in rep["records"]):
            for side, run in order:
                for rec in runs[(side, run)][1]["records"]:
                    if rec["case"] == case:
                        rec.update(started_ns=t, finished_ns=t + 10, block=b, layout=layout, side=side, run=run)
                        t += 10
        for (side, run), (n, rep) in runs.items():
            rep.update(block=b, run=run, layout=layout, side=side,
                       started_ns=min(r["started_ns"] for r in rep["records"]),
                       finished_ns=max(r["finished_ns"] for r in rep["records"]))
            if layout > 1:
                rep["binaries"] = {**rep["binaries"], "layout": layout,
                                   "sha256": {"release/examples/perf_lab": f"{side}{layout}"}}
                rep["binaries_finished"] = copy.deepcopy(rep["binaries"])
            out.append({"n": n, "side": side, "path": f"pass-{n}-{side}.json", "report": rep})
        t += 100
    return sorted(out, key=lambda p: p["n"])


def run_gate(ps, sides=("cand", "base", "main"), require=("base", "main"), **kw):
    kw.setdefault("context", CONTEXT)
    return gate_module.gate(ps, {s: REVS[s] for s in sides}, "cand", list(require), HARNESS, **kw)


def worse(factor, n):
    """The candidate's run n: `factor` give or take a few percent of it, so blocks' contrasts
    differ (identical ones tie: their p is 2^-blocks); no difference stays none."""
    return factor ** (1 + 0.05 * ((n * 7) % 5 - 2))


def slower(factor, series="q06", base=(4.8, 4.85, 4.8)):
    """records_of: the candidate's `series` about `factor` times the baselines'."""
    def records_of(side, n):
        values = [v * (worse(factor, n) if side == "cand" else 1) for v in base]
        return [record(samples={"q01": [10.0, 10.1, 9.9], series: values})]
    return records_of


class Schedule(unittest.TestCase):
    def test_identical_sides_pass(self):
        summary = run_gate(passes())
        self.assertEqual((summary["errors"], summary["blocking"], summary["flagged"]), ([], [], []))
        self.assertTrue(summary["passed"])

    def test_one_baseline_never_passes_a_gate_that_requires_two(self):
        summary = run_gate(passes(sides=("base", "cand")), sides=("cand", "base"))
        self.assertIn("required baselines missing: main", summary["errors"])
        self.assertFalse(summary["passed"])

    def test_each_case_s_runs_are_a_back_to_back_palindrome(self):
        """Round 8: a palindrome of whole passes isn't time-balanced (a case's runs on each side
        sit where the other cases' durations put them); its six runs back to back, mirrored, are."""
        def two(side, n):
            return [record(), record(case="kernel-intersect", samples={"triangles": [50.0]})]
        whole_passes = passes(two)
        t = 10 ** 6
        for p in whole_passes:  # each run runs every case before the next run starts
            for r in p["report"]["records"]:
                r.update(started_ns=t, finished_ns=t + 10)
                t += 10
            p["report"].update(started_ns=p["report"]["records"][0]["started_ns"], finished_ns=t)
        self.assertTrue(any("aren't back to back" in e for e in run_gate(whole_passes)["errors"]))
        cyclic = passes(order_of=lambda b: [(s, 1) for s in SIDES] + [(s, 2) for s in SIDES])
        self.assertTrue(any("not a palindrome" in e for e in run_gate(cyclic)["errors"]))

    def test_overlapping_blocks_are_refused(self):
        ps = passes()
        for p in ps[6:]:
            for r in p["report"]["records"]:
                r["started_ns"] -= 130
                r["finished_ns"] -= 130
        self.assertIn("blocks 1 and 2 overlap", run_gate(ps)["errors"])

    def test_fewer_blocks_than_the_round_are_diagnostic_only(self):
        one = passes(blocks=1)
        self.assertTrue(any("1 blocks, at least 2" in e for e in run_gate(one)["errors"]))
        diagnostic = run_gate(one, min_blocks=1)
        self.assertEqual(diagnostic["errors"], [])
        self.assertTrue(diagnostic["diagnostic"])
        self.assertFalse(diagnostic["passed"])

    def test_a_pass_must_say_what_it_ran(self):
        ps = passes()
        ps[3]["report"]["git"]["dirty"] = 2
        ps[4]["report"]["binaries"]["revision"] = REVS["base"]
        ps[5]["report"]["prepared"]["changed"] = ["/fast/lubm.nt"]
        del ps[6]["report"]["finished_ns"]
        errors = " ".join(run_gate(ps)["errors"])
        for text in ("dirty 2", "expected " + REVS[ps[4]["side"]], "prepared data changed", "unfinished pass"):
            self.assertIn(text, errors)


class Layouts(unittest.TestCase):
    def test_any_k_blocks_in_a_row_run_every_layout(self):
        # Round 8: the old rotation never ran a third of the layouts when 3 divided K.
        for k in range(2, 10):
            for first in range(1, k + 1):
                self.assertEqual({gate_module.block_layout(b, k) for b in range(first, first + k)},
                                 set(range(1, k + 1)), (k, first))

    def test_the_layout_is_the_unit_and_one_build_s_blocks_are_their_own(self):
        """Round 8: blocks of one layout aren't independent (its effect, tau^2 / K, stays however
        many repeat it): its blocks' mean is one unit. One build per side: each block is one."""
        contrasts = [0.020, 0.021, -0.010, -0.011]
        self.assertEqual(gatestats.loss_test(contrasts, [1, 1, 2, 2])["units"], 2)
        self.assertEqual(gatestats.loss_test(contrasts, [1, 2, 3, 4])["units"], 4)
        self.assertEqual(gatestats.loss_test(contrasts, [1, 1, 1, 1])["units"], 4)

    def test_layout_components_are_a_revision_s_own(self):
        """Round 8: gate-power.py's layout split pooled revisions, so their difference passed for a
        layout's."""
        spec_ = importlib.util.spec_from_file_location("gate_power", ROOT / "benches/probes/gate-power.py")
        probe = importlib.util.module_from_spec(spec_)
        spec_.loader.exec_module(probe)
        runs = {("a" * 40, 1): [10.0, 10.1], ("a" * 40, 2): [10.0, 10.1],
                ("b" * 40, 1): [20.0, 20.1], ("b" * 40, 2): [10.0, 10.1]}
        with tempfile.TemporaryDirectory() as tmp:
            paths = []
            for (rev, layout), values in runs.items():
                for i, v in enumerate(values):
                    path = Path(tmp) / f"{rev[0]}{layout}{i}.json"
                    path.write_text(json.dumps({"binaries": {"revision": rev, "layout": layout}, "records": [
                        {"case": "lab-olympics", "status": "ok", "samples": {"q01": [v]}}]}), encoding="utf-8")
                    paths.append(path)
            rows = {r["binary"]: r["layout_sd"] for r in probe.layout_components(paths, 0.001)}
        self.assertLess(rows["aaaaaaaa"], 0.001)
        self.assertGreater(rows["bbbbbbbb"], 0.3)


class Rounds(unittest.TestCase):
    W3C = {"passed": True, "errors": []}
    GUARDS = {"passed": True, "errors": [], "guards": 81}

    def round_(self, factor, k=0, **kw):
        return run_gate(passes(slower(factor), **kw), context={**CONTEXT, "round": k})

    def test_a_loss_needs_two_rounds_and_the_family_s_holm(self):
        first = self.round_(1.08)
        self.assertEqual({(f["side"], f["dimension"]) for f in first["flagged"]},
                         {("base", "time/q06"), ("main", "time/q06")})
        passed, text = gate_module.verdict([first], self.W3C, self.GUARDS)
        self.assertFalse(passed)
        self.assertIn("**pending**", text)
        with patch.object(gate_module, "checkout_rev", lambda checkout, cand, harness: harness):  # no repository
            job = gate_module.confirm_job([first], "C:/repo")
        self.assertEqual((job["id"], job["requires"]["hosts"]), ("rg-cccccccc-confirm", ["phuoc-yu"]))
        self.assertEqual(job["steps"][0][2:], ["plan-cases", "lab-olympics"])
        self.assertEqual({s[2] for s in job["steps"][1:-2]}, {"block"})
        confirmed, text = gate_module.verdict([first, self.round_(1.08, 1)], self.W3C, self.GUARDS)
        self.assertFalse(confirmed)
        self.assertIn("**confirmed loss**: lab-olympics against base: time/q06", text)
        self.assertTrue(gate_module.verdict([first, self.round_(1.0, 1)], self.W3C, self.GUARDS)[0])

    def test_a_loss_first_seen_while_confirming_another_is_confirmed_not_dropped(self):
        """Round 8: combine() followed only round 1's flags, so a loss round 2 showed passed."""
        family = ["base|a|time/q", "base|b|time/q"]
        r1 = {"family": family, "pvalues": {family[0]: 0.001, family[1]: 0.5}}
        r2 = {"pvalues": {family[0]: 0.001, family[1]: 0.0001}}
        d = gate_module.decide([[r1, r2]])
        self.assertEqual((d["losses"], d["pending"]), ([family[0]], [family[1]]))
        d = gate_module.decide([[r1, r2, {"pvalues": {family[1]: 0.0002}}]])
        self.assertEqual(sorted(d["losses"]), family)
        # Round 3 only where it could still decide: in a family of a thousand, a round-2 p of
        # 0.004 can't reach Holm's alpha / m.
        many = family + [f"base|c{i}|time/q" for i in range(1000)]
        r1 = {"family": many, "pvalues": {**{h: 0.5 for h in many}, family[0]: 0.001}}
        d = gate_module.decide([[r1, {"pvalues": {family[0]: 0.5, family[1]: 0.004}}]])
        self.assertEqual(d["pending"], [])

    def test_the_test_is_one_sided_holm_and_the_rounds_as_declared(self):
        self.assertAlmostEqual(gatestats.t_sf(2.015048, 5), 0.05, places=5)
        self.assertAlmostEqual(gatestats.t_sf(6.313752, 1), 0.05, places=5)
        self.assertAlmostEqual(gatestats.t_ppf(0.95, 5), 2.015048, places=4)
        faster = self.round_(0.8)
        q06 = faster["comparisons"]["base"]["lab-olympics"]["dimensions"]["time/q06"]
        self.assertGreater(q06["p"], 0.95)
        self.assertEqual(faster["flagged"], [])
        self.assertEqual(gatestats.holm({"a": 0.01, "b": 0.02, "c": 0.03}), {"a", "b", "c"})
        self.assertEqual(gatestats.holm({"a": 0.01, "b": 0.04, "c": 0.04}), {"a"})
        self.assertAlmostEqual(gatestats.combine_rounds([0.01, 0.02]), 0.02 / 0.9)
        self.assertAlmostEqual(gatestats.combine_rounds([0.5, 0.001, 0.001]), 0.01)

    def test_a_confirmation_runs_whole_squares_of_its_timed_sides(self):
        """Sol r2: the candidate is timed whatever revision it shares; only a main that is base's
        build isn't. Counting revisions gave three timed sides 2 blocks."""
        for cand, main, sides in (("b", "m", 3), ("m", "m", 3), ("b", "b", 2), ("c", "m", 3), ("c", "b", 2)):
            with self.subTest(cand=cand, main=main), patch.dict(REVS, {"cand": cand * 40, "base": "b" * 40, "main": main * 40}):
                summary = run_gate(passes(slower(1.08), blocks=6))
                for calibrated in (None, 3):
                    measure = {"lab-olympics": {"blocks": calibrated}} if calibrated else None
                    with patch.object(gate_module, "checkout_rev", return_value="x"):
                        job = gate_module.confirm_job([summary], "C:/repo", measure=measure)
                    n = len([s for s in job["steps"] if s[2] == "block"])
                    self.assertEqual(n % sides, 0, (cand, main, calibrated, n))
                    self.assertGreaterEqual(n, calibrated or gate_module.CONFIRM_BLOCKS[0])

    def test_a_hypothesis_no_round_compared_is_never_a_pass(self):
        """Review abba-not-compared r1 W-OUT1 (8 Nov 2026): a baseline that reports no memory leaves
        the family's memory hypotheses not compared (compare_case, "unsupported by this baseline"),
        out of the p-values; decide() saw none and verdict() said PASSED over no test. The shared
        decision result carries each untested hypothesis with its reason, and the verdict names it
        and doesn't pass, whether every hypothesis or one of them went untested."""
        def records_of(side, n):
            return [record(samples={"q01": [10.0, 10.1, 9.9], "q06": [4.8, 4.85, 4.8]},
                           anon=None if side != "cand" else 2_000_000)]
        memory = [f"{s}|lab-olympics|memory/anon_kernel" for s in ("base", "main")]
        for family in (memory, memory + ["base|lab-olympics|time/q01"]):
            with self.subTest(hypotheses=len(family)):
                s = run_gate(passes(records_of, blocks=4), family=family)
                passed, text = gate_module.verdict([s], self.W3C, self.GUARDS)
                self.assertFalse(passed)
                self.assertNotIn("**PASSED**", text)
                for h in memory:
                    side = h.split("|")[0]
                    self.assertIn(f"**not compared**: lab-olympics against {side}: memory/anon_kernel: "
                                  f"{side} reports none (unsupported by this baseline)", text)
                self.assertEqual(sorted(gate_module.decide([[s]])["not_compared"]), sorted(memory))

    def test_a_round_beyond_the_statistical_slots_that_compared_nothing_is_never_a_pass(self):
        """Review release-not-compared r1 W1: decide() took not-compared reasons from the three
        statistical slots only, so a fourth round that tested nothing was dropped and the verdict
        PASSED. Three rounds compare the memory hypotheses; the fourth's baseline reports none."""
        def records_of(base_anon):
            def of(side, n):
                return [record(samples={"q01": [10.0, 10.1, 9.9], "q06": [4.8, 4.85, 4.8]},
                               anon=1_000_000 if side == "cand" else base_anon)]
            return of
        family = [f"{s}|lab-olympics|memory/anon_kernel" for s in ("base", "main")]
        rounds = [run_gate(passes(records_of(1_000_000 if k < 3 else None), blocks=4), family=family,
                           context={**CONTEXT, "round": k}) for k in range(4)]
        passed, text = gate_module.verdict(rounds, self.W3C, self.GUARDS)
        self.assertFalse(passed)
        self.assertIn("**not compared**: lab-olympics against base: memory/anon_kernel", text)
        self.assertTrue(gate_module.verdict(rounds[:3], self.W3C, self.GUARDS)[0])  # the three alone pass

    def test_a_refused_plan_of_the_gate_s_jobs_exits_as_refused(self):
        """Review release-not-compared r1 OUT1: `release_gate.py jobs --plan` returned 0 when the
        fleet check refused the set, as abba.py run --plan did. It exits 1; a taken plan 0."""
        with tempfile.TemporaryDirectory() as tmp:
            subprocess.run(["git", "init", "-q", tmp], check=True, capture_output=True)
            subprocess.run(["git", "-C", tmp, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q",
                            "--allow-empty", "-m", "x"], check=True, capture_output=True)
            for code, expected in ((1, 1), (0, 0)):
                with self.subTest(fleet_check=code), \
                        patch.object(sys, "argv", ["release_gate.py", "jobs", "HEAD", "--base", "HEAD", "--main", "HEAD",
                                                   "--checkout", tmp, "--out", str(Path(tmp) / "out"), "--plan"]), \
                        patch.object(gate_module, "jobs", return_value={"rg-x": {"id": "rg-x"}}), \
                        patch.object(gate_module, "set_plan", return_value="plan"), \
                        patch.object(gate_module, "toml_set", return_value=""), \
                        patch.object(gate_module.cases_registry, "load_at", return_value=[]), \
                        patch.object(gate_module, "fleet_call", return_value=(code, "said")) as fleet, \
                        contextlib.redirect_stdout(io.TextIOWrapper(io.BytesIO(), encoding="utf-8")):
                    self.assertEqual(gate_module.main(), expected)
                    self.assertEqual([c.args[0] for c in fleet.call_args_list], ["check"])

    def test_nothing_flagged_needs_no_confirmation(self):
        clean = self.round_(1.0)
        self.assertIsNone(gate_module.confirm_job([clean], "C:/repo"))
        self.assertTrue(gate_module.verdict([clean], self.W3C, self.GUARDS)[0])

    def test_two_parts_on_one_host_are_not_rounds_of_each_other(self):
        """Placed by capability, the timed and the big part may land on one host: grouped by host,
        the big part's round 0 read as a confirmation of the timed part's (and its losses as
        confirmed). A part is its prefix; a confirmation keeps its part's."""
        timed, big = self.round_(1.0), run_gate(passes(slower(1.0)), context={**CONTEXT, "prefix": "rg-cccccccc-big"})
        confirm = run_gate(passes(slower(1.0)), context={**CONTEXT, "round": 1})
        parts = gate_module.rounds_of([timed, big, confirm])
        self.assertEqual(sorted(len(rounds) for rounds in parts), [1, 2])
        self.assertEqual([r["context"]["prefix"] for r in max(parts, key=len)], ["rg-cccccccc", "rg-cccccccc"])

    def test_a_confirmation_runs_where_its_first_round_ran_and_says_why(self):
        """Its pin is the gate's one genuine locality (its builds, its floor, its rounds combined);
        a report naming no host gives no confirmation job, never one on a host named in code."""
        first = self.round_(1.08)
        with patch.object(gate_module, "checkout_rev", lambda checkout, cand, harness: harness):
            job = gate_module.confirm_job([first], "C:/repo")
            self.assertEqual((job["requires"]["hosts"], job["requires"]["roles"]), (["phuoc-yu"], ["measure", "build"]))
            self.assertIn("first round", job["pin_reason"])
            hostless = run_gate(passes(slower(1.08)), context={**CONTEXT, "host": ""})
            with self.assertRaises(SystemExit):
                gate_module.confirm_job([hostless], "C:/repo")

    def test_a_part_s_passes_must_come_from_the_host_its_report_names(self):
        """Review r1 R1-F1: a timed part judged its passes by its report's host (its floor) without
        checking that they ran there; the fleet may move a timed job between steps."""
        def ran_on(host):
            out = passes(slower(1.0))
            for p in out:
                p["report"]["machine"] = {**MACHINE, "fleet_host": host} if host else \
                    {k: v for k, v in MACHINE.items() if k != "fleet_host"}
            return out
        self.assertEqual(run_gate(ran_on("phuoc-yu"))["errors"], [])
        moved = run_gate(ran_on("office-pc"))["errors"]
        self.assertTrue(any("passes ran on office-pc, not on phuoc-yu" in e for e in moved), moved)
        unsaid = run_gate(ran_on(None))["errors"]
        self.assertTrue(any("don't say which fleet host ran them" in e for e in unsaid), unsaid)
        # A report naming no host (a run outside a fleet job) holds them to none.
        self.assertEqual(run_gate(ran_on(None), context={**CONTEXT, "host": ""})["errors"], [])

    def test_a_part_asks_for_the_data_its_cases_read(self):
        """The fleet places by `has` only what the job can't make: the verified bench assets (frozen
        datasets, ORE ontologies, reference taxonomies). What benches/sparql/prepare.sh makes is the
        job's own (Johannes, 2 Nov 2026: "Data is the job's responsibility, not the fleet's"): no
        capability, but the disk to make it in, its source and its file."""
        need = gate_module.data_needs
        self.assertEqual(need([{"data": "social people=1000"}]), [])
        self.assertEqual(need([{"data": "ore:ore_ont_9724"}]), ["owlrs-bench-assets"])
        self.assertEqual(need([{"data": "el classes=10", "reference": "/fast/ref/x.tax"}]), ["owlrs-bench-assets"])
        self.assertEqual(need([{"data": "volume:bsbm-100m.nt"}]), [])
        self.assertEqual(need([{"data": "volume:watdiv-10m.nt", "queries": "@watdiv-10m"}]), [])
        frozen = {d["file"]: d for d in __import__("tomllib").loads((ROOT / "benches/datasets.toml").read_text(encoding="utf-8"))["dataset"]}
        self.assertEqual(gate_module.data_disk_gb([{"data": "volume:bsbm-100m.nt"}]),
                         -(-2 * frozen["bsbm-100m.nt"]["bytes"] // 10 ** 9))
        # The LUBM data was the assets' until 7 Nov 2026; now a job makes it (the next test).
        self.assertEqual(gate_module.data_disk_gb([{"data": "volume:lubm-10.nt"}]),
                         -(-2 * frozen["lubm-10.nt"]["bytes"] // 10 ** 9))

    def test_a_part_asks_for_what_its_data_s_makers_need_not_the_assets(self):
        """Orchestrator, 7 Nov 2026 (Johannes's 2 Nov decisions 1 and 8: a job sets up what it needs
        where it's missing, never a blanket copy): a set makes the datasets its cases read where a
        host lacks them; only a file no maker here can make asks for the bench assets. The LUBM and
        OWL2Bench makers run in Docker; perf_lab's support-stress generator needs only the build.
        REAS (abba-run-8ac6a2b5-91f067f1) passed --plan and failed at its data step: its
        support-stress files were on one host, and nothing made them."""
        need = gate_module.data_needs
        self.assertEqual(need([{"data": "volume:lubm-10.nt", "extra": ["volume:univ-bench.nt"]}]), ["docker"])
        self.assertEqual(need([{"data": "concat:volume:lubm-1.nt+social people=10"}]), ["docker"])
        self.assertEqual(need([{"data": "volume:owl2bench-rl-1.nt"}]), ["docker"])
        # Sol-loop r1 S1: riot labels an OWL2Bench file's blank nodes afresh, so a made copy can meet
        # only a form pin; one without it (owl2bench-ql-1.nt) still needs the assets.
        self.assertEqual(need([{"data": "volume:owl2bench-ql-1.nt"}]), ["owlrs-bench-assets"])
        self.assertEqual(need([{"data": "volume:support-stress/copies-8.nq"}]), [])
        self.assertEqual(need([{"data": "volume:era-shacl/ERA.ttl"}]), ["owlrs-bench-assets"])  # no maker here
        self.assertEqual(need([{"data": "volume:lubm-10.nt"}, {"data": "ore:ore_ont_9724"}]),
                         ["docker", "owlrs-bench-assets"])

    def test_a_job_whose_makers_start_containers_declares_them(self):
        """The master, 8 Nov 2026 (#129's regression): a timing job making LUBM data asked the fleet
        for Docker, and the fleet refused it, since a shared host's envelope can't hold containers a
        script starts unseen. The fleet's contract (F61, job spec 9): the job declares them (`spec = 9`,
        `containers`, Linux), each within every step's ask; their memory comes out of every step's
        slice, so the job asks that much more and its builds and blocks keep what they had. The
        makers' env carries the same limits (their containers are held to them)."""
        base = {"roles": ["measure", "build"], "os": "linux", "cores": 16, "ram_gb": 12}
        lubm = [{"data": "volume:lubm-10.nt", "extra": ["volume:univ-bench.nt"]}]
        requires, fields, env = gate_module.data_job(lubm, dict(base), harness=None)
        self.assertEqual(fields["spec"], 9)
        [maker] = fields["containers"]
        self.assertGreater(maker["memory_gb"], 0)
        self.assertGreater(maker["cpus"], 0)
        self.assertEqual(requires["ram_gb"], base["ram_gb"] + maker["memory_gb"])  # each slice keeps its 12 GB
        self.assertLessEqual(maker["cpus"], requires["cores"])
        self.assertEqual((requires["os"], "docker" in requires["has"]), ("linux", True))
        self.assertGreaterEqual(requires["disk_gb"], 1)
        self.assertEqual(env, {"NRESE_MAKER_MEMORY_GB": f"{maker['memory_gb']:g}", "NRESE_MAKER_CPUS": f"{maker['cpus']:g}"})
        self.assertEqual(gate_module.data_job([{"data": "volume:owl2bench-rl-1.nt"}], dict(base), harness=None)[1]["spec"], 9)
        # One declared container serves every maker of the job, one at a time: with a SPARQL file made
        # beside, it is at least what benches/sparql/prepare.sh's own containers take (its CAP).
        cap = re.search(r"^CAP=\(--cpus (\d+) --memory (\d+)g", (ROOT / "benches/sparql/prepare.sh").read_text(
            encoding="utf-8"), re.M)
        [both] = gate_module.data_job(lubm + [{"data": "volume:watdiv-10m.nt", "queries": "@watdiv-10m"}],
                                      dict(base), harness=None)[1]["containers"]
        self.assertGreaterEqual((both["memory_gb"], both["cpus"]), (int(cap[2]), int(cap[1])))
        # Only the makers of the job's own files count, not every maker the catalogue names (the first
        # plan of shacl-lubm declared SPARQL's 8 GB for LUBM alone: frozen() reads the whole catalogue).
        self.assertEqual((maker["memory_gb"], maker["cpus"]),
                         gate_module.MAKER_CONTAINERS["bash benches/reasoning/prepare-lubm.sh"])

    def test_a_release_gate_part_asks_what_its_harness_makes_and_how(self):
        """Review r1 R1 of pkg/maker-containers (8 Nov 2026): a part's data is made by its harness's own
        prepare (release-gate.sh prepare runs the harness worktree's release_gate.py and catalogue), so
        its declaration, its makers' env and its memory are the harness's (its MAKER_CONTAINERS and
        catalogue), not the planning tree's. A harness whose LUBM maker takes 6 GB and 4 CPUs is
        declared so; one whose catalogue has LUBM from the bench assets declares nothing."""
        with tempfile.TemporaryDirectory() as tmp:
            sized = harness_commit(Path(tmp) / "sized", lubm_size=(6, 4))
            timed = gate_module.jobs(sized, sized, (Path(tmp) / "sized").as_posix(), "rg-test", 2, "shacl-lubm",
                                     checks=False)["rg-test-timed"]
            self.assertEqual(timed["containers"], [{"name": "maker", "memory_gb": 6, "cpus": 4}])
            self.assertEqual((timed["env"]["NRESE_MAKER_MEMORY_GB"], timed["env"]["NRESE_MAKER_CPUS"]), ("6", "4"))
            # Review r2 R2-W1: the sizes are the catalogue's [makers], declared data; nothing in the
            # harness's code, a table of the same name written or changed there, alters what the job is told.
            coded = harness_commit(Path(tmp) / "coded", lubm_size=(6, 4), after=(
                f'MAKER_CONTAINERS = {{"{LUBM_MAKER}": (9, 9)}}\nMAKER_CONTAINERS["{LUBM_MAKER}"] = (9, 9)\n'))
            self.assertEqual(gate_module.jobs(coded, coded, (Path(tmp) / "coded").as_posix(), "rg-test", 2,
                                              "shacl-lubm", checks=False)["rg-test-timed"]["containers"],
                             [{"name": "maker", "memory_gb": 6, "cpus": 4}])
            assets = harness_commit(Path(tmp) / "assets", lubm_made_by="the bench assets' copy")
            timed = gate_module.jobs(assets, assets, (Path(tmp) / "assets").as_posix(), "rg-test", 2, "shacl-lubm",
                                     checks=False)["rg-test-timed"]
            self.assertNotIn("containers", timed)
            self.assertIn("owlrs-bench-assets", timed["requires"]["has"])
            sized_ram = gate_module.jobs(sized, sized, (Path(tmp) / "sized").as_posix(), "rg-test", 2, "shacl-lubm",
                                         checks=False)["rg-test-timed"]["requires"]["ram_gb"]
            self.assertEqual(sized_ram, timed["requires"]["ram_gb"] + 6)  # its blocks' ask, plus the harness's maker

    def test_a_harness_whose_makers_it_can_t_declare_is_refused(self):
        """Review r1 R1 and sol-loop r1 S1 of pkg/maker-containers: a harness whose makers start
        containers it declares no size for (between engine-v2 838cd7eb and this package: unlabelled,
        unlimited), or whose tables are code, or whose catalogue can't be read, would have its data
        step stopped or its data unknown: refused at the plan, saying why. Fix check r3 R3-W1 (the
        master, 8 Nov 2026): a harness's tables are its catalogue's [makers] alone, so every harness
        whose tables are code is refused for having no [makers], whatever the code says."""
        with tempfile.TemporaryDirectory() as tmp:
            for name, kw, cases, why in (
                    ("unsized", {"lubm_size": None}, "shacl-lubm", "container"),
                    ("computed", {"tables": 'MAKER_CONTAINERS = {"bash " + "x": (1, 1)}\n'}, "shacl-lubm",
                     "[makers]"),
                    ("uncatalogued", {"catalogue": False, "lubm_size": None}, "shacl-lubm", "datasets.toml"),
                    # Review r2 R2-W1: a harness from before [makers] whose sizes are code (this package's
                    # commits before it; bafc5741 declared 4/8 where H's code made 6/4): refused, never run.
                    ("sizes as code", {"legacy": True}, "shacl-lubm", "[makers]"),
                    # Sol-loop r3 S1: sized makers whose scripts don't take the declaration.
                    ("unheld", {"helper": False}, "shacl-lubm", "ns_maker_args"),
                    # S3: SPARQL's maker shares the declared container, so its size counts too.
                    ("no SPARQL size", {"sparql_size": False}, "shacl-lubm,watdiv-10m", "prepare.sh"),
                    # S4: a file the harness's catalogue doesn't name: its data step can't make or check it.
                    ("unnamed file", {"drop": ("lubm-10.nt",)}, "shacl-lubm", "lubm-10.nt"),
                    # Review r2 R2-W1, its witnesses: a write after the literal of a harness whose tables are
                    # code (the builder declared 4/8 where H says 6/4): refused, never run.
                    *((f"changed by {how}", {"legacy": True, "after": text}, "shacl-lubm", "[makers]")
                      for how, text in (
                          ("subscript", f'MAKER_CONTAINERS["{LUBM_MAKER}"] = (6, 4)\n'),
                          ("augmented", f'MAKER_CONTAINERS |= {{"{LUBM_MAKER}": (6, 4)}}\n'),
                          ("update", f'MAKER_CONTAINERS.update({{"{LUBM_MAKER}": (6, 4)}})\n'),
                          ("setdefault", 'MAKER_CONTAINERS.setdefault("bash x", (1, 1))\n'),
                          ("del", f'del MAKER_CONTAINERS["{LUBM_MAKER}"]\n'),
                          ("a function", 'def grow():\n    global MAKER_CONTAINERS\n    MAKER_CONTAINERS = {}\n'))),
                    ("assigned twice", {"legacy": True, "after": f'MAKER_CONTAINERS = {{"{LUBM_MAKER}": (6, 4)}}\n'},
                     "shacl-lubm", "[makers]"),
                    ("re-assigned annotated", {"legacy": True,
                                               "after": f'MAKER_CONTAINERS: dict = {{"{LUBM_MAKER}": (6, 4)}}\n'},
                     "shacl-lubm", "[makers]"),
                    ("malformed [makers]", {"lubm_size": (0, 4)}, "shacl-lubm", "[makers]")):
                with self.subTest(name):
                    repo = Path(tmp) / name.replace(" ", "-")
                    harness = harness_commit(repo, **kw)
                    with self.assertRaises(SystemExit) as refused:
                        gate_module.jobs(harness, harness, repo.as_posix(), "rg-test", 2, cases, checks=False)
                    self.assertIn(why, str(refused.exception))

    def test_a_harness_s_makers_are_its_catalogue_s_and_its_code_is_never_read(self):
        """Fix check r3 R3-W1 of pkg/maker-containers (8 Nov 2026): a harness without [makers] whose
        release_gate.py gives LUBM's maker Docker through an alias (r3's witness: its literal says [],
        `x = DATASET_MAKERS[LUBM]; x.append("docker")` after it) was planned with no container, no Docker
        and its blocks' memory alone, while its data step starts LUBM's containers. The master's boundary
        (8 Nov 2026): a harness's maker tables come only from its catalogue's [makers]; one without is
        refused before admission, naming the remedy, and no Python of the harness is read. Each row on
        its own: the witness (refused), and two guards that pass before and after: [makers] 6/4 declared
        6/4 though the harness's code raises and says 9/9; its release_gate.py never shown."""
        legacy = (f'DATASET_MAKERS = {{"{LUBM_MAKER}": [], "bash benches/reasoning/prepare-owl2bench.sh": ["docker"], '
                  '"perf_lab generate support-stress": []}\n'
                  'FORM_ONLY = {"bash benches/reasoning/prepare-owl2bench.sh"}\n'
                  'SPARQL_PREPARE = "benches/sparql/prepare.sh"\n'
                  f'_legacy_needs = DATASET_MAKERS["{LUBM_MAKER}"]\n_legacy_needs.append("docker")\n')
        real = subprocess.run

        def planned(repo: Path, harness: str):
            shown = []

            def run(args, *a, **kw):
                if list(args[:2]) == ["git", "-C"] and len(args) > 4 and args[3] == "show":
                    shown.append(str(args[4]).split(":", 1)[-1])
                return real(args, *a, **kw)
            try:
                with patch.object(subprocess, "run", run):
                    timed = gate_module.jobs(harness, harness, repo.as_posix(), "rg-test", 2, "shacl-lubm",
                                             checks=False)["rg-test-timed"]
            except SystemExit as refused:
                return str(refused), shown
            return timed, shown

        with tempfile.TemporaryDirectory() as tmp:
            with self.subTest("r3 witness: no [makers], Docker added by an alias in its code"):
                repo = Path(tmp) / "alias"
                got, shown = planned(repo, harness_commit(repo, tables=legacy))
                self.assertIsInstance(got, str, f"planned, not refused: {got}")
                for said in ("[makers]", "merge refactor/engine-v2", "--harness"):
                    self.assertIn(said, got)
                self.assertNotIn("benches/fast/release_gate.py", shown)
            with self.subTest("guard: [makers] 6/4, its code raising and saying 9/9"):
                repo = Path(tmp) / "inert"
                got, shown = planned(repo, harness_commit(repo, lubm_size=(6, 4), after=(
                    'raise RuntimeError("the harness code ran")\n'
                    f'MAKER_CONTAINERS = {{"{LUBM_MAKER}": (9, 9)}}\n')))
                self.assertIsInstance(got, dict, f"refused: {got}")
                self.assertEqual(got["containers"], [{"name": "maker", "memory_gb": 6, "cpus": 4}])
                self.assertEqual((got["env"]["NRESE_MAKER_MEMORY_GB"], got["env"]["NRESE_MAKER_CPUS"]), ("6", "4"))
                self.assertIn("benches/datasets.toml", shown)  # the probe sees the harness's reads
                self.assertNotIn("benches/fast/release_gate.py", shown)

    def test_a_catalogue_s_makers_are_refused_where_a_file_s_maker_would_be_ambiguous_or_unbounded(self):
        """Sol-loop r6 S6/S7 of pkg/maker-containers: a file's maker is the [makers] start its made_by
        begins with, so two starts one of which begins the other would depend on their order (which a
        save re-sorts); a container must be finite and positive; an entry must be a table."""
        for label, makers in (
                ("overlapping starts", {LUBM_MAKER + " 10": {"needs": ["docker"], "container": [6, 4]},
                                        LUBM_MAKER: {"needs": ["docker"], "container": [4, 8]}}),
                ("infinite memory", {LUBM_MAKER: {"needs": ["docker"], "container": [float("inf"), 8]}}),
                ("not a table", {LUBM_MAKER: 4}),
                ("an unknown key", {LUBM_MAKER: {"needs": ["docker"], "size": 4}})):
            with self.subTest(label), self.assertRaises(ValueError):
                gate_module.catalogue_makers(makers)

    def test_saving_the_catalogue_keeps_its_makers_as_they_are(self):
        """Sol-loop r6 S5: `suite.py data freeze` rewrites benches/datasets.toml (datasets.save()); its
        [makers] come back as they were, an empty section too (its presence says the catalogue holds them,
        so a harness with it isn't read as one from before them)."""
        from suitekit import datasets
        import tomllib
        for makers in (tomllib.loads((ROOT / "benches/datasets.toml").read_text(encoding="utf-8"))["makers"], {}):
            with self.subTest(makers=len(makers)), tempfile.TemporaryDirectory() as tmp:
                (Path(tmp) / "benches").mkdir()
                datasets.save(Path(tmp), {"budget_gb": 300, "makers": makers, "dataset": []})
                saved = tomllib.loads((Path(tmp) / "benches/datasets.toml").read_text(encoding="utf-8"))
                self.assertEqual(saved.get("makers"), makers)

    def test_this_tree_s_tables_are_the_literals_a_harness_reader_reads(self):
        """harness_tables() reads a harness's makers from its catalogue's [makers], never running its
        code: this tree, read so from a commit, gives the tables its own data step uses (one source:
        catalogue_makers()), and its SPARQL data step is SPARQL_PREPARE: the one path planning and
        preparation both use (sol-loop r6 S4), the catalogue's own."""
        import tomllib
        makers = tomllib.loads((ROOT / "benches/datasets.toml").read_text(encoding="utf-8"))["makers"]
        [sparql] = [s for s, m in makers.items() if m.get("sparql")]
        self.assertEqual("bash " + gate_module.SPARQL_PREPARE, sparql)
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp)
            (repo / "benches/fast").mkdir(parents=True)
            shutil.copy2(ROOT / "benches/fast/release_gate.py", repo / "benches/fast/release_gate.py")
            shutil.copy2(ROOT / "benches/datasets.toml", repo / "benches/datasets.toml")
            harness = commit_all(repo)
            read = gate_module.harness_tables(repo.as_posix(), harness)
        own = gate_module.local_tables()
        self.assertEqual((read.makers, read.form_only, read.sparql, read.containers, read.catalogue),
                         (own.makers, own.form_only, own.sparql, own.containers, own.catalogue))

    def test_a_job_whose_makers_start_no_container_asks_as_before(self):
        """Design note 2026-11-08-maker-containers, invariant 3: no maker that starts containers, no
        declaration: the job's spec, memory and capabilities as data_needs() and data_disk_gb() say."""
        base = {"roles": ["measure", "build"], "os": "linux", "cores": 16, "ram_gb": 12}
        for cases in ([{"data": "social people=1000"}], [{"data": "ore:ore_ont_9724"}],
                      [{"data": "volume:support-stress/copies-8.nq"}],
                      [{"data": "volume:watdiv-10m.nt", "queries": "@watdiv-10m"}]):
            with self.subTest(cases=cases):
                requires, fields, env = gate_module.data_job(cases, dict(base), harness=None)
                self.assertEqual((fields, env), ({}, {}))
                has, disk = gate_module.data_needs(cases), gate_module.data_disk_gb(cases)
                self.assertEqual(requires, base | ({"has": has} if has else {}) | ({"disk_gb": disk} if disk else {}))

    def test_a_release_gate_part_making_lubm_declares_its_maker(self):
        """The release gate's parts are the other job builder: the same declaration, from the same
        function, and its TOML is what the fleet reads (an inline table per container)."""
        import tomllib
        with patch.object(gate_module, "harness_tables", LOCAL_TABLES, create=True):  # a fake harness: this tree's
            found = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, "shacl-lubm", checks=False)
        timed = found["rg-test-timed"]
        self.assertEqual(timed["spec"], 9)
        [maker] = timed["containers"]
        self.assertEqual(timed["env"]["NRESE_MAKER_MEMORY_GB"], f"{maker['memory_gb']:g}")
        read = tomllib.loads(gate_module.toml_set({"id": "rg-test", "fail": "continue"}, list(found.values())))
        self.assertEqual(read["job"][0]["containers"], [maker])
        self.assertEqual(read["job"][0]["spec"], 9)

    def test_a_dataset_is_as_pinned_by_its_bytes_or_by_its_rdfc_form(self):
        """7 Nov 2026: riot labels univ-bench.nt's blank nodes afresh on every make, so a made copy's
        bytes aren't the pin while its RDFC-1.0 canonical N-Quads' SHA-256 is the verified copy's
        (stage-data-probe-1, stage-data-rdfc-1). A file pinned by form (`rdfc_sha256`) passes by
        either; one pinned by bytes alone, by its bytes only; a form pin with nothing to compute the
        form with fails closed."""
        import hashlib
        problem = gate_module.pin_problem
        never = lambda _: self.fail("a bytes-only pin asked for the form")  # noqa: E731
        with tempfile.TemporaryDirectory() as d:
            path = Path(d, "f.nt")
            path.write_bytes(b"<a> <b> _:x .\n")
            sha, other = hashlib.sha256(b"<a> <b> _:x .\n").hexdigest(), "0" * 64
            self.assertIsNone(problem(path, {"sha256": sha}, never))
            self.assertIn("sha256", problem(path, {"sha256": other}, never))
            self.assertIsNone(problem(path, {"sha256": other, "rdfc_sha256": "f" * 64}, lambda _: "f" * 64))
            self.assertIn("RDFC-1.0", problem(path, {"sha256": other, "rdfc_sha256": "f" * 64}, lambda _: "e" * 64))
            self.assertIn("rdfc", problem(path, {"sha256": other, "rdfc_sha256": "f" * 64}, None))

    def test_a_copy_of_an_older_release_is_made_again_not_used(self):
        """7 Nov 2026: lubm-10 and lubm-100 became their sorted distinct lines (UBA's threads order
        them anew on every make), so the asset hosts' copies are an older release, at another size.
        Where a maker can make the file, the data step makes it rather than stopping on the old copy;
        the data view takes the first copy that is its pin (dataset_source())."""
        import hashlib
        if shutil.which("bash") is None:
            self.skipTest("needs bash")
        new, old = b"<a> <b> <c> .\n<d> <e> <f> .\n", b"<d> <e> <f> .\n<a> <b> <c> .\n<a> <b> <c> .\n"
        entry = {"made_by": "bash benches/reasoning/prepare-lubm.sh 10", "bytes": len(new),
                 "sha256": hashlib.sha256(new).hexdigest()}
        with tempfile.TemporaryDirectory() as root, tempfile.TemporaryDirectory() as cache, \
                tempfile.TemporaryDirectory() as general:
            root, cache, general = Path(root), Path(cache), Path(general)
            (general / "lubm-10.nt").write_bytes(old)
            (root / "benches/reasoning").mkdir(parents=True)
            (root / "benches/reasoning/prepare-lubm.sh").write_text(
                f"#!/usr/bin/env bash\nprintf '%s' '{new.decode()}' > \"$NRESE_DATASET_DIR/lubm-10.nt\"\n",
                encoding="utf-8", newline="\n")
            (root / "benches/datasets.toml").write_text(
                '[[dataset]]\nfile = "lubm-10.nt"\n' + "".join(f'{k} = {json.dumps(v)}\n' for k, v in entry.items()),
                encoding="utf-8")
            with patch.dict(os.environ, {"SPARQL_DATA": str(cache)}), \
                    patch.object(gate_module.hosts_registry, "input_path", return_value=general):
                lines, ok = gate_module.data_state([{"data": "volume:lubm-10.nt"}], make=True, root=root)
            self.assertTrue(ok, "\n".join(lines))
            self.assertEqual((cache / "lubm-10.nt").read_bytes(), new)
            self.assertEqual((general / "lubm-10.nt").read_bytes(), old)  # the assets are read-only
            # The view: the first copy that is its pin, the older release skipped by its size.
            chosen, problem = gate_module.dataset_source("lubm-10.nt", entry, [general, cache], None)
            self.assertEqual((chosen, problem), (cache / "lubm-10.nt", None))
            (cache / "lubm-10.nt").unlink()
            chosen, problem = gate_module.dataset_source("lubm-10.nt", entry, [general, cache], None)
            self.assertEqual(chosen, general / "lubm-10.nt")
            self.assertIn("bytes", problem)

    def test_a_record_says_which_frozen_bytes_its_case_loaded(self):
        """Review r1 of pkg/stage-data, R1-W1: a comparison can hold a verdict to the same input only
        if each record says which bytes its case loaded. The prepared manifest's hash where it holds
        the file unchanged, else the file's own; None where the file isn't there, or the run can't
        read it (the Docker runtime's volume)."""
        import hashlib
        with tempfile.TemporaryDirectory() as data, tempfile.TemporaryDirectory() as fast_data:
            data, fast_data = Path(data), Path(fast_data)
            (data / "lubm-10.nt").write_bytes(b"<a> <b> <c> .\n")
            case = {"name": "t", "data": "volume:lubm-10.nt", "extra": ["volume:univ-bench.nt"]}
            stat = (data / "lubm-10.nt").stat()
            with patch.object(fast, "RUNTIME", "native"), patch.object(fast, "FAST_DATA", fast_data), \
                    patch.object(fast.hosts, "input_path", return_value=data), \
                    patch.dict(os.environ, {"NRESE_FAST_BIN": str(fast_data)}), patch.dict(fast._DIGESTS, clear=True):
                self.assertEqual(fast.case_inputs(case), {"lubm-10.nt": hashlib.sha256(b"<a> <b> <c> .\n").hexdigest(),
                                                          "univ-bench.nt": None})
            (fast_data / fast.PREPARED).write_text(json.dumps({"files": {(data / "lubm-10.nt").as_posix(): {
                "bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns, "sha256": "p" * 64}}}), encoding="utf-8")
            with patch.object(fast, "RUNTIME", "native"), patch.object(fast, "FAST_DATA", fast_data), \
                    patch.object(fast.hosts, "input_path", return_value=data), \
                    patch.dict(os.environ, {"NRESE_FAST_BIN": str(fast_data)}), patch.dict(fast._DIGESTS, clear=True):
                self.assertEqual(fast.case_inputs(case)["lubm-10.nt"], "p" * 64)  # the prepared manifest's
            with patch.object(fast, "RUNTIME", "docker"), patch.dict(fast._DIGESTS, clear=True):
                self.assertEqual(fast.case_inputs(case), {"lubm-10.nt": None, "univ-bench.nt": None})
            # Sol-loop r3 V2: a concatenation's order is part of what was loaded.
            (data / "univ-bench.nt").write_bytes(b"<u> <v> <w> .\n")
            ab = {"name": "t", "data": "concat:volume:lubm-10.nt+volume:univ-bench.nt"}
            ba = {"name": "t", "data": "concat:volume:univ-bench.nt+volume:lubm-10.nt"}
            with patch.object(fast, "RUNTIME", "native"), patch.object(fast, "FAST_DATA", fast_data), \
                    patch.object(fast.hosts, "input_path", return_value=data), \
                    patch.dict(os.environ, {"NRESE_FAST_BIN": str(fast_data)}), patch.dict(fast._DIGESTS, clear=True):
                self.assertNotEqual(fast.case_inputs(ab), fast.case_inputs(ba))
                # Sol-loop r4 W1: the same concatenations listed in another order are the same input.
                both = {"name": "t", "data": "volume:lubm-10.nt", "extra": [ab["data"], ba["data"]]}
                self.assertEqual(fast.case_inputs(both), fast.case_inputs({**both, "extra": both["extra"][::-1]}))
            # Fix check r2, R2-W2: an ORE case's converted ontology is a loaded external file, hashed.
            (fast_data / "ore").mkdir()
            (fast_data / "ore" / "ore_ont_9724.nt").write_bytes(b"<o> <p> <q> .\n")
            with patch.object(fast, "RUNTIME", "native"), patch.object(fast, "FAST_DATA", fast_data), \
                    patch.object(fast.hosts, "input_path", return_value=data), \
                    patch.dict(os.environ, {"NRESE_FAST_BIN": str(fast_data)}), patch.dict(fast._DIGESTS, clear=True):
                self.assertEqual(fast.case_inputs({"name": "o", "data": "ore:ore_ont_9724"}),
                                 {"ore:ore_ont_9724": hashlib.sha256(b"<o> <p> <q> .\n").hexdigest()})
                # Sol-loop r5 X2: a file there but unreadable is unknown, and the run goes on.
                with patch.object(fast, "file_digest", side_effect=PermissionError("denied")), \
                        patch.dict(fast._DIGESTS, clear=True):
                    self.assertEqual(fast.case_inputs({"name": "o", "data": "ore:ore_ont_9724"}), {"ore:ore_ont_9724": None})
                # A kind of input nothing here resolves is an external file of unknown bytes: never none.
                self.assertEqual(fast.case_inputs({"name": "n", "data": "url:https://example.org/x.nt"}),
                                 {"url:https://example.org/x.nt": None})

    def test_which_external_files_a_case_loads_has_one_answer(self):
        """The orchestrator, 7 Nov 2026 (18:12): the recorder and the comparison decide what an input
        is by one function (suitekit.inputs.declared()); a kind it doesn't know is external, of
        unknown bytes; only a bare generator spec is generated."""
        from suitekit import inputs
        self.assertEqual(inputs.declared({"data": "social people=1000"}), set())
        self.assertEqual(inputs.declared({"data": "none"}), set())
        self.assertEqual(inputs.declared({"data": "volume:lubm-10.nt", "extra": ["volume:univ-bench.nt"]}),
                         {"lubm-10.nt", "univ-bench.nt"})
        self.assertEqual(inputs.declared({"data": "ore:ore_ont_9724"}), {"ore:ore_ont_9724"})
        self.assertEqual(inputs.declared({"data": "el classes=10", "reference": "/fast/ref/x.tax"}),
                         {"reference:/fast/ref/x.tax"})
        self.assertEqual(inputs.declared({"data": "concat:volume:univ-bench.nt+social people=10"}),
                         {"univ-bench.nt", "concat:volume:univ-bench.nt+social people=10"})
        self.assertEqual(inputs.declared({"data": "url:https://example.org/x.nt"}), {"url:https://example.org/x.nt"})

    def test_the_catalogue_keeps_a_form_pin_and_a_note_when_it_is_written_again(self):
        """7 Nov 2026: `suite.py data` rewrites benches/datasets.toml from a fixed list of keys, so a
        freeze would have dropped univ-bench.nt's `rdfc_sha256` (its only pin a made copy can meet)
        and a file's `note` (lubm-100's make time) without a word."""
        from suitekit import datasets
        entry = {"file": "univ-bench.nt", "made_by": "bash benches/reasoning/prepare-lubm.sh 1", "bytes": 47781,
                 "sha256": "2" * 64, "rdfc_sha256": "7" * 64, "note": "made in 8 s on khaleesi-wsl"}
        with tempfile.TemporaryDirectory() as root:
            (Path(root) / "benches").mkdir()
            datasets.save(Path(root), {"budget_gb": 300, "dataset": [entry]})
            self.assertEqual(datasets.load(Path(root))["dataset"], [entry])
        # Sol-loop r1 S3: a freeze that changes the bytes drops the form pin, which was the old graph's
        # (it accepted the old graph after a re-freeze); the same bytes keep it.
        same = datasets.refreeze(dict(entry), 47781, "2" * 64, "2026-11-08")
        self.assertEqual((same["rdfc_sha256"], same.get("frozen")), ("7" * 64, None))
        changed = datasets.refreeze(dict(entry), 47781, "3" * 64, "2026-11-08")
        self.assertNotIn("rdfc_sha256", changed)
        self.assertEqual((changed["sha256"], changed["frozen"]), ("3" * 64, "2026-11-08"))

    def test_the_data_step_makes_a_missing_file_aside_and_keeps_only_what_its_pin_accepts(self):
        """Orchestrator, 7 Nov 2026: a maker writes into a directory aside (NRESE_DATASET_DIR, never
        the shared volume); each file the cases need is checked against its pin and only then renamed
        into the cache. A file that isn't its pin stays out, the cache as it was, and only it fails."""
        import hashlib
        if shutil.which("bash") is None:
            self.skipTest("needs bash")
        made = {"univ-bench.nt": b"<a> <b> <c> .\n", "lubm-1.nt": b"<q> <r> <s> .\n"}
        pins = {"univ-bench.nt": hashlib.sha256(made["univ-bench.nt"]).hexdigest(),
                "lubm-1.nt": hashlib.sha256(b"<not> <what> <it makes> .\n").hexdigest()}
        with tempfile.TemporaryDirectory() as root, tempfile.TemporaryDirectory() as cache:
            root, cache = Path(root), Path(cache)
            (root / "benches/reasoning").mkdir(parents=True)
            (root / "benches/reasoning/prepare-lubm.sh").write_text(
                '#!/usr/bin/env bash\nset -e\nd=${NRESE_DATASET_DIR:?}\n'
                + "".join(f"printf '%s' '{body.decode()}' > \"$d/{name}\"\n" for name, body in made.items()),
                encoding="utf-8", newline="\n")
            (root / "benches/datasets.toml").write_text("".join(
                f'[[dataset]]\nfile = "{name}"\nmade_by = "bash benches/reasoning/prepare-lubm.sh 1"\n'
                f'bytes = {len(made[name])}\nsha256 = "{pins[name]}"\n\n' for name in made), encoding="utf-8")
            with patch.dict(os.environ, {"SPARQL_DATA": str(cache)}), \
                    patch.object(gate_module.hosts_registry, "input_path", return_value=None):
                lines, ok = gate_module.data_state([{"data": "volume:lubm-1.nt", "extra": ["volume:univ-bench.nt"]}],
                                                   make=True, root=root)
            text = "\n".join(lines)
            self.assertFalse(ok, text)
            self.assertEqual(sorted(p.name for p in cache.iterdir()), ["univ-bench.nt"], text)  # nothing else, no aside
            self.assertEqual((cache / "univ-bench.nt").read_bytes(), made["univ-bench.nt"])
            self.assertIn("made univ-bench.nt", text)
            self.assertIn("FAILED lubm-1.nt", text)

    def test_the_data_probe_says_what_a_host_holds_and_what_it_cannot_make(self):
        """The probe (release_gate.py data; release-gate.sh prepare with --make): a file in the cache
        at its frozen size is held, at another size it differs, absent it is missing with its maker;
        a missing WatDiv query set can't be made again (a clock-seeded generator), only copied."""
        frozen = {d["file"]: d for d in __import__("tomllib").loads(
            (ROOT / "benches/datasets.toml").read_text(encoding="utf-8"))["dataset"]}
        with tempfile.TemporaryDirectory() as cache:
            (Path(cache) / "watdiv-10m.nt").write_bytes(b"x" * 10)
            with patch.dict(os.environ, {"SPARQL_DATA": cache}), \
                    patch.object(gate_module.hosts_registry, "input_path", return_value=None):
                lines, ok = gate_module.data_state([{"data": "volume:watdiv-10m.nt", "queries": "@watdiv-10m"},
                                                    {"data": "volume:bsbm-100m.nt"}])
        self.assertFalse(ok)
        text = "\n".join(lines)
        self.assertIn(f"DIFFERS watdiv-10m.nt: {Path(cache) / 'watdiv-10m.nt'} is 10 bytes, frozen "
                      f"{frozen['watdiv-10m.nt']['bytes']}", text)
        self.assertIn("MISSING bsbm-100m.nt: made by `bash benches/sparql/prepare.sh bsbm-100m`", text)
        self.assertIn("MISSING query set watdiv-10m", text)
        self.assertIn("can't be made again", text)
        # A concatenation's parts are inputs too (Sol r2 T2: "no frozen inputs" where both were absent).
        with tempfile.TemporaryDirectory() as cache, patch.dict(os.environ, {"SPARQL_DATA": cache}), \
                patch.object(gate_module.hosts_registry, "input_path", return_value=None):
            lines, ok = gate_module.data_state([{"data": "concat:volume:univ-bench.nt+volume:lubm-1.nt"}])
        self.assertFalse(ok)
        self.assertEqual(sorted(line.split(":")[0] for line in lines), ["MISSING lubm-1.nt", "MISSING univ-bench.nt"])

    def test_a_query_set_is_held_only_as_pinned_by_name_and_digest(self):
        """Review r1 R1-F2 (DATA_PINS): `data --make` said "held" for 56 files of the right count and
        wrong contents. A set is held only with exactly its pinned names, each with its SHA-256
        (benches/sparql/queries.sha256): wrong contents, other names, and a bench-inputs revision
        that delivers another set all fail, and the cache keeps what it had; the right revision's
        set is taken (the orchestrator's decision (b), 2 Nov 2026: a git input of the job, read by
        `git archive`). The CLI exits nonzero on a failure, and release-gate.sh prepare stops on it."""
        right = {"C1-1.rq": b"SELECT * { ?s ?p ?o }\n", "L1-1.rq": b"SELECT ?s { ?s ?p 1 }\n",
                 "S1-1.rq": b"ASK { ?s ?p ?o }\n"}
        wrong = {name: body + b"# not the pinned one\n" for name, body in right.items()}
        renamed = {f"X{name}": body for name, body in right.items()}
        sha = lambda body: __import__("hashlib").sha256(body).hexdigest()  # noqa: E731
        case = [{"data": "none", "queries": "@watdiv-10m"}]

        def write(directory, files):
            directory.mkdir(parents=True, exist_ok=True)
            for name, body in files.items():
                (directory / name).write_bytes(body)

        with tempfile.TemporaryDirectory() as root, tempfile.TemporaryDirectory() as repo:
            root, repo = Path(root), Path(repo)
            (root / "benches/sparql").mkdir(parents=True)
            (root / "benches/sparql/queries.sha256").write_text(
                "".join(f"{sha(body)}  watdiv-10m/{name}\n" for name, body in right.items()), encoding="utf-8")
            git = lambda *a: __import__("subprocess").run(["git", "-C", str(repo), *a], capture_output=True,  # noqa: E731
                                                          text=True, check=True).stdout.strip()
            git("init", "-q")
            revs = {}
            for label, files in (("wrong", wrong), ("right", right)):
                for old in (repo / "queries" / "watdiv-10m").glob("*.rq"):
                    old.unlink()
                write(repo / "queries" / "watdiv-10m", files)
                git("add", "-A")
                git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", label)
                revs[label] = git("rev-parse", "HEAD")

            def state(cached, make=False, rev=""):
                with tempfile.TemporaryDirectory() as cache:
                    if cached is not None:
                        write(Path(cache) / "queries" / "watdiv-10m", cached)
                    with patch.dict(os.environ, {"SPARQL_DATA": cache, "BENCH_INPUTS_REV": rev}), \
                            patch.object(gate_module.hosts_registry, "input_path", return_value=None):
                        lines, ok = gate_module.data_state(case, make=make, root=root, inputs_repo=repo)
                    held = {p.name: p.read_bytes() for p in (Path(cache) / "queries" / "watdiv-10m").glob("*.rq")}
                return lines, ok, held

            lines, ok, _ = state(right)
            self.assertTrue(ok, lines)
            for what, cached, why in (("same count, wrong contents", wrong, "differ from their SHA-256 pins"),
                                      ("same count, other names", renamed, "pinned files missing"),
                                      ("absent", None, "no ")):
                with self.subTest(what):
                    lines, ok, held = state(cached)
                    self.assertFalse(ok, lines)
                    self.assertIn(why, "\n".join(lines))
            # The revision with the wrong set: refused, the cache as it was; the right one: taken.
            lines, ok, held = state(wrong, make=True, rev=revs["wrong"])
            self.assertFalse(ok, lines)
            self.assertIn("aren't its pins", "\n".join(lines))
            self.assertEqual(held, wrong)
            lines, ok, held = state(wrong, make=True, rev=revs["right"])
            self.assertTrue(ok, lines)
            self.assertEqual(held, right)
            # The probe never takes a set; an unknown revision is a failure, not a pass.
            self.assertEqual(state(None, rev=revs["right"])[2], {})
            self.assertFalse(state(None, make=True, rev="0" * 40)[1])
            # The CLI's exit says it (release-gate.sh prepare stops on a nonzero one).
            with tempfile.TemporaryDirectory() as cache:
                write(Path(cache) / "queries" / "watdiv-10m", wrong)
                with patch.dict(os.environ, {"SPARQL_DATA": cache, "BENCH_INPUTS_REV": "0" * 40}), \
                        patch.object(gate_module.hosts_registry, "input_path", return_value=None), \
                        patch.object(gate_module, "ROOT", root), \
                        patch.object(gate_module.cases_registry, "load", return_value=[{"name": "q", **case[0]}]), \
                        patch.object(sys, "argv", ["release_gate.py", "data", "--cases", "q", "--make"]), \
                        patch.object(sys.stdout, "reconfigure", lambda **kw: None, create=True), \
                        patch("builtins.print"):
                    self.assertEqual(gate_module.main(), 1)


class Verdicts(unittest.TestCase):
    def test_the_candidate_failing_a_case_blocks_and_a_baseline_failing_is_reported(self):
        def records_of(side, n):
            if side == "main":
                r = record(status="failed")
                r["notes"] = ['Error: "unknown argument --reason-runs"']
                return [r]
            return [record()]
        summary = run_gate(passes(records_of))
        row = summary["comparisons"]["main"]["lab-olympics"]
        self.assertFalse(row["comparable"])
        self.assertIn("main can't run it (unknown argument --reason-runs", row["reason"])
        self.assertTrue(summary["passed"])
        failing = run_gate(passes(lambda side, n: [record(status="wrong" if side == "cand" else "ok")]))
        self.assertIn("lab-olympics", failing["candidate_failures"])
        self.assertFalse(failing["passed"])

    def test_a_baseline_off_its_route_with_right_answers_is_compared(self):
        def records_of(side, n):
            r = record()
            if side == "base":
                r["status"] = "off-route"
                r["routes"] = [{"path": "q.q06.operators", "expected": "has wcoj", "got": ["join"], "ok": False}]
            return [r]
        summary = run_gate(passes(records_of))
        row = summary["comparisons"]["base"]["lab-olympics"]
        self.assertTrue(row["comparable"])
        self.assertIn("off the route", " ".join(row["notes"]))
        self.assertTrue(summary["passed"])

    def test_stopping_at_a_declared_limit_where_a_baseline_finished_blocks(self):
        def records_of(side, n):
            if side == "cand":
                return [record(status="memory-limit", expected="memory-limit")]
            return [record(expected="memory-limit")]
        summary = run_gate(passes(records_of), min_blocks=1)
        self.assertTrue(any("stops at its declared outcome (memory-limit) where base completed" in b
                            for b in summary["blocking"]))

    def test_cases_not_run_block_and_those_left_to_the_big_host_must_be_timed_there(self):
        self.assertFalse(run_gate(passes(), not_run={"bsbm-bi-100m": "its cap is 28 GB"})["passed"])
        here = run_gate(passes(), elsewhere={"bsbm-bi-100m": "timed on khaleesi-wsl"})
        ok = {"passed": True, "errors": [], "guards": 81}
        w3c = {"passed": True, "errors": []}
        passed, text = gate_module.verdict([here], w3c, ok)
        self.assertFalse(passed)
        self.assertIn("NOT timed anywhere: bsbm-bi-100m", text)
        big = {**here, "planned": ["bsbm-bi-100m"], "elsewhere": {}, "context": {**CONTEXT, "host": "khaleesi-wsl"}}
        self.assertTrue(gate_module.verdict([here, big], w3c, ok)[0])

    def test_a_series_the_candidate_lost_blocks(self):
        def records_of(side, n):
            samples = {"q01": [10.0]} if side == "cand" else {"q01": [10.0], "q06": [4.8]}
            return [record(samples=samples)]
        self.assertTrue(any("time/q06: lost" in b for b in run_gate(passes(records_of))["blocking"]))

    def test_answers_that_differ_from_a_baseline_block_unless_a_check_pins_them(self):
        def records_of(side, n):
            return [record(rows={"q01": 5, "q06": 1000 if side == "main" else 1033})]
        unpinned = run_gate(passes(records_of))
        self.assertTrue(any("q06: rows [1033] here, [1000] on main: answers differ" in b for b in unpinned["blocking"]))
        pinned = run_gate(passes(records_of), catalog=[{"name": "lab-olympics", "check": {"q.q06.rows": 1033}}])
        self.assertTrue(pinned["passed"])


class MainAdapter(unittest.TestCase):
    """main f9b515e8's perf lab emits no samples_ms."""

    def main_result(self, p50=4.8):
        return {"label": "main", "load_s": 0.2, "peak_mib": 511, "sum_p50_ms": 15.0,
                "queries": [{"name": "q01", "rows": 5, "p50_ms": 10.0, "min_ms": 9.9, "max_ms": 10.2},
                            {"name": "q06", "rows": 1033, "p50_ms": p50, "min_ms": p50, "max_ms": p50}]}

    def test_fast_takes_main_s_p50_as_one_value_per_process(self):
        case = {"name": "lab-olympics", "metric": "sum_p50_ms", "queries": "@olympics"}
        self.assertEqual(fast.samples(case, self.main_result()), {"q01": [10.0], "q06": [4.8]})
        self.assertEqual(fast.estimator(self.main_result()), "process-p50")

    def test_without_routes_unreported_values_are_unchecked_not_wrong(self):
        result = self.main_result()
        with patch.dict(fast.OPTIONS, {"routes": False}):
            value = fast.check("q.q06.value", 1033, result, {}, {})
            route = fast.route("q.q06.operators", "has dictionary string test", result)
        self.assertIsNone(value["ok"])
        self.assertIsNone(route["ok"])
        self.assertFalse(fast.check("q.q06.value", 1033, result, {}, {})["ok"])  # with routes: wrong

    def test_main_is_compared_by_process_p50s_on_both_sides(self):
        def records_of(side, n):
            if side == "main":
                return [record(samples={"q01": [10.0], "q06": [4.8]}, estimator="process-p50")]
            return [record(samples={"q01": [10.0, 10.1, 9.9], "q06": [17.0, 18.0, 30.0]})]
        row = run_gate(passes(records_of))["comparisons"]["main"]["lab-olympics"]
        self.assertIn("one p50 per process", " ".join(row["notes"]))
        self.assertAlmostEqual(row["dimensions"]["time/q06"]["new"], 18.0)  # the process p50, not the pool

    def test_a_prepared_run_generates_nothing(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(fast, "FAST_DATA", Path(tmp)), \
                patch.object(fast, "RUNTIME", "native"), patch.dict(fast.OPTIONS, {"prepared": True}), \
                patch.object(fast, "container") as container, \
                patch.object(fast, "native_dirs", lambda: {"/fast": Path(tmp), "/target": Path(tmp)}):
            with self.assertRaisesRegex(RuntimeError, "missing input: social people=10 was not prepared"):
                fast.ensure_data("social people=10")
            container.assert_not_called()

    @unittest.skipIf(os.name == "nt", "the gate prepares on a Linux host (shared data: hardlinks to read-only blobs)")
    def test_a_concatenation_is_its_parts_as_they_are(self):
        """Sol, PR #64 r1 F1: a concatenation made while a part wasn't its freeze outlived the part's
        repair, and the shared index handed it to the next gate; prepare recorded bytes of no frozen
        input as prepared. Witness: dl-abox-lubm1's `concat:volume:univ-bench.nt+volume:lubm-1.nt`.
        Sol, gate-input-provenance r1: a making stopped between the file and its sums left the old
        sums beside the new file, and they matched once the part was restored."""
        case = {"name": "c", "data": "concat:volume:a.nt+volume:b.nt"}
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "data").mkdir()
            a, b = root / "data" / "a.nt", root / "data" / "b.nt"
            a.write_text("<urn:a> <urn:p> <urn:o> .\n", encoding="utf-8")
            # A making stopped as it moves the sums in: `mv` that fails there. The file and the sums
            # are left as a kill at that point leaves them (the making's trap takes only its partials).
            stopper = root / "bin" / "mv"
            stopper.parent.mkdir()
            stopper.write_text(f'#!/bin/sh\ncase "$*" in *.parts*) exit 1 ;; esac\nexec "{shutil.which("mv")}" "$@"\n',
                               encoding="utf-8")
            stopper.chmod(0o755)

            def prepared(gate: str, stopped: bool = False) -> str:
                """The case prepared in GATE's data, sharing the host's blobs: its concatenation."""
                data = root / gate
                path = f"{stopper.parent}{os.pathsep}{os.environ['PATH']}" if stopped else os.environ["PATH"]
                with patch.object(fast, "FAST_DATA", data), patch.object(fast, "RUNTIME", "native"), \
                        patch.object(fast, "SHARED", root / "shared"), patch.object(fast, "FAST_LOCK", root / "lock"), \
                        patch.object(fast, "native_dirs", lambda: {"/fast": data, "/data": root / "data"}), \
                        patch.object(fast, "load_cases", list), patch.object(fast, "select", lambda *_: [case]), \
                        patch.object(fast, "binaries", dict), patch.object(fast.manifest, "git", lambda _: {}), \
                        patch.dict(fast.OPTIONS, {"prepared": False}), patch.dict(os.environ, {"PATH": path}):
                    self.assertEqual(fast.prepare(None), 1 if stopped else 0)
                concat = data / "concat-a-b.nt"
                if not stopped:
                    recorded = json.loads((data / fast.PREPARED).read_text(encoding="utf-8"))["files"]
                    self.assertEqual(recorded[concat.as_posix()]["sha256"], fast.file_digest(concat))
                return concat.read_text(encoding="utf-8")

            frozen, altered = "<urn:b> <urn:p> <urn:o> .\n", "<urn:b> <urn:p> <urn:o> . # not its freeze\n"
            whole = a.read_text(encoding="utf-8") + frozen
            b.write_text(altered, encoding="utf-8")
            prepared("gate-1")
            b.write_text(frozen, encoding="utf-8")  # its freeze again
            self.assertEqual(prepared("gate-2"), whole, "SPEC concat-parts: not the shared one of other bytes")
            self.assertEqual(prepared("gate-1"), whole, "SPEC concat-parts: nor this gate's own")
            b.write_text(altered, encoding="utf-8")
            prepared("gate-1", stopped=True)
            b.write_text(frozen, encoding="utf-8")
            self.assertEqual(prepared("gate-1"), whole,
                             "SPEC concat-parts: nor one whose making stopped between the file and its sums")

    def test_a_query_set_kept_with_the_data_is_in_the_case_manifest_by_its_name(self):
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "watdiv-10m").mkdir()
            (Path(tmp) / "watdiv-10m" / "C1-1.rq").write_text("SELECT * { ?s ?p ?o }", encoding="utf-8")
            case = {"name": "watdiv", "queries": "@watdiv-10m", "metric": "sum_p50_ms", "data": "none"}
            with patch.dict("os.environ", {"NRESE_FAST_QUERY_SETS": tmp}), patch.object(fast, "RUNTIME", "native"):
                found = fast.case_manifest([case])
        self.assertEqual(found["series"]["watdiv"], ["C1-1"])

    def test_prepared_files_are_checked_by_size_and_time(self):
        with tempfile.TemporaryDirectory() as tmp:
            data = Path(tmp) / "d.nt"
            data.write_text("x")
            stat = data.stat()
            files = {data.as_posix(): {"bytes": stat.st_size, "mtime_ns": stat.st_mtime_ns, "sha256": "-"}}
            self.assertEqual(fast.changed_inputs(files), [])
            data.write_text("xy")
            self.assertEqual(fast.changed_inputs(files), [data.as_posix()])


class DeclaredMetric(unittest.TestCase):
    """The orchestrator, 7 Nov 2026 (18:05): C1's A-B-B-A abba-run-2102481f-fbbc9ee1 timed
    ql-memberships-type-commits-wide, which declares `commits.read_p50_ms`, by the commit latency
    (`time/_`, ratio 0.98), while B's reader read in ~10 ms against A's 0.004 ms: a loss in no
    hypothesis. fast.samples() took a section's `samples_ms` (the commits') for any metric of that
    section. A case's timed series is the metric it declares."""
    CASE = {"name": "ql-memberships-type-commits-wide", "metric": "commits.read_p50_ms", "tool": "perf_lab"}

    @staticmethod
    def result(read_p50, commit_ms=0.2077):
        return {"commits": {"commits": 400, "p50_ms": commit_ms, "p99_ms": 0.9461, "reads": 10010,
                            "read_p50_ms": read_p50, "read_p99_ms": 0.119, "samples_ms": [commit_ms] * 400}}

    def test_a_case_is_timed_by_the_metric_it_declares(self):
        self.assertEqual(fast.samples(self.CASE, self.result(0.004)), {"_": [0.004]})
        # Its section's own latency keeps its samples (the commits' p50 is what they sample).
        self.assertEqual(fast.samples({**self.CASE, "metric": "commits.p50_ms"}, self.result(0.004))["_"],
                         [0.2077] * 400)
        # Another statistic of a sampled section is that statistic, not the samples' median.
        self.assertEqual(fast.samples({**self.CASE, "metric": "commits.p99_ms"}, self.result(0.004)), {"_": [0.9461]})

    def test_a_metric_in_seconds_is_a_series_in_milliseconds(self):
        """Review r1 R1-F1: `commits.whole_s` reached the series in seconds, where the comparison's
        floors are milliseconds. A metric declared in seconds (`*_s`) is milliseconds in the series,
        per process and over repeated processes alike; a p50 sample series is as it was."""
        case = {"name": "rule-delta-sweep", "metric": "commits.whole_s", "tool": "perf_lab"}
        whole = lambda s: {"commits": {"whole_s": s, "p50_ms": 0.2, "samples_ms": [0.2] * 4}}  # noqa: E731
        self.assertEqual(fast.samples(case, whole(3.0)), {"_": [3000.0]})
        self.assertEqual(fast.samples(case, {**whole(3.0), "_repeats": [whole(3.0), whole(3.2)]}), {"_": [3000.0, 3200.0]})
        self.assertEqual(fast.metric_value(case, whole(3.0)), 3000.0)
        self.assertEqual(fast.samples({**case, "metric": "commits.p50_ms"}, whole(3.0)), {"_": [0.2] * 4})

    def test_a_600_ms_whole_run_loss_is_a_regression_in_compare(self):
        """Review r1 R1-F1, the real caller: `fast.py compare` (compare_reports) with its 2 ms floor
        called 3.0 s -> 3.6 s "slower, below absolute floor" and exited 0. Specified independently: a
        whole run 600 ms slower, 20 % and far above 2 ms, is a regression."""
        from suitekit import compare
        case = {"name": "rule-delta-sweep", "metric": "commits.whole_s", "tool": "perf_lab"}

        def report(seconds):
            records = []
            for rep in (1, 2):
                result = {"commits": {"whole_s": seconds, "p50_ms": 0.2, "samples_ms": [0.2] * 4}}
                records.append({"case": case["name"], "rep": rep, "status": "ok", "checks": [], "routes": [],
                                "metric": fast.metric_value(case, result), "samples": fast.samples(case, result)})
            return {"reps": 2, "machine": {"host": "fixture", "governor": "performance"},
                    "started": "2026-11-08T02:00:00", "records": records}
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = compare.compare_reports(report(3.0), report(3.6))
        self.assertEqual(code, 1, out.getvalue())
        self.assertNotIn("below absolute floor", out.getvalue())

    @staticmethod
    def saved(case, metric, series, units=None):
        records = []
        for rep in (1, 2):
            r = {"case": case, "rep": rep, "status": "ok", "checks": [], "routes": [], "metric": metric,
                 "samples": {"_": list(series)}}
            if units is not None:
                r["series_units"] = units
            records.append(r)
        return {"reps": 2, "machine": {"host": "fixture", "governor": "performance"},
                "started": "2026-11-08T02:00:00", "records": records}

    def compared(self, base, new):
        from suitekit import compare
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = compare.compare_reports(base, new)
        return code, out.getvalue()

    def test_reports_of_two_timing_unit_generations_are_normalised_or_refused(self):
        """Fix check r2, R2-F1: reports before 8 Nov 2026 saved `commits.whole_s` in seconds and
        timed it by its section's commit latencies; new ones save milliseconds of the whole run. A
        record says its generation (`series_units`; none: 1); compare normalises what that generation
        fixes exactly and refuses what it can't, before any verdict."""
        # The verdicts first (a red run fails at a verdict, not at a missing name); generation 2 literal.
        # An old rule-delta-sweep record timed its commit latencies: another quantity, refused.
        old = self.saved("rule-delta-sweep", 3.0, [0.2, 0.2, 0.2])
        new = self.saved("rule-delta-sweep", 3000.0, [3000.0], units=2)
        code, text = self.compared(old, new)
        self.assertEqual(code, 2, text)
        self.assertNotIn("faster", text)
        self.assertIn("timing units", text)
        # A load_s case was milliseconds in both generations: an old 3.0 s load against a new 3.6 s is
        # a 600 ms regression.
        load = next(c["name"] for c in __import__("suitekit.cases", fromlist=["load"]).load() if c.get("metric") == "load_s")
        code, text = self.compared(self.saved(load, 3000.0, [3000.0]),
                                   self.saved(load, 3600.0, [3600.0], units=2))
        self.assertEqual(code, 1, text)
        self.assertIn("SLOWER", text)
        # Same generation on both sides compares as it always did.
        code, text = self.compared(self.saved("rule-delta-sweep", 3.0, [10.0] * 3),
                                   self.saved("rule-delta-sweep", 3.0, [20.0] * 3))
        self.assertEqual(code, 1, text)  # 10 ms more, above the 2 ms floor
        # A generation compare doesn't know is refused.
        code, text = self.compared(new, self.saved("rule-delta-sweep", 3600.0, [3600.0], units=99))
        self.assertEqual(code, 2, text)
        self.assertIn("unknown timing units", text)
        # Every record fast.py writes says its generation: compare's own.
        from suitekit import compare
        self.assertEqual(fast.SERIES_UNITS, compare.SERIES_UNITS)
        self.assertEqual(compare.SERIES_UNITS, 2)

    def test_an_old_seconds_series_is_milliseconds_after_normalising(self):
        """A generation-1 record of a seconds field whose series was its own value (no section samples):
        its metric and series times 1000."""
        from suitekit import compare
        record = {"case": "x", "metric": 3.0, "samples": {"_": [3.0, 3.1]}}
        self.assertEqual(compare.normalised(record, "phase.whole_s"),
                         {**record, "metric": 3000.0, "samples": {"_": [3000.0, 3100.0]},
                          "series_units": compare.SERIES_UNITS})
        self.assertIsNone(compare.normalised({**record, "case": "y"}, "commits.whole_s"))  # section latency
        self.assertEqual(compare.normalised(dict(record), "load_s")["metric"], 3.0)  # was ms already

    CHECK = "closure_after_commits.equal"  # rule-delta-sweep's registry check (benches/cases/core.toml)

    @classmethod
    def two_cases(cls, units, sweep_status="ok", sweep_peak=1000):
        """r3's reports: rule-delta-sweep of one generation (1: seconds and its commit latencies;
        2: milliseconds of the whole run) beside an unchanged rl-hierarchy."""
        records = []
        for rep in (1, 2):
            ok = sweep_status == "ok"
            sweep = {"case": "rule-delta-sweep", "rep": rep, "status": sweep_status, "routes": [],
                     "checks": [{"path": cls.CHECK, "want": "true", "got": ok, "ok": ok}],
                     "metric": 3.0 if units is None else 3000.0,
                     "samples": {"_": [0.2, 0.2, 0.2] if units is None else [3000.0]},
                     "peak_mib": sweep_peak, "memory_kind": "lifetime"}
            hierarchy = {"case": "rl-hierarchy", "rep": rep, "status": "ok", "checks": [], "routes": [],
                         "metric": 100.0, "samples": {"_": [100.0]}, "peak_mib": 500, "memory_kind": "lifetime"}
            for r in (sweep, hierarchy):
                if units is not None:
                    r["series_units"] = units
                records.append(r)
        return {"reps": 2, "machine": {"host": "fixture", "governor": "performance"},
                "started": "2026-11-08T02:00:00", "records": records}

    def test_refused_timing_keeps_the_case_s_status_checks_and_memory(self):
        """Fix check r3, R3-F1: refusing a case's timing (units of two generations) dropped its whole
        record, so a case gone from ok to wrong beside an unchanged one exited 0. Specified (r3's
        required revision; compare.py's order, "nothing lost hides behind a time"): only the refused
        case's time is not compared; its status, checks and memory are, and their failure is nonzero."""
        no_ratio = lambda text: self.assertIsNone(re.search(r"^rule-delta-sweep +\d", text, re.M), text)  # noqa: E731
        with self.subTest("ok -> wrong beside an unchanged case: an acceptance failure, the status named"):
            code, text = self.compared(self.two_cases(None), self.two_cases(2, sweep_status="wrong"))
            self.assertEqual(code, 1, text)
            self.assertRegex(text, r"rule-delta-sweep +status ok -> wrong +CHANGED")
            no_ratio(text)
            self.assertRegex(text, r"rl-hierarchy .* same")
        with self.subTest("the wrong case alone: no timing left, its failure is the verdict (1), not the refusal (2)"):
            def alone(report):
                return {**report, "records": [r for r in report["records"] if r["case"] == "rule-delta-sweep"]}
            code, text = self.compared(alone(self.two_cases(None)), alone(self.two_cases(2, sweep_status="wrong")))
            self.assertEqual(code, 1, text)
            self.assertIn("status ok -> wrong", text)
        with self.subTest("a refused case's peak still counts: 1000 -> 2000 MiB"):
            code, text = self.compared(self.two_cases(None), self.two_cases(2, sweep_peak=2000))
            self.assertEqual(code, 1, text)
            self.assertRegex(text, r"rule-delta-sweep +peak .*1000 -> 2000 MiB +MORE MEMORY")
        with self.subTest("guard (as at 43b1f309): all ok, the refused case listed, no ratio for it, exit 0"):
            code, text = self.compared(self.two_cases(None), self.two_cases(2))
            self.assertEqual(code, 0, text)
            self.assertRegex(text, r"rule-delta-sweep.*not compared")
            no_ratio(text)

    def test_refused_timing_keeps_the_wrong_status_in_compact_reports(self):
        """R3-F1 through `fast.py compare` on compact records alone (their full reports not here):
        they keep status, checks and `series_units`, so the verdict is the full reports'."""
        with tempfile.TemporaryDirectory() as tmp:
            paths = []
            for name, report in (("base", self.two_cases(None)), ("new", self.two_cases(2, sweep_status="wrong"))):
                full = Path(tmp) / f"{name}-full.json"
                full.write_text(json.dumps(report), encoding="utf-8")
                paths.append(fast.write_compact(full, Path(tmp) / f"{name}.json"))
                full.unlink()
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                code = fast.compare(argparse.Namespace(base=str(paths[0]), new=str(paths[1])))
        self.assertEqual(code, 1, out.getvalue())
        self.assertRegex(out.getvalue(), r"rule-delta-sweep +status ok -> wrong +CHANGED")
        self.assertIn("full report isn't here", out.getvalue())

    def test_c1_s_read_slowdown_is_a_loss_in_its_family(self):
        """The run's own values, per pass: A 0.004 ms in every block, B 10.371, 9.843 and 10.547 ms."""
        a = [fast.samples(self.CASE, self.result(0.004))["_"] for _ in range(3)]
        b = [fast.samples(self.CASE, self.result(ms, 0.2056))["_"] for ms in (10.371, 9.843, 10.547)]
        blocks = [1, 2, 3]
        r = gate_module.dimension(a, blocks, b, blocks, {}, "ms")
        self.assertEqual(r["status"], "loss shown", r)
        self.assertGreater(r["ratio"], 1000)


class Memory(unittest.TestCase):
    def test_the_scope_s_anon_and_kernel_memory_is_gated_its_page_cache_reported(self):
        summary = run_gate(passes(lambda side, n: [record(cgroup=5000 if side == "cand" else 1000,
                                                          anon=int((worse(2, n) if side == "cand" else 1) * 10 ** 9))]))
        row = summary["comparisons"]["base"]["lab-olympics"]
        self.assertEqual(row["reported"]["cgroup_peak_mib"], [1000, 5000])
        self.assertNotIn("memory/cgroup", row["dimensions"])
        self.assertIn(("base", "memory/anon_kernel"), {(f["side"], f["dimension"]) for f in summary["flagged"]})
        self.assertEqual(fast.anon_kernel_bytes("anon 1000\nfile 99999\nkernel 200\n"), 1200)
        self.assertEqual(fast.anon_kernel_bytes("anon 10\nfile 5\nslab 5\nkernel_stack 2\n"), 17)

    def test_a_difference_within_the_counter_s_step_is_tested_never_neutral(self):
        """Round 8: below the resolution the old judge said neutral without a test. A consistent
        loss of one step is a loss; an inconsistent one is `unresolved`, reported."""
        def dim(cand):
            blocks = list(range(1, len(cand) + 1))
            return gate_module.dimension([[500.0]] * len(cand), blocks, [[v] for v in cand], blocks, {}, "MiB")
        self.assertEqual(dim([501.0] * 6)["status"], "loss shown")  # tied: p = 2^-6
        self.assertEqual(dim([501.0] * 2)["p"], 0.25)  # never 0 (two tied blocks were p = 0 in both rounds)
        unresolved = dim([501.0, 500.0, 500.0])
        self.assertEqual(unresolved["status"], "unresolved")
        self.assertGreater(unresolved["p"], 0.045)


class Evidence(unittest.TestCase):
    def test_a_declared_hypothesis_without_candidate_evidence_fails_closed(self):
        """Sol, 20 Oct 2026: with the baseline's memory reading and none from the candidate, the
        hypothesis went untested and the gate passed a confirmed loss. A reading lost on the
        candidate blocks; a baseline without the metric is stated, not missing."""
        dims = ("time/q01", "time/q06", "memory/anon_kernel")
        family = [f"{s}|lab-olympics|{d}" for s in ("base", "main") for d in dims]
        rows = [  # case, the baseline's reading, the candidate's per pass n, the declared family's extra
            ("complete", 1_000_000, lambda n: 2_000_000, []),
            ("absent", 1_000_000, lambda n: None, []),
            ("partial", 1_000_000, lambda n: None if n % 3 == 0 else 2_000_000, []),
            ("unsupported by the baseline", None, lambda n: 2_000_000, []),
            ("declared, never measured", 1_000_000, lambda n: 1_000_000, ["base|lab-olympics|time/q99"]),
        ]
        out = {}
        for name, base_anon, cand_anon, extra in rows:
            def records_of(side, n, base_anon=base_anon, cand_anon=cand_anon):
                anon = cand_anon(n) if side == "cand" else base_anon
                return [record(samples={"q01": [10.0, 10.1, 9.9], "q06": [4.8, 4.85, 4.8]}, anon=anon)]
            s = run_gate(passes(records_of, blocks=4), family=family + extra)
            memory = "base|lab-olympics|memory/anon_kernel"
            out[name] = (memory in s["pvalues"], any("memory/anon_kernel" in b for b in s["blocking"]),
                         any("no evidence from the candidate" in e for e in s["errors"]),
                         "unsupported" in s["not_compared"].get(memory, ""))
        self.assertEqual(out, {
            "complete": (True, False, False, False),
            "absent": (False, True, False, False),
            "partial": (False, True, False, False),
            "unsupported by the baseline": (False, False, False, True),
            "declared, never measured": (True, False, True, False),
        })


@patch.object(gate_module, "harness_tables", LOCAL_TABLES, create=True)  # fake harnesses: this tree's metadata
class Parts(unittest.TestCase):
    def test_plan_never_lowers_a_cap(self):
        catalog = [{"name": "small", "cap_gb": 4}, {"name": "bsbm-bi-100m", "cap_gb": 28},
                   {"name": "huge", "cap_gb": 60}]
        run, not_run, elsewhere = gate_module.plan(catalog, 24)
        self.assertEqual(run, ["small"])
        self.assertIn("its cap is 28 GB", not_run["bsbm-bi-100m"])
        run, not_run, elsewhere = gate_module.plan(catalog, 24, big_gb=28)
        self.assertEqual((run, sorted(not_run)), (["small"], ["huge"]))
        self.assertIn("timed in the big part", elsewhere["bsbm-bi-100m"])  # a part, never a host's name
        run, not_run, elsewhere = gate_module.plan(catalog, 28, min_gb=24)  # the big host's share
        self.assertEqual(run, ["bsbm-bi-100m"])

    def test_guards_are_read_from_performance_md_and_must_each_pass(self):
        names = gate_module.guard_names((ROOT / "docs/design/performance.md").read_text(encoding="utf-8"))
        self.assertIn("ordered_dedup_uses_any_complete_sort_order_without_gathering", names)
        log = ("test a::ordered_dedup_uses_any_complete_sort_order_without_gathering ... ok\n"
               "test cycles_are_counted_without_their_solutions ... ignored\n")
        errors, _ = gate_module.guards_check(["ordered_dedup_uses_any_complete_sort_order_without_gathering",
                                              "cycles_are_counted_without_their_solutions", "a_missing_guard_name"], log)
        self.assertEqual(len(errors), 2)
        self.assertIn("ignored", errors[0])
        self.assertIn("no test of that name ran", errors[1])

    def test_w3c_counts_are_required_per_suite(self):
        with tempfile.TemporaryDirectory() as tmp:
            logs = Path(tmp)
            (logs / "w3c_owl2_rl.log").write_text(
                "W3C OWL 2 RL test cases (RDF-based semantics): 86 passed, 0 failed (0 expected), 3 skipped\n"
                "test result: ok. 1 passed; 0 failed; 0 ignored\n")
            (logs / "w3c_shacl.log").write_text("W3C SHACL Core: 98 of 98 pass\n")
            (logs / "w3c_sparql11.log").write_text(
                "W3C SPARQL 1.1 on engine v2: passed / failed / skipped (failures shared with the reference)\n"
                "  QueryEvaluation: 239 / 0 / 0 (0)\n  total: 505 / 0 / 0 (0)\n")
            (logs / "tableau-w3c.tsv").write_text("a\tConsistencyTest\tAL\tpass\t0\n" * 410 +
                                                  "b\tInconsistencyTest\tAL\tdisputed\t0\n")
            # Each suite's counts from its own kit's log or table (the gate's declared kits).
            _, kits, bad = gate_module.load_w3c_expected(
                (ROOT / "benches/fast/w3c-expected.toml").read_text(encoding="utf-8"))
            self.assertEqual(bad, [])
            counts = gate_module.kit_counts(logs, kits)
            expected = {"OWL 2 RL": {"passed": 86, "skipped": 3}, "SHACL Core": {"passed": 98},
                        "SPARQL 1.1": {"passed": 505}, "OWL 2 DL": {"passed": 410, "disputed": 1},
                        "SHACL-SPARQL": {"passed": 22}}
            errors = gate_module.w3c_check(counts, expected)
            self.assertEqual(errors, ["SHACL-SPARQL: no counts (its kit didn't run, or its data was missing)"])
            counts["OWL 2 RL"]["skipped"] = 89
            self.assertIn("OWL 2 RL: 89 skipped, at most 3 known", gate_module.w3c_check(counts, expected))

    def test_the_jobs_run_the_whole_gate_in_blocks_asking_by_capability_one_set(self):
        """The brief of 2 Nov 2026: the gate asks by capability, never a host, and goes as one set.
        Each timed part builds every side where it times them (its own first step: no binaries
        staged between hosts, which WSL hosts refuse), holding that host alone from there."""
        found = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None)
        timed, checks = found["rg-test-timed"], found["rg-test-checks"]
        self.assertEqual((timed["kind"], timed["class"]), ("owlrs.timed", "exclusive"))
        for job in found.values():
            self.assertNotIn("hosts", job["requires"], job["id"])
            self.assertNotIn("after", job, job["id"])
        self.assertEqual((timed["requires"]["roles"], timed["requires"]["os"]), (["measure", "build"], "linux"))
        self.assertEqual(checks["requires"]["roles"], ["test"])
        # The first step builds every side and prepares the data; no step is untimed, so a part starts
        # only where it can time and keeps that host (planner held_between_steps; Sol r1 S1).
        self.assertNotIn("untimed_steps", timed)
        self.assertIn("release-gate.sh build", timed["steps"][0][2])
        self.assertIn("release-gate.sh prepare", timed["steps"][0][2])
        self.assertEqual(timed["steps"][0][4], "c" * 40)  # the harness, then the revisions
        self.assertEqual([s[2] for s in timed["steps"][1:]], ["block"] * 3 + ["report", "clean"])
        # No step asks less memory than the blocks: placed by a smaller ask, the builds could land on
        # a host the blocks don't fit, whose hold then ends without them (Sol r2 S1).
        for job in (timed, found["rg-test-big"]):
            asks = [n.get("ram_gb", job["requires"]["ram_gb"]) for n in job.get("step_needs", [])]
            self.assertTrue(all(a >= job["requires"]["ram_gb"] for a in asks), job["id"])
        import tomllib
        whole = tomllib.loads(gate_module.toml_set({"id": "rg-test", "fail": "continue"}, list(found.values())))
        self.assertEqual((whole["set"]["id"], [j["id"] for j in whole["job"]]), ("rg-test", list(found)))
        self.assertEqual(whole["job"][0]["steps"], timed["steps"])
        self.assertNotIn("_plan", whole["job"][0])  # the planner's own figures stay out of the job
        blocks_ = [s for s in timed["steps"] if s[2] == "block"]
        self.assertEqual([s[3] for s in blocks_], ["1", "2", "3"])  # 2 asked: one whole square of three sides
        self.assertEqual(blocks_[0][2:], ["block", "1", "c" * 40, "1", "-", f"base={gate_module.BASE}",
                                          f"main={gate_module.MAIN}", f"cand={'c' * 40}"])
        self.assertEqual(timed["env"]["GATE_MIN_BLOCKS"], "3")
        for job in found.values():
            self.assertNotIn("priority", job)
            self.assertEqual(job["priority_class"], "merge-gate")
        import tomllib
        self.assertEqual(tomllib.loads(gate_module.toml_job(timed))["steps"], timed["steps"])

    def test_the_big_cases_run_on_a_linux_host_never_native_windows(self):
        """Round 8: the big job went to khaleesi, native Windows, with the build's ELF binaries. It
        asks for Linux and the memory of its cases, and builds its own binaries there."""
        found = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None)
        big = found["rg-test-big"]
        self.assertEqual((big["requires"]["os"], big["requires"]["ram_gb"]), ("linux", 32))
        self.assertIn("release-gate.sh build", big["steps"][0][2])
        self.assertNotIn("untimed_steps", big)
        # Its own gate directory: on the timed part's host it would overwrite that part's plan (Sol r1 S3).
        self.assertNotEqual(big["env"]["GATE_NAME"], found["rg-test-timed"]["env"]["GATE_NAME"])
        self.assertFalse([i for i in big["inputs"] if "artifact" in i["source"]])
        self.assertEqual(big["env"]["GATE_MIN_GB"], "24")
        # The private input repository is a git input of a part whose cases read a WatDiv query set,
        # and of no other (the orchestrator's decision (b), 2 Nov 2026).
        with tempfile.TemporaryDirectory() as repo:
            run = lambda *a: __import__("subprocess").run(["git", "-C", repo, *a], capture_output=True,  # noqa: E731
                                                          text=True, check=True)
            run("init", "-q")
            run("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "inputs")
            head = run("rev-parse", "HEAD").stdout.strip()
            with patch.dict(os.environ, {"BENCH_INPUTS_GIT": repo}):
                watdiv = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, "watdiv-10m", checks=False)
                other = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, "lab-olympics", checks=False)
        self.assertIn({"name": "BENCH_INPUTS", "source": {"git": Path(repo).as_posix(), "rev": head}},
                      watdiv["rg-test-timed"]["inputs"])
        self.assertNotIn("BENCH_INPUTS", [i["name"] for i in other["rg-test-timed"]["inputs"]])
        # Only the first git input is checked out: the job is told the commit to take the sets from.
        self.assertEqual(watdiv["rg-test-timed"]["env"]["BENCH_INPUTS_REV"], head)
        self.assertNotIn("BENCH_INPUTS_REV", other["rg-test-timed"]["env"])
        # Review r1 R1-F3 (GATE_DIRECTORIES): two requests at one candidate, and the timed part and
        # the checks, all named gate-v2-CAND8, one mutable gate directory on a shared host. Each
        # request's each part has its own; the same request gives the same names (its resume).
        with patch.dict(os.environ, {"BENCH_INPUTS_GIT": str(ROOT / "no-such-inputs")}):
            first = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "review-first", 3, "lab-olympics")
            second = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "review-second", 3, "kernel-intersect")
            again = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "review-first", 3, "lab-olympics")
        names = [job["env"]["GATE_NAME"] for job in [*first.values(), *second.values()]]
        self.assertEqual(len(names), len(set(names)), names)
        self.assertEqual([j["env"]["GATE_NAME"] for j in first.values()], [j["env"]["GATE_NAME"] for j in again.values()])
        # A selection with no case above the timed part's memory has no big part.
        small = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, "lab-olympics")
        self.assertEqual(sorted(small), ["rg-test-checks", "rg-test-timed"])
        # And the step itself: on another system than Linux, unpack refuses.
        import shutil
        import subprocess
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("no bash")
        with tempfile.TemporaryDirectory() as tmp:
            shim = Path(tmp) / "shim"
            shim.mkdir()
            (shim / "uname").write_text("#!/bin/sh\necho MINGW64_NT-10.0\n", encoding="utf-8", newline="\n")
            (shim / "uname").chmod(0o755)
            env = {**__import__("os").environ, "NRESE_GATE_DIR": f"{tmp}/gate", "GATE_BIN": f"{tmp}/bin",
                   "FLEET_OUT": f"{tmp}/out", "FLEET_INPUT_BINARIES": tmp}
            env["PATH"] = str(shim) + __import__("os").pathsep + env["PATH"]
            done = subprocess.run([bash, "-c", f'PATH="$(cygpath -u "{shim}" 2>/dev/null || echo "{shim}"):$PATH" '
                                   f'exec bash scripts/release-gate.sh unpack'], cwd=ROOT, env=env,
                                  capture_output=True, text=True, encoding="utf-8")
            # The gate's own checkout (of this checkout's commit), if it made one, is under tmp.
            left = remove_checkouts_under(tmp)
        self.assertEqual(done.returncode, 2, done.stdout + done.stderr)
        self.assertIn("is not Linux", done.stdout)
        self.assertEqual(left, [], "a checkout this test made stayed registered")

    def test_a_block_of_two_sides_is_complete_with_its_four_reports(self):
        """20 Oct 2026: release-gate.sh block counted six reports a block whatever the sides, so an
        A-B-B-A of two (the agents' confirmation rounds) never completed."""
        import shutil
        import subprocess
        # Every python3 call the gate makes goes to this test's own interpreter: a gate host's is
        # 3.11 or later (tomllib), a machine running the suite may have an older python3.
        bash, python = shutil.which("bash"), sys.executable
        if bash is None:
            self.skipTest("no bash")
        head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp).as_posix()
            # fast.py block as it numbers its reports (2 per side a block); every other python3 call the real one.
            shim = Path(tmp) / "shim"
            shim.mkdir()
            (shim / "python3").write_text(f"""#!/usr/bin/env bash
case " $* " in
  *" benches/fast/fast.py block "*)
    n=0; out=; sides=()
    while [ $# -gt 0 ]; do case $1 in --block) n=$2 ;; --out) out=$2 ;; --side) sides+=("${{2%%:*}}") ;; esac; shift; done
    per=$(( 2 * ${{#sides[@]}} ))
    for k in $(seq 1 $per); do echo '{{"finished_ns": 1}}' > "$out/pass-$(( per * (n - 1) + k ))-x$k.json"; done ;;
  *) exec "{Path(python).as_posix()}" "$@" ;;
esac
""", encoding="utf-8", newline="\n")
            (shim / "python3").chmod(0o755)
            setup = f'''
. scripts/lib/gate-publish.sh
GATE={t}/gate GATE_BIN={t}/bin
b=$GATE_BIN/bin-test; mkdir -p "$b/release" "$GATE" {t}/out
echo tool > "$b/release/tool"; echo {head} > "$b/REVISION"; echo timing=watchdog > "$b/CAPABILITIES"
(cd "$b" && sha256sum release/tool > SHA256SUMS)
point {head} "$b"
echo '{{"run": ["lab-olympics"]}}' > "$GATE/plan.json"
'''
            env = {**os.environ, "NRESE_GATE_DIR": f"{t}/gate", "GATE_BIN": f"{t}/bin", "FLEET_OUT": f"{t}/out",
                   "GATE_QUIET": "0"}
            subprocess.run([bash, "-c", setup], cwd=ROOT, env=env, check=True, capture_output=True)
            done = subprocess.run([bash, "-c", f'PATH="$(cygpath -u "{shim}" 2>/dev/null || echo "{shim}"):$PATH" '
                                   f'exec bash scripts/release-gate.sh block 2 {head} 1 - base={head} main={head} '
                                   f'cand={head}'],
                                  cwd=ROOT, env=env, capture_output=True, text=True, encoding="utf-8")
            # An A-B-B-A's A' (benches/suite/abba.py) is A's build by design: timed, never skipped.
            aa = subprocess.run([bash, "-c", f'PATH="$(cygpath -u "{shim}" 2>/dev/null || echo "{shim}"):$PATH" '
                                 f'exec bash scripts/release-gate.sh block 3 {head} 1 - a={head} a2={head} b={head}'],
                                cwd=ROOT, env=env, capture_output=True, text=True, encoding="utf-8")
            reports = len(list(Path(tmp).glob("out/passes/pass-*.json"))) - 4  # block 2's: base and cand
            left = remove_checkouts_under(tmp)  # the harness's checkout, by its path alone
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertEqual(left, [], "a checkout this test made stayed registered")
        self.assertIn("block 2: done", done.stdout)
        self.assertIn("main is the same build as another side", done.stdout)  # timed once, as base
        self.assertEqual((aa.returncode, reports), (0, 6), aa.stdout + aa.stderr)  # a, a2, b: twice each
        self.assertNotIn("same build", aa.stdout)

    def test_prepare_refuses_an_input_that_isnt_its_freeze(self):
        """Sol, #50's D6 harness review r1 H3 (IB0556-0730): prepare linked an input whose hash
        isn't its freeze (benches/datasets.toml) into the data view, recorded "NOT the freeze" and
        went on, and the passes read it. Witness: copies-17's update script altered."""
        import shutil
        import subprocess
        bash, python = shutil.which("bash"), shutil.which("python3")
        if bash is None or python is None:
            self.skipTest("no bash or python3")
        head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp).as_posix()
            try:
                (Path(tmp) / "link").symlink_to(tmp)
            except OSError:
                self.skipTest("the data view is symlinks: the gate prepares on a Linux host")
            # The plan runs copies-17 alone; fast.py prepare leaves a mark; every other python3 call the real one.
            shim = Path(tmp) / "shim"
            shim.mkdir()
            (shim / "python3").write_text(f"""#!/usr/bin/env bash
case " $* " in
  *" benches/fast/release_gate.py plan "*) echo '{{"run": ["support-copies-17"], "not_run": [], "elsewhere": []}}' ;;
  *" benches/fast/fast.py prepare "*) touch "{t}/prepared" ;;
  *) exec "{Path(python).as_posix()}" "$@" ;;
esac
""", encoding="utf-8", newline="\n")
            (shim / "python3").chmod(0o755)
            inputs = Path(tmp) / "inputs" / "support-stress"
            inputs.mkdir(parents=True)
            # Each at its pinned size, its bytes altered: the data step takes them as there, and the
            # view's freeze check is what must refuse them (7 Nov 2026: at another size, a file a job
            # can make is made instead, and that path has its own test).
            pinned = {d["file"]: d["bytes"] for d in __import__("tomllib").loads(
                (ROOT / "benches/datasets.toml").read_text(encoding="utf-8"))["dataset"]}
            altered = b"INSERT DATA { <http://example.org/s> a <http://example.org/Absent> }\n"
            for name in ("copies-17.nq", "copies-17.nq.reader", "copies-17.nq.commits.ru"):
                size = pinned[f"support-stress/{name}"]
                (inputs / name).write_bytes((altered * (size // len(altered) + 1))[:size])
            view = Path(tmp) / "gate" / "data" / "support-stress"
            view.mkdir(parents=True)
            (view / "copies-17.nq.commits.ru").symlink_to(inputs / "copies-17.nq.commits.ru")  # an earlier view's
            setup = f'''
. scripts/lib/gate-publish.sh
GATE={t}/gate GATE_BIN={t}/bin
b=$GATE_BIN/bin-test; mkdir -p "$b/release" {t}/out
echo tool > "$b/release/tool"; echo {head} > "$b/REVISION"; echo timing=watchdog > "$b/CAPABILITIES"
(cd "$b" && sha256sum release/tool > SHA256SUMS)
point {head} "$b"
'''
            env = {**os.environ, "NRESE_GATE_DIR": f"{t}/gate", "GATE_BIN": f"{t}/bin", "FLEET_OUT": f"{t}/out",
                   "BENCH_GENERAL_DATA": (Path(tmp) / "inputs").as_posix(), "SPARQL_DATA": f"{t}/sparql",
                   "NRESE_FAST_SHARED": f"{t}/shared"}
            subprocess.run([bash, "-c", setup], cwd=ROOT, env=env, check=True, capture_output=True)
            done = subprocess.run([bash, "-c", f'PATH="$(cygpath -u "{shim}" 2>/dev/null || echo "{shim}"):$PATH" '
                                   f'exec bash scripts/release-gate.sh prepare {head}'],
                                  cwd=ROOT, env=env, capture_output=True, text=True, encoding="utf-8")
            linked = sorted(p.name for p in view.iterdir())
            prepared = (Path(tmp) / "prepared").exists()
            # The harness's checkout this run made, removed by its path alone: never a `prune`, which
            # takes any registration whose directory this process space can't see, another space's
            # live checkout among them (release-gate.sh's drop_checkout; Sol, PR #64 r1 F2).
            left = remove_checkouts_under(tmp)
        self.assertNotEqual(done.returncode, 0, f"SPEC frozen inputs: prepare went on\n{done.stdout}{done.stderr}")
        self.assertIn("NOT the freeze support-stress/copies-17.nq.commits.ru", done.stdout + done.stderr)
        self.assertEqual((linked, prepared), ([], False),
                         "SPEC frozen inputs: no input that isn't its freeze in the view, and no data prepared")
        self.assertEqual(left, [], "the harness checkout this test made stayed registered")

    def test_a_main_that_is_the_base_is_timed_once_and_declared_once(self):
        """Orchestrator, 20 Oct 2026: the jobs require a main, so a confirmation passes the base
        twice and every block timed it twice. The gate takes the base's runs for it, and its
        hypotheses aren't declared beside the base's."""
        sides = {"cand": REVS["cand"], "base": REVS["base"], "main": REVS["base"]}
        ps = passes(sides=("base", "cand"))
        for p in ps:
            if p["side"] == "base":
                for key in ("binaries", "binaries_finished"):
                    p["report"][key]["revision"] = REVS["base"]
        family = [f"{s}|lab-olympics|time/{q}" for s in ("base", "main") for q in ("q01", "q06")]
        s = gate_module.gate(ps, sides, "cand", ["base", "main"], HARNESS, context=CONTEXT, family=family)
        self.assertEqual((s["errors"], s["aliases"], sorted(s["family"])),
                         ([], {"main": "base"}, sorted(h for h in family if h.startswith("base|"))))
        self.assertTrue(s["passed"])
        self.assertIn("main is base's build", gate_module.render(s))

    def test_a_job_s_checkout_holds_the_candidate_and_the_harness_or_there_is_no_job(self):
        """Keepers, 20 Oct 2026: a confirmation's candidate rebased onto the harness was built in a
        checkout at the harness, which doesn't contain it, and the build failed with exit 2."""
        import subprocess
        with tempfile.TemporaryDirectory() as tmp:
            git = lambda *a: subprocess.run(["git", "-C", tmp, *a], capture_output=True, text=True,  # noqa: E731
                                            check=True).stdout.strip()
            git("init", "-q")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "base")
            root = git("rev-parse", "HEAD")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "harness")
            harness = git("rev-parse", "HEAD")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "rebased candidate")
            rebased = git("rev-parse", "HEAD")
            git("checkout", "-q", "-b", "elsewhere", root)
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "diverged")
            diverged = git("rev-parse", "HEAD")

            def checkout_of(candidate):
                found = gate_module.jobs(candidate, harness, tmp, "rg-test", 2, None, base=root, main=root,
                                         checks=False, big_gb=None)
                return found["rg-test-timed"]["inputs"][0]["source"]["rev"]
            self.assertEqual((checkout_of(rebased), checkout_of(root)), (rebased, harness))
            # A side the checkout's history doesn't hold is a git input of its own (Sol r1 S4);
            # one it holds isn't. The bench-inputs repository is the job's input where it exists and
            # a case reads a WatDiv set (the full catalogue does), absent otherwise (review r1 N1:
            # the test found this PC's repository and failed elsewhere's contract).
            for inputs_repo, extra in ((str(Path(tmp) / "no-such-inputs"), []), (tmp, ["BENCH_INPUTS"])):
                with patch.dict(os.environ, {"BENCH_INPUTS_GIT": inputs_repo}):
                    found = gate_module.jobs(rebased, harness, tmp, "rg-test", 2, None, base=diverged, main=root,
                                             checks=False, big_gb=None)
                names = [i["name"] for i in found["rg-test-timed"]["inputs"]]
                self.assertEqual(names, ["checkout", f"side-{diverged[:8]}", *extra])
            with self.assertRaises(SystemExit) as refused:
                checkout_of(diverged)
            self.assertIn("contains the other", str(refused.exception))

    def test_a_cached_build_counts_only_with_its_whole_identity(self):
        """Round 8: binaries from before counted by revision alone, whatever toolchain, flags or
        linker made them."""
        import shutil
        import subprocess
        bash = shutil.which("bash")
        if bash is None or shutil.which("rustc") is None:
            self.skipTest("no bash or rustc")
        script = r'''
. scripts/lib/gate-publish.sh
GATE_BIN=$1 GATE=$1/gate-x
a=$(identity_dir HEAD 1)
[ "$a" = "$(identity_dir HEAD 1)" ] && echo same
[ "$a" != "$(RUSTFLAGS="-C opt-level=2" identity_dir HEAD 1)" ] && echo flags
[ "$a" != "$(identity_dir HEAD 2)" ] && echo layout
d=$1/build; mkdir -p "$d/release"
printf "x Linker: LLD 22.1.8 y" > "$d/release/tool"; lld_of "$d"
printf "no linker" > "$d/release/other"; lld_of "$d" 2>/dev/null || echo not-lld
'''
        with tempfile.TemporaryDirectory() as tmp:
            done = subprocess.run([bash, "-c", script, "-", Path(tmp).as_posix()], cwd=ROOT,
                                  capture_output=True, text=True, encoding="utf-8")
        self.assertEqual(done.stdout.split(), ["same", "flags", "layout", "LLD", "22.1.8", "not-lld"],
                         done.stderr[-500:])

    def test_the_command_line_plans_the_family_and_splits_into_blocks(self):
        import subprocess
        script = ROOT / "benches/fast/release_gate.py"
        done = subprocess.run([sys.executable, "-B", str(script), "plan", "--max-gb", "24"], capture_output=True,
                              text=True, encoding="utf-8")
        self.assertEqual(done.returncode, 0, done.stderr[-500:])
        plan = json.loads(done.stdout)
        self.assertTrue(plan["run"] and plan["family"])
        with tempfile.TemporaryDirectory() as tmp:
            (Path(tmp) / "plan.json").write_text(done.stdout, encoding="utf-8")
            (Path(tmp) / "measure.toml").write_text(f'[case.{plan["run"][0]}]\nblocks = 3\n', encoding="utf-8")
            done = subprocess.run([sys.executable, "-B", str(script), "blocks", "--plan", str(Path(tmp) / "plan.json"),
                                   "--measure", str(Path(tmp) / "measure.toml"), "--square", "1", "--out", tmp],
                                  capture_output=True, text=True, encoding="utf-8")
            self.assertEqual(done.returncode, 0, done.stderr[-500:])
            third = json.loads((Path(tmp) / "plan-block-3.json").read_text(encoding="utf-8"))
        self.assertEqual(third["run"], plan["run"][:1])

    def test_an_empty_case_selection_is_refused_never_every_case(self):
        from types import SimpleNamespace
        with self.assertRaises(SystemExit):
            fast.select([{"name": "a", "area": "query"}], SimpleNamespace(cases="", areas=None))

    def test_a_block_that_work_not_its_own_disturbed_is_noted_in_the_report(self):
        """COT-13 (20 Oct 2026): the watcher no longer stops work that isn't the job's own; it
        leaves it running and records it, and the report says which blocks it disturbed."""
        with tempfile.TemporaryDirectory() as tmp:
            for n, verdict in ((1, {"verdict": "quiet", "samples": 30, "unowned_share": 0.0}),
                               (2, {"verdict": "disturbed", "samples": 30, "unowned_share": 0.4,
                                    "not_resumed": ["pid 51"]})):
                (Path(tmp) / f"quiet-block-{n}.json").write_text(json.dumps(verdict), encoding="utf-8")
            verdicts, notes = gate_module.quiet_verdicts(Path(tmp))
        self.assertEqual(sorted(verdicts), [1, 2])
        self.assertEqual(len(notes), 2)
        self.assertTrue(notes[0].startswith("block 2: disturbed") and "40%" in notes[0], notes)
        self.assertIn("not resumed", notes[1])

    @unittest.skipUnless(sys.platform.startswith("linux"), "the watcher watches /proc (Linux)")
    def test_a_blocks_attempts_from_every_crash_point_one_repeat_at_most_a_verdict_before_acceptance(self):
        """The orchestrator, 20 Oct 2026: a disturbed block is left out of the statistics, both
        sides, and repeated once; a disturbed repeat is kept and listed. Sol, 20 Oct 2026: a restart
        accepted a disturbed block, or ran a third attempt. One table over the states a crash leaves
        a block in: what the restarted step runs and keeps. git, the watcher and fast.py are faked
        (the watcher's --resume is the real one); no repository is touched."""
        import shutil
        import subprocess
        bash, python = shutil.which("bash"), sys.executable  # as above: this test's own interpreter
        head = "a" * 40
        reports = [f"pass-{k}-x{k}.json" for k in range(1, 5)]
        complete = {r: '{"finished_ns": 1}' for r in reports}
        archived = {f"disturbed-{r[:-5]}-9.json": "{}" for r in reports}
        quiet, disturbed = '{"verdict": "quiet"}', '{"verdict": "disturbed"}'
        # case, the state file, files in passes, an earlier watcher's record, the verdicts of the
        # attempts it runs -> exit, attempts run, pass-*, disturbed-pass-*, final state, a report
        # note; and a fault: mv failing on paths that contain a text, or a directory where the
        # state is written (its temporary file) or renamed to
        faults = {
            "a state that can't be written: nothing run":
                (None, {}, None, ["quiet"], (2, 0, 0, 0, None, None), ("squat", "block-1.state.new")),
            "a state that can't be renamed: nothing run":
                (None, {}, None, ["quiet"], (2, 0, 0, 0, "squatted", None), ("squat", "block-1.state")),
            "an acceptance that can't be saved: not accepted":
                ("1:ran", {**complete, "quiet-block-1.json": quiet}, None, [], (2, 0, 4, 0, "1:ran", None),
                 ("squat", "block-1.state.new")),
            "an archival move that fails: no repeat":
                ("1:ran", {**complete, "quiet-block-1.json": disturbed}, None, ["quiet"], (2, 0, 4, 0, "1:archiving", None),
                 ("mv", "disturbed-pass")),
        }
        rows = [
            ("fresh, quiet", None, {}, None, ["quiet"], (0, 1, 4, 0, "1:accepted", None)),
            ("fresh, disturbed then quiet", None, {}, None, ["disturbed", "quiet"],
             (0, 2, 4, 4, "2:accepted", "left out of the statistics")),
            ("fresh, disturbed twice: kept", None, {}, None, ["disturbed", "disturbed"],
             (0, 2, 4, 4, "2:accepted", "its repeat disturbed, kept")),
            ("a crash during attempt 1: resumed", "1:running", dict(list(complete.items())[:2]), None, ["quiet"],
             (0, 1, 4, 0, "1:accepted", None)),
            ("attempt 1 run, its verdict not recorded: run again", "1:ran", complete, None, ["quiet"],
             (0, 1, 4, 0, "1:accepted", None)),
            ("attempt 1 disturbed, a crash before archiving", "1:ran", {**complete, "quiet-block-1.json": disturbed},
             None, ["quiet"], (0, 1, 4, 4, "2:accepted", "left out of the statistics")),
            ("a crash midway through archiving", "1:archiving",
             {**dict(list(complete.items())[2:]), **dict(list(archived.items())[:2]), "quiet-block-1.json": disturbed},
             None, ["quiet"], (0, 1, 4, 4, "2:accepted", "left out of the statistics")),
            ("archived, a crash before the repeat", "2:start", archived, None, ["quiet"], (0, 1, 4, 4, "2:accepted", None)),
            ("a crash during the repeat: resumed, never a third", "2:running",
             {**archived, **dict(list(complete.items())[:1])}, None, ["disturbed"],
             (0, 1, 4, 4, "2:accepted", "its repeat disturbed, kept")),
            ("the repeat disturbed and recorded: kept, nothing run", "2:ran",
             {**archived, **complete, "quiet-block-1.json": disturbed}, None, [], (0, 0, 4, 4, "2:accepted", None)),
            ("accepted before: nothing run", "1:accepted", {**complete, "quiet-block-1.json": quiet}, None, [],
             (0, 0, 4, 0, "1:accepted", None)),
            ("complete and disturbed, no state, an earlier record unresolved: stopped", None,
             {**complete, "quiet-block-1.json": disturbed}, "pid 51\n", [], (2, 0, 4, 0, None, None)),
            ("a third attempt: never started", "3:start", {}, None, [], (2, 0, 0, 0, "3:start", None)),
        ]
        rows = [(*row, None) for row in rows] + [(case, *rest) for case, rest in faults.items()]
        for case, state, files, frozen, verdicts, expected, fault in rows:
            with self.subTest(case), tempfile.TemporaryDirectory() as tmp:
                t = Path(tmp).as_posix()
                shim = Path(tmp) / "shim"
                shim.mkdir()
                (Path(tmp) / "verdicts").write_text("".join(f"{v}\n" for v in verdicts), encoding="utf-8")
                # fast.py block writes the block's reports (each run counted); quiet-watch.py writes
                # the next verdict when it is stopped; its --resume is the real watcher's.
                (shim / "python3").write_text(f"""#!/usr/bin/env bash
case " $* " in
  *" benches/fast/fast.py block "*)
    echo run >> {t}/runs
    n=0; out=; sides=()
    while [ $# -gt 0 ]; do case $1 in --block) n=$2 ;; --out) out=$2 ;; --side) sides+=("${{2%%:*}}") ;; esac; shift; done
    per=$(( 2 * ${{#sides[@]}} ))
    for k in $(seq 1 $per); do echo '{{"finished_ns": 1}}' > "$out/pass-$(( per * (n - 1) + k ))-x$k.json"; done
    sleep 1 ;;  # a block takes time: the watcher is up before it ends
  *"quiet-watch.py"*" --resume "*) exec "{Path(python).as_posix()}" "{(ROOT / 'scripts/lib/quiet-watch.py').as_posix()}" "$2" --resume ;;
  *"quiet-watch.py"*)
    state=$2
    verdict() {{
      v=$(head -1 {t}/verdicts); sed -i 1d {t}/verdicts
      printf '{{"verdict": "%s", "samples": 9, "unowned_share": 0.5}}\\n' "$v" > "$state/verdict.json"
      echo "$v" > "$state/watch.log"; exit 0
    }}
    trap verdict TERM
    while :; do sleep 0.2; done ;;
  *) exec "{Path(python).as_posix()}" "$@" ;;
esac
""", encoding="utf-8", newline="\n")
                (shim / "python3").chmod(0o755)
                # git, faked: the revision is a fixed id, its tree empty, a worktree an empty
                # directory; anything else the step asks fails loudly (no repository is touched).
                (shim / "git").write_text(f"""#!/usr/bin/env bash
while [ "$1" = -c ] || [ "$1" = -C ]; do shift 2; done
case "$1 $2" in
  "rev-parse --short=8") echo "${{3:0:8}}" ;;
  "rev-parse --show-object-format") echo sha1 ;;
  "rev-parse HEAD") echo {head} ;;
  "ls-tree -r") ;;
  "worktree add") mkdir -p "${{@: -2:1}}" ;;
  "status --porcelain") ;;
  *) if [ "$1" = rev-parse ] && [ $# -eq 2 ]; then echo "$2"; else echo "fake git: $*" >&2; exit 99; fi ;;
esac
""", encoding="utf-8", newline="\n")
                (shim / "git").chmod(0o755)
                if fault and fault[0] == "mv":
                    (shim / "mv").write_text(f'#!/bin/sh\ncase "$*" in *{fault[1]}*) echo "injected mv failure" >&2; exit 1 ;; '
                                             'esac\nexec /bin/mv "$@"\n', encoding="utf-8", newline="\n")
                    (shim / "mv").chmod(0o755)
                setup = f'''
. scripts/lib/gate-publish.sh
GATE={t}/gate GATE_BIN={t}/bin
b=$GATE_BIN/bin-test; mkdir -p "$b/release" "$GATE" {t}/out/passes
echo tool > "$b/release/tool"; echo {head} > "$b/REVISION"; echo timing=watchdog > "$b/CAPABILITIES"
(cd "$b" && sha256sum release/tool > SHA256SUMS)
point {head} "$b"
echo '{{"run": ["lab-olympics"]}}' > "$GATE/plan.json"
'''
                env = {**os.environ, "NRESE_GATE_DIR": f"{t}/gate", "GATE_BIN": f"{t}/bin", "FLEET_OUT": f"{t}/out",
                       "PATH": f"{shim}{os.pathsep}{os.environ['PATH']}"}
                subprocess.run([bash, "-c", setup], cwd=ROOT, env=env, check=True, capture_output=True)
                passes = Path(tmp) / "out" / "passes"
                for name, content in files.items():
                    (passes / name).write_text(content, encoding="utf-8")
                if state:
                    attempt, stage = state.split(":")
                    (passes / "block-1.state").write_text(f"attempt={attempt}\nstage={stage}\n", encoding="utf-8")
                if fault and fault[0] == "squat":
                    (passes / fault[1]).mkdir()  # a directory where the state is written or renamed to
                if frozen:
                    (Path(tmp) / "gate" / "quiet").mkdir(parents=True)
                    (Path(tmp) / "gate" / "quiet" / "frozen").write_text(frozen, encoding="utf-8")
                done = subprocess.run([bash, "-c", f'PATH="{shim}:$PATH" exec bash scripts/release-gate.sh block 1 {head} 1 - '
                                       f'base={head} cand={head}'], cwd=ROOT, env=env, capture_output=True, text=True)
                runs = Path(tmp) / "runs"
                final = passes / "block-1.state"
                found = (done.returncode, len(runs.read_text().split()) if runs.exists() else 0,
                         len(list(passes.glob("pass-*.json"))), len(list(passes.glob("disturbed-pass-*.json"))),
                         "squatted" if final.is_dir() else
                         ":".join(line.split("=", 1)[1] for line in final.read_text().split()) if final.exists() else None)
                self.assertEqual(found, expected[:5], done.stdout + done.stderr)
                if frozen:
                    self.assertEqual((Path(tmp) / "gate" / "quiet" / "frozen").read_text(), frozen)
                notes = gate_module.quiet_verdicts(passes)[1]
                if expected[5]:
                    self.assertTrue(any(expected[5] in note for note in notes), notes)

    def test_report_writes_markdown_and_json(self):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            for p in passes():
                (directory / p["path"]).write_text(json.dumps(p["report"]), encoding="utf-8")
            summary = run_gate(gate_module.load_passes(directory))
            text = gate_module.render(summary)
            self.assertIn("**PASSED.**", text)
            self.assertIn("## Against main", text)


# The owner-approved trade-offs' tests: test_release_acceptance.py's Tradeoffs (the verdict reads
# them at the candidate's tree).


@patch.object(gate_module, "harness_tables", LOCAL_TABLES, create=True)  # fake harnesses: this tree's metadata
class Measurement(unittest.TestCase):
    """benches/NOISY.md: the noise reduced in the harness, not tolerated."""

    def test_a_case_whose_noise_needs_more_blocks_is_tested_on_them_and_the_jobs_run_them(self):
        measure = {"a": {"blocks": 2}, "b": {"blocks": 4}, "c": {"blocks": 3}}
        self.assertEqual(gate_module.blocks(["a", "b", "c"], measure, square=1),  # one side: no squares
                         [["a", "b", "c"], ["a", "b", "c"], ["b", "c"], ["b"]])
        with self.assertRaises(TypeError):  # Sol r1: no square given planned 2 and 8 blocks for three sides
            gate_module.blocks(["a"], measure)
        # Sol r2: the plan files follow the job's minimum (jobs() with 4 blocks a round and a case
        # needing 2: 6 block steps, the plan 3), and a declared outcome's case keeps its one block.
        plan = gate_module.blocks(["a", "b", "c"], measure, 6, square=3, checked={"c"})
        self.assertEqual([sorted(cases) for cases in plan], [["a", "b", "c"]] + [["a", "b"]] * 5)
        # Whole Latin squares of the timed sides (tf2-gate-1: a first run's cost; a case in 2
        # blocks of three sides leaves one side never opening): 2 -> 3, 4 -> 6, 3 stays.
        self.assertEqual(gate_module.blocks(["a", "b", "c"], measure, square=3),
                         [["a", "b", "c"]] * 3 + [["b"]] * 3)
        timed = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None, measure=measure)["rg-test-timed"]
        planned = [i for i, s in enumerate(timed["steps"]) if s[2:] == ["plan-blocks", "c" * 40]]
        self.assertEqual(planned, [1])  # after the builds, before the first block
        self.assertEqual(timed["steps"][2][2], "block")
        self.assertEqual([s[6] for s in timed["steps"] if s[2] == "block"], [f"plan-block-{k}.json" for k in range(1, 7)])
        self.assertEqual((timed["env"]["GATE_SQUARE"], timed["env"]["GATE_MIN_BLOCKS"]), ("3", "3"))
        two = gate_module.jobs("c" * 40, "c" * 40, "C:/repo", "rg-test", 2, None, measure=measure,
                               main=gate_module.BASE)["rg-test-timed"]  # main is base's build: two sides
        self.assertEqual(len([s for s in two["steps"] if s[2] == "block"]), 4)

        def records_of(side, n):
            return [record()] + ([record(case="lab-yago-tiny")] if n <= 12 else [])
        summary = run_gate(passes(lambda side, n: [record(), record(case="lab-yago-tiny")], blocks=3)[:12] +
                           [p for p in passes(records_of, blocks=3) if p["n"] > 12])
        self.assertEqual(summary["errors"], [])
        row = summary["comparisons"]["base"]
        self.assertEqual(row["lab-olympics"]["dimensions"]["time/q01"]["units"], 3)
        self.assertEqual(row["lab-yago-tiny"]["dimensions"]["time/q01"]["units"], 2)

    def test_the_passes_measure_alike_and_the_host_s_policy_holds(self):
        ps = passes()
        ps[3]["report"]["options"] = {**ps[3]["report"]["options"], "measure": "a" * 64}
        self.assertIn("the passes measured with different calibrations (fast.py --measure)", run_gate(ps)["errors"])
        ps = passes()
        state = {"frequency": {"driver": ["amd-pstate-epp"], "governor": ["performance"], "boost": "1"}}
        for p in ps:
            p["report"]["host_state"] = copy.deepcopy(state)
        ps[5]["report"]["host_state"]["frequency"]["governor"] = ["powersave"]
        self.assertTrue(any("frequency policy changed" in e for e in run_gate(ps)["errors"]))
        self.assertEqual(fast.scope_stalls_ms("x\nfast-scope-pressure cpu=1500 io=250000 memory=\n"),
                         {"cpu": 1.5, "io": 250.0})

    def test_a_series_in_coarse_steps_is_tested_at_its_step(self):
        self.assertEqual(gate_module.quantum([[[4.0, 5.0]], [[17.4, 5.5]]]), 0.1)
        self.assertEqual(gate_module.quantum([[[0.0091, 4.0]]]), 0.0)

    def test_calibrate_buys_runs_where_cheap_and_blocks_where_noise_needs_them_within_budget(self):
        quiet = [10.0, 10.01, 9.99, 10.0, 10.0, 10.01]
        noisy = [10.0, 10.4, 9.7, 10.2, 9.9, 10.3]

        def records_of(side, n):
            i = (n - 1) % 6
            out = [record(case="lab-olympics", samples={"q01": [quiet[i]] * 5}),
                   record(case="q-paths", samples={"broader-closure": [noisy[i]] * 5})]
            for r in out:
                r["case_s"] = 10.0
            return out
        ps = passes(records_of)
        catalog = [{"name": "lab-olympics", "metric": "sum_p50_ms", "queries": "@olympics", "args": []},
                   {"name": "q-paths", "metric": "sum_p50_ms", "queries": "q-paths", "args": ["--runs", "10"]}]
        cheap = gate_module.calibrate(catalog, ps, budget_min=1, m=10, batch=False)["cases"]
        self.assertEqual((cheap["lab-olympics"]["runs"], cheap["lab-olympics"]["warmup"]), (200, 3))
        self.assertEqual((cheap["lab-olympics"]["blocks"], cheap["q-paths"]["blocks"]), (2, 2))
        rich = gate_module.calibrate(catalog, ps, budget_min=240, m=10, batch=False)
        # Two blocks are one degree of freedom (t at alpha / m: about 64): even the quiet case buys one.
        self.assertEqual(rich["cases"]["lab-olympics"]["blocks"], 3)
        self.assertGreater(rich["cases"]["q-paths"]["blocks"], 3)
        self.assertLessEqual(rich["minutes"], 240)
        import tomllib
        self.assertEqual(tomllib.loads(gate_module.write_measure(rich, "a test"))["case"]["q-paths"]["blocks"],
                         rich["cases"]["q-paths"]["blocks"])

    def test_fast_applies_the_calibration_alike_and_pools_repeated_processes(self):
        case = {"name": "x", "tool": "perf_lab", "args": ["--runs", "5", "--qerror"], "repeat": 1}
        with patch.dict(fast.MEASURE, {"x": {"runs": 41, "warmup": 3, "batch": 7, "repeat": 2}}, clear=True), \
                patch.dict(fast.OPTIONS, {"batch": False}):
            got = fast.measured(case)
        self.assertEqual(got["args"], ["--runs", "41", "--qerror", "--warmup", "3"])
        self.assertEqual(got["repeat"], 2)
        with patch.dict(fast.MEASURE, {"x": {"batch": 7, "batch_queries": ["q01", "q07"]}}, clear=True), \
                patch.dict(fast.OPTIONS, {"batch": True}):
            self.assertEqual(fast.measured(case)["args"][-4:], ["--batch", "7", "--batch-queries", "q01,q07"])
        result = {"queries": [{"name": "q", "samples_ms": [3.0]}],
                  "_repeats": [{"queries": [{"name": "q", "samples_ms": [1.0, 2.0]}]},
                               {"queries": [{"name": "q", "samples_ms": [3.0]}]}]}
        self.assertEqual(fast.samples({"metric": "sum_p50_ms"}, result), {"q": [1.0, 2.0, 3.0]})


class Instances(unittest.TestCase):
    """lks-abba-11 and -13 timed the same two files: creaders' read p50 x1.032 and x0.951, each at
    p = 0.001; lks-aa-20 timed a build against a byte-identical copy: x1.048 in one session, x0.983
    in the next. The offset is the cached file instance's, the same in every block that times it."""

    def test_every_block_times_a_fresh_copy_and_the_gate_refuses_a_build_timed_in_place(self):
        with tempfile.TemporaryDirectory() as tmp:
            built = Path(tmp) / "bin-cccccccc-l1"
            (built / "release" / "examples").mkdir(parents=True)
            (built / "release" / "examples" / "perf_lab").write_bytes(b"\x7fELF perf lab")
            (built / ".refs").mkdir()
            side = {"name": "cand", "bin": built.as_posix()}
            first, = fast.fresh_copies([side], Path(tmp) / "block-1")
            second, = fast.fresh_copies([side], Path(tmp) / "block-2")
            lab = [Path(s["bin"]) / "release" / "examples" / "perf_lab" for s in (first, second)]
            self.assertEqual([x.read_bytes() for x in lab], [b"\x7fELF perf lab"] * 2)
            self.assertFalse(lab[0].samefile(built / "release" / "examples" / "perf_lab"))
            self.assertFalse(lab[0].samefile(lab[1]))
            self.assertEqual((first["built"], (Path(first["bin"]) / ".refs").exists()), (built.as_posix(), False))
        ps = passes()
        del ps[2]["report"]["instance"]
        self.assertIn("timed its side's build in place", " ".join(run_gate(ps)["errors"]))

    def test_a_block_removes_only_the_copies_it_made_also_when_a_copy_fails(self):
        """Sol, 20 Oct 2026: block removed a block-<n>-<pid> directory that was there before it,
        copied outside its cleanup, and took any side name as a path."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "instances"
            other = root / f"block-1-{os.getpid()}"
            other.mkdir(parents=True)
            (other / "keep").write_text("another block's", encoding="utf-8")
            built = Path(tmp) / "bin"
            (built / "release").mkdir(parents=True)
            (built / "release" / "tool").write_text("tool", encoding="utf-8")
            sides = [{"name": "base", "bin": built.as_posix()}, {"name": "cand", "bin": built.as_posix()}]
            with fast.block_copies(sides, root, 1) as timed:
                self.assertTrue(all(Path(s["bin"], "release", "tool").is_file() for s in timed))
            self.assertEqual(sorted(p.name for p in root.iterdir()), [other.name])
            with self.assertRaises(OSError), fast.block_copies([*sides, {"name": "main", "bin": f"{tmp}/none"}], root, 1):
                pass
            self.assertEqual(sorted(p.name for p in root.iterdir()), [other.name])
            self.assertTrue((other / "keep").is_file())
        with self.assertRaises(SystemExit):
            fast.parse_side("../escape:/bin:rev")

    def test_each_square_of_blocks_gives_every_side_every_place_once_drawn_from_the_seed(self):
        """tf2-gate-1 (20 Oct 2026): a block's first run cost 15.8 % on one series. With the mirror
        alternating per pair of blocks the middle side (main; A' in an A-B-B-A) never opened."""
        firsts = set()
        for seed in [None, *map(str, range(20))]:
            for square in range(3):
                orders = [[s for s, run in fast.palindrome(list(SIDES), 3 * square + j, seed) if run == 1]
                          for j in (1, 2, 3)]
                for place in range(3):
                    self.assertEqual(sorted(o[place] for o in orders), sorted(SIDES), (seed, square, orders))
            firsts.add(fast.palindrome(list(SIDES), 1, seed)[0][0])
            self.assertEqual(fast.palindrome(list(SIDES), 3, seed), fast.palindrome(list(SIDES), 3, seed))
        self.assertEqual(firsts, set(SIDES))
        self.assertEqual([fast.palindrome(["a", "b"], b)[0][0] for b in (1, 2, 3)], ["a", "b", "a"])

    def test_a_loss_is_judged_against_the_host_s_a_a_floor(self):
        """An A-A' gate (every side one revision, each block its own copies) gives the floor; a
        round whose candidate is 3.5 % slower in every block, by its instances' luck as in
        lks-abba-11, is no loss against it, while a 15 % loss still is."""
        import math
        import random

        def offset(side, n):  # each block's copy of a side: its own offset
            return math.exp(random.Random(f"{side}{(n - 1) // 6}").gauss(0, 0.04))

        def copies(side, n):
            return [record(samples={"q06": [v * offset(side, n) for v in (4.8, 4.85, 4.8)]})]
        aa = passes(copies, blocks=8, sides=("base", "cand"))  # main, the same build, timed once as base
        for p in aa:
            for key in ("binaries", "binaries_finished"):
                p["report"][key]["revision"] = REVS["base"]
        summary = gate_module.gate(aa, {s: REVS["base"] for s in ("cand", "base", "main")}, "cand",
                                   ["base", "main"], HARNESS, context=CONTEXT)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "phuoc-yu.toml"
            path.write_text(gate_module.write_floor(gate_module.noise_floor(summary), "aa-test"), encoding="utf-8")
            floor = gate_module.read_floor(path)
        self.assertGreater(floor["cases"]["lab-olympics"]["time/q06"], 0.03)

        def flagged(factor, floor_):
            s = run_gate(passes(slower(factor), blocks=4), floor=floor_)
            return {f["side"] for f in s["flagged"] if f["dimension"] == "time/q06"}
        self.assertEqual(flagged(1.035, None), {"base", "main"})
        self.assertEqual(flagged(1.035, floor), set())
        self.assertEqual(flagged(1.15, floor), {"base", "main"})
        with self.assertRaises(ValueError):  # a gate of two revisions is no A-A' run
            gate_module.noise_floor(run_gate(passes()))


class MeasurementSides(unittest.TestCase):
    """A side is built and timed from its revision, or from its measurement commit: a commit on
    that revision alone that changes nothing but its perf lab (PROTOCOL.md §5). T0, 28 Oct 2026:
    until then the gate patched main's and v1.5's perf lab at build time, code no commit held."""

    PERF_LAB = "crates/nrese-store/examples/perf_lab.rs"
    ENGINE = "crates/nrese-store/src/lib.rs"

    def setUp(self):
        self.repo = tempfile.mkdtemp(prefix="measurement-sides-")
        self.addCleanup(shutil.rmtree, self.repo, True)
        self.git("init", "-q")
        self.side = self.commit({self.ENGINE: "pub fn engine() {}\n", self.PERF_LAB: "fn main() { spawn(); }\n",
                                 "Cargo.lock": "# the side's\n"}, "the side")
        self.later = self.commit({"README.md": "later\n"}, "a later revision")

    def git(self, *args: str) -> str:
        return subprocess.run(["git", "-C", self.repo, *args], capture_output=True, text=True, check=True).stdout.strip()

    def commit(self, changes: dict, message: str, on: str | None = None) -> str:
        """A commit of `changes` (path: text, or None to delete) on `on` (else the current HEAD)."""
        if on:
            self.git("checkout", "-q", "--detach", on)
        for path, text in changes.items():
            file = Path(self.repo, path)
            if text is None:
                file.unlink()
            else:
                file.parent.mkdir(parents=True, exist_ok=True)
                file.write_text(text, encoding="utf-8")
        self.git("add", "-A")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.org", "commit", "-q", "-m", message)
        return self.git("rev-parse", "HEAD")

    def test_only_a_commit_on_the_side_that_changes_just_its_perf_lab_is_built_for_it(self):
        loop = {self.PERF_LAB: "fn main() { arm(); }\n"}
        outside, parent = "changes more than its perf lab", "isn't a commit on"
        cases = {  # name: (its changes, the revision it is on, None if built for the side, else why not)
            "its perf lab": (loop, self.side, None),
            "the engine as well": ({**loop, self.ENGINE: "pub fn engine() { faster() }\n"}, self.side, outside),
            "its lockfile": ({**loop, "Cargo.lock": "# another\n"}, self.side, outside),
            "the engine moved into the perf lab": ({self.ENGINE: None,
                                                    "crates/nrese-store/examples/lib.rs": "pub fn engine() {}\n"},
                                                   self.side, outside),
            "on another revision": (loop, self.later, parent),
        }
        wrong = []
        for name, (changes, on, why) in cases.items():
            commit = self.commit(changes, name, on)
            try:
                got = "built" if gate_module.measured_at(self.repo, self.side, {self.side: commit}) == commit else "?"
            except SystemExit as e:
                got = str(e)
            if (got == "built") != (why is None) or (why and why not in got):
                wrong.append(f"{name}: {got}")
        self.assertEqual(wrong, [], "SPEC measurement side: its only parent is the side, and it changes "
                                    "nothing outside the perf lab's sources")

    def test_a_change_outside_the_perf_lab_is_seen_whatever_the_diff_configuration(self):
        """Sol, PR #54 r1 R1: under a user's diff.ignoreSubmodules=all, `git diff` left a gitlink
        added outside the perf lab out of its paths, and the commit was built for the side."""
        self.git("config", "diff.ignoreSubmodules", "all")
        self.git("checkout", "-q", "--detach", self.side)
        self.git("update-index", "--add", "--cacheinfo", f"160000,{self.side},outside-submodule")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.org", "commit", "-q", "-m", "a gitlink outside")
        gitlink = self.git("rev-parse", "HEAD")
        with self.assertRaisesRegex(SystemExit, "changes more than its perf lab .*outside-submodule"):
            gate_module.measured_at(self.repo, self.side, {self.side: gitlink})
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        self.assertEqual(gate_module.measured_at(self.repo, self.side, {self.side: loop}), loop,
                         "SPEC measurement side: its perf lab alone is still built for it")

    def test_a_replace_ref_changes_nothing_the_check_reads(self):
        """Sol, PR #54 R1 fix review D1 and D2: a replace ref stands other objects in for the
        stored ones; standing the side's tree in for an engine change hid the change (D1). The
        check reads the objects as stored (STORED_OBJECTS): the change is refused, and a valid
        commit is admitted whatever stands in for its tree, which the gate's checkout doesn't read
        either (test_the_build_compiles_the_commit_checked)."""
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        engine = self.commit({self.ENGINE: "pub fn engine() { faster() }\n"}, "the engine", self.side)
        tree = lambda commit: self.git("rev-parse", f"{commit}^{{tree}}")
        self.git("replace", tree(engine), tree(self.side))
        with self.assertRaisesRegex(SystemExit, "changes more than its perf lab .*crates/nrese-store/src/lib.rs"):
            gate_module.measured_at(self.repo, self.side, {self.side: engine})
        self.git("replace", "-d", tree(engine))
        self.git("replace", tree(loop), tree(engine))
        self.assertEqual(gate_module.measured_at(self.repo, self.side, {self.side: loop}), loop)

    def gate_beside(self, loop: str) -> Path:
        """The gate's scripts beside the side's commits, in a commit of their own on HEAD, its side
        measured at LOOP: the commit the gate is launched from, whose checkout its code runs from
        (release-gate.sh); a scratch directory for its runs."""
        for rel in ("scripts/release-gate.sh", "benches/fast/release_gate.py", ".gitignore"):
            Path(self.repo, rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / rel, Path(self.repo, rel))
        shutil.copytree(ROOT / "scripts/lib", Path(self.repo, "scripts/lib"), ignore=shutil.ignore_patterns("__pycache__"))
        shutil.copytree(ROOT / "benches/suite/suitekit", Path(self.repo, "benches/suite/suitekit"),
                        ignore=shutil.ignore_patterns("__pycache__"))
        Path(self.repo, "benches/fast/measurement-sides.toml").write_text(
            f'[side.{self.side}]\ncommit = "{loop}"\nref = "refs/measure/t-{self.side[:8]}"\n', encoding="utf-8")
        self.git("add", ".gitignore", "scripts", "benches")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.org", "commit", "-q", "-m", "the gate")
        tmp = Path(tempfile.mkdtemp(prefix="measurement-build-"))
        self.addCleanup(shutil.rmtree, tmp, True)
        return tmp

    def test_the_build_compiles_the_revisions_bytes(self):
        """Sol, PR #54 fix check 1 S1: worktree() took HEAD and a clean status for the revision's
        source, and both trust the index and the attributes. A smudge filter (info/attributes) made
        the checkout other bytes, and a reused checkout changed under core.checkStat=minimal with its
        size and mtime kept stayed clean: the build went on with either. Now it refuses both, and a
        checkout as stored still builds."""
        bash = shutil.which("bash")
        if bash is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        git_dir = Path(self.git("rev-parse", "--absolute-git-dir"))

        def build(gate: str) -> str:
            env = {**os.environ, "NRESE_GATE_DIR": (tmp / gate / "gate").as_posix(),
                   "GATE_BIN": (tmp / gate / "bin").as_posix(), "FLEET_OUT": (tmp / gate / "out").as_posix()}
            done = subprocess.run([bash, "scripts/release-gate.sh", "build", self.side], cwd=self.repo, env=env,
                                  capture_output=True, text=True)
            return done.stdout + done.stderr

        went = f"build {self.side} at {loop}"
        # A filter pair rewrites the engine at checkout and back for status: HEAD and a clean
        # status agree with the rewritten bytes.
        self.git("config", "filter.other.smudge", "sed s/engine/rival/")
        self.git("config", "filter.other.clean", "sed s/rival/engine/")
        (git_dir / "info").mkdir(exist_ok=True)
        (git_dir / "info" / "attributes").write_text("*.rs filter=other\n", encoding="utf-8")
        smudged = build("smudged")
        (git_dir / "info" / "attributes").unlink()
        self.git("config", "--remove-section", "filter.other")
        # A checkout as stored builds. Its engine then changed, size and mtime kept, under a stat
        # cache that compares only those, its entry older than the index (not racily clean).
        stored = build("stat")
        src = self.space(tmp / "stat" / "gate") / "build" / f"src-{loop[:8]}"
        engine = src / self.ENGINE
        old = os.stat(engine).st_mtime_ns - 10**10
        os.utime(engine, ns=(old, old))
        self.git("-C", src.as_posix(), "update-index", "-q", "--refresh")
        engine.write_text(engine.read_text(encoding="utf-8").replace("engine", "engin3"), encoding="utf-8")
        os.utime(engine, ns=(old, old))
        self.git("config", "core.checkStat", "minimal")
        self.git("config", "core.trustctime", "false")
        changed = build("stat")
        self.git("config", "--unset", "core.checkStat")
        self.git("config", "--unset", "core.trustctime")
        # A checkout as stored, plus a file the repository ignores, which status doesn't show and
        # a build may read (nrese-server's build.rs embeds apps/nrese-console/dist/; Sol, S1 r1):
        # it builds without it (Sol, S1 r2: the gate's own leftovers go the same way).
        clean = build("ignored")
        (git_dir / "info" / "exclude").write_text("dist/\n", encoding="utf-8")
        extra = self.space(tmp / "ignored" / "gate") / "build" / f"src-{loop[:8]}" / "dist" / "index.html"
        extra.parent.mkdir(parents=True)
        extra.write_text("<p>not the revision's</p>\n", encoding="utf-8")
        ignored = build("ignored")
        self.assertEqual((went in smudged, went in stored, went in changed, went in clean, went in ignored,
                          extra.exists()), (False, True, False, True, True, False),
                         f"SPEC the gate builds the revision's stored bytes and nothing else\n--- smudged\n"
                         f"{smudged}\n--- changed\n{changed}\n--- ignored\n{ignored}")
        self.assertIn("doesn't hold", smudged + changed)

    def test_the_gates_own_checkout_may_hold_its_scratch_a_built_one_not(self):
        """Sol, S1 r2: the gate writes ignored scratch into its own harness checkout (fast.py's
        tmp/fast/queries, the watcher's __pycache__, cargo-guarded's tmp logs) and checks that
        checkout again before each block: only a checkout to be built must hold nothing else."""
        self.git("checkout", "-q", "--detach", self.side)
        scratch = Path(self.repo, "tmp", "fast", "queries", "q.rq")
        scratch.parent.mkdir(parents=True)
        scratch.write_text("SELECT * {}\n", encoding="utf-8")
        self.assertEqual(gate_module.checkout_differs(self.repo, self.side), [],
                         "SPEC the harness's checkout: its stored bytes, the gate's scratch beside them")
        self.assertIn("tmp/fast/queries/q.rq (not in the revision)",
                      gate_module.checkout_differs(self.repo, self.side, nothing_else=True),
                      "SPEC a checkout to be built: nothing else")
        # Sol, S1 r3: code the harness would import isn't scratch: a sourceless benches/suite/csv.pyc
        # (ignored as *.py[cod]) loads before the standard library's csv.
        planted = Path(self.repo, "benches", "suite", "csv.pyc")
        planted.parent.mkdir(parents=True, exist_ok=True)
        planted.write_bytes(b"\x00not the revision's\n")
        self.assertEqual(gate_module.checkout_differs(self.repo, self.side),
                         ["benches/suite/csv.pyc (not in the revision)"],
                         "SPEC the harness's checkout: no code but the revision's")

    def test_the_gates_own_checkout_holds_nothing_else_outside_its_scratch(self):
        """Sol, PR #60 r1 R1: the harness's checkout refused only names that looked like code, by a
        case-sensitive suffix. An ignored benches/cases/private.toml put an unreviewed case in the
        plan (the catalogue reads every *.toml), and Windows imports benches/suite/csv.PYC as
        csv.pyc. A scratch root that was a link was left out before links were looked at."""
        self.git("checkout", "-q", "--detach", self.side)
        named = lambda: [d.split(" (")[0] for d in gate_module.checkout_differs(self.repo, self.side)]
        for planted in ("benches/cases/private.toml", "benches/suite/csv.PYC"):
            path = Path(self.repo, planted)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"not the revision's\n")
            with self.subTest(planted=planted):
                self.assertEqual(named(), [planted], "SPEC the harness's checkout: its revision and its scratch")
            path.unlink()
        elsewhere = tempfile.mkdtemp(prefix="linked-scratch-")
        self.addCleanup(shutil.rmtree, elsewhere, True)
        try:
            os.symlink(elsewhere, Path(self.repo, "tmp"), target_is_directory=True)
        except OSError as error:
            self.skipTest(f"no directory link here: {error}")
        self.assertEqual(named(), ["tmp"], "SPEC the scratch is the checkout's own directory")

    def test_no_bytecode_from_before_the_run_is_executed(self):
        """Sol, PR #60 r1 R2: the gate kept Python's caches in $GATE/pycache, where they outlive a
        run and no check reads them: an unchecked-hash .pyc planted there for suitekit/cases.py ran
        in place of the source the gate had checked. No cache older than a run runs in it: an
        earlier run's go with it, and one planted where they were kept, or beside the source, isn't
        read."""
        bash = shutil.which("bash")
        if bash is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        import py_compile
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        gate = tmp / "planted"
        build = lambda: self.run_gate(tmp, "planted", "build", self.side)
        went = f"build {self.side} at {loop}"
        self.assertIn(went, build(), "an earlier run")
        source = str(Path(self.repo, "benches/suite/suitekit/cases.py").resolve())
        body = tmp / "planted.py"
        body.write_text('raise RuntimeError("PR60 planted bytecode executed")\n', encoding="utf-8")
        self.assertEqual(list((gate / "gate" / "run").glob("*/[0-9]*-*")), [], "SPEC an earlier run's caches go with it")
        planted = []
        for prefix in ((gate / "gate" / "pycache").as_posix(), None):
            held, sys.pycache_prefix = sys.pycache_prefix, prefix
            try:
                planted.append(importlib.util.cache_from_source(source))
            finally:
                sys.pycache_prefix = held
        for cached in planted:
            py_compile.compile(str(body), cfile=str(cached), dfile=source, doraise=True,
                               invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)
        output = build()
        self.assertNotIn("PR60 planted bytecode executed", output, "SPEC the gate runs the source it checked")
        self.assertIn(went, output)

    def test_nothing_the_launching_checkout_holds_beside_its_commit_runs(self):
        """Sol, PR #60 fix checks 1 and 2: files in the checkout the gate was launched from, beside
        its commit's, ran in the validator before anything was checked: a sourceless
        suitekit/cases/__init__.pyc (git clean -fd keeps it), an untracked suitekit/cases/__init__.py
        or benches/suite/csv.py (no rule of what may lie there tells one from a commit hook's new
        file). The gate's code runs from a checked checkout of the launching commit: nothing else
        in the launching checkout runs, nor stops the gate."""
        if shutil.which("bash") is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        import py_compile
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        went = f"build {self.side} at {loop}"
        ran = "PR60 A LAUNCHING CHECKOUT FILE RAN"
        body = tmp / "planted.py"
        body.write_text(f'raise RuntimeError("{ran}")\n', encoding="utf-8")
        planted = []
        for name in ("benches/suite/suitekit/cases/__init__.py", "benches/suite/csv.py",
                     "benches/suite/target/__init__.pyc", "benches/suite/__pycache__/__init__.pyc"):
            path = Path(self.repo, name)
            path.parent.mkdir(parents=True, exist_ok=True)
            if name.endswith(".pyc"):
                py_compile.compile(str(body), cfile=str(path), doraise=True)
            else:
                shutil.copy(body, path)
            planted.append(path)
        output = self.run_gate(tmp, "sources", "build", self.side)
        self.assertNotIn(ran, output, "SPEC nothing beside the launching commit's files runs")
        self.assertIn(went, output, "SPEC nor stops the gate")
        # Sol, fix check 1: the sourceless package, also in a directory no walk can list (0111).
        planted[0].unlink()
        py_compile.compile(str(body), cfile=str(planted[0].with_suffix(".pyc")), doraise=True)
        output = self.run_gate(tmp, "sourceless", "build", self.side)
        self.assertNotIn(ran, output, "SPEC nothing beside the launching commit's files runs")
        self.assertIn(went, output, "SPEC nor stops the gate")
        if os.name == "nt" or os.geteuid() == 0:
            self.skipTest("a directory only searchable needs a non-root POSIX user")
        os.chmod(planted[0].parent, 0o111)
        self.addCleanup(os.chmod, planted[0].parent, 0o755)
        output = self.run_gate(tmp, "unlisted", "build", self.side)
        self.assertNotIn(ran, output, "SPEC nothing beside the launching commit's files runs")
        self.assertIn(went, output, "SPEC nor stops the gate")

    def without_flock(self, tmp: Path) -> str:
        """A PATH of this one's commands but flock, as on a host without it (Git Bash): ns.sh then
        locks with suitekit/lock.py."""
        if shutil.which("flock") is None:
            return os.environ.get("PATH", "")
        farm = tmp / "no-flock"
        farm.mkdir()
        for entry in os.environ.get("PATH", "").split(os.pathsep):
            for name in os.listdir(entry) if os.path.isdir(entry) else ():
                target = Path(entry, name)
                if name != "flock" and not os.path.lexists(farm / name) and target.is_file() \
                        and os.access(target, os.X_OK):
                    (farm / name).symlink_to(target)
        return farm.as_posix()

    def test_no_python_of_the_launching_checkout_runs_in_the_libraries(self):
        """Sol, PR #60 fix 3's sol-loop S1: scripts/lib/ns.sh, sourced from the launching checkout,
        named that checkout's suite for the Python it starts: where there is no flock it locks with
        suitekit/lock.py, and a changed one there ran in `clean --patch-keyed --remove`. The
        libraries' Python is the launching commit's: the checked checkout's, or for `clean`, which
        makes none, its lock helper's bytes alone."""
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("needs bash")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        ran = "PR60 A LAUNCHING CHECKOUT LOCK RAN"
        Path(self.repo, "benches/suite/suitekit/lock.py").write_text(f'raise RuntimeError("{ran}")\n',
                                                                     encoding="utf-8")
        path = self.without_flock(tmp)
        # BIN NAME IDENTITY-LINE AGE: a build there, its files and references AGE old.
        make = r'''b=$1/$2; mkdir -p "$b/release/examples" "$b/.refs"; echo tool > "$b/release/examples/perf_lab"
printf 'revision x\n%s\nlayout 1\n' "$3" > "$b/IDENTITY"; echo built > "$b/BUILD.txt"
(cd "$b" && sha256sum release/examples/perf_lab > SHA256SUMS); touch "$b/.refs/gate-$2"
find "$b" -maxdepth 2 -exec touch -d "$4" {} +'''
        subprocess.run([bash, "-c", make, "make", (tmp / "keyed" / "bin").as_posix(), "bin-unused",
                        "patch perf_lab_watchdog.py 0123456789abcdef", "3 days ago"], check=True)
        output = self.run_gate(tmp, "keyed", "clean", "--patch-keyed", "--remove", path=path)
        self.assertNotIn(ran, output, "SPEC no Python of the launching checkout runs")
        self.assertIn("released-now\tbin-unused", output, "SPEC the build goes, under the launching commit's lock")
        subprocess.run([bash, "-c", make, "make", (tmp / "clean" / "bin").as_posix(), "bin-old",
                        "profile release --locked", "30 days ago"], check=True)
        output = self.run_gate(tmp, "clean", "clean", path=path)
        self.assertNotIn(ran, output, "SPEC no Python of the launching checkout runs")
        self.assertIn("unused builds removed: bin-old", output, "SPEC clean locks with the launching commit's Python")
        self.assertEqual(list((tmp / "clean" / "gate" / "run").glob("*/[0-9]*-*")), [], "SPEC and leaves no run of its own")

    def test_a_callers_paths_are_the_launching_checkouts(self):
        """Sol, PR #60 fix 3's sol-loop S2: the gate runs its code from a checkout of its own, and
        `jobs HEAD --out jobs` wrote its jobs into that checkout, named it their repository, and
        left it unclean, so the gate's next start refused it. A caller's paths, and the jobs'
        repository, are the launching checkout's."""
        if shutil.which("bash") is None:
            self.skipTest("needs bash")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        # The jobs place their parts by the host registry, which the gate's commit holds.
        Path(self.repo, "benches").mkdir(exist_ok=True)
        shutil.copy(ROOT / "benches/hosts.toml", Path(self.repo, "benches/hosts.toml"))
        tmp = self.gate_beside(loop)
        for start in ("first", "next"):
            output = self.run_gate(tmp, "jobs", "jobs", "HEAD", "--base", "HEAD", "--main", "HEAD", "--no-checks",
                                   "--out", "jobs")
            written = sorted(Path(self.repo, "jobs").glob("*.toml"))
            self.assertTrue(written, f"SPEC the {start} start writes the jobs where the caller said:\n{output}")
        for file in written:
            repositories = re.findall(r'git = "([^"]+)"', file.read_text(encoding="utf-8"))
            self.assertTrue(repositories, file.name)
            for repository in repositories:
                self.assertEqual(Path(repository).resolve(), Path(self.repo).resolve(),
                                 f"SPEC the jobs' repository is the launching checkout: {file.name}")

    def test_the_report_reads_its_own_jobs_plan_not_the_gate_directory_s(self):
        """Review r1 R1-F3: `release-gate.sh report` read $GATE/plan.json and inputs.txt, which another
        request's preparation on the same host could have replaced; the report then judged its passes
        against another family. It reads the copies its own job's prepare left in its results ($OUT);
        a confirmation, which prepares nothing (GATE_PLAN), reads its part's."""
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("needs bash")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        head = self.git("rev-parse", "HEAD")
        args_file = tmp / "report-args.txt"
        (tmp / "shim").mkdir()
        (tmp / "shim" / "python3").write_text(f"""#!/usr/bin/env bash
case " $* " in *" benches/fast/release_gate.py report "*)
  printf '%s\\n' "$@" > "{args_file.as_posix()}"; echo '{{}}' > "$FLEET_OUT/release-gate.json"; exit 0 ;; esac
exec "{Path(sys.executable).as_posix()}" "$@"
""", encoding="utf-8", newline="\n")
        (tmp / "shim" / "python3").chmod(0o755)

        def report(name: str, extra: dict) -> list[str]:
            env = self.gate_env(tmp, name)
            gate, out = Path(env["NRESE_GATE_DIR"]), Path(env["FLEET_OUT"])
            gate.mkdir(parents=True, exist_ok=True)
            out.mkdir(parents=True, exist_ok=True)
            # The gate directory: another request's plan; the job's results: its own.
            for where, label in ((gate, "another"), (out, "its-own")):
                (where / "plan.json").write_text(json.dumps(
                    {"run": [label], "not_run": {f"{label}-not": "why"}, "elsewhere": {}, "family": []}), encoding="utf-8")
                (where / "inputs.txt").write_text(f"{label} inputs\n", encoding="utf-8")
            (gate / "plan-confirm1.json").write_text(json.dumps(
                {"run": ["confirm"], "not_run": {"confirm-not": "why"}, "elsewhere": {}}), encoding="utf-8")
            args_file.unlink(missing_ok=True)
            done = subprocess.run([bash, "scripts/release-gate.sh", "report", head, f"cand={head}", f"base={head}"],
                                  cwd=self.repo, env={**env, **extra}, capture_output=True, text=True)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            return args_file.read_text(encoding="utf-8").splitlines()

        def value(args: list[str], flag: str) -> str:
            return args[args.index(flag) + 1]

        own = report("own", {})
        out = Path(self.gate_env(tmp, "own")["FLEET_OUT"])
        self.assertEqual(Path(value(own, "--family")), out / "plan.json")
        self.assertEqual(Path(value(own, "--inputs")), out / "inputs.txt")
        self.assertIn("its-own-not=why", own)
        self.assertNotIn("another-not=why", own)
        confirm = report("confirm", {"GATE_PLAN": "plan-confirm1.json"})
        gate = Path(self.gate_env(tmp, "confirm")["NRESE_GATE_DIR"])
        self.assertEqual(Path(value(confirm, "--family")), gate / "plan.json")
        self.assertIn("confirm-not=why", confirm)

    def test_callers_at_once_share_no_code_checkout(self):
        """Sol, PR #60 fix check 3 (R3): the code checkout, shared under the launching commit's name,
        had no owner: of two `jobs` started at once on one gate directory, one met the other's
        checkout half made (a directory, not yet a checkout) and both failed. Each invocation makes,
        checks and removes its own: one paused while making it stops no other, neither leaves
        anything behind, and a clean and a restart after them run."""
        if shutil.which("bash") is None:
            self.skipTest("needs bash")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        # The jobs place their parts by the host registry, which the gate's commit holds.
        Path(self.repo, "benches").mkdir(exist_ok=True)
        shutil.copy(ROOT / "benches/hosts.toml", Path(self.repo, "benches/hosts.toml"))
        tmp = self.gate_beside(loop)
        # git, but the first `worktree add` makes its directory and waits there until released.
        signals = tmp / "signals"
        signals.mkdir()
        hold = tmp / "hold" / "git"
        hold.parent.mkdir()
        hold.write_text(f'''#!/usr/bin/env bash
case " $* " in
  *" worktree add "*)
    if mkdir "{signals.as_posix()}/claimed" 2>/dev/null; then
      mkdir -p "${{@: -2:1}}" && touch "{signals.as_posix()}/made"
      while [ ! -e "{signals.as_posix()}/go" ]; do sleep 0.05; done
    fi ;;
esac
exec "{Path(shutil.which("git")).as_posix()}" "$@"
''', encoding="utf-8", newline="\n")
        hold.chmod(0o755)
        path = hold.parent.as_posix() + os.pathsep + os.environ.get("PATH", "")
        jobs = lambda out: ("jobs", "HEAD", "--base", "HEAD", "--main", "HEAD", "--no-checks", "--out", out)
        start = lambda out: subprocess.Popen(
            [shutil.which("bash"), "scripts/release-gate.sh", *jobs(out)], cwd=self.repo,
            env=self.gate_env(tmp, "at-once", path), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        first = start("jobs-a")
        self.addCleanup(lambda: first.poll() is None and first.kill())
        try:
            deadline = time.monotonic() + 120
            while not (signals / "made").exists():
                self.assertIsNone(first.poll(), "the first caller ended before making its checkout")
                self.assertLess(time.monotonic(), deadline, "the first caller made no checkout")
                time.sleep(0.05)
            second = start("jobs-b")
            self.addCleanup(lambda: second.poll() is None and second.kill())
            output = second.communicate(timeout=300)[0]
            self.assertEqual(second.returncode, 0, f"SPEC a caller beside a half-made checkout runs:\n{output}")
        finally:
            (signals / "go").touch()
        output = first.communicate(timeout=300)[0]
        self.assertEqual(first.returncode, 0, f"SPEC so does the one paused while making it:\n{output}")
        for out in ("jobs-a", "jobs-b"):
            self.assertTrue(list(Path(self.repo, out).glob("*.toml")), f"{out}: no jobs written")
        self.assertEqual(list((tmp / "at-once" / "gate" / "run").glob("*/[0-9]*-*")), [], "SPEC each removes its own")
        self.assertIn("MiB reclaimed", self.run_gate(tmp, "at-once", "clean"), "SPEC a clean after them runs")
        output = self.run_gate(tmp, "at-once", *jobs("jobs-c"))
        self.assertTrue(list(Path(self.repo, "jobs-c").glob("*.toml")), f"SPEC and a restart:\n{output}")
        listed = self.git("worktree", "list", "--porcelain")
        self.assertEqual(listed.count("worktree "), 1, f"SPEC no checkout stays registered:\n{listed}")

    def test_what_a_dead_run_left_stops_neither_clean_nor_build(self):
        """Sol, PR #60 fix check 3 (R3): a code checkout left half made (a directory, not a checkout)
        stopped `build` and `clean` alike, clean behind the bootstrap that refused it. `clean` makes
        no code checkout: it removes what dead invocations left, keeps a live one's, and the build
        runs after it."""
        bash = shutil.which("bash")
        if bash is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        gate = tmp / "stale" / "gate"
        head = self.git("rev-parse", "--short=8", "HEAD")
        # A dead invocation's pid, and a live one's, as the gate's shell numbers them.
        dead = subprocess.run([bash, "-c", "echo $$"], capture_output=True, text=True, check=True).stdout.strip()
        live = subprocess.Popen([bash, "-c", "echo $$; exec sleep 300"], stdout=subprocess.PIPE, text=True)
        self.addCleanup(live.wait)
        self.addCleanup(live.kill)
        alive = live.stdout.readline().strip()
        # This process space's run directory: the one a first start (its help) leaves.
        self.run_gate(tmp, "stale")
        space, = (gate / "run").iterdir()
        for pid in (dead, alive):
            (space / f"{pid}-abcdef" / f"src-{head}").mkdir(parents=True)
        (gate / f"src-{head}").mkdir()  # 47e5cee7's shared code checkout, half made (Sol's case)
        output = self.run_gate(tmp, "stale", "clean")
        self.assertIn("MiB reclaimed", output, f"SPEC clean runs:\n{output}")
        self.assertFalse((space / f"{dead}-abcdef").exists(), "SPEC what a dead invocation left goes")
        self.assertTrue((space / f"{alive}-abcdef").exists(), "SPEC a live one's stays")
        output = self.run_gate(tmp, "stale", "build", self.side)
        self.assertIn(f"build {self.side} at {loop}", output, f"SPEC and the build runs:\n{output}")

    def test_clean_asks_only_its_own_process_space(self):
        """Sol, PR #60 fix check 4 (R4): clean took a pid it couldn't find for a dead run's, and a
        live WSL caller's, asked from Git Bash on the same host and gate directory, lost its code
        checkout under it. Clean asks only its own process space: another space's run stays and is
        reported; what a killed run of its own left goes, its checkout's registration with it."""
        bash = shutil.which("bash")
        if bash is None or os.name == "nt":
            self.skipTest("a killed process group: POSIX")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        gate = tmp / "spaces" / "gate"
        head = self.git("rev-parse", "--short=8", "HEAD")
        # git, but `worktree add` waits once its checkout is made, until the run is killed there.
        signals = tmp / "signals"
        signals.mkdir()
        hold = tmp / "hold" / "git"
        hold.parent.mkdir()
        git = Path(shutil.which("git")).as_posix()
        hold.write_text(f'''#!/usr/bin/env bash
case " $* " in
  *" worktree add "*)
    "{git}" "$@" || exit
    touch "{signals.as_posix()}/made"
    while :; do sleep 0.05; done ;;
esac
exec "{git}" "$@"
''', encoding="utf-8", newline="\n")
        hold.chmod(0o755)
        path = hold.parent.as_posix() + os.pathsep + os.environ.get("PATH", "")
        run = subprocess.Popen([bash, "scripts/release-gate.sh"], cwd=self.repo, start_new_session=True,
                               env=self.gate_env(tmp, "spaces", path),
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(lambda: run.poll() is None and os.killpg(run.pid, signal.SIGKILL))
        deadline = time.monotonic() + 120
        while not (signals / "made").exists():
            self.assertIsNone(run.poll(), "the run ended before making its checkout")
            self.assertLess(time.monotonic(), deadline, "the run made no checkout")
            time.sleep(0.05)
        os.killpg(run.pid, signal.SIGKILL)
        run.wait()
        killed = [checkout.parent for checkout in (gate / "run").rglob(f"src-{head}")]
        self.assertEqual(len(killed), 1, f"the killed run left its checkout: {killed}")
        # Another space's run, live there, under a pid this space doesn't have; its checkout is
        # registered under a path this space can't see (WSL's /mnt/c/... from Git Bash): moved away.
        foreign = gate / "run" / "space-elsewhere" / f"{run.pid}-abcdef"
        checkout = foreign / f"src-{head}"
        foreign.mkdir(parents=True)
        self.git("worktree", "add", "-q", "--detach", str(checkout), "HEAD")
        checkout.rename(foreign / "out-of-view")
        output = self.run_gate(tmp, "spaces", "clean")
        self.assertTrue(foreign.exists(), f"SPEC another process space's run stays:\n{output}")
        self.assertIn("clean: 1 runs of another process space kept", output, "SPEC and is reported")
        self.assertFalse(killed[0].exists(), f"SPEC what a killed run of this space left goes:\n{output}")
        (foreign / "out-of-view").rename(checkout)
        back = subprocess.run(["git", "-C", str(checkout), "rev-parse", "HEAD"], capture_output=True, text=True)
        self.assertEqual(back.stdout.strip(), self.git("rev-parse", "HEAD"),
                         f"SPEC another space's checkout stays registered (Sol, R4 sol-loop S1):\n{back.stderr}")
        listed = self.git("worktree", "list", "--porcelain")
        self.assertEqual(listed.count("worktree "), 2,
                         f"SPEC the killed run's registration goes, the main's and the other space's stay:\n{listed}")

    def test_a_registration_left_without_its_checkout_stops_no_build(self):
        """Sol, PR #60 R4 sol-loop S5 (rounds 2 and 4): clean prunes no repository-wide
        registrations (another process space's live checkouts are among them), and a build
        checkout of this space's left registered without its directory (a removal killed after its
        .git went, or a clean killed between removing and deregistering) stopped the next build of
        that revision there. Clean deregisters what it removes itself, and the build makes its own
        space's path again over a registration left there: no other space has that path."""
        if shutil.which("bash") is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        self.run_gate(tmp, "left")  # its help: this process space's directory
        build = self.space(tmp / "left" / "gate") / "build" / f"src-{loop[:8]}"
        build.parent.mkdir(parents=True)
        self.git("worktree", "add", "-q", "--detach", str(build), loop)
        (build / ".git").unlink()  # its removal killed halfway, the .git file gone first
        self.run_gate(tmp, "left", "clean")
        self.assertFalse(build.exists(), "clean removed the build directory")
        listed = self.git("worktree", "list", "--porcelain")
        self.assertNotIn(build.name, listed, f"SPEC and its registration with it:\n{listed}")
        # A clean killed between the two: the directory gone, its registration left.
        build.parent.mkdir(parents=True, exist_ok=True)
        self.git("worktree", "add", "-q", "--detach", str(build), loop)
        shutil.rmtree(build)
        output = self.run_gate(tmp, "left", "build", self.side)
        self.assertIn(f"build {self.side} at {loop}", output, f"SPEC the next build makes its checkout:\n{output}")

    def test_a_registration_missing_here_is_never_taken_over(self):
        """Sol, PR #60 R4 sol-loop S6: `worktree add -f` at a path shared by revision took over a
        registration whose directory was missing here, which may be another process space's live
        checkout under the same path (another mount namespace sharing the repository). The build
        makes its checkout under its own space's path: the other checkout stays registered and
        works."""
        if shutil.which("bash") is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        other = tmp / "elsewhere" / "gate" / "build" / f"src-{loop[:8]}"
        other.parent.mkdir(parents=True)
        self.git("worktree", "add", "-q", "--detach", str(other), loop)
        away = other.with_name("out-of-view")
        other.rename(away)
        output = self.run_gate(tmp, "elsewhere", "build", self.side)
        self.assertIn(f"build {self.side} at {loop}", output, f"the build makes its own checkout:\n{output}")
        away.rename(other)
        back = subprocess.run(["git", "-C", str(other), "rev-parse", "HEAD"], capture_output=True, text=True)
        self.assertEqual(back.stdout.strip(), loop, f"SPEC the other checkout stays registered:\n{back.stderr}")

    def test_clean_never_deletes_a_checkout_it_cannot_remove(self):
        """Sol, PR #60 R4 sol-loop S1 (round 4): where git couldn't remove a checkout, clean's
        fallback deleted it: on shared storage another process space's live harness checkout,
        whose .git names that space's repository mount. Clean deletes only its own space's
        checkouts so; any other it removes by git, or leaves."""
        if shutil.which("bash") is None:
            self.skipTest("needs bash")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        head = self.git("rev-parse", "--short=8", "HEAD")
        other = tmp / "shared" / "gate" / f"src-{head}"
        other.parent.mkdir(parents=True)
        self.git("worktree", "add", "-q", "--detach", str(other), "HEAD")
        (other / ".git").unlink()  # Git for Windows hides it, and Windows won't overwrite a hidden file
        (other / ".git").write_text("gitdir: /elsewhere/repository/.git/worktrees/other\n", encoding="utf-8")
        output = self.run_gate(tmp, "shared", "clean")
        self.assertIn("MiB reclaimed", output, f"clean ran:\n{output}")
        self.assertTrue((other / ".gitignore").is_file(), f"SPEC another space's checkout stays:\n{output}")

    def test_a_checkout_past_windows_path_limit_is_made_and_removed(self):
        """The Windows merge check of PR #60's R4 (gate-windows-6e6f0dd18d-1793236830): the gate's
        checkouts lie a few directories down, and under the job's work directory the tree's longest
        path (123 characters) passed Windows' 260: Git for Windows refused to write it, and the gate
        had no checkout to run from. Its checkouts are made, checked and removed whatever the length
        of the paths in them. (A checkout's own root stays within git's: an explicit GIT_DIR, which
        `worktree add` gives its child, must be shorter than PATH_MAX - 40, 220 on Windows,
        core.longpaths or not.) Only Windows has the limit: elsewhere this holds as it is."""
        if shutil.which("bash") is None:
            self.skipTest("needs bash")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        tmp = self.gate_beside(loop)
        # A 193-character path in the gate's commit, by plumbing: never in this checkout's files.
        long = "benches/" + "p" * 90 + "/" + "q" * 90 + ".txt"
        blob = subprocess.run(["git", "-C", self.repo, "hash-object", "-w", "--stdin"], input="a long path\n",
                              capture_output=True, text=True, check=True).stdout.strip()
        self.git("update-index", "--add", "--cacheinfo", f"100644,{blob},{long}")
        commit = self.git("-c", "user.name=t", "-c", "user.email=t@example.org", "commit-tree", self.git("write-tree"),
                          "-p", "HEAD", "-m", "a long path")
        self.git("update-ref", "HEAD", commit)
        name = "long"
        output = self.run_gate(tmp, name)
        self.assertIn("release-gate.sh build REV", output, f"SPEC the code checkout is made and checked:\n{output}")
        self.assertEqual(list((tmp / name / "gate" / "run").glob("*/[0-9]*-*")), [], "SPEC and removed")
        listed = self.git("worktree", "list", "--porcelain")
        self.assertEqual(listed.count("worktree "), 1, f"SPEC its registration with it:\n{listed}")

    def space(self, gate: Path) -> Path:
        """This process space's directory in GATE (release-gate.sh's $SPACE), where the gate's
        runs and checkouts lie."""
        space, = (gate / "run").glob("space-*")
        return space

    def gate_env(self, tmp: Path, name: str, path: str | None = None) -> dict[str, str]:
        """A release-gate.sh run's environment: its gate, binaries and results under TMP/NAME, its
        python3 this Python (bytecode planted for it carries its tag), then PATH (this process's by
        default)."""
        shim = tmp / "shim" / "python3"
        if not shim.exists():
            shim.parent.mkdir()
            shim.write_text(f'#!/usr/bin/env bash\nexec "{Path(sys.executable).as_posix()}" "$@"\n',
                            encoding="utf-8", newline="\n")
            shim.chmod(0o755)
        gate = tmp / name
        return {**os.environ, "NRESE_GATE_DIR": (gate / "gate").as_posix(), "GATE_BIN": (gate / "bin").as_posix(),
                "FLEET_OUT": (gate / "out").as_posix(),
                "PATH": shim.parent.as_posix() + os.pathsep + (path or os.environ.get("PATH", ""))}

    def run_gate(self, tmp: Path, name: str, *command: str, path: str | None = None) -> str:
        """release-gate.sh COMMAND from the test's checkout in gate_env's environment; the output."""
        done = subprocess.run([shutil.which("bash"), "scripts/release-gate.sh", *command], cwd=self.repo,
                              env=self.gate_env(tmp, name, path), capture_output=True, text=True)
        return done.stdout + done.stderr

    def test_the_build_compiles_the_commit_checked(self):
        """PR #54 fix check 1 (D2's other half): a replace ref made after the check returned stood
        another engine in for the checked commit's in the checkout the build compiles, and HEAD
        and a clean status didn't show it. release-gate.sh build, from the check to the checkout,
        reads the objects as stored: with no replace ref (the control) and with one made right
        after the check, its worktree holds the commit checked."""
        bash = shutil.which("bash")
        if bash is None or shutil.which("flock") is None:
            self.skipTest("the gate builds on Linux hosts (bash, flock)")
        loop = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        engine = self.commit({self.ENGINE: "pub fn engine() { faster() }\n"}, "another engine", self.side)
        tmp = self.gate_beside(loop)
        # The check as it is; right after it (INJECT), a replace ref standing another engine in.
        (tmp / "shim").mkdir()
        (tmp / "shim" / "python3").write_text(f"""#!/usr/bin/env bash
out=$("{Path(sys.executable).as_posix()}" "$@"); code=$?
case " $* " in *" benches/fast/release_gate.py measured-at "*) [ -z "${{INJECT:-}}" ] || git replace $INJECT ;; esac
printf '%s\\n' "$out"; exit $code
""", encoding="utf-8", newline="\n")
        (tmp / "shim" / "python3").chmod(0o755)
        tree = lambda commit: self.git("rev-parse", f"{commit}^{{tree}}")
        got = {}
        for name, inject in (("no replace ref", ""), ("one made after the check", f"{tree(loop)} {tree(engine)}")):
            gate = tmp / name.replace(" ", "-")
            env = {**os.environ, "NRESE_GATE_DIR": (gate / "gate").as_posix(), "GATE_BIN": (gate / "bin").as_posix(),
                   "FLEET_OUT": (gate / "out").as_posix(), "INJECT": inject,
                   "PATH": (tmp / "shim").as_posix() + os.pathsep + os.environ.get("PATH", "")}
            done = subprocess.run([bash, "scripts/release-gate.sh", "build", self.side], cwd=self.repo, env=env,
                                  capture_output=True, text=True)
            src = self.space(gate / "gate") / "build" / f"src-{loop[:8]}"
            got[name] = tuple((src / p).read_text(encoding="utf-8") if (src / p).exists() else done.stdout + done.stderr
                              for p in (self.ENGINE, self.PERF_LAB))
            if inject:
                self.git("replace", "-d", tree(loop))
        self.assertEqual(got, {name: ("pub fn engine() {}\n", "fn main() { arm(); }\n") for name in got},
                         "SPEC measurement side: the gate compiles the commit it checked")

    def test_a_message_in_another_encoding_is_no_error(self):
        """Sol, PR #54 R1 fix review r3 D4: the commit object was decoded in the locale's encoding, so
        a measurement commit whose message is ISO-8859-1 stopped the check with UnicodeDecodeError."""
        message = Path(tempfile.mkdtemp(prefix="measurement-message-"), "message")
        self.addCleanup(shutil.rmtree, message.parent, True)
        message.write_bytes(b"its perf lab, caf\xe9\n")
        self.git("checkout", "-q", "--detach", self.side)
        Path(self.repo, self.PERF_LAB).write_text("fn main() { arm(); }\n", encoding="utf-8")
        self.git("add", self.PERF_LAB)
        self.git("-c", "user.name=t", "-c", "user.email=t@example.org", "-c", "i18n.commitEncoding=ISO-8859-1",
                 "commit", "-q", "-F", str(message))
        commit = self.git("rev-parse", "HEAD")
        self.assertEqual(gate_module.measured_at(self.repo, self.side, {self.side: commit}), commit)

    def test_a_graft_or_a_shallow_boundary_moves_no_parent(self):
        """Sol, PR #54 R1 fix review D3: the parent query walked history, so a graft or a shallow
        boundary refused a valid measurement commit; its parent is the one its object names."""
        commit = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        git_dir = Path(self.git("rev-parse", "--absolute-git-dir"))
        for name, text in (("info/grafts", f"{commit} {self.later}\n"), ("shallow", f"{commit}\n")):
            with self.subTest(name):
                (git_dir / name).parent.mkdir(exist_ok=True)
                (git_dir / name).write_text(text, encoding="utf-8")
                try:
                    self.assertEqual(gate_module.measured_at(self.repo, self.side, {self.side: commit}), commit)
                finally:
                    (git_dir / name).unlink()

    def test_a_side_without_a_measurement_commit_is_built_from_itself(self):
        self.assertEqual(gate_module.measured_at(self.repo, self.later, {}), self.later)

    def test_a_measurement_commit_is_built_only_as_its_side(self):
        """Sol r1 S1: built as a side of its own, it would share its side's build directory under
        another REVISION, and the second of the two builds would be refused."""
        commit = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        with self.assertRaisesRegex(SystemExit, f"is the measurement commit of {self.side[:12]}"):
            gate_module.measured_at(self.repo, commit, {self.side: commit})

    def test_a_job_that_builds_a_side_takes_its_measurement_commit_as_an_input(self):
        commit = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        inputs = gate_module.measurement_inputs(self.repo, [self.side, self.later, self.side],
                                                {self.side: commit})
        self.assertEqual(inputs, [{"name": f"measure-{self.side[:8]}", "source": {"git": self.repo, "rev": commit}}])

    def test_a_job_takes_no_measurement_commit_its_checkout_already_reaches(self):
        """Master, 10 Nov (IB0343-654a): the fleet stages a job's checkout with its history and takes one git
        input (F150), so a measurement commit the checkout reaches is no input of its own; one it doesn't
        reach still is. Both checkouts hold the side itself, so neither takes it as an input."""
        commit = self.commit({self.PERF_LAB: "fn main() { arm(); }\n"}, "its perf lab", self.side)
        holding = self.commit({"README.md": "holds it\n"}, "a checkout that reaches it", commit)
        kept = [{"name": f"measure-{self.side[:8]}", "source": {"git": self.repo, "rev": commit}}]
        with patch.object(gate_module, "measurement_sides", return_value={self.side: commit}):
            self.assertEqual(gate_module.side_inputs(self.repo, holding, [self.side]), [])
            self.assertEqual(gate_module.side_inputs(self.repo, self.later, [self.side]), kept)

    def test_main_and_v1_5_are_measured_at_their_commits(self):
        table = gate_module.tomllib.loads(gate_module.MEASUREMENT_SIDES.read_text(encoding="utf-8"))["side"]
        self.assertTrue({gate_module.MAIN, gate_module.BASE} <= set(table))
        for rev, side in table.items():
            with self.subTest(side=rev):
                self.assertRegex(rev, r"^[0-9a-f]{40}$")
                self.assertRegex(side["commit"], r"^[0-9a-f]{40}$")
                self.assertTrue(re.fullmatch(rf"refs/measure/[a-z0-9]+-{rev[:8]}", side["ref"]), side["ref"])


class PatchKeyedBuilds(unittest.TestCase):
    """The builds of a perf lab patched at build time (before T0) can't be reused after it: their
    identity names the patch. `clean --patch-keyed` lists them, and with --remove their binaries go
    unless a gate used them within GATE_LIVE_HOURS; their IDENTITY, BUILD.txt and SHA256SUMS stay."""

    def test_only_an_unused_patch_keyed_build_loses_its_binaries_and_only_when_asked(self):
        bash = shutil.which("bash")
        if not bash:
            self.skipTest("needs bash")
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp).as_posix()
            setup = f'''
make() {{  # NAME IDENTITY-LINE REF-AGE
  b={t}/bin/$1; mkdir -p "$b/release/examples" "$b/.refs"; echo tool > "$b/release/examples/perf_lab"
  printf 'revision x\\n%s\\nlayout 1\\n' "$2" > "$b/IDENTITY"; echo built > "$b/BUILD.txt"
  (cd "$b" && sha256sum release/examples/perf_lab > SHA256SUMS); touch -d "$3" "$b/.refs/gate-$1"
}}
make bin-unused 'patch perf_lab_watchdog.py 0123456789abcdef' '3 days ago'
make bin-live 'patch perf_lab_watchdog.py 0123456789abcdef' 'now'
make bin-after 'profile release --locked' '3 days ago'
'''
            env = {**os.environ, "NRESE_GATE_DIR": f"{t}/gate", "GATE_BIN": f"{t}/bin", "FLEET_OUT": f"{t}/out"}
            subprocess.run([bash, "-c", setup], env=env, check=True, capture_output=True)

            def clean(*args: str) -> dict[str, str]:
                done = subprocess.run([bash, "scripts/release-gate.sh", "clean", "--patch-keyed", *args], cwd=ROOT,
                                      env=env, capture_output=True, text=True, encoding="utf-8")
                self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
                return {line.split("\t")[1]: line.split("\t")[0] for line in done.stdout.splitlines()}

            def binaries() -> set[str]:
                return {d.name for d in Path(tmp, "bin").iterdir() if (d / "release").is_dir()}

            self.assertEqual(clean(), {"bin-unused": "would-release", "bin-live": "kept-live"})
            self.assertEqual(binaries(), {"bin-unused", "bin-live", "bin-after"}, "SPEC: a dry run removes nothing")
            self.assertEqual(clean("--remove"), {"bin-unused": "released-now", "bin-live": "kept-live"})
            self.assertEqual(binaries(), {"bin-live", "bin-after"})
            self.assertEqual({p.name for p in Path(tmp, "bin", "bin-unused").iterdir() if p.is_file()},
                             {"IDENTITY", "BUILD.txt", "SHA256SUMS"}, "SPEC: the record stays")
            self.assertEqual(clean(), {"bin-unused": "released", "bin-live": "kept-live"})
            self.assertTrue(Path(tmp, "out", "patch-keyed-builds.tsv").is_file())
            # The gate's own checkout (of this checkout's commit), if it made one, is under tmp.
            self.assertEqual(remove_checkouts_under(tmp), [], "a checkout this test made stayed registered")

    def test_a_patch_keyed_build_is_never_a_verified_build(self):
        """Sol r1 S2: a gate's pointer or an archive from before T0 could hand a block a build of
        patched source; build_verified, which block, reuse and pack go through, refuses it."""
        bash = shutil.which("bash")
        if not bash:
            self.skipTest("needs bash")
        with tempfile.TemporaryDirectory() as tmp:
            t = Path(tmp).as_posix()
            script = f'''
. scripts/lib/gate-publish.sh
for kind in patched plain; do
  b={t}/bin-$kind; mkdir -p "$b/release"; echo tool > "$b/release/tool"; echo r > "$b/REVISION"
  (cd "$b" && sha256sum release/tool > SHA256SUMS)
  [ $kind = patched ] && echo "patch perf_lab_watchdog.py 0123456789abcdef" > "$b/IDENTITY" || echo "layout 1" > "$b/IDENTITY"
  build_verified "$b" r && echo "$kind verified" || echo "$kind refused"
done
'''
            done = subprocess.run([bash, "-c", script], cwd=ROOT, capture_output=True, text=True, encoding="utf-8")
        self.assertEqual(done.stdout.split("\n")[:2], ["patched refused", "plain verified"], done.stderr)


if __name__ == "__main__":
    unittest.main()
