"""D16-109: a full tier requires cargo-deny in both Cargo workspaces."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

import isolation

ROOT = Path(__file__).resolve().parents[3]
CARGO = '''case "$1" in
  metadata) echo '{"packages":[{"name":"fixture"}]}' ;;
  deny)
    [ "$DENY" != missing ] || exit 101
    [ "$2" != --version ] || { echo "cargo-deny $DENY_VERSION"; exit 0; }
    echo "$*" >> "$SEEN"
    case "$DENY:$*" in
      workspace:*--manifest-path*) exit 0 ;;
      workspace:*|harness:*--manifest-path*) exit 1 ;;
    esac ;;
esac
exit 0
'''


def full_gate_root(root: Path, source: Path = ROOT) -> Path:
    """A checkout at ROOT in which `scripts/check.sh all` runs SOURCE's check.sh with its tools stubbed:
    cargo by CARGO, the steps' scripts exiting 0."""
    (root / "scripts").mkdir()
    shutil.copy(source / "scripts/check.sh", root / "scripts/check.sh")
    (root / "scripts/lib").mkdir()
    shutil.copy(source / "scripts/lib/gate-tools.txt", root / "scripts/lib/gate-tools.txt")
    for name in ("cargo-guarded.sh", "check-locks.sh", "check-names.sh", "fetch-w3c-tests.sh"):
        (root / "scripts" / name).write_text("#!/bin/sh\nexit 0\n", newline="\n")
        (root / "scripts" / name).chmod(0o755)
    binaries = root / "bin"
    binaries.mkdir()
    scripts = {
        "cargo": CARGO,
        "python": 'case "$1" in benches/suite/tests/isolation.py) exit 0 ;; esac\n'
                  'exec "$REAL_PYTHON" "$@"\n',
    }
    for name, body in scripts.items():
        path = binaries / name
        path.write_text("#!/bin/sh\n" + body, newline="\n")
        path.chmod(0o755)
    return binaries


def run_all(root: Path, binaries: Path, state: str, version: str, seen: Path) -> subprocess.CompletedProcess:
    env = isolation.inherit("NRESE_", "FLEET_", "CARGO_",
        PATH=binaries.as_posix() + os.pathsep + os.environ["PATH"],
        REAL_PYTHON=Path(sys.executable).as_posix(), NRESE_ON_BUILD_HOST="1",
        DENY=state, DENY_VERSION=version, SEEN=seen.as_posix())
    return subprocess.run([shutil.which("bash"), "scripts/check.sh", "all"],
                          cwd=root, env=env, capture_output=True, text=True, timeout=60)


class FullGate(unittest.TestCase):
    def test_deny_is_required_and_both_workspaces_are_checked(self):
        # Spec: D16-109 / the release-route brief: absent or failing cargo-deny
        # cannot produce a full-tier pass; success checks both workspaces.
        with tempfile.TemporaryDirectory(prefix="nrese-full-gate-") as directory:
            root = Path(directory)
            binaries = full_gate_root(root)
            # Spec: master's r1 F2: a version other than the checked-in pin fails
            # all, with both installed and required versions in its diagnostic.
            for state in ("missing", "workspace", "harness", "older", "newer", "ok"):
                with self.subTest(state=state):
                    seen = root / f"seen-{state}"
                    version = {"older": "0.20.1", "newer": "0.20.3"}.get(state, "0.20.2")
                    done = run_all(root, binaries, state, version, seen)
                    self.assertEqual(done.returncode, 0 if state == "ok" else 1,
                                     f"SPEC required deny: {state}\n{done.stdout}{done.stderr}")
                    if state in ("older", "newer"):
                        self.assertIn(version, done.stdout + done.stderr)
                        self.assertIn("0.20.2", done.stdout + done.stderr)
                    if state != "missing":
                        self.assertEqual(seen.read_text().splitlines(), [
                            "deny --all-features check",
                            "deny --manifest-path benches/nrese-bench-harness/Cargo.toml "
                            "--config deny.toml --all-features check"], done.stdout + done.stderr)

    def test_a_new_full_tier_step_needs_no_fixture_edit(self):
        # Spec: D16-371: the fixture is check.sh's, not a hand-picked copy: #186 (`features`) and #207
        # (console steps) each broke this test by adding a step, and cost a merge-set gate. A check.sh
        # whose full tier runs a script of a new name still passes here with cargo-deny as pinned.
        with tempfile.TemporaryDirectory(prefix="nrese-full-gate-source-") as directory:
            source = Path(directory) / "source"
            shutil.copytree(ROOT / "scripts", source / "scripts", ignore=shutil.ignore_patterns("__pycache__"))
            check = source / "scripts/check.sh"
            text = check.read_text(encoding="utf-8")
            anchor = '    step "cargo-deny version" deny_version\n'
            self.assertIn(anchor, text)
            check.write_text(text.replace(anchor, '    step "a new step" bash scripts/a-new-step.sh\n' + anchor),
                             encoding="utf-8", newline="\n")
            (source / "scripts/a-new-step.sh").write_text("#!/bin/sh\necho 'a step that needs its host'; exit 1\n",
                                                          encoding="utf-8", newline="\n")
            root = Path(directory) / "root"
            root.mkdir()
            binaries = full_gate_root(root, source)
            done = run_all(root, binaries, "ok", "0.20.2", root / "seen")
            self.assertEqual(done.returncode, 0, f"SPEC a new step, no fixture edit\n{done.stdout}{done.stderr}")
            self.assertIn("=== a new step: exit 0", done.stdout)
