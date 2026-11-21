"""The quick check (scripts/check.sh quick; scripts/lib/fleet-gate.sh and gate-job.sh, mode quick): a
commit's mechanical faults found in minutes before its review, on the fleet only. It runs the push
tier's own steps, fewer, on the crates push selects for the same branch; its pass is recorded under
its own tier, by tree, and never opens a push.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_quick_check.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import time
import unittest

import isolation
import test_push_gate as fixture  # the push gate's scratch repository and fleet stand-in (its tests aren't rerun here)

ROOT = fixture.ROOT
BASH = fixture.BASH
ZERO = fixture.ZERO


def step_args(job_text):
    """The gate step's arguments of a submitted job: (mode, tree, files)."""
    steps = json.loads(re.search(r"^steps = (.*)$", job_text, re.M).group(1))[0]
    return steps[2], steps[3], steps[steps.index("--") + 1:]


@unittest.skipUnless(BASH, "the gate is a bash script")
class QuickOnTheFleet(fixture.Repo):
    """check.sh quick by hand, with the push gate's stand-in fleet."""

    def quick(self, **env):
        return subprocess.run([BASH, "scripts/check.sh", "quick"], cwd=self.repo, capture_output=True, text=True,
                              env={**self.env, **env}, timeout=120, stdin=subprocess.DEVNULL)

    def submitted(self):
        return [step_args(path.read_text()) for path in sorted(self.fake.glob("job-*.toml"))]

    def passes(self):
        passed = Path(self.git(self.repo, "rev-parse", "--absolute-git-dir")) / "nrese-gate-passed"
        return passed.read_text().splitlines() if passed.exists() else []

    def test_it_checks_the_branchs_files_as_a_push_of_the_new_branch_does(self):
        # Spec (quick check design, 21 Nov 2026, §2): the files are the branch's against the line, from
        # where it left origin's refactor/engine-v2: a push of the branch new on the remote gates the
        # same files; never only since its upstream (a push by hand of a branch with one: the control).
        first = self.commit("a.txt", "a changed\n")
        tip = self.commit("b.txt", "b changed\n")
        self.fleet("pending")
        self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", ZERO))
        self.git(self.repo, "update-ref", "refs/remotes/origin/pkg/x", first)
        self.git(self.repo, "branch", "-q", "-u", "origin/pkg/x")
        self.quick()
        subprocess.run([BASH, "scripts/check.sh", "push"], cwd=self.repo, capture_output=True, env=self.env,
                       timeout=120, stdin=subprocess.DEVNULL)
        tree = self.tree(tip)
        self.assertEqual(self.submitted(), [("push", tree, ["a.txt", "b.txt"]), ("quick", tree, ["a.txt", "b.txt"]),
                                            ("push", tree, ["b.txt"])],
                         "SPEC quick-branch: quick wasn't told the branch's files against the line")

    def test_it_is_a_quick_job_of_the_gate_kind_and_never_a_merge_gate(self):
        # Spec (design §3, §7): kind owlrs.gate, est_min 10, a 30-minute step deadline; it lands
        # nothing, so no merge-gate class whatever ref the caller names.
        self.commit("a.txt", "a changed\n")
        self.fleet("pending")
        self.quick(NRESE_PUSH_REF="refs/heads/refactor/engine-v2")
        job = (self.fake / "job-0.toml").read_text()
        got = {line for line in job.splitlines() if line.startswith(("kind", "est_min", "step_deadline_min",
                                                                      "priority_class"))}
        self.assertEqual(got, {'kind = "owlrs.gate"', "est_min = 10", "step_deadline_min = 30"}, "SPEC quick-job")

    def test_a_pass_is_its_tiers_by_tree_and_never_opens_a_push(self):
        # Spec (design §3, §6): a verified quick pass is recorded under tier quick, keyed as push's
        # (tree, file list, toolchain), naming the commit first; the same tree and files then pass at
        # once; a push of that commit is still gated on its own, and another tree runs again.
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("done", self.tree(tip), ["a.txt"], tier="quick")
        done = self.quick()
        self.assertEqual(done.returncode, 0, f"SPEC quick-verified: {done.stderr}")
        record = self.passes()
        self.assertEqual(len(record), 1, record)
        self.assertRegex(record[0], rf"^quick {self.tree(tip)} [0-9a-f]{{40}} rustc-\S+ flags=\S* tools=\S+ fleet"
                                    rf" \| {tip} fleet:gate-1@office-pc cpu=znver3$", "SPEC quick-record")
        self.fleet("pending")
        again = self.quick()
        self.assertEqual((again.returncode, len(self.submitted())), (0, 1), f"SPEC quick-verified: {again.stderr}")
        pushed = self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", ZERO))
        self.assertEqual((pushed.returncode != 0, [m for m, _, _ in self.submitted()]), (True, ["quick", "push"]),
                         f"SPEC quick-not-push: a quick pass let a push through\n{pushed.stderr}")
        other = self.commit("b.txt", "b changed\n")
        self.quick()
        self.assertEqual([(m, t) for m, t, _ in self.submitted()][-1], ("quick", self.tree(other)),
                         "SPEC quick-by-tree: another tree passed on the pass")

    def test_it_runs_on_the_fleet_only(self):
        # Spec (design §3; rule: heavy work on the fleet only): no fleet, or no verdict from it, is no
        # pass and exit 75, and nothing runs here instead; a failed verdict is exit 1 and no pass.
        tip = self.commit("a.txt", "a changed\n")
        owner = self.env.pop("FLEET_OWNER")
        unowned = self.quick()
        self.env["FLEET_OWNER"] = owner
        self.fleet("pending")
        waited = self.quick()
        self.fleet("failed", self.tree(tip), ["a.txt"], tier="quick")  # a whole record of a failed clippy
        result = self.fake / "results" / "out" / "gate.json"
        record = json.loads(result.read_text())
        record.update(code=1, steps=[{"name": "clippy", "code": 101, "seconds": 1}])
        result.write_text(json.dumps(record))
        failed = self.quick()
        for done, want in ((unowned, 75), (waited, 75), (failed, 1)):
            self.assertEqual(done.returncode, want, f"SPEC quick-fleet-only: {done.stdout}{done.stderr}")
            self.assertNotIn("=== fmt", done.stdout, "SPEC quick-fleet-only: a step ran here")
        self.assertEqual((len(self.submitted()), self.passes()), (2, []), "SPEC quick-fleet-only")


# The build host's side: check.sh's tiers over a file list, with stand-ins, each call one line in $CALLS.
# A workspace of alpha, beta depending on alpha, and nrese-server (the generated views' crate); alpha
# has probes. HAS_NEXTEST says whether cargo-nextest is there; a build admission call holding FAIL fails.
CARGO = """#!/bin/sh
echo "cargo $*" >> "$CALLS"
case "$1" in
  metadata) echo '{"packages":[{"name":"alpha"},{"name":"beta"},{"name":"nrese-server"}]}' ;;
  nextest) [ -n "${HAS_NEXTEST:-}" ] || exit 1 ;;
  tree) case "$3" in alpha) printf 'alpha v0.1.0\\nbeta v0.1.0\\n' ;; *) printf '%s v0.1.0\\n' "$3" ;; esac ;;
esac
exit 0
"""
RECORD = '#!/bin/sh\necho "{name} $*" >> "$CALLS"\nexit 0\n'
STUBS = {
    "scripts/cargo-guarded.sh": '#!/bin/sh\necho "guarded${NRESE_FUZZ_SEED:+ seed=$NRESE_FUZZ_SEED} $*" >> "$CALLS"\n'
                                'case "$*" in *"${FAIL:-no failure planted}"*) exit 1 ;; esac\nexit 0\n',
    "scripts/fetch-w3c-tests.sh": RECORD.format(name="w3c"),
    "scripts/check-locks.sh": RECORD.format(name="locks"),
    "scripts/check-features.sh": RECORD.format(name="features"),
    "scripts/check-names.sh": RECORD.format(name="names"),
    "scripts/miri.sh": RECORD.format(name="miri"),
    "scripts/check-python-names.py": 'import os, sys\nopen(os.environ["CALLS"], "a")'
                                     '.write("python-names " + " ".join(sys.argv[1:]) + "\\n")\n',
    "scripts/lib/test-targets.py": 'import sys\nprint("-p " + sys.argv[2] + " --lib")\n',
    "scripts/lib/fuzz-targets.txt": "alpha:lib:random\nbeta:lib:random\n",
    "scripts/lib/explorer-targets.txt": "# a listed explorer\nbeta:lib:explore::\n",
    "benches/suite/tests/isolation.py": 'import os\nopen(os.environ["CALLS"], "a").write("suite\\n")\n',
    "crates/alpha/Cargo.toml": '[package]\nname = "alpha"\n',
    "crates/alpha/tests/it/probes/mod.rs": "mod topic;\n",
    "crates/beta/Cargo.toml": '[package]\nname = "beta"\n',
    "crates/nrese-server/Cargo.toml": '[package]\nname = "nrese-server"\n',
}
ALPHA = ["crates/alpha/src/lib.rs"]


@unittest.skipUnless(BASH, "the gate is a bash script")
class QuickTier(unittest.TestCase):
    """check.sh quick and push as a build host runs them, in a scratch repository where origin's
    refactor/engine-v2 is HEAD."""

    @classmethod
    def setUpClass(cls):
        temp = tempfile.TemporaryDirectory(prefix="nrese-quick-tier-")
        cls.addClassCleanup(temp.cleanup)
        cls.root = Path(temp.name)
        tools = cls.root / "bin"
        tools.mkdir()
        (tools / "cargo").write_text(CARGO, newline="\n")
        (tools / "cargo").chmod(0o755)
        gitconfig = cls.root / "gitconfig"
        gitconfig.write_text("[user]\n\tname = t\n\temail = t@t\n[core]\n\tautocrlf = false\n"
                             "[init]\n\tdefaultBranch = main\n")
        cls.env = isolation.inherit(
            *fixture.LEAKS, git={"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": gitconfig.as_posix()},
            PATH=tools.as_posix() + os.pathsep + os.environ.get("PATH", ""), NRESE_ON_BUILD_HOST="1")
        cls.repo = cls.root / "repo"
        subprocess.run(["git", "init", "-q", str(cls.repo)], check=True, capture_output=True, env=cls.env)
        (cls.repo / "scripts").mkdir()
        shutil.copy(ROOT / "scripts/check.sh", cls.repo / "scripts/check.sh")
        for rel, text in STUBS.items():
            (cls.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            (cls.repo / rel).write_text(text, newline="\n")
        (cls.repo / "scripts/cargo-guarded.sh").chmod(0o755)
        for args in (["add", "-A"], ["commit", "-q", "-m", "base"],
                     ["update-ref", "refs/remotes/origin/refactor/engine-v2", "HEAD"]):
            subprocess.run(["git", "-C", str(cls.repo), *args], check=True, capture_output=True, env=cls.env)
        cls.runs = 0

    def tier(self, mode, files, **env):
        """(exit, {step: exit}, the build admission's calls, output) of MODE over FILES."""
        QuickTier.runs += 1
        own = self.root / f"run-{self.runs}"
        own.mkdir()
        (own / "files").write_text("".join(f"{f}\n" for f in files), newline="\n")
        calls = own / "calls"
        done = subprocess.run([BASH, "scripts/check.sh", mode], cwd=self.repo, capture_output=True, text=True,
                              timeout=120, stdin=subprocess.DEVNULL,
                              env={**self.env, "CALLS": calls.as_posix(),
                                   "NRESE_FILES_FROM": (own / "files").as_posix(), **env})
        steps = {name: int(code) for code, name in re.findall(r"(?m)^ +(\d+)  (.+) \(\d+ s\)$", done.stdout)}
        lines = calls.read_text().splitlines() if calls.exists() else []
        return done.returncode, steps, [line for line in lines if line.startswith("guarded ")], \
            done.stdout + done.stderr

    def test_a_changed_crate_gets_the_quick_steps_and_no_more(self):
        # Spec (design §1), worked by hand for a change to alpha (beta depends on it, has the listed
        # explorer target; alpha has probes): fmt, message strings; clippy on alpha, every target;
        # alpha's unit tests; its it binary's probes:: module; nrese-server's generated views; one
        # seed of alpha's random tests; beta's explorer target. Not beta's tests, the W3C data, Miri,
        # the suite contracts.
        fallback = ["guarded clippy --locked -p alpha --all-targets",
                    "guarded test --locked --no-fail-fast -p alpha --lib",
                    "guarded test --locked --no-fail-fast -p alpha --test it -- probes::",
                    "guarded test --locked --no-fail-fast -p nrese-server --test it -- generated_views",
                    "guarded seed=SEED test --locked -q -p alpha --lib -- random",
                    "guarded test --locked -q -p beta --lib -- explore::"]
        nextest = ["guarded clippy --locked -p alpha --all-targets",
                   "guarded nextest run --locked --no-fail-fast -p alpha --lib",
                   "guarded nextest run --locked --no-fail-fast -p alpha -E (binary_id(alpha::it)) & test(probes::)",
                   "guarded nextest run --locked --no-fail-fast -p nrese-server -E (binary_id(nrese-server::it)) "
                   "& test(generated_views)",
                   "guarded seed=SEED nextest run --locked --no-fail-fast -p alpha -E (binary_id(alpha) & test(random))",
                   "guarded nextest run --locked --no-fail-fast -p beta -E (binary_id(beta) & test(explore::))"]
        steps = {"fmt", "message strings", "clippy", "unit tests", "probes", "generated views", "seeds", "explorer"}
        for has, want in (("", fallback), ("1", nextest)):
            with self.subTest(nextest=bool(has)):
                code, got, calls, out = self.tier("quick", ALPHA, HAS_NEXTEST=has)
                self.assertEqual(code, 0, out)
                self.assertEqual([re.sub(r"seed=\d+", "seed=SEED", c) for c in calls], want,
                                 f"SPEC quick-steps: the build admission's calls\n{out}")
                self.assertEqual(set(got), steps, f"SPEC quick-steps: the summary\n{out}")

    def test_its_one_seed_is_the_push_tiers_first(self):
        # Spec (design §1): one seed, the first of push's three, so a failure replays in push.
        seeds = {}
        for mode in ("push", "quick"):
            _, _, calls, out = self.tier(mode, ALPHA)
            seeds[mode] = [re.search(r"seed=(\d+)", c).group(1) for c in calls if "seed=" in c and "-p alpha" in c]
        self.assertEqual(len(seeds["push"]), 3, seeds)
        self.assertEqual(seeds["quick"], seeds["push"][:1], "SPEC quick-seed")

    def test_it_selects_the_crates_push_selects(self):
        # Spec (design §2): changed_crates on the same list, unchanged: a crate's file, one depending on
        # it, a workspace-wide file (every crate), no crate at all.
        for files, want in ((ALPHA, "alpha"), (["crates/beta/src/lib.rs", "docs/a.md"], "beta"),
                            (["Cargo.lock"], "alpha beta nrese-server"), (["docs/a.md"], "none")):
            with self.subTest(files=files):
                lines = [re.search(r"(?m)^=== changed: ([^;\n]*)", self.tier(mode, files)[3]).group(1)
                         for mode in ("push", "quick")]
                self.assertEqual(lines, [want, want], "SPEC quick-same-crates")

    def test_a_crate_without_probes_is_skipped_quietly(self):
        # Spec (design §5): probes run where the crate has a probes module; elsewhere no step at all.
        code, steps, calls, out = self.tier("quick", ["crates/beta/src/lib.rs"])
        self.assertEqual((code, "probes" in steps, any("probes::" in c for c in calls)), (0, False, False), out)

    def test_a_failing_clippy_or_drift_step_fails_the_tier(self):
        # Spec (design §8): a planted clippy finding and a planted drift each fail the tier; the drift
        # tests run whatever changed, a document alone included. Controls: nothing planted, it passes.
        cases = ((ALPHA, None, 0, "clippy"), (ALPHA, "clippy", 1, "clippy"),
                 (ALPHA, None, 0, "generated views"), (ALPHA, "generated_views", 1, "generated views"),
                 (["docs/a.md"], None, 0, "generated views"), (["docs/a.md"], "generated_views", 1, "generated views"))
        for files, plant, want, step in cases:
            with self.subTest(files=files, plant=plant):
                code, steps, _, out = self.tier("quick", files, **({"FAIL": plant} if plant else {}))
                self.assertEqual((code, steps.get(step) == 0), (want, want == 0), f"SPEC quick-fails\n{out}")


@unittest.skipUnless(BASH and shutil.which("flock"), "the gate job runs on Linux fleet hosts (flock)")
class QuickGateJob(fixture.Repo):
    """scripts/lib/gate-job.sh mode quick on its host, with stand-ins for its tools (as test_push_gate's
    GateJob)."""

    def test_it_has_its_own_lock_worktree_and_target_and_sweeps_only_alone(self):
        # Spec (design §3): wt-quick, target-quick-<release>, lock-quick: a quick check never waits
        # behind a push gate's lock; the host's one sweep never runs while the other gate does.
        stubs = self.root / "stubs"
        stubs.mkdir()
        for name, body in {
            "rustup": "exit 0",
            "rustc": '[ "${1#+}" != "$1" ] && shift; case "$1" in -vV) echo "release: 1.98.1"; echo "commit-hash: abcdef1234567890" ;;'
                     ' --print) echo "    native - Select the CPU of the current host (currently znver3)." ;; esac',
            "cargo": 'echo "cargo-nextest 0.9.146"',
            "cc": 'while [ $# -gt 0 ]; do [ "$1" = -o ] && { shift; : > "$1"; }; shift; done',
        }.items():
            (stubs / name).write_text("#!/bin/sh\n" + body + "\n")
            (stubs / name).chmod(0o755)
        cache, seen = self.root / "cache", self.root / "seen"
        check = ('echo "target=$CARGO_TARGET_DIR" > "$SEEN"\necho "wt=$PWD" >> "$SEEN"\n'
                 'printf "=== summary\\n  0  fmt (1 s)\\n"\n')
        for rel, body in {"scripts/check.sh": check, "scripts/cargo-guarded.sh": 'echo "-C target-cpu=native"\n',
                          "rust-toolchain.toml": '[toolchain]\nchannel = "1.98.1"\n',
                          "benches/suite/requirements.txt": ""}.items():
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / rel).write_text(body)
        empty = self.git(self.repo, "hash-object", "/dev/null")
        python = cache / f"py-{empty[:12]}" / "bin"
        python.mkdir(parents=True)
        (python / "python").write_text('#!/bin/sh\nexec python3 "$@"\n')
        (python / "python").chmod(0o755)
        shutil.copy(ROOT / "scripts/lib/gate-job.sh", self.repo / "scripts/lib/gate-job.sh")
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-q", "-m", "two")
        tree = self.tree("HEAD")
        old = cache / "target-old"  # another toolchain's, unused for long: the sweep's to remove
        old.mkdir()
        (old / ".last-used").write_text("0")
        # A push gate running: the push lock held until its stdin closes.
        holder = subprocess.Popen(["flock", str(cache / "lock"), "cat"], stdin=subprocess.PIPE,
                                  stdout=subprocess.DEVNULL)
        self.addCleanup(holder.kill)
        while subprocess.run(["flock", "-n", str(cache / "lock"), "true"]).returncode == 0:
            time.sleep(0.05)

        def job(n):
            out = self.root / f"out-{n}"
            out.mkdir()
            return subprocess.run(
                [BASH, "scripts/lib/gate-job.sh", "quick", tree, empty, "--"], cwd=self.repo, capture_output=True,
                text=True, timeout=60,
                env={**self.env, "FLEET_OUT": str(out), "NRESE_GATE_CACHE": str(cache), "SEEN": str(seen),
                     "HOME": str(self.root), "PATH": f"{stubs}:{os.environ['PATH']}", "NRESE_GATE_CC": "cc",
                     "FLEET_JOB": f"gate-{n}", "FLEET_STEP": "1", "FLEET_HOST": "office-pc"})

        beside = job(1)
        self.assertEqual(beside.returncode, 0, f"SPEC quick-own-lock\n{beside.stdout}{beside.stderr}")
        said = dict(line.split("=", 1) for line in seen.read_text().split())
        self.assertEqual((Path(said["target"]), Path(said["wt"]).resolve()),
                         (cache / "target-quick-1.98.1", (cache / "wt-quick").resolve()), "SPEC quick-own-cache")
        self.assertTrue(old.exists(), "SPEC quick-sweep-alone: swept while the push gate ran")
        holder.stdin.close()
        holder.wait(timeout=30)
        alone = job(2)
        self.assertEqual((alone.returncode, old.exists()), (0, False),
                         f"SPEC quick-sweep-alone: no sweep with the host to itself\n{alone.stdout}")


if __name__ == "__main__":
    unittest.main()
