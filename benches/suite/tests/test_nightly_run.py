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
        """check.sh nightly; the outside check's job left out (NRESE_GATE_NO_OUTSIDE) unless ENV says."""
        return subprocess.run([BASH, "scripts/check.sh", "nightly", *commit], cwd=self.repo, capture_output=True,
                              text=True, env={**self.env, "NRESE_GATE_NO_OUTSIDE": "1", **env}, timeout=120,
                              stdin=subprocess.DEVNULL)

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

    def test_the_outside_check_is_a_job_of_its_own_sent_first(self):
        # Spec (scripts/lib/fleet-gate.sh, nightly): the outside check goes to the fleet before the nightly
        # run, as a job of its own for the same commit: the commit's own check.sh nightly-outside, run as
        # on a build host, on Linux, needing Docker and image builds, on the private overlay's
        # gate_outside hosts, never followed. Its step (scripts/lib/outside-job.sh) keeps check.sh's
        # output as check.log and a gate job's record as gate.json, tier nightly-outside, the tree, its
        # attempt and check.sh's steps and exit, which is the step's (the fleet keeps a job's files, not
        # its step's output). The nightly run's job is sent after it exactly as without it, its id
        # the outside job's with the tier in place of nightly-outside, and the last job the run names as
        # submitted (what a launcher reads for the nightly run's id).
        tip = self.commit("a.txt", "a changed\n")
        lists = Path(self.env["NRESE_BENCH_PRIVATE"]) / "hosts.toml"
        lists.write_text(lists.read_text() + 'gate_outside = ["docker-host", "docker-host-2"]\n', newline="\n")
        self.fleet("blocked")  # ends the nightly run's follow at once
        done = self.nightly(NRESE_GATE_NO_OUTSIDE="")
        jobs = [path.read_text() for path in sorted(self.fake.glob("job-*.toml"))]
        self.assertEqual(len(jobs), 2, f"SPEC outside-job\n{done.stderr}")
        outside, night = jobs
        for line in ('kind = "owlrs.any"', 'NRESE_ON_BUILD_HOST = "1"', 'os = "linux"',
                     'has = ["docker", "docker-build"]', 'hosts = ["docker-host", "docker-host-2"]', f'rev = "{tip}"'):
            self.assertIn(line, outside, "SPEC outside-job")
        step = json.loads(re.search(r"^steps = (.*)$", outside, re.M).group(1))[0]
        # The step, run over a stand-in check.sh that prints its argument and a summary, and exits 7.
        with tempfile.TemporaryDirectory(prefix="nrese-outside-step-") as tmp:
            (Path(tmp) / "scripts/lib").mkdir(parents=True)
            for name in ("scripts/lib/outside-job.sh", "scripts/lib/gate-key.sh", "scripts/lib/gate-record.py"):
                shutil.copy(ROOT / name, Path(tmp) / name)
            (Path(tmp) / "scripts/check.sh").write_text(
                'printf "ran %s\\n=== summary\\n  1  outside counts (2 s)\\n" "$1"\nexit 7\n', newline="\n")
            out = Path(tmp) / "out"
            out.mkdir()
            ran = subprocess.run([BASH, *step[1:]], cwd=tmp, capture_output=True, text=True, timeout=60,
                                 env={**self.env, "FLEET_OUT": out.as_posix(), "FLEET_JOB": "outside-1"})
            self.assertEqual((ran.returncode, (out / "check.log").read_text().splitlines()[0]),
                             (7, "ran nightly-outside"), f"SPEC outside-job: its step {step}\n{ran.stderr}")
            record = json.loads((out / "gate.json").read_text())
            self.assertEqual({k: record[k] for k in ("tier", "tree", "job", "attempt", "summary", "steps", "code")},
                             {"tier": "nightly-outside", "tree": self.tree(tip), "job": "outside-1",
                              "attempt": (out / "attempt").read_text().strip(), "summary": True,
                              "steps": [{"name": "outside counts", "code": 1, "seconds": 2}], "code": 7},
                             "SPEC outside-record")
            # A green check whose record can't be written is no green job: exit 3.
            (Path(tmp) / "scripts/check.sh").write_text('echo "=== summary"\nexit 0\n', newline="\n")
            (Path(tmp) / "scripts/lib/gate-record.py").write_text("raise SystemExit(1)\n", newline="\n")
            ran = subprocess.run([BASH, *step[1:]], cwd=tmp, capture_output=True, text=True, timeout=60,
                                 env={**self.env, "FLEET_OUT": out.as_posix(), "FLEET_JOB": "outside-1"})
            self.assertEqual(ran.returncode, 3, f"SPEC outside-record: unwritten\n{ran.stderr}")
        self.assertEqual(quick.step_args(night), ("nightly", self.tree(tip), []), "SPEC outside-beside")
        ids = [re.search(r'^id = "(.*)"$', job, re.M).group(1) for job in jobs]
        self.assertEqual(ids[0], ids[1].replace("gate-nightly-", "gate-nightly-outside-", 1), "SPEC outside-id")
        said = re.findall(r"(?m)^fleet gate \S+: submitted(.*)$", done.stderr)
        self.assertEqual([s.split(" ", 2)[1] for s in said], ["the", "(priority"], f"SPEC outside-said\n{done.stderr}")

    def test_without_hosts_for_it_the_outside_check_is_not_sent_and_the_run_says_so(self):
        # Spec (scripts/lib/fleet-gate.sh, nightly): with no gate_outside list in the private overlay and
        # no NRESE_GATE_OUTSIDE_HOSTS, the outside check isn't submitted, the run says so, and the
        # nightly run's job goes out as ever; NRESE_GATE_OUTSIDE_HOSTS alone sends it, to those hosts.
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("blocked")
        done = self.nightly(NRESE_GATE_NO_OUTSIDE="")
        jobs = [path.read_text() for path in sorted(self.fake.glob("job-*.toml"))]
        self.assertEqual([quick.step_args(job) for job in jobs], [("nightly", self.tree(tip), [])],
                         f"SPEC outside-no-hosts\n{done.stderr}")
        self.assertIn("the outside check wasn't submitted: no hosts for it", done.stderr, "SPEC outside-no-hosts")
        for path in self.fake.glob("job-*.toml"):
            path.unlink()
        self.nightly(NRESE_GATE_NO_OUTSIDE="", NRESE_GATE_OUTSIDE_HOSTS="docker-host-3")
        jobs = [path.read_text() for path in sorted(self.fake.glob("job-*.toml"))]
        self.assertIn('hosts = ["docker-host-3"]', jobs[0], "SPEC outside-no-hosts: the override")

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
# target/nextest/<profile>, whatever the target directory: $FAKE/count.xml for the ignored test,
# $FAKE/known.xml for a known open fault's run (its filter names a `known::` test), else $FAKE/full.xml.
# A call holding FAIL fails; one holding FAILED_TESTS exits as nextest does when tests failed (100); one
# holding EMPTY selects no test (4 with --no-tests=fail).
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
    case "$*" in *known::*) report=known ;; esac
    if [ -n "${NEXTEST_PROFILE:-}" ]; then
      mkdir -p "target/nextest/$NEXTEST_PROFILE" && cp "$FAKE/$report.xml" "target/nextest/$NEXTEST_PROFILE/junit.xml"
    fi ;;
esac
case "$*" in *"${FAIL:-no failure planted}"*) exit 1 ;; esac
case "$*" in *"${FAILED_TESTS:-no failure planted}"*) exit 100 ;; esac
exit 0
"""
COUNT = "equality_answer_tests::a_count_past_64_bits_fails_as_a_range_limit"
# The stand-in workspace's inventory, worked by hand: two tests not #[ignore]d, an #[ignore]d one no step
# runs, and the count test; and each step's report, every test it selects passing once (nextest's report
# leaves out the tests a run doesn't select).
INVENTORY = {"alpha": {"t1": False, "slow": True}, "beta::it": {"x::t2": False}, "nrese-sparql::it": {COUNT: True}}
FULL = {"alpha": ["t1"], "beta::it": ["x::t2"]}
COUNTED = {"nrese-sparql::it": [COUNT]}
KNOWN = "known::k1"  # a known open fault's test, where a test lists one
# A stand-in benches/reasoning/dl/outside.sh: its call recorded ($CALLS.outside: first seed, cases, label),
# a summary row per seed (outside.sh's columns), its block failing under OUTSIDE=fail-BLOCK, or ending as
# a missed planted fault does (exit 5, no failing row) under OUTSIDE=crash-BLOCK.
OUTSIDE_STUB = """#!/bin/sh
mkdir -p "$1" && echo "$NRESE_FUZZ_SEED $NRESE_FUZZ_CASES $DL_OUTSIDE_LABEL" >> "$CALLS.outside"
outcome=pass; [ "${OUTSIDE:-}" = starve ] && outcome=reference-undecided
seq "$NRESE_FUZZ_SEED" $((NRESE_FUZZ_SEED + NRESE_FUZZ_CASES - 1)) \\
  | awk -v o="$outcome" '{ printf "%s\\tshape\\ttags\\tq\\tclass\\t%s\\tdetail\\n", $1, o }' > "$1/summary.tsv"
[ "${OUTSIDE:-}" != "crash-$(basename "$1")" ] || exit 5
[ "${OUTSIDE:-}" = "fail-$(basename "$1")" ] || exit 0
printf '%s\\tshape\\ttags\\tq\\tclass\\tFAIL\\tdetail\\n' "$NRESE_FUZZ_SEED" >> "$1/summary.tsv"
exit 101
"""


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
        for rel in ("scripts/check.sh", "scripts/lib/nightly-receipt.py", "scripts/lib/gate-key.sh",
                    "scripts/lib/outside-counts.py"):
            shutil.copy(ROOT / rel, self.repo / rel)
        self.listed()  # no known open fault, as the stand-in workspace has none
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

    def listed(self, *faults):
        """scripts/lib/known-faults.tsv: its header, then each (test id, what is wrong)."""
        lines = ["# known open faults", *(f"{test}\t{about}" for test, about in faults)]
        (self.repo / "scripts/lib/known-faults.tsv").write_text("\n".join(lines) + "\n", newline="\n")

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

    def known(self, *, ignored=True, failed=True):
        """A stand-in workspace with one known open fault, beta::it known::k1, listed and inventoried
        (#[ignore]d unless IGNORED is false); its run reports it failed, or passed."""
        self.given({**INVENTORY, "beta::it": {"x::t2": False, KNOWN: ignored}}, FULL, COUNTED)
        self.listed((f"beta::it {KNOWN}", "the fault, said plainly"))
        (self.fake / "known.xml").write_text(junit({"beta::it": [KNOWN]}, failed=(KNOWN,) if failed else ()))

    def test_a_listed_fault_that_fails_is_counted_and_named_and_the_night_passes(self):
        # Spec (scripts/lib/known-faults.tsv, its header): a test of the list runs alone, by its exact
        # name, in the nightly profile and tier, after the other steps. Its failure, which its run exits
        # 100 on, is the outcome `known-open-fault`: counted (known_open_faults, a number) and named with
        # what is wrong, in the receipt and the gate log. The tier passes.
        self.known()
        code, steps, calls, out = self.tier(FAILED_TESTS=KNOWN)
        self.assertEqual(calls[-1], "guarded tier=nightly profile=nightly w3c-required nextest run --locked "
                         f"--no-fail-fast --no-tests=fail --run-ignored only --workspace -E (binary_id(beta::it) & "
                         f"test(={KNOWN}))", f"SPEC known-alone\n{out}")
        self.assertEqual((code, steps.get("known open faults"), steps.get("receipt")), (0, 0, 0),
                         f"SPEC known-passes-the-night\n{out}")
        receipt = self.receipt()
        outcome = {c["id"]: c for c in receipt["cases"]}[f"beta::it {KNOWN}"]
        self.assertEqual((outcome["step"], outcome["outcome"], receipt["known_open_faults"]),
                         ("known", "known-open-fault", 1), "SPEC known-counted")
        self.assertEqual(receipt["known_open_fault_list"],
                         [{"id": f"beta::it {KNOWN}", "about": "the fault, said plainly"}], "SPEC known-named")
        self.assertIn("receipt: known open faults: 1\n"
                      f"receipt: known open fault: beta::it {KNOWN}: the fault, said plainly", out, "SPEC known-log")

    def test_a_listed_fault_that_passes_is_red(self):
        # Spec: a listed test that passes is red, "known fault no longer reproduces", so the list never
        # goes stale: the receipt isn't complete and the tier fails, the known step itself passing.
        self.known(failed=False)
        code, steps, _, out = self.tier()
        self.assertEqual((code, steps.get("known open faults"), steps.get("receipt")), (1, 0, 1),
                         f"SPEC known-no-longer-reproduces\n{out}")
        self.assertIn(f"known: known fault no longer reproduces: beta::it {KNOWN} passed", out,
                      "SPEC known-no-longer-reproduces: its reason")
        self.assertFalse(self.receipt()["complete"], "SPEC known-no-longer-reproduces")

    def test_a_listed_test_the_other_tiers_would_run_is_red(self):
        # Spec: the list is held to the #[ignore] marks, the one thing that keeps a listed test out of the
        # other tiers: a listed test that isn't #[ignore]d fails the receipt and the tier.
        self.known(ignored=False)
        code, steps, _, out = self.tier(FAILED_TESTS=KNOWN)
        self.assertEqual((code, steps.get("receipt")), (1, 1), f"SPEC known-ignored\n{out}")
        self.assertIn(f"the known open fault beta::it {KNOWN} is no #[ignore]d test of the inventory", out,
                      "SPEC known-ignored: its reason")

    def test_a_listed_entry_that_is_no_test_is_red(self):
        # Spec (scripts/lib/known-faults.tsv, its header: Rust tests only): an entry that names no test of
        # the inventory, an outside check's difference say, fails the receipt and the tier: it is never
        # counted as a known open fault.
        self.known()
        self.listed((f"beta::it {KNOWN}", "the fault, said plainly"), ("outside seed-777", "an outside row"))
        code, steps, _, out = self.tier(FAILED_TESTS=KNOWN)
        self.assertEqual((code, steps.get("receipt")), (1, 1), f"SPEC known-tests-only\n{out}")
        self.assertIn("the known open fault outside seed-777 is no #[ignore]d test of the inventory", out,
                      "SPEC known-tests-only: its reason")

    def outside(self, **env):
        """check.sh nightly-outside as its job runs it, over a stand-in outside.sh (OUTSIDE_STUB: a row per
        seed, `pass`, or under OUTSIDE=starve `reference-undecided`; OUTSIDE=fail-BLOCK adds a FAIL row to
        that block and exits 101, as the test does) and a docker that lists no container."""
        stub = self.repo / "benches/reasoning/dl/outside.sh"
        stub.parent.mkdir(parents=True, exist_ok=True)
        stub.write_text(OUTSIDE_STUB, newline="\n")
        docker = self.calls.parent / "bin/docker"
        docker.write_text("#!/bin/sh\nexit 0\n", newline="\n")
        docker.chmod(0o755)
        done = subprocess.run([BASH, "scripts/check.sh", "nightly-outside"], cwd=self.repo, capture_output=True,
                              text=True, timeout=120, stdin=subprocess.DEVNULL, env={**self.env, **env})
        steps = {name: int(code) for code, name in re.findall(r"(?m)^ +(\d+)  (.+) \(\d+ s\)$", done.stdout)}
        runs = Path(f"{self.calls}.outside").read_text().splitlines() if Path(f"{self.calls}.outside").exists() else []
        Path(f"{self.calls}.outside").unlink(missing_ok=True)
        return done.returncode, steps, runs, done.stdout + done.stderr

    def test_the_outside_check_runs_fixed_then_fresh_seeds_and_counts_them(self):
        # Spec (scripts/check.sh nightly-outside): outside.sh over seeds 0..1999, then over 1,000 fresh ones
        # from 5000 up, the same for one commit on one day, each labelled with the job; then the counts:
        # every fixed case compared, a green run. Off a build host it never runs.
        code, steps, runs, out = self.outside()
        self.assertEqual((code, steps), (0, dict.fromkeys(
            ["outside (fixed seeds)", "outside (fresh seeds)", "outside counts"], 0)), f"SPEC outside-steps\n{out}")
        first = int(runs[1].split()[0])
        self.assertEqual((runs[0], runs[1].split()[1:]), ("0 2000 fleet.job=gate-1", ["1000", "fleet.job=gate-1"]),
                         "SPEC outside-blocks")
        self.assertTrue(first >= 5000 and (first - 5000) % 1000 == 0, f"SPEC outside-fresh: {first}")
        self.assertEqual(self.outside()[2][1], runs[1], "SPEC outside-fresh: the same commit and day")
        self.assertIn("outside: seeds 0..1999, drawn 2000, compared 2000, dropped: none; disagreements 0", out,
                      "SPEC outside-log")
        self.assertEqual(self.outside(NRESE_ON_BUILD_HOST="")[0], 2, "SPEC outside-job-only")

    def test_only_a_fresh_difference_never_fails_the_outside_check(self):
        # Spec: the counts step judges both blocks, each run to its end. A difference in the fixed block
        # fails it and the run; one in the fresh block is listed (new_disagreements) and never fails it; a
        # block whose run ended otherwise (here a missed planted fault, exit 5) fails it, the fresh one too;
        # a fixed block with fewer than half its cases compared is an empty run, which fails it.
        code, steps, _, out = self.outside(OUTSIDE="fail-fixed")
        self.assertEqual((code, steps["outside (fixed seeds)"], steps["outside counts"]), (1, 0, 1),
                         f"SPEC outside-fixed-red\n{out}")
        code, steps, _, out = self.outside(OUTSIDE="fail-fresh")
        new = json.loads((self.out / "outside/outside.json").read_text())["new_disagreements"]
        self.assertEqual((code, steps["outside counts"], len(new)), (0, 0, 1), f"SPEC outside-fresh-never-red\n{out}")
        code, steps, _, out = self.outside(OUTSIDE="crash-fresh")
        self.assertEqual((code, steps["outside counts"]), (1, 1), f"SPEC outside-error\n{out}")
        self.assertIn("new disagreements 0; ERROR: outside.sh exit 5", out, "SPEC outside-error")
        code, steps, _, out = self.outside(OUTSIDE="starve")
        self.assertEqual((code, steps["outside counts"]), (1, 1), f"SPEC outside-empty-run\n{out}")
        self.assertIn("EMPTY RUN", out, "SPEC outside-empty-run")

    def test_a_known_run_that_ends_otherwise_fails_its_step(self):
        # Spec: the known step passes only when its tests ran (0, or 100: some failed); any other end,
        # a build that fails here, fails the step and the tier.
        self.known()
        code, steps, _, out = self.tier(FAIL=KNOWN)
        self.assertEqual((code, steps.get("known open faults", 0) != 0), (1, True), f"SPEC known-step\n{out}")


if __name__ == "__main__":
    unittest.main()
