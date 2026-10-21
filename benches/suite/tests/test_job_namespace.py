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
    state.setdefault("networks", {})
    state.setdefault("images", {})
    if args[:1] == ["info"]:
        pass
    elif args[:1] in (["volume"], ["network"]) and args[1:2] == ["create"]:
        labels, _, _, _, plain = options(args[2:])
        state[args[0] + "s"].setdefault(plain[-1], {"labels": labels, "created": os.environ.get("FAKE_CREATED", "2026-10-19T12:00:00Z")})
        print(plain[-1])
    elif args[:1] in (["volume"], ["network"]) and args[1:2] == ["rm"]:
        name = args[-1]
        users = [c for c in state["containers"].values() if name in c["volumes"]]
        if name not in state[args[0] + "s"] or users:
            code = 1
        else:
            del state[args[0] + "s"][name]
    elif args[:1] in (["volume"], ["network"]) and args[1:2] == ["inspect"]:
        _, _, fmt, _, plain = options(args[2:])
        item = state[args[0] + "s"].get(plain[-1])
        if item is None: code = 1
        elif fmt: print(render(item, fmt))
    elif args[:1] in (["volume"], ["network"]) and args[1:2] == ["ls"]:
        _, filters, _, _, _ = options(args[2:])
        for name, item in state[args[0] + "s"].items():
            if matches(item, filters): print(name)
    elif args[:2] == ["image", "inspect"]:
        _, _, fmt, _, plain = options(args[2:])
        image = state["images"].get(plain[-1])
        if image is None: code = 1
        elif fmt: print(image)
    elif args[:1] == ["pull"]:
        code = 1  # nothing to pull from here
    elif args[:1] == ["run"]:
        labels, _, _, name, plain = options(args[1:])
        state.setdefault("runs", []).append(args)
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


def bash(script: str, *args: str, env: dict | None = None, timeout: int = 300) -> subprocess.CompletedProcess:
    return subprocess.run([BASH, "-c", script, "-", *args], cwd=ROOT, env={**os.environ, **(env or {})},
                          capture_output=True, text=True, encoding="utf-8", timeout=timeout)


@unittest.skipUnless(BASH, "needs bash")
class GateBuilds(unittest.TestCase):
    def test_a_build_used_between_the_sweeps_look_and_its_delete_stays(self):
        """Sol, 20 Oct 2026: the sweep found a build old, a gate then stamped it, and the sweep
        deleted it anyway. Use and removal share the lock; the sweep checks again under it."""
        script = r'''
. scripts/lib/gate-publish.sh
GATE_BIN=$1 GATE=$1/gate-x NRESE_LOCK_DIR=$1/locks
dir=$GATE_BIN/bin-aaaaaaaa-identity
mkdir -p "$dir/release" "$GATE/builds" && echo tool > "$dir/release/tool" && echo rev > "$dir/REVISION"
(cd "$dir" && sha256sum release/tool > SHA256SUMS)
echo bin-aaaaaaaa-identity > "$GATE/builds/$(git rev-parse --short=8 HEAD)"
find "$dir" -exec touch -d "30 days ago" {} +
# A gate uses the build right after the sweep's first look found it old.
eval "first_$(declare -f old_build)"
looks=0
old_build() { looks=$((looks + 1)); local r=0; first_old_build "$1" || r=1; [ $looks = 1 ] && bin_of HEAD >/dev/null; return $r; }
sweep_builds
[ -f "$dir/release/tool" ] && echo kept
# Unused, it goes; a gate that asks for it then fails closed, never recreating it.
unset -f old_build; eval "$(declare -f first_old_build | sed 1s/first_old_build/old_build/)"
find "$dir" -exec touch -d "30 days ago" {} +
sweep_builds
bin_of HEAD; [ -e "$dir" ] || echo gone
'''
        with tempfile.TemporaryDirectory() as tmp:
            done = bash(script, Path(tmp).as_posix())
        self.assertEqual(done.stdout.split()[:2], ["kept", "bin-aaaaaaaa-identity"], done.stdout + done.stderr)
        self.assertTrue(done.stdout.split()[2].endswith("/none"), done.stdout)
        self.assertEqual(done.stdout.split()[3], "gone")

    def test_a_portable_and_an_x86_64_v3_build_have_different_identities(self):
        """Sol, 20 Oct 2026: the identity hashed RUSTFLAGS as given; cargo-guarded.sh adds the CPU
        choice (NRESE_TARGET_CPU), so a portable build's identity was an x86-64-v3 build's."""
        if shutil.which("rustc") is None:
            self.skipTest("no rustc")
        script = r'''
. scripts/lib/gate-publish.sh
GATE_BIN=/unused
for cpu in portable x86-64-v3; do NRESE_TARGET_CPU=$cpu identity_dir HEAD; done
NRESE_TARGET_CPU=x86-64-v3 build_identity HEAD | grep '^rustflags'
'''
        done = bash(script, env={"RUSTFLAGS": ""})
        portable, v3, flags = done.stdout.splitlines()
        self.assertNotEqual(portable, v3, done.stderr)
        self.assertEqual(flags, "rustflags -C target-cpu=x86-64-v3")


@unittest.skipUnless(BASH, "needs bash")
class Coverage(unittest.TestCase):
    def test_two_jobs_perf_lab_runs_of_one_label_keep_their_own_stores(self):
        """Sol, 20 Oct 2026: scripts/office/perflab.sh's store-LABEL was every job's: job b's run
        removed job a's store."""
        # A fake perf lab, wherever the script looks for it: job a's first run marks its store and
        # waits while job b runs the same label start to end, then says whether its store is there.
        script = r'''
. scripts/lib/ns.sh
s=$HOME/nrese-bench/scratch
fake='#!/bin/sh
[ "$1" = --store ] || exit 0
mkdir -p "$2"
if [ "$FLEET_JOB" = job-a ] && [ ! -e "$HOME/started" ]; then
  echo a > "$2/owner"; touch "$HOME/started"
  while [ ! -e "$HOME/go" ]; do sleep 0.1; done
  cat "$2/owner" > "$HOME/result" 2>/dev/null || echo missing > "$HOME/result"
fi'
for lab in "$s/perf_lab-run" "$(FLEET_JOB=job-a ns_dir "$s" perf_lab "$PWD")/run" "$(FLEET_JOB=job-b ns_dir "$s" perf_lab "$PWD")/run"; do
  mkdir -p "$(dirname "$lab")" && printf '%s\n' "$fake" > "$lab" && chmod +x "$lab"
done
FLEET_JOB=job-a NO_BUILD=1 bash scripts/office/perflab.sh run >/dev/null 2>&1 & a=$!
until [ -e "$HOME/started" ]; do sleep 0.1; done
FLEET_JOB=job-b NO_BUILD=1 bash scripts/office/perflab.sh run >/dev/null 2>&1
touch "$HOME/go"; wait $a
cat "$HOME/result"
'''
        with tempfile.TemporaryDirectory() as tmp:
            done = bash(script, env={"HOME": Path(tmp).as_posix()})
        self.assertEqual(done.stdout.strip(), "a", done.stderr)

    def test_a_jobs_network_is_labelled_cleaned_with_it_and_swept_after_it(self):
        """Sol, 20 Oct 2026: prepare-npd-data.sh's network had no labels: an aborted job left it."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            _, shell_side = fake_docker(tmp)
            mine, ended = tmp / "mine", tmp / "ended"
            mine.mkdir()
            ended.mkdir()
            script = r'''
. scripts/lib/ns.sh
FLEET_JOB=mine ns_network create "$(FLEET_JOB=mine ns_name npd-net "$1")" "$1" reasoning >/dev/null
FAKE_CREATED=2026-10-01T00:00:00Z FLEET_JOB=ended ns_network create "$(FLEET_JOB=ended ns_name npd-net "$2")" "$2" reasoning >/dev/null
FLEET_JOB=mine ns_clean "$1" "" all
'''
            done = bash(script, mine.as_posix(), ended.as_posix(), env=shell_side)
            self.assertEqual(done.stdout.split(), [ns.name("npd-net", mine, {"FLEET_JOB": "mine"})], done.stderr)
            self.assertEqual(state(tmp)["networks"][ns.name("npd-net", ended, {"FLEET_JOB": "ended"})]["labels"]["nrese.ns"],
                             "ended")
            shutil.rmtree(ended)
            done = subprocess.run([BASH, (ROOT / "scripts" / "bench-cleanup.sh").as_posix()],
                                  env={**shell_side, "FLEET_JOB": "this-job"}, capture_output=True, text=True, timeout=120)
            self.assertEqual((done.returncode, state(tmp)["networks"]), (0, {}), done.stderr)

    def test_shared_dataset_preparation_runs_one_at_a_time(self):
        """Sol, 20 Oct 2026: prepare-datasets.sh's staging ($name.parts, .part files) was every
        job's: a second preparer removed the first's and published an incomplete file."""
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            prep = tmp / "prep.sh"
            prep.write_text(f'#!/usr/bin/env bash\nset -euo pipefail\n. "{(ROOT / "scripts/lib/ns.sh").as_posix()}"\n'
                            'ns_serialize datasets "$0" "$@"\n'
                            f'echo "start $1" >> "{(tmp / "log").as_posix()}"; sleep 1\n'
                            f'echo "end $1" >> "{(tmp / "log").as_posix()}"\n', newline="\n")
            env = {**os.environ, "NRESE_LOCK_DIR": (tmp / "locks").as_posix()}
            runs = [subprocess.Popen([BASH, prep.as_posix(), str(k)], env=env) for k in (1, 2)]
            for run in runs:
                run.wait(timeout=120)
            log = (tmp / "log").read_text().split("\n")[:4]
        self.assertEqual([line.split()[0] for line in log], ["start", "end", "start", "end"], log)
        self.assertEqual(log[0].split()[1], log[1].split()[1])

    def test_the_lint_takes_mounts_names_and_compose_and_documents_what_it_cant(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            cases = {"default.sh": 'docker run --rm -v "${X:-nrese-target}:/t" img\n',
                     "assigned.sh": 'vol=sc-$system\ndocker run --rm -v "$vol:/s" img\n',
                     "fstring.py": 'mount = f"nrese-suite-{run}-store"\n',
                     "compose.yml": 'services:\n  w:\n    container_name: shared\n',
                     "network.sh": 'docker network create npd\n',
                     # What it can't tell apart (a host directory or a volume; a flag built at run
                     # time): passes, so review covers these forms.
                     "env-only.sh": 'docker run --rm -v "$SHARED_VOLUME:/target" img\n',
                     "concat.py": "args = ['docker', 'run', '--' + 'name', 'shared', 'img']\n"}
            for name, text in cases.items():
                (tmp / name).write_text(text, newline="\n")
                done = subprocess.run([BASH, (ROOT / "scripts" / "check-names.sh").as_posix(), (tmp / name).as_posix()],
                                      capture_output=True, text=True)
                expected = 0 if name in ("env-only.sh", "concat.py") else 1
                self.assertEqual(done.returncode, expected, (name, done.stdout))


@unittest.skipUnless(BASH, "needs bash")
class Timing(unittest.TestCase):
    def test_images_are_pinned_at_the_start_or_the_job_stops(self):
        """Sol, 20 Oct 2026: a tag that couldn't be resolved was run by its tag, and a later
        resolution could pin another image within one comparison."""
        import json
        from suitekit.runtime import DockerRuntime, Spec
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            python_side, _ = fake_docker(tmp)
            (tmp / "state.json").write_text(json.dumps({"containers": {}, "volumes": {}, "n": 0,
                                                        "images": {"img:1": "sha256:first"}}))
            env = {**python_side, "NRESE_LOCK_DIR": str(tmp / "locks"), "FLEET_JOB": "job-a"}
            with patch.dict(os.environ, env):
                # A tag neither here nor pullable stops the job at its start, nothing pinned.
                with self.assertRaises(ns.ImageError):
                    ns.pin_images(["img:1", "img:gone"], ROOT)
                self.assertEqual(ns.pinned(ROOT), {})
                with self.assertRaises(SystemExit):
                    DockerRuntime(tmp, 0).pin(["img:gone"])
                done = bash('. scripts/lib/ns.sh; ns_pin_images "$PWD" img:gone || echo stopped', env=env)
                self.assertEqual(done.stdout.split()[-1:], ["stopped"], done.stderr)
                # Pinned at the start; another job retags; a later step pins again: the first ID.
                self.assertEqual(ns.pin_images(["img:1"], ROOT), {"img:1": "sha256:first"})
                retagged = state(tmp)
                retagged["images"]["img:1"] = "sha256:second"
                (tmp / "state.json").write_text(json.dumps(retagged))
                ns.pin_images(["img:1"], ROOT)
                args = DockerRuntime(tmp, 0).args(Spec("x", "img:1", ["serve"]), detached=True)
                self.assertEqual((args[-1], "img:1" in args), ("sha256:first", False))
                # A tag the job didn't pin is never run by its tag.
                with self.assertRaises(ns.ImageError):
                    DockerRuntime(tmp, 0).args(Spec("y", "img:other", ["serve"]), detached=True)
                done = bash('. scripts/lib/ns.sh; echo "$(ns_image img:1 "$PWD")"; ns_image img:other "$PWD" || echo refused', env=env)
                self.assertEqual(done.stdout.split(), ["sha256:first", "refused"], done.stderr)

    def test_a_timed_gate_pass_refuses_to_make_data(self):
        import argparse
        sys.path.insert(0, str(ROOT / "benches" / "fast"))
        import fast
        args = argparse.Namespace(cases="lab-olympics", areas=None, prepared=False, no_routes=False,
                                  empty_queries=False)
        with patch.dict(os.environ, {"NRESE_TIMED_GATE": "1"}), patch.object(fast, "RUNTIME", "native"), \
                patch.object(fast, "native_dirs", return_value={}), patch.dict(fast.OPTIONS):
            with self.assertRaisesRegex(SystemExit, "prepared data only"):
                fast.run(args)

    def test_a_long_locked_command_arrives_whole_without_flock(self):
        """20 Oct 2026: where there is no flock (Windows), the locked shell got its functions as one
        -c argument; past about 8 KB it arrived cut short and the command failed."""
        script = r'''
. scripts/lib/ns.sh
eval "long() { $(for i in $(seq 0 399); do printf 'echo "line %s of a long function"; ' "$i"; done) }"
ns_locked_python "$1/a.lock" long
'''
        with tempfile.TemporaryDirectory() as tmp:
            done = bash(script, Path(tmp).as_posix())
        self.assertEqual((done.returncode, done.stdout.splitlines()[-1:]), (0, ["line 399 of a long function"]),
                         done.stderr)

    def test_a_holder_killed_mid_hold_leaves_the_lock_free_at_once(self):
        """Sol, 20 Oct 2026: the lock directory's pid was the parent's, and Python never took over
        a dead holder's: a crash blocked every later preparation. An OS lock goes with its holder."""
        import signal
        suite = (ROOT / "benches" / "suite").as_posix()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "datasets.lock"
            holders = {
                "python": [sys.executable, "-c", f"import sys, time; sys.path.insert(0, {suite!r})\n"
                           f"from suitekit.lock import lock\nwith lock({str(path)!r}):\n"
                           "    print('held', flush=True); time.sleep(120)"],
                "shell": [BASH, "-c", f'. "{(ROOT / "scripts/lib/ns.sh").as_posix()}"; '
                                      f'ns_locked "{path.as_posix()}" sh -c "echo held; sleep 120"'],
            }
            for kind, command in holders.items():
                holder = subprocess.Popen(command, stdout=subprocess.PIPE, text=True, start_new_session=os.name != "nt")
                self.assertEqual(holder.stdout.readline().strip(), "held", kind)
                if os.name == "nt":  # the holder and what it started, at once (SIGKILL's equivalent)
                    subprocess.run(["taskkill", "/F", "/T", "/PID", str(holder.pid)], capture_output=True)
                else:
                    os.killpg(holder.pid, signal.SIGKILL)
                holder.wait(timeout=30)
                got = threading.Event()

                def take():
                    with ns.lock(path):
                        got.set()
                taker = threading.Thread(target=take, daemon=True)
                taker.start()
                # A bound for a lock that never comes free, not a speed: the holder is gone.
                self.assertTrue(got.wait(timeout=10), f"{kind}: the lock stayed held after its holder died")
                # Its handle closed before the directory goes (Windows refuses to remove an open file).
                taker.join(timeout=10)


@unittest.skipUnless(BASH, "needs bash")
class PinSets(unittest.TestCase):
    """Sol, 20 Oct 2026: the reasoning scorecard pinned Jena's image for one of its four Jena
    systems, so the others failed after the rest had timed; the pins and the runs were two lists."""

    def test_every_system_a_scorecard_can_run_has_its_image_pinned(self):
        script = r'''
. benches/reasoning/systems.sh
RUST_IMAGE=rust:test
for system in $REASONING_SYSTEMS; do
  image=$(system_image "$system") || exit 3
  pin_set $REASONING_SYSTEMS | grep -qxF "$image" || { echo "unpinned $system"; exit 4; }
done
pin_set oracle jena-unknown >/dev/null 2>&1 || echo refused
'''
        done = bash(script)
        self.assertEqual((done.returncode, done.stdout.split()), (0, ["refused"]), done.stdout + done.stderr)
        scorecard = (ROOT / "benches/reasoning/reasoning-scorecard.sh").read_text(encoding="utf-8")
        # Every jena-* system it dispatches is one of the table's, and its runs name systems, never images.
        self.assertIn('measured "$2" "jena-$3"', scorecard)
        self.assertNotRegex(scorecard, r'measured "\$2" (nrese-bench/|"\$RUST_IMAGE")')
        # The competitors' scorecard: its table names every system it can load and serve.
        import re
        competitors = (ROOT / "benches/competitors/scorecard.sh").read_text(encoding="utf-8")
        table = set(re.findall(r"\[(\w+)\]=\w+_IMAGE", competitors))
        loaders = set(re.findall(r"^load_(\w+)\(\)", competitors, re.M))
        self.assertTrue(loaders and loaders <= table, (loaders, table))

    def test_the_calibration_container_runs_its_pinned_image(self):
        """Sol, 20 Oct 2026: the manifest timed a no-op container of alpine:latest by its tag."""
        import json
        from suitekit import manifest
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            python_side, _ = fake_docker(tmp)
            env = {**python_side, "NRESE_LOCK_DIR": str(tmp / "locks"), "FLEET_JOB": "job-a"}
            (tmp / "state.json").write_text(json.dumps({"containers": {}, "volumes": {}, "n": 0, "images": {}}))
            with patch.dict(os.environ, env):
                with self.assertRaises(ns.ImageError):  # not resolvable: the job stops
                    manifest.container_start_ms(ROOT)
                (tmp / "state.json").write_text(json.dumps({"containers": {}, "volumes": {}, "n": 0,
                                                            "images": {manifest.CALIBRATION_IMAGE: "sha256:cal"}}))
                self.assertIsNotNone(manifest.container_start_ms(ROOT))
            runs = state(tmp)["runs"]
            self.assertEqual(len(runs), 3)
            self.assertTrue(all("sha256:cal" in r and manifest.CALIBRATION_IMAGE not in r for r in runs), runs)


if __name__ == "__main__":
    unittest.main()
