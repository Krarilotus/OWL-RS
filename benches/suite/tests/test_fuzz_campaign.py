"""D16-477: a campaign's failed build keeps and reports the compiler's diagnostic. A campaign runs
each target's test harness, never another executable of the same build."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import isolation


def campaign_checkout(root: Path, targets: str) -> Path:
    scripts = root / "scripts"
    (scripts / "lib").mkdir(parents=True)
    campaign = scripts / "fuzz-campaign.sh"
    shutil.copyfile(isolation.ROOT / "scripts/fuzz-campaign.sh", campaign)
    (scripts / "lib/fuzz-targets.txt").write_text(targets)
    return campaign


def executable(path: Path, body: str) -> Path:
    path.write_text("#!/bin/sh\n" + body, newline="\n")
    path.chmod(0o755)
    return path


def artifact(kind: str, name: str, test: bool, path: Path) -> str:
    """One compiler-artifact message as `cargo --message-format=json` writes it: compact, one line."""
    return json.dumps({
        "reason": "compiler-artifact", "package_id": "path+file:///w/crates/nrese-server#0.1.0",
        "manifest_path": "/w/crates/nrese-server/Cargo.toml",
        "target": {"kind": [kind], "crate_types": ["bin"], "name": name, "test": True},
        "profile": {"opt_level": "3", "test": test}, "features": [],
        "filenames": [path.as_posix()], "executable": path.as_posix(), "fresh": True}, separators=(",", ":"))


@unittest.skipUnless(shutil.which("bash"), "the campaign runs in Bash")
class BuildDiagnostics(unittest.TestCase):
    def test_failed_build_keeps_and_reports_its_stderr(self):
        # Spec: D16-477 requires the compiler's error, not only 'no test binary'.
        diagnostic = "error[E0433]: cannot find witness in classes"
        with tempfile.TemporaryDirectory(prefix="fuzz-build-diagnostic-") as directory:
            root = Path(directory)
            campaign = campaign_checkout(root, "nrese-engine:lib:witness\n")
            executable(root / "scripts/cargo-guarded.sh", f"printf '%s\\n' '{diagnostic}' >&2\nexit 101\n")
            out = root / "out"
            done = subprocess.run([shutil.which("bash"), campaign.as_posix(), "0", out.as_posix()],
                                  env=isolation.environment(), capture_output=True, text=True, timeout=10)
            self.assertIn(diagnostic, done.stderr)
            self.assertNotEqual(done.returncode, 0)
            self.assertIn(diagnostic, (out / "build-nrese-engine-lib.log").read_text())


@unittest.skipUnless(shutil.which("bash"), "the campaign runs in Bash")
class TestHarness(unittest.TestCase):
    """12 Nov 2026: an integration test's build also gives its crate's program, which a warm build
    listed after the harness; the campaign ran the program with the test filter, and every server
    run failed with 'unsupported argument'. Spec: the campaign runs the requested target's test
    harness (its kind and name, test profile), and refuses a build without exactly one, or one
    that failed after building it."""

    def campaign(self, root: Path, messages: list[str], then: str = "") -> subprocess.CompletedProcess:
        campaign = campaign_checkout(root, "nrese-server:it:http_fuzz_tests::\n")
        listing = root / "cargo.json"
        listing.write_text("".join(m + "\n" for m in messages), newline="\n")
        executable(root / "scripts/cargo-guarded.sh", f"cat '{listing.as_posix()}'\n{then}")
        # One round: the deadline is taken at 100, the first check sees 100, the next the end.
        bin_dir = root / "bin"
        bin_dir.mkdir()
        calls = root / "date-calls"
        executable(bin_dir / "date", f"echo x >> '{calls.as_posix()}'\n"
                   f"[ $(wc -l < '{calls.as_posix()}') -le 2 ] && echo 100 || echo 999999\n")
        env = isolation.environment()
        env["PATH"] = str(bin_dir) + os.pathsep + env["PATH"]
        return subprocess.run([shutil.which("bash"), campaign.as_posix(), "1", (root / "out").as_posix()],
                              env=env, capture_output=True, text=True, timeout=30)

    def test_the_harness_runs_when_the_crate_s_program_is_listed_after_it(self):
        with tempfile.TemporaryDirectory(prefix="fuzz-harness-") as directory:
            root = Path(directory)
            ran = root / "harness-ran"
            harness = executable(root / "it-harness", f"echo \"$@\" > '{ran.as_posix()}'\n")
            program = executable(root / "nrese-server",
                                 "echo 'Error: unsupported argument: \"'$1'\"'\nexit 1\n")
            messages = [artifact("test", "it", True, harness), artifact("bin", "nrese-server", False, program),
                        '{"reason":"build-finished","success":true}']
            done = self.campaign(root, messages)
            self.assertIn("DONE runs=1 seeds=1 failures=0", done.stdout, done.stdout + done.stderr)
            self.assertEqual(ran.read_text().split(), ["http_fuzz_tests::", "-q"])
            out = root / "out"
            self.assertEqual((out / "build-nrese-server-it.json").read_text().splitlines(), messages)
            self.assertEqual((out / "binaries.txt").read_text(), f"nrese-server:it {harness.as_posix()}\n")

    def test_a_build_that_fails_after_the_harness_is_refused(self):
        diagnostic = "error[E0425]: cannot find value `route` in this scope"
        with tempfile.TemporaryDirectory(prefix="fuzz-harness-") as directory:
            root = Path(directory)
            ran = root / "harness-ran"
            harness = executable(root / "it-harness", f"touch '{ran.as_posix()}'\n")
            done = self.campaign(root, [artifact("test", "it", True, harness)],
                                 then=f"printf '%s\\n' '{diagnostic}' >&2\nexit 101\n")
            self.assertNotEqual(done.returncode, 0, done.stdout)
            self.assertFalse(ran.exists(), "the harness ran after a failed build")
            self.assertIn(diagnostic, done.stderr)
            self.assertNotIn("DONE", done.stdout)

    def test_a_build_without_the_harness_is_refused(self):
        with tempfile.TemporaryDirectory(prefix="fuzz-harness-") as directory:
            root = Path(directory)
            program = executable(root / "nrese-server", "exit 0\n")
            done = self.campaign(root, [artifact("bin", "nrese-server", False, program)])
            self.assertNotEqual(done.returncode, 0, done.stdout)
            self.assertIn("no test binary for nrese-server it", done.stderr)
            self.assertNotIn("DONE", done.stdout)


if __name__ == "__main__":
    unittest.main()
