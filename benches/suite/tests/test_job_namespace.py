"""The job namespace (suitekit/ns.py, scripts/lib/ns.sh), the cleanup by label only
(scripts/bench-cleanup.sh), the name lint (scripts/check-names.sh) and the gate's publication by
build identity (scripts/lib/gate-publish.sh). 19 Oct 2026: every fleet job's checkout is
`jobs/<id>/work`, so concurrent A-B-B-A jobs shared one build volume named after it, timed each
other's builds, and each one's cleanup removed the others'."""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "benches" / "suite"))
from suitekit import ns  # noqa: E402

# A POSIX bash: Git's on Windows, not WSL's launcher.
BASH = next((b for b in (["C:/Program Files/Git/bin/bash.exe"] if os.name == "nt" else []) if Path(b).is_file()),
            shutil.which("bash"))

# A Docker that keeps its containers and volumes, with their labels, in a JSON file.
FAKE = r'''
import json, os, sys, time
sys.stdout.reconfigure(newline="\n")  # Docker's own lines, on Windows too
state_path = os.environ["FAKE_DOCKER_STATE"]
lock = state_path + ".lock"
while True:
    try:
        os.mkdir(lock); break
    except FileExistsError:
        time.sleep(0.005)
try:
    state = json.load(open(state_path)) if os.path.exists(state_path) else {"containers": {}, "volumes": {}, "n": 0}
    args = sys.argv[1:]

    def options(rest):
        labels, filters, fmt, name, plain = {}, [], None, None, []
        i = 0
        while i < len(rest):
            a = rest[i]
            if a == "--label": k, _, v = rest[i + 1].partition("="); labels[k] = v; i += 2; continue
            if a == "--filter": filters.append(rest[i + 1]); i += 2; continue
            if a in ("-f", "--format"): fmt = rest[i + 1]; i += 2; continue
            if a == "--name": name = rest[i + 1]; i += 2; continue
            plain.append(a); i += 1
        return labels, filters, fmt, name, plain

    def matches(item, filters):
        for f in filters:
            if f.startswith("label="):
                k, _, v = f[6:].partition("=")
                if k not in item["labels"] or (v and item["labels"][k] != v):
                    return False
        return True

    def render(item, fmt):
        out = fmt
        for key in ("nrese.ns", "nrese.checkout", "nrese.owner"):
            value = item["labels"].get(key, "")
            out = out.replace('{{index .Labels "%s"}}' % key, value).replace('{{index .Config.Labels "%s"}}' % key, value)
        return out.replace("{{.CreatedAt}}", item["created"]).replace("{{.Created}}", item["created"])

    code = 0
    if args[:1] == ["info"]:
        pass
    elif args[:2] == ["volume", "create"]:
        labels, _, _, _, plain = options(args[2:])
        state["volumes"].setdefault(plain[-1], {"labels": labels, "created": os.environ.get("FAKE_CREATED", "2026-10-19T12:00:00Z")})
        print(plain[-1])
    elif args[:2] == ["volume", "rm"]:
        name = args[-1]
        users = [c for c in state["containers"].values() if name in c["volumes"]]
        if name not in state["volumes"] or users:
            code = 1
        else:
            del state["volumes"][name]
    elif args[:2] == ["volume", "inspect"]:
        _, _, fmt, _, plain = options(args[2:])
        item = state["volumes"].get(plain[-1])
        if item is None: code = 1
        elif fmt: print(render(item, fmt))
    elif args[:2] == ["volume", "ls"]:
        _, filters, _, _, _ = options(args[2:])
        for name, item in state["volumes"].items():
            if matches(item, filters): print(name)
    elif args[:1] == ["run"]:
        labels, _, _, name, plain = options(args[1:])
        state["n"] += 1
        cid = f"c{state['n']}"
        volumes = [plain[i + 1].split(":")[0] for i, a in enumerate(plain) if a == "-v"]
        if "-d" in plain:
            state["containers"][cid] = {"name": name, "labels": labels, "volumes": volumes,
                                        "created": os.environ.get("FAKE_CREATED", "2026-10-19T12:00:00Z")}
        print(cid)
    elif args[:2] == ["ps", "-a"]:
        _, filters, _, _, _ = options(args[2:])
        for cid, item in state["containers"].items():
            if matches(item, filters): print(cid)
    elif args[:1] == ["rm"]:
        for cid in [a for a in args[1:] if not a.startswith("-")]:
            state["containers"].pop(cid, None)
    elif args[:1] == ["inspect"]:
        _, _, fmt, _, plain = options(args[1:])
        item = state["containers"].get(plain[-1])
        if item is None: code = 1
        else: print(render(item, fmt))
    elif args[:1] == ["images"]:
        pass
    json.dump(state, open(state_path, "w"))
finally:
    os.rmdir(lock)
sys.exit(code)
'''


def fake_docker(tmp: Path) -> tuple[dict, dict]:
    """The fake's environment for Python (a .cmd on Windows) and for bash."""
    (tmp / "fakedocker.py").write_text(FAKE, encoding="utf-8")
    sh = tmp / "docker"
    sh.write_text(f'#!/bin/sh\nexec "{Path(sys.executable).as_posix()}" "{(tmp / "fakedocker.py").as_posix()}" "$@"\n',
                  newline="\n")
    sh.chmod(0o755)
    state = {"FAKE_DOCKER_STATE": str(tmp / "state.json")}
    if os.name == "nt":
        cmd = tmp / "docker.cmd"
        cmd.write_text(f'@"{sys.executable}" "{tmp / "fakedocker.py"}" %*\n')
        python_side = {**state, "NRESE_DOCKER": str(cmd)}
    else:
        python_side = {**state, "NRESE_DOCKER": str(sh)}
    return python_side, {**os.environ, **state, "NRESE_DOCKER": sh.as_posix()}


def state(tmp: Path) -> dict:
    import json
    return json.loads((tmp / "state.json").read_text())


@unittest.skipUnless(BASH, "needs bash")
class JobNamespaces(unittest.TestCase):
    def test_two_jobs_built_and_cleaned_concurrently_leave_each_other_intact(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            python_side, shell_side = fake_docker(tmp)
            jobs = {}
            for job in ("job-a", "job-b"):
                root = tmp / "jobs" / job / "work"  # fleet's checkout: the same name in every job
                root.mkdir(parents=True)
                env = {"FLEET_JOB": job}
                jobs[job] = (root, env, ns.name("target", root, env), ns.name("fast-case", root, env))
            self.assertNotEqual(jobs["job-a"][2], jobs["job-b"][2])
            with patch.dict(os.environ, python_side):
                for root, env, volume, container in jobs.values():
                    ns.docker(ns.volume("create", volume, root, "fast", env), check=True)
                    ns.docker(["run", "-d", *ns.name_args(container, root, "fast", env), "-v", f"{volume}:/target",
                               "img"], check=True)
                # Job a's own cleanup (fast.py clean --build) while job b's is the host's bench-cleanup
                # run in job b's namespace: at once.
                a_root, a_env, *_ = jobs["job-a"]
                cleaned = {}
                thread = threading.Thread(target=lambda: cleaned.update(a=ns.clean(a_root, "fast", environ=a_env)))
                thread.start()
                done = subprocess.run([BASH, (ROOT / "scripts" / "bench-cleanup.sh").as_posix(), "--owner", "nobody"],
                                      env={**shell_side, "FLEET_JOB": "job-b"}, capture_output=True, text=True,
                                      timeout=120)
                thread.join()
            self.assertEqual(done.returncode, 0, done.stderr)
            left = state(tmp)
            self.assertEqual(sorted(left["volumes"]), [jobs["job-b"][2]])  # b's: another owner's cleanup ran
            self.assertEqual([c["name"] for c in left["containers"].values()], [jobs["job-b"][3]])
            self.assertEqual(len(cleaned["a"]), 2)
            # Job b's whole cleanup takes b's and nothing else.
            done = subprocess.run([BASH, (ROOT / "scripts" / "bench-cleanup.sh").as_posix(), "--job"],
                                  env={**shell_side, "FLEET_JOB": "job-b"}, capture_output=True, text=True, timeout=120)
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual((state(tmp)["volumes"], state(tmp)["containers"]), ({}, {}))

    def test_the_host_sweep_takes_ended_jobs_labelled_resources_and_lists_unlabelled_ones(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            python_side, shell_side = fake_docker(tmp)
            ended, running = tmp / "jobs" / "ended" / "work", tmp / "jobs" / "running" / "work"
            ended.mkdir(parents=True)
            running.mkdir(parents=True)
            with patch.dict(os.environ, {**python_side, "FAKE_CREATED": "2026-10-01T00:00:00Z"}):
                for root, job in ((ended, "ended"), (running, "running")):
                    env = {"FLEET_JOB": job}
                    ns.docker(ns.volume("create", ns.name("target", root, env), root, "fast", env), check=True)
                ns.docker(["volume", "create", "nrese-target-work"], check=True)  # from before the labels
            shutil.rmtree(ended)  # its job ended; fleet removed its directory
            done = subprocess.run([BASH, (ROOT / "scripts" / "bench-cleanup.sh").as_posix()],
                                  env={**shell_side, "FLEET_JOB": "this-job"}, capture_output=True, text=True, timeout=120)
            self.assertEqual(done.returncode, 0, done.stderr)
            self.assertEqual(sorted(state(tmp)["volumes"]),
                             sorted([ns.name("target", running, {"FLEET_JOB": "running"}), "nrese-target-work"]))
            self.assertIn("not removed: nrese-target-work", done.stdout)

    def test_python_and_shell_name_alike(self):
        with tempfile.TemporaryDirectory() as tmp:
            for job in ("", "Consol-SCC@office"):
                done = subprocess.run([BASH, "-c", f'. "{(ROOT / "scripts/lib/ns.sh").as_posix()}"; ns_name target "$1"',
                                       "-", Path(tmp).as_posix()], env={**os.environ, "FLEET_JOB": job},
                                      capture_output=True, text=True)
                self.assertEqual(done.stdout, ns.name("target", tmp, {"FLEET_JOB": job}))


@unittest.skipUnless(BASH, "needs bash")
class NameLint(unittest.TestCase):
    def test_the_lint_fires_on_a_literal_name_and_passes_the_helpers(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            cases = {"bad.sh": 'docker volume create nrese-target\n', "bad2.sh": 'docker run --rm --name prep img\n',
                     "bad.py": 'subprocess.run(["docker", "run", "--name", "x", "img"])\n',
                     "bad3.sh": 'docker run --rm -v nrese-cargo:/c img\n',
                     "good.sh": '# docker volume rm nrese-target in a comment\nns_volume create "$v" "$ROOT" fast\n',
                     "good.py": 'p.add_argument("--name", default="x")\nns.docker(ns.volume("rm", v))\n'}
            for name, text in cases.items():
                (tmp / name).write_text(text, newline="\n")
                done = subprocess.run([BASH, (ROOT / "scripts" / "check-names.sh").as_posix(), (tmp / name).as_posix()],
                                      capture_output=True, text=True)
                self.assertEqual(done.returncode, 1 if name.startswith("bad") else 0, (name, done.stdout))


@unittest.skipUnless(BASH, "needs bash")
class GatePublication(unittest.TestCase):
    def test_concurrent_publishers_never_remove_or_replace_a_published_build(self):
        """Two gates building one identity: the first's build stays whole; nothing deletes it
        while a reader uses it; the age sweep spares one used within its window."""
        script = r'''
set -u
. "$1/scripts/lib/gate-publish.sh"
GATE_BIN=$2 GATE=$2/gate-x
dir=$GATE_BIN/bin-aaaaaaaa-identity
make() {  # a private build, its marker $1
  local part=$dir.partial-$1
  mkdir -p "$part/release" && echo "$1" > "$part/release/tool" && echo rev > "$part/REVISION"
  (cd "$part" && sha256sum release/tool > SHA256SUMS)
  echo "$part"
}
publish "$(make first)" "$dir" || exit 3
# A reader checks the build throughout while three more gates publish the same identity at once.
( for _ in $(seq 1 20); do build_verified "$dir" rev || { echo MISSING; exit 1; }; done ) & reader=$!
for k in 1 2 3; do ( publish "$(make "gate$k")" "$dir" || echo "PUBLISH $k" ) & done
wait $reader || echo READER
wait
cat "$dir/release/tool"
ls -d "$GATE_BIN"/*.partial-* 2>/dev/null
mkdir -p "$dir/.refs" && touch "$dir/.refs/gate-x"
sweep_builds
old=$GATE_BIN/bin-bbbbbbbb-identity
mkdir -p "$old/release" && touch -d "30 days ago" "$old/release" "$old"
sweep_builds
'''
        with tempfile.TemporaryDirectory() as tmp:
            done = subprocess.run([BASH, "-c", script, "-", ROOT.as_posix(), Path(tmp).as_posix()],
                                  capture_output=True, text=True, timeout=300)
        self.assertEqual(done.stdout.split(), ["first", "bin-bbbbbbbb-identity"], done.stdout + done.stderr)


if __name__ == "__main__":
    unittest.main()
