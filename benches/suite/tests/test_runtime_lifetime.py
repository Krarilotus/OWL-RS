"""Local scope-contract fixtures: mock systemd, never launch native workloads."""
import importlib.util
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from suitekit import runtime


def load_fast():
    """benches/fast/fast.py as a module of its own, as test_fast_timeouts loads it."""
    spec = importlib.util.spec_from_file_location(
        "fast_lifetime_contract", Path(__file__).resolve().parents[2] / "fast" / "fast.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ScopeLifetime(unittest.TestCase):
    def test_unbounded_existing_prefix_is_unchanged(self):
        self.assertEqual(runtime.scope_argv("case-1", "6g", {"FLEET_SLICE": "fleet-example.slice"}),
            ["systemd-run", "--user", "--scope", "-q", "--slice=fleet-example.slice", "--unit=case-1",
             "-p", "MemoryMax=6G", "-p", "MemorySwapMax=0", "-p", "OOMPolicy=continue", "--"])

    def test_rules_interval_reserves_shutdown_without_cutting_engine_allowance(self):
        argv = runtime.scope_argv("rules-block-1", "14g", {"FLEET_SLICE": "fleet-example.slice"},
                                  interval_s=75 * 60, stop_s=30)
        self.assertIn("RuntimeMaxSec=4380", argv)
        self.assertIn("TimeoutStopSec=30", argv)
        self.assertIn("KillMode=control-group", argv)
        self.assertIn("SendSIGKILL=yes", argv)
        self.assertEqual(argv[-1], "--")

    def test_worker_binds_to_exact_parent_with_its_own_stop_allowance(self):
        argv = runtime.scope_argv("worker-2", "12g", {"FLEET_SLICE": "fleet-example.slice",
            "SUITE_SCOPE_PARENT": "dictionary-block-1", "SUITE_SCOPE_STOP_S": "7"})
        for value in ("BindsTo=dictionary-block-1.scope", "After=dictionary-block-1.scope",
                      "TimeoutStopSec=7", "MemoryMax=12G", "MemorySwapMax=0"):
            self.assertIn(value, argv)
        self.assertFalse(any("RuntimeMaxSec=" in value for value in argv), "worker must not renew the block deadline")

    def test_bad_or_recursive_settings_fail_before_launch(self):
        for interval, stop in ((120, 30), (0, 1), (float("inf"), 1), (float("nan"), 1),
                               (300, 0), (300, -1), (300, float("inf"))):
            with self.subTest(interval=interval, stop=stop), self.assertRaises(ValueError):
                runtime.scope_argv("outer", "1g", {}, interval_s=interval, stop_s=stop)
        for parent in ("worker", "../other", "other.scope/escape"):
            with self.subTest(parent=parent), self.assertRaises(ValueError):
                runtime.scope_argv("worker", "1g", {"SUITE_SCOPE_PARENT": parent})
        with self.assertRaises(ValueError):
            runtime.scope_argv("other", "1g", {"SUITE_SCOPE_PARENT": "outer"}, interval_s=300)

    def test_entry_prefixes_whole_existing_launcher_and_preserves_inherited_lease(self):
        descriptor = os.open(__file__, os.O_RDONLY)
        self.addCleanup(os.close, descriptor)
        os.set_inheritable(descriptor, True)
        class Executed(BaseException):
            pass
        captured = {}
        def execute(program, argv, environ):
            captured.update(program=program, argv=argv, environ=dict(environ))
            self.assertTrue(os.get_inheritable(descriptor), "entry dropped an existing inheritable lease")
            raise Executed()
        with patch.dict(os.environ, {"FLEET_SLICE": "fleet-example.slice"}, clear=True), \
                patch.object(sys, "argv", [str(runtime.__file__), "rules-block-1", "14g", "4500", "30",
                                          "bash", "existing-launch.sh", "block", "1"]), \
                patch.object(os, "execvpe", side_effect=execute):
            with self.assertRaises(Executed):
                runpy.run_path(runtime.__file__, run_name="__main__")
        self.assertEqual(captured["argv"][-4:], ["bash", "existing-launch.sh", "block", "1"])
        self.assertEqual(captured["environ"]["SUITE_SCOPE_PARENT"], "rules-block-1")
        self.assertEqual(captured["environ"]["SUITE_SCOPE_STOP_S"], "30")

    def test_entry_requires_fleet_and_refuses_nested_deadline(self):
        for environ in ({}, {"FLEET_SLICE": "fleet-example.slice", "SUITE_SCOPE_PARENT": "old"}):
            with patch.dict(os.environ, environ, clear=True), \
                    patch.object(sys, "argv", [str(runtime.__file__), "outer", "1g", "300", "2", "sh"]), \
                    patch.object(os, "execvpe") as execute:
                with self.assertRaises(ValueError):
                    runpy.run_path(runtime.__file__, run_name="__main__")
                execute.assert_not_called()

    def test_fast_process_forwards_only_explicit_leases_and_its_case_limit(self):
        """fast.py's own `process`, its boundaries (scope, paths, the run) replaced. The limit's
        value is test_fast_timeouts' (default, declared, unlimited); here, that it reaches the run."""
        fast = load_fast()
        observed, argv = [], []
        completed = types.SimpleNamespace(returncode=0, stdout="", stderr="")

        def run(command, **kwargs):
            argv.append(command)
            observed.append(kwargs)
            return completed

        with tempfile.TemporaryDirectory() as temporary, \
                patch.multiple(fast, native_dirs=lambda: {}, FAST_DATA=Path(temporary),
                               scope_argv=lambda *_: ["scope", "--"], native=lambda command, _: command,
                               end_scope=lambda *_: None, pinned_cpus=lambda: None,
                               AnonKernelSampler=lambda where: types.SimpleNamespace(start=lambda: None,
                                                                                     stop=lambda: None),
                               subprocess=types.SimpleNamespace(**{**vars(subprocess), "run": run})):
            for descriptors in ("", "9 14"):
                with patch.dict(os.environ, {"SUITE_LEASE_FDS": descriptors}):
                    self.assertEqual(fast.process("case", 6, ["original-command"])[0], 0)
                self.assertEqual(observed[-1]["pass_fds"], tuple(map(int, descriptors.split())))
                self.assertEqual(observed[-1]["timeout"], fast.CASE_TIMEOUT_S)
                # Every process an environment pad of its own size (its initial stack moves).
                self.assertLess(len(observed[-1]["env"]["NRESE_LAYOUT_PAD"]), 4096)
            # Pinned (hosts.toml `cpus`): the command, inside its scope, on those CPUs alone.
            with patch.object(fast, "pinned_cpus", lambda: "8-15,24-31"):
                fast.process("case", 6, ["original-command"])
            self.assertEqual(argv[-1][:5], ["scope", "--", "taskset", "-c", "8-15,24-31"])

    def test_populated_owned_cgroup_prevents_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            group = base / "owned"
            group.mkdir()
            (group / "cgroup.events").write_text("populated 1\nfrozen 0\n")
            calls = []
            def systemctl(command, timeout):
                calls.append(command)
                stdout = "/owned\n" if "ControlGroup" in command else "LoadState=loaded\nActiveState=failed\n"
                return types.SimpleNamespace(returncode=0, stdout=stdout, stderr="")
            with patch.object(runtime, "run_quiet", side_effect=systemctl), \
                    patch.object(runtime, "Path", return_value=base):
                for _ in range(2):
                    with self.assertRaisesRegex(RuntimeError, "still populated"):
                        runtime.end_scope("owned")
                (group / "cgroup.events").write_text("populated 0\nfrozen 0\n")
                runtime.end_scope("owned")
            self.assertTrue(all(command[-1] == "owned.scope" for command in calls))

    def test_release_refuses_missing_events_or_unverified_ownership(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            (base / "owned").mkdir()
            reply = lambda argv, _: types.SimpleNamespace(returncode=0,
                stdout="/owned\n" if "ControlGroup" in argv else "LoadState=loaded\nActiveState=inactive\n", stderr="")
            with patch.object(runtime, "run_quiet", side_effect=reply), patch.object(runtime, "Path", return_value=base):
                with self.assertRaises(FileNotFoundError):
                    runtime.end_scope("owned")
        for group, code in (("/../unowned", 0), ("relative", 0), ("", 1)):
            with patch.object(runtime, "run_quiet", return_value=types.SimpleNamespace(
                    returncode=code, stdout=group, stderr="probe failed")) as call:
                with self.assertRaisesRegex(RuntimeError, "ownership unverified"):
                    runtime.end_scope("owned")
                self.assertEqual(call.call_count, 1)

    def test_unrelated_scope_is_never_stopped(self):
        replies = [types.SimpleNamespace(returncode=0, stdout="", stderr=""),
                   types.SimpleNamespace(returncode=1, stdout="", stderr="gone"),
                   types.SimpleNamespace(returncode=0, stdout="LoadState=not-found\nActiveState=inactive\n", stderr="")]
        with patch.object(runtime, "run_quiet", side_effect=replies) as run:
            runtime.end_scope("exact-owned")
        self.assertTrue(all(call.args[0][-1] == "exact-owned.scope" for call in run.call_args_list))


class BoundContainer(unittest.TestCase):
    """Spec: accepted ERA R1/R2/R3: one immutable launch, agent-only reads/effects,
    exact identity and caps, explicit closure, primary and cleanup errors retained.
    This executable Python agent fixture proves the consumer, not Docker containment.
    """

    AGENT = '''import json, sys, time
from pathlib import Path
r = json.load(sys.stdin)
p = Path(r['fleet_dir'])
mode = (p / 'mode').read_text()
phase, read = r['phase'], r.get('read')
with (p / 'operations.jsonl').open('a') as f:
    f.write(json.dumps(r) + '\\n')
a = dict(owner=r['owner'], attempt=r['attempt'], phase=phase, ack=True,
         daemon_id='daemon-one', container_id='a'*64, returncode=0, stdout='', stderr='')
if phase == 'launch':
    try:
        with (p / 'used').open('x') as f: f.write(json.dumps(r['argv']))
        a['stdout'] = 'a'*64 + '\\n'
        if mode == 'pending': a.update(ack=False, error='launch UNKNOWN')
    except FileExistsError: a.update(ack=False, error='grant consumed')
elif phase == 'read':
    a['read'] = read
    if read == 'inspect':
        i = dict(Id='a'*64, Name='/bound-one', Config={'Labels':{'fleet.owner':r['owner']}},
                 HostConfig=dict(Memory=12*1024**3, MemorySwap=12*1024**3,
                                 NanoCpus=1000000000, CpusetCpus='11'))
        if mode == 'wrong-cpu': i['HostConfig']['CpusetCpus']='12'
        a['stdout'] = json.dumps([i])
    elif read == 'stats': a['stdout']='2MiB / 12GiB\\n'
    elif read == 'logs':
        a.update(stdout='Load time: 1\\nValidation time: 2\\n', stderr='raw diagnostic\\n')
        if mode == 'converter-ok':
            a['stdout']='_:r <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/ns/shacl#ValidationReport> .\\n_:v <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/ns/shacl#ValidationResult> .\\n'
        if mode == 'converter-empty': a['stdout']=''
    elif read == 'wait':
        if mode == 'timeout': time.sleep(30)
        if mode == 'agent-timeout': a.update(returncode=124, timed_out=True)
        if mode == 'wait-close': a.update(returncode=9, stderr='primary wait failure')
        if mode == 'wrong-daemon': a['daemon_id']='daemon-two'
        if mode == 'wrong-cid': a['container_id']='b'*64
        a['stdout']='7\\n' if mode == 'exit7' else '0\\n'
elif phase == 'start':
    if mode == 'wrong-start': a['attempt']='other-attempt'
    if (p / 'report-target').exists():
        Path((p / 'report-target').read_text()).write_text('raw report from fixture engine\\n')
elif phase == 'close':
    a['phase_closed']=mode not in ('pending', 'close', 'wait-close')
    if not a['phase_closed']: a.update(ack=False, error='cleanup UNKNOWN')
    if not (p / 'used').exists():
        a.update(ack=False, phase_closed=False, error='unconsumed plan requires quiescent driver')
    if mode == 'missing-close': a.pop('ack')
print(json.dumps(a))
'''

    def test_bound_lifecycle_and_failed_closure(self):
        self.assertTrue(callable(getattr(runtime.DockerRuntime, 'run_bound', None)),
                        'ERA requires an executable admitted-argv consumer, absent on baseline')
        modes = ('ok', 'exit7', 'timeout', 'agent-timeout', 'pending', 'close', 'missing-close',
                 'wait-close', 'wrong-cpu', 'wrong-daemon', 'wrong-cid', 'wrong-start', 'retreat')
        for mode in modes:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                agent = root / 'agent.py'
                agent.write_text(self.AGENT)
                (root / 'mode').write_text(mode)
                request = root / 'request.json'
                identity = dict(fleet_dir=tmp, job='fixture', step=0, attempt='one', owner='owner-one')
                env = dict(FLEET_CONTAINER_AGENT=str(agent), FLEET_CONTAINER_REQUEST=str(request),
                           FLEET_CONTAINER_NAME='bound-one', FLEET_CONTAINER_OWNER='owner-one', FLEET_CPU='11')
                argv = ['/resolved/docker', '--host', 'unix:///observed/docker.sock', 'create',
                        '--pull=never', '--memory', '12g', '--memory-swap', '12g', '--cpus', '1',
                        '--cpuset-cpus', '11', '--name', 'bound-one', '--label', 'fleet.owner=owner-one',
                        'sha256:' + 'c'*64, 'validate']
                request.write_text(json.dumps(dict(identity, argv=argv)))
                original = request.read_bytes()
                rt = runtime.DockerRuntime(root, 0)
                calls = 0
                def interrupted():
                    nonlocal calls
                    calls += 1
                    return mode == 'retreat' and calls > 1
                log = root / 'engine.log'
                with patch.dict(os.environ, env), \
                        patch.object(rt, 'args', side_effect=AssertionError('must not rerender argv')), \
                        patch.object(runtime, 'run_quiet', side_effect=AssertionError('no direct Docker')):
                    if mode in ('ok', 'exit7', 'timeout', 'agent-timeout'):
                        measured = rt.run_bound(argv, log, .1 if mode == 'timeout' else 2700)
                        self.assertEqual((measured.rc, measured.timed_out),
                                         (124, True) if mode in ('timeout', 'agent-timeout') else
                                         (7 if mode == 'exit7' else 0, False))
                    else:
                        with self.assertRaises((RuntimeError, InterruptedError)):
                            rt.run_bound(argv, log, 2700, interrupted)
                    with self.assertRaises(RuntimeError):
                        runtime.DockerRuntime(root, 0).run_bound(argv, root / 'replay.log')
                self.assertEqual(json.loads((root / 'used').read_text()), argv)
                self.assertEqual(request.read_bytes(), original)
                record = json.loads(log.with_suffix('.log.bound.json').read_text())
                operations = [json.loads(line) for line in (root / 'operations.jsonl').read_text().splitlines()]
                self.assertTrue(all(op['op'] == 'container_phase' and
                                    all(op[k] == v for k, v in identity.items()) for op in operations))
                self.assertTrue(any(op['phase'] == 'close' for op in operations))
                if mode in ('pending', 'close', 'missing-close', 'wait-close'):
                    self.assertFalse(record['phase_closed'])
                    self.assertTrue(record['cleanup_errors'])
                if mode == 'wait-close':
                    self.assertIn('primary wait failure', record['primary_error'])
                if mode == 'wrong-cpu':
                    self.assertFalse(any(op['phase'] == 'start' for op in operations))
                if mode == 'ok':
                    self.assertTrue(record['phase_closed'])
                    self.assertEqual(log.read_text(), 'Load time: 1\nValidation time: 2\nraw diagnostic\n')
                    self.assertTrue(any(op.get('read') == 'stats' for op in operations))
                    self.assertEqual(next(op for op in operations if op.get('read') == 'wait')['timeout_s'], 2700)

    def test_consume_retains_raw_reports_and_refuses_stale_or_unclosed_success(self):
        # Spec: the canonical consume command must keep complete per-attempt
        # evidence, propagate actual exits, never count a stale report, and never
        # publish successful rows when closure is unconfirmed.
        # Root's prelaunch RETREAT correction: InterruptedError stays exit130
        # when unconsumed close is refused; only the quiescent controller finalizes.
        import csv
        era_path = Path(__file__).resolve().parents[2] / 'breadth/era-shacl/run.py'
        definition = importlib.util.spec_from_file_location('era_consumer_contract', era_path)
        era = importlib.util.module_from_spec(definition)
        definition.loader.exec_module(era)
        self.assertTrue(callable(getattr(era, 'fleet_consume', None)),
                        'canonical consume command must execute the admitted plan')
        for mode in ('ok', 'exit7', 'close', 'stale', 'input-mismatch', 'source-mismatch',
                     'converter-ok', 'converter-empty', 'retreat-before'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                data, out = root / 'data', root / 'results'
                data.mkdir()
                (out / 'reports').mkdir(parents=True)
                source = data / 'ES.ttl'
                source.write_text('tiny fixture input')
                report = out / 'reports/jena-ES-tds-r1.ttl'
                report.write_text('stale prior report')
                converting = mode.startswith('converter-')
                if mode != 'stale' and not converting:
                    (root / 'report-target').write_text(str(report))
                (root / 'mode').write_text(mode)
                agent = root / 'agent.py'
                agent.write_text(self.AGENT)
                plan_args = types.SimpleNamespace(data='/fixture/data', results='/fixture/results',
                    job_id='fixture', owner='t3:fixture', source_git='source.git', source_rev='a'*40,
                    engines='jena', subsets='ES', shapes='tds', runs=1,
                    converter_image='sha256:'+'d'*64 if converting else None,
                    cpu_pool='11', host='fixture-host')
                job = era.fleet_job(plan_args)
                # Only the fixture host's path and attempt resolution is simulated;
                # the actual CLI consume, runtime and child transport all execute.
                number = 1 if converting else 0
                driver = job['steps'][number]
                case = json.loads(driver[5])
                case.update(data=str(data), results=str(out),
                            inputs={'ES.ttl': [source.stat().st_size,
                                              '0'*64 if mode == 'input-mismatch' else era.kit.sha256(source)]})
                launch = job['step_needs'][number]['docker']['argv']
                binding = {'@FLEET_CPU@':'11', '@FLEET_CONTAINER_NAME@':'bound-one',
                           '@FLEET_CONTAINER_OWNER@':'owner-one'}
                for token, value in binding.items():
                    launch = [s.replace(token, value) for s in launch]
                launch = ['/resolved/docker', '--host', 'unix:///observed/docker.sock', *launch[1:]]
                request = root / 'request.json'
                request.write_text(json.dumps(dict(fleet_dir=tmp, job='fixture', step=0, attempt='one',
                                                   owner='owner-one', argv=launch)))
                env = dict(FLEET_CONTAINER_AGENT=str(agent), FLEET_CONTAINER_REQUEST=str(request),
                           FLEET_CONTAINER_NAME='bound-one', FLEET_CONTAINER_OWNER='owner-one',
                           FLEET_CPU='11', FLEET_OUT=str(root / 'fetched'))
                with patch.dict(os.environ, env), \
                        patch.object(era.kit, 'retreated_since', return_value=mode == 'retreat-before'), \
                        patch.object(era.kit.manifest, 'git', return_value={
                            'commit': 'b'*40 if mode == 'source-mismatch' else 'a'*40, 'dirty':0}), \
                        patch.object(runtime, 'run_quiet', side_effect=AssertionError('no direct Docker')):
                    if mode in ('close', 'input-mismatch', 'source-mismatch', 'converter-empty'):
                        with self.assertRaises((RuntimeError, ValueError)):
                            era.main([*driver[3:5], json.dumps(case)])
                    else:
                        self.assertEqual(era.main([*driver[3:5], json.dumps(case)]),
                                         130 if mode == 'retreat-before' else
                                         7 if mode == 'exit7' else 1 if mode == 'stale' else 0)
                attempt = root / 'fetched/era-attempts/bound-one'
                record = json.loads((attempt / 'invocation.json').read_text())
                if not converting and mode != 'source-mismatch':
                    self.assertEqual((attempt / 'previous-report.ttl').read_text(), 'stale prior report')
                if mode in ('input-mismatch', 'source-mismatch'):
                    self.assertFalse((root / 'operations.jsonl').exists(), 'input mismatch must precede launch')
                if mode in ('close', 'input-mismatch', 'source-mismatch', 'converter-empty'):
                    self.assertFalse((attempt / 'results.csv').exists(), 'failed closure/preflight is not success')
                    self.assertIn('error', record)
                    self.assertNotIn('rows', record, 'unreadable output is not a zero count')
                elif mode == 'retreat-before':
                    self.assertEqual(record['interrupted'], 'RETREAT before bound launch')
                    self.assertNotIn('measured', record)
                    self.assertNotIn('error', record, 'cleanup refusal must not replace exit130 classification')
                    self.assertFalse((attempt / 'results.csv').exists())
                    self.assertFalse((root / 'used').exists(), 'prelaunch RETREAT must not consume its plan')
                    phases = [json.loads(line)['phase'] for line in
                              (root / 'operations.jsonl').read_text().splitlines()]
                    self.assertEqual(phases, ['close'], 'no launch, start, read or retry on prelaunch RETREAT')
                    bound = json.loads((attempt / 'container.log.bound.json').read_text())
                    self.assertFalse(bound['phase_closed'])
                    self.assertTrue(bound['finality_pending'])
                    self.assertNotIn('measured', bound)
                    self.assertEqual(bound['primary_error'], 'InterruptedError: RETREAT before bound launch')
                    self.assertIn('unconsumed plan requires quiescent driver', ''.join(bound['cleanup_errors']))
                    raw = json.loads((attempt / 'container.log.agent-0000-close-.stdout').read_text())
                    self.assertFalse(raw['ack'])
                    self.assertFalse(raw['phase_closed'])
                elif mode == 'converter-ok':
                    self.assertEqual(record['rows'], 1)
                    self.assertIn('not oracle equivalence', record['report_count'])
                    self.assertIn('ValidationReport', (attempt / 'report.nt').read_text())
                else:
                    with (attempt / 'results.csv').open(newline='') as f:
                        rows = list(csv.DictReader(f))
                    self.assertEqual([r['status'] for r in rows], ['ok','ok'] if mode == 'ok' else ['failed','failed'])
                    self.assertEqual(rows[1]['rows'], '', 'no implicit converter or fabricated oracle count')
                    self.assertIn('unavailable', rows[1]['note'])
                if mode not in ('stale', 'input-mismatch', 'source-mismatch', 'retreat-before') and not converting:
                    self.assertEqual((attempt / 'report.ttl').read_text(), 'raw report from fixture engine\n')


if __name__ == "__main__":
    unittest.main()
