"""Build admission (scripts/cargo-guarded.sh): a cargo run waits for a slot and until its declared
memory peak fits above the floor, never building anyway, and for no earlier run that can't start
itself; its wait is bounded, terminal or not; a request that can never fit is refused at once; a
dead holder is reclaimed by its identity; and a run says
it was stopped by the main PC's guard only when the guard stopped a process of its own build.
Sol's round-1 probes (Harness/notes/review-build-admission-r1-20261020/probe.py) are here with
the behaviour they asked for.

Run: python -m unittest discover -s benches/suite/tests -p test_build_admission.py
"""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[3]
BASH = shutil.which("bash")
GB = 1024 * 1024  # kB


class Admission(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="nrese-admission-test-")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.repo = self.root / "repo"
        for rel in ("scripts/cargo-guarded.sh", "scripts/check.sh", "scripts/lib/disk.sh", "scripts/lib/target-cpu.sh",
                    "scripts/lib/prune-stale.py", "scripts/lib/memory-cap.ps1", "scripts/lib/msvc-finalize.py"):
            (self.repo / rel).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(ROOT / rel, self.repo / rel)
        self.stubs = self.root / "stubs"
        self.stubs.mkdir()
        # cargo: says when it started and with how many jobs, runs a while, and ends with $CODE.
        # The guard's notices (an intent, then STOPPED or STOP-FAILED), by NOTICE: late, the machine
        # turns busy and a completed stop of this build's process comes a second after cargo ended;
        # other, a completed stop of another build's; failed, an intent for this build whose stop
        # fails two seconds later; intent, an intent that never completes.
        self.stub("cargo", 'echo "$(date +%s%N) $CARGO_BUILD_JOBS" > "$OUT/$RUN.started"\n'
                           'sleep "${SLEEP:-0}"\n'
                           'log() { echo "$(date +%s) $*" >> "$GUARD/killed.log"; }\n'
                           'intent() { log "KILLED-BY-GUARD 4242 rustc.exe 4242@1 jobs=$1"; }\n'
                           'case "${NOTICE:-}" in\n'
                           '  late) echo busy > "$GUARD/BUSY"\n'
                           '    (sleep 1; intent "$NRESE_BUILD_JOB"; log GUARD-STOPPED 4242@1) > /dev/null 2>&1 & ;;\n'
                           '  other) intent nrese-build-another; log GUARD-STOPPED 4242@1 ;;\n'
                           '  failed) intent "$NRESE_BUILD_JOB"; (sleep 2; log GUARD-STOP-FAILED 4242@1) > /dev/null 2>&1 & ;;\n'
                           '  intent) intent "$NRESE_BUILD_JOB" ;;\n'
                           '  after) intent "$NRESE_BUILD_JOB"; (/usr/bin/sleep 3; log GUARD-STOPPED 4242@1) > /dev/null 2>&1 & ;;\n'
                           'esac\n'
                           'echo "$(date +%s%N)" > "$OUT/$RUN.ended"\nexit "${CODE:-0}"')
        self.stub("nproc", "echo 32")  # 12 compiler jobs by default
        self.guard = self.root / "guard"
        self.guard.mkdir()
        self.out = self.root / "out"
        self.out.mkdir()
        self.free(24)
        self.disk(400)

    def stub(self, name, body):
        (self.stubs / name).write_text("#!/bin/sh\n" + body + "\n")
        (self.stubs / name).chmod(0o755)

    def disk(self, gb):
        (self.root / "diskfree").write_text(f"{gb}\n")

    def free(self, gb):
        (self.root / "meminfo").write_text(f"MemTotal: {64 * GB} kB\nMemFree: {gb * GB} kB\n")

    def env(self, run, **env):
        base = {k: v for k, v in os.environ.items() if not k.startswith(("GIT_", "NRESE_", "CARGO_", "FLEET_"))}
        merged = {**base, "HOME": str(self.root), "PATH": f"{self.stubs}{os.pathsep}{os.environ['PATH']}",
                "OUT": str(self.out), "RUN": run, "GUARD": str(self.guard),
                "NRESE_BUILD_SLOT_DIR": str(self.root / "slots"), "NRESE_GUARD_BUSY": str(self.guard / "BUSY"),
                "NRESE_GUARD_KILLED": str(self.guard / "killed.log"), "NRESE_MEMINFO": str(self.root / "meminfo"),
                "NRESE_BUILD_FLOOR_GB": "16", "NRESE_MEMORY_CAP_GB": "0", "NRESE_MIN_FREE_GB": "0",
                "NRESE_TARGET_BUDGET_GB": "1000", "CARGO_TARGET_DIR": str(self.root / "target"),
                "NRESE_QUIET_DIR": str(self.root / "no-quiet-slot"), "NRESE_BUILD_SLOT_WAIT_S": "600",
                "NRESE_BUILD_SLOT_POLL_S": "1",
                "NRESE_GUARD_DISKFREE": str(self.root / "diskfree"), **env}
        # None: not set at all (the script's own default).
        return {k: v for k, v in merged.items() if v is not None}

    def start(self, run, stderr=subprocess.DEVNULL, **env):
        build = subprocess.Popen([BASH, "scripts/cargo-guarded.sh", "build"], cwd=self.repo,
                                 env=self.env(run, **env), stdout=subprocess.DEVNULL, stderr=stderr, text=True)
        self.addCleanup(lambda: build.poll() is not None or (build.kill(), build.wait()))
        return build

    def started(self, run, within):
        deadline = time.time() + within
        while time.time() < deadline:
            if (self.out / f"{run}.started").exists():
                return (self.out / f"{run}.started").read_text().split()
            time.sleep(0.2)
        return None

    def late_writer_done(self):
        # The stub's late writer (NOTICE=after) ends before the test does: Windows can't remove a
        # directory a process still writes to.
        deadline = time.time() + 15
        while "GUARD-STOPPED" not in (self.guard / "killed.log").read_text() and time.time() < deadline:
            time.sleep(0.2)

    def ended(self, run):
        return int((self.out / f"{run}.ended").read_text())

    def test_fleet_resource_projection_preserves_the_declared_peak(self):
        self.free(64)
        shutil.copy(ROOT / "scripts/lib/gate-job.sh", self.repo / "scripts/lib/gate-job.sh")
        # Keep the real guard. Its existing helper interface only records the cap
        # here: this fixture must not attach a new JobObject or compile Rust.
        helper = self.repo / "scripts/lib/memory-cap.ps1"
        helper.write_text('''param([int]$ProcessId, [double]$LimitGB, [string]$Registry)
[IO.File]::WriteAllText((Join-Path $env:OUT ($env:RUN+'.cap')), $LimitGB.ToString([Globalization.CultureInfo]::InvariantCulture))
Write-Output 'fixture-memory-job'
''' + f"# {self.root.name}\n", encoding="utf-8")
        # This admission-only fixture has no actual JobObject. Match its fake cap
        # boundary with a fake compiler finalizer; real membership is covered below.
        (self.repo / "scripts/lib/msvc-finalize.py").write_text(
            "import sys\nfrom pathlib import Path\nif sys.argv[1] == 'prepare':\n"
            " Path(sys.argv[sys.argv.index('--state')+1]+'.env').write_bytes(b'')\n print('fixture-binding')\n")
        self.stub("cargo", 'cat "$NRESE_BUILD_SLOT_DIR"/run/*/peak > "$OUT/$RUN.peak"\n' +
                  (self.stubs / "cargo").read_text())
        transport = self.root / "transport"
        transport.mkdir()
        (transport / "sitecustomize.py").write_text(
            "import os,sys\nsys.stdout.reconfigure(newline=os.environ['RESOURCE_ENDING'])\n")
        observe = '''set -T
trap 'case "$BASH_COMMAND" in "export CARGO_BUILD_WARNINGS="*) printf "%s\\0" "$CARGO_BUILD_JOBS" "$NRESE_MEMORY_CAP_GB" "$NRESE_BUILD_PEAK_GB" > "$PROJECTION"; exit 0;; esac' DEBUG
source scripts/lib/gate-job.sh "$@"
'''
        try:
            # The actual failing whole-valued budget under both transports, and
            # one fractional ask proving it is retained, never rounded to fit.
            for run, ram, ending, expected in (("whole-lf", 24.0, "\n", "24"),
                                               ("whole-crlf", 24.0, "\r\n", "24"),
                                               ("fraction-crlf", 24.5, "\r\n", "24.5")):
                with self.subTest(run=run):
                    projection = self.out / (run + ".projection")
                    prologue = self.out / (run + ".prologue")
                    env = self.env(run, FLEET_JOB="resource-fixture", FLEET_STEP="1", FLEET_OUT=str(prologue),
                        NRESE_GATE_REQUEST="request", NRESE_GATE_BOUNDED="1",
                        NRESE_GATE_NEEDS=json.dumps([{"cores": 16, "ram_gb": ram}]),
                        PYTHONPATH=transport.as_posix(), RESOURCE_ENDING=ending, PROJECTION=projection.as_posix(),
                        PATH=str(Path(sys.executable).parent)+os.pathsep+str(Path(BASH).parent)+os.pathsep+self.env(run)["PATH"])
                    done = subprocess.run([BASH, "-c", observe, "resource-prologue", "push", "tree", "list",
                        "windows" if os.name == "nt" else "rest", "0", "request", "base", "flags", "compiler", "--"],
                        cwd=self.repo, env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=30)
                    (self.out / (run + ".prologue.log")).write_bytes(done.stdout + done.stderr)
                    self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
                    values = projection.read_bytes().split(b"\0")[:-1]
                    projected = dict(zip(("CARGO_BUILD_JOBS", "NRESE_MEMORY_CAP_GB", "NRESE_BUILD_PEAK_GB"),
                                         (value.decode() for value in values)))
                    build = self.start(run, stderr=subprocess.PIPE, **projected)
                    _, err = build.communicate(timeout=60)
                    (self.out / (run + ".guard.log")).write_text(err)
                    started = (self.out / (run + ".started")).exists()
                    (self.out / (run + ".json")).write_text(json.dumps({"projected": projected,
                        "code": build.returncode, "cargo_started": started, "stderr": err}))
                    self.assertEqual(projected, {"CARGO_BUILD_JOBS": "16", "NRESE_MEMORY_CAP_GB": expected,
                                                 "NRESE_BUILD_PEAK_GB": expected})
                    if ram == 24.0:
                        self.assertEqual(build.returncode, 0, err)
                        self.assertEqual(self.started(run, within=1)[1], "16")
                        self.assertEqual((self.out / (run + ".peak")).read_text().strip(), "24576")
                    else:
                        # Fractional native peak support is outside this fix;
                        # unchanged admission refuses it before Cargo starts.
                        self.assertNotEqual(build.returncode, 0)
                        self.assertIn("invalid arithmetic operator", err)
                        self.assertFalse(started)
                    if os.name == "nt":
                        self.assertEqual((self.out / (run + ".cap")).read_text(), expected)
        finally:
            if os.environ.get("FLEET_OUT"):
                shutil.copytree(self.out, Path(os.environ["FLEET_OUT"]) / ("resource-fixture-" + self.root.name), dirs_exist_ok=True)

    @unittest.skipUnless(os.name == "nt", "native MSVC helper lifetime uses Windows JobObjects")
    def test_compiler_completion_disposes_only_the_prebound_owned_helper(self):
        # Spec: root's bounded alternative B (23 Oct 2026): after synchronous Cargo,
        # stop only the pre-bound compiler-install helper in this exact build job;
        # foreign and unselected-install/test workers survive, unknowns refuse
        # completion, and a Cargo failure remains primary. Root review r1 adds:
        # selection reaches Cargo; conflicting target selectors refuse before it.
        import _winapi
        import ctypes

        installation = self.root / "compiler"
        outsider = self.root / "not-the-compiler"
        for directory in (installation, outsider):
            directory.mkdir()
            shutil.copyfile(sys.executable, directory / "VCTIP.EXE")
            for dll in Path(sys.base_prefix).glob("python3*.dll"):
                shutil.copyfile(dll, directory / dll.name)
                (directory / (dll.stem + "._pth")).write_text(str(Path(sys.base_prefix) / "Lib") + "\n")
            shutil.copyfile(sys.executable, directory / "cl.exe")
        sleeper = ["-I", "-c", "import time; time.sleep(120)"]
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        get_times = kernel.GetProcessTimes
        get_times.argtypes = [ctypes.c_void_p] + [ctypes.POINTER(ctypes.c_uint64)] * 4
        get_times.restype = ctypes.c_int
        kernel.OpenJobObjectW.argtypes = [ctypes.c_ulong, ctypes.c_int, ctypes.c_wchar_p]
        kernel.OpenJobObjectW.restype = ctypes.c_void_p
        kernel.AssignProcessToJobObject.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        kernel.AssignProcessToJobObject.restype = ctypes.c_int
        kernel.CloseHandle.argtypes = [ctypes.c_void_p]
        controller = self.root / "fake-cargo.py"
        controller.write_text('''import json,os,subprocess,sys,time
from pathlib import Path
out=Path(os.environ['OUT']); run=os.environ['RUN']
images=[str(Path(os.environ.get('CC') or os.environ['NRESE_MSVC_COMPILER']).with_name('VCTIP.EXE'))]
if os.environ['UNKNOWN']=='1': images += [os.environ['OTHER_IMAGE'],sys.executable]
children=[subprocess.Popen([image,'-I','-c','import time; time.sleep(120)'],
    stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL) for image in images]
ready=out/(run+'.workers'); temp=ready.with_suffix('.tmp')
temp.write_text(json.dumps({'pids':[p.pid for p in children], 'job':os.environ['NRESE_BUILD_JOB'],
    'compiler_environment':{k:os.environ.get(k) for k in ('CC','CXX','HOST_CC','TARGET_CXX',
        'CC_x86_64_pc_windows_msvc','CXX_x86_64_pc_windows_msvc','INCLUDE','LIB',
        'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER')}})); temp.replace(ready)
deadline=time.monotonic()+45
while not (out/(run+'.held')).exists():
    if time.monotonic()>deadline: sys.exit(98)
    time.sleep(.05)
sys.exit(int(os.environ['CODE']))
''')
        self.stub("cargo", 'exec python "$CONTROLLER"')
        observations = []
        try:
            for run, unknown, cargo_code, expected in (("known", "0", 0, 0),
                                                       ("unknown", "1", 0, 75),
                                                       ("cargo-failed", "1", 19, 19)):
                with self.subTest(run=run):
                    handles = []
                    foreign = None
                    unrelated_bash = None
                    build = self.start(run, stderr=subprocess.PIPE, CONTROLLER=str(controller), UNKNOWN=unknown,
                        OTHER_IMAGE=str(outsider / "VCTIP.EXE"), CODE=str(cargo_code),
                        NRESE_MSVC_COMPILER=str(installation / "cl.exe"),
                        INCLUDE=str(installation), LIB=str(installation), CC=None, CXX=None,
                        CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER='rust-lld.exe')
                    try:
                        ready = self.out / (run + ".workers")
                        deadline = time.monotonic() + 60
                        while not ready.exists() and build.poll() is None and time.monotonic() < deadline:
                            time.sleep(.05)
                        self.assertTrue(ready.exists(), "Cargo fixture never started")
                        observed = json.loads(ready.read_text())
                        pids = observed['pids']
                        # Cargo has started only after prepare, and cannot return before
                        # our acknowledgement. The same-image foreign process therefore
                        # falls inside the disposal time window: only membership protects it.
                        foreign = subprocess.Popen([str(installation / "VCTIP.EXE"), *sleeper],
                            stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                        times = [ctypes.c_uint64() for _ in range(4)]
                        self.assertTrue(get_times(int(foreign._handle), *(ctypes.byref(t) for t in times)))
                        foreign_creation = times[0].value
                        if unknown == '1':
                            # Same image and creation window as the execution caller,
                            # but explicitly a worker in this job. Its held stdin keeps
                            # the Bash builtin blocked without creating another child.
                            unrelated_bash = subprocess.Popen([BASH, '-c', 'read -r -t 120 line'],
                                stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                            member_job = kernel.OpenJobObjectW(1, False, observed['job'])
                            self.assertTrue(member_job)
                            try:
                                self.assertTrue(kernel.AssignProcessToJobObject(member_job, int(unrelated_bash._handle)))
                            finally:
                                kernel.CloseHandle(member_job)
                            self.assertTrue(get_times(int(unrelated_bash._handle), *(ctypes.byref(t) for t in times)))
                            bash_creation = times[0].value
                        # The fixture Cargo waits for this acknowledgement: these handles
                        # refer to its still-live children, and remain held across finalization.
                        for pid in pids:
                            handle = _winapi.OpenProcess(0x100000 | 0x1000 | 1, False, pid)
                            handles.append(handle)
                            self.assertEqual(_winapi.WaitForSingleObject(handle, 0), 258)
                        (self.out / (run + ".held")).touch()
                        _, err = build.communicate(timeout=60)
                        (self.out / (run + ".log")).write_text(err)
                        observation = {'case': run, 'code': build.returncode,
                            'selected_compiler': str(installation / 'cl.exe'),
                            'cargo_environment': observed['compiler_environment'],
                            'helper_alive': _winapi.WaitForSingleObject(handles[0], 0) == 258,
                            'helper_exit': _winapi.GetExitCodeProcess(handles[0]),
                            'foreign_pid': foreign.pid, 'foreign_alive': foreign.poll() is None,
                            'foreign_creation': foreign_creation,
                            'unrelated_bash_pid': unrelated_bash.pid if unrelated_bash else None,
                            'unrelated_bash_alive': unrelated_bash.poll() is None if unrelated_bash else None,
                            'unknowns_alive': [_winapi.WaitForSingleObject(h, 0) == 258 for h in handles[1:]]}
                        completion_path = self.repo / 'tmp' / ('msvc-' + observed['job'] + '.completion.json')
                        observation['completion'] = json.loads(completion_path.read_text()) if completion_path.exists() else None
                        observations.append(observation)  # Preserve violated behavior even if an earlier assertion fails.
                        self.assertEqual(_winapi.WaitForSingleObject(handles[0], 0), 0,
                                         "owned compiler helper survived synchronous Cargo completion")
                        self.assertEqual(_winapi.GetExitCodeProcess(handles[0]), 75)
                        for handle in handles[1:]:
                            self.assertEqual(_winapi.WaitForSingleObject(handle, 0), 258,
                                             "unknown/test worker was terminated")
                        self.assertIsNone(foreign.poll(), "same-image foreign process was terminated")
                        if unrelated_bash:
                            self.assertIsNone(unrelated_bash.poll(), 'unrelated same-image Bash worker was terminated')
                        self.assertEqual(build.returncode, expected, err)
                        for key in ('CC', 'CXX', 'HOST_CC', 'TARGET_CXX',
                                    'CC_x86_64_pc_windows_msvc', 'CXX_x86_64_pc_windows_msvc'):
                            value = observed['compiler_environment'][key]
                            self.assertEqual(Path(value) if value else None, installation / 'cl.exe',
                                             'Cargo did not receive the selected compiler')
                        for key in ('INCLUDE', 'LIB'):
                            self.assertEqual(observed['compiler_environment'][key], str(installation))
                        self.assertEqual(observed['compiler_environment']['CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER'],
                                         'rust-lld.exe', 'the existing Rust linker changed')
                        binding = json.loads((self.repo / 'tmp' / ('msvc-' + observed['job'] + '.binding.json')).read_text())
                        self.assertGreater(foreign_creation, binding['prepared_filetime'])
                        receipts = list((self.repo / "tmp").glob("msvc-*.completion.json"))
                        receipt = next(json.loads(p.read_text()) for p in receipts
                            if any(row['identity']['pid'] == pids[0] for row in json.loads(p.read_text())['disposals']))
                        self.assertEqual(receipt['cargo_exit'], cargo_code)
                        self.assertEqual(receipt['finalization_exit'], 75 if unknown == "1" else 0)
                        self.assertEqual(len(receipt['disposals']), 1)
                        self.assertEqual(len(receipt['pairs']), 1, 'an unselected installation gained disposal authority')
                        disposal = receipt['disposals'][0]
                        self.assertEqual(Path(disposal['matched_pair']['helper']), installation / "VCTIP.EXE")
                        self.assertEqual(disposal['status'], 'disposed_and_handle_signaled')
                        caller_path = self.repo / 'tmp' / ('msvc-' + observed['job'] + '.binding.json.caller.json')
                        caller_raw = caller_path.read_bytes()
                        caller = json.loads(caller_raw)
                        self.assertEqual(receipt['execution_caller_binding'], hashlib.sha256(caller_raw).hexdigest())
                        self.assertEqual(caller['binding'], receipt['binding'])
                        self.assertEqual(caller['job'], receipt['job'])
                        execution_caller = receipt['execution_caller']
                        self.assertEqual(execution_caller, caller['caller'])
                        self.assertTrue(execution_caller['live'] and execution_caller['member'])
                        self.assertNotIn(execution_caller['pid'], [receipt['caller']['pid'], receipt['inspector']['pid'], *pids])
                        self.assertEqual(Path(execution_caller['image']), Path(receipt['caller']['image']))
                        self.assertGreaterEqual(execution_caller['creation_filetime'], binding['prepared_filetime'])
                        self.assertIn(execution_caller, receipt['remaining'])
                        if unrelated_bash:
                            self.assertGreater(bash_creation, binding['prepared_filetime'])
                            unrelated = next(row for row in receipt['remaining'] if row['pid'] == unrelated_bash.pid)
                            self.assertEqual(unrelated['creation_filetime'], bash_creation)
                            self.assertEqual(Path(unrelated['image']), Path(execution_caller['image']))
                            self.assertTrue(unrelated['member'] and unrelated['live'])
                            self.assertNotEqual(unrelated, execution_caller)
                        observation.update(receipt=receipt, prepared_filetime=binding['prepared_filetime'], caller_record=caller)
                    finally:
                        (self.out / (run + ".held")).touch()
                        for handle in handles:
                            if _winapi.WaitForSingleObject(handle, 0) == 258:
                                _winapi.TerminateProcess(handle, 99)
                                _winapi.WaitForSingleObject(handle, 10000)
                            _winapi.CloseHandle(handle)
                        if build.poll() is None:
                            build.terminate()
                            build.wait(timeout=10)
                        if foreign is not None and foreign.poll() is None:
                            foreign.terminate()
                            foreign.wait(timeout=10)
                        if unrelated_bash is not None:
                            if unrelated_bash.poll() is None:
                                unrelated_bash.terminate()
                                unrelated_bash.wait(timeout=10)
                            unrelated_bash.stdin.close()
            # An explicit finalizer selection alone must not allow Cargo's higher
            # precedence target selector to choose another installed compiler.
            self.stub('cargo', 'echo started > "$OUT/conflict.started"')
            with self.subTest(run='conflicting-selection'):
                build = self.start('conflict', stderr=subprocess.PIPE,
                    NRESE_MSVC_COMPILER=str(installation / 'cl.exe'), INCLUDE=str(installation), LIB=str(installation),
                    CC_x86_64_pc_windows_msvc=str(outsider / 'cl.exe'))
                _, err = build.communicate(timeout=60)
                observations.append({'case': 'conflicting-selection', 'code': build.returncode,
                    'cargo_started': (self.out / 'conflict.started').exists()})
                self.assertEqual(build.returncode, 75, 'conflicting compiler selection was admitted: ' + err)
                self.assertFalse((self.out / 'conflict.started').exists(), 'Cargo ran with ambiguous compiler ownership')
        finally:
            (self.out / "compiler-completion.json").write_text(json.dumps(observations, indent=2))
            preserved = self.out / 'finalizer-records'
            preserved.mkdir(exist_ok=True)
            for record in (self.repo / 'tmp').glob('msvc-*'):
                shutil.copy2(record, preserved / record.name)
            if os.environ.get("FLEET_OUT"):
                shutil.copytree(self.out, Path(os.environ["FLEET_OUT"]) / ("compiler-fixture-" + self.root.name), dirs_exist_ok=True)

    def test_compilation_authority_respects_option_values_and_test_argument_separator(self):
        # Spec: root's second review (23 Oct 2026): only an actual compilation-only
        # command grants disposal; a flag-looking value/test argument does not.
        import runpy
        classify = runpy.run_path(str(self.repo / 'scripts/lib/msvc-finalize.py'))['compilation_only']
        for args, expected in ((['build'], True), (['test', '--no-run'], True),
                (['test', '--', '--no-run'], False), (['test', '--test', '--no-run'], False),
                (['test'], False), (['nextest', 'run', '--no-run'], False),
                (['nextest', 'list', '--list-type', 'binaries-only'], True),
                (['nextest', 'list', '-E', '--list-type=binaries-only'], False),
                (['nextest', 'list', '--', '--list-type=binaries-only'], False),
                (['nextest', 'list', '--list-type=full'], False),
                (['nextest', 'list', '--unknown', '--list-type=binaries-only'], False)):
            with self.subTest(args=args):
                self.assertEqual(classify(args), expected)

    @unittest.skipUnless(os.name == "nt", "native build/test ownership uses Windows JobObjects")
    def test_build_finalizes_before_tests_and_test_helpers_have_no_disposal_authority(self):
        # Spec: root's second review (23 Oct 2026): Windows tests consume the exact
        # binaries-only build after its helper has stopped; a selected-image helper
        # born during mixed Cargo/test execution stays alive and refuses completion.
        import _winapi
        installation = self.root / 'compiler'
        installation.mkdir()
        for name in ('cl.exe', 'VCTIP.EXE'):
            shutil.copyfile(sys.executable, installation / name)
        for dll in Path(sys.base_prefix).glob('python3*.dll'):
            shutil.copyfile(dll, installation / dll.name)
            (installation / (dll.stem + '._pth')).write_text(str(Path(sys.base_prefix) / 'Lib') + '\n')
        controller = self.root / 'phase-cargo.py'
        controller.write_text('''import json,os,subprocess,sys,time
from pathlib import Path
args=sys.argv[1:]; phase='build' if args[:2]==['nextest','list'] else 'test'
if args[:1]==['metadata']:
    assert args==['metadata','--locked','--format-version','1']
    print('{}');sys.exit(0)
out=Path(os.environ['OUT']); prefix=os.environ['RUN']+'.'+phase
if '--binaries-metadata' in args:
    metadata=json.loads(Path(args[args.index('--binaries-metadata')+1]).read_text())
    assert metadata['phase']=='build' and metadata['run']==os.environ['RUN']
child=subprocess.Popen([str(Path(os.environ['NRESE_MSVC_COMPILER']).with_name('VCTIP.EXE')),
    '-I','-c','import time;time.sleep(120)'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
row={'pid':child.pid,'phase':phase,'run':os.environ['RUN'],'argv':args,'job':os.environ['NRESE_BUILD_JOB']}
ready=out/(prefix+'.ready'); temporary=ready.with_suffix('.tmp')
temporary.write_text(json.dumps(row));temporary.replace(ready)
deadline=time.monotonic()+45
while not (out/(prefix+'.held')).exists():
    if time.monotonic()>deadline:sys.exit(98)
    time.sleep(.05)
if phase=='build':print(json.dumps(row))
sys.exit(0 if phase=='build' else int(os.environ['CODE']))
''')
        self.stub('cargo', 'exec python "$CONTROLLER" "$@"')
        check = (self.repo / 'scripts/check.sh').read_text()
        seam = check[check.index('windows_tests() {'):check.index('windows_part() {')]
        wrapper = self.repo / 'windows-test-seam.sh'
        wrapper.write_text('guarded=scripts/cargo-guarded.sh\n' +
            'package_args() { local name; for name in "$@"; do printf -- "-p %s " "$name"; done; }\n' +
            seam + '\nwindows_tests "$@"\n')
        observations = []
        try:
            for label, args, code in (('cargo-test', ['test', '--locked'], 0),
                    ('nextest-run', ['nextest', 'run', '--locked'], 19),
                    ('windows-split', None, 0)):
                with self.subTest(run=label):
                    handles = []
                    command = [BASH, 'scripts/cargo-guarded.sh', *args] if args else [
                        BASH, str(wrapper), 'binary_id(chosen)', 'chosen']
                    proc = subprocess.Popen(command, cwd=self.repo, env=self.env(label,
                        CONTROLLER=str(controller), CODE=str(code),
                        NRESE_MSVC_COMPILER=str(installation / 'cl.exe'),
                        INCLUDE=str(installation), LIB=str(installation), CC=None, CXX=None),
                        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
                    rows = []
                    try:
                        for phase in (('build', 'test') if args is None else ('test',)):
                            ready = self.out / (label + '.' + phase + '.ready')
                            deadline = time.monotonic() + 60
                            while not ready.exists() and proc.poll() is None and time.monotonic() < deadline:
                                time.sleep(.05)
                            self.assertTrue(ready.exists(), 'expected build/test phase did not start')
                            row = json.loads(ready.read_text()); rows.append(row)
                            handle = _winapi.OpenProcess(0x100000 | 0x1000 | 1, False, row['pid'])
                            handles.append(handle)
                            self.assertEqual(_winapi.WaitForSingleObject(handle, 0), 258)
                            if phase == 'test' and args is None:
                                self.assertEqual(_winapi.WaitForSingleObject(handles[0], 0), 0,
                                                 'compiler helper survived into test execution')
                                self.assertEqual(_winapi.GetExitCodeProcess(handles[0]), 75)
                                self.assertEqual(row['argv'], ['nextest', 'run', '--binaries-metadata',
                                    row['argv'][3], '--cargo-metadata', row['argv'][5],
                                    '--no-fail-fast', '-E', 'binary_id(chosen)'])
                                self.assertEqual(rows[0]['argv'], ['nextest', 'list', '--locked', '-p', 'chosen',
                                    '--cargo-metadata', row['argv'][5], '--list-type', 'binaries-only', '--message-format', 'json'])
                            (self.out / (label + '.' + phase + '.held')).touch()
                        _, err = proc.communicate(timeout=60)
                        (self.out / (label + '.log')).write_text(err)
                        self.assertEqual(_winapi.WaitForSingleObject(handles[-1], 0), 258,
                                         'test-created selected-image worker was terminated')
                        self.assertEqual(proc.returncode, code or 75, err)
                        receipts = [json.loads((self.repo / 'tmp' / ('msvc-' + row['job'] + '.completion.json')).read_text())
                                    for row in rows]
                        self.assertFalse(receipts[-1]['compilation_only'])
                        self.assertEqual(receipts[-1]['disposals'], [])
                        self.assertEqual(receipts[-1]['cargo_exit'], code)
                        self.assertEqual(receipts[-1]['finalization_exit'], 75)
                        if args is None:
                            self.assertTrue(receipts[0]['compilation_only'])
                            self.assertEqual(receipts[0]['finalization_exit'], 0)
                        observations.append({'case': label, 'rows': rows, 'receipts': receipts,
                                             'test_helper_alive': True, 'exit': proc.returncode})
                    finally:
                        for phase in ('build', 'test'):
                            (self.out / (label + '.' + phase + '.held')).touch()
                        for handle in handles:
                            if _winapi.WaitForSingleObject(handle, 0) == 258:
                                _winapi.TerminateProcess(handle, 99)
                                _winapi.WaitForSingleObject(handle, 10000)
                            _winapi.CloseHandle(handle)
                        if proc.poll() is None:
                            proc.terminate(); proc.wait(timeout=10)
        finally:
            (self.out / 'build-test-completion.json').write_text(json.dumps(observations, indent=2))
            if os.environ.get('FLEET_OUT'):
                shutil.copytree(self.out, Path(os.environ['FLEET_OUT']) / ('build-test-fixture-' + self.root.name), dirs_exist_ok=True)

    def test_a_build_that_fits_above_the_floor_runs_though_the_machine_is_busy(self):
        # The orchestrator, 20 Oct 2026: BUSY stayed set for hours under the agents' own load. 12 GB
        # free, the default 8 GB floor: room for a 3 GB peak, not for a 5 GB one.
        (self.guard / "BUSY").write_text("12.0 GB RAM, 394 GB disk free")
        self.free(12)
        floor = {"NRESE_BUILD_FLOOR_GB": None, "NRESE_GUARD_THRESHOLDS": str(self.root / "no-thresholds")}
        fits = self.start("a", NRESE_BUILD_PEAK_GB="3", **floor)
        self.assertIsNotNone(self.started("a", within=60), "held by BUSY though it fits")
        self.assertEqual(fits.wait(timeout=120), 0)
        waiting = self.start("b", NRESE_BUILD_PEAK_GB="5", NRESE_BUILD_SLOT_WAIT_S="15", **floor)
        self.assertEqual(waiting.wait(timeout=120), 75)  # ends by itself: no process left in the directory
        self.assertFalse((self.out / "b.started").exists())

    def test_no_run_starts_below_the_guards_disk_line_busy_or_not(self):
        # Sol, review-build-admission-r5 (r5-1): ignoring BUSY ignored the guard's disk kill line (100 GB
        # on C:). Memory fits; C: has 90 GB free: no build; at 400 GB the same run builds.
        (self.guard / "BUSY").write_text("24.0 GB RAM, 90 GB disk free")
        self.disk(90)
        floor = {"NRESE_BUILD_FLOOR_GB": None, "NRESE_GUARD_THRESHOLDS": str(self.root / "no-thresholds")}
        low = self.start("low", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="1", NRESE_BUILD_SLOT_WAIT_S="15", **floor)
        _, err = low.communicate(timeout=60)
        self.assertEqual(low.returncode, 75, err)
        self.assertIn("below its kill line of 100 GB", err)
        self.assertFalse((self.out / "low.started").exists())
        self.disk(400)
        fits = self.start("fits", NRESE_BUILD_PEAK_GB="1", **floor)
        self.assertEqual(fits.wait(timeout=120), 0)
        self.assertIsNotNone(self.started("fits", within=1))

    @unittest.skipUnless(shutil.which("mkfifo"), "a disk reading that never comes (a FIFO)")
    def test_a_disk_reading_that_never_comes_ends_at_the_deadline_and_holds_no_one(self):
        # r6-2: the disk query ran unbounded inside the admission lock: a stalled one held its hook and
        # every hook behind it. A FIFO no one writes stands for it.
        fifo = self.root / "stalled"
        subprocess.run(["mkfifo", str(fifo)], check=True)
        stalled = self.start("stalled", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="1", NRESE_BUILD_SLOT_WAIT_S="3",
                             NRESE_GUARD_DISKFREE=str(fifo), NRESE_GUARD_DISK_TIMEOUT_S="1")
        beside = self.start("beside", NRESE_BUILD_PEAK_GB="1")
        _, err = stalled.communicate(timeout=60)
        self.assertEqual(stalled.returncode, 75, err)
        self.assertFalse((self.out / "stalled.started").exists())
        self.assertEqual(beside.wait(timeout=120), 0)
        self.assertIsNotNone(self.started("beside", within=1), "held up by a stalled disk reading")

    def thresholds(self, text):
        path = self.root / "thresholds"
        path.write_text(text)
        return {"NRESE_BUILD_FLOOR_GB": None, "NRESE_GUARD_THRESHOLDS": str(path)}

    @unittest.skipUnless(shutil.which("mkfifo"), "disk readings that keep running out (a FIFO)")
    def test_disk_readings_slower_than_a_poll_end_at_the_deadline(self):
        # Sol, review-build-admission-r8 (r8-1): a reading older than a poll was retried before the
        # deadline was looked at, so readings that kept taking longer than a poll retried forever. Each
        # here takes its 3 s timeout (a FIFO no one writes), the poll is 1 s, the limit 2 s.
        fifo = self.root / "slow"
        subprocess.run(["mkfifo", str(fifo)], check=True)
        run = self.start("slow", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="1", NRESE_BUILD_SLOT_WAIT_S="2",
                         NRESE_BUILD_SLOT_POLL_S="1", NRESE_GUARD_DISKFREE=str(fifo), NRESE_GUARD_DISK_TIMEOUT_S="3")
        _, err = run.communicate(timeout=90)
        self.assertEqual(run.returncode, 75, err)
        self.assertFalse((self.out / "slow.started").exists())

    @unittest.skipUnless(shutil.which("mkfifo"), "a thresholds file that never answers (a FIFO)")
    def test_a_thresholds_file_that_never_answers_is_no_file(self):
        # r8-2: the thresholds read had no bound; a stalled one held the run before it ever queued.
        fifo = self.root / "thresholds-stalled"
        subprocess.run(["mkfifo", str(fifo)], check=True)
        run = self.start("t", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="1", NRESE_BUILD_FLOOR_GB=None,
                         NRESE_GUARD_THRESHOLDS=str(fifo), NRESE_GUARD_THRESHOLDS_TIMEOUT_S="1")
        _, err = run.communicate(timeout=90)
        self.assertEqual(run.returncode, 0, err)
        self.assertIsNotNone(self.started("t", within=1), "today's lines, so it fits")

    def test_the_floor_and_the_disk_line_come_from_the_guards_thresholds_file(self):
        # The orchestrator, IB0612-8d33: the guard writes its lines; admission never reads its
        # PowerShell. A 6 GB kill line: a 10 GB floor, 13 GB free; a 200 GB disk line: 210 GB needed.
        self.free(13)
        floor = self.thresholds("ram_soft_gb=16 disk_soft_gb=150 ram_kill_gb=6 disk_kill_gb=200\n")
        fits = self.start("a", NRESE_BUILD_PEAK_GB="3", **floor)
        self.assertEqual(fits.wait(timeout=120), 0)
        self.assertIsNotNone(self.started("a", within=1), "3 GB above a 10 GB floor with 13 GB free")
        waiting = self.start("b", NRESE_BUILD_PEAK_GB="4", NRESE_BUILD_SLOT_WAIT_S="15", **floor)
        self.assertEqual(waiting.wait(timeout=120), 75)  # ends by itself: no process left in the directory
        self.assertFalse((self.out / "b.started").exists(), "4 GB above a 10 GB floor with 13 GB free")
        self.disk(205)
        low = self.start("c", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="1", NRESE_BUILD_SLOT_WAIT_S="15", **floor)
        _, err = low.communicate(timeout=60)
        self.assertEqual(low.returncode, 75, err)
        self.assertIn("below its kill line of 200 GB", err)

    def test_an_unreadable_thresholds_file_takes_todays_lines_and_says_so(self):
        # Anything but the guard's one line of four numbers is no file: an 8 GB floor, and the wait
        # names the file. 11 GB free: a 4 GB peak doesn't fit.
        self.free(11)
        floor = self.thresholds("ram_kill_gb=12\n")
        run = self.start("a", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="4", NRESE_BUILD_SLOT_WAIT_S="15", **floor)
        _, err = run.communicate(timeout=60)
        self.assertEqual(run.returncode, 75, err)
        self.assertIn("above the 8 GB floor", err)
        self.assertIn("thresholds file", err)
        self.assertIn("missing or unreadable", err)

    def test_a_lock_held_past_the_deadline_admits_nothing_and_ends_at_the_deadline(self):
        # Sol, review-build-admission-r7 (r7-2): the first acquisition waited for the lock without a
        # deadline, then admitted on a disk reading taken before the wait. Another live process holds
        # the lock for 8 s while C: falls from 110 GB to 99; the run (1 s limit) must end before then.
        self.disk(110)
        holder = subprocess.Popen([BASH, "-c", 'read -r -a s < /proc/$$/stat; echo "$$@${s[21]}"; for i in $(seq 60); do sleep 1; done'],
                                  stdout=subprocess.PIPE, text=True)
        self.addCleanup(lambda: (holder.kill(), holder.wait()))
        identity = holder.stdout.readline().strip()
        lock = self.root / "slots" / ".lock"
        lock.mkdir(parents=True)
        (lock / "holder").write_text(identity + "\n")
        run = self.start("a", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="1", NRESE_BUILD_SLOT_WAIT_S="1",
                         NRESE_GUARD_DISK_TIMEOUT_S="1", NRESE_GUARD_THRESHOLDS=str(self.root / "no-thresholds"),
                         NRESE_BUILD_FLOOR_GB=None)
        # Once it has queued (a loaded machine takes seconds to get there), C: falls; it must then end
        # within its deadline while the lock is still held.
        deadline = time.time() + 60
        while not list((self.root / "slots" / "queue").glob("*")) and run.poll() is None and time.time() < deadline:
            time.sleep(0.1)
        self.disk(99)
        deadline = time.time() + 30
        while run.poll() is None and time.time() < deadline:
            time.sleep(0.2)
        ended_while_locked = run.poll() is not None
        shutil.rmtree(lock)
        _, err = run.communicate(timeout=60)
        self.assertTrue(ended_while_locked, "it waited for the lock past its deadline")
        self.assertEqual(run.returncode, 75, err)
        self.assertFalse((self.out / "a.started").exists(), "admitted below the guard's disk line")

    def test_room_that_appears_after_the_deadline_admits_nothing(self):
        # Sol, r5 follow-up: a sleep running past the deadline, then room: the run must end, not build.
        self.free(10)
        run = self.start("late", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="2", NRESE_BUILD_SLOT_WAIT_S="2",
                         NRESE_BUILD_SLOT_POLL_S="5")
        # Its wait starts with its ticket (a loaded machine takes seconds to get there); the room
        # comes a second after its 2 s deadline, before its 5 s sleep would have ended.
        deadline = time.time() + 60
        while not list((self.root / "slots" / "queue").glob("*")) and time.time() < deadline:
            time.sleep(0.1)
        time.sleep(3)
        self.free(64)
        _, err = run.communicate(timeout=60)
        self.assertEqual(run.returncode, 75, err)
        self.assertFalse((self.out / "late.started").exists())

    def test_a_run_that_cannot_start_holds_up_no_one_behind_it_who_can(self):
        # 20 Oct 2026 (16:52, 18:11): a run that couldn't start headed the line and stalled every
        # hook behind it. 24 GB free, a 16 GB floor: 8 GB of room; the first asks 12 GB, the next 2.
        self.start("big", NRESE_BUILD_PEAK_GB="12")
        deadline = time.time() + 30
        while not list((self.root / "slots" / "queue").glob("*")) and time.time() < deadline:
            time.sleep(0.2)
        small = self.start("small", NRESE_BUILD_PEAK_GB="2")
        self.assertIsNotNone(self.started("small", within=60), "held up by a run that can't start")
        self.assertEqual(small.wait(timeout=120), 0)
        self.assertFalse((self.out / "big.started").exists())

    def test_without_the_guards_thresholds_file_the_floor_is_8_gb(self):
        self.free(12)
        floor = {"NRESE_BUILD_FLOOR_GB": None, "NRESE_GUARD_THRESHOLDS": str(self.root / "no-thresholds")}
        fits = self.start("a", NRESE_BUILD_PEAK_GB="4", **floor)
        self.assertIsNotNone(self.started("a", within=60))
        self.assertEqual(fits.wait(timeout=120), 0)
        waiting = self.start("b", NRESE_BUILD_PEAK_GB="5", NRESE_BUILD_SLOT_WAIT_S="15", **floor)
        self.assertEqual(waiting.wait(timeout=120), 75)  # ends by itself: no process left in the directory
        self.assertFalse((self.out / "b.started").exists())

    def test_a_build_runs_with_the_jobs_memory_allows_and_the_next_waits_for_its_peak(self):
        # 24 GB free, a 16 GB floor: 8 GB of room; 12 jobs would peak at 9.2 GB, 10 at 8.0 GB.
        first = self.start("a", SLEEP="6")
        self.assertEqual(self.started("a", within=60)[1], "10")
        second = self.start("b")
        self.assertEqual(first.wait(timeout=120), 0)
        self.assertEqual(second.wait(timeout=120), 0)
        self.assertGreater(int(self.started("b", within=1)[0]), self.ended("a"), "b ran beside a's declared peak")
        # Room for fewer than 2 jobs: no build, however long it waits.
        self.free(18)
        waiting = self.start("c", NRESE_BUILD_SLOT_WAIT_S="15")
        self.assertIsNone(self.started("c", within=8))
        # Let the waiter and its children finish before Windows removes their cwd.
        self.assertEqual(waiting.wait(timeout=120), 75)
        self.assertFalse((self.out / "c.started").exists())

    def test_no_more_runs_than_slots_however_much_memory_is_free(self):
        self.free(64)
        first = self.start("a", SLEEP="4", NRESE_BUILD_SLOTS="1", NRESE_BUILD_PEAK_GB="1")
        self.assertIsNotNone(self.started("a", within=60))
        second = self.start("b", NRESE_BUILD_SLOTS="1", NRESE_BUILD_PEAK_GB="1")
        self.assertEqual(first.wait(timeout=120), 0)
        self.assertEqual(second.wait(timeout=120), 0)
        self.assertGreater(int(self.started("b", within=1)[0]), self.ended("a"))

    def test_a_request_that_can_never_fit_is_refused_at_once_and_holds_no_one_up(self):
        never = self.start("a", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="100")
        _, err = never.communicate(timeout=60)
        self.assertEqual(never.returncode, 2)
        self.assertIn("more than this machine can ever give it", err)
        follower = self.start("b", NRESE_BUILD_PEAK_GB="1")
        self.assertIsNotNone(self.started("b", within=60))
        self.assertEqual(follower.wait(timeout=120), 0)

    def test_a_holder_that_is_gone_is_reclaimed_even_when_its_pid_lives_on(self):
        # A live process whose pid a crashed holder had: its identity (pid and start time) differs.
        sleeper = subprocess.Popen([BASH, "-c", "sleep 120 & echo $!; wait"], stdout=subprocess.PIPE, text=True)
        self.addCleanup(lambda: (sleeper.kill(), sleeper.wait()))
        pid = sleeper.stdout.readline().strip()
        self.addCleanup(lambda: subprocess.run([BASH, "-c", f"kill {pid} 2> /dev/null"]))
        # The holder named by its pid alone (before identities: kill -0 says it lives) and by its pid
        # with another start time; and one whose pid is gone.
        for holder in (pid, f"{pid}@1", "99999999@1"):
            stale = self.root / "slots" / "run" / holder
            stale.mkdir(parents=True)
            (stale / "peak").write_text("999999")
        build = self.start("a")
        self.assertIsNotNone(self.started("a", within=60))
        self.assertEqual(build.wait(timeout=120), 0)
        self.assertEqual(list((self.root / "slots" / "run").iterdir()), [])

    def test_only_a_completed_stop_of_its_own_build_counts_and_a_late_one_is_waited_for(self):
        for notice, code in (("late", 75), ("other", 127), ("failed", 127), ("intent", 127)):
            (self.guard / "BUSY").unlink(missing_ok=True)
            (self.guard / "killed.log").unlink(missing_ok=True)
            build = self.start(f"said-{notice}", stderr=subprocess.PIPE, NOTICE=notice, CODE="127",
                               # 10 s: the late stop comes after 1 s, but a loaded Git Bash took
                               # over 4 s to show it (20 Oct 2026, full suite, 2 of 3 runs).
                               NRESE_GUARD_NOTICE_WAIT_S="10")
            _, err = build.communicate(timeout=120)
            self.assertEqual(build.returncode, code, (notice, err))
            self.assertEqual("stopped by the PC guard" in err, code == 75, (notice, err))

    def test_a_completion_seen_after_the_deadline_does_not_count(self):
        # Sol, round 3: a turn of the wait that runs late (sleep 1 taking 4 s) must not stretch a
        # 2 s deadline to take in a completion published at 3 s.
        self.stub("sleep", '[ "$1" = 1 ] && exec /usr/bin/sleep 4; exec /usr/bin/sleep "$@"')
        build = self.start("said-after", stderr=subprocess.PIPE, NOTICE="after", CODE="127",
                           NRESE_GUARD_NOTICE_WAIT_S="2")
        _, err = build.communicate(timeout=120)
        self.assertEqual(build.returncode, 127, err)
        self.assertNotIn("stopped by the PC guard", err)
        self.late_writer_done()

    def test_even_the_first_look_at_the_notices_counts_only_before_the_deadline(self):
        # Sol, round 4: the first scan of the guard's notices taking 4 s (an awk that is slow only
        # for them) must not take in a completion published at 3 s against a 2 s deadline.
        self.stub("awk", 'case "$*" in *"-v s="*) /bin/sleep 4 ;; esac\nexec /usr/bin/awk "$@"')
        build = self.start("said-after", stderr=subprocess.PIPE, NOTICE="after", CODE="127",
                           NRESE_GUARD_NOTICE_WAIT_S="2")
        _, err = build.communicate(timeout=120)
        self.assertEqual(build.returncode, 127, err)
        self.assertNotIn("stopped by the PC guard", err)
        self.late_writer_done()

    @unittest.skipUnless(shutil.which("powershell"), "the memory cap's helper is Windows'")
    def test_a_broken_cap_helper_is_built_again_and_a_cap_never_silently_dropped(self):
        script = self.repo / "scripts/lib/memory-cap.ps1"
        # A copy of the script of its own (a comment of its own): its own cache entry, never the
        # one every build here uses.
        script.write_text(script.read_text() + f"\n# test {self.root.name}\n")
        digest = hashlib.sha256(script.read_bytes()).hexdigest()[:12]
        cached = Path(tempfile.gettempdir()) / "nrese-memory-cap" / f"memory-cap-{digest}.exe"
        cached.parent.mkdir(exist_ok=True)
        self.addCleanup(lambda: cached.unlink(missing_ok=True))
        cached.write_bytes(b"not a program")
        build = self.start("corrupt", stderr=subprocess.PIPE, NRESE_MEMORY_CAP_GB="32")
        _, err = build.communicate(timeout=300)
        self.assertEqual(build.returncode, 0, err)
        self.assertIsNotNone(self.started("corrupt", within=1), "no build after the helper was rebuilt")
        self.assertNotEqual(cached.read_bytes(), b"not a program", "the broken helper stayed")
        # A helper that can't be built: with a cap asked for, no build.
        script.write_text(script.read_text() + "\n} this script does not parse {\n")
        build = self.start("uncapped", stderr=subprocess.PIPE, NRESE_MEMORY_CAP_GB="32")
        _, err = build.communicate(timeout=300)
        self.assertEqual(build.returncode, 1, err)
        self.assertIn("memory cap could not be applied (scripts/lib/memory-cap.ps1): not building", err)
        self.assertIsNone(self.started("uncapped", within=1))

    def test_a_wait_without_a_terminal_ends_failing_with_its_reason_and_nothing_built(self):
        # A hook has no terminal and no one to interrupt it: it fails, it never hangs or builds.
        build = self.start("a", stderr=subprocess.PIPE, NRESE_BUILD_PEAK_GB="12", NRESE_BUILD_SLOT_WAIT_S="15")
        _, err = build.communicate(timeout=60)
        self.assertEqual(build.returncode, 75, err)
        self.assertIn("not built: no room to build after", err)
        self.assertIn("it may peak at 12 GB", err)
        self.assertFalse((self.out / "a.started").exists())
        self.assertEqual(list((self.root / "slots" / "queue").glob("*")), [])


if __name__ == "__main__":
    unittest.main()
