"""What benchmark runs leave on a shared host's disk (notes/2026-10-19-office-disk.md): a gate's
prepared data once per host, not once per gate name; the cleanup's volumes by our names alone,
and Docker's build cache bounded."""
from __future__ import annotations

import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
sys.path.insert(0, str(ROOT / "benches" / "fast"))
import fast  # noqa: E402
import isolation  # noqa: E402
from test_job_namespace import cleanup_checkout  # noqa: E402


class SharedPreparedData(unittest.TestCase):
    def test_a_second_gate_links_the_first_gate_s_data_in_one_read_only_copy(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            shared, first, second = tmp / "shared", tmp / "gate-a" / "fast-data", tmp / "gate-b" / "fast-data"
            first.mkdir(parents=True)
            (first / "geo4-points10.nq").write_text("<a> <b> <c> <g> .\n")
            (first / "geo4-points10.nq.expect.json").write_text('{"expect": {}}')
            index = {}
            try:
                fast.share(fast.spec_files(first / "geo4-points10.nq"), index, shared, first)
                self.assertEqual(list(index), ["geo4-points10.nq"])
                self.assertEqual(len(list((shared / "blobs").iterdir())), 2)
                self.assertFalse(os.stat(first / "geo4-points10.nq").st_mode & stat.S_IWUSR)  # read-only
                self.assertTrue(fast.adopt(second / "geo4-points10.nq", index, shared, second))
                for name in ("geo4-points10.nq", "geo4-points10.nq.expect.json"):
                    self.assertTrue(os.path.samefile(first / name, second / name))
                # Sharing again (the second gate's prepare) keeps the one copy.
                fast.share(fast.spec_files(second / "geo4-points10.nq"), index, shared, second)
                self.assertEqual(len(list((shared / "blobs").iterdir())), 2)
                self.assertFalse(fast.adopt(second / "missing.nt", index, shared, second))
            finally:
                for path in (shared / "blobs").iterdir():
                    os.chmod(path, stat.S_IWUSR | stat.S_IRUSR)


# A POSIX bash: Git's on Windows (as fast.py's NRESE_BASH), not WSL's launcher.
BASH = os.environ.get("NRESE_BASH") or next(
    (b for b in (["C:/Program Files/Git/bin/bash.exe"] if os.name == "nt" else []) if Path(b).is_file()),
    shutil.which("bash"))


@unittest.skipUnless(BASH, "needs bash")
class CleanupCache(unittest.TestCase):
    def test_the_build_cache_is_bounded_and_nothing_unlabelled_is_pruned_or_removed(self):
        """Office's disk, 19 Oct 2026: bench-cleanup never touched Docker's build cache. It is
        bounded now; volumes stay the label sweep's (scripts/lib/ns.sh), never a wildcard prune."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            log = tmp / "docker.log"
            stub = tmp / "docker"
            root, outside = cleanup_checkout(tmp / "work"), tmp / "outside"
            leftovers = ("benches/reasoning/results/r/x.inferred.nt", "tmp/bench-run-1/scratch")
            for path in (base / rel for base in (root, outside) for rel in leftovers):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("")
            stub.write_text(f"""#!/usr/bin/env bash
case "$1 $2" in
  "info "*) exit 0 ;;
  "ps -a") ;;
  "volume ls") case "$*" in *label*) ;; *) printf '%s\\n' nrese-suite-r1-store colleague-db ;; esac ;;
  "network ls") ;;
  "volume inspect") exit 1 ;;
  "images "*) ;;
  *) echo "$*" >> '{log.as_posix()}' ;;
esac
""", newline="\n")
            stub.chmod(0o755)
            env = isolation.environment(NRESE_DOCKER=stub.as_posix(), NRESE_BENCH_KEEP="1",
                                        NRESE_BENCH_LOCAL=str(tmp / "local"))
            done = subprocess.run([BASH, (root / "scripts" / "bench-cleanup.sh").as_posix()], env=env, capture_output=True,
                                  text=True, timeout=120)
            self.assertEqual(done.returncode, 0, done.stderr)
            calls = log.read_text().splitlines()
            self.assertIn("builder prune -f --keep-storage 10GB", calls)
            self.assertFalse([c for c in calls if ("prune" in c and "builder" not in c) or c.startswith("volume rm")],
                             calls)
            # Its own checkout's run leftovers go; another directory's stay.
            self.assertEqual([(base / rel).exists() for base in (root, outside) for rel in leftovers],
                             [False, False, True, True])


if __name__ == "__main__":
    unittest.main()
