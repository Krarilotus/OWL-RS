"""The quick check (scripts/check.sh quick; scripts/lib/fleet-gate.sh and gate-job.sh, mode quick): a
commit's mechanical faults found in minutes before its review, on the fleet only. It runs the push
tier's own steps, fewer, on the crates push selects for the same branch; its pass is recorded under
its own tier, by tree, and never opens a push.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_quick_check.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

from fnmatch import fnmatch
import json
import os
from pathlib import Path
import re
import shutil
import signal
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
        # (tree, file list, toolchain), naming the commit first; a push of that commit is still gated on
        # its own; the same tree and files then pass at once, and another tree runs again.
        tip = self.commit("a.txt", "a changed\n")
        self.fleet("done", self.tree(tip), ["a.txt"], tier="quick")
        host = json.loads((self.fake / "results" / "out" / "gate.json").read_text())["fleet_host"]
        done = self.quick()
        self.assertEqual(done.returncode, 0, f"SPEC quick-verified: {done.stderr}")
        pushed = self.push(("refs/heads/pkg/x", tip, "refs/heads/pkg/x", ZERO))
        self.assertEqual((pushed.returncode != 0, [m for m, _, _ in self.submitted()]), (True, ["quick", "push"]),
                         f"SPEC quick-not-push: a quick pass let a push through\n{pushed.stderr}")
        record = self.passes()
        self.assertEqual(len(record), 1, record)
        self.assertRegex(record[0], rf"^quick {self.tree(tip)} [0-9a-f]{{40}} rustc-\S+ flags=\S* tools=\S+ fleet"
                                    rf" \| {tip} fleet:gate-1@{re.escape(host)} cpu=znver3$", "SPEC quick-record")
        self.fleet("pending")
        again = self.quick()
        self.assertEqual((again.returncode, len(self.submitted())), (0, 2), f"SPEC quick-verified: {again.stderr}")
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

    def test_a_commit_whose_runner_has_no_quick_mode_is_refused_before_any_job(self):
        # Spec (review r1, 1): the job runs the commit's own gate-job.sh; one from before quick's own
        # entries (under the cache's quick/) would run push's, or leave quick's where older sweeps remove
        # them. Such a commit, named (NRESE_PUSH_COMMIT) or HEAD, is refused: exit 2, nothing submitted.
        current = (self.repo / "scripts/lib/gate-job.sh").read_text()
        self.commit("a.txt", "a changed\n")
        old = self.commit("scripts/lib/gate-job.sh", "#!/usr/bin/env bash\n# a gate job from before quick/\n")
        self.commit("scripts/lib/gate-job.sh", current)
        self.fleet("pending")
        named = self.quick(NRESE_PUSH_COMMIT=old)
        self.assertEqual((named.returncode, "has no quick check" in named.stderr, self.submitted()), (2, True, []),
                         f"SPEC quick-runner: {named.stdout}{named.stderr}")
        head = self.quick()  # the control: HEAD's runner has the quick mode
        self.assertEqual((head.returncode, len(self.submitted())), (75, 1), f"SPEC quick-runner: {head.stderr}")


# The build host's side: check.sh's tiers over a file list, with stand-ins, each call one line in $CALLS.
# A workspace of alpha, beta depending on alpha, gamma nested under crates/rdf/, and nrese-server (the
# generated views' crate); alpha and gamma have probes. HAS_NEXTEST says whether cargo-nextest is there;
# TREE_FAILS makes cargo tree fail (1: for every crate, else for the one it names); a build admission call
# holding FAIL fails, one holding EMPTY selects no test (nextest then fails, 4, unless told
# --no-tests=pass; cargo test passes, 0 passed).
CARGO = """#!/bin/sh
echo "cargo $*" >> "$CALLS"
case "$1" in
  metadata) echo '{"packages":[{"name":"alpha"},{"name":"beta"},{"name":"gamma"},{"name":"nrese-server"}]}' ;;
  nextest) [ -n "${HAS_NEXTEST:-}" ] || exit 1 ;;
  tree) case "${TREE_FAILS:-}" in 1|"$3") exit 101 ;; esac
    case "$3" in alpha) printf 'alpha v0.1.0\\nbeta v0.1.0\\n' ;; *) printf '%s v0.1.0\\n' "$3" ;; esac ;;
esac
exit 0
"""
GUARDED = """#!/bin/sh
echo "guarded${NRESE_FUZZ_SEED:+ seed=$NRESE_FUZZ_SEED} $*" >> "$CALLS"
case "$*" in *"${FAIL:-no failure planted}"*) exit 1 ;; esac
case "$*" in
  *"${EMPTY:-no empty selection}"*)
    case "$*" in nextest*--no-tests=pass*) ;; nextest*) exit 4 ;; *) echo "test result: ok. 0 passed; 0 failed" ;; esac ;;
  test*) echo "test result: ok. 1 passed; 0 failed" ;;
esac
exit 0
"""
RECORD = '#!/bin/sh\necho "{name} $*" >> "$CALLS"\nexit 0\n'
STUBS = {
    "scripts/cargo-guarded.sh": GUARDED,
    "scripts/fetch-w3c-tests.sh": RECORD.format(name="w3c"),
    "scripts/check-locks.sh": RECORD.format(name="locks"),
    "scripts/check-features.sh": RECORD.format(name="features"),
    "scripts/check-names.sh": RECORD.format(name="names"),
    "scripts/check-effect.sh": RECORD.format(name="effect"),
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
    "crates/rdf/gamma/Cargo.toml": '[package]\nname = "gamma"\n',
    "crates/rdf/gamma/tests/it/probes.rs": "\n",
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
        # the suite contracts (no gate file changed).
        fallback = ["guarded clippy --locked -p alpha --all-targets",
                    "guarded test --locked --no-fail-fast -p alpha --lib",
                    "guarded test --locked --no-fail-fast -p alpha --test it -- probes::",
                    "guarded test --locked --no-fail-fast -p nrese-server --test it -- generated_views",
                    "guarded seed=SEED test --locked -q -p alpha --lib -- random",
                    "guarded test --locked -q -p beta --lib -- explore::"]
        nextest = ["guarded clippy --locked -p alpha --all-targets",
                   "guarded nextest run --locked --no-fail-fast --no-tests=pass -p alpha --lib",
                   "guarded nextest run --locked --no-fail-fast --no-tests=pass -p alpha -E (binary_id(alpha::it)) "
                   "& test(probes::)",
                   "guarded nextest run --locked --no-fail-fast --no-tests=fail -p nrese-server "
                   "-E (binary_id(nrese-server::it)) & test(generated_views)",
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

    def test_a_change_to_the_gates_own_files_gets_the_suite_contracts_as_in_push(self):
        # Spec (check.sh suite_contracts, one rule for both tiers): a change under scripts/,
        # benches/suite/ or .githooks/ runs the suite contracts in quick as in push. The cases are the
        # gate files of two changes whose contracts failed only in the push gate (21 Nov 2026): the
        # effect check added to the guards, its stand-in missing from this file's fixture; the
        # runner check that took a silent `cargo nextest --version` for no runner. And each of the other
        # two folders alone: a contract, a hook.
        cases = {"effect check": ["scripts/check-effect.sh", "scripts/check.sh"],
                 "runner check": ["benches/suite/tests/test_push_gate.py",
                                  "benches/suite/tests/test_push_tier_monotone.py", "scripts/check.sh",
                                  "scripts/lib/gate-tools.txt", "scripts/lib/windows-job.sh"],
                 "a contract": ["benches/suite/tests/test_push_gate.py"], "a hook": [".githooks/pre-push"]}
        for name, files in cases.items():
            for mode in ("quick", "push"):
                with self.subTest(case=name, mode=mode):
                    code, steps, _, out = self.tier(mode, files)
                    self.assertEqual((code, steps.get("suite contracts")), (0, 0),
                                     f"SPEC quick-suite-contracts\n{out}")

    def test_its_one_seed_is_the_push_tiers_first_whatever_the_environment_says(self):
        # Spec (design §1; review r1, 6): one seed, the first of push's three, so a failure replays in
        # push; each tier's count is its own, never the caller's NRESE_SEEDS.
        seeds = {}
        for mode, inherited in (("push", "1"), ("quick", "3")):
            _, _, calls, out = self.tier(mode, ALPHA, NRESE_SEEDS=inherited)
            seeds[mode] = [re.search(r"seed=(\d+)", c).group(1) for c in calls if "seed=" in c and "-p alpha" in c]
        self.assertEqual(len(seeds["push"]), 3, f"SPEC push-seeds: {seeds}")
        self.assertEqual(seeds["quick"], seeds["push"][:1], "SPEC quick-seed")

    def test_it_selects_the_crates_push_selects(self):
        # Spec (design §2): changed_crates on the same list, unchanged: a crate's file, one depending on
        # it, a workspace-wide file (every crate), no crate at all.
        for files, want in ((ALPHA, "alpha"), (["crates/beta/src/lib.rs", "docs/a.md"], "beta"),
                            (["Cargo.lock"], "alpha beta gamma nrese-server"), (["docs/a.md"], "none")):
            with self.subTest(files=files):
                lines = []
                for mode in ("push", "quick"):
                    said = re.search(r"(?m)^=== changed: ([^;\n]*)", self.tier(mode, files)[3])
                    lines.append(said.group(1) if said else "no changed: line")
                self.assertEqual(lines, [want, want], "SPEC quick-same-crates")

    def test_probes_run_where_a_crate_has_them_at_any_depth_and_nowhere_else(self):
        # Spec (design §5; review r1, 4): probes run where the changed crate has a probes module, a crate
        # nested under crates/ (crates/rdf/) included; elsewhere no step at all.
        for files, crate in ((["crates/rdf/gamma/src/lib.rs"], "gamma"), (["crates/beta/src/lib.rs"], None)):
            with self.subTest(crate=crate):
                code, steps, calls, out = self.tier("quick", files)
                ran = [c for c in calls if "probes::" in c]
                want = [f"guarded test --locked --no-fail-fast -p {crate} --test it -- probes::"] if crate else []
                self.assertEqual((code, steps.get("probes"), ran), (0, 0 if crate else None, want),
                                 f"SPEC quick-probes\n{out}")

    def test_a_failing_clippy_or_drift_step_fails_the_tier(self):
        # Spec (design §8): a planted clippy finding and a planted drift each fail the tier at that step;
        # the drift tests run whatever changed, a document alone included. Controls: nothing planted, it
        # passes.
        cases = ((ALPHA, None, 0, "clippy"), (ALPHA, "clippy", 1, "clippy"),
                 (ALPHA, None, 0, "generated views"), (ALPHA, "generated_views", 1, "generated views"),
                 (["docs/a.md"], None, 0, "generated views"), (["docs/a.md"], "generated_views", 1, "generated views"))
        for files, plant, want, step in cases:
            with self.subTest(files=files, plant=plant):
                code, steps, _, out = self.tier("quick", files, **({"FAIL": plant} if plant else {}))
                self.assertIn(step, steps, f"SPEC quick-fails: no {step} step\n{out}")
                self.assertEqual((code, steps[step] != 0), (want, want != 0), f"SPEC quick-fails\n{out}")

    def test_no_unit_tests_or_probes_pass_and_no_drift_test_fails_under_both_runners(self):
        # Spec (review r1, 3): a changed crate with no unit tests, or a probes module with none, passes
        # those steps; the generated views' test gone fails its step; the same under nextest and cargo test.
        cases = (("-p alpha --lib", 0, "unit tests"), ("probes::", 0, "probes"),
                 ("generated_views", 1, "generated views"))
        for has in ("", "1"):
            for empty, want, step in cases:
                with self.subTest(nextest=bool(has), empty=empty):
                    code, steps, _, out = self.tier("quick", ALPHA, HAS_NEXTEST=has, EMPTY=empty)
                    self.assertEqual((code, step in steps, steps.get(step) == 0), (want, True, want == 0),
                                     f"SPEC quick-empty\n{out}")

    def test_targets_that_cant_be_told_fail_their_step(self):
        # Spec (review r1, 5): a failed lookup of the dependents and a target list that can't be read
        # fail their step, never select nothing; a readable list naming none of the crates passes.
        code, steps, _, out = self.tier("quick", ALPHA, TREE_FAILS="1")
        self.assertEqual((code, steps.get("explorer", 0) != 0), (1, True), f"SPEC quick-discovery: cargo tree\n{out}")
        # Review r2, 3: a list that is there but can't be read (a directory in its place) as one that is gone.
        for rel, step in (("scripts/lib/explorer-targets.txt", "explorer"), ("scripts/lib/fuzz-targets.txt", "seeds")):
            for how in ("missing", "a directory"):
                with self.subTest(list=rel, how=how):
                    path = self.repo / rel
                    path.rename(path.with_name("away"))
                    try:
                        if how == "a directory":
                            path.mkdir()
                            (path / "entry").write_text(path.with_name("away").read_text())
                        code, steps, _, out = self.tier("quick", ALPHA)
                    finally:
                        if path.is_dir():
                            shutil.rmtree(path)
                        path.with_name("away").rename(path)
                    self.assertEqual((code, steps.get(step, 0) != 0), (1, True),
                                     f"SPEC quick-discovery: {rel} {how}\n{out}")
        code, steps, _, out = self.tier("quick", ["crates/nrese-server/src/lib.rs"])
        self.assertEqual((code, steps.get("seeds"), "explorer" in steps), (0, 0, False),
                         f"SPEC quick-discovery: lists naming no changed crate\n{out}")

    def test_a_lookup_failing_for_one_crate_fails_quick_and_leaves_push_the_dependents_found(self):
        # Spec (review r2, 2): alpha's lookup finds beta, gamma's fails. Push runs the dependents found (beta),
        # as before quick; quick fails its explorer step on the failed lookup.
        files = ALPHA + ["crates/rdf/gamma/src/lib.rs"]
        pushed = self.tier("push", files, TREE_FAILS="gamma")[3]
        said = re.search(r"(?m)^=== changed: .*; depending on them: (.*)$", pushed)
        code, steps, _, out = self.tier("quick", files, TREE_FAILS="gamma")
        self.assertEqual((said and said.group(1), code, steps.get("explorer", 0) != 0), ("beta", 1, True),
                         f"SPEC dependents-partial\n{pushed}\n{out}")


# The sweeps of every runner before this one, push's and /verify's: these entries at the cache's root
# (and wt/target).
OLDER_SWEEPS = ("bin-*", "target-*", "py-*", "node-*")


def real_cargo():
    """The toolchain's own cargo binary (rustup's proxy needs the host's home, which the jobs here
    don't have), or None where there is no Cargo."""
    if not shutil.which("cargo") or not shutil.which("rustc"):
        return None
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], cwd=ROOT, capture_output=True, text=True,
                             timeout=60, env={**os.environ, "RUSTUP_AUTO_INSTALL": "0"})
    cargo = Path(sysroot.stdout.strip()) / "bin" / "cargo"
    return cargo if sysroot.returncode == 0 and cargo.is_file() else None


@unittest.skipUnless(BASH and shutil.which("flock"), "the gate job runs on Linux fleet hosts (flock)")
class QuickGateJob(fixture.Repo):
    """scripts/lib/gate-job.sh mode quick on its host, with stand-ins for its tools (as test_push_gate's
    GateJob). Each job runs in a process group of its own, stopped with its tree at the test's end."""

    HOST = "host-a"

    def setUp(self):
        super().setUp()
        stubs = self.root / "stubs"
        stubs.mkdir()
        for name, body in {
            "rustup": "exit 0",
            "rustc": '[ "${1#+}" != "$1" ] && shift; case "$1" in -vV) echo "release: 1.98.1"; echo "commit-hash: abcdef1234567890" ;;'
                     ' --print) echo "    native - Select the CPU of the current host (currently znver3)." ;; esac',
            "cargo": '[ "$1" = nextest ] && exec cargo-nextest "$@"; exit 0',
            "curl": fixture.CURL_NEXTEST,
            "cc": 'while [ $# -gt 0 ]; do [ "$1" = -o ] && { shift; : > "$1"; }; shift; done',
        }.items():
            (stubs / name).write_text("#!/bin/sh\n" + body + "\n")
            (stubs / name).chmod(0o755)
        self.stubs, self.cache, self.seen = stubs, self.root / "cache", self.root / "seen"
        # check.sh: where it builds and runs, its files and Python; held (HOLD) until told to go, at most
        # 30 s; then the cargo-nextest it runs.
        check = ('s="$SEEN-$1"\n'
                 'printf "target=%s\\nwt=%s\\nfiles=%s\\npython=%s\\n" "$CARGO_TARGET_DIR" "$PWD" "$NRESE_FILES_FROM"'
                 ' "$(command -v python)" > "$s"\n'
                 'if [ -n "${HOLD:-}" ]; then : > "$HOLD.ready"; n=0\n'
                 '  while [ ! -e "$HOLD.go" ] && [ $n -lt 600 ]; do sleep 0.05; n=$((n + 1)); done; fi\n'
                 'echo "nextest=$(cargo nextest --version | cut -d" " -f2)" >> "$s"\n'
                 'printf "=== summary\\n  0  fmt (1 s)\\n"\n')
        for rel, body in {"scripts/check.sh": check, "scripts/cargo-guarded.sh": 'echo "-C target-cpu=native"\n',
                          "rust-toolchain.toml": '[toolchain]\nchannel = "1.98.1"\n',
                          "benches/suite/requirements.txt": ""}.items():
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / rel).write_text(body)
        self.empty = self.git(self.repo, "hash-object", "/dev/null")
        for home in (self.cache, self.cache / "quick"):  # the suite's Python, made already in each mode's place
            python = home / f"py-{self.empty[:12]}" / "bin"
            python.mkdir(parents=True)
            (python / "python").write_text('#!/bin/sh\nexec python3 "$@"\n')
            (python / "python").chmod(0o755)
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-q", "-m", "two")
        self.sent = self.tree("HEAD")
        self.jobs = 0

    def start(self, mode, **env):
        """gate-job.sh MODE, started in a process group of its own."""
        self.jobs += 1
        out = self.root / f"out-{self.jobs}"
        out.mkdir()
        log = open(self.root / f"job-{self.jobs}.log", "w")
        job = subprocess.Popen(
            [BASH, "scripts/lib/gate-job.sh", mode, self.sent, self.empty, "--"], cwd=self.repo, stdout=log,
            stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True,
            env={**self.env, "FLEET_OUT": str(out), "NRESE_GATE_CACHE": str(self.cache), "SEEN": str(self.seen),
                 "HOME": str(self.root), "PATH": f"{self.stubs}:{os.environ['PATH']}", "NRESE_GATE_CC": "cc",
                 "FLEET_JOB": f"gate-{self.jobs}", "FLEET_STEP": "1", "FLEET_HOST": self.HOST, **env})
        self.addCleanup(self.stop, job, log)
        return job

    @staticmethod
    def stop(job, log):
        if job.poll() is None:
            os.killpg(job.pid, signal.SIGKILL)
        job.wait(timeout=30)
        log.close()

    @staticmethod
    def ended(job, seconds=30):
        """The job's exit, or None if it is still running after SECONDS."""
        try:
            return job.wait(timeout=seconds)
        except subprocess.TimeoutExpired:
            return None

    def said(self, mode):
        path = Path(f"{self.seen}-{mode}")
        return dict(line.split("=", 1) for line in path.read_text().splitlines()) if path.exists() else {}

    def run_job(self, mode, **env):
        job = self.start(mode, **env)
        code = self.ended(job)
        self.assertEqual(code, 0, f"{mode} job\n{(self.root / f'job-{self.jobs}.log').read_text()}")

    @staticmethod
    def appeared(path, seconds=30):
        deadline = time.monotonic() + seconds
        while not path.exists() and time.monotonic() < deadline:
            time.sleep(0.05)
        return path.exists()

    def test_it_runs_and_ends_while_a_push_gate_holds_its_lock(self):
        # Spec (design §3): a quick check never waits behind a push gate's lock.
        holder = subprocess.Popen(["flock", str(self.cache / "lock"), "cat"], stdin=subprocess.PIPE,
                                  stdout=subprocess.DEVNULL, start_new_session=True)

        def release():
            holder.stdin.close()
            try:
                holder.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(holder.pid, signal.SIGKILL)
                holder.wait(timeout=10)
        self.addCleanup(release)
        deadline = time.monotonic() + 10
        while subprocess.run(["flock", "-n", str(self.cache / "lock"), "true"]).returncode == 0:
            self.assertLess(time.monotonic(), deadline, "the stand-in push gate didn't take its lock")
            time.sleep(0.05)
        job = self.start("quick")
        code = self.ended(job)
        self.assertEqual((code, "target" in self.said("quick"), holder.poll()), (0, True, None),
                         f"SPEC quick-own-lock: quick didn't run its check and end while the push lock was held\n"
                         f"{(self.root / 'job-1.log').read_text()}")

    def test_its_entries_are_out_of_every_other_sweep_and_its_sweep_leaves_theirs(self):
        # Spec (design §3; review r1, 1): what quick uses (target, worktree, files, Python) is in quick/,
        # which no older runner's or /verify's sweep matches, nor push's now: an over-budget push sweep
        # leaves it; quick's sweep removes its own stale entries only, never push's.
        self.run_job("quick")
        used = {k: Path(v).resolve() for k, v in self.said("quick").items() if k != "nextest"}
        cache = self.cache.resolve()
        matched = [k for k, p in used.items() if p.relative_to(cache).parts[0] != "quick"
                   or any(fnmatch(p.relative_to(cache).parts[0], g) for g in OLDER_SWEEPS)
                   or p.relative_to(cache).parts[:2] == ("wt", "target")]
        self.assertEqual(matched, [], f"SPEC quick-out-of-older-sweeps: {used}")
        stale = self.cache / "target-old", self.cache / "quick" / "target-old"
        for old in stale:
            old.mkdir()
            (old / ".last-used").write_text("0")
        self.run_job("push", NRESE_HOST_CACHE_GB="0")
        self.assertEqual(([k for k, p in used.items() if not p.exists()], stale[0].exists()), ([], False),
                         "SPEC quick-out-of-older-sweeps: push's sweep, over budget")
        pushed = [Path(p) for k, p in self.said("push").items() if k in ("target", "python")]
        self.run_job("quick", NRESE_HOST_CACHE_GB="0")
        self.assertEqual(([p for p in pushed if not p.exists()], stale[1].exists()), ([], False),
                         "SPEC quick-sweeps-its-own: quick's sweep, over budget")

    def test_each_job_runs_the_nextest_its_checkout_pins(self):
        # Spec (review r1, 2): two jobs pinning different cargo-nextest versions, the second installing
        # its own while the first runs: each runs its own version.
        pins = self.repo / "scripts/lib/gate-tools.txt"
        pins.write_text("cargo-nextest 0.9.1\n")
        hold = self.root / "hold"
        first = self.start("quick", HOLD=str(hold))
        self.assertTrue(self.appeared(Path(f"{hold}.ready")), "the quick job didn't reach its check")
        pins.write_text("cargo-nextest 0.9.2\n")
        second = self.start("push")
        pushed = self.ended(second)
        Path(f"{hold}.go").touch()
        quick = self.ended(first)
        self.assertEqual((quick, self.said("quick").get("nextest"), pushed, self.said("push").get("nextest")),
                         (0, "0.9.1", 0, "0.9.2"), "SPEC tools-per-version\n"
                         f"{(self.root / 'job-1.log').read_text()}{(self.root / 'job-2.log').read_text()}")

    def test_a_job_whose_cargo_runs_another_nextest_than_the_pinned_one_has_no_verdict(self):
        # Spec (review r2, 1; r3, 1): real Cargo runs `cargo nextest` from $CARGO_HOME/bin before PATH. With
        # another version there, or the pinned one's text from a version command that fails (check.sh
        # would fall back to cargo test), the job ends with no verdict (3) before its check; with the
        # pinned one it runs it.
        cargo = real_cargo()
        if cargo is None:
            self.skipTest("no Cargo here")
        (self.stubs / "cargo").write_text(f'#!/bin/sh\n[ "$1" = nextest ] && exec "{cargo}" "$@"\nexit 0\n')
        (self.repo / "scripts/lib/gate-tools.txt").write_text("cargo-nextest 0.9.2\n")
        got = {}
        for case, version, status in (("other", "0.9.1", 0), ("failing", "0.9.2", 1), ("pinned", "0.9.2", 0)):
            home = self.root / f"cargo-home-{case}"
            (home / "bin").mkdir(parents=True)
            (home / "bin" / "cargo-nextest").write_text(f'#!/bin/sh\necho "cargo-nextest {version}"\nexit {status}\n')
            (home / "bin" / "cargo-nextest").chmod(0o755)
            Path(f"{self.seen}-quick").unlink(missing_ok=True)
            job = self.start("quick", CARGO_HOME=str(home))
            got[case] = (self.ended(job), self.said("quick").get("nextest"))
        self.assertEqual(got, {"other": (3, None), "failing": (3, None), "pinned": (0, "0.9.2")},
                         f"SPEC cargo-runs-the-pinned-nextest ({cargo})\n"
                         + "".join((self.root / f"job-{n}.log").read_text() for n in (1, 2, 3)))


if __name__ == "__main__":
    unittest.main()
