"""D16-477: a campaign's failed build keeps and reports the compiler's diagnostic."""
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

import isolation


@unittest.skipUnless(shutil.which("bash"), "the campaign runs in Bash")
class BuildDiagnostics(unittest.TestCase):
    def test_failed_build_keeps_and_reports_its_stderr(self):
        # Spec: D16-477 requires the compiler's error, not only 'no test binary'.
        diagnostic = "error[E0433]: cannot find witness in classes"
        with tempfile.TemporaryDirectory(prefix="fuzz-build-diagnostic-") as directory:
            root = Path(directory)
            scripts = root / "scripts"
            (scripts / "lib").mkdir(parents=True)
            campaign = scripts / "fuzz-campaign.sh"
            shutil.copyfile(isolation.ROOT / "scripts/fuzz-campaign.sh", campaign)
            (scripts / "lib/fuzz-targets.txt").write_text("nrese-engine:lib:witness\n")
            build = scripts / "cargo-guarded.sh"
            build.write_text(f"#!/bin/sh\nprintf '%s\\n' '{diagnostic}' >&2\nexit 101\n", newline="\n")
            build.chmod(0o755)
            out = root / "out"
            done = subprocess.run([shutil.which("bash"), campaign.as_posix(), "0", out.as_posix()],
                                  env=isolation.environment(), capture_output=True, text=True, timeout=10)
            self.assertIn(diagnostic, done.stderr)
            self.assertNotEqual(done.returncode, 0)
            self.assertIn(diagnostic, (out / "build-nrese-engine-lib.log").read_text())


if __name__ == "__main__":
    unittest.main()
