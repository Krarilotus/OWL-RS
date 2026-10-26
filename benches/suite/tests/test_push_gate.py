"""The push gate (.githooks/pre-push, scripts/check.sh push, scripts/lib/fleet-gate.sh): a pushed
commit is gated on the fleet, by its own tree and the files of the pushed range, and only a verified
fleet result for exactly that tree lets it through. No verdict means no push: nothing runs here
instead. A push of deletions alone has no tree to gate. The merge check's Windows part
(scripts/check.sh windows, scripts/lib/windows-job.sh) is gated the same way, on a Windows host.

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
        # check.sh: says whether an old cache entry was still there while it ran, and prints its summary.
        check = ('echo "old=$(test -d "$NRESE_GATE_CACHE/target-old" && echo there)" > "$SEEN"\n'
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
            # The sweep after the run, not before.
            self.assertEqual(seen.read_text().split(), ["old=there"])
            self.assertFalse(old.exists(), "the stale entry wasn't swept after the run")


class WindowsCheck(Repo):
    """scripts/check.sh windows COMMIT, with the stand-in fleet."""

    def check(self, commit):
        return subprocess.run([BASH, "scripts/check.sh", "windows", commit], cwd=self.repo, capture_output=True,
                              text=True, env=self.env, timeout=120)

    def submitted(self):
        return sorted(self.fake.glob("job-*.toml"))

    def test_it_is_a_job_on_a_windows_host_for_the_commits_own_tree_from_heads_scripts(self):
        """Without a verdict there is no pass: the job is cancelled, nothing runs here. The job runs
        HEAD's launcher on COMMIT and its tree, from a checkout of HEAD (which holds COMMIT), held to
        its memory ask."""
        older = self.commit("a.txt", "a changed\n")
        head = self.commit("b.txt", "b changed\n")
        self.fleet("pending")
        done = self.check(older)
        self.assertEqual((done.returncode != 0, len(self.submitted()), (self.fake / "cancelled").exists()),
                         (True, 1, True), f"SPEC windows-no-verdict\n{done.stdout}{done.stderr}")
        self.assertNotIn("=== ", done.stdout, "a step ran here")
        text = self.submitted()[0].read_text()
        steps = json.loads(re.search(r"^steps = (.*)$", text, re.M).group(1))
        self.assertEqual(steps, [["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File",
                                  "scripts/lib/windows-job.ps1", older, self.tree(older)]], "SPEC windows-job")
        for line in ('os = "windows"', 'kind = "owlrs.any"', f'rev = "{head}"', 'NRESE_MEMORY_CAP_GB = "16"'):
            self.assertIn(line, text, "SPEC windows-job")

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

    def test_a_commit_head_doesnt_hold_is_refused_before_any_job(self):
        self.git(self.repo, "checkout", "-q", "-b", "side")
        side = self.commit("a.txt", "side\n")
        self.git(self.repo, "checkout", "-q", "main")
        self.commit("b.txt", "main\n")
        self.fleet("pending")
        done = self.check(side)
        self.assertEqual((done.returncode != 0, self.submitted(), "HEAD doesn't hold" in done.stderr), (True, [], True),
                         f"SPEC windows-held\n{done.stderr}")


class WindowsJob(Repo):
    """scripts/lib/windows-job.sh, the Windows part's step on its host, run from a copy as
    scripts/lib/windows-job.ps1 runs it, with stand-ins for the tree's scripts and the toolchain."""

    def test_it_checks_out_the_commit_runs_every_step_and_says_what_it_was(self):
        stubs = self.root / "stubs"
        stubs.mkdir()
        (stubs / "cargo").write_text("#!/bin/sh\nexit 1\n", newline="\n")  # no cargo-nextest: cargo test
        # rustup's proxy where STUB_NO_TOOLCHAIN is set: the tree's toolchain isn't installed, and the
        # proxy installs it (STUB_NO_TOOLCHAIN, a file, says so) unless RUSTUP_AUTO_INSTALL=0.
        (stubs / "rustc").write_text(
            '#!/bin/sh\nif [ -n "$STUB_NO_TOOLCHAIN" ]; then\n  [ "$RUSTUP_AUTO_INSTALL" != 0 ] || exit 1\n'
            f'  : > "$STUB_NO_TOOLCHAIN"\nfi\nexec "{(self.fake / "bin" / "rustc").as_posix()}" "$@"\n', newline="\n")
        for name in ("cargo", "rustc"):
            (stubs / name).chmod(0o755)
        # The tree's cargo-guarded.sh: needs a usable stdin, as Python's subprocess and coreutils do,
        # says where it builds, and fails as STUB_FAIL says (<its first argument>:<exit>).
        guarded = ('case "$1" in --print-rustflags) echo "-C target-cpu=native"; exit 0 ;; esac\n'
                   f'"{Path(sys.executable).as_posix()}" -c "import os; os.fstat(0)" || exit 9\n'
                   'printf %s "$CARGO_TARGET_DIR" > "$FLEET_OUT/target-dir"\n'
                   '[ "${STUB_FAIL%%:*}" != "$1" ] || exit "${STUB_FAIL#*:}"\n')
        for rel, body in {"scripts/cargo-guarded.sh": guarded, "scripts/fetch-w3c-tests.sh": "exit 0\n",
                          "benches/suite/tests/isolation.py": "import sys\nsys.exit(0)\n"}.items():
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / rel).write_text(body, newline="\n")
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-q", "-m", "the commit under test")
        commit, tree = self.git(self.repo, "rev-parse", "HEAD"), self.tree("HEAD")
        # The job's checkout, HEAD: that commit and the job's script.
        shutil.copy(ROOT / "scripts/lib/windows-job.sh", self.repo / "scripts/lib/windows-job.sh")
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-q", "-m", "the job's script")
        tool = self.git(self.repo, "rev-parse", "HEAD")
        empty = self.git(self.repo, "hash-object", "/dev/null")
        cases = [  # tree sent, STUB_FAIL, stdin closed, toolchain missing -> exit, the build step's exit, stdin
            (tree, "", False, False, 0, 0, "valid"),
            ("0" * 40, "", False, False, 3, None, None),         # another tree checked out: no verdict
            (tree, "build:101", False, False, 1, 101, "valid"),  # a step fails: the tree fails
            (tree, "build:75", False, False, 3, 75, "valid"),    # the PC guard stopped a build: no verdict
            (tree, "", True, False, 0, 0, "invalid"),            # no usable stdin (fleet I14): said, steps get /dev/null
            (tree, "", False, True, 3, None, None),              # no toolchain for the tree: no verdict, none installed
        ]
        for case, (sent, fail, closed, missing, code, build, stdin) in enumerate(cases):
            self.git(self.repo, "checkout", "-q", "main")
            (self.repo / "tmp" / "windows-job").mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / "scripts/lib/windows-job.sh", self.repo / "tmp/windows-job/windows-job.sh")
            out = self.root / f"out-{case}"
            out.mkdir()
            installed = self.root / f"installed-{case}"
            run = ("exec 0<&-; " if closed else "") + 'exec bash "$@"'
            done = subprocess.run(
                [BASH, "-c", run, "-", "tmp/windows-job/windows-job.sh", commit, sent], cwd=self.repo,
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
                {k: result[k] for k in ("tier", "tree", "list_hash", "code", "tool", "stdin", "tracked_changes", "job",
                                        "fleet_host", "summary")},
                {"tier": "windows", "tree": tree, "list_hash": empty, "code": code, "tool": tool, "stdin": stdin,
                 "tracked_changes": "", "job": f"win-{case}", "fleet_host": "kraris-gptler", "summary": True},
                f"SPEC windows-record: case {case}")
            self.assertEqual([(s["name"], s["code"]) for s in result["steps"]],
                             [("W3C data", 0), ("build", build), ("test", 0), ("doc tests", 0), ("suite contracts", 0)],
                             f"SPEC windows-run: case {case}")
            self.assertEqual(result["attempt"], (out / "attempt").read_text().strip())
            self.assertRegex(result["identity"],
                             r"^rustc-1\.98\.1-abcdef123456 flags=-C_target-cpu=native tools=[0-9a-f]{12}$")
            self.assertEqual(self.git(self.repo, "rev-parse", "HEAD"), commit, "SPEC windows-run: not the commit sent")


if __name__ == "__main__":
    unittest.main()
