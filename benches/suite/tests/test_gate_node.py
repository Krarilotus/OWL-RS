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
SUPPORT = b"// npm's own code, which its launchers load\n"


def node_script(reports: str) -> bytes:
    return f"#!/bin/sh\necho v{reports}\n".encode()


def archive(platform: str, reports: str) -> bytes:
    """nodejs.org's layout: node-v<version>-<platform>/, with bin/node on Linux, node (Git Bash's
    name for node.exe here), npm and npx beside it on Windows."""
    top = f"node-v{VERSION}-{platform}"
    where = top if platform == "win-x64" else f"{top}/bin"
    support = f"{top}/node_modules/npm/bin" if platform == "win-x64" else f"{top}/lib/node_modules/npm/bin"
    files = {f"{where}/node": node_script(reports), f"{where}/npm": b"#!/bin/sh\necho 10.9.9\n",
             f"{where}/npx": b"#!/bin/sh\necho 10.9.9\n", f"{support}/npx-cli.js": SUPPORT}
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

    def gate_node(self, tools: Path, script: str = 'gate_node "$@"', path: str = ""):
        return subprocess.run(
            [BASH, "-c", f'. scripts/lib/gate-node.sh && {script}', "-", self.cache.as_posix(), tools.as_posix()],
            cwd=ROOT, capture_output=True, text=True, timeout=60,
            env=isolation.inherit(*LEAKS, PATH=f"{path}{os.pathsep if path else ''}{self.path}", SEEN=self.seen.as_posix(),
                                  DIST=self.dist.as_posix(), BASE="https://dist.example",
                                  NRESE_NODE_DIST="https://dist.example", PYTHON=sys.executable,
                                  NRESE_PYTHON=sys.executable))

    def seed(self, sha: str, provenance: bool, npx_usable: bool = True) -> Path:
        """An entry from before with the pinned version's node and npm: with its provenance (the record
        gate_node_unpack writes) or without (made before it, or by another archive), and an npx that runs or
        one that is a directory."""
        entry = self.cache / f"node-{VERSION}-{sha[:12]}"
        bin_dir = entry / "bin" if self.platform == "linux-x64" else entry
        bin_dir.mkdir(parents=True)
        for tool, body in {"node": node_script(VERSION), "npm": b"#!/bin/sh\necho 10.9.9\n",
                           "npx": b"#!/bin/sh\necho 10.9.9\n"}.items():
            if tool == "npx" and not npx_usable:
                (bin_dir / tool).mkdir()
                continue
            (bin_dir / tool).write_bytes(body)
            (bin_dir / tool).chmod(0o755)
        if provenance:
            (entry / ".provenance").write_text(f"sha256 {sha} {self.name}\n", newline="\n")
        return bin_dir

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
            # The gate jobs' use marks, at the entry and beside its programs, aren't its payload.
            for marked in {entry, bin_dir}:
                (marked / ".last-used").write_text("1\n")
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

    def test_an_entry_is_reused_only_as_the_build_it_records(self):
        # Review r1 F2: reuse checked a version string, never the build: an entry of the pinned version
        # with another payload and no record of the archive it came from was taken.
        good = self.publish(VERSION)
        tools = self.pin(good)
        self.seed(good, provenance=False)
        done = self.gate_node(tools)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(self.fetches(), 1, "SPEC gate-node: an entry without its build's record is made again")
        entry = self.cache / f"node-{VERSION}-{good[:12]}"
        self.assertIn(good, (entry / ".provenance").read_text(), "SPEC gate-node: and records the verified sha256")
        # One with its record is taken as it is: no fetch.
        done = self.gate_node(tools)
        self.assertEqual((done.returncode, self.fetches()), (0, 1), done.stderr)

    def fetches(self) -> int:
        return self.seen.read_text().count("dist.example") if self.seen.exists() else 0

    def test_an_unusable_npx_never_lets_the_hosts_run(self):
        # Review r1 F1: npx was checked by existence alone; one that is a directory passed, and the console
        # step's bare `npx` reached the host's (on Windows, with the host's own node).
        good = self.publish(VERSION)
        tools = self.pin(good)
        self.seed(good, provenance=True, npx_usable=False)
        host = self.root / "host-bin"
        host.mkdir()
        marker = self.root / "host-npx-ran"
        (host / "npx").write_text(f'#!/bin/sh\n: > "{marker.as_posix()}"\necho HOST-NPX\n', newline="\n")
        (host / "npx").chmod(0o755)
        # As the gate jobs and the console step run it: the cache in Git Bash's form on Windows
        # (windows-job.sh's cygpath -u: a `C:` in PATH splits at its colon), the entry first on PATH,
        # then a bare npx.
        done = self.gate_node(tools, 'c=$(cygpath -u "$1" 2> /dev/null || printf "%s" "$1"); '
                                     'bin=$(gate_node "$c" "$2") && PATH="$bin:$PATH" && npx --version',
                              path=host.as_posix())
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertFalse(marker.exists(), f"SPEC gate-node: the host's npx ran\n{done.stdout}{done.stderr}")
        self.assertEqual(done.stdout.strip(), "10.9.9", "SPEC gate-node: the pinned build's npx ran")

    def test_a_damaged_install_is_never_reused_under_its_record(self):
        # Review r2 F1/F2: a genuine install, then damaged with its records left as they are: a zero-byte npx
        # (Git Bash runs it as an empty script, exit 0; a bare npx then skips it and finds the host's), an npx
        # replaced by another that runs, and npm's support code changed. Each is made again from the archive;
        # the host's npx never runs. Without the archive to make it again: no Node at all, never the host's.
        good = self.publish(VERSION)
        tools = self.pin(good)
        entry = self.cache / f"node-{VERSION}-{good[:12]}"
        bin_dir = entry / "bin" if self.platform == "linux-x64" else entry
        support = entry / ("node_modules" if self.platform == "win-x64" else "lib/node_modules") / "npm/bin/npx-cli.js"
        host = self.root / "host-bin"
        host.mkdir()
        marker = self.root / "host-npx-ran"
        (host / "npx").write_text(f'#!/bin/sh\n: > "{marker.as_posix()}"\necho HOST-NPX\n', newline="\n")
        (host / "npx").chmod(0o755)
        console = ('c=$(cygpath -u "$1" 2> /dev/null || printf "%s" "$1"); '
                   'bin=$(gate_node "$c" "$2") && PATH="$bin:$PATH" && npx --version')
        damages = {"zero-byte npx": lambda: (bin_dir / "npx").write_bytes(b""),
                   "another npx": lambda: (bin_dir / "npx").write_bytes(b"#!/bin/sh\necho STALE-NPX\n"),
                   "npm's support code": lambda: support.write_bytes(b"// another\n")}
        for name, damage in damages.items():
            with self.subTest(name):
                shutil.rmtree(self.cache, ignore_errors=True)
                self.seen.unlink(missing_ok=True)
                self.assertEqual(self.gate_node(tools).returncode, 0, "a genuine install first")
                records = {p: (entry / p).read_bytes() if (entry / p).exists() else None
                           for p in (".provenance", ".manifest")}
                damage()
                done = self.gate_node(tools, console, path=host.as_posix())
                self.assertFalse(marker.exists(), f"SPEC gate-node: the host's npx ran ({name})\n{done.stdout}{done.stderr}")
                self.assertEqual((done.returncode, done.stdout.strip(), self.fetches()), (0, "10.9.9", 2),
                                 f"SPEC gate-node: made again from the archive ({name})\n{done.stderr}")
                self.assertEqual(support.read_bytes(), SUPPORT, f"SPEC gate-node: the archive's payload ({name})")
                self.assertEqual({p: (entry / p).read_bytes() if (entry / p).exists() else None for p in records},
                                 records, f"SPEC gate-node: records of the same archive and payload ({name})")
        # Damaged, and the archive can't be had: no Node, and nothing of the host's runs.
        (bin_dir / "npx").write_bytes(b"")
        (self.dist / f"v{VERSION}" / self.name).unlink()
        done = self.gate_node(tools, console, path=host.as_posix())
        self.assertNotEqual(done.returncode, 0, "SPEC gate-node: a damaged entry that can't be made again is no Node")
        self.assertFalse(marker.exists(), f"SPEC gate-node: the host's npx ran\n{done.stdout}{done.stderr}")

    def test_a_build_reporting_another_version_is_refused(self):
        sha = self.publish("1.0.0")  # the archive's sha256 is pinned, yet its node isn't VERSION
        done = self.gate_node(self.pin(sha))
        self.assertNotEqual(done.returncode, 0, "SPEC gate-node: a build not of the pinned version was taken")
        self.assertEqual([e for e in self.entries() if e.startswith("node-")], [], done.stderr)


if __name__ == "__main__":
    unittest.main()
