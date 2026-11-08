"""The gate's own Node (scripts/lib/gate-node.sh gate_node), for the console's step on a gate host:
nodejs.org's build of the version scripts/lib/gate-tools.txt pins, taken only by its archive's sha256,
unpacked under the host's cache through the cache's one entry maker (scripts/lib/suite-python.sh
suite_python_entry: under its lock, made aside, moved in whole), never the host's own Node. Toolchains
on hosts belong to the job (the owner, 2 Nov 2026), so no host capability is asked for.

Run: python benches/suite/tests/isolation.py discover -s benches/suite/tests -p test_gate_node.py
An assertion tagged `SPEC <tag>` states the behaviour its test is for.
"""

import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

import isolation  # the suite's git environment policy (inherit())

ROOT = Path(__file__).resolve().parents[3]
BASH = shutil.which("bash")
LEAKS = ("NRESE_", "FLEET_")
VERSION = "9.9.9"


def node_script(reports: str) -> bytes:
    return f"#!/bin/sh\necho v{reports}\n".encode()


def archive(platform: str, reports: str) -> bytes:
    """nodejs.org's layout: node-v<version>-<platform>/, with bin/node on Linux, node (Git Bash's
    name for node.exe here), npm and npx beside it on Windows."""
    top = f"node-v{VERSION}-{platform}"
    where = top if platform == "win-x64" else f"{top}/bin"
    files = {f"{where}/node": node_script(reports), f"{where}/npm": b"#!/bin/sh\necho 10.9.9\n",
             f"{where}/npx": b"#!/bin/sh\necho 10.9.9\n"}
    out = io.BytesIO()
    if platform == "win-x64":
        with zipfile.ZipFile(out, "w") as z:
            for name, body in files.items():
                z.writestr(name, body)
    else:
        with tarfile.open(fileobj=out, mode="w:gz") as t:
            for name, body in files.items():
                info = tarfile.TarInfo(name)
                info.size, info.mode = len(body), 0o755
                t.addfile(info, io.BytesIO(body))
    return out.getvalue()


class GateNode(unittest.TestCase):
    """A stand-in for nodejs.org's distribution (a directory, fetched by a curl that copies from it and
    says what it was asked for), a gate-tools.txt pinning VERSION by each archive's sha256, and an empty
    cache."""

    def setUp(self):
        if BASH is None:
            self.skipTest("needs bash")
        temp = tempfile.TemporaryDirectory(prefix="nrese-gate-node-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.cache, self.seen, self.dist = self.root / "cache", self.root / "seen", self.root / "dist"
        self.platform = "win-x64" if os.name == "nt" else "linux-x64"
        self.ext = "zip" if self.platform == "win-x64" else "tar.gz"
        self.name = f"node-v{VERSION}-{self.platform}.{self.ext}"
        (self.dist / f"v{VERSION}").mkdir(parents=True)
        self.publish(VERSION)
        stand_ins = self.root / "stand-ins"
        stand_ins.mkdir()
        # curl: the -o file from DIST, by the URL's path below the base; unzip: Python's zipfile.
        for name, body in {"curl": 'while [ $# -gt 1 ]; do [ "$1" != -o ] || out=$2; shift; done\n'
                                   'echo "$1" >> "$SEEN"\ncp "$DIST/${1#"$BASE"/}" "$out"\n',
                           "unzip": '"$PYTHON" -m zipfile -e "$2" "$4" && chmod -R +x "$4"\n'}.items():
            (stand_ins / name).write_text(f"#!/bin/sh\n{body}", newline="\n")
            (stand_ins / name).chmod(0o755)
        self.path = f"{stand_ins.as_posix()}{os.pathsep}{os.environ.get('PATH', '')}"

    def publish(self, reports: str) -> str:
        data = archive(self.platform, reports)
        (self.dist / f"v{VERSION}" / self.name).write_bytes(data)
        return hashlib.sha256(data).hexdigest()

    def pin(self, sha: str) -> Path:
        tools = self.root / "gate-tools.txt"
        other = "win-x64" if self.platform == "linux-x64" else "linux-x64"
        tools.write_text(f"cargo-nextest 0.9.146\nnode {VERSION} {self.platform} {sha} {other} {'0' * 64}\n",
                         newline="\n")
        return tools

    def gate_node(self, tools: Path):
        return subprocess.run(
            [BASH, "-c", '. scripts/lib/gate-node.sh && gate_node "$@"', "-", self.cache.as_posix(), tools.as_posix()],
            cwd=ROOT, capture_output=True, text=True, timeout=60,
            env=isolation.inherit(*LEAKS, PATH=self.path, SEEN=self.seen.as_posix(), DIST=self.dist.as_posix(),
                                  BASE="https://dist.example", NRESE_NODE_DIST="https://dist.example",
                                  PYTHON=sys.executable, NRESE_PYTHON=sys.executable))

    def entries(self):
        return sorted(p.name for p in self.cache.iterdir()) if self.cache.exists() else []

    def test_the_pinned_build_by_its_sha256_once(self):
        sha = self.publish(VERSION)
        tools = self.pin(sha)
        url = f"https://dist.example/v{VERSION}/{self.name}\n"
        entry = self.cache / f"node-{VERSION}-{sha[:12]}"
        for attempt in ("fetched", "there"):
            done = self.gate_node(tools)
            self.assertEqual(done.returncode, 0, f"SPEC gate-node: {attempt}\n{done.stderr}")
            bin_dir = Path(done.stdout.strip())
            self.assertEqual(bin_dir.parent if self.platform == "linux-x64" else bin_dir, entry,
                             f"SPEC gate-node: the cache's own entry, by version and sha256 ({attempt})")
            node = bin_dir / "node"
            reported = subprocess.run([BASH, "-c", 'exec "$1" --version', "-", node.as_posix()], capture_output=True,
                                      text=True).stdout.strip()
            self.assertEqual(reported, f"v{VERSION}", f"SPEC gate-node: the pinned build's node ({attempt})")
            self.assertEqual(self.seen.read_text(), url, f"SPEC gate-node: fetched once ({attempt})")
        self.assertNotIn(".partial", " ".join(self.entries()), "SPEC gate-node: nothing made aside is left")

    def test_an_archive_not_of_the_pinned_sha256_is_never_taken(self):
        self.publish(VERSION)
        done = self.gate_node(self.pin("1" * 64))
        self.assertNotEqual(done.returncode, 0, "SPEC gate-node: an archive of another sha256 was taken")
        self.assertEqual([e for e in self.entries() if e.startswith("node-")], [],
                         f"SPEC gate-node: nothing of it is left\n{done.stderr}")

    def test_an_entry_not_reporting_the_pinned_version_is_made_again(self):
        good = self.publish(VERSION)
        tools = self.pin(good)
        entry = self.cache / f"node-{VERSION}-{good[:12]}"
        # One there from before, damaged: its node reports another version.
        bin_dir = entry / "bin" if self.platform == "linux-x64" else entry
        bin_dir.mkdir(parents=True)
        (bin_dir / "node").write_bytes(node_script("1.0.0"))
        (bin_dir / "node").chmod(0o755)
        done = self.gate_node(tools)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.seen.read_text().count("dist.example"), 1, "SPEC gate-node: fetched again")
        reported = subprocess.run([BASH, "-c", 'exec "$1" --version', "-", (bin_dir / "node").as_posix()],
                                  capture_output=True, text=True).stdout.strip()
        self.assertEqual(reported, f"v{VERSION}", "SPEC gate-node: the entry is the pinned build now")

    def test_an_entry_without_its_npm_is_made_again(self):
        # sol-loop r1 S2: an entry with the pinned node but no npm or npx was taken, and PATH then
        # reached the host's npm (on Windows its launcher runs its own node.exe beside it).
        good = self.publish(VERSION)
        tools = self.pin(good)
        entry = self.cache / f"node-{VERSION}-{good[:12]}"
        bin_dir = entry / "bin" if self.platform == "linux-x64" else entry
        for missing in ("npm", "npx"):
            with self.subTest(missing):
                shutil.rmtree(self.cache, ignore_errors=True)
                self.seen.unlink(missing_ok=True)
                bin_dir.mkdir(parents=True)
                for tool in ("node", "npm", "npx"):
                    if tool != missing:
                        (bin_dir / tool).write_bytes(node_script(VERSION) if tool == "node" else b"#!/bin/sh\n")
                        (bin_dir / tool).chmod(0o755)
                done = self.gate_node(tools)
                self.assertEqual(done.returncode, 0, done.stderr)
                self.assertEqual(self.seen.read_text().count("dist.example"), 1,
                                 f"SPEC gate-node: an entry without its {missing} is fetched again")
                self.assertTrue((bin_dir / missing).exists(), f"SPEC gate-node: and has its {missing} then")

    def test_a_build_reporting_another_version_is_refused(self):
        sha = self.publish("1.0.0")  # the archive's sha256 is pinned, yet its node isn't VERSION
        done = self.gate_node(self.pin(sha))
        self.assertNotEqual(done.returncode, 0, "SPEC gate-node: a build not of the pinned version was taken")
        self.assertEqual([e for e in self.entries() if e.startswith("node-")], [], done.stderr)


if __name__ == "__main__":
    unittest.main()
