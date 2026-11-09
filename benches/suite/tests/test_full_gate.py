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


class FullGate(unittest.TestCase):
    def test_deny_is_required_and_both_workspaces_are_checked(self):
        # Spec: D16-109 / the release-route brief: absent or failing cargo-deny
        # cannot produce a full-tier pass; success checks both workspaces.
        with tempfile.TemporaryDirectory(prefix="nrese-full-gate-") as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            shutil.copy(ROOT / "scripts/check.sh", root / "scripts/check.sh")
            for name in ("cargo-guarded.sh", "check-locks.sh", "check-names.sh", "fetch-w3c-tests.sh"):
                (root / "scripts" / name).write_text("#!/bin/sh\nexit 0\n", newline="\n")
                (root / "scripts" / name).chmod(0o755)
            binaries = root / "bin"
            binaries.mkdir()
            scripts = {
                "cargo": '''case "$1" in
  metadata) echo '{"packages":[{"name":"fixture"}]}' ;;
  deny)
    [ "$DENY" != missing ] || exit 101
    [ "$2" != --version ] || exit 0
    echo "$*" >> "$SEEN"
    case "$DENY:$*" in
      workspace:*--manifest-path*) exit 0 ;;
      workspace:*|harness:*--manifest-path*) exit 1 ;;
    esac ;;
esac
exit 0
''',
                "python": 'case "$1" in benches/suite/tests/isolation.py) exit 0 ;; esac\n'
                          'exec "$REAL_PYTHON" "$@"\n',
            }
            for name, body in scripts.items():
                path = binaries / name
                path.write_text("#!/bin/sh\n" + body, newline="\n")
                path.chmod(0o755)
            for state in ("missing", "workspace", "harness", "ok"):
                with self.subTest(state=state):
                    seen = root / f"seen-{state}"
                    env = isolation.inherit("NRESE_", "FLEET_", "CARGO_",
                        PATH=binaries.as_posix() + os.pathsep + os.environ["PATH"],
                        REAL_PYTHON=Path(sys.executable).as_posix(), NRESE_ON_BUILD_HOST="1",
                        DENY=state, SEEN=seen.as_posix())
                    done = subprocess.run([shutil.which("bash"), "scripts/check.sh", "all"],
                                          cwd=root, env=env, capture_output=True, text=True, timeout=60)
                    self.assertEqual(done.returncode, 0 if state == "ok" else 1,
                                     f"SPEC required deny: {state}\n{done.stdout}{done.stderr}")
                    if state != "missing":
                        self.assertEqual(seen.read_text().splitlines(), [
                            "deny --all-features check",
                            "deny --manifest-path benches/nrese-bench-harness/Cargo.toml "
                            "--config deny.toml --all-features check"], done.stdout + done.stderr)
