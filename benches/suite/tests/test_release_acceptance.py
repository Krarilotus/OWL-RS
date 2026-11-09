"""Release acceptance (benches/fast/release_gate.py verdict, w3c, guards; scripts/release-gate.sh):
the gate passes only the exact candidate, with every required outcome present and only approved
exceptions (Harness notes 2026-11-09-g1-release-acceptance-design.md).

The audit of 8 Nov 2026 (tests-tooling T01-T03; catalogue D16-130, D16-126, D16-125) found the
final join pass a candidate it must refuse, each reproduced on engine-v2 3a4e0a21e:
- checks of another candidate, or naming none, joined to a candidate's timed parts (D16-130);
- a launched W3C kit that died without a libtest summary left no trace in the verdict (D16-126);
- a wrong answer the case catalogue declares as known passed without any owner exception, and the
  verdict didn't name it (D16-125).
"""
from __future__ import annotations

import contextlib
import copy
import ctypes
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

import isolation
import test_release_gate as fixtures  # the gate's pass and round fixtures (a module: its tests aren't rerun here)

ROOT = Path(__file__).resolve().parents[3]
GATE = ROOT / "benches/fast/release_gate.py"
gate_module = fixtures.gate_module
POLICY = "benches/release-tradeoffs.toml"
# What the candidate's tree requires of its checks (release_gate.py load_contract): this tree's, in
# every candidate a test makes.
CONTRACT = ("benches/fast/w3c-expected.toml", "docs/design/performance.md")
IDENTITY = "rustc-1.98.1-0123456789ab flags=-C_target-cpu=native tools=abcdef012345"
ATTEMPT = "a" * 32  # a check's attempt (release_gate.py attempt), in its result, receipt and attempt record
QUOTE = 'quote = "accept it for v1.5"\nsource = "OWL-RS master thread, 9 Nov 2026 10:11"\n'


def git(repo: Path, *args: str) -> str:
    return subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, check=True,
                          timeout=60, env=isolation.inherit()).stdout.strip()


class Repository:
    """A repository of the test's own: commits whose trees differ only by their policy file, each
    holding this tree's contract (CONTRACT)."""

    def __init__(self, path: Path):
        self.path = path
        path.mkdir(parents=True, exist_ok=True)
        git(path, "init", "-q")

    def commit(self, policy: str = "", message: str = "a candidate", files: dict[str, str] | None = None) -> str:
        contract = {rel: (ROOT / rel).read_text(encoding="utf-8") for rel in CONTRACT}
        for rel, text in {**contract, POLICY: policy, **(files or {})}.items():
            (self.path / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.path / rel).write_text(text, encoding="utf-8", newline="\n")
        git(self.path, "add", "-A")
        git(self.path, "commit", "-q", "--allow-empty", "-m", message)
        return git(self.path, "rev-parse", "HEAD")

    def tree(self, rev: str) -> str:
        return git(self.path, "rev-parse", f"{rev}^{{tree}}")

    def same_tree(self, rev: str) -> str:
        """Another commit of REV's tree (a merge commit that changed nothing, say)."""
        return git(self.path, "commit-tree", self.tree(rev), "-p", rev, "-m", "the same tree")


def contract():
    """This tree's contract: the declared kits and suites, and the §0 guards."""
    suites, kits, bad = gate_module.load_w3c_expected((ROOT / CONTRACT[0]).read_text(encoding="utf-8"))
    assert not bad, bad
    return suites, kits, gate_module.guard_names((ROOT / CONTRACT[1]).read_text(encoding="utf-8"))


def receipt(kind: str, commit: str, tree: str, **changes) -> dict:
    """A check's receipt as release_gate.py w3c/guards --run write it: every unit the contract
    requires launched once and run whole."""
    units = [kit.name for kit in contract()[1]] if kind == "w3c" else ["guards"]
    # The attempt where the receipt type holds one: on the code before attempts (the red of fix check r2)
    # the join then reads its own receipts, and shows what it accepts, not a receipt it can't read.
    attempt = {"attempt": ATTEMPT} if "attempt" in gate_module.CheckReceipt._fields else {}
    return {"kind": kind, "commit": commit, "tree": tree, "dirty": False, "identity": IDENTITY,
            "host": "office-pc", "job": "rg-cand-checks", **attempt,
            "outcomes": [{"unit": unit, "exit": 0, "log_sha256": "0" * 64, "summary": True} for unit in units],
            **changes}


def checks(commit: str, tree: str) -> tuple[dict, dict]:
    """Passed W3C and guard results as the contract requires them (every kit, every suite's counts,
    every guard ok), each with its receipt for COMMIT at TREE."""
    suites, _, guards = contract()
    counts = {suite: {**want, "failed": 0} for suite, want in suites.items()}
    counts["test results"] = {"passed": len(suites), "failed": 0}
    return ({"counts": counts, "expected": suites, "errors": [], "passed": True, "attempt": ATTEMPT,
             "receipt": receipt("w3c", commit, tree)},
            {"guards": len(guards), "results": {name: ["ok"] for name in guards}, "errors": [], "passed": True,
             "attempt": ATTEMPT, "receipt": receipt("guards", commit, tree)})


def with_attempt(result: dict, attempt: str) -> dict:
    """RESULT as attempt ATTEMPT's: its own field, and its receipt's where it holds one (receipt())."""
    result["attempt"] = attempt
    if "attempt" in result["receipt"]:
        result["receipt"]["attempt"] = attempt
    return result


def attempt_record(kind: str, attempt: str, tree: str) -> dict:
    """KIND's attempt record as `release_gate.py attempt` writes it (KIND.attempt.json beside its result)."""
    return {"kind": kind, "attempt": attempt, "tree": tree}


def rounds(candidate: str, records_of=None) -> list[dict]:
    """Two timed rounds of one part whose candidate side is CANDIDATE: a gate's first run and its
    confirmation (a verdict decides over both)."""
    out = []
    for k in (0, 1):
        summary = fixtures.run_gate(fixtures.passes(records_of), context={**fixtures.CONTEXT, "round": k})
        summary["candidate"]["revision"] = candidate
        out.append(summary)
    return out


def verdict(repo: Path, candidate: str, timed: list[dict], w3c: dict, guards: dict,
            attempts: dict | None = None) -> subprocess.CompletedProcess:
    """`release_gate.py verdict` on these parts, CANDIDATE resolved in REPO; each check's attempt record
    beside its result: the one its result names (ATTEMPTS: others, or None for none)."""
    tmp = Path(tempfile.mkdtemp(prefix="acceptance-", dir=isolation.SCRATCH))
    tree = git(repo, "rev-parse", f"{candidate}^{{tree}}")
    for kind, result in (("w3c", w3c), ("guards", guards)):
        record = (attempts or {}).get(kind, attempt_record(kind, str(result.get("attempt")), tree))
        if record is not None:
            (tmp / f"{kind}.attempt.json").write_text(json.dumps(record), encoding="utf-8")
    try:
        return verdict_on(repo, candidate, tmp, timed, w3c, guards)
    finally:
        shutil.rmtree(tmp, True)


def verdict_on(repo: Path, candidate: str, where: Path, timed: list[dict], w3c: dict | Path,
               guards: dict | Path) -> subprocess.CompletedProcess:
    """`release_gate.py verdict` with the timed parts and each check written into WHERE (a check given as a
    path: that file, where its attempt record lies beside it)."""
    files = []
    for i, part in enumerate([*timed, w3c, guards]):
        if isinstance(part, Path):
            files.append(part)
            continue
        files.append(where / ("w3c.json" if i == len(timed) else "guards.json" if i > len(timed) else f"part-{i}.json"))
        files[-1].write_text(json.dumps(part), encoding="utf-8")
    args = [a for f in files[:-2] for a in ("--timed", str(f))]
    return subprocess.run([sys.executable, str(GATE), "verdict", "--checkout", str(repo), "--candidate", candidate,
                           *args, "--w3c", str(files[-2]), "--guards", str(files[-1])],
                          capture_output=True, text=True, timeout=120,
                          env=isolation.inherit("NRESE_", "FLEET_", PYTHONIOENCODING="utf-8"))


class Scratch(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="release-acceptance-", dir=isolation.SCRATCH))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.repo = Repository(self.tmp / "repo")


class CandidateJoin(Scratch):
    """D16-130 (audit T01): every input of the verdict is evidence of the one candidate tree named."""

    def test_checks_of_another_candidate_never_pass(self):
        # Spec: T01 / the design's invariant 1: the candidate's timed parts with genuine passed checks of
        # another tree, or checks naming no candidate (results from before receipts), are NOT PASSED,
        # the receipt's mismatch named.
        a = self.repo.commit(message="candidate A")
        b = self.repo.commit(policy="# another tree\n", message="candidate B")
        w3c, guards = checks(b, self.repo.tree(b))
        done = verdict(self.repo.path, a, rounds(a), w3c, guards)
        self.assertEqual(done.returncode, 1, f"SPEC checks of B joined to A's timed parts\n{done.stdout}{done.stderr}")
        self.assertIn(f"ran tree {self.repo.tree(b)}", done.stdout)
        bare = ({"counts": {}, "expected": {}, "errors": [], "passed": True},
                {"guards": 81, "results": {}, "errors": [], "passed": True})
        done = verdict(self.repo.path, a, rounds(a), *bare)
        self.assertEqual(done.returncode, 1, f"SPEC checks naming no candidate\n{done.stdout}{done.stderr}")
        self.assertIn("no receipt", done.stdout)

    def test_a_timed_part_of_another_candidate_never_passes(self):
        # Spec: T01: the candidate's checks with a timed part (one of several) that timed another tree.
        a = self.repo.commit(message="candidate A")
        b = self.repo.commit(policy="# another tree\n", message="candidate B")
        done = verdict(self.repo.path, a, [rounds(a)[0], rounds(b)[1]], *checks(a, self.repo.tree(a)))
        self.assertEqual(done.returncode, 1, f"SPEC a timed round of B joined to A\n{done.stdout}{done.stderr}")
        self.assertIn(f"timed {b[:12]}", done.stdout)

    def test_the_candidate_s_own_evidence_passes_under_any_commit_of_its_tree(self):
        # Guard (the positive side; red on the parent only for want of --candidate): a verdict belongs
        # to a tree (rule.owl-rs-results-keyed-by-tree): checks run at a merge commit of the candidate's
        # tree are the candidate's. A receipt whose commit isn't of the tree it names is refused.
        a = self.repo.commit(message="candidate A")
        merged = self.repo.same_tree(a)
        done = verdict(self.repo.path, a, rounds(a), *checks(merged, self.repo.tree(a)))
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("PASSED", done.stdout)
        b = self.repo.commit(policy="# another tree\n", message="candidate B")
        done = verdict(self.repo.path, a, rounds(a), *checks(b, self.repo.tree(a)))
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn(f"{b[:12]} isn't of tree", done.stdout)


class EvidenceContract(Scratch):
    """Package review r1 R1: a check binds as the run the candidate's tree requires (its kits, suites
    and guards), never by its own `passed`; the receipt proves that run."""

    def refused(self, name: str, w3c: dict, guards: dict, *named: str) -> None:
        a = self.repo.commit()
        for part in (w3c, guards):
            part["receipt"].update(commit=a, tree=self.repo.tree(a))
        done = verdict(self.repo.path, a, rounds(a), w3c, guards)
        self.assertEqual(done.returncode, 1, f"SPEC {name}: refused\n{done.stdout}{done.stderr}")
        for text in named:
            self.assertIn(text, done.stdout, name)

    def test_a_receipt_of_a_failed_or_unproven_run_is_refused_whatever_passed_says(self):
        # Spec: review r1 R1, probes failed-kit, missing-summary, missing-hash, truthy-failed-result, and a
        # unit launched twice: each result claims passed; each fails the candidate's contract, named.
        kit = contract()[1][0].name
        for name, change, text in (("failed-kit", {"exit": 101}, f"{kit}: exited 101"),
                                   ("missing-summary", {"summary": False}, f"{kit}: exited 0 without passing"),
                                   ("missing-hash", {"log_sha256": ""}, f"{kit}: no SHA-256 of its log")):
            with self.subTest(name):
                w3c, guards = checks("c", "t")
                w3c["receipt"]["outcomes"][0].update(change)
                self.refused(name, w3c, guards, text)
        with self.subTest("truthy-failed-result"):
            w3c, guards = checks("c", "t")
            w3c.update(passed="false", errors=["the W3C run failed"])
            self.refused("truthy-failed-result", w3c, guards, "w3c: its result doesn't say passed")
        with self.subTest("a unit launched twice"):
            w3c, guards = checks("c", "t")
            w3c["receipt"]["outcomes"].append(dict(w3c["receipt"]["outcomes"][0]))
            self.refused("a unit launched twice", w3c, guards, f"{kit}: launched 2 times")

    def test_a_partial_run_is_refused_its_missing_kits_suites_and_guards_named(self):
        # Spec: review r1 R1, probe partial-kits-zero-guards: one of the seven kits, no guard ran.
        w3c, guards = checks("c", "t")
        w3c["receipt"]["outcomes"] = w3c["receipt"]["outcomes"][:1]
        del w3c["counts"]["OWL 2 RL"]
        guards.update(guards=0, results={})
        missing_kit = contract()[1][-1].name
        self.refused("partial-kits-zero-guards", w3c, guards, f"{missing_kit}: no launch recorded",
                     "OWL 2 RL: no counts", f"{contract()[2][0]}: no test of that name ran")

    def test_the_producer_s_partial_expectations_never_pass_the_candidate(self):
        # Spec: review r1 R1, probe generated-partial-w3c-final-verdict: `w3c --run` with an expectations
        # file declaring one kit writes a passing result and receipt at the candidate's own tree; the join
        # holds it to the candidate tree's seven kits.
        stub = "#!/usr/bin/env bash\necho 'test result: ok. 1 passed; 0 failed; 0 ignored'\n"
        a = self.repo.commit(files={"scripts/cargo-guarded.sh": stub})
        partial = self.tmp / "partial.toml"
        partial.write_text('[kit."nrese-owl:w3c_owl2"]\nsuites = []\n', encoding="utf-8")
        result = self.tmp / "w3c.json"
        made = subprocess.run([sys.executable, str(GATE), "w3c", str(self.tmp / "logs"), "--expected", str(partial),
                               "--run", "--identity", IDENTITY, "--attempt", ATTEMPT, "--json", str(result)],
                              cwd=self.repo.path,
                              capture_output=True, text=True, timeout=120,
                              env=isolation.inherit("NRESE_", "FLEET_", "CARGO_", PYTHONIOENCODING="utf-8"))
        self.assertEqual(made.returncode, 0, made.stdout + made.stderr)
        w3c = json.loads(result.read_text(encoding="utf-8"))
        self.assertEqual(w3c["receipt"]["tree"], self.repo.tree(a))
        done = verdict(self.repo.path, a, rounds(a), w3c, checks(a, self.repo.tree(a))[1])
        self.assertEqual(done.returncode, 1, f"SPEC one kit of seven\n{done.stdout}")
        self.assertIn("nrese-sparql:w3c_sparql11: no launch recorded", done.stdout)


class CheckDirectory(Scratch):
    """D16-130 at the shell (scripts/release-gate.sh verdict): the checks' directory holds one result of each."""

    def test_two_w3c_results_in_the_checks_directory_are_refused_not_the_first_taken(self):
        # Spec: T01 ("selecting a result directory with two incompatible checks fails"): today the step
        # takes `ls | head -1` of the W3C and guard results; a rerun beside the first leaves two.
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("needs bash")
        candidate = self.repo.commit(message="candidate", files={
            rel: (ROOT / rel).read_text(encoding="utf-8") for rel in ("scripts/release-gate.sh", "benches/fast/release_gate.py",
                                                                     ".gitignore")})
        for directory in ("scripts/lib", "benches/suite/suitekit"):
            shutil.copytree(ROOT / directory, self.repo.path / directory, ignore=shutil.ignore_patterns("__pycache__"))
        candidate = self.repo.commit(message="the gate")
        tree = self.repo.tree(candidate)
        timed, results = self.tmp / "timed", self.tmp / "checks"
        (results / "rerun").mkdir(parents=True)
        timed.mkdir()
        (timed / "release-gate.json").write_text(json.dumps(rounds(candidate)[0]), encoding="utf-8")
        w3c, guards = checks(candidate, tree)
        (results / "w3c.json").write_text(json.dumps(w3c), encoding="utf-8")
        (results / "rerun" / "w3c.json").write_text(json.dumps(w3c), encoding="utf-8")
        (results / "guards.json").write_text(json.dumps(guards), encoding="utf-8")
        shim = self.tmp / "shim"
        shim.mkdir()
        (shim / "python3").write_text(f'#!/usr/bin/env bash\nexec "{Path(sys.executable).as_posix()}" "$@"\n',
                                      encoding="utf-8", newline="\n")
        (shim / "python3").chmod(0o755)
        env = isolation.inherit("NRESE_", "FLEET_", NRESE_GATE_DIR=(self.tmp / "gate").as_posix(),
                                GATE_BIN=(self.tmp / "bin").as_posix(), FLEET_OUT=(self.tmp / "out").as_posix(),
                                PATH=shim.as_posix() + os.pathsep + os.environ["PATH"])
        done = subprocess.run([bash, "scripts/release-gate.sh", "verdict", candidate, timed.as_posix(), results.as_posix()],
                              cwd=self.repo.path, env=env, capture_output=True, text=True, timeout=300)
        output = done.stdout + done.stderr
        self.assertEqual(done.returncode, 2, f"SPEC two W3C results: refused\n{output}")
        self.assertIn("2 w3c.json", output)
        self.assertNotIn("PASSED", output.replace("NOT PASSED", ""))


def kits_toml(*kits: tuple[str, list[str]], suites: str = "") -> str:
    lines = [suites]
    for kit, names in kits:
        lines.append(f'[kit."{kit}"]\nsuites = {json.dumps(names)}\n')
    return "\n".join(lines)


RL_SUITE = '[suite."OWL 2 RL"]\npassed = 86\nskipped = 3\n'
RL_LOG = ("W3C OWL 2 RL test cases (RDF-based semantics): 86 passed, 0 failed (0 expected), 3 skipped\n"
          "test result: ok. 1 passed; 0 failed; 0 ignored\n")
KITS = (("nrese-store:w3c_owl2_rl", ["OWL 2 RL"]), ("nrese-owl:w3c_owl2", []),
        ("nrese-reasoner:w3c_n3_reasoner", []))


class KitOutcomes(Scratch):
    """D16-126 (audit T02): every declared W3C kit has one recorded launch outcome; a kit that
    didn't run whole is no pass, whatever its log says or doesn't."""

    def w3c(self, logs: Path, *extra: str, cwd: Path | None = None) -> subprocess.CompletedProcess:
        expected = self.tmp / "w3c-expected.toml"
        if not expected.exists():
            expected.write_text(kits_toml(*KITS, suites=RL_SUITE), encoding="utf-8")
        return subprocess.run([sys.executable, str(GATE), "w3c", str(logs), "--expected", str(expected),
                               "--json", str(self.tmp / "w3c.json"), *extra], cwd=cwd, capture_output=True,
                              text=True, timeout=120, env=isolation.inherit("NRESE_", "FLEET_", "CARGO_",
                                                                            PYTHONIOENCODING="utf-8"))

    def test_logs_without_a_recorded_launch_of_every_declared_kit_fail(self):
        # Spec: T02's witness: w3c_owl2 failed to compile (no libtest summary) and w3c_n3_reasoner was
        # killed before writing anything; every suite with an expectation is fine. Exit 1, both kits named.
        logs = self.tmp / "logs"
        logs.mkdir()
        (logs / "w3c_owl2_rl.log").write_text(RL_LOG, encoding="utf-8")
        (logs / "w3c_owl2.log").write_text("error[E0425]: cannot find value `x` in this scope\n"
                                           "error: could not compile `nrese-owl` (test \"w3c_owl2\")\n", encoding="utf-8")
        (logs / "w3c_n3_reasoner.log").write_text("", encoding="utf-8")
        done = self.w3c(logs)
        self.assertEqual(done.returncode, 1, f"SPEC kits that didn't run whole\n{done.stdout}{done.stderr}")
        for kit in ("nrese-owl:w3c_owl2", "nrese-reasoner:w3c_n3_reasoner"):
            self.assertIn(kit, done.stdout)

    def cargo_guarded(self, behaviour: dict[str, str]) -> Path:
        """A checkout whose scripts/cargo-guarded.sh plays each kit (its --test name) as BEHAVIOUR says,
        committed: the receipt names its commit and tree."""
        checkout = self.tmp / "checkout"
        cases = "\n".join(f"  {name}) {body} ;;" for name, body in behaviour.items())
        stub = checkout / "scripts" / "cargo-guarded.sh"
        stub.parent.mkdir(parents=True)
        stub.write_text(f'#!/usr/bin/env bash\n# test --release --locked -p CRATE --test NAME -- --nocapture\n'
                        f'case "$7" in\n{cases}\n  *) echo "unexpected $*"; exit 3 ;;\nesac\n',
                        encoding="utf-8", newline="\n")
        git(checkout.parent, "init", "-q", str(checkout))
        git(checkout, "add", "-A")
        git(checkout, "commit", "-q", "-m", "the checks' checkout")
        return checkout

    def run_kits(self, behaviour: dict[str, str]) -> tuple[subprocess.CompletedProcess, dict]:
        checkout = self.cargo_guarded(behaviour)
        logs = self.tmp / "out-w3c"
        done = self.w3c(logs, "--run", "--identity", IDENTITY, "--attempt", ATTEMPT, cwd=checkout)
        result = json.loads((self.tmp / "w3c.json").read_text(encoding="utf-8")) if (self.tmp / "w3c.json").exists() else {}
        return done, result

    RL_OK = f"printf '%s' '{RL_LOG}'; exit 0"
    OK = "echo 'test result: ok. 1 passed; 0 failed; 0 ignored'; exit 0"

    def test_a_kit_that_exits_non_zero_or_without_its_summary_fails_and_each_launch_is_recorded(self):
        # Spec: T02's proof list: exit 101 without a summary, death by a signal, an empty or truncated log,
        # the guard's 75: each NOT PASSED with the kit named; all ran whole: passed, and the receipt holds
        # every launch (its exit, its log's SHA-256) and the checkout's commit and tree.
        for name, body in {"compile error, exit 101": "echo 'error: could not compile'; exit 101",
                           "killed (exit 137)": "kill -9 $$",
                           "summary missing, exit 0": "echo 'running 1 test'; exit 0",
                           "a failed summary, exit 0": "echo 'test result: FAILED. 0 passed; 1 failed'; exit 0",
                           "the guard held the build (75)": "exit 75"}.items():
            with self.subTest(name):
                shutil.rmtree(self.tmp / "checkout", True)
                shutil.rmtree(self.tmp / "out-w3c", True)
                (self.tmp / "w3c.json").unlink(missing_ok=True)
                done, result = self.run_kits({"w3c_owl2_rl": self.RL_OK, "w3c_owl2": body, "w3c_n3_reasoner": self.OK})
                self.assertEqual(done.returncode, 1, f"SPEC {name}\n{done.stdout}{done.stderr}")
                self.assertIn("nrese-owl:w3c_owl2", " ".join(result.get("errors", [])), result)
        shutil.rmtree(self.tmp / "checkout", True)
        shutil.rmtree(self.tmp / "out-w3c", True)
        done, result = self.run_kits({"w3c_owl2_rl": self.RL_OK, "w3c_owl2": self.OK, "w3c_n3_reasoner": self.OK})
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        checkout = self.tmp / "checkout"
        r = result["receipt"]
        self.assertEqual((r["kind"], r["commit"], r["tree"], r["identity"]),
                         ("w3c", git(checkout, "rev-parse", "HEAD"), git(checkout, "rev-parse", "HEAD^{tree}"), IDENTITY))
        self.assertEqual([(o["unit"], o["exit"], o["summary"]) for o in r["outcomes"]],
                         [(k, 0, True) for k, _ in KITS])
        log = (self.tmp / "out-w3c" / "w3c_owl2_rl.log").read_bytes()
        self.assertEqual(r["outcomes"][0]["log_sha256"], hashlib.sha256(log).hexdigest())

    def test_a_re_check_refuses_a_receipt_whose_logs_a_later_run_replaced(self):
        # Spec: sol-loop r1 S1: a run passed and left its receipt; a later run stopped midway, having
        # replaced w3c_owl2's log with a compile failure; a re-check (no --run) reads the logs as they are.
        done, _ = self.run_kits({"w3c_owl2_rl": self.RL_OK, "w3c_owl2": self.OK, "w3c_n3_reasoner": self.OK})
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        (self.tmp / "out-w3c" / "w3c_owl2.log").write_text("error: could not compile `nrese-owl`\n", encoding="utf-8")
        again = self.w3c(self.tmp / "out-w3c")
        self.assertEqual(again.returncode, 1, f"SPEC a replaced log\n{again.stdout}{again.stderr}")
        self.assertIn("nrese-owl:w3c_owl2: its log isn't the one its launch recorded", again.stdout)

    def test_a_suite_s_counts_come_from_its_own_kit_s_log(self):
        # Spec: the design's attack 8: the RL counts printed by another kit's log don't stand for the RL kit.
        done, result = self.run_kits({"w3c_owl2_rl": self.OK, "w3c_owl2": self.RL_OK, "w3c_n3_reasoner": self.OK})
        self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
        self.assertIn("OWL 2 RL", " ".join(result["errors"]))

    def test_the_gate_s_kits_are_declared_once_and_claim_every_expected_suite(self):
        # Spec: T02 ("the expectation table still omits the two extra kits"): release-gate.sh's w3c step
        # launches what w3c-expected.toml declares (no list of its own), every kit the step ran before
        # this change among them, and every suite with an expectation belongs to exactly one kit.
        listed = subprocess.run([sys.executable, str(GATE), "w3c", "--list-kits", "--expected",
                                 str(ROOT / "benches/fast/w3c-expected.toml")], capture_output=True, text=True,
                                timeout=60, env=isolation.inherit("NRESE_", "FLEET_"))
        self.assertEqual(listed.returncode, 0, listed.stderr)
        self.assertEqual(listed.stdout.split(), [
            "nrese-sparql:w3c_sparql11", "nrese-sparql:geosparql_compliance", "nrese-shacl:w3c_shacl",
            "nrese-store:w3c_owl2_rl", "nrese-dl:tableau_w3c", "nrese-owl:w3c_owl2", "nrese-reasoner:w3c_n3_reasoner"])
        declared = tomllib.loads((ROOT / "benches/fast/w3c-expected.toml").read_text(encoding="utf-8"))
        owners = [s for kit in declared["kit"].values() for s in kit["suites"]]
        self.assertEqual(sorted(owners), sorted(declared["suite"]))
        step = (ROOT / "scripts/release-gate.sh").read_text(encoding="utf-8").split("\nw3c)", 1)[1].split("\n  ;;", 1)[0]
        self.assertNotIn("w3c_sparql11", step, "SPEC the step's kits are the declared ones, never a list of its own")


class GuardRun(Scratch):
    """D16-126's guard half: the guards' test run keeps its exit code; a guard reported ok under a run
    that failed is no pass."""

    def test_guards_under_a_failed_test_run_do_not_pass(self):
        checkout = self.tmp / "checkout"
        stub = checkout / "scripts" / "cargo-guarded.sh"
        stub.parent.mkdir(parents=True)
        stub.write_text("#!/usr/bin/env bash\necho 'test a::first_guard_name_here ... ok'\n"
                        "echo 'test b::other_matched_test_failing ... FAILED'\nexit 101\n", encoding="utf-8", newline="\n")
        git(checkout.parent, "init", "-q", str(checkout))
        git(checkout, "add", "-A")
        git(checkout, "commit", "-q", "-m", "checks")
        (self.tmp / "names").write_text("first_guard_name_here\n", encoding="utf-8")
        out = self.tmp / "guards.json"
        done = subprocess.run([sys.executable, str(GATE), "guards", "--run", "--names", str(self.tmp / "names"),
                               "--log", str(self.tmp / "test.log"), "--identity", IDENTITY, "--attempt", ATTEMPT,
                               "--json", str(out)],
                              cwd=checkout, capture_output=True, text=True, timeout=120,
                              env=isolation.inherit("NRESE_", "FLEET_", "CARGO_", PYTHONIOENCODING="utf-8"))
        self.assertEqual(done.returncode, 1, f"SPEC a guard ok under a run that exited 101\n{done.stdout}{done.stderr}")
        result = json.loads(out.read_text(encoding="utf-8"))
        self.assertIn("exited 101", " ".join(result["errors"]))
        self.assertEqual([(o["unit"], o["exit"]) for o in result["receipt"]["outcomes"]], [("guards", 101)])

    def test_a_launch_that_fails_leaves_no_earlier_result(self):
        # Spec: review r1 R2, probes guards-launch-error and stale-guard-after-launch-error: a passed
        # guards.json from an earlier run, then a run that fails before it launches (its setup raises: the
        # guards' names can't be read): no result stands afterwards, never the earlier one.
        out = self.tmp / "guards.json"
        out.write_text(json.dumps(checks("c", "t")[1]), encoding="utf-8")
        done = subprocess.run([sys.executable, str(GATE), "guards", "--run", "--names", str(self.tmp / "no-names"),
                               "--log", str(self.tmp / "test.log"), "--identity", IDENTITY, "--attempt", ATTEMPT,
                               "--json", str(out)],
                              cwd=self.tmp, capture_output=True, text=True, timeout=120,
                              env=isolation.inherit("NRESE_", "FLEET_", "CARGO_", PYTHONIOENCODING="utf-8"))
        self.assertNotEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertFalse(out.exists(), f"SPEC the earlier passed result is gone\n{done.stdout}{done.stderr}")

    def test_the_guards_step_fails_when_its_checker_publishes_nothing(self):
        # Spec: review r1 R2, probe exact-guards-shell-boundary: release-gate.sh's guards step, its checker
        # failing before it publishes, an earlier passed guards.json in its results: the step fails and no
        # earlier result stands.
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("needs bash")
        body = (ROOT / "scripts/release-gate.sh").read_text(encoding="utf-8").split("\nguards)\n", 1)[1].split("\n  ;;", 1)[0]
        script = self.tmp / "guards-step.sh"
        script.write_text('set -uo pipefail\nOUT="$1"\nGATE="$1"\nlog() { echo "$*"; }\n'
                          'gate_identity() { echo identity; }\n'
                          'py() { echo "the checker failed before publishing" >&2; return 1; }\n' + body + "\n",
                          encoding="utf-8", newline="\n")
        out = self.tmp / "out"
        out.mkdir()
        (out / "guards.json").write_text(json.dumps(checks("c", "t")[1]), encoding="utf-8")
        done = subprocess.run([bash, script.as_posix(), out.as_posix()], capture_output=True, text=True, timeout=60,
                              env=isolation.inherit("NRESE_", "FLEET_"))
        self.assertNotEqual(done.returncode, 0, f"SPEC the step fails\n{done.stdout}{done.stderr}")
        self.assertFalse((out / "guards.json").exists(), "SPEC no earlier result stands")


class AttemptRecords(Scratch):
    """Fix check r2, R2 (the orchestrator's design of 13:01): each check step writes a fresh attempt record
    first (KIND.attempt.json: a random id and the tree); its result and receipt carry that id; the verdict
    takes a result only beside a record of the same id."""

    def attempt(self, kind: str, where: Path) -> str:
        done = subprocess.run([sys.executable, str(GATE), "attempt", kind, str(where)], cwd=self.repo.path,
                              capture_output=True, text=True, timeout=60,
                              env=isolation.inherit("NRESE_", "FLEET_", PYTHONIOENCODING="utf-8"))
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        return done.stdout.strip()

    def test_a_normal_run_passes_with_its_own_attempt(self):
        # Spec: a guards run end to end (attempt record, then the real producer through a cargo stand-in
        # that runs every §0 guard ok), a W3C result of its own attempt: the verdict passes; each record
        # names a fresh id and the candidate's tree.
        names = contract()[2]
        stub = "#!/usr/bin/env bash\n" + "".join(f"echo 'test a::{n} ... ok'\n" for n in names) + \
               "echo 'test result: ok. 1 passed; 0 failed; 0 ignored'\n"
        a = self.repo.commit(files={"scripts/cargo-guarded.sh": stub})
        out = self.tmp / "out"
        out.mkdir()
        attempts = {kind: self.attempt(kind, out) for kind in ("w3c", "guards")}
        self.assertNotEqual(attempts["w3c"], attempts["guards"])
        for kind in attempts:
            record = json.loads((out / f"{kind}.attempt.json").read_text(encoding="utf-8"))
            self.assertEqual(record, attempt_record(kind, attempts[kind], self.repo.tree(a)))
        made = subprocess.run([sys.executable, str(GATE), "guards", "--run", "--log", str(out / "test.log"),
                               "--identity", IDENTITY, "--attempt", attempts["guards"], "--json", str(out / "guards.json")],
                              cwd=self.repo.path, capture_output=True, text=True, timeout=120,
                              env=isolation.inherit("NRESE_", "FLEET_", "CARGO_", PYTHONIOENCODING="utf-8"))
        self.assertEqual(made.returncode, 0, made.stdout + made.stderr)
        guards = json.loads((out / "guards.json").read_text(encoding="utf-8"))
        self.assertEqual((guards["attempt"], guards["receipt"]["attempt"]), (attempts["guards"],) * 2)
        w3c = checks(a, self.repo.tree(a))[0]
        with_attempt(w3c, attempts["w3c"])
        (out / "w3c.json").write_text(json.dumps(w3c), encoding="utf-8")
        done = verdict_on(self.repo.path, a, self.tmp, rounds(a), out / "w3c.json", out / "guards.json")
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)

    def test_a_result_of_another_attempt_or_none_is_refused(self):
        # Spec: the orchestrator's design, items 3 and 5: a result beside a newer attempt record (its
        # producer never published), beside no record, or an attempt record with no result: each refused
        # with its named reason.
        a = self.repo.commit()
        tree = self.repo.tree(a)
        w3c, guards = checks(a, tree)
        for name, attempts, text in (
                ("a newer attempt", {"guards": attempt_record("guards", "b" * 32, tree)},
                 "guards: stale result from an earlier attempt"),
                ("no attempt record", {"w3c": None}, "w3c: no attempt record")):
            with self.subTest(name):
                done = verdict(self.repo.path, a, rounds(a), copy.deepcopy(w3c), copy.deepcopy(guards), attempts)
                self.assertEqual(done.returncode, 1, f"SPEC {name}\n{done.stdout}")
                self.assertIn(text, done.stdout)
        with self.subTest("an attempt record without its result"):
            where = self.tmp / "no-result"
            where.mkdir()
            (where / "guards.attempt.json").write_text(json.dumps(attempt_record("guards", "c" * 32, tree)),
                                                       encoding="utf-8")
            done = verdict_on(self.repo.path, a, where, rounds(a), w3c, where / "guards.json")
            self.assertEqual(done.returncode, 1, done.stdout + done.stderr)
            self.assertIn("guards: no result for its latest attempt", done.stdout)


@contextlib.contextmanager
def undeletable(path: Path):
    """PATH held open as a Windows reader may hold it: read and write sharing, no delete sharing
    (CreateFileW), so neither rm nor os.remove can take it away while held."""
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateFileW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p,
                                   ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p]
    kernel.CreateFileW.restype = ctypes.c_void_p
    kernel.CloseHandle.argtypes = [ctypes.c_void_p]
    generic_read, share_read_write, open_existing = 0x80000000, 0x00000003, 3
    handle = kernel.CreateFileW(str(path), generic_read, share_read_write, None, open_existing, 0, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        yield
    finally:
        kernel.CloseHandle(handle)


@unittest.skipUnless(os.name == "nt", "a reader that blocks deletion is Windows' (no delete sharing)")
class ResultInvalidation(Scratch):
    """Fix check r1, R2 (IB1223-a565): an earlier result that can't be removed is no run's result. The
    step fails before anything runs; the checker's failed removal is a crash (exit 3), never the judged
    result's 0 or 1."""

    def setUp(self):
        super().setUp()
        self.out = self.tmp / "out"
        (self.out / "guards").mkdir(parents=True)
        (self.out / "w3c").mkdir()

    def step(self, name: str, py: str) -> subprocess.CompletedProcess:
        """release-gate.sh's NAME step as checked in, its checker PY (`-c` the step's summary line)."""
        bash = shutil.which("bash")
        if bash is None:
            self.skipTest("needs bash")
        body = (ROOT / "scripts/release-gate.sh").read_text(encoding="utf-8").split(f"\n{name})\n", 1)[1].split("\n  ;;", 1)[0]
        # The step's checkout: a commit of its own (an attempt record names its tree).
        cwd = self.tmp / "cwd"
        if not (cwd / ".git").exists():
            (cwd / "scripts").mkdir(parents=True, exist_ok=True)
            (cwd / "scripts/fetch-w3c-tests.sh").write_text("exit 0\n", encoding="utf-8", newline="\n")
            git(self.tmp, "init", "-q", str(cwd))
            git(cwd, "add", "-A")
            git(cwd, "commit", "-q", "-m", "the step's checkout")
        script = self.tmp / f"{name}-step.sh"
        # py: the step's summary line (-c) prints nothing; its attempt record is the real checker's; its
        # check is PY's.
        script.write_text('set -uo pipefail\nOUT="$1"\nGATE="$1"\nlog() { echo "$*"; }\n'
                          'gate_identity() { echo identity; }\nsha256sum() { cat > /dev/null; }\n'
                          'py() { [ "$1" != -c ] || return 0; [ "${2:-}" != attempt ] || { "$REALPY" "$REALGATE" "${@:2}"; return; }; '
                          f'"$REALPY" "$REALGATE" {py}; }}\n' + body + "\n",
                          encoding="utf-8", newline="\n")
        return subprocess.run([bash, script.as_posix(), self.out.as_posix()], cwd=cwd, capture_output=True, text=True,
                              timeout=120, env=isolation.inherit("NRESE_", "FLEET_", "CARGO_",
                                                                 REALPY=Path(sys.executable).as_posix(),
                                                                 REALGATE=GATE.as_posix(), PYTHONIOENCODING="utf-8"))

    def test_a_step_whose_earlier_result_can_t_be_removed_fails_and_runs_nothing(self):
        # Spec: fix check r1, witness check_boundaries.py: a passed guards.json (w3c.json) of an earlier run,
        # held by a reader without delete sharing; the step's checker can't publish (its names / expectations
        # can't be read). Today the step's rm fails silently, the checker exits 1 and the step accepts the
        # earlier file: exit 0, and the verdict passes on it. The step must fail, saying why.
        for name, result, py in (
                ("guards", "guards.json", 'guards --run --identity identity --names "$OUT/no-names" '
                                          '--log "$OUT/guards/test.log" --json "$OUT/guards.json"'),
                ("w3c", "w3c.json", 'w3c "$OUT/w3c" --expected "$OUT/no-expected" --run --identity identity '
                                    '--json "$OUT/w3c.json"')):
            with self.subTest(name):
                earlier = self.out / result
                earlier.write_text(json.dumps(checks("c", "t")[0 if name == "w3c" else 1]), encoding="utf-8")
                with undeletable(earlier):
                    done = self.step(name, py)
                self.assertNotEqual(done.returncode, 0, f"SPEC {name}: an earlier result held\n{done.stdout}{done.stderr}")
                self.assertIn(f"{name}: the earlier result", done.stdout + done.stderr)

    def test_a_locked_earlier_result_is_refused_by_the_verdict_after_its_step_fails(self):
        # Spec: fix check r2, R2 (IB1254-0ebf; the orchestrator's design of 13:01): an earlier complete
        # passing result of this very candidate, held by a reader without delete sharing; its step fails
        # before anything runs; the actual verdict CLI on what the results directory then holds refuses
        # it: a stale result from an earlier attempt. Today it passes, exit 0.
        a = self.repo.commit()
        tree = self.repo.tree(a)
        earlier = "0" * 32
        for name, py in (("guards", 'guards --run --identity identity --attempt x --names "$OUT/no-names" '
                                    '--log "$OUT/guards/test.log" --json "$OUT/guards.json"'),
                         ("w3c", 'w3c "$OUT/w3c" --expected "$OUT/no-expected" --run --identity identity '
                                 '--attempt x --json "$OUT/w3c.json"')):
            with self.subTest(name):
                w3c, guards = checks(a, tree)
                for kind, result in (("w3c", w3c), ("guards", guards)):
                    if kind == name:
                        with_attempt(result, earlier)
                    (self.out / f"{kind}.json").write_text(json.dumps(result), encoding="utf-8")
                    (self.out / f"{kind}.attempt.json").write_text(
                        json.dumps(attempt_record(kind, result["attempt"], tree)), encoding="utf-8")
                with undeletable(self.out / f"{name}.json"):
                    done = self.step(name, py)
                self.assertNotEqual(done.returncode, 0, done.stdout + done.stderr)
                judged = verdict_on(self.repo.path, a, self.tmp, rounds(a), self.out / "w3c.json",
                                    self.out / "guards.json")
                self.assertEqual(judged.returncode, 1, f"SPEC {name}: the earlier result refused\n{judged.stdout}")
                self.assertIn(f"{name}: stale result from an earlier attempt", judged.stdout)

    def test_the_checker_s_failed_removal_is_a_crash_not_a_judged_result(self):
        # Spec: fix check r1: the checker exits 0 (passed) or 1 (not passed) only with its own published
        # result; an earlier result it can't remove exits 3.
        for name, args in (("guards", ["guards", "--run", "--identity", IDENTITY, "--attempt", ATTEMPT,
                                       "--names", "no-names", "--log", "test.log", "--json", "guards.json"]),
                           ("w3c", ["w3c", "logs", "--expected", "no-expected", "--run", "--identity", IDENTITY,
                                    "--attempt", ATTEMPT, "--json", "w3c.json"])):
            with self.subTest(name):
                earlier = self.tmp / f"{name}.json"
                earlier.write_text("{}", encoding="utf-8")
                with undeletable(earlier):
                    done = subprocess.run([sys.executable, str(GATE), *args], cwd=self.tmp, capture_output=True,
                                          text=True, timeout=120,
                                          env=isolation.inherit("NRESE_", "FLEET_", "CARGO_", PYTHONIOENCODING="utf-8"))
                self.assertEqual(done.returncode, 3, f"SPEC {name}: a crash\n{done.stdout}{done.stderr}")
                self.assertIn("can't be removed", done.stderr)


def wrong_everywhere(side, n):
    return [fixtures.record(status="wrong", expected="wrong")]


def exception(case: str = "lab-olympics", outcome: str = "wrong", by: str = "Johannes", extra: str = QUOTE) -> str:
    return (f'[[exception]]\ncase = "{case}"\noutcome = "{outcome}"\napproved = "2026-11-09"\nby = "{by}"\n'
            f'reason = "the fixture\'s inherited answer"\n{extra}')


class Authority(Scratch):
    """D16-125 (audit T03): a known issue explains evidence; only Johannes grants a release exception,
    named and scoped, in the candidate tree's one policy file."""

    def test_a_declared_wrong_answer_without_an_owner_exception_blocks_and_is_named(self):
        # Spec: T03: both sides wrong, the case declares `outcome = "wrong"` with its `known` issue, the
        # policy holds no exception: NOT PASSED, the case and its wrong answer named.
        a = self.repo.commit()
        done = verdict(self.repo.path, a, rounds(a, wrong_everywhere), *checks(a, self.repo.tree(a)))
        self.assertEqual(done.returncode, 1, f"SPEC an unapproved wrong answer\n{done.stdout}{done.stderr}")
        self.assertIn("lab-olympics: wrong, no owner exception", done.stdout)

    def test_a_named_exception_covers_its_case_and_outcome_only_and_is_listed(self):
        # Spec: T03's proof list: a named approved exception is disclosed and accepted within its scope;
        # a mismatched one blocks.
        a = self.repo.commit(policy=exception())
        done = verdict(self.repo.path, a, rounds(a, wrong_everywhere), *checks(a, self.repo.tree(a)))
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("owner exception: lab-olympics wrong, approved 2026-11-09 by Johannes", done.stdout)
        for policy in (exception(case="kernel-intersect"), exception(outcome="memory-limit")):
            b = self.repo.commit(policy=policy)
            done = verdict(self.repo.path, b, rounds(b, wrong_everywhere), *checks(b, self.repo.tree(b)))
            self.assertEqual(done.returncode, 1, f"SPEC out of scope: {policy}\n{done.stdout}")

    def test_an_exception_governs_its_wrong_answer_where_a_baseline_answers_right(self):
        # Spec: sol-loop r1 S2: the candidate wrong, as the case declares, every baseline right. Without an
        # exception: NOT PASSED, and the round alone never passes; with Johannes's for that case and outcome:
        # PASSED, no other blocker left standing for the same outcome.
        def records_of(side, n):
            return [fixtures.record(status="wrong" if side == "cand" else "ok", expected="wrong")]
        a = self.repo.commit()
        timed = rounds(a, records_of)
        self.assertFalse(timed[0]["passed"], "SPEC a declared wrong answer passes no round alone")
        done = verdict(self.repo.path, a, timed, *checks(a, self.repo.tree(a)))
        self.assertEqual(done.returncode, 1, done.stdout)
        self.assertIn("lab-olympics: wrong, no owner exception", done.stdout)
        b = self.repo.commit(policy=exception())
        done = verdict(self.repo.path, b, rounds(b, records_of), *checks(b, self.repo.tree(b)))
        self.assertEqual(done.returncode, 0, f"SPEC the exception governs its wrong answer\n{done.stdout}")

    def test_an_entry_without_johannes_s_word_is_refused(self):
        # Spec: role.owners (bar exceptions are Johannes's) and rule.record-owner-intent-verbatim: an entry
        # by anyone else, or without the quote and its source, is a policy error: NOT PASSED.
        for policy in (exception(by="the owner"), exception(extra="")):
            a = self.repo.commit(policy=policy)
            done = verdict(self.repo.path, a, rounds(a, wrong_everywhere), *checks(a, self.repo.tree(a)))
            self.assertEqual(done.returncode, 1, f"SPEC {policy}\n{done.stdout}")
            self.assertIn("release-tradeoffs.toml", done.stdout)

    def test_the_policy_is_the_candidate_tree_s_never_a_report_s_or_another_tree_s(self):
        # Spec: the design's attack 4: a timed report carries what its harness tree held; the verdict reads
        # the policy at the candidate's commit only.
        other = self.repo.commit(policy=exception(), message="a tree with the exception")
        a = self.repo.commit(message="the candidate, without it")
        timed = rounds(a, wrong_everywhere)
        for summary in timed:
            summary["exceptions"] = [{"case": "lab-olympics", "outcome": "wrong", "by": "Johannes"}]
        done = verdict(self.repo.path, a, timed, *checks(a, self.repo.tree(a)))
        self.assertEqual(done.returncode, 1, f"SPEC {other[:8]}'s exception doesn't cover {a[:8]}\n{done.stdout}")

    def test_a_limit_every_side_stops_at_is_named_never_silent(self):
        # Guard (the master's decision 1 of 9 Nov; red on the parent only for want of --candidate): a
        # declared capability limit (memory-limit) where every side stops is no wrong answer and no loss:
        # it passes, listed. One where a baseline completes blocks, as before.
        a = self.repo.commit()
        limit = lambda side, n: [fixtures.record(status="memory-limit", expected="memory-limit")]  # noqa: E731
        done = verdict(self.repo.path, a, rounds(a, limit), *checks(a, self.repo.tree(a)))
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("known issue: lab-olympics: memory-limit", done.stdout)


class Tradeoffs(Scratch):
    """Only an owner-approved trade-off accepts a loss (benches/release-tradeoffs.toml at the
    candidate's tree). Moved from test_release_gate.py: the verdict reads its policy from the
    candidate commit, no longer from what each timed report embedded."""

    def rounds(self, candidate: str, losses: tuple[float, float] = (1.08, 1.08)) -> list[dict]:
        """Two rounds, round k's q06 LOSSES[k] times the baselines', the paying win in both."""
        def records_of(loss):
            def records(side, n):
                cand = side == "cand"
                return [fixtures.record(samples={"q01": [10.0, 10.1, 9.9],
                                                 "q06": [v * (fixtures.worse(loss, n) if cand else 1)
                                                         for v in [4.8, 4.85, 4.8]]}),
                        fixtures.record(case="kernel-intersect",
                                        samples={"triangles": [v * (fixtures.worse(0.36, n) if cand else 1)
                                                               for v in [140.0, 140.5]]})]
            return records
        # Three blocks: with two, the 8 % loss sat on Holm's threshold (combined p 0.0082 against
        # 0.0083), and which runs a side got in the block's order decided it.
        out = []
        for k in (0, 1):
            summary = fixtures.run_gate(fixtures.passes(records_of(losses[k]), blocks=3),
                                        context={**fixtures.CONTEXT, "round": k})
            summary["candidate"]["revision"] = candidate
            out.append(summary)
        return out

    def entry(self, **changes) -> str:
        fields = {"case": "lab-olympics", "dimension": "time/q06", "max_ratio": 1.12, "approved": "2026-10-20",
                  "by": "Johannes", "reason": "text index: 8 % slower filters for 2.8x faster triangles",
                  "quote": "accept the filters' loss for the triangles", "source": "OWL-RS master thread, 20 Oct 2026",
                  "pays_with": {"case": "kernel-intersect", "dimension": "time/triangles"}, **changes}
        lines = ["[[tradeoff]]"]
        for key, value in fields.items():
            if isinstance(value, dict):
                value = "{ " + ", ".join(f'{k} = "{v}"' for k, v in value.items()) + " }"
            elif isinstance(value, str):
                value = json.dumps(value)
            lines.append(f"{key} = {value}")
        return "\n".join(lines) + "\n"

    def verdict(self, policy: str = "", losses: tuple[float, float] = (1.08, 1.08)) -> subprocess.CompletedProcess:
        a = self.repo.commit(policy=policy)
        return verdict(self.repo.path, a, self.rounds(a, losses), *checks(a, self.repo.tree(a)))

    def test_a_trade_off_covers_a_loss_only_within_its_bound_in_every_round(self):
        # Spec: review r1 R3, probe real-reports-tradeoff-8-percent-then-50-percent: an approval bounded
        # at 1.12 with its win shown in both rounds; round 1's loss 8 %, round 2's 50 %: anything beyond
        # the bound blocks (PROTOCOL.md §5), whichever round shows it.
        done = self.verdict(self.entry(), losses=(1.08, 1.50))
        self.assertEqual(done.returncode, 1, f"SPEC a 50 % loss beyond a 1.12 bound\n{done.stdout}")
        self.assertIn("**confirmed loss**: lab-olympics against", done.stdout)

    def test_a_policy_file_of_another_shape_is_refused_never_read_as_none(self):
        # Spec: review r1 (malformed input): `tradeoff = 1` raised TypeError; a [tradeoff] or [exception]
        # table read as no entries. Each is an error naming its table, never an exception or an empty policy.
        for text in ("tradeoff = 1", '[tradeoff]\ncase = "x"\n', '[exception]\ncase = "x"\n', "exception = [1]"):
            with self.subTest(text):
                policy = gate_module.load_policy(text)
                self.assertEqual((policy.tradeoffs, policy.exceptions), ([], []))
                self.assertTrue(any("an array of tables" in e for e in policy.errors), policy.errors)

    def test_a_confirmed_loss_blocks_unless_an_approved_trade_off_with_its_win_covers_it(self):
        self.assertEqual(self.verdict().returncode, 1)
        done = self.verdict(self.entry())
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        self.assertIn("accepted trade-off", done.stdout)
        for entry in (self.entry(max_ratio=1.05), self.entry(pays_with={"case": "lab-olympics", "dimension": "time/q01"}),
                      self.entry(dimension="time/q01"), self.entry(by="the owner")):
            self.assertEqual(self.verdict(entry).returncode, 1, entry)

    def test_the_list_starts_empty_and_a_malformed_entry_is_refused(self):
        policy = gate_module.load_policy((ROOT / POLICY).read_text(encoding="utf-8"))
        self.assertEqual((policy.tradeoffs, policy.exceptions, policy.errors), ([], [], []))
        bad = gate_module.load_policy('[[tradeoff]]\ncase = "x"\ndimension = "time/q"\nmax_ratio = 1.1\n')
        self.assertEqual(bad.tradeoffs, [])
        self.assertIn("missing approved, by, pays_with, quote, reason, source", bad.errors[0])


if __name__ == "__main__":
    unittest.main()
