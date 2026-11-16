"""The crates the push tier lints and tests for a change (scripts/check.sh changed_crates, run as
`scripts/check.sh crates FILE...`).

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_changed_crates.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.

Spec: D16-637 (15 Nov 2026). The push tier for engine-v2's commit "workspace: version 1.5.0"
(ca1146da8, gate-push-a6b45993a7-1794750320) printed "changed: none" and ran no crate's clippy or
tests, though the commit changed the root Cargo.toml's [workspace.package]. A change to the root
manifest, the root lock file or the toolchain file reaches every crate: the selection names every
crate of the workspace. A file inside one crate still selects that crate alone.

Each case runs the tree's check.sh, copied alone (it needs no other file to start), in a small
workspace written to a temporary directory (two crates, one nested like crates/rdf/*), with the
workspace's own `cargo metadata`. Needs bash and cargo on PATH.
"""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[3]
# By its PATH entry: Windows looks in System32 (WSL's bash) before PATH for a bare name.
BASH = shutil.which("bash")
EVERY = {"alpha", "beta"}
# ca1146da8's files, as its push tier got them (git show --stat ca1146da8^2).
VERSION_BUMP = [".github/workflows/images.yml", "Cargo.lock", "Cargo.toml",
                "benches/nrese-bench-harness/Cargo.lock", "benches/oxigraph-comparison/Cargo.lock",
                "benches/term-reads-count/Cargo.lock", "docs/ops/openapi.json", "fuzz/Cargo.lock"]
FILES = {
    "Cargo.toml": '[workspace]\nmembers = ["crates/alpha", "crates/rdf/beta"]\nresolver = "3"\n\n'
                  '[workspace.package]\nversion = "1.4.0"\nedition = "2021"\n',
    "crates/alpha/Cargo.toml": '[package]\nname = "alpha"\nversion.workspace = true\nedition.workspace = true\n',
    "crates/alpha/src/lib.rs": "",
    "crates/rdf/beta/Cargo.toml": '[package]\nname = "beta"\nversion.workspace = true\nedition.workspace = true\n',
    "crates/rdf/beta/src/lib.rs": "",
}


class ChangedCrates(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for tool in ("bash", "cargo"):
            if not shutil.which(tool):
                raise RuntimeError(f"{tool} is not on PATH: the selection can't be run")
        temp = tempfile.TemporaryDirectory(prefix="nrese-changed-crates-")
        cls.addClassCleanup(temp.cleanup)
        cls.dir = Path(temp.name)
        for name, text in FILES.items():
            (cls.dir / name).parent.mkdir(parents=True, exist_ok=True)
            (cls.dir / name).write_text(text, newline="\n")
        (cls.dir / "scripts").mkdir()
        shutil.copy(ROOT / "scripts/check.sh", cls.dir / "scripts/check.sh")

    def selected(self, *files):
        run = subprocess.run([BASH, "scripts/check.sh", "crates", *files], cwd=self.dir, capture_output=True,
                             text=True, timeout=120, stdin=subprocess.DEVNULL)
        self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
        return set(run.stdout.split())

    def test_the_version_bump_that_ran_nothing_selects_every_crate(self):
        self.assertEqual(self.selected(*VERSION_BUMP), EVERY, "SPEC every-crate: the 1.5.0 commit's files")

    def test_each_workspace_wide_file_alone_selects_every_crate(self):
        for name in ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", ".cargo/config.toml", "clippy.toml"):
            with self.subTest(name):
                self.assertEqual(self.selected(name), EVERY, "SPEC every-crate: a workspace-wide file")

    def test_a_file_inside_one_crate_selects_that_crate_alone(self):
        self.assertEqual(self.selected("crates/rdf/beta/src/lib.rs"), {"beta"}, "SPEC one-crate")

    def test_a_crates_own_manifest_is_not_the_root_manifest(self):
        self.assertEqual(self.selected("crates/alpha/Cargo.toml"), {"alpha"}, "SPEC one-crate")

    def test_another_cargo_projects_lock_file_reaches_no_workspace_crate(self):
        self.assertEqual(self.selected("fuzz/Cargo.lock", "docs/ops/openapi.json"), set(), "SPEC other-projects")


if __name__ == "__main__":
    unittest.main()
