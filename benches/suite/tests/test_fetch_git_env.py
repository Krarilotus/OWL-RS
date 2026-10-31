"""Offline regression for fetch-w3c-tests.sh's hook-local Git environment isolation.

Runs the actual script prefix and fetch(), never its network suite invocations.
All Git writes and both fixed/old-prefix cases are confined to TemporaryDirectory.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

import isolation


# Also works unchanged under benches/suite/tests when integrated for discovery.
FETCH_SCRIPT = Path(__file__).resolve().parents[3] / "scripts" / "fetch-w3c-tests.sh"
BASH = Path("C:/Program Files/Git/bin/bash.exe") if os.name == "nt" else shutil.which("bash")


def shell_path(path: Path) -> str:
    value = path.resolve().as_posix()
    return f"/{value[0].lower()}{value[2:]}" if os.name == "nt" else value


def fixture_environment() -> dict[str, str]:
    # The suite's git policy (never let a hook redirect fixture setup), offline.
    return isolation.inherit(GIT_ALLOW_PROTOCOL="file", GIT_TERMINAL_PROMPT="0")


class FetchGitEnvironment(unittest.TestCase):
    def exercise(self, cached: bool, old_prefix: bool = False) -> None:
        env = fixture_environment()
        with tempfile.TemporaryDirectory(prefix="owlrs-fetch-hook-") as temporary:
            root = Path(temporary).resolve()
            outer, fixture, target = (root / name for name in ("outer", "fixture", "cache"))

            def git(repo: Path, *args: str, extra_env: dict | None = None) -> str:
                return subprocess.check_output(
                    ["git", "-C", str(repo), *args], env={**env, **(extra_env or {})},
                    text=True, encoding="utf-8", stderr=subprocess.STDOUT).strip()

            def repository(repo: Path, name: str) -> str:
                repo.mkdir()
                git(repo, "init", "-q")
                (repo / "suite").mkdir()
                (repo / "suite" / "sentinel.txt").write_bytes(name.encode() + b"\n")
                git(repo, "add", "suite/sentinel.txt")
                git(repo, "-c", "user.name=Offline Fixture", "-c", "user.email=fixture@example.invalid",
                    "-c", "commit.gpgsign=false", "commit", "-qm", name)
                return git(repo, "rev-parse", "HEAD")

            outer_head = repository(outer, "outer untouched")
            commit = repository(fixture, "fixture data")
            self.assertNotEqual(outer_head, commit)
            git(outer, "remote", "add", "origin", "https://example.invalid/outer-sentinel.git")
            hook_index = outer / "hook.index"
            shutil.copyfile(outer / ".git" / "index", hook_index)
            (outer / "staged-only.txt").write_bytes(b"virtual staged index\n")
            git(outer, "add", "staged-only.txt", extra_env={"GIT_INDEX_FILE": str(hook_index)})

            if cached:
                target.mkdir()
                git(target, "init", "-q")
                git(target, "fetch", "-q", str(fixture), commit)
                git(target, "checkout", "-q", "FETCH_HEAD")
                # The cached branch must restore bytes, not merely report success.
                (target / "suite" / "sentinel.txt").write_bytes(b"replace this cached content\n")

            snapshot_paths = [outer / ".git" / "HEAD", outer / ".git" / "config",
                              outer / ".git" / "index", hook_index,
                              outer / "suite" / "sentinel.txt", outer / "staged-only.txt"]
            before = {path: path.read_bytes() for path in snapshot_paths}
            refs = git(outer, "show-ref")

            source = FETCH_SCRIPT.read_text(encoding="utf-8")
            prefix, separator, _ = source.partition("\nfetch w3c/rdf-tests ")
            self.assertTrue(separator, "could not isolate the actual fetch-script prefix")
            if old_prefix:
                prefix, count = re.subn(
                    r'(?m)^while IFS= read -r variable; do\n  unset "\$variable"\n'
                    r'done < <\(git rev-parse --local-env-vars\)\n', "", prefix)
                self.assertEqual(count, 1, "negative control must remove exactly the existing env fix")
            child = root / "fetch-prefix.sh"
            child.write_text(prefix + '\nfetch fixture/offline "$FIXTURE_COMMIT" "$TARGET" suite\n',
                             encoding="utf-8", newline="\n")
            fetch_log = root / "fetch.log"
            hook = {"GIT_DIR": shell_path(outer / ".git"), "GIT_COMMON_DIR": shell_path(outer / ".git"),
                    "GIT_WORK_TREE": shell_path(outer), "GIT_INDEX_FILE": shell_path(hook_index),
                    "GIT_PREFIX": "hook-context/"}
            child_env = {**env, **hook, "CHILD_SCRIPT": shell_path(child), "TARGET": shell_path(target),
                         "LOCAL_FIXTURE": shell_path(fixture), "FIXTURE_COMMIT": commit,
                         "FETCH_LOG": shell_path(fetch_log)}
            # Only network transport is replaced. Identity, init, remotes, config,
            # sparse-checkout and checkout all execute real Git with their actual env.
            parent = r'''
set -euo pipefail
git() {
  if [ "${1:-}" = -C ] && [ "${3:-}" = fetch ]; then
    printf 'local fetch\n' >> "$FETCH_LOG"
    command git -C "$2" fetch -q --no-tags "$LOCAL_FIXTURE" "$FIXTURE_COMMIT"
  else
    command git "$@"
  fi
}
export -f git
before=$(declare -p GIT_DIR GIT_COMMON_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX)
rc=0
bash --noprofile --norc "$CHILD_SCRIPT" || rc=$?
[ "$before" = "$(declare -p GIT_DIR GIT_COMMON_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_PREFIX)" ]
printf 'PARENT_ENV_RETAINED\n'
exit "$rc"
'''
            done = subprocess.run([str(BASH), "--noprofile", "--norc", "-c", parent], cwd=root,
                                  env=child_env, text=True, encoding="utf-8", errors="replace",
                                  stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=30)
            self.assertIn("PARENT_ENV_RETAINED", done.stdout)
            if old_prefix:
                self.assertNotEqual(done.returncode, 0, "old prefix unexpectedly succeeded: " + done.stdout)
                self.assertIn("remote origin already exists", done.stdout)
                self.assertFalse(fetch_log.exists(), "negative control must fail before any fetch")
                return
            self.assertEqual(done.returncode, 0, done.stdout)
            self.assertEqual(git(outer, "show-ref"), refs)
            self.assertEqual({path: path.read_bytes() for path in snapshot_paths}, before)
            self.assertEqual(git(target, "rev-parse", "HEAD"), commit)
            self.assertEqual((target / "suite" / "sentinel.txt").read_bytes(), b"fixture data\n")
            self.assertEqual(fetch_log.read_text() if fetch_log.exists() else "",
                             "" if cached else "local fetch\n")

    def test_fixed_prefix_initializes_only_the_foreign_cache(self):
        self.exercise(cached=False)

    def test_fixed_prefix_restores_the_already_cached_suite(self):
        self.exercise(cached=True)

    def test_old_prefix_is_rejected_in_both_paths(self):
        for cached in (False, True):
            with self.subTest(cached=cached):
                self.exercise(cached=cached, old_prefix=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fetch-script", default=FETCH_SCRIPT, type=Path)
    args = parser.parse_args()
    FETCH_SCRIPT = args.fetch_script.resolve(strict=True)
    unittest.main(argv=[__file__], verbosity=2)
