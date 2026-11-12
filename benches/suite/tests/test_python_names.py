"""The gate's undefined-name check (scripts/check-python-names.py, run by the push tier over the
changed Python under benches/ and scripts/): a name no scope provides fails the tier, and nothing
else pyflakes reports does. A suite test that replaces a whole function never runs its body, so
before 12 Nov 2026 a name missing there (`ROOT` after #132's merge) passed every test and failed
only on a fleet host.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_python_names.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import isolation  # the suite's git environment policy (inherit())

ROOT = Path(__file__).resolve().parents[3]
BASH = shutil.which("bash")
CHECKER = ROOT / "scripts" / "check-python-names.py"
LEAKS = ("NRESE_", "FLEET_", "CARGO_BUILD_", "CARGO_TARGET_DIR")
# The shape of #132's miss: a module-level name a function reads and nothing defines.
MISSING = "import json\n\n\ndef path():\n    return ROOT / 'x'\n"


@unittest.skipUnless(importlib.util.find_spec("pyflakes"), "pyflakes: the gate's suite environment has it "
                     "(benches/suite/requirements.txt)")
class Checker(unittest.TestCase):
    def check(self, *texts):
        with tempfile.TemporaryDirectory(prefix="nrese-python-names-") as tmp:
            paths = []
            for i, text in enumerate(texts):
                paths.append(Path(tmp) / f"m{i}.py")
                paths[-1].write_text(text, newline="\n")
            done = subprocess.run([sys.executable, str(CHECKER), *map(str, paths), str(Path(tmp) / "deleted.py")],
                                  capture_output=True, text=True, timeout=60)
            return done.returncode, done.stdout.replace(tmp, "TMP")

    def test_a_name_nothing_defines_fails_and_nothing_else_is_reported(self):
        code, out = self.check(MISSING, "import os\n")
        self.assertEqual(code, 1, f"SPEC python-names-undefined: an undefined name fails:\n{out}")
        self.assertEqual(out.replace("\\", "/").splitlines(), ["TMP/m0.py:5:12: undefined name 'ROOT'"],
                         "SPEC python-names-only-undefined: that name, by file and line; the unused imports aren't "
                         "reported")

    def test_files_whose_names_all_resolve_pass(self):
        code, out = self.check("import os\n\n\ndef path():\n    return os.sep\n", "x = 1\n")
        self.assertEqual((code, out), (0, ""), "SPEC python-names-undefined: nothing undefined passes")


# Stand-ins, each call one line in $FAKE/calls: cargo (metadata: no package), the W3C data fetch, the
# name lint and the checker itself.
CARGO = '#!/bin/sh\necho "cargo $*" >> "$FAKE/calls"\n[ "$1" != metadata ] || echo \'{"packages":[]}\'\nexit 0\n'
STUBS = {
    "scripts/fetch-w3c-tests.sh": '#!/bin/sh\nexit 0\n',
    "scripts/check-names.sh": '#!/bin/sh\nexit 0\n',
    "scripts/check-python-names.py": 'import os, sys\nopen(os.environ["FAKE"] + "/calls", "a")'
                                     '.write("names " + " ".join(sys.argv[1:]) + "\\n")\n',
}


@unittest.skipUnless(BASH, "the push tier is a bash script")
class PushTier(unittest.TestCase):
    """A scratch repository with check.sh, where origin's refactor/engine-v2 is HEAD, running the push
    tier as a build host does over a given file list."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="nrese-python-names-tier-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.fake = self.root / "fake"
        (self.fake / "bin").mkdir(parents=True)
        (self.fake / "bin" / "cargo").write_text(CARGO, newline="\n")
        (self.fake / "bin" / "cargo").chmod(0o755)
        gitconfig = self.root / "gitconfig"
        gitconfig.write_text("[user]\n\tname = t\n\temail = t@t\n[core]\n\tautocrlf = false\n"
                             "[init]\n\tdefaultBranch = main\n")
        self.repo = self.root / "repo"
        self.files = self.root / "files"
        self.env = isolation.inherit(
            *LEAKS, git={"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": gitconfig.as_posix()},
            PATH=(self.fake / "bin").as_posix() + os.pathsep + os.environ.get("PATH", ""),
            FAKE=self.fake.as_posix(), NRESE_ON_BUILD_HOST="1", NRESE_FILES_FROM=self.files.as_posix())
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True, capture_output=True, env=self.env)
        (self.repo / "scripts").mkdir()
        shutil.copy(ROOT / "scripts/check.sh", self.repo / "scripts/check.sh")
        for rel, text in STUBS.items():
            (self.repo / rel).write_text(text, newline="\n")
        for args in (["add", "-A"], ["commit", "-q", "-m", "base"],
                     ["update-ref", "refs/remotes/origin/refactor/engine-v2", "HEAD"]):
            subprocess.run(["git", "-C", str(self.repo), *args], check=True, capture_output=True, env=self.env)

    def test_the_changed_python_under_benches_and_scripts_is_checked(self):
        self.files.write_text("benches/fast/fast.py\nscripts/seed-sweep.py\napps/x/tool.py\nbenches/fast/CATALOG.md\n",
                              newline="\n")
        done = subprocess.run([BASH, "scripts/check.sh", "push"], cwd=self.repo, capture_output=True, text=True,
                              env=self.env, timeout=120, stdin=subprocess.DEVNULL)
        calls = (self.fake / "calls").read_text().splitlines() if (self.fake / "calls").exists() else []
        self.assertEqual([c for c in calls if c.startswith("names")], ["names benches/fast/fast.py scripts/seed-sweep.py"],
                         f"SPEC python-names-push-tier: the changed Python under benches/ and scripts/, once:\n"
                         f"{done.stdout}{done.stderr}")


if __name__ == "__main__":
    unittest.main()
