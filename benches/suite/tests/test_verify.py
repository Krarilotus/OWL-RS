"""/verify's runner (scripts/verify/verify.py; the brief, 20 Oct 2026: agents prove HTTP-level
behaviour by driving a real server, not by claiming it). A verification tool's worst defect is
a false pass, so: every kind of check fails when it should and holds when it should, each failure
with its message, and an incomplete exchange fails before its expectations are read (the self-
test, run here against a stand-in for nrese-server and its own fault server); a failed or
interrupted run still stops its server, removes its data and writes a failing summary; a start
is retried only when its own log says its port was taken, after its process is reaped; and a
scenario that would check less than it says is refused before anything starts. Nothing is built.

    python -m unittest discover benches/suite/tests
"""
from __future__ import annotations

import argparse
import contextlib
import importlib.util
import io
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "scripts/verify"))
sys.path.append(str(ROOT / "benches/suite"))  # suitekit, after the standard modules (queue.py)
spec = importlib.util.spec_from_file_location("verify_runner", ROOT / "scripts/verify/verify.py")
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)
from verifykit import scenario  # noqa: E402
from verifykit.server import LaunchError, Server  # noqa: E402

# nrese-server's routes the self-test and the doctor read, answered as it answers them (compact
# JSON), its words when its port is taken, and with VERIFY_STAND_IN=never-ready a /readyz of 503.
# With VERIFY_STAND_IN=behind-a-launcher it is a launcher, as a Windows venv's python.exe is: the
# server is its child, which it waits for (the child's pid in child.pid beside the script);
# behind-a-launcher-stubborn, the same with a child that ignores SIGTERM; and
# behind-a-launcher-that-leaves, a launcher that goes at once and leaves its child serving.
STAND_IN = r'''
import json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

mode = os.environ.get("VERIFY_STAND_IN", "")
if mode.startswith("behind-a-launcher"):
    import subprocess
    child_mode = "stubborn" if mode.endswith("-stubborn") else ""
    child = subprocess.Popen([sys.executable, *sys.argv], env={**os.environ, "VERIFY_STAND_IN": child_mode})
    with open(os.path.join(os.path.dirname(os.path.abspath(sys.argv[0])), "child.pid"), "w") as f:
        f.write(str(child.pid))
    sys.exit(0 if mode.endswith("-that-leaves") else child.wait())
if mode == "stubborn":
    import signal
    signal.signal(signal.SIGTERM, signal.SIG_IGN)

host, port = next(a.split("=", 1)[1] for a in sys.argv if a.startswith("server.bind_address=")).rsplit(":", 1)
ROWS = {"head": {"vars": ["x"]}, "results": {"bindings": [{"x": {"type": "literal", "value": v}} for v in "ab"]}}
ROUTES = {
    "/healthz": ("application/json", {"status": "ok"}),
    "/readyz": ("application/json", {"status": "ready", "consistency": "consistent", "revision": 0, "quad_count": 0}),
    "/version": ("application/json", {"service": "stand-in", "version": "0", "store_mode": "on-disk"}),
    "/api/v1/repositories": ("application/json", [{"id": "nrese", "default": True}]),
    "/dataset/query": ("application/sparql-results+json", ROWS),
}
if os.environ.get("VERIFY_STAND_IN") == "never-ready":
    ROUTES["/readyz"] = ("application/json", {"status": "starting"})

class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        media, body = ROUTES.get(self.path.split("?")[0], ("application/problem+json", {"status": 404}))
        data = json.dumps(body, separators=(",", ":")).encode()
        status = 404 if media == "application/problem+json" else 503 if body == {"status": "starting"} else 200
        self.send_response(status)
        self.send_header("Content-Type", media)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

class Server(HTTPServer):
    allow_reuse_address = False  # on Windows SO_REUSEADDR would take a port another socket holds

try:
    server = Server((host, int(port)), Handler)
except OSError as e:
    print(f"failed to bind {host}:{port}: {e}", flush=True)
    sys.exit(1)
server.serve_forever()
'''


class StandIn:
    """The stand-in as a server command, in a directory of its own."""

    def __init__(self):
        self.dir = Path(tempfile.mkdtemp(prefix="verify-test-"))
        self.script = self.dir / "stand_in.py"
        self.script.write_text(STAND_IN, encoding="utf-8")
        self.target = {"command": [sys.executable, str(self.script)],
                       "binary": {"path": str(self.script), "sha256": "0" * 64, "revision": None}}

    def main(self, *argv: str) -> tuple[int, str]:
        """verify.py's main with the stand-in as its server: its exit code and output."""
        original, verify.target_of = verify.target_of, lambda args: self.target
        out = io.StringIO()
        try:
            with contextlib.redirect_stdout(out):
                code = verify.main([*argv[:1], "--server", str(self.script), "--start-timeout", "30", *argv[1:]])
        finally:
            verify.target_of = original
        return code, out.getvalue()

    def remove(self):
        shutil.rmtree(self.dir, ignore_errors=True)


def kill(pid: int) -> None:
    """Ends one process of the test's own, if it is still there."""
    try:
        os.kill(pid, getattr(signal, "SIGKILL", signal.SIGTERM))
    except OSError:
        pass


def refused(port: int) -> bool:
    try:
        socket.create_connection(("127.0.0.1", port), timeout=2).close()
        return False
    except OSError:
        return True


class SelfTest(unittest.TestCase):
    """One self-test run against the stand-in and the fault server; its tests read what it left."""

    @classmethod
    def setUpClass(cls):
        cls.stand_in = StandIn()
        cls.evidence = cls.stand_in.dir / "evidence"
        cls.code, cls.output = cls.stand_in.main("selftest", "--evidence", str(cls.evidence))
        cls.summary = json.loads((cls.evidence / "summary.json").read_text(encoding="utf-8"))
        cls.steps = {s["name"]: s for r in cls.summary["scenarios"] for s in r["steps"]}

    @classmethod
    def tearDownClass(cls):
        cls.stand_in.remove()

    def test_every_check_and_every_fault_goes_as_it_must(self):
        self.assertEqual(self.code, 0, self.output)
        self.assertIn("self-test passed", self.output)
        self.assertEqual(self.steps["status, wrong"]["failures"], ["status: expected 418, got 200"])
        self.assertTrue(self.steps["rows"]["ok"])
        # A body two bytes of its announced hundred: the exchange's failure, not the body's check.
        self.assertEqual(self.steps["truncated body"]["failures"],
                         ["incomplete response: 2 of 100 body bytes, then the connection closed"])
        record = json.loads((self.evidence / "selftest-faults" / "steps" / "01-truncated-body.json").read_text())
        body = record["response"]["body"]
        self.assertEqual((body["saved_complete"], body["transport_complete"]), (True, False))
        # The summary's path is the last line, and it says which steps failed and why.
        printed = Path(self.output.strip().splitlines()[-1].removeprefix("summary: "))
        self.assertIn('json $.head.vars: expected ["y"], got ["x"]', printed.read_text(encoding="utf-8"))

    def test_a_failed_run_still_stops_the_server_and_removes_its_data(self):
        result = self.summary["scenarios"][0]
        self.assertFalse(result["ok"])  # its wrong rows failed
        cleanup = result["cleanup"]
        self.assertTrue(cleanup["stopped"] and cleanup["removed"], cleanup)
        self.assertFalse(Path(cleanup["directory"]).exists())
        self.assertTrue(refused(result["launch"]["port"]))


class Interruption(unittest.TestCase):
    def test_an_interrupted_run_writes_a_failing_summary_and_cleans_up(self):
        stand_in = StandIn()
        self.addCleanup(stand_in.remove)
        file = stand_in.dir / "three.toml"
        file.write_text("\n".join(f'[[step]]\nname = "health {i}"\npath = "/healthz"\nexpect = {{ status = 200 }}\n'
                                  for i in range(1, 4)), encoding="utf-8")
        calls, original = [], verify.client.exchange

        def exchange(*args):  # SIGTERM arriving during the second step
            calls.append(args)
            if len(calls) == 2:
                raise verify.Interrupted("terminated (SIGTERM)")
            return original(*args)

        verify.client.exchange = exchange
        try:
            code, output = stand_in.main("run", "--evidence", str(stand_in.dir / "evidence"), str(file))
        finally:
            verify.client.exchange = original
        self.assertEqual(code, 3, output)
        summary = json.loads((stand_in.dir / "evidence" / "summary.json").read_text(encoding="utf-8"))
        self.assertTrue(summary["title"].startswith("/verify INTERRUPTED"), summary["title"])
        result = summary["scenarios"][0]
        self.assertEqual(result["error"], "interrupted: terminated (SIGTERM)")
        self.assertEqual([(s["name"], s["ok"]) for s in result["steps"]], [("health 1", True), ("health 2", False)])
        self.assertTrue(result["cleanup"]["removed"] and result["cleanup"]["stopped"], result["cleanup"])


class Resolution(unittest.TestCase):
    def test_resolving_the_host_name_is_within_the_deadline(self):
        from verifykit import client
        release = threading.Event()

        def resolver(*args, **kwargs):  # answers only once the exchange has returned
            release.wait(10)
            raise OSError("resolved too late")

        original, client.socket.getaddrinfo = client.socket.getaddrinfo, resolver
        try:
            got = client.get("http://resolves.slowly.invalid:8080/healthz", 0.2)
        finally:
            client.socket.getaddrinfo = original
            release.set()
        self.assertEqual(got["error"], "no response: the 0.2 s deadline passed while resolving the host's name")

    def test_an_address_whose_socket_cant_be_made_gives_way_to_the_next(self):
        from verifykit import client
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        self.addCleanup(listener.close)
        port = listener.getsockname()[1]
        both = [(socket.AF_INET6, socket.SOCK_STREAM, 6, "", ("::1", port, 0, 0)),
                (socket.AF_INET, socket.SOCK_STREAM, 6, "", ("127.0.0.1", port))]
        make = client.socket.socket

        def without_ipv6(family=-1, *args, **kwargs):  # a host with IPv6 off
            if family == socket.AF_INET6:
                raise OSError(97, "Address family not supported by protocol")
            return make(family, *args, **kwargs)

        resolve = client.socket.getaddrinfo
        client.socket.getaddrinfo, client.socket.socket = (lambda *a, **k: both), without_ipv6
        try:
            sock = client.connect("dual.example", port, client.Deadline(5))
        finally:
            client.socket.getaddrinfo, client.socket.socket = resolve, make
        self.addCleanup(sock.close)
        self.assertEqual(sock.getpeername()[1], port)


def isolated_build(build: Path, revision: str, digest: str, contract: str = "isolated-build/1") -> None:
    """The PROVENANCE build-on-host.sh writes after an isolated build of `revision`."""
    (build / "PROVENANCE").write_bytes(f"contract {contract}\nrevision {revision}\ntree {'1' * 40}\n"
                                       f"rustc 1.98.1\nsha256 {digest}\n".encode())


class SweptBuild(unittest.TestCase):
    def test_a_build_swept_while_it_is_chosen_is_a_launch_failure_with_a_summary(self):
        root = Path(tempfile.mkdtemp(prefix="verify-test-"))
        self.addCleanup(shutil.rmtree, root, True)
        build = root / "cache" / "bin-0123456789ab"
        build.mkdir(parents=True)
        (build / "nrese-server").write_bytes(b"x")
        (build / "SHA256SUMS").write_text(f"{verify.sha256_of(build / 'nrese-server')}  nrese-server\n")
        isolated_build(build, "0123456789ab" + "0" * 28, verify.sha256_of(build / "nrese-server"))
        (build / ".last-used").mkdir()  # its mark can't be written, as when a sweep took the directory
        scenario_file = root / "one.toml"
        scenario_file.write_text('[[step]]\nname = "health"\npath = "/healthz"\nexpect = { status = 200 }\n')
        original, verify.CACHE = verify.CACHE, root / "cache"
        try:
            with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                code = verify.main(["run", "--server-rev", "0123456789ab" + "0" * 28,
                                    "--evidence", str(root / "evidence"), str(scenario_file)])
        finally:
            verify.CACHE = original
        self.assertEqual(code, 3)
        summary = json.loads((root / "evidence" / "summary.json").read_text(encoding="utf-8"))
        self.assertTrue(summary["scenarios"][0]["error"].startswith("launch: reading the server's build failed"))


class Provenance(unittest.TestCase):
    """--server-rev runs only an isolated build of the commit it names (PR #47, Sol r1 F1). A
    bin-<sha12> from before the builds were isolated (26 Oct 2026) can hold another commit's code
    under this one's name, its REVISION and checksum valid: only the PROVENANCE build-on-host.sh
    writes after an isolated build tells them apart."""

    def test_only_an_isolated_build_of_the_commit_is_run(self):
        revision = "0123456789ab" + "0" * 28
        other = "0123456789ab" + "f" * 28  # the same bin-<sha12>
        cases = {  # name: its PROVENANCE, or None for none; whether it may run
            "an isolated build": (lambda b, d: isolated_build(b, revision, d), True),
            "a build from before (the witness)": (None, False),
            "another commit's record": (lambda b, d: isolated_build(b, other, d), False),
            "another binary's record": (lambda b, d: isolated_build(b, revision, "0" * 64), False),
            "another contract": (lambda b, d: isolated_build(b, revision, d, "isolated-build/0"), False),
            "an empty record": (lambda b, d: (b / "PROVENANCE").write_bytes(b""), False),
        }
        wrong = []
        for name, (record, admitted) in cases.items():
            root = Path(tempfile.mkdtemp(prefix="verify-provenance-"))
            self.addCleanup(shutil.rmtree, root, True)
            build = root / "bin-0123456789ab"
            build.mkdir()
            (build / "nrese-server").write_bytes(b"#!/bin/sh\n# a commit's code\n")
            digest = verify.sha256_of(build / "nrese-server")
            (build / "SHA256SUMS").write_bytes(f"{digest}  nrese-server\n".encode())
            (build / "REVISION").write_bytes(f"{revision}\n".encode())
            if record:
                record(build, digest)
            original, verify.CACHE = verify.CACHE, root
            try:
                target = verify.target_of(argparse.Namespace(url=None, server_rev=revision, server=None))
                ran = target["binary"]["sha256"] == digest
            except LaunchError:
                ran = False
            finally:
                verify.CACHE = original
            if ran != admitted:
                wrong.append(f"{name}: {'run' if ran else 'refused'}")
        self.assertEqual(wrong, [], "SPEC provenance: --server-rev runs exactly the isolated builds")


class AdmittedBytes(unittest.TestCase):
    """What --server-rev runs is the build it admitted, whatever happens to bin-<sha12> after (PR
    #47, fix check 1): a sweep can take the entry, and a builder from an older harness put another
    commit's code there under this commit's REVISION with a valid checksum and no PROVENANCE, while
    the record is read or before the server starts (and restarts, from the same command)."""

    def test_what_runs_is_the_build_admitted_or_nothing(self):
        revision = "0123456789ab" + "0" * 28
        admitted, other = b"#!/bin/sh\n# this commit's code\n", b"#!/bin/sh\n# another commit's code\n"

        def entry(build: Path, code: bytes, record: bool) -> None:
            build.mkdir()
            (build / "nrese-server").write_bytes(code)
            digest = verify.sha256_of(build / "nrese-server")
            (build / "SHA256SUMS").write_bytes(f"{digest}  nrese-server\n".encode())
            (build / "REVISION").write_bytes(f"{revision}\n".encode())
            if record:
                isolated_build(build, revision, digest)

        wrong = []
        for when in ("while its record is read", "after it was chosen"):
            root = Path(tempfile.mkdtemp(prefix="verify-admitted-"))
            self.addCleanup(shutil.rmtree, root, True)
            build = root / "bin-0123456789ab"
            entry(build, admitted, True)
            replaced = []

            def replace():  # the sweep, then the older builder's publication
                shutil.rmtree(build)
                entry(build, other, False)
                replaced.append(when)

            read_text = Path.read_text

            def reading(path, *args, **kwargs):
                text = read_text(path, *args, **kwargs)
                if path.name == "PROVENANCE" and when == "while its record is read" and not replaced:
                    replace()
                return text

            original, verify.CACHE, Path.read_text = verify.CACHE, root, reading
            try:
                target = verify.target_of(argparse.Namespace(url=None, server_rev=revision, server=None))
            except LaunchError:
                target = None
            finally:
                verify.CACHE, Path.read_text = original, read_text
            if when == "after it was chosen":
                replace()
            ran = Path(target["command"][0]).read_bytes() if target else None
            if not replaced or ran not in (admitted, None) or (when == "after it was chosen" and ran is None):
                wrong.append(f"{when}: {'nothing' if ran is None else 'another commit' if ran == other else ran}")
        self.assertEqual(wrong, [], "SPEC provenance: what runs is the bytes admitted (or, while they "
                                    "are read, nothing)")


@unittest.skipUnless(shutil.which("flock") and shutil.which("bash"), "needs flock (Linux)")
class Migration(unittest.TestCase):
    """build-on-host.sh's migration of /verify's old cache, the block as written: its lock file stays
    (the same inode), so a build of the old kind waiting on it and every later migration contend
    on one lock, and while one holds it nothing of the old cache goes (Sol r3, finding 1)."""

    def migrate(self, old: Path) -> None:
        text = (ROOT / "scripts/verify/build-on-host.sh").read_text(encoding="utf-8")
        block = text[text.index("migrated=0\n"):]
        block = block[:block.index("\nfi\n") + 4]
        subprocess.run(["bash", "-c", f"set -euo pipefail\nold='{old}'\ncache='{old}-new'\n{block}"],
                       check=True, capture_output=True, timeout=60)

    def test_the_old_lock_stays_and_guards_what_a_holder_builds(self):
        old = Path(tempfile.mkdtemp(prefix="verify-old-"))
        self.addCleanup(shutil.rmtree, old, True)
        (old / "lock").touch()
        inode = (old / "lock").stat().st_ino
        (old / "bin-0123456789ab").mkdir()
        (old / "target\nnewline").mkdir()  # du prints such a name over two lines
        self.migrate(old)
        self.assertEqual(sorted(p.name for p in old.iterdir()), ["lock"])
        self.assertEqual((old / "lock").stat().st_ino, inode)
        # A build of the old kind holds the lock (a waiter that got it): its target survives a migration.
        (old / "target-1.98.1").mkdir()
        # --no-fork: the holder is the sleep itself, so killing it frees the lock and its pipes at once.
        holder = subprocess.Popen(["flock", "--no-fork", str(old / "lock"), "sleep", "30"],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        deadline = time.monotonic() + 10
        while subprocess.run(["flock", "-n", str(old / "lock"), "true"]).returncode == 0:
            self.assertLess(time.monotonic(), deadline, "the holder never took the lock")
            time.sleep(0.05)
        self.migrate(old)
        self.assertTrue((old / "target-1.98.1").is_dir())


@unittest.skipUnless(shutil.which("flock") and Path("/proc/self/cgroup").exists(), "a Linux fleet host's step")
class OwnTarget(unittest.TestCase):
    """build-on-host.sh compiles in its own checkout's target directory, whatever the host sets, never
    in one another checkout builds in (the gates', shared until 26 Oct 2026, or another build's):
    cargo decides freshness by mtimes, and its unit hashes don't depend on the checkout's path, so
    another checkout's build there could stand in for this commit's code."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="verify-target-"))
        self.addCleanup(shutil.rmtree, self.root, True)
        self.cache, stubs, self.seen = self.root / "cache", self.root / "stubs", self.root / "seen"
        stubs.mkdir()
        for name, body in {"rustup": "exit 0", "rustc": 'echo "release: 1.98.1"'}.items():
            (stubs / name).write_text(f"#!/bin/sh\n{body}\n")
            (stubs / name).chmod(0o755)
        env = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "NRESE_", "FLEET_", "CARGO_"))}
        env.update(HOME=str(self.root), PATH=f"{stubs}:{env['PATH']}", NRESE_VERIFY_CACHE=str(self.cache),
                   SEEN=str(self.seen),
                   CARGO_TARGET_DIR=str(self.cache / "target-1.98.1"))  # a host's own setting: the shared directory
        self.env = env

    def commit(self, name: str) -> Path:
        """A checkout of one commit of its own, whose cargo-guarded.sh says where it builds and
        leaves a binary there (its checkout's path, executable)."""
        repo = self.root / name
        (repo / "scripts").mkdir(parents=True)
        (repo / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.98.1"\n')
        (repo / "scripts" / "cargo-guarded.sh").write_text(
            'echo "$CARGO_TARGET_DIR" >> "$SEEN"\nmkdir -p "$CARGO_TARGET_DIR/debug"\n'
            'echo "$PWD" > "$CARGO_TARGET_DIR/debug/nrese-server"\nchmod +x "$CARGO_TARGET_DIR/debug/nrese-server"\n')
        (repo / "src.rs").write_text(f"// commit {name}\n")
        for args in (["init", "-q"], ["add", "-A"], ["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "c"]):
            subprocess.run(["git", *args], cwd=repo, env=self.env, check=True, capture_output=True, timeout=60)
        return repo

    def build(self, repo: Path, run: str) -> dict:
        """build-on-host.sh in `repo`, as a fleet step: its build.json."""
        out = self.root / f"out-{run}"
        out.mkdir()
        done = subprocess.run(["bash", str(ROOT / "scripts/verify/build-on-host.sh")], cwd=repo,
                              env={**self.env, "FLEET_OUT": str(out)}, capture_output=True, text=True, timeout=120)
        self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
        return json.loads((out / "build.json").read_text(encoding="utf-8"))

    def test_each_build_compiles_in_its_own_checkout(self):
        checkouts = [self.commit("a"), self.commit("b")]  # two commits, so neither reuses the other's binary
        for repo in checkouts:
            self.build(repo, repo.name)
        built = [Path(line) for line in self.seen.read_text().split()]
        self.assertEqual(len(built), 2)
        for path, repo in zip(built, checkouts):
            self.assertTrue(path.name == "target" and path.parent.samefile(repo),
                            f"SPEC own-target: built in {path}, not {repo}'s own target directory")
        self.assertEqual(list(self.cache.glob("target*")), [], "SPEC own-target: a build compiled in the host's cache")

    def test_a_build_from_before_isolated_builds_is_built_again(self):
        """A bin-<sha12> from the shared target can hold another commit's code under this one's
        name, its REVISION and checksum valid (PR #47, Sol r1 F1): it is built again, and the
        isolated build it leaves is reused."""
        repo = self.commit("a")
        sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=repo, env=self.env, check=True,
                             capture_output=True, text=True, timeout=60).stdout.strip()
        legacy = self.cache / f"bin-{sha[:12]}"
        legacy.mkdir(parents=True)
        (legacy / "nrese-server").write_bytes(b"#!/bin/sh\n# another commit's code\n")
        (legacy / "nrese-server").chmod(0o755)
        (legacy / "SHA256SUMS").write_bytes(f"{verify.sha256_of(legacy / 'nrese-server')}  nrese-server\n".encode())
        (legacy / "REVISION").write_bytes(f"{sha}\n".encode())
        self.assertTrue(self.build(repo, "first")["built"], "SPEC provenance: a build from before was reused")
        self.assertEqual((legacy / "nrese-server").read_text().strip(), str(repo), "not this checkout's build")
        self.assertFalse(self.build(repo, "second")["built"], "an isolated build of this tree is built again")


class Launch(unittest.TestCase):
    def setUp(self):
        self.stand_in = StandIn()
        self.addCleanup(self.stand_in.remove)

    def server(self, **env) -> Server:
        server = Server(self.stand_in.target["command"], {}, env, self.stand_in.dir / "server.log", start_timeout=5)
        self.addCleanup(server.cleanup)
        return server

    def test_a_stop_ends_what_the_server_started(self):
        """The merge check's Windows part under the suite's own venv (lane-d-wjp-windows2-4fff13fc):
        the server's command was a launcher whose child held the port, and a stop ended the
        launcher alone. A stop ends the server with all it started, a child that ignores SIGTERM
        too (Sol, windows-job-python r3: the grace ended with the launcher, not its group)."""
        for mode in ("behind-a-launcher", "behind-a-launcher-stubborn"):
            with self.subTest(mode=mode):
                server = self.server(VERIFY_STAND_IN=mode)
                server.start()
                self.addCleanup(kill, int((self.stand_in.dir / "child.pid").read_text()))  # if it stayed
                port = server.attempts[-1]["port"]
                server.stop()
                deadline = time.monotonic() + 5  # a bound for an exit under way, not a speed
                while not refused(port) and time.monotonic() < deadline:
                    time.sleep(0.1)
                self.assertTrue(refused(port), f"SPEC verify-stop: the server's child still holds its port ({mode})")

    @unittest.skipIf(os.name == "nt", "Windows finds a tree through its live parent (taskkill /T): what a gone "
                                      "parent left isn't found")
    def test_a_stop_ends_what_a_gone_launcher_left(self):
        """Sol, windows-job-python r3: a stop passed over a server whose own process had gone, so a
        launcher that left its child left it serving. The stop takes the process group."""
        server = self.server(VERIFY_STAND_IN="behind-a-launcher-that-leaves")
        with self.assertRaisesRegex(LaunchError, "exited with code 0 before it was ready"):
            server.start()
        self.addCleanup(kill, int((self.stand_in.dir / "child.pid").read_text()))  # if it stayed
        port = server.attempts[-1]["port"]
        # A child left serving binds within this bound; a stopped one never does.
        deadline = time.monotonic() + 5
        while refused(port) and time.monotonic() < deadline:
            time.sleep(0.1)
        self.assertTrue(refused(port), "SPEC verify-stop: a gone launcher's child serves on")

    def test_a_start_whose_port_was_taken_is_reaped_then_retried(self):
        with socket.socket() as taken:
            taken.bind(("127.0.0.1", 0))
            taken.listen()
            server = self.server()
            server.port = taken.getsockname()[1]  # as if another process took it after our probe
            server.start()
        first, second = server.attempts
        self.assertEqual((first["outcome"], first["exit_code"]), ("failed to bind", 1))
        self.assertEqual(second["outcome"], "ready")
        self.assertNotEqual(second["port"], first["port"])

    def test_a_start_that_isnt_ready_is_stopped_not_taken_for_a_bind_failure(self):
        server = self.server(VERIFY_STAND_IN="never-ready")
        # An earlier start's words in the same log mustn't decide this one.
        server.log.write_text("failed to bind 127.0.0.1:1 (an earlier start)\n", encoding="utf-8")
        server.start_timeout = 1.5
        with self.assertRaisesRegex(LaunchError, "not ready after 1.5 s"):
            server.start()
        [attempt] = server.attempts
        self.assertTrue(attempt["outcome"].startswith("not ready"), attempt)
        self.assertIsNotNone(attempt["exit_code"])  # stopped and reaped
        self.assertTrue(refused(attempt["port"]))


class Refusals(unittest.TestCase):
    def test_a_scenario_that_would_check_less_than_it_says_is_refused(self):
        step = {"name": "s", "path": "/healthz", "expect": {"status": 200}}
        cases = [
            ({**step, "expect": {"status": 200, "stauts": 201}}, "unknown key 'stauts'"),
            ({**step, "expect": {"status": 200, "body": {"contians": "x"}}}, "unknown key 'contians'"),
            ({**step, "expect": {"status": 200, "json": {"$.a": {}}}}, "an empty predicate checks nothing"),
            ({k: v for k, v in step.items() if k != "expect"}, "no expect.status"),
            ({**step, "path": "/x/${repo}"}, "${repo} is neither"),
        ]
        for case, message in cases:
            with self.subTest(message=message), self.assertRaisesRegex(scenario.ScenarioError, re.escape(message)):
                scenario.from_dict({"step": [case]}, "case")


if __name__ == "__main__":
    unittest.main()
