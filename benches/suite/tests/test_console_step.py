"""The console's step of a tier (scripts/check.sh `console`, run by the push tier for a change under
apps/nrese-console/, by the Windows tier and by `all`): its typecheck and tests run wherever the tier does. A gate's
checkout is fresh, without node_modules; the step installs the console's dependencies from its
lockfile there, and a host that can't install them fails the step by name. It never passes the
console unchecked (before 8 Nov 2026 it printed "skipped" and passed).

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_console_step.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

import isolation  # the suite's git environment policy (inherit())

ROOT = Path(__file__).resolve().parents[3]
BASH = shutil.which("bash")
# A caller's gate, slot, fleet or cargo variables would make the tier behave as its caller's does.
LEAKS = ("NRESE_", "FLEET_", "CARGO_BUILD_", "CARGO_TARGET_DIR")
# Stand-ins, each call one line in $FAKE/calls. cargo succeeds (metadata: one package, foo); the
# build admission (scripts/cargo-guarded.sh), the W3C data fetch and the suite contracts succeed;
# npm fails `ci` while $FAKE/no-install exists; npx succeeds.
CARGO = """#!/bin/sh
echo "cargo $*" >> "$FAKE/calls"
[ "$1" != metadata ] || echo '{"packages":[{"name":"foo"}]}'
exit 0
"""
STUBS = {
    "scripts/cargo-guarded.sh": '#!/bin/sh\necho "guarded $*" >> "$FAKE/calls"\nexit 0\n',
    "scripts/fetch-w3c-tests.sh": '#!/bin/sh\necho "w3c" >> "$FAKE/calls"\nexit 0\n',
    "benches/suite/tests/isolation.py": 'import os\nopen(os.environ["FAKE"] + "/calls", "a").write("suite\\n")\n',
}
NPM = """#!/bin/sh
echo "npm $*" >> "$FAKE/calls"
case " $* " in
  *" ci "*) [ ! -e "$FAKE/no-install" ] || { echo "npm ci: cannot install" >&2; exit 1; } ;;
esac
exit 0
"""
NPX = '#!/bin/sh\necho "npx $*" >> "$FAKE/calls"\nexit 0\n'
CONSOLE_FILE = "apps/nrese-console/src/App.tsx"


class ConsoleStep(unittest.TestCase):
    """A scratch repository with the console's files and check.sh, where origin's refactor/engine-v2
    is HEAD (nothing of nrese-exec changed), running the push tier as a build host does: the gate's
    file list names one console file."""

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="nrese-console-step-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.fake = self.root / "fake"
        (self.fake / "bin").mkdir(parents=True)
        for name, text in (("cargo", CARGO), ("npm", NPM), ("npx", NPX)):
            (self.fake / "bin" / name).write_text(text, newline="\n")
            (self.fake / "bin" / name).chmod(0o755)
        gitconfig = self.root / "gitconfig"
        gitconfig.write_text("[user]\n\tname = t\n\temail = t@t\n[core]\n\tautocrlf = false\n"
                             "[init]\n\tdefaultBranch = main\n")
        self.repo = self.root / "repo"
        self.files = self.root / "files"
        self.files.write_text(f"{CONSOLE_FILE}\n", newline="\n")
        self.env = isolation.inherit(
            *LEAKS, git={"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": gitconfig.as_posix()},
            PATH=(self.fake / "bin").as_posix() + os.pathsep + os.environ.get("PATH", ""),
            FAKE=self.fake.as_posix(), NRESE_ON_BUILD_HOST="1", NRESE_FILES_FROM=self.files.as_posix())
        self.git(self.root, "init", "-q", str(self.repo))
        (self.repo / "scripts").mkdir()
        shutil.copy(ROOT / "scripts/check.sh", self.repo / "scripts/check.sh")
        for rel, text in {**STUBS, "apps/nrese-console/package.json": "{}\n",
                          "apps/nrese-console/package-lock.json": "{}\n", CONSOLE_FILE: "export {};\n"}.items():
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            (self.repo / rel).write_text(text, newline="\n")
        self.git(self.repo, "add", "-A")
        self.git(self.repo, "commit", "-q", "-m", "base")
        self.git(self.repo, "update-ref", "refs/remotes/origin/refactor/engine-v2", "HEAD")

    def git(self, where, *args):
        return subprocess.run(["git", "-C", str(where), *args], check=True, capture_output=True, text=True,
                              env=self.env).stdout.strip()

    def push_tier(self):
        return subprocess.run([BASH, "scripts/check.sh", "push"], cwd=self.repo, capture_output=True, text=True,
                              env=self.env, timeout=120, stdin=subprocess.DEVNULL)

    def windows_tier(self):
        env = {**self.env, "FLEET_OUT": (self.root / "out").as_posix()}
        env.pop("NRESE_FILES_FROM")
        (self.root / "out").mkdir(exist_ok=True)
        return subprocess.run([BASH, "scripts/check.sh", "windows"], cwd=self.repo, capture_output=True, text=True,
                              env=env, timeout=120, stdin=subprocess.DEVNULL)

    def calls(self, tool):
        path = self.fake / "calls"
        lines = path.read_text().splitlines() if path.exists() else []
        return [line for line in lines if line.split()[0] == tool]

    def test_a_fresh_checkout_installs_from_the_lockfile_then_checks(self):
        done = self.push_tier()
        out = done.stdout + done.stderr
        self.assertEqual(self.calls("npm")[:1], ["npm --prefix apps/nrese-console ci --no-audit --no-fund"],
                         f"SPEC console-step-installs: dependencies from the lockfile first:\n{out}")
        self.assertIn("npm --prefix apps/nrese-console run typecheck", self.calls("npm"),
                      f"SPEC console-step-checks: the typecheck runs:\n{out}")
        self.assertEqual(self.calls("npx"), ["npx --prefix apps/nrese-console vitest run --root apps/nrese-console"],
                         f"SPEC console-step-checks: the tests run:\n{out}")
        self.assertEqual(done.returncode, 0, f"SPEC console-step-checks: the tier passes:\n{out}")

    def test_a_host_that_cant_install_fails_the_step_by_name(self):
        (self.fake / "no-install").write_text("")
        done = self.push_tier()
        out = done.stdout + done.stderr
        self.assertNotEqual(done.returncode, 0, f"SPEC console-step-never-skips: the tier fails:\n{out}")
        self.assertRegex(done.stdout, r"(?m)^  1  console dependencies \(",
                         f"SPEC console-step-never-skips: the summary names the step:\n{out}")
        self.assertNotIn("skipped", out, "SPEC console-step-never-skips: nothing passes as skipped")

    def test_the_windows_tier_checks_the_console_too(self):
        # The Windows tier runs everything, whatever changed: the console's checks included (before
        # 8 Nov 2026 no Windows gate ran them).
        done = self.windows_tier()
        out = done.stdout + done.stderr
        self.assertEqual(self.calls("npm")[:1], ["npm --prefix apps/nrese-console ci --no-audit --no-fund"],
                         f"SPEC console-step-windows: the dependencies from the lockfile:\n{out}")
        self.assertEqual(self.calls("npx"), ["npx --prefix apps/nrese-console vitest run --root apps/nrese-console"],
                         f"SPEC console-step-windows: the console's tests run:\n{out}")
        self.assertRegex(done.stdout, r"(?m)^  0  console tests \(",
                         f"SPEC console-step-windows: as a step of the tier's summary:\n{out}")
        self.assertEqual(done.returncode, 0, out)

    def test_dependencies_left_by_an_earlier_tree_are_installed_again(self):
        # sol-loop r1 S1: the Linux gate's worktree stays from one gate to the next, and `git clean -fd`
        # keeps the ignored node_modules: an earlier tree's dependencies would stand in for this one's
        # lockfile.
        (self.repo / "apps/nrese-console/node_modules").mkdir()
        done = self.push_tier()
        out = done.stdout + done.stderr
        self.assertEqual(self.calls("npm")[:1], ["npm --prefix apps/nrese-console ci --no-audit --no-fund"],
                         f"SPEC console-step-installs: the lockfile's dependencies, whatever is installed:\n{out}")
        self.assertEqual(len(self.calls("npx")), 1, f"SPEC console-step-checks: the tests run:\n{out}")
        self.assertEqual(done.returncode, 0, out)


if __name__ == "__main__":
    unittest.main()
