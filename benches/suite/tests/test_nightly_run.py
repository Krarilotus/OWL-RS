"""The nightly run (scripts/check.sh nightly; scripts/lib/fleet-gate.sh, mode nightly): one commit with
every test in its full form (NRESE_CHECK_TIER=nightly), on the fleet only, and a receipt of one outcome
per test of the tree's inventory. Its pass is its own tier's, by tree, and it lands nothing.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_nightly_run.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

import isolation
import test_push_gate as fixture  # the push gate's scratch repository and fleet stand-in (its tests aren't rerun here)
import test_quick_check as quick  # the build host's stand-ins (its tests aren't rerun here)

ROOT = fixture.ROOT
BASH = fixture.BASH
ZERO = fixture.ZERO


@unittest.skipUnless(BASH, "the gate is a bash script")
class NightlyOnTheFleet(fixture.Repo):
    """check.sh nightly by hand, with the push gate's stand-in fleet."""

    def nightly(self, *commit, **env):
        return subprocess.run([BASH, "scripts/check.sh", "nightly", *commit], cwd=self.repo, capture_output=True,
                              text=True, env={**self.env, **env}, timeout=120, stdin=subprocess.DEVNULL)

    def submitted(self):
        return [quick.step_args(path.read_text()) for path in sorted(self.fake.glob("job-*.toml"))]

    def passes(self):
        passed = Path(self.git(self.repo, "rev-parse", "--absolute-git-dir")) / "nrese-gate-passed"
        return passed.read_text().splitlines() if passed.exists() else []

    def test_it_is_a_gate_job_of_the_whole_commit_and_never_a_merge_gate(self):
        # Spec: kind owlrs.gate, as a push gate's,
        # the commit's tree with no file list (the whole suite, unselected); it lands nothing, so
        # no merge-gate class whatever ref the caller names.
        # The fleet places it, as it places a push gate: no host is named unless NRESE_GATE_HOSTS names
        # some. Its follower waits a start window and the step deadline
        # before it gives up, so a run started late in its window ends by its own deadline, never cut
        # first.
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("blocked")  # ends the follow at once, the start window unspent
        done = self.nightly(NRESE_PUSH_REF="refs/heads/refactor/engine-v2", NRESE_GATE_START_S="600")
        self.assertEqual(self.submitted(), [("nightly", self.tree(tip), [])], "SPEC nightly-whole")
        job = (self.fake / "job-0.toml").read_text()
        self.assertIn('kind = "owlrs.gate"', job, "SPEC nightly-job")
        self.assertNotIn("priority_class", job, "SPEC nightly-job: a merge gate")
        self.assertNotRegex(job, r"(?m)^hosts *=", "SPEC nightly-host: a host pinned")
        deadline = int(re.search(r"^step_deadline_min = (\d+)$", job, re.M).group(1))
        start, limit = map(int, re.search(r"a start within (\d+) s, a verdict within (\d+) s", done.stderr).groups())
        self.assertEqual(start, 600, done.stderr)
        self.assertGreaterEqual(limit, start + deadline * 60, f"SPEC nightly-limit: {done.stderr}")
        self.nightly(NRESE_GATE_HOSTS="test-host-1")
        self.assertIn('hosts = ["test-host-1"]', (self.fake / "job-1.toml").read_text(), "SPEC nightly-host")

    def test_no_selection_reaches_it(self):
        # Spec (22 Nov: the suite contracts' selection by what each reads is the push tier's, the quick
        # check's and the Windows part's only): with the selector and declared contracts on the line, and a
        # commit whose range reaches only one of them, the nightly job is still sent with no list.
        tests = self.repo / "benches/suite/tests"
        tests.mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / "benches/suite/tests/contracts.py", tests / "contracts.py")
        (tests / "test_a.py").write_text('READS = ("a.txt",)\n', newline="\n")
        (tests / "test_b.py").write_text('READS = ("b.txt",)\n', newline="\n")
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-qm", "the contracts")
        self.git(self.repo, "push", "-q", "origin", "HEAD:refs/heads/refactor/engine-v2")
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("blocked")  # ends the follow at once
        done = self.nightly()
        self.assertEqual(self.submitted(), [("nightly", self.tree(tip), [])],
                         f"SPEC nightly-whole: the nightly job was sent a selection\n{done.stderr}")
        self.assertNotIn("NRESE_CONTRACTS", (self.fake / "job-0.toml").read_text(), "SPEC nightly-whole")

    def test_its_pass_is_its_own_tiers_and_never_opens_a_push(self):
        # Spec (a release never rests on another commit's
        # nightly result): a verified nightly pass is recorded under tier nightly, naming the commit; the
        # same tree then passes at once; a push of that commit is still gated on its own; another commit's
        # tree runs its own nightly job.
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("done", self.tree(tip), [], tier="nightly")
        done = self.nightly()
        self.assertEqual(done.returncode, 0, f"SPEC nightly-verified: {done.stderr}")
        self.assertRegex(self.passes()[0], rf"^nightly {self.tree(tip)} [0-9a-f]{{40}} \S+ flags=\S* tools=\S+ fleet"
                                           rf" \| {tip} fleet:gate-1@", "SPEC nightly-record")
        again = self.nightly()
        self.assertEqual((again.returncode, len(self.submitted())), (0, 1), f"SPEC nightly-verified: {again.stderr}")
        self.fleet("pending")
        pushed = self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", ZERO))
        self.assertEqual((pushed.returncode != 0, [m for m, _, _ in self.submitted()]), (True, ["nightly", "push"]),
                         f"SPEC nightly-not-push: a nightly pass let a push through\n{pushed.stderr}")
        other = self.commit("b.txt", "b changed\n")
        self.nightly()
        self.assertEqual(self.submitted()[-1][:2], ("nightly", self.tree(other)),
                         "SPEC nightly-by-tree: another commit's nightly pass stood for this one")

    def test_it_runs_on_the_fleet_only(self):
        # Spec (heavy work runs on the fleet only): no fleet, or no verdict from it, is
        # exit 75 and no pass; a failed verdict is exit 1; nothing runs here instead.
        tip = self.commit("a.txt", "a changed\n")
        owner = self.env.pop("FLEET_OWNER")
        unowned = self.nightly()
        self.env["FLEET_OWNER"] = owner
        self.fleet("pending")
        waited = self.nightly()
        self.fleet("failed", self.tree(tip), [], tier="nightly")
        result = self.fake / "results" / "out" / "gate.json"
        record = json.loads(result.read_text())
        record.update(code=1, steps=[{"name": "test (full forms)", "code": 100, "seconds": 1}])
        result.write_text(json.dumps(record))
        failed = self.nightly()
        for done, want in ((unowned, 75), (waited, 75), (failed, 1)):
            self.assertEqual(done.returncode, want, f"SPEC nightly-fleet-only: {done.stdout}{done.stderr}")
            self.assertNotIn("=== test", done.stdout, "SPEC nightly-fleet-only: a step ran here")
        self.assertEqual(self.passes(), [], "SPEC nightly-fleet-only")

    def test_a_commit_without_the_tier_is_refused_before_any_job(self):
        # Spec: the job runs the commit's own check.sh; one without the tier would end in a usage error
        # on the host. Such a commit is refused: exit 2, nothing submitted; HEAD, which has it, isn't.
        current = (self.repo / "scripts/check.sh").read_text()
        old = self.commit("scripts/check.sh", "#!/usr/bin/env bash\n# a gate from before the nightly tier\n")
        self.commit("scripts/check.sh", current)
        self.fleet("pending")
        named = self.nightly(old)
        self.assertEqual((named.returncode, "has no nightly tier" in named.stderr, self.submitted()), (2, True, []),
                         f"SPEC nightly-runner: {named.stdout}{named.stderr}")
        head = self.nightly()
        self.assertEqual((head.returncode, len(self.submitted())), (75, 1), f"SPEC nightly-runner: {head.stderr}")


# Stand-ins for cargo (the workspace of quick's tests, its builds in $TARGET) and the build admission
# running nextest: each call recorded with the tier and profile it sees; `nextest list` prints
# $FAKE/inventory.json; a run in a profile writes its JUnit report where nextest does, in the workspace's
# target/nextest/<profile>, whatever the target directory: $FAKE/count.xml for the ignored test, else
# $FAKE/full.xml. A call holding FAIL fails; one holding EMPTY selects no test (4 with --no-tests=fail).
CARGO = """#!/bin/sh
case "$1" in
  metadata) echo '{"packages":[{"name":"alpha"},{"name":"beta"},{"name":"gamma"},{"name":"nrese-server"}],'\
'"target_directory":"'"$TARGET"'","workspace_root":"'"$PWD"'"}' ;;
  nextest) exit 0 ;;
esac
"""
GUARDED = """#!/bin/sh
echo "guarded${NRESE_CHECK_TIER:+ tier=$NRESE_CHECK_TIER}${NEXTEST_PROFILE:+ profile=$NEXTEST_PROFILE}\
${NRESE_W3C_REQUIRED:+ w3c-required} $*" >> "$CALLS"
case "$*" in *--no-tests=fail*) case "$*" in *"${EMPTY:-no empty selection}"*) exit 4 ;; esac ;; esac
case "$*" in
  "nextest list"*) cat "$FAKE/inventory.json" ;;
  "nextest run"*) report=full; case "$*" in *--run-ignored*) report=count ;; esac
    if [ -n "${NEXTEST_PROFILE:-}" ]; then
      mkdir -p "target/nextest/$NEXTEST_PROFILE" && cp "$FAKE/$report.xml" "target/nextest/$NEXTEST_PROFILE/junit.xml"
    fi ;;
esac
case "$*" in *"${FAIL:-no failure planted}"*) exit 1 ;; esac
exit 0
"""
COUNT = "equality_answer_tests::a_count_past_64_bits_fails_as_a_range_limit"
# The stand-in workspace's inventory, worked by hand: two tests not #[ignore]d, an #[ignore]d one no step
# runs, and the count test; and each step's report, every test it selects passing once (nextest's report
# leaves out the tests a run doesn't select).
INVENTORY = {"alpha": {"t1": False, "slow": True}, "beta::it": {"x::t2": False}, "nrese-sparql::it": {COUNT: True}}
FULL = {"alpha": ["t1"], "beta::it": ["x::t2"]}
COUNTED = {"nrese-sparql::it": [COUNT]}


def junit(suites, failed=(), skipped=()):
    """A JUnit report of nextest's shape: a testsuite per binary id, a testcase per test it reports."""
    def mark(name):
        return "<failure/>" if name in failed else '<skipped message="ignored"/>' if name in skipped else ""
    cases = "".join(f'<testsuite name="{binary}">' + "".join(
        f'<testcase name="{name}" classname="{binary}">{mark(name)}</testcase>'
        for name in names) + "</testsuite>" for binary, names in suites.items())
    return f'<?xml version="1.0"?><testsuites name="nextest-run">{cases}</testsuites>'


@unittest.skipUnless(BASH, "the gate is a bash script")
class NightlyTier(unittest.TestCase):
    """check.sh nightly as its fleet job runs it, over quick's stand-in workspace (alpha, beta, gamma,
    nrese-server)."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="nrese-nightly-tier-")
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        (root / "bin").mkdir()
        (root / "bin/cargo").write_text(CARGO, newline="\n")
        (root / "bin/cargo").chmod(0o755)
        self.calls, self.fake, self.out, self.repo = root / "calls", root / "fake", root / "out", root / "repo"
        self.fake.mkdir()
        for rel, text in {**quick.STUBS, "scripts/cargo-guarded.sh": GUARDED}.items():
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / rel).write_text(text, newline="\n")
        (self.repo / "scripts/cargo-guarded.sh").chmod(0o755)
        for rel in ("scripts/check.sh", "scripts/lib/nightly-receipt.py", "scripts/lib/gate-key.sh"):
            shutil.copy(ROOT / rel, self.repo / rel)
        self.env = isolation.inherit(*fixture.LEAKS, PATH=(root / "bin").as_posix() + os.pathsep + os.environ["PATH"],
                                     NRESE_ON_BUILD_HOST="1", CALLS=self.calls.as_posix(), FAKE=self.fake.as_posix(),
                                     TARGET=(root / "target").as_posix(), FLEET_OUT=self.out.as_posix(),
                                     FLEET_JOB="gate-1", FLEET_HOST="test-host-1")
        for args in (["init", "-q"], ["add", "-A"], ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "base"]):
            subprocess.run(["git", "-C", str(self.repo), *args], check=True, capture_output=True, env=self.env)
        (self.out / "attempt").parent.mkdir()
        (self.out / "attempt").write_text("gate-1:0:1:1\n")
        self.given(INVENTORY, FULL, COUNTED)

    def given(self, inventory, full, counted, failed=(), skipped=()):
        """The inventory nextest lists and the report each step's run writes."""
        suites = {b: {"testcases": {n: {"ignored": i, "filter-match": {"status": "matches"}} for n, i in cases.items()}}
                  for b, cases in inventory.items()}
        (self.fake / "inventory.json").write_text(json.dumps({"test-count": 4, "rust-suites": suites}))
        (self.fake / "full.xml").write_text(junit(full, failed, skipped))
        (self.fake / "count.xml").write_text(junit(counted))

    def tier(self, **env):
        done = subprocess.run([BASH, "scripts/check.sh", "nightly"], cwd=self.repo, capture_output=True, text=True,
                              timeout=120, stdin=subprocess.DEVNULL, env={**self.env, **env})
        steps = {name: int(code) for code, name in re.findall(r"(?m)^ +(\d+)  (.+) \(\d+ s\)$", done.stdout)}
        calls = [c for c in self.calls.read_text().splitlines() if c.startswith("guarded ")] \
            if self.calls.exists() else []
        self.calls.unlink(missing_ok=True)
        return done.returncode, steps, calls, done.stdout + done.stderr

    def receipt(self):
        return json.loads((self.out / "receipt/receipt.json").read_text())

    def test_every_test_runs_once_in_its_full_form_and_the_receipt_holds_each(self):
        # Spec, worked by hand
        # for the stand-in workspace: the W3C data fetched and required; the inventory listed first, whole
        # (no default filter, ignored tests included); every crate's tests not ignored in one run, and then
        # the count test alone among the ignored, each in the nightly profile and tier; the receipt holds one
        # outcome per inventoried test, the ignored one no step runs skipped with its reason, and the gate's
        # receipt fields. Nothing else built, linted or tested.
        code, steps, calls, out = self.tier()
        crates = "-p alpha -p beta -p gamma -p nrese-server"
        self.assertEqual((code, calls), (0, [
            f"guarded w3c-required nextest list --locked --ignore-default-filter --run-ignored all --message-format json {crates}",
            f"guarded tier=nightly profile=nightly w3c-required nextest run --locked --no-fail-fast {crates}",
            "guarded tier=nightly profile=nightly w3c-required nextest run --locked --no-fail-fast --no-tests=fail"
            f" --run-ignored only -p nrese-sparql -E binary_id(nrese-sparql::it) & test(={COUNT})"]),
            f"SPEC nightly-full-forms\n{out}")
        self.assertEqual(steps, dict.fromkeys(["W3C data", "inventory", "test (full forms)", "count past 64 bits",
                                               "receipt"], 0), f"SPEC nightly-steps\n{out}")
        receipt = self.receipt()
        self.assertEqual([(c["id"], c["outcome"], c.get("reason")) for c in receipt["cases"]], [
            ("alpha slow", "skipped", "#[ignore]d: no step of the tier runs it"), ("alpha t1", "passed", None),
            ("beta::it x::t2", "passed", None), (f"nrese-sparql::it {COUNT}", "passed", None)], "SPEC receipt-outcomes")
        head = subprocess.run(["git", "-C", str(self.repo), "rev-parse", "HEAD", "HEAD^{tree}"], capture_output=True,
                              text=True, check=True).stdout.split()
        self.assertEqual({k: receipt[k] for k in ("kind", "commit", "tree", "dirty", "host", "job", "attempt", "complete")},
                         {"kind": "nightly", "commit": head[0], "tree": head[1], "dirty": False, "host": "test-host-1",
                          "job": "gate-1", "attempt": "gate-1:0:1:1", "complete": True}, "SPEC receipt-fields")

    def test_a_receipt_short_of_its_inventory_is_not_green(self):
        # Spec: not green when a test a step selected has no outcome (one
        # withheld from the run), a test has two, an outcome names a test the inventory doesn't list, or the
        # count step's test isn't in the inventory. Each fails the receipt step and the tier, the tests' own
        # steps passing; the control above is green.
        withheld = {"alpha": ["t1"], "beta::it": []}
        twice = {"alpha": ["t1", "t1"], "beta::it": ["x::t2"]}
        unknown = {**FULL, "gamma": ["t3"]}
        no_count = {k: v for k, v in INVENTORY.items() if k != "nrese-sparql::it"}
        for name, inventory, full, said in (("withheld", INVENTORY, withheld, "full-forms: no outcome for beta::it x::t2"),
                                            ("twice", INVENTORY, twice, "full-forms: 2 outcomes for alpha t1"),
                                            ("unknown", INVENTORY, unknown, "gamma t3, which the inventory doesn't list"),
                                            ("no count", no_count, FULL, f"selects nrese-sparql::it {COUNT}, which the")):
            with self.subTest(name):
                self.given(inventory, full, COUNTED)
                code, steps, _, out = self.tier()
                self.assertEqual((code, steps.get("receipt"), steps.get("test (full forms)")), (1, 1, 0),
                                 f"SPEC receipt-not-green\n{out}")
                self.assertIn(said, out, "SPEC receipt-not-green: its reason")
                self.assertFalse(self.receipt()["complete"], "SPEC receipt-not-green")

    def test_a_selected_test_reported_skipped_is_no_outcome(self):
        # Spec: a test a step selected needs a run outcome, passed or failed. Its runner
        # exiting 0 while reporting it skipped leaves it unrun: the receipt step and the tier fail, the test's
        # own step passing. An #[ignore]d test a step reports skipped without selecting it is no outcome of
        # that step: the receipt stays complete, that test skipped as the tier's own outcome.
        reported = {"alpha": ["t1", "slow"], "beta::it": ["x::t2"], "nrese-sparql::it": [COUNT]}
        self.given(INVENTORY, reported, COUNTED, skipped=("slow", COUNT))
        code, steps, _, out = self.tier()
        outcomes = {c["id"]: (c["step"], c["outcome"]) for c in self.receipt()["cases"]}
        self.assertEqual((code, outcomes["alpha slow"], outcomes[f"nrese-sparql::it {COUNT}"]),
                         (0, (None, "skipped"), ("count", "passed")), f"SPEC receipt-skipped-unselected\n{out}")
        self.given(INVENTORY, reported, COUNTED, skipped=("slow", COUNT, "x::t2"))
        code, steps, _, out = self.tier()
        self.assertEqual((code, steps.get("test (full forms)"), steps.get("receipt")), (1, 0, 1),
                         f"SPEC receipt-skipped-selected\n{out}")
        self.assertIn("full-forms: beta::it x::t2 was selected, but reported skipped: it never ran", out,
                      "SPEC receipt-skipped-selected: its reason")
        self.assertFalse(self.receipt()["complete"], "SPEC receipt-skipped-selected")

    def test_a_report_that_cannot_be_removed_fails_its_step(self):
        # Spec: each step's report is removed before it runs, so an earlier one never stands for
        # it; where it can't be removed, the step fails before running and the tier fails.
        (self.repo / "target/nextest/nightly/junit.xml").mkdir(parents=True)
        (self.repo / "target/nextest/nightly/junit.xml/held").write_text("an earlier report's place\n")
        code, steps, calls, out = self.tier()
        self.assertEqual((code, steps.get("test (full forms)", 0) != 0, steps.get("receipt")), (1, True, 1),
                         f"SPEC nightly-report-removed\n{out}")
        self.assertIn("full-forms: the earlier report", out, "SPEC nightly-report-removed: its reason")
        self.assertFalse(any("nextest run" in c for c in calls), f"SPEC nightly-report-removed: a run went ahead\n{out}")

    def test_a_count_step_that_selects_no_test_fails(self):
        # Spec (a filter that matches nothing never passes): the count step selecting
        # no test fails its step and the tier.
        code, steps, _, out = self.tier(EMPTY="a_count_past")
        self.assertEqual((code, steps.get("count past 64 bits", 0) != 0), (1, True), f"SPEC nightly-count\n{out}")

    def test_a_failing_test_fails_the_tier_and_is_its_outcome(self):
        # Spec: a failed night is the tree's failure: a test that fails fails its step and
        # the tier, and is that test's outcome in a complete receipt.
        self.given(INVENTORY, FULL, COUNTED, failed=("x::t2",))
        code, steps, _, out = self.tier(FAIL="--no-fail-fast -p alpha")
        outcomes = {c["id"]: c["outcome"] for c in self.receipt()["cases"]}
        self.assertEqual((code, steps.get("test (full forms)"), steps.get("receipt"), outcomes["beta::it x::t2"]),
                         (1, 1, 0, "failed"), f"SPEC nightly-fails\n{out}")


if __name__ == "__main__":
    unittest.main()
