"""The suite contracts run when what they read changed (contracts.py), and their declarations stay honest.

Spec (the order for pkg/contracts-by-reads, 21 Nov 2026): "each contract declares what it reads, the
gate runs it when one of those changed, and a contract that declares nothing always runs". A change
to what every contract reads (the runner, the selector, the suite's Python and packages) runs them all.

Honesty, here, by reading the contracts' sources: every declared read names a file this repository
tracks, and every repository file a contract names (`ROOT / "a" / "b"`, the paths it copies into a
checkout of its own, the modules it imports and theirs) is among its reads. What a script the contract
runs reads in turn is the sandbox proof's to find (each contract run in a checkout holding only its
reads). This contract reads every contract, so it declares nothing and always runs.

Run: python benches/suite/tests/isolation.py test_contract_reads
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""
import ast
import shutil
import subprocess
import sys
import tempfile
import unittest
from itertools import combinations
from pathlib import Path, PurePosixPath

import contracts

ROOT = Path(__file__).resolve().parents[3]
TESTS = contracts.TESTS


class Selection(unittest.TestCase):
    """contracts.select over contracts declared here."""

    CONTRACTS = {
        "test_paths": (f"{TESTS}/test_paths.py", ("scripts/check.sh", "scripts/lib/*.sh")),
        "test_deep": (f"{TESTS}/test_deep.py", ("crates/**/src/**/*.rs",)),
        "test_dir": (f"{TESTS}/test_dir.py", ("benches/fast/",)),
        "test_any": (f"{TESTS}/test_any.py", None),
    }

    def picked(self, *changed):
        return contracts.select(self.CONTRACTS, list(changed))

    def test_a_change_reaches_a_contract_through_a_path_a_glob_or_a_directory(self):
        cases = [  # changed file -> the declaring contracts it reaches (test_any always runs)
            ("scripts/check.sh", ["test_paths"]),
            ("scripts/lib/gate-key.sh", ["test_paths"]),
            ("scripts/lib/gate-tools.txt.sh.bak", []),           # `*.sh` ends there
            ("scripts/lib/deeper/x.sh", []),                     # `*` stays within one segment
            ("crates/a/src/lib.rs", ["test_deep"]),
            ("crates/rdf/b/src/x/y.rs", ["test_deep"]),          # `**/` any directories, none included
            ("crates/a/tests/t.rs", []),
            ("benches/fast/queries/q.sparql", ["test_dir"]),     # a directory: everything under it
            ("benches/fastest.py", []),
            ("docs/note.md", []),
        ]
        for changed, reached in cases:
            with self.subTest(changed):
                self.assertEqual(self.picked(changed), sorted([*reached, "test_any"]), f"SPEC reaches: {changed}")

    def test_its_own_file_reaches_a_contract(self):
        self.assertEqual(self.picked(f"{TESTS}/test_deep.py"), ["test_any", "test_deep"], "SPEC own-file")

    def test_a_contract_that_declares_nothing_or_no_literal_always_runs(self):
        for source in ("import x\n", "READS = tuple(['a'])\n", 'READS = ("a", 1)\n', 'READS = ("",)\n', "READS = (\n",
                       'READS = "scripts/check.sh"\n'):
            with self.subTest(source):
                self.assertIsNone(contracts.declared(source), "SPEC no-declaration")
        self.assertEqual(contracts.declared('READS = ("a", "b/")\n'), ("a", "b/"))
        self.assertEqual(self.picked("docs/note.md"), ["test_any"], "SPEC no-declaration: always runs")
        self.assertEqual(self.picked(), ["test_any"], "SPEC no-declaration: runs with no change at all")

    def test_a_change_to_what_every_contract_reads_runs_them_all(self):
        for changed in (f"{TESTS}/isolation.py", f"{TESTS}/conftest.py", f"{TESTS}/contracts.py",
                        "benches/suite/requirements.txt", "scripts/lib/gate-tools.txt"):
            with self.subTest(changed):
                self.assertIsNone(self.picked(changed, "docs/note.md"), "SPEC every: all of them")

    def test_more_files_never_select_fewer_contracts(self):
        files = ["scripts/check.sh", "crates/a/src/lib.rs", "benches/fast/x.py", "docs/note.md", f"{TESTS}/test_dir.py"]
        everyone = sorted(self.CONTRACTS)
        for n in range(len(files) + 1):
            for fewer in combinations(files, n):
                for more in (fewer + (extra,) for extra in files if extra not in fewer):
                    small, large = self.picked(*fewer), self.picked(*more)
                    self.assertTrue(set(small if small is not None else everyone) <=
                                    set(large if large is not None else everyone), f"SPEC monotone: {fewer} {more}")


@unittest.skipUnless(shutil.which("git"), "needs git")
class Command(unittest.TestCase):
    """`contracts.py select [--commit COMMIT]`, as the gate runs it: in a repository of its own with
    declaring contracts, the crate scan, one contract that declares nothing, and a helper module that
    is no contract."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="contract-reads-")
        self.addCleanup(temp.cleanup)
        self.repo = Path(temp.name)
        tests = self.repo / TESTS
        tests.mkdir(parents=True)
        shutil.copy(ROOT / TESTS / "contracts.py", tests / "contracts.py")
        for name, text in {"test_a.py": 'READS = ("a.txt",)\n', "test_b.py": 'READS = ("b/",)\n',
                           "crate_scan.py": 'READS = ("crates/**/*.rs",)\n', "test_c.py": "import os\n",
                           "helper.py": 'READS = ("never.txt",)\n'}.items():
            (tests / name).write_text(text, newline="\n")
        self.git("init", "-q")
        self.git("add", "-A")
        self.git("commit", "-qm", "contracts")
        self.commit = self.git("rev-parse", "HEAD")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.repo), *args], capture_output=True, text=True, check=True,
                              timeout=60).stdout.strip()

    def select(self, *files, commit=None):
        done = subprocess.run([sys.executable, f"{TESTS}/contracts.py", "select", *(["--commit", commit] if commit else [])],
                              cwd=self.repo, input="".join(f"{f}\n" for f in files), capture_output=True, text=True,
                              timeout=60)
        self.assertEqual(done.returncode, 0, done.stderr)
        return done.stdout.split()

    def test_it_prints_what_the_files_reach_nothing_for_all_and_none_for_none(self):
        self.assertEqual(self.select("a.txt"), ["test_a", "test_c"], "SPEC command")
        self.assertEqual(self.select("crates/x/src/lib.rs"), ["crate_scan", "test_c"], "SPEC command: the crate scan")
        self.assertEqual(self.select("a.txt", "b/x", "crates/y.rs"), [], "SPEC command: every contract, nothing printed")
        (self.repo / TESTS / "test_c.py").write_text('READS = ("c.txt",)\n', newline="\n")
        self.assertEqual(self.select("docs/x.md"), ["none"], "SPEC command: none reached")

    def test_with_a_commit_it_reads_that_commits_declarations(self):
        (self.repo / TESTS / "test_a.py").write_text('READS = ("other.txt",)\n', newline="\n")
        self.assertEqual(self.select("a.txt", commit=self.commit), ["test_a", "test_c"], "SPEC commit: the commit's own")
        self.assertEqual(self.select("a.txt"), ["test_c"], "the work tree's")

    def test_a_usage_error_prints_nothing_to_run(self):
        done = subprocess.run([sys.executable, f"{TESTS}/contracts.py", "pick"], cwd=self.repo, capture_output=True,
                              text=True, input="", timeout=60)
        self.assertEqual((done.returncode, done.stdout), (2, ""), "SPEC command: a failure selects nothing itself")


# What a contract names, read from its source: the module-level names bound to a directory of this
# repository (`ROOT = Path(__file__).resolve().parents[3]`, `HERE = ....parent`, `X = ROOT / "a"`),
# the paths it joins to them, and the paths it gives isolation.checkout.

def tracked() -> set[str]:
    out = subprocess.run(["git", "-C", str(ROOT), "ls-files", "-z"], capture_output=True, text=True, check=True,
                         timeout=60).stdout
    return {f for f in out.split("\0") if f}


def anchored(node, names: dict[str, PurePosixPath], here: PurePosixPath) -> PurePosixPath | None:
    """The repository path NODE denotes, if it is one: `Path(__file__)` and what .resolve(),
    .parent, .parents[N] and `/ "literal"` make of it, or a name bound to one."""
    if isinstance(node, ast.Name):
        return names.get(node.id)
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == "Path" and node.args \
            and isinstance(node.args[0], ast.Name) and node.args[0].id == "__file__":
        return here
    if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute) and node.func.attr == "resolve":
        return anchored(node.func.value, names, here)
    if isinstance(node, ast.Attribute) and node.attr == "parent":
        base = anchored(node.value, names, here)
        return base.parent if base is not None else None
    if isinstance(node, ast.Subscript) and isinstance(node.value, ast.Attribute) and node.value.attr == "parents" \
            and isinstance(node.slice, ast.Constant) and isinstance(node.slice.value, int):
        base = anchored(node.value.value, names, here)
        return base.parents[node.slice.value] if base is not None else None
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div) and isinstance(node.right, ast.Constant) \
            and isinstance(node.right.value, str):
        base = anchored(node.left, names, here)
        return base / node.right.value if base is not None else None
    return None


def strings(node, constants: dict[str, list[str]]) -> list[str]:
    """The literal strings NODE is, or holds (a starred or named module-level tuple of them)."""
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return [node.value]
    if isinstance(node, ast.Starred):
        return strings(node.value, constants)
    if isinstance(node, ast.Name):
        return constants.get(node.id, [])
    if isinstance(node, ast.Attribute) and node.attr == "NS_SH":
        return ["scripts/lib/ns.sh", "benches/suite/suitekit/lock.py"]
    if isinstance(node, (ast.Tuple, ast.List)):
        return [s for e in node.elts for s in strings(e, constants)]
    return []


def named(path: str) -> tuple[set[str], list[PurePosixPath]]:
    """The repository files the contract at PATH names (joined to a repository directory, or copied
    into a checkout), and the directories it puts on sys.path."""
    tree = ast.parse((ROOT / path).read_text(encoding="utf-8"))
    here = PurePosixPath(path)
    names: dict[str, PurePosixPath] = {}
    constants: dict[str, list[str]] = {}
    for node in tree.body:
        if isinstance(node, ast.Assign) and len(node.targets) == 1 and isinstance(node.targets[0], ast.Name):
            where = anchored(node.value, names, here)
            if where is not None:
                names[node.targets[0].id] = where
            elif isinstance(node.value, (ast.Tuple, ast.List)):
                constants[node.targets[0].id] = strings(node.value, {})
    found, paths = set(), [here.parent]
    for node in ast.walk(tree):
        if isinstance(node, ast.BinOp):
            where = anchored(node, names, here)
            if where is not None:
                found.add(where.as_posix())
        elif isinstance(node, ast.Call):
            func = node.func
            name = func.attr if isinstance(func, ast.Attribute) else getattr(func, "id", "")
            if name == "checkout":
                found |= {s for arg in node.args[1:] for s in strings(arg, constants)}
            elif name == "insert" and isinstance(func, ast.Attribute) and isinstance(func.value, ast.Attribute) \
                    and func.value.attr == "path" and len(node.args) == 2:
                arg = node.args[1]
                if isinstance(arg, ast.Call) and getattr(arg.func, "id", "") == "str" and arg.args:
                    where = anchored(arg.args[0], names, here)
                    if where is not None:
                        paths.append(where)
    return found, paths


def imported(path: str, paths: list[PurePosixPath], files: set[str]) -> set[str]:
    """The repository modules the file at PATH imports, found on PATHS (and its own directory), and
    theirs, transitively."""
    seen, todo = set(), [path]
    while todo:
        current = todo.pop()
        if current in seen:
            continue
        seen.add(current)
        tree = ast.parse((ROOT / current).read_text(encoding="utf-8"))
        wanted = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                wanted |= {a.name for a in node.names}
            elif isinstance(node, ast.ImportFrom) and node.module and node.level == 0:
                wanted |= {node.module, *(f"{node.module}.{a.name}" for a in node.names)}
        for module in wanted:
            rel = module.replace(".", "/")
            for base in [PurePosixPath(current).parent, *paths]:
                hit = next((c for c in (f"{base}/{rel}.py", f"{base}/{rel}/__init__.py") if c in files), None)
                if hit:
                    todo.append(hit)
                    break
    return seen - {path}


class Declarations(unittest.TestCase):
    """This repository's contracts and what they declare."""

    maxDiff = None  # every contract and file a failure names

    @classmethod
    def setUpClass(cls):
        cls.files = tracked()
        cls.contracts = sorted(f for f in cls.files if contracts.CONTRACT.fullmatch(f))

    def test_each_declared_read_names_a_tracked_file(self):
        for path in self.contracts:
            reads = contracts.declared((ROOT / path).read_text(encoding="utf-8")) or ()
            for read in reads:
                with self.subTest(contract=path, read=read):
                    rule = contracts.pattern(read)
                    self.assertTrue(any(rule.fullmatch(f) for f in self.files),
                                    f"SPEC honest: {path} declares {read!r}, which names no tracked file")

    def test_each_file_a_contract_names_or_imports_is_among_its_reads(self):
        missing = {}
        for path in self.contracts:
            reads = contracts.declared((ROOT / path).read_text(encoding="utf-8"))
            if reads is None:
                continue  # it always runs
            found, paths = named(path)
            wanted = {f for f in found if f in self.files} | imported(path, paths, self.files)
            lacking = sorted(f for f in wanted if not contracts.reaches(path, reads, f))
            if lacking:
                missing[path] = lacking
        self.assertEqual(missing, {}, "SPEC honest: read but not declared (contract: files)")


if __name__ == "__main__":
    unittest.main()
