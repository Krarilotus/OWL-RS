"""The images benches/sparql/images.sh builds (MillenniumDB, G-CARE) say what they were built from:
the context's git tree and the source commit, as labels on the image itself, which a cell's receipt
reads off the id that ran (driver.Suite.image_source). Orchestrator, 8 Nov 2026 13:39: only from a
clean checkout; a build at another source commit says so in its own label. Review
image-source-commit r1 (orchestrator 18:26): a checkout whose cleanliness can't be read gets no
label (F1), and the source commit its Dockerfile declares is one value, or the build refuses."""
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
    def build(self, *args: str, dirty: bool = False, corrupt: bool = False, dockerfile: str | None = None,
              ok: bool = True) -> tuple[str | None, str, str]:
        """images.sh ARGS in a checkout of its own with a docker that only records what it's asked:
        its `buildx build` line (None if it asked none), the context's tree id there, and stderr.
        `dockerfile` replaces the context's Dockerfile before the checkout is committed; `dirty`
        changes a tracked file after; `corrupt` breaks the index after, so `git status` fails."""
        with tempfile.TemporaryDirectory() as d:
            root = Path(d).resolve() / "repo"
            isolation.checkout(root, "benches/sparql/images.sh", "scripts/lib/ns.sh", "benches/suite/suitekit/lock.py",
                               "benches/suite/suitekit/dockerfile.py",
                               "benches/competitors/millenniumdb/Dockerfile", "benches/competitors/gcare/Dockerfile")
            context = "benches/competitors/" + args[0]
            if dockerfile is not None:
                (root / context / "Dockerfile").write_text(dockerfile, encoding="utf-8", newline="\n")
            git = lambda *a: subprocess.run(["git", "-C", str(root), *a], capture_output=True, text=True,  # noqa: E731
                                            check=True).stdout.strip()
            git("init", "-q")
            git("add", "-A")
            git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "x")
            tree = git("rev-parse", f"HEAD:{context}")
            if dirty:
                (root / "benches/sparql/images.sh").write_text((root / "benches/sparql/images.sh").read_text() + "\n")
            if corrupt:
                # The witness: a changed Dockerfile, and an index git status can't read (it exits
                # non-zero, printing nothing on stdout), while HEAD's trees still read.
                (root / context / "Dockerfile").write_text((root / context / "Dockerfile").read_text() + "\n")
                (root / ".git" / "index").write_bytes(b"not an index")
                self.assertNotEqual(subprocess.run(["git", "-C", str(root), "status", "--porcelain"],
                                                   capture_output=True).returncode, 0)
            bin_ = Path(d) / "bin"
            bin_.mkdir()
            (bin_ / "docker").write_text(FAKE_DOCKER, encoding="utf-8", newline="\n")
            (bin_ / "docker").chmod(0o755)
            log = Path(d) / "docker.log"
            path = subprocess.run([BASH, "-c", "echo $PATH"], capture_output=True, text=True).stdout.strip()
            env = {**isolation.environment(), "PATH": f"{bin_.as_posix()}:{path}",
                   "DOCKER_LOG": log.as_posix(), "NRESE_QUIET_DIR": (Path(d) / "no-slot").as_posix()}
            done = subprocess.run([BASH, "benches/sparql/images.sh", *args], cwd=root, env=env, capture_output=True,
                                  text=True, timeout=120)
            if ok:
                self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            else:
                self.assertNotEqual(done.returncode, 0, done.stdout + done.stderr)
            lines = log.read_text().splitlines() if log.exists() else []
            build = next((line for line in lines if line.startswith("buildx build")), None)
            return build, tree, done.stderr

    def test_a_clean_build_names_its_context_tree_and_source_commit(self):
        for image, commit in (("millenniumdb", "ecbf6dde5a5864f088eee3b0836ad6adba1d623b"), ("gcare", "d603d2b")):
            with self.subTest(image):
                build, tree, _ = self.build(image)
                self.assertIn(f"--label owlrs.built-from=benches/competitors/{image}@{tree}", build)
                self.assertIn(f"--label owlrs.source-commit={commit}", build)  # its Dockerfile's ARG default

    def test_another_source_commit_is_named(self):
        build, _, _ = self.build("millenniumdb", "0123456789abcdef0123456789abcdef01234567")
        self.assertIn("--label owlrs.source-commit=0123456789abcdef0123456789abcdef01234567", build)

    def test_a_dirty_checkout_labels_nothing(self):
        build, _, _ = self.build("millenniumdb", dirty=True)
        self.assertNotIn("owlrs.", build)

    def test_a_checkout_whose_status_fails_labels_nothing(self):
        """Review r1 F1: `git status` failing prints nothing; that is no evidence of a clean checkout,
        so a changed Dockerfile behind a broken index gets no label naming the committed tree."""
        for image in ("millenniumdb", "gcare"):
            with self.subTest(image):
                build, _, _ = self.build(image, corrupt=True)
                self.assertIsNotNone(build)
                self.assertNotIn("owlrs.", build)

    def test_a_source_commit_its_dockerfile_doesn_t_declare_once_refuses_the_build(self):
        """Review r1 follow-up (orchestrator 18:26): the declared source commit is every `ARG NAME=`
        default of the context's Dockerfile, one non-empty value; absent, empty or two values: the
        build refuses, naming why, and asks docker nothing. Also with a commit given, whose build
        argument a Dockerfile without the ARG wouldn't use."""
        first = "FROM a AS build\nARG MDB_COMMIT=1111111111111111111111111111111111111111\nRUN x\n"
        cases = {
            "absent": ("FROM a\nRUN x\n", ()),
            "empty": ("FROM a\nARG MDB_COMMIT=\nRUN x\n", ()),
            "two values": (first + "FROM b\nARG MDB_COMMIT=2222222222222222222222222222222222222222\n", ()),
            "a value and an empty one": (first + "FROM b\nARG MDB_COMMIT=\n", ()),
            "absent, a commit given": ("FROM a\nRUN x\n", ("0123456789abcdef0123456789abcdef01234567",)),
        }
        for case, (dockerfile, given) in cases.items():
            with self.subTest(case):
                build, _, stderr = self.build("millenniumdb", *given, dockerfile=dockerfile, ok=False)
                self.assertIsNone(build)
                self.assertIn("MDB_COMMIT", stderr)
        twice = first + "FROM b\nARG MDB_COMMIT=1111111111111111111111111111111111111111\n"  # twice, one value
        crlf = twice.replace("\n", "\r\n")  # a Dockerfile checked out with CRLF: the same one value
        for case, dockerfile in (("twice", twice), ("crlf", crlf)):
            with self.subTest(case):
                build, _, _ = self.build("millenniumdb", dockerfile=dockerfile)
                self.assertIn("--label owlrs.source-commit=1111111111111111111111111111111111111111 ", build)
                self.assertIn("-t nrese-bench/millenniumdb:1111111 ", build)

    def test_the_declared_commit_is_one_reader_s_for_images_sh_and_coverage(self):
        """Design check r6 N1: images.sh's declared() is suitekit/dockerfile.py's, the reader suite.py
        coverage judges an image's source commit against; its exact value, written here by hand."""
        sys.path.insert(0, str(ROOT / "benches/suite"))
        from suitekit.dockerfile import declared_source_commit
        self.assertIn('ns_python "$NS_SUITE/suitekit/dockerfile.py"', (ROOT / "benches/sparql/images.sh").read_text())
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "Dockerfile"
            path.write_bytes(b"FROM a\r\nARG MDB_COMMIT=ecbf6dde5a5864f088eee3b0836ad6adba1d623b\r\n"
                             b"ARGS MDB_COMMIT=x\nARG MDB_COMMIT_2=y\n")
            self.assertEqual(declared_source_commit(path, "MDB_COMMIT"), "ecbf6dde5a5864f088eee3b0836ad6adba1d623b")
            with self.assertRaises(ValueError):
                declared_source_commit(Path(d) / "missing", "MDB_COMMIT")


if __name__ == "__main__":
    unittest.main()
