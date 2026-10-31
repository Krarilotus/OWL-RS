"""The push gate (.githooks/pre-push, scripts/check.sh push, scripts/lib/fleet-gate.sh): a pushed
commit is gated on the fleet, by its own tree and the files of the pushed range, and only a verified
fleet result for exactly that tree lets it through. No verdict means no push: nothing runs here
instead. A push of deletions alone has no tree to gate. The merge check's Windows part
(scripts/check.sh windows, scripts/lib/windows-job.sh) is gated the same way, on a Windows host, and
a push to refactor/engine-v2 needs it too.

Run: python -m unittest discover -s benches/suite/tests -p test_push_gate.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
BASH = shutil.which("bash")
# Run from a git hook or inside a gate job, the caller's variables (git's own, the gate's, the
# fleet's, the thread counts, the Windows job's RUSTUP_AUTO_INSTALL) would make the gates these tests
# run behave as their caller does.
LEAKS = ("GIT_", "NRESE_", "FLEET_", "NEXTEST_", "CARGO_BUILD_", "CARGO_TARGET_DIR", "CARGO_ENCODED_RUSTFLAGS",
         "RUSTFLAGS", "RUST_TEST_THREADS", "RUSTUP_AUTO_INSTALL")
CLEAN = {k: v for k, v in os.environ.items() if not k.startswith(LEAKS)}
ZERO = "0" * 40
# The scripts a push runs, copied into each scratch repository (those that exist).
COPIED = (".githooks/pre-push", "scripts/check.sh", "scripts/cargo-guarded.sh", "scripts/lib/disk.sh",
          "scripts/lib/target-cpu.sh", "scripts/lib/remote.sh", "scripts/lib/gate-key.sh",
          "scripts/lib/fleet-gate.sh", "scripts/lib/fleet-job.sh", "scripts/lib/gate-tools.txt",
          "scripts/lib/gate-record.py", "scripts/lib/windows-job.ps1", "scripts/lib/windows-job.sh",
          "benches/suite/requirements.txt")
# A stand-in for the fleet's CLI: each submit keeps its job file; status, results and cancel as told.
FLEET = """#!/bin/sh
case "$1" in
  submit) n=$(ls "$FAKE"/job-*.toml 2> /dev/null | wc -l); cp "$2" "$FAKE/job-$n.toml"; echo gate-1 ;;
  status) cat "$FAKE/status" ;;
  cancel) echo "$2" >> "$FAKE/cancelled" ;;
  results) echo "$FAKE/results" ;;
esac
"""
# A stand-in for rustc: the toolchain a gate's identity names.
RUSTC = """#!/bin/sh
[ "$1" = -vV ] && printf 'rustc 1.98.1\\nbinary: rustc\\ncommit-hash: abcdef1234567890\\nrelease: 1.98.1\\n'
exit 0
"""


class Repo(unittest.TestCase):
    """A scratch repository whose `origin` has refactor/engine-v2, the push scripts copied in, the
    fleet and rustc stood in for."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="nrese-push-gate-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.fake = self.root / "fake"
        (self.fake / "bin").mkdir(parents=True)
        (self.fake / "results" / "out").mkdir(parents=True)
        for name, text in (("fleet", FLEET), ("bin/rustc", RUSTC)):
            (self.fake / name).write_text(text, newline="\n")
            (self.fake / name).chmod(0o755)
        gitconfig = self.root / "gitconfig"
        gitconfig.write_text("[user]\n\tname = t\n\temail = t@t\n[core]\n\tautocrlf = false\n"
                             "[init]\n\tdefaultBranch = main\n")
        self.env = {**CLEAN, "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": gitconfig.as_posix(),
                    "PATH": (self.fake / "bin").as_posix() + os.pathsep + CLEAN.get("PATH", ""),
                    "FAKE": self.fake.as_posix(), "NRESE_FLEET_BIN": (self.fake / "fleet").as_posix(),
                    "FLEET_OWNER": "t3:test", "NRESE_GATE_START_S": "0", "NRESE_GATE_POLL_S": "0"}
        self.origin = self.root / "origin.git"
        self.git(self.root, "init", "-q", "--bare", str(self.origin))
        self.repo = self.root / "repo"
        self.git(self.root, "init", "-q", str(self.repo))
        for rel in COPIED:
            if (ROOT / rel).exists():
                (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(ROOT / rel, self.repo / rel)
        for name in ("a.txt", "b.txt", "c.txt"):
            (self.repo / name).write_text(f"{name}\n", newline="\n")
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-q", "-m", "base")
        self.git(self.repo, "remote", "add", "origin", self.origin.as_posix())
        self.git(self.repo, "push", "-q", "origin", "HEAD:refs/heads/refactor/engine-v2")
        self.git(self.repo, "config", "nrese.buildHost", "fleet")
        self.base = self.git(self.repo, "rev-parse", "HEAD")

    def git(self, where, *args):
        return subprocess.run(["git", "-C", str(where), *args], check=True, capture_output=True, text=True,
                              env=self.env).stdout.strip()

    def commit(self, name, text):
        (self.repo / name).write_text(text, newline="\n")
        self.git(self.repo, "commit", "-q", "-am", f"change {name}")
        return self.git(self.repo, "rev-parse", "HEAD")

    def push(self, *lines):
        """The hook as git runs it for a push to origin: one line per ref on stdin, each ended by a
        bare LF as git ends it (bytes: a text-mode pipe on Windows would add CR)."""
        stdin = "".join(" ".join(line) + "\n" for line in lines).encode()
        done = subprocess.run([BASH, ".githooks/pre-push", "origin", self.origin.as_posix()], cwd=self.repo,
                              input=stdin, capture_output=True, env=self.env, timeout=120)
        return subprocess.CompletedProcess(done.args, done.returncode, done.stdout.decode(errors="replace"),
                                           done.stderr.decode(errors="replace"))

    def fleet(self, state, tree=None, files=(), tier="push"):
        """The fleet's answer: the job's state and, with TREE, a whole, consistent passing result
        of its gate (TIER) for TREE and FILES."""
        (self.fake / "status").write_text(json.dumps({"state": state, "host": "office-pc"}))
        out = self.fake / "results" / "out"
        for name in ("gate.json", "attempt", "gate.log"):
            if (out / name).exists():
                (out / name).unlink()
        if tree is None:
            return
        listed = "".join(f"{f}\n" for f in files).encode()  # bytes: the list check.sh hashes has bare LFs
        list_hash = subprocess.run(["git", "hash-object", "--stdin"], input=listed, capture_output=True, cwd=self.repo,
                                   env=self.env, check=True).stdout.decode().strip()
        identity = subprocess.run([BASH, "-c", ". scripts/lib/gate-key.sh && gate_identity"], cwd=self.repo,
                                  capture_output=True, text=True, env=self.env).stdout.strip()
        (out / "gate.json").write_text(json.dumps({
            "tier": tier, "part": "rest", "tree": tree, "list_hash": list_hash, "identity": identity,
            "w3c_optional": False, "tracked_changes": "", "code": 0, "host": "office-pc", "cpu": "znver3",
            "job": "gate-1", "fleet_host": "office-pc", "attempt": "gate-1:0:1:1", "summary": True,
            "steps": [{"name": "fmt", "code": 0, "seconds": 1}]}))
        (out / "attempt").write_text("gate-1:0:1:1\n")
        (out / "gate.log").write_text("=== summary\n  0  fmt (1 s)\n")

    def jobs(self):
        """(input rev, the gated tree, the files) of each job submitted, in order."""
        found = []
        for path in sorted(self.fake.glob("job-*.toml")):
            text = path.read_text()
            steps = json.loads(re.search(r"^steps = (.*)$", text, re.M).group(1))[0]
            rev = re.search(r'rev = "([0-9a-f]+)"', text).group(1)
            found.append((rev, steps[3], steps[steps.index("--") + 1:]))
        return found

    def tree(self, commit):
        return self.git(self.repo, "rev-parse", f"{commit}^{{tree}}")


class PushGate(Repo):
    """The hook and check.sh's push tier, end to end with the stand-in fleet."""

    def test_deletions_alone_need_no_gate(self):
        """A push that only deletes refs has no tree to gate: the hook lets it through and asks
        nothing of the fleet (I9: the remote deletion of a plan branch was refused on 25 Oct)."""
        env_owner = self.env.pop("FLEET_OWNER")
        done = self.push(("(delete)", ZERO, "refs/heads/plan/old", self.base))
        self.env["FLEET_OWNER"] = env_owner
        self.assertEqual((done.returncode, self.jobs()), (0, []), f"SPEC deletions: {done.stderr}")

    def test_no_fleet_verdict_no_push(self):
        """When no fleet host starts the gate in time, the push is refused: the job is cancelled
        and nothing runs here instead (no local fallback)."""
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("pending")
        done = self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", ZERO))
        self.assertEqual((done.returncode != 0, len(self.jobs()), (self.fake / "cancelled").exists()),
                         (True, 1, True), f"SPEC no-verdict: the fleet wasn't asked, or the push went\n{done.stderr}")
        self.assertNotIn("=== fmt", done.stdout + done.stderr, "a tier ran here")

    def test_a_push_to_engine_v2_is_a_merge_gate_and_a_packages_isnt(self):
        """The hook names the ref each commit goes to: the push tier of refactor/engine-v2's is a merge
        gate on the fleet, a package's isn't (the orchestrator, 31 Oct 2026; Sol, merge-gate-class r1)."""
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("pending")
        self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", ZERO))
        self.push(("refs/heads/v2", tip, "refs/heads/refactor/engine-v2", self.base))
        classes = [re.findall(r"^priority_class = .*$", path.read_text(), re.M)
                   for path in sorted(self.fake.glob("job-*.toml"))]
        self.assertEqual(classes, [[], ['priority_class = "merge-gate"']], "SPEC merge-gate: by the hook")

    def test_a_build_host_variable_in_the_pushers_environment_changes_nothing(self):
        """NRESE_ON_BUILD_HOST, the gate's own mark for a run on a build host, in the pusher's
        environment doesn't open a push: it is gated on the fleet as any push, and with no verdict
        it is refused, with no tier run here (review PR 27 r1, B1: the push went through on an
        empty file list, fmt alone)."""
        cargo = self.fake / "bin" / "cargo"  # a formatted tree's `cargo fmt --check`: the empty tier passes
        cargo.write_text("#!/bin/sh\nexit 0\n", newline="\n")
        cargo.chmod(0o755)
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("pending")
        self.env["NRESE_ON_BUILD_HOST"] = "1"
        done = self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", ZERO))
        self.assertEqual((done.returncode != 0, len(self.jobs()), "=== fmt" in done.stdout + done.stderr),
                         (True, 1, False), "SPEC build-host-env: the push skipped the fleet gate, or a tier ran here\n"
                                           f"{done.stdout}{done.stderr}")

    def test_a_verified_fleet_result_for_the_tree_lets_the_push_through(self):
        """A whole, consistent passing result of the fleet's gate for exactly the pushed tree and
        files lets the push through and is remembered: the next push of that tree needs no job."""
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("done", self.tree(tip), ["a.txt"])
        done = self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", ZERO))
        self.assertEqual(done.returncode, 0, f"SPEC verified: a verified pass didn't let the push through\n"
                                             f"{done.stderr}")
        self.fleet("pending")
        again = self.push(("refs/heads/pkg/y", tip, "refs/heads/pkg/y", ZERO))
        self.assertEqual((again.returncode, len(self.jobs())), (0, 1), f"SPEC verified: not remembered\n{again.stderr}")

    def test_a_pass_counts_only_for_the_pushed_tree(self):
        """A pass for one tree never lets another through: a tip with another tree is gated on
        its own, and a fleet result for the earlier tree doesn't count for it."""
        first = self.commit("a.txt", "a changed\n")
        self.fleet("done", self.tree(first), ["a.txt"])
        done = self.push(("refs/heads/pkg/x", first, "refs/heads/pkg/x", ZERO))
        self.assertEqual(done.returncode, 0, f"SPEC verified: {done.stderr}")
        second = self.commit("b.txt", "b changed\n")
        done = self.push(("refs/heads/pkg/x", second, "refs/heads/pkg/x", first))
        self.assertNotEqual(done.returncode, 0, f"SPEC pushed-tree: a tree passed on another's result\n{done.stderr}")
        self.assertEqual([tree for _, tree, _ in self.jobs()], [self.tree(first), self.tree(second)])

    def test_the_files_come_from_the_pushed_range(self):
        """The gate is told the files of the pushed range, from where the remote has the branch
        (or, new there, where it left refactor/engine-v2) to the pushed commit: not the last commit's
        alone, nor what the work tree holds besides. It gates that commit itself."""
        first = self.commit("a.txt", "a changed\n")
        tip = self.commit("b.txt", "b changed\n")
        (self.repo / "c.txt").write_text("c, not committed\n", newline="\n")
        (self.repo / "d.txt").write_text("d, untracked\n", newline="\n")
        self.fleet("pending")
        self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", ZERO))
        self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", first))
        self.assertEqual(self.jobs(), [(tip, self.tree(tip), ["a.txt", "b.txt"]), (tip, self.tree(tip), ["b.txt"])],
                         "SPEC pushed-range: the gate wasn't told the pushed range's files, for the pushed commit")

    def test_landing_on_engine_v2_takes_the_windows_part_too(self):
        """A push to refactor/engine-v2 lands a merge: besides the push tier it needs the merge
        check's Windows part for the pushed commit, since nothing else builds and tests it on Windows
        before it lands (the commit tier builds nothing). With no Windows verdict the push is refused;
        the merge check's own pass, stored for the commit's inputs, serves without a new job."""
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("done", self.tree(tip), ["a.txt"])  # the push tier passes; no Windows result anywhere
        landing = ("refs/heads/refactor/engine-v2", tip, "refs/heads/refactor/engine-v2", self.base)
        done = self.push(landing)
        jobs = sorted(self.fake.glob("job-*.toml"))
        self.assertNotEqual(done.returncode, 0, f"SPEC landing-windows: engine-v2 took a commit no Windows check "
                                                f"passed\n{done.stderr}")
        self.assertEqual([('os = "windows"' in j.read_text(), f'rev = "{tip}"' in j.read_text()) for j in jobs],
                         [(False, True), (True, True)],
                         "SPEC landing-windows: not the push tier, then the Windows part, both for the pushed commit")
        self.fleet("done", self.tree(tip), [], tier="windows")
        merge_check = subprocess.run([BASH, "scripts/check.sh", "windows", tip], cwd=self.repo, capture_output=True,
                                     text=True, env=self.env, timeout=120)
        self.assertEqual(merge_check.returncode, 0, merge_check.stderr)
        self.fleet("pending")
        again = self.push(landing)
        self.assertEqual((again.returncode, len(list(self.fake.glob("job-*.toml")))), (0, 3),
                         f"SPEC landing-windows-reuse: the merge check's pass didn't serve the push\n{again.stderr}")


class FleetGate(Repo):
    """scripts/lib/fleet-gate.sh on its own, against a stand-in for the fleet's CLI that keeps several
    jobs (gate-1, gate-2, ...). Ported with the script from tooling/remote-gates' test_gate_offload.py."""

    MULTI = """#!/bin/sh
case "$1" in
  submit) n=$(( $(cat "$FAKE/n" 2> /dev/null || echo 0) + 1 )); echo $n > "$FAKE/n"
          cp "$2" "$FAKE/job-$n.toml"; echo gate-$n ;;
  status) cat "$FAKE/status-$2" ;;
  cancel) echo "$2" >> "$FAKE/cancelled" ;;
  results) echo "$FAKE/results-$2" ;;
esac
"""

    def setUp(self):
        super().setUp()
        self.tip = self.commit("a.txt", "a changed\n")
        (self.fake / "fleet").write_text(self.MULTI, newline="\n")

    def reset(self):
        for path in self.fake.iterdir():
            if path.name not in ("fleet", "bin"):
                shutil.rmtree(path) if path.is_dir() else path.unlink()

    def job(self, n, host, state, **result):
        """Job gate-N: its end on HOST, and its record: a whole, passing one, changed by RESULT."""
        (self.fake / f"status-gate-{n}").write_text(json.dumps({"state": state, "host": host}))
        out = self.fake / f"results-gate-{n}" / "out"
        out.mkdir(parents=True)
        (out / "gate.json").write_text(json.dumps({
            "tier": "push", "tree": self.tree(self.tip), "list_hash": "L", "identity": "ID", "w3c_optional": False,
            "tracked_changes": "", "code": 0, "host": host, "cpu": "znver3", "job": f"gate-{n}", "fleet_host": host,
            "attempt": f"gate-{n}:1", "summary": True, "steps": [{"name": "test", "code": 0, "seconds": 1}],
            **result}))
        (out / "attempt").write_text(f"gate-{n}:1\n")
        (out / "gate.log").write_text("=== summary\n  0  test (1 s)\n")

    def gate(self, mode="push"):
        """(exit, where it passed, jobs submitted, stderr) of one fleet-gate.sh run for the tip."""
        where = self.fake / "where"
        done = subprocess.run(
            [BASH, "scripts/lib/fleet-gate.sh", mode, "/dev/null", self.tree(self.tip), "L", "ID", where.as_posix(),
             self.tip], cwd=self.repo, capture_output=True, text=True, env=self.env, timeout=120)
        submitted = int((self.fake / "n").read_text()) if (self.fake / "n").exists() else 0
        return done.returncode, where.read_text().strip() if where.exists() else "", submitted, done.stderr

    def test_only_a_whole_consistent_record_of_this_job_gives_a_verdict(self):
        failing = [{"name": "test", "code": 1, "seconds": 1}]
        cases = [  # the job's end, the record's differences -> exit
            ("done", {}, 0),
            ("failed", {"code": 1, "steps": failing}, 1),
            ("done", {"tree": "0" * 40}, 75),
            ("done", {"identity": "other"}, 75),
            ("done", {"tracked_changes": " M a.txt"}, 75),
            ("done", {"code": 137}, 75),
            # Sol, review-remote-gates-r1-20261020 (EVIDENCE_ACCEPTANCE): a failed job with a
            # zero-exit record and no steps; a done job whose record has exit 0 and a failed step.
            ("failed", {"steps": []}, 75),
            ("done", {"steps": failing}, 75),
            # Another job's, another host's, an earlier attempt's record; no summary.
            ("done", {"job": "gate-0"}, 75),
            ("done", {"fleet_host": "phuoc-yu"}, 75),
            ("done", {"attempt": "gate-1:0"}, 75),
            ("done", {"summary": False}, 75),
        ]
        for state, result, want in cases:
            self.reset()
            self.job(1, "office-pc", state, **result)
            code, where, _, err = self.gate()
            self.assertEqual((code, where), (want, "fleet:gate-1@office-pc cpu=znver3" if want == 0 else ""),
                             f"SPEC whole-record: {state} {result}\n{err}")

    def test_a_job_compiles_and_tests_on_the_cores_it_asks_for(self):
        """A host shared under a presence cap (office-pc, khaleesi) limits memory, not CPUs: the job's
        compilers and test runners use the cores it asks for (remote-gates, uncommitted, 21 Oct)."""
        (self.fake / "status-gate-1").write_text(json.dumps({"state": "pending", "host": "-"}))
        self.env["NRESE_GATE_START_S"] = "0"
        self.gate()
        job = (self.fake / "job-1.toml").read_text()
        for var in ("CARGO_BUILD_JOBS", "NEXTEST_TEST_THREADS", "RUST_TEST_THREADS"):
            self.assertIn(f'{var} = "8"', job, "SPEC job-cores")
        self.assertIn("cores = 8", job)
        self.assertIn(f'rev = "{self.tip}"', job, "the job isn't for the commit")

    def test_the_merge_checks_jobs_are_merge_gates(self):
        """The orchestrator, 31 Oct 2026: the merge check's jobs go ahead as the fleet's merge gates, its
        Windows part always and its push tier for a commit that goes to refactor/engine-v2, as its caller
        states (NRESE_PUSH_REF). Sol, merge-gate-class r1: told by the parents instead, a merge check of
        merges chained before their push wasn't one, and a package that merges another was."""
        package = self.tip
        self.git(self.repo, "checkout", "-q", "-b", "other", self.base)
        other = self.commit("b.txt", "b changed\n")
        # engine-v2 as a merge check meets it: two merges on the base, neither pushed yet
        self.git(self.repo, "checkout", "-q", "-b", "v2", self.base)
        self.git(self.repo, "merge", "-q", "--no-ff", "-m", "merge a package", package)
        self.git(self.repo, "merge", "-q", "--no-ff", "-m", "merge another", other)
        chained = self.git(self.repo, "rev-parse", "HEAD")
        # a package that takes another in, from engine-v2's head
        self.git(self.repo, "checkout", "-q", "-b", "integrated", self.base)
        self.git(self.repo, "merge", "-q", "--no-ff", "-m", "take a package in", package)
        integrated = self.git(self.repo, "rev-parse", "HEAD")
        v2, gate = "refs/heads/refactor/engine-v2", 'priority_class = "merge-gate"'
        cases = [  # tier, commit, the ref it goes to -> its job's class
            ("windows", package, None, gate),
            ("push", chained, v2, gate),
            ("push", integrated, "refs/heads/pkg/integrated", None),
            ("push", chained, None, None),  # by hand, naming no ref
        ]
        for mode, commit, ref, want in cases:
            self.reset()
            (self.fake / "status-gate-1").write_text(json.dumps({"state": "pending", "host": "-"}))
            self.tip = commit
            self.env.pop("NRESE_PUSH_REF", None)
            if ref:
                self.env["NRESE_PUSH_REF"] = ref
            self.gate(mode)
            got = re.findall(r"^priority_class = .*$", (self.fake / "job-1.toml").read_text(), re.M)
            self.assertEqual(got, [want] if want else [], f"SPEC merge-gate: {mode} {commit} for {ref}")

    def test_a_failure_on_an_unconfirmed_host_counts_only_when_another_host_fails_too(self):
        cases = [  # first host, its code, the re-run's code -> exit, recorded pass, jobs submitted
            # The pass names the run it re-checked (Sol: durable provenance).
            ("khaleesi-wsl", 1, 0, 0,
             "fleet:gate-2@office-pc cpu=znver3 re-checking fleet:gate-1@khaleesi-wsl (failed there)", 2),
            ("khaleesi-wsl", 1, 1, 1, "", 2),
            ("office-pc", 1, None, 1, "", 1),
        ]
        failing = {"code": 1, "steps": [{"name": "test", "code": 1, "seconds": 1}]}
        for first, code, again, want, recorded, submitted in cases:
            self.reset()
            self.job(1, first, "failed", **failing)
            if again is not None:
                self.job(2, "office-pc", "done" if again == 0 else "failed", **({} if again == 0 else failing))
            got, where, n, err = self.gate()
            self.assertEqual((got, where, n), (want, recorded, submitted), f"SPEC unconfirmed: {first} {again}\n{err}")
            if again is not None:
                self.assertIn(f"unconfirmed ({first})", err)
                self.assertIn('hosts = ["office-pc", "phuoc-yu", "gamergrill"]', (self.fake / "job-2.toml").read_text())

    def test_a_windows_failure_on_khaleesi_is_checked_again_on_a_windows_host(self):
        """The Windows part's re-check after an unconfirmed failure goes to kraris-gptler: a Linux host
        would never start it."""
        failing = {"code": 1, "steps": [{"name": "test", "code": 1, "seconds": 1}]}
        self.job(1, "khaleesi", "failed", tier="windows", **failing)
        self.job(2, "kraris-gptler", "done", tier="windows")
        got, where, n, err = self.gate("windows")
        self.assertEqual(
            (got, where, n, 'hosts = ["kraris-gptler"]' in (self.fake / "job-2.toml").read_text()),
            (0, "fleet:gate-2@kraris-gptler cpu=znver3 re-checking fleet:gate-1@khaleesi (failed there)", 2, True),
            f"SPEC windows-unconfirmed\n{err}")

    def test_no_gate_under_flags_its_identity_doesnt_name(self):
        """Sol, review-remote-gates-r1-20261020 (ENCODED_RUSTFLAGS): two encoded cfgs gave one identity."""
        for flags in ("--cfg\x1fvariant_a", ""):
            done = subprocess.run([BASH, "-c", ". scripts/lib/gate-key.sh && gate_identity"], cwd=self.repo,
                                  capture_output=True, text=True, env={**self.env, "CARGO_ENCODED_RUSTFLAGS": flags})
            self.assertNotEqual(done.returncode, 0, f"SPEC encoded-flags: {done.stdout}")
            self.assertIn("CARGO_ENCODED_RUSTFLAGS", done.stderr)


class MeasurementRefs(Repo):
    """A pushed refs/measure/* ref is a commit on an old side's tree (benches/PROTOCOL.md §5), which
    engine-v2's push tier isn't for: check.sh push checks it by the measurement rule against the
    declared sides (release_gate.py measurement-ref), and refuses anything else. T0, 28 Oct 2026: the
    tier ran on main's and v1.5's measurement commits and both gates failed, so neither ref could go."""

    PERF_LAB = "crates/nrese-store/examples/perf_lab.rs"

    def setUp(self):
        super().setUp()
        (self.repo / "benches/fast").mkdir(parents=True, exist_ok=True)
        shutil.copy(ROOT / "benches/fast/release_gate.py", self.repo / "benches/fast/release_gate.py")
        shutil.copytree(ROOT / "benches/suite/suitekit", self.repo / "benches/suite/suitekit",
                        ignore=shutil.ignore_patterns("__pycache__"))
        self.side = self.add({self.PERF_LAB: "fn main() { spawn(); }\n",
                              "crates/nrese-store/src/lib.rs": "pub fn engine() {}\n"}, "an old side")
        self.git(self.repo, "push", "-q", "origin", "HEAD:refs/heads/refactor/engine-v2")

    def add(self, changes: dict, message: str, on: str | None = None) -> str:
        if on:
            self.git(self.repo, "checkout", "-q", "--detach", on)
        for path, text in changes.items():
            (self.repo / path).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / path).write_text(text, newline="\n")
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-q", "-m", message)
        return self.git(self.repo, "rev-parse", "HEAD")

    def declare(self, side: str, ref: str, commit: str):
        """The sides table of the pushing checkout: SIDE measured at COMMIT, pushed as REF."""
        (self.repo / "benches/fast/measurement-sides.toml").write_text(
            f'[side.{side}]\ncommit = "{commit}"\nref = "{ref}"\n', newline="\n")

    def pushed(self, ref: str, commit: str):
        self.fleet("pending")
        return self.push((ref, commit, ref, ZERO))

    def test_a_declared_measurement_ref_goes_without_the_push_tier(self):
        commit = self.add({self.PERF_LAB: "fn main() { arm(); }\n"}, "its measurement side", self.side)
        ref = f"refs/measure/old-{self.side[:8]}"
        self.declare(self.side, ref, commit)
        done = self.pushed(ref, commit)
        self.assertEqual((done.returncode, self.jobs()), (0, []), f"SPEC measurement ref: {done.stderr}")

    def test_a_measurement_ref_whose_commit_touches_the_engine_is_refused(self):
        commit = self.add({self.PERF_LAB: "fn main() { arm(); }\n",
                           "crates/nrese-store/src/lib.rs": "pub fn engine() { faster() }\n"}, "more", self.side)
        ref = f"refs/measure/old-{self.side[:8]}"
        self.declare(self.side, ref, commit)
        done = self.pushed(ref, commit)
        self.assertNotEqual(done.returncode, 0, "SPEC measurement ref: a commit touching the engine went")
        self.assertIn("changes more than its perf lab", done.stdout + done.stderr)
        self.assertEqual(self.jobs(), [], "the push tier ran instead")

    def test_a_measurement_ref_that_doesnt_name_its_commits_parent_is_refused(self):
        later = self.add({"a.txt": "later\n"}, "a later revision", self.side)
        commit = self.add({self.PERF_LAB: "fn main() { arm(); }\n"}, "on the later revision", later)
        ref = f"refs/measure/old-{self.side[:8]}"
        self.declare(self.side, ref, commit)  # the ref names the side; the commit's parent is another
        wrong_parent = self.pushed(ref, commit)
        self.declare(self.side, ref, commit)
        undeclared = self.pushed(f"refs/measure/old-{later[:8]}", commit)
        self.assertEqual((wrong_parent.returncode != 0, undeclared.returncode != 0), (True, True),
                         f"SPEC measurement ref: went\n{wrong_parent.stderr}\n{undeclared.stderr}")
        self.assertIn(f"isn't a commit on {self.side[:12]} alone", wrong_parent.stdout + wrong_parent.stderr)
        self.assertIn("declares this measurement ref", undeclared.stdout + undeclared.stderr)
        self.assertEqual(self.jobs(), [])

    def test_a_table_with_an_abbreviated_side_admits_nothing(self):
        """Sol, T0 refs r1 S1: with the side abbreviated, the lookup by full revision missed, the
        parent and path checks never ran, and a commit changing the lockfile went."""
        later = self.add({"a.txt": "later\n"}, "a later revision", self.side)
        commit = self.add({"Cargo.lock": "# another\n"}, "not a measurement side", later)
        ref = f"refs/measure/old-{self.side[:8]}"
        self.declare(self.side[:8], ref, commit)
        done = self.pushed(ref, commit)
        self.assertNotEqual(done.returncode, 0, f"SPEC measurement ref: went\n{done.stderr}")
        self.assertIn("full revisions", done.stdout + done.stderr)
        self.assertEqual(self.jobs(), [])

    def test_a_measurement_ref_whose_side_is_on_no_remote_is_refused(self):
        """T0, 28 Oct 2026: main's and v1.5's sides were ids from before the history rewrite, on no
        remote ref; the ref's push would have published them with it."""
        unpublished = self.add({"a.txt": "only here\n"}, "a side no remote has", self.side)
        commit = self.add({self.PERF_LAB: "fn main() { arm(); }\n"}, "its measurement side", unpublished)
        ref = f"refs/measure/old-{unpublished[:8]}"
        self.declare(unpublished, ref, commit)
        done = self.pushed(ref, commit)
        self.assertNotEqual(done.returncode, 0, f"SPEC measurement ref: an unpublished side went\n{done.stderr}")
        self.assertIn("the side must be published", done.stdout + done.stderr)
        self.assertEqual(self.jobs(), [])

    def test_a_graft_publishes_no_side(self):
        """PR #54 fix check 1, the check's other walk: whether the side is on a remote ref was
        asked of a history walk, which a graft changes; a graft putting an unpublished side under
        the remote branch let its ref go, and the push would have published the side. The check
        reads the objects as stored (release_gate.py STORED_OBJECTS)."""
        unpublished = self.add({"a.txt": "only here\n"}, "a side no remote has", self.base)
        commit = self.add({self.PERF_LAB: "fn main() { arm(); }\n"}, "its measurement side", unpublished)
        self.git(self.repo, "checkout", "-q", "--detach", self.side)  # the push's own scripts, which base lacks
        git_dir = Path(self.git(self.repo, "rev-parse", "--absolute-git-dir"))
        (git_dir / "info").mkdir(exist_ok=True)
        (git_dir / "info" / "grafts").write_text(f"{self.side} {unpublished}\n", encoding="utf-8")
        ref = f"refs/measure/old-{unpublished[:8]}"
        self.declare(unpublished, ref, commit)
        done = self.pushed(ref, commit)
        self.assertNotEqual(done.returncode, 0, f"SPEC measurement ref: a grafted side went\n{done.stderr}")
        self.assertIn("the side must be published", done.stdout + done.stderr)

    def test_a_remote_tracking_ref_made_here_publishes_no_side(self):
        """Sol, PR #54 fix check 1 S2: the check took any refs/remotes/* ref for the remote's word, and
        any local process can write one; a side no remote has went with a ref made here. Only the
        remote's own list of its branches counts."""
        unpublished = self.add({"a.txt": "only here\n"}, "a side no remote has", self.side)
        commit = self.add({self.PERF_LAB: "fn main() { arm(); }\n"}, "its measurement side", unpublished)
        self.git(self.repo, "update-ref", "refs/remotes/origin/planted", unpublished)
        ref = f"refs/measure/old-{unpublished[:8]}"
        self.declare(unpublished, ref, commit)
        done = self.pushed(ref, commit)
        self.assertNotEqual(done.returncode, 0, f"SPEC measurement ref: a side published by a local ref went\n"
                                                f"{done.stderr}")
        self.assertIn("the side must be published", done.stdout + done.stderr)
        self.assertEqual(self.jobs(), [])

    def test_a_remote_that_cant_be_asked_publishes_no_side(self):
        """The check asks the remote the push goes to; a remote that doesn't answer proves nothing, even
        for a side its branches have."""
        commit = self.add({self.PERF_LAB: "fn main() { arm(); }\n"}, "its measurement side", self.side)
        ref = f"refs/measure/old-{self.side[:8]}"
        self.declare(self.side, ref, commit)
        self.fleet("pending")
        gone = (self.root / "gone.git").as_posix()
        done = subprocess.run([BASH, ".githooks/pre-push", "origin", gone], cwd=self.repo,
                              input=f"{ref} {commit} {ref} {ZERO}\n".encode(), capture_output=True, env=self.env,
                              timeout=120)
        self.assertNotEqual(done.returncode, 0, "SPEC measurement ref: went without the remote's answer")
        self.assertIn(b"couldn't be asked", done.stdout + done.stderr)
        self.assertEqual(self.jobs(), [])

    @unittest.skipUnless(shutil.which("setsid"), "a process left behind in its own session (setsid)")
    def test_a_process_a_remote_leaves_behind_doesnt_hold_the_answer(self):
        """Sol, S2 r1 D1: git's output was read from pipes, drained without a bound, so a transport
        helper's process left behind holding stderr (an ssh master, say) kept the check waiting after
        git had failed. Only git's exit counts: the push is refused at once."""
        commit = self.add({self.PERF_LAB: "fn main() { arm(); }\n"}, "its measurement side", self.side)
        ref = f"refs/measure/old-{self.side[:8]}"
        self.declare(self.side, ref, commit)
        self.fleet("pending")
        helper, pidfile = self.root / "helper.sh", self.root / "helper.pid"
        helper.write_text(f"setsid sh -c 'echo $$ > {pidfile.as_posix()}; exec sleep 100' >&2 &\nexit 1\n",
                          newline="\n")
        self.git(self.repo, "config", "protocol.ext.allow", "always")
        try:
            done = subprocess.run([BASH, ".githooks/pre-push", "origin", f"ext::sh {helper.as_posix()}"],
                                  cwd=self.repo, input=f"{ref} {commit} {ref} {ZERO}\n".encode(),
                                  capture_output=True, env=self.env, timeout=45)
        finally:
            if pidfile.exists():
                try:
                    os.kill(int(pidfile.read_text()), 9)
                except (ValueError, ProcessLookupError):
                    pass
        self.assertNotEqual(done.returncode, 0, "SPEC measurement ref: went without the remote's answer")
        self.assertIn(b"couldn't be asked", done.stdout + done.stderr)


@unittest.skipUnless(shutil.which("flock"), "the gate job runs on Linux fleet hosts (flock)")
class GateJob(Repo):
    """scripts/lib/gate-job.sh, the job's step on its host, with stand-ins for its tools. Ported with the
    script from tooling/remote-gates' test_gate_offload.py."""

    def test_it_runs_only_the_tree_it_was_sent_on_a_host_that_links_and_says_what_it_was(self):
        stubs = self.root / "stubs"
        stubs.mkdir()
        for name, body in {
            "rustup": "exit 0",
            "rustc": 'case "$1" in -vV) echo "release: 1.98.1"; echo "commit-hash: abcdef1234567890" ;;'
                     ' --print) echo "    native - Select the CPU of the current host (currently znver3)." ;; esac',
            "cargo": 'echo "cargo-nextest 0.9.146"',  # the pinned one (Miri's too): nothing to install
            "cc-links": 'while [ $# -gt 0 ]; do [ "$1" = -o ] && { shift; : > "$1"; }; shift; done',
            "cc-broken": "exit 1",
        }.items():
            (stubs / name).write_text("#!/bin/sh\n" + body + "\n")
            (stubs / name).chmod(0o755)
        cache, seen = self.root / "cache", self.root / "seen"
        # check.sh: says whether an old cache entry was still there while it ran, where it builds, and
        # prints its summary.
        check = ('echo "old=$(test -d "$NRESE_GATE_CACHE/target-old" && echo there)" > "$SEEN"\n'
                 'echo "target=$CARGO_TARGET_DIR" >> "$SEEN"\n'
                 'printf "=== summary\\n  0  fmt (1 s)\\n"\n')
        for rel, body in {"scripts/check.sh": check, "scripts/cargo-guarded.sh": 'echo "-C target-cpu=native"\n',
                          "rust-toolchain.toml": '[toolchain]\nchannel = "1.98.1"\n',
                          "benches/suite/requirements.txt": ""}.items():
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / rel).write_text(body)
        # The suite's Python environment, made already (the empty requirements file's).
        empty = self.git(self.repo, "hash-object", "/dev/null")
        python = cache / f"py-{empty[:12]}" / "bin"
        python.mkdir(parents=True)
        (python / "python").write_text('#!/bin/sh\nexec python3 "$@"\n')
        (python / "python").chmod(0o755)
        shutil.copy(ROOT / "scripts/lib/gate-job.sh", self.repo / "scripts/lib/gate-job.sh")
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-q", "-m", "two")
        tree = self.tree("HEAD")
        cases = [  # tree sent, C compiler -> exit
            (tree, "cc-links", 0),
            ("0" * 40, "cc-links", 3),   # another tree checked out
            (tree, "no-such-cc", 3),     # no C compiler: no verdict
            (tree, "cc-broken", 3),      # one that doesn't link: no verdict
        ]
        for case, (sent, linker, code) in enumerate(cases):
            old = cache / "target-old"  # another toolchain's, unused for long
            old.mkdir(exist_ok=True)
            (old / ".last-used").write_text("0")
            out = self.root / f"out-{case}"
            out.mkdir()
            done = subprocess.run(
                [BASH, "scripts/lib/gate-job.sh", "push", sent, empty, "--"], cwd=self.repo, capture_output=True,
                text=True, timeout=120,
                env={**self.env, "FLEET_OUT": str(out), "NRESE_GATE_CACHE": str(cache), "SEEN": str(seen),
                     "HOME": str(self.root), "PATH": f"{stubs}:{os.environ['PATH']}", "NRESE_GATE_CC": linker,
                     "FLEET_JOB": f"gate-{case}", "FLEET_STEP": "1", "FLEET_HOST": "office-pc"})
            self.assertEqual(done.returncode, code, f"SPEC job: {sent} {linker}\n{done.stdout}{done.stderr}")
            if code:
                self.assertFalse((out / "gate.json").exists(), linker)
                continue
            result = json.loads((out / "gate.json").read_text())
            self.assertEqual({k: result[k] for k in ("tier", "tree", "code", "cpu", "tracked_changes", "job",
                                                     "fleet_host", "summary")},
                             {"tier": "push", "tree": tree, "code": 0, "cpu": "znver3", "tracked_changes": "",
                              "job": f"gate-{case}", "fleet_host": "office-pc", "summary": True})
            self.assertEqual(result["attempt"], (out / "attempt").read_text().strip())
            self.assertRegex(result["identity"],
                             r"^rustc-1\.98\.1-abcdef123456 flags=-C_target-cpu=native tools=[0-9a-f]{12}$")
            self.assertEqual(result["steps"], [{"name": "fmt", "code": 0, "seconds": 1}])
            said = dict(line.split("=", 1) for line in seen.read_text().split())
            # The sweep after the run, not before.
            self.assertEqual(said["old"], "there")
            self.assertFalse(old.exists(), "the stale entry wasn't swept after the run")
            # A target of the gates' own in the cache, not target-<release>, which /verify's builds
            # from other checkouts shared until 26 Oct 2026 (cargo decides freshness by mtimes).
            target = Path(said["target"])
            self.assertTrue(target.parent == cache and target.name != "target-1.98.1",
                            f"SPEC gate-target: the gate builds in {target}")


class WindowsCheck(Repo):
    """scripts/check.sh windows COMMIT, with the stand-in fleet."""

    def check(self, commit):
        return subprocess.run([BASH, "scripts/check.sh", "windows", commit], cwd=self.repo, capture_output=True,
                              text=True, env=self.env, timeout=120)

    def submitted(self):
        return sorted(self.fake.glob("job-*.toml"))

    def test_it_is_the_commits_own_job_on_a_host_that_can_run_the_suite(self):
        """Without a verdict there is no pass, and the exit says so (75, not a failure's 1): the job
        is cancelled and nothing runs here. The job is COMMIT's own (its checkout, its launcher, its
        tree); it states what it needs (a host that can run the suite, 6 cores, its memory held to
        the ask) and picks no host."""
        older = self.commit("a.txt", "a changed\n")
        self.commit("b.txt", "b changed\n")
        self.fleet("pending")
        done = self.check(older)
        self.assertEqual((done.returncode, len(self.submitted()), (self.fake / "cancelled").exists()),
                         (75, 1, True), f"SPEC windows-no-verdict\n{done.stdout}{done.stderr}")
        self.assertNotIn("=== ", done.stdout, "a step ran here")
        text = self.submitted()[0].read_text()
        steps = json.loads(re.search(r"^steps = (.*)$", text, re.M).group(1))
        self.assertEqual(steps, [["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
                                  "scripts/lib/windows-job.ps1", self.tree(older)]], "SPEC windows-job")
        for line in ('os = "windows"', 'kind = "owlrs.any"', 'has = ["owlrs-windows-suite"]', "cores = 6",
                     f'rev = "{older}"', "ram_gb = 9", 'NRESE_MEMORY_CAP_GB = "9"'):
            self.assertIn(line, text, "SPEC windows-job")
        self.assertNotIn("hosts =", text, "SPEC windows-job: the tool picked a host")

    def test_a_verified_result_passes_once_and_never_opens_a_push(self):
        """A whole, consistent Windows result for the tree passes and is remembered; it is the
        Windows part's pass only: a push of the same tree is gated as any push."""
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("done", self.tree(tip), [], tier="windows")
        done = self.check(tip)
        self.assertEqual(done.returncode, 0, f"SPEC windows-verified\n{done.stderr}")
        self.fleet("pending")
        again = self.check(tip)
        self.assertEqual((again.returncode, len(self.submitted())), (0, 1), f"SPEC windows-verified\n{again.stderr}")
        pushed = self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", tip))  # nothing new in its range
        self.assertEqual((pushed.returncode != 0, len(self.submitted())), (True, 2),
                         f"SPEC tier-key: the Windows pass let a push through\n{pushed.stderr}")

    def passed(self):
        """A commit that passed the Windows part: a crate naming one doc (as nrese-server compiles
        docs/ops/config-reference.md in), a doc nothing names, and files a crate's tests may read;
        the fleet answers no more."""
        for path, text in {"crates/x/src/lib.rs": 'pub const R: &str = include_str!("../../../docs/named.md");\n',
                           "docs/named.md": "read by code\n", "docs/note.md": "words only\n",
                           "Dockerfile": "ARG RUST_VERSION=1.98.1\n", "ops/compose.yml": "endpoint: /dataset/sparql\n"
                           }.items():
            (self.repo / path).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / path).write_text(text, newline="\n")
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-qm", "passed")
        passed = self.git(self.repo, "rev-parse", "HEAD")
        self.fleet("done", self.tree(passed), [], tier="windows")
        self.assertEqual(self.check(passed).returncode, 0)
        self.fleet("pending")
        return passed

    def changed(self, base, path):
        """A commit on BASE that changes PATH, checked out."""
        self.git(self.repo, "checkout", "-q", "--detach", base)
        (self.repo / path).write_text("changed\n", newline="\n")
        self.git(self.repo, "commit", "-qam", f"change {path}")
        return self.git(self.repo, "rev-parse", "HEAD")

    def test_a_commit_that_changes_only_docs_no_code_names_carries_the_pass(self):
        """A commit whose inputs are a passed commit's carries that pass with its record, and asks
        nothing of the fleet."""
        passed = self.passed()
        done = self.check(self.changed(passed, "docs/note.md"))
        self.assertEqual((done.returncode, len(self.submitted())), (0, 1), f"SPEC windows-carry\n{done.stderr}")
        self.assertIn(f"carried from {passed} fleet:gate-1@office-pc cpu=znver3", done.stdout, "SPEC windows-carry")

    def test_a_commit_that_changes_an_input_runs_again(self):
        """Rust code, any file but documentation (a crate's tests read the Dockerfile and ops/), and a
        doc that code names are inputs: a change to one runs the Windows part again (Sol, 26 Oct), also
        when the user's settings decorate git's grep output or take files for binary."""
        for key in ("grep.lineNumber", "grep.column", "diff.default.binary"):
            self.git(self.repo, "config", "--global", key, "true")
        self.git(self.repo, "config", "--global", "color.grep", "always")
        passed = self.passed()
        for path in ("crates/x/src/lib.rs", "Dockerfile", "ops/compose.yml", "docs/named.md"):
            with self.subTest(path):
                before = len(self.submitted())
                done = self.check(self.changed(passed, path))
                self.assertEqual((done.returncode != 0, len(self.submitted())), (True, before + 1),
                                 f"SPEC windows-inputs: a change to {path} carried {passed}'s pass\n{done.stdout}")

    def test_a_commit_from_before_the_windows_tier_is_refused_before_any_job(self):
        self.git(self.repo, "rm", "-q", "scripts/lib/windows-job.ps1")
        self.git(self.repo, "commit", "-qm", "no windows tier")
        self.fleet("pending")
        done = self.check(self.git(self.repo, "rev-parse", "HEAD"))
        self.assertEqual((done.returncode, self.submitted(), "has no Windows tier" in done.stderr), (2, [], True),
                         f"SPEC windows-tier\n{done.stderr}")


class WindowsJob(Repo):
    """scripts/lib/windows-job.sh, the Windows part's step on its host, in the job's checkout of the
    commit: it readies the host and runs the tree's own check.sh windows, with stand-ins for the
    tree's build and test tools and for the toolchain."""

    def test_it_runs_the_trees_own_windows_tier_and_says_what_it_was(self):
        stubs = self.root / "stubs"
        stubs.mkdir()
        # One workspace package, and no cargo-nextest: cargo test, crate by crate.
        (stubs / "cargo").write_text('#!/bin/sh\n[ "$1" = metadata ] || exit 1\necho \'{"packages": [{"name": "x"}]}\'\n',
                                     newline="\n")
        # rustup's proxy where STUB_NO_TOOLCHAIN is set: the tree's toolchain isn't installed, and the
        # proxy installs it (STUB_NO_TOOLCHAIN, a file, says so) unless RUSTUP_AUTO_INSTALL=0.
        (stubs / "rustc").write_text(
            '#!/bin/sh\nif [ -n "$STUB_NO_TOOLCHAIN" ]; then\n  [ "$RUSTUP_AUTO_INSTALL" != 0 ] || exit 1\n'
            f'  : > "$STUB_NO_TOOLCHAIN"\nfi\nexec "{(self.fake / "bin" / "rustc").as_posix()}" "$@"\n', newline="\n")
        for name in ("cargo", "rustc"):
            (stubs / name).chmod(0o755)
        # The tree's cargo-guarded.sh: needs a usable stdin, as Python's subprocess and coreutils do,
        # says where it builds, and fails as STUB_FAIL says (<its first argument>:<exit>).
        guarded = ('#!/bin/sh\ncase "$1" in --print-rustflags) echo "-C target-cpu=native"; exit 0 ;; esac\n'
                   f'"{Path(sys.executable).as_posix()}" -c "import os; os.fstat(0)" || exit 9\n'
                   'printf %s "$CARGO_TARGET_DIR" > "$FLEET_OUT/target-dir"\n'
                   '[ "${STUB_FAIL%%:*}" != "$1" ] || exit "${STUB_FAIL#*:}"\n')
        for rel, body in {"scripts/cargo-guarded.sh": guarded, "scripts/fetch-w3c-tests.sh": "exit 0\n",
                          "benches/suite/tests/isolation.py": "import sys\nsys.exit(0)\n"}.items():
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / rel).write_text(body, newline="\n")
        (self.repo / "scripts/cargo-guarded.sh").chmod(0o755)
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-q", "-m", "the commit under test")
        tree = self.tree("HEAD")
        empty = self.git(self.repo, "hash-object", "/dev/null")
        cases = [  # tree sent, STUB_FAIL, stdin closed, toolchain missing -> exit, the build step's exit, stdin
            (tree, "", False, False, 0, 0, "valid"),
            ("0" * 40, "", False, False, 3, None, None),         # another tree checked out: no verdict
            (tree, "build:101", False, False, 1, 101, "valid"),  # a step fails: the tree fails
            (tree, "build:75", False, False, 75, 75, "valid"),   # the PC guard stopped a build: no verdict
            (tree, "", True, False, 0, 0, "invalid"),            # no usable stdin (fleet I14): said, steps get /dev/null
            (tree, "", False, True, 3, None, None),              # no toolchain for the tree: no verdict, none installed
        ]
        for case, (sent, fail, closed, missing, code, build, stdin) in enumerate(cases):
            out = self.root / f"out-{case}"
            out.mkdir()
            installed = self.root / f"installed-{case}"
            run = ("exec 0<&-; " if closed else "") + 'exec bash "$@"'
            done = subprocess.run(
                [BASH, "-c", run, "-", "scripts/lib/windows-job.sh", sent], cwd=self.repo,
                stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120,
                env={**self.env, "PATH": stubs.as_posix() + os.pathsep + self.env["PATH"], "FLEET_OUT": str(out),
                     "FLEET_JOB": f"win-{case}", "FLEET_STEP": "1", "FLEET_HOST": "kraris-gptler", "STUB_FAIL": fail,
                     "STUB_NO_TOOLCHAIN": installed.as_posix() if missing else "",
                     "CARGO_TARGET_DIR": (self.root / "shared-target").as_posix()})  # a host's shared one
            self.assertEqual(done.returncode, code, f"SPEC windows-run: case {case}\n{done.stdout}{done.stderr}")
            self.assertFalse(installed.exists(), "SPEC windows-toolchain: a toolchain was installed")
            if build is None:
                self.assertFalse((out / "gate.json").exists(), f"case {case}")
                continue
            built = Path((out / "target-dir").read_text())
            self.assertTrue(built.name == "target" and os.path.samefile(built.parent, self.repo),
                            f"SPEC windows-target: built in {built}, not its checkout's own target directory")
            result = json.loads((out / "gate.json").read_text())
            self.assertEqual(
                {k: result[k] for k in ("tier", "tree", "list_hash", "code", "stdin", "tracked_changes", "job",
                                        "fleet_host", "summary")},
                {"tier": "windows", "tree": tree, "list_hash": empty, "code": code, "stdin": stdin,
                 "tracked_changes": "", "job": f"win-{case}", "fleet_host": "kraris-gptler", "summary": True},
                f"SPEC windows-record: case {case}")
            self.assertEqual([(s["name"], s["code"]) for s in result["steps"]],
                             [("W3C data", 0), ("build", build), ("test", 0), ("doc tests", 0), ("suite contracts", 0)],
                             f"SPEC windows-run: case {case}")
            self.assertEqual(result["attempt"], (out / "attempt").read_text().strip())
            self.assertRegex(result["identity"],
                             r"^rustc-1\.98\.1-abcdef123456 flags=-C_target-cpu=native tools=[0-9a-f]{12}$")


if __name__ == "__main__":
    unittest.main()
