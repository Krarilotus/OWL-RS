"""scripts/lib/changed-crates.sh: the crates the push tier lints and tests for a change.

    python3 -B scripts/tests/test_changed_crates.py

Spec: D16-637 (15 Nov 2026). The push tier for engine-v2's commit "workspace: version 1.5.0"
(ca1146da8, gate-push-a6b45993a7-1794750320) printed "changed: none" and ran no crate's clippy or
tests, though the commit changed the root Cargo.toml's [workspace.package]. A change to the root
manifest, the root lock file or the toolchain file reaches every crate: the selection names every
crate of the workspace. A file inside one crate still selects that crate alone.

Each case runs the library's changed_crates through bash in a small workspace written to a
temporary directory (two crates, one nested like crates/rdf/*), with the workspace's own `cargo
metadata`. Needs bash and cargo on PATH.
"""
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

LIB = os.path.join(os.path.dirname(os.path.abspath(__file__)), os.pardir, "lib", "changed-crates.sh")
EVERY = {"alpha", "beta"}
# ca1146da8's files, as its push tier got them (git show --stat ca1146da8^2).
VERSION_BUMP = [".github/workflows/images.yml", "Cargo.lock", "Cargo.toml",
                "benches/nrese-bench-harness/Cargo.lock", "benches/oxigraph-comparison/Cargo.lock",
                "benches/term-reads-count/Cargo.lock", "docs/ops/openapi.json", "fuzz/Cargo.lock"]


def posix(path):
    return os.path.abspath(path).replace("\\", "/")


class ChangedCrates(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for tool in ("bash", "cargo"):
            if not shutil.which(tool):
                raise RuntimeError(f"{tool} is not on PATH: the selection can't be run")
        # By its PATH entry: Windows looks in System32 (WSL's bash) before PATH for a bare name.
        cls.bash = shutil.which("bash")
        cls.dir = tempfile.mkdtemp(prefix="changed-crates-")
        files = {
            "Cargo.toml": '[workspace]\nmembers = ["crates/alpha", "crates/rdf/beta"]\nresolver = "3"\n\n'
                          '[workspace.package]\nversion = "1.4.0"\nedition = "2021"\n',
            "crates/alpha/Cargo.toml": '[package]\nname = "alpha"\nversion.workspace = true\nedition.workspace = true\n',
            "crates/alpha/src/lib.rs": "",
            "crates/rdf/beta/Cargo.toml": '[package]\nname = "beta"\nversion.workspace = true\nedition.workspace = true\n',
            "crates/rdf/beta/src/lib.rs": "",
        }
        for name, text in files.items():
            path = os.path.join(cls.dir, name)
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w", encoding="utf-8", newline="\n") as f:
                f.write(text)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.dir)

    def selected(self, *files):
        script = 'python() { "$PY" "$@"; }; . "$LIB"; changed_crates "$@"'
        env = dict(os.environ, PY=posix(sys.executable), LIB=posix(LIB))
        run = subprocess.run([self.bash, "-c", script, "bash", *files], cwd=self.dir, env=env,
                             capture_output=True, text=True, timeout=120)
        self.assertEqual(run.returncode, 0, run.stderr)
        return set(run.stdout.split())

    def test_the_version_bump_that_ran_nothing_selects_every_crate(self):
        self.assertEqual(self.selected(*VERSION_BUMP), EVERY)

    def test_each_workspace_wide_file_alone_selects_every_crate(self):
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml", "clippy.toml"):
            with self.subTest(name):
                self.assertEqual(self.selected(name), EVERY)

    def test_a_file_inside_one_crate_selects_that_crate_alone(self):
        self.assertEqual(self.selected("crates/rdf/beta/src/lib.rs"), {"beta"})

    def test_a_crates_own_manifest_is_not_the_root_manifest(self):
        self.assertEqual(self.selected("crates/alpha/Cargo.toml"), {"alpha"})

    def test_another_cargo_projects_lock_file_reaches_no_workspace_crate(self):
        self.assertEqual(self.selected("fuzz/Cargo.lock", "docs/ops/openapi.json"), set())


if __name__ == "__main__":
    unittest.main()
