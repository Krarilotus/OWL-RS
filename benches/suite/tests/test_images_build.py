"""The images benches/sparql/images.sh builds (MillenniumDB, G-CARE) say what they were built from:
the context's git tree and the source commit, as labels on the image itself, which a cell's receipt
reads off the id that ran (driver.Suite.image_source). Orchestrator, 8 Nov 2026 13:39: only from a
clean checkout; a build at another source commit says so in its own label."""
from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import isolation  # noqa: E402
from test_job_namespace import BASH  # noqa: E402

ROOT = Path(__file__).resolve().parents[3]
FAKE_DOCKER = '#!/bin/sh\nprintf "%s\\n" "$*" >> "$DOCKER_LOG"\nexit 0\n'


class Labels(unittest.TestCase):
    def build(self, *args: str, dirty: bool = False) -> tuple[str, str]:
        """images.sh ARGS in a checkout of its own with a docker that only records what it's asked:
        the `buildx build` line, and the context's tree id there."""
        with tempfile.TemporaryDirectory() as d:
            root = Path(d).resolve() / "repo"
            isolation.checkout(root, "benches/sparql/images.sh", "scripts/lib/ns.sh", "benches/suite/suitekit/lock.py",
                               "benches/competitors/millenniumdb/Dockerfile", "benches/competitors/gcare/Dockerfile")
            git = lambda *a: subprocess.run(["git", "-C", str(root), *a], capture_output=True, text=True,  # noqa: E731
                                            check=True).stdout.strip()
            git("init", "-q")
            git("add", "-A")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "x")
            context = "benches/competitors/" + args[0]
            tree = git("rev-parse", f"HEAD:{context}")
            if dirty:
                (root / "benches/sparql/images.sh").write_text((root / "benches/sparql/images.sh").read_text() + "\n")
            bin_ = Path(d) / "bin"
            bin_.mkdir()
            (bin_ / "docker").write_text(FAKE_DOCKER, encoding="utf-8", newline="\n")
            (bin_ / "docker").chmod(0o755)
            log = Path(d) / "docker.log"
            env = {**isolation.environment(), "PATH": f"{bin_.as_posix()}:{subprocess.run([BASH, '-c', 'echo $PATH'], capture_output=True, text=True).stdout.strip()}",
                   "DOCKER_LOG": log.as_posix(), "NRESE_QUIET_DIR": (Path(d) / "no-slot").as_posix()}
            done = subprocess.run([BASH, "benches/sparql/images.sh", *args], cwd=root, env=env, capture_output=True,
                                  text=True, timeout=120)
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            build = next(line for line in log.read_text().splitlines() if line.startswith("buildx build"))
            return build, tree

    def test_a_clean_build_names_its_context_tree_and_source_commit(self):
        for image, commit in (("millenniumdb", "ecbf6dde5a5864f088eee3b0836ad6adba1d623b"), ("gcare", "d603d2b")):
            with self.subTest(image):
                build, tree = self.build(image)
                self.assertIn(f"--label owlrs.built-from=benches/competitors/{image}@{tree}", build)
                self.assertIn(f"--label owlrs.source-commit={commit}", build)  # its Dockerfile's ARG default

    def test_another_source_commit_is_named_and_a_dirty_checkout_labels_nothing(self):
        build, _ = self.build("millenniumdb", "0123456789abcdef0123456789abcdef01234567")
        self.assertIn("--label owlrs.source-commit=0123456789abcdef0123456789abcdef01234567", build)
        build, _ = self.build("millenniumdb", dirty=True)
        self.assertNotIn("owlrs.", build)


if __name__ == "__main__":
    unittest.main()
