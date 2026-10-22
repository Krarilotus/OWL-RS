"""Fleet push regressions. Fixtures do no Rust builds and never contact a real fleet.

Execute only as an owned official proof job; executed revisions are in the report.
These fixtures do not establish a complete ordinary gate pass.
"""
from __future__ import annotations

import base64
import contextlib
import copy
import importlib.util
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
LIB = ROOT / 'scripts/lib'
sys.path.insert(0, str(LIB))
import gate_protocol as protocol

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

client = load('gate_client_test', LIB/'gate-client.py')
selection = load('gate_selection_test', LIB/'gate-selection.py')
CLEAN = {k: v for k, v in os.environ.items() if not k.startswith(('GIT_', 'NRESE_', 'FLEET_', 'CARGO_',
    'NEXTEST_', 'RUST', 'MIRI', 'PYTHONPATH'))}
CLEAN.update({'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull})
CLEAN['PYTHONDONTWRITEBYTECODE'] = '1'

def fixture(part='rest'):
    r = {'key': 'key', 'head': 'a'*40, 'tree': 'b'*40, 'base': 'c'*40,
         'list_hash': 'd'*40, 'tooling': 'e'*64, 'flags': '-C target-cpu=native',
         'release': '1.98.1', 'rustc_commit': 'f'*40, 'files': [], 'windows_crates': [],
         'jobs': {}}
    spec = client.make_spec({**r, 'raw_flags': '', 'target_cpu': 'native'}, part,
        't3:fixture', '/fixture', {})
    r['jobs'][part] = spec
    s = {'changed': [], 'dependents': [], 'os_crates': [], 'windows_packages': [],
         '_source': {'request': r['key'], 'tree': r['tree'], 'files': r['files']}}
    rows = []
    attempts = []
    for i, unit in enumerate(['all']):
        required = protocol.required_for(r, part, s)
        rows.append({'request': r['key'], 'head': r['head'], 'tree': r['tree'], 'base': r['base'],
          'list_hash': r['list_hash'], 'tooling': r['tooling'], 'flags': r['flags'],
          'release': r['release'], 'rustc_commit': r['rustc_commit'], 'part': part, 'unit': unit,
          'step': i, 'job': spec['id'], 'fleet_host': 'native-host', 'attempt': f'{spec["id"]}:{i}:unique',
          'cpu': 'actual-cpu', 'target': 'x86_64-pc-windows-msvc' if part == 'windows' else 'x86_64-unknown-linux-gnu',
          'snapshot_after': r['tree'], 'tracked_changes': '', 'began': 110, 'ended': 120,
          'code': 0, 'required': required, 'selection': s,
          'steps': [{'name': name, 'code': 0} for name in required],
          'memory_cap_gb': spec['step_needs'][i]['ram_gb'], 'memory_job': 'nrese-build-real'})
        attempts.append({'step': i, 'host': 'native-host', 'started': 100, 'ended': 130, 'exit': 0, 'failure': None})
    for row in rows:
        row['artifact_key'] = protocol.artifact_key(r['tree'], r['release'], r['rustc_commit'],
                                                    r['flags'], row['target'], row['cpu'])
        row['artifact_target'] = ('target-gate-win-' if part == 'windows' else 'target-gate-') + row['artifact_key']
    job = {'spec': spec, 'state': 'done', 'next_step': len(rows), 'attempts': attempts}
    return r, rows, job

class Evidence(unittest.TestCase):
    def test_complete_linux_and_native_windows_parts(self):
        for part in protocol.UNITS:
            r, rows, job = fixture(part)
            self.assertEqual(protocol.validate_part(r, part, rows, job), 0)

    def test_r1_3_rejects_failed_zero_partial_or_contradictory_success(self):
        def state(j, rows): j['state'] = 'failed'
        def empty(j, rows): rows[0]['steps'] = []
        def failed_step(j, rows): rows[0]['steps'][0]['code'] = 101
        def no_record(j, rows): rows.pop()
        def skipped_required(j, rows): rows[0]['required'].remove('miri')
        for mutation in (state, empty, failed_step, no_record, skipped_required):
            with self.subTest(mutation=mutation.__name__):
                r, rows, job = fixture()
                mutation(job, rows)
                with self.assertRaises(protocol.NoVerdict): protocol.validate_part(r, 'rest', rows, job)

    def test_r1_3_binds_the_last_fleet_attempt_not_a_sibling_marker(self):
        for field, value in (('tree', 'x'*40), ('head', 'x'*40), ('base', 'x'*40), ('flags', 'other'),
                             ('tooling', 'x'*64), ('list_hash', 'x'*40), ('job', 'previous-job'),
                             ('fleet_host', 'other-host'), ('snapshot_after', 'x'*40),
                             ('tracked_changes', ' M Cargo.toml'), ('attempt', '')):
            r, rows, job = fixture()
            rows[0][field] = value
            with self.subTest(field=field), self.assertRaises(protocol.NoVerdict):
                protocol.validate_unit(r, 'rest', 0, rows[0], job)
        r, rows, job = fixture()
        job['attempts'].append({**job['attempts'][0], 'started': 200, 'ended': 240})
        with self.assertRaises(protocol.NoVerdict): protocol.validate_unit(r, 'rest', 0, rows[0], job)
        rows[0].update(began=210, ended=220, attempt='new-attempt')
        self.assertEqual(protocol.validate_unit(r, 'rest', 0, rows[0], job), 0)

    def test_real_failed_unit_has_failure_code_and_cannot_be_a_pass(self):
        r, rows, job = fixture()
        rows[0]['code'] = 1
        rows[0]['steps'][0]['code'] = 101
        job['state'] = 'failed'
        job['attempts'][0]['exit'] = 1
        job['attempts'][0]['failure'] = {'kind': 'exit', 'code': 1}
        self.assertEqual(protocol.validate_unit(r, 'rest', 0, rows[0], job), 1)

    def test_controller_infrastructure_failure_cannot_be_a_workload_verdict(self):
        for failure in ({'kind': 'oom'}, {'kind': 'exit', 'code': 75}, {'kind': 'timeout'}):
            r, rows, job = fixture()
            rows[0]['code'] = 1; rows[0]['steps'][0]['code'] = 101
            job['state'] = 'failed'; job['attempts'][0].update(exit=1, failure=failure)
            with self.subTest(failure=failure), self.assertRaises(protocol.NoVerdict):
                protocol.validate_unit(r, 'rest', 0, rows[0], job)

    def test_windows_requires_the_declared_aggregate_job_cap(self):
        for field, value in (('memory_job', ''), ('memory_cap_gb', 0), ('target', '')):
            r, rows, job = fixture('windows')
            rows[0][field] = value
            with self.subTest(field=field), self.assertRaises(protocol.NoVerdict):
                protocol.validate_unit(r, 'windows', 0, rows[0], job)

    def test_scope_inventory_cannot_drop_selected_tests_or_side_work(self):
        r, rows, job = fixture()
        r['files'] = ['Cargo.toml', 'benches/suite/tests/x.py', 'benches/nrese-bench-harness/src/lib.rs']
        rows[0]['selection'] = {'changed': ['a'], 'dependents': [], 'os_crates': [], 'windows_packages': [],
            '_source': {'request': r['key'], 'tree': r['tree'], 'files': r['files']}}
        with self.assertRaises(protocol.NoVerdict): protocol.validate_unit(r, 'rest', 0, rows[0], job)

    def test_r1_2_set_empty_encoded_flags_rejected_before_lookup(self):
        for value in ('', '--cfg\x1fa', '--cfg\x1fb'):
            with self.subTest(value=value), self.assertRaises(protocol.NoVerdict):
                protocol.check_overrides({'CARGO_ENCODED_RUSTFLAGS': value})
        for name in ('RUSTC_WRAPPER', 'RUSTUP_TOOLCHAIN', 'CARGO_BUILD_TARGET',
                     'CARGO_BUILD_BUILD_DIR', 'CARGO_BUILD_TARGET_DIR',
                     'CARGO_PROFILE_DEV_OPT_LEVEL', 'CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS'):
            with self.assertRaises(protocol.NoVerdict): protocol.check_overrides({name: ''})
        self.assertNotEqual(protocol.digest({'flags': '-C a b'}), protocol.digest({'flags': '-C_a_b'}))

    def test_r5_environment_isolation(self):
        e = protocol.clean_env({'PATH': '/bin', 'CARGO_HOME': '/cache', 'GIT_DIR': '/source/.git',
            'GIT_INDEX_FILE': '/source/index', 'NRESE_ON_BUILD_HOST': '1', 'NRESE_FILES_FROM': '/bad',
            'NEXTEST_TEST_THREADS': '100', 'FLEET_OWNER': 'old-owner', 'CARGO_BUILD_JOBS': '100'})
        self.assertEqual(e, {'PATH': '/bin', 'CARGO_HOME': '/cache'})

    def test_rsh_checked_head_cannot_authorize_shared_or_foreign_workspace_artifacts(self):
        r, rows, job = fixture()
        valid = copy.deepcopy(rows[0])
        for field, value in (('artifact_key', ''), ('artifact_target', 'target-1.98.1'),
                             ('artifact_key', protocol.artifact_key('0'*40, r['release'],
                              r['rustc_commit'], r['flags'], valid['target'], valid['cpu']))):
            row = {**valid, field: value}
            with self.subTest(field=field, value=value), self.assertRaises(protocol.NoVerdict):
                protocol.validate_unit(r, 'rest', 0, row, job)

    def test_artifact_identity_changes_with_each_source_and_native_tool_input(self):
        inputs = ['a'*40, '1.98.1', 'b'*40, '-C target-cpu=native',
                  'x86_64-unknown-linux-gnu', 'actual-native-features']
        same = protocol.artifact_key(*inputs)
        self.assertEqual(same, protocol.artifact_key(*inputs))
        for i in range(len(inputs)):
            different = inputs.copy(); different[i] += '-different'
            self.assertNotEqual(same, protocol.artifact_key(*different))
        source = (LIB/'gate-job.sh').read_text()
        self.assertIn('target-gate-$artifact', source)
        self.assertIn('target-gate-win-$artifact', source)
        self.assertIn('export CARGO_BUILD_BUILD_DIR="$CARGO_TARGET_DIR"', source)
        self.assertIn('export CARGO_BUILD_BUILD_DIR="$CARGO_TARGET_DIR"', (ROOT/'scripts/miri.sh').read_text())
        self.assertNotIn('gate-cache-sweep.py', source)

    def test_empty_role_override_remains_bound_to_the_complete_fleet_spec(self):
        for part in protocol.UNITS:
            r, rows, job=fixture(part)
            self.assertEqual(r['jobs'][part]['requires']['roles'],[])
            self.assertEqual(protocol.validate_part(r,part,rows,job),0)
            for different in (None,['test']):
                changed=copy.deepcopy(job)
                if different is None: del changed['spec']['requires']['roles']
                else: changed['spec']['requires']['roles']=different
                with self.subTest(part=part,roles=different), self.assertRaises(protocol.NoVerdict):
                    protocol.validate_part(r,part,rows,changed)

    def test_confirmation_matches_explicit_roles_and_excludes_the_failed_physical_host(self):
        r, _, _=fixture('windows')
        spec=r['jobs']['windows']
        catalog=[{'spec':{'name':name,'os':os_name,'roles':roles}} for name,os_name,roles in (
            ('khaleesi','windows',['measure','test']),
            ('khaleesi-wsl','windows',['test']),
            ('kraris-gptler','windows',['build','prepare']),
            ('other-native','windows',[]),
            ('test-native','windows',['test']),
            ('linux-test','linux',['test']))]
        self.assertEqual(client.confirmation_hosts(spec,catalog),
                         ['kraris-gptler','other-native','test-native'])
        explicit=copy.deepcopy(spec); explicit['requires']['roles']=['test']
        self.assertEqual(client.confirmation_hosts(explicit,catalog),['test-native'])


class Selection(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.root)
        self.metadata = {'workspace_members': ['a', 'b'], 'packages': [
          {'id': n, 'name': n, 'manifest_path': str(self.root/'crates'/n/'Cargo.toml')} for n in ('a', 'b')]}

    def test_global_and_deleted_inputs_select_every_workspace_member(self):
        for files in (['Cargo.lock'], ['.cargo/config.toml'], ['crates/deleted/src/lib.rs']):
            selected = selection.select(files, self.metadata, self.root, lambda _: {'a', 'b'})
            self.assertEqual(selected, {'changed': ['a', 'b'], 'dependents': []})

    def test_dependents_are_preserved_and_failed_selection_is_not_empty_success(self):
        self.assertEqual(selection.select(['crates/a/src/lib.rs'], self.metadata, self.root,
            lambda _: {'a', 'b'}), {'changed': ['a'], 'dependents': ['b']})
        with self.assertRaises(ValueError):
            selection.select(['crates/a/src/lib.rs'], self.metadata, self.root, lambda _: set())

    def test_custom_cargo_home_config_is_an_effective_input_not_a_parent_dot_cargo(self):
        checkout = self.root/'checkout'; checkout.mkdir()
        home = self.root/'custom-registry'; home.mkdir()
        protocol.check_external_cargo_config(checkout, {'CARGO_HOME':str(home)})
        (home/'config.toml').write_text('[build]\nrustc-wrapper="unbound-tool"\n')
        with self.assertRaises(protocol.NoVerdict):
            protocol.check_external_cargo_config(checkout, {'CARGO_HOME':str(home)})

    def test_truthful_os_and_step_resources_with_broad_placement(self):
        for part in protocol.UNITS:
            r, _, _ = fixture(part)
            spec = r['jobs'][part]
            self.assertEqual(spec['requires']['os'], 'windows' if part == 'windows' else 'linux')
            self.assertEqual(spec['requires']['roles'], [])
            self.assertEqual(spec['kind'], 'owlrs.gate')
            self.assertEqual(spec['class'], 'shareable')
            self.assertIs(spec['yields'], True)
            self.assertEqual(spec['requires']['hosts'], [])
            self.assertEqual(spec['requires']['has'], [])
            needs = json.loads(spec['env']['NRESE_GATE_NEEDS'])
            self.assertEqual(needs, spec['step_needs'])
            self.assertEqual(len(needs), len(spec['steps']))
            if part == 'windows': self.assertIn('scripts/lib/gate-unit.ps1', spec['steps'][0])

@unittest.skipUnless(os.name == 'nt', 'native Windows Git tool discovery')
class NativeGitTools(unittest.TestCase):
    def resolve(self, selected_git):
        # Supply only Get-Command's result; exercise the production resolver and
        # filesystem checks. Dummy layout files are never executed or exported.
        script = r'''
$ErrorActionPreference='Stop'
. $env:NRESE_RESOLVER_SOURCE
function Get-Command {
    param($Name,$CommandType,$ErrorAction)
    if ($env:NRESE_RESOLVER_GIT) { [pscustomobject]@{Source=$env:NRESE_RESOLVER_GIT} }
}
try { Resolve-GitBash; exit 0 } catch { [Console]::Error.WriteLine($_.Exception.Message); exit 75 }
'''
        return subprocess.run(['powershell','-NoProfile','-NonInteractive','-EncodedCommand',
            base64.b64encode(script.encode('utf-16le')).decode()],stdin=subprocess.DEVNULL,
            capture_output=True,text=True,timeout=10,env={**os.environ,
                'NRESE_RESOLVER_SOURCE':str(LIB/'git-bash.ps1'),
                'NRESE_RESOLVER_GIT':str(selected_git) if selected_git else ''})

    def test_standard_and_portable_layouts_use_real_bash_from_selected_git(self):
        with tempfile.TemporaryDirectory(prefix='git-bash-layout-') as d:
            for layout in ('Program Files/Git','fleet/tools/PortableGit'):
                root=Path(d)/layout
                real=root/'usr/bin/bash.exe'; real.parent.mkdir(parents=True)
                real.touch()
                launcher=root/'bin/bash.exe'; launcher.parent.mkdir(); launcher.touch()
                for relative in ('cmd/git.exe','bin/git.exe','mingw64/bin/git.exe',
                                 'mingw32/bin/git.exe','usr/bin/git.exe'):
                    with self.subTest(layout=layout,git=relative):
                        done=self.resolve(root/relative)
                        self.assertEqual(done.returncode,0,done.stderr)
                        self.assertEqual(Path(done.stdout.strip()),real)
                        self.assertNotEqual(Path(done.stdout.strip()),launcher)

    def test_hook_git_core_layout_keeps_the_selected_installation(self):
        with tempfile.TemporaryDirectory(prefix='git-hook-core-layout-') as d:
            root=Path(d)/'Program Files/Git'
            real=root/'usr/bin/bash.exe'; real.parent.mkdir(parents=True); real.touch()
            for arch in ('mingw64','mingw32'):
                selected=root/arch/'libexec/git-core/git.exe'
                done=self.resolve(selected)
                self.assertEqual(done.returncode,0,done.stderr)
                self.assertEqual(Path(done.stdout.strip()),real)

    def test_missing_git_is_a_clear_failure(self):
        done=self.resolve(None)
        self.assertEqual(done.returncode,75)
        self.assertIn('git.exe was not found on PATH',done.stderr)

    def test_missing_real_bash_does_not_accept_the_launcher(self):
        with tempfile.TemporaryDirectory(prefix='git-bash-missing-') as d:
            root=Path(d)/'Git'
            launcher=root/'bin/bash.exe'; launcher.parent.mkdir(parents=True); launcher.touch()
            selected=root/'cmd/git.exe'
            done=self.resolve(selected)
            self.assertEqual(done.returncode,75)
            self.assertIn('requires real Git Bash',done.stderr)
            self.assertIn(str(selected),done.stderr)
            self.assertEqual(done.stdout,'')

    def test_unrecognized_git_location_is_not_a_search_for_another_bash(self):
        with tempfile.TemporaryDirectory(prefix='git-bash-unrecognized-') as d:
            done=self.resolve(Path(d)/'shims/git.exe')
            self.assertEqual(done.returncode,75)
            self.assertIn('cannot resolve Git for Windows root',done.stderr)


@unittest.skipUnless(os.name == 'nt', 'native Windows JobObject workload proof')
class NativeWrapper(unittest.TestCase):
    @contextlib.contextmanager
    def retained_artifacts(self, directory):
        try:
            yield
        finally:
            source = Path(directory)/'out'
            if os.environ.get('FLEET_OUT') and source.exists():
                destination = Path(os.environ['FLEET_OUT'])/('native-fixture-'+Path(directory).name)
                shutil.copytree(source,destination,dirs_exist_ok=True)

    def exercise(self, sleeping, limit, helper_fail=False, arguments=()):
        with tempfile.TemporaryDirectory(prefix='gate-native-proof-') as d, self.retained_artifacts(d):
            root = Path(d)
            (root/'scripts/lib').mkdir(parents=True)
            shutil.copy(ROOT/'scripts/lib/memory-cap.ps1', root/'scripts/lib/real-memory-cap.ps1')
            # The owned helper must give its native descendants valid default stdin,
            # even when the wrapper's detached entry has no handle. Real helper stays
            # byte-for-byte unchanged, preserving its compatible compiled cache.
            (root/'scripts/lib/memory-cap.ps1').write_text("""param([int]$ProcessId,[double]$LimitGB)
$probe=@'
import os,pathlib,subprocess,sys
subprocess.check_call([sys.executable,'-c','pass'])
pathlib.Path(os.environ['FLEET_OUT'],'helper-stdio-ok').write_text('ok')
'@
& python -c $probe
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
if ($env:NRESE_GATE_FIXTURE_HELPER_FAIL -eq '1') { exit 31 }
& (Join-Path $PSScriptRoot 'real-memory-cap.ps1') -ProcessId $ProcessId -LimitGB $LimitGB
exit $LASTEXITCODE
""",encoding='utf-8')
            out = root/'out'; out.mkdir()
            (root/'fixture.sh').write_text('''#!/usr/bin/env bash
set -eu
cap=$(cygpath -u "$NRESE_GATE_CAP_FILE")
fixture_out=$(cygpath -u "$FLEET_OUT")
cat "/proc/$$/winpid" > "$fixture_out/bash-pid"
until [ -s "$cap" ]; do sleep 0.1; done
: > "$fixture_out/arguments"
if [ "$#" -gt 0 ]; then printf '%s\\0' "$@" > "$fixture_out/arguments"; fi
python -c 'import os,time,pathlib,subprocess,sys; pathlib.Path(os.environ["FLEET_OUT"],"worker-pid").write_text(str(os.getpid())); subprocess.check_call([sys.executable,"-c","pass"]); time.sleep('''+str(sleeping)+''')'
''', encoding='utf-8')
            # Reproduce a detached native entry without a valid stdin handle. The
            # boundary must provide EOF to its child; the fixture's native Python
            # then spawns a default-stdin child, exercising GetStdHandle directly.
            quote = lambda value: "'"+value.replace("'","''")+"'"
            launcher = '''Add-Type -TypeDefinition '
using System; using System.Runtime.InteropServices;
public static class GateFixtureStdin {
 [DllImport("kernel32.dll")] public static extern bool SetStdHandle(int which, IntPtr handle);
}'
[GateFixtureStdin]::SetStdHandle(-10, [IntPtr]::Zero) | Out-Null
& '''+quote(str(ROOT/'scripts/lib/gate-unit.ps1'))+' '+quote(json.dumps(['fixture.sh', *arguments]))+'''
exit $LASTEXITCODE
'''
            argv = ['powershell','-NoProfile','-NonInteractive','-EncodedCommand',
                    base64.b64encode(launcher.encode('utf-16le')).decode()]
            env = {**os.environ, 'FLEET_JOB':os.environ.get('FLEET_JOB','native-fixture'), 'FLEET_STEP':'1',
                   'FLEET_OUT':str(out), 'NRESE_GATE_NEEDS':'[{"cores":1,"ram_gb":1}]',
                   'NRESE_GATE_UNIT_S':str(limit), 'NRESE_GATE_FIXTURE_HELPER_FAIL':str(int(helper_fail))}
            log = out/'wrapper.log'
            # A killed parent can leave inherited pipe writers alive. Files retain
            # partial output and let the harness reach its own bound independently.
            with log.open('wb') as stream:
                child = subprocess.Popen(argv,cwd=root,env=env,stdin=subprocess.DEVNULL,
                                         stdout=stream,stderr=subprocess.STDOUT)
                try:
                    code = child.wait(timeout=limit+30)
                except subprocess.TimeoutExpired:
                    subprocess.run(['taskkill','/PID',str(child.pid),'/T','/F'],
                        stdin=subprocess.DEVNULL,stdout=stream,stderr=subprocess.STDOUT,timeout=10)
                    child.wait(timeout=10)
                    self.fail('native wrapper exceeded its bound: '+log.read_text(errors='replace')[-6000:])
            text = log.read_text(errors='replace')
            done = subprocess.CompletedProcess(argv,code,text,'')
            self.assertTrue((out/'helper-stdio-ok').exists(), text)
            self.assertEqual((out/'worker-pid').exists(), not helper_fail, text)
            for name in ('bash-pid','worker-pid'):
                if not (out/name).exists():
                    if name=='bash-pid': self.fail('no native waiter identity: '+text)
                    continue
                pid = int((out/name).read_text())
                alive = subprocess.run(['powershell','-NoProfile','-Command',
                    f'if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 1 }}'],
                    stdin=subprocess.DEVNULL,capture_output=True,timeout=10)
                self.assertEqual(alive.returncode,0,'native part left '+name+' alive: '+text)
            if not helper_fail:
                self.assertEqual((out/'arguments').read_bytes(),
                                 b''.join(value.encode('utf-8')+b'\0' for value in arguments), text)
            lifecycle = (out/'unit-0.lifecycle.log').read_text()
            self.assertIn('cleanup-end',lifecycle,text)
            # A Git bin/bash launcher has a different PID from its real Bash
            # child. Capping only the launcher admits later workers uncapped.
            bash_pid = int((out/'bash-pid').read_text())
            self.assertIn('bash-started '+str(bash_pid),lifecycle,text)
            if helper_fail:
                self.assertIn('helper-exit 31',lifecycle,text)
                self.assertNotIn('cap-attached',lifecycle,text)
            else:
                self.assertIn('helper-exit 0',lifecycle,text)
                self.assertIn('cap-attached nrese-build-',lifecycle,text)
                self.assertIn('job-active-final 0',lifecycle,text)
                name = (out/'unit-0.cap-helper.log').read_text().strip()
                self.assertRegex(name,r'^nrese-build-[0-9]+-[0-9]+$')
                self.assertIn('cap-attached '+name,lifecycle,text)
            return done

    def test_30_direct_request_arguments_remain_separate(self):
        # The ordinary first push has metadata plus hundreds of paths. The old
        # PowerShell @(...ConvertFrom-Json...) made that entire array one filename;
        # the earlier one-script-argument lifecycle fixtures could not expose it.
        arguments = ['push', 'a'*40, 'b'*40, 'windows', '0', 'c'*64,
                     'd'*40, '-C target-cpu=native', 'e'*40, '--']
        arguments += [f'crates/fixture/src/file-{i}.rs' for i in range(315)]
        arguments += ['path with spaces/file.rs', 'say "quoted"', 'C:\\path with spaces\\']
        done = self.exercise(0, 30, arguments=arguments)
        self.assertEqual(done.returncode, 0, (done.stdout, done.stderr))

    def test_20_helper_failure_before_cap_stops_its_waiter_tree(self):
        done = self.exercise(0,30,helper_fail=True)
        self.assertNotEqual(done.returncode,0,done.stdout)

    def test_00_normal_exit_closes_the_aggregate_job(self):
        done = self.exercise(0, 30)
        self.assertEqual(done.returncode, 0, (done.stdout, done.stderr))

    def test_10_deadline_ends_the_named_job_and_gives_no_verdict(self):
        done = self.exercise(30, 10)
        self.assertEqual(done.returncode, 75, (done.stdout, done.stderr))

@unittest.skipUnless(os.name != 'nt' and shutil.which('bash') and shutil.which('timeout'),
                     'CLI fixtures use POSIX executables; native wrapper gets its own fleet proof')
class OrdinaryHook(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='push-gate-proof-')
        self.addCleanup(temp.cleanup)
        self.home = Path(temp.name)
        self.repo = self.home/'repo'
        self.repo.mkdir()
        self.env = dict(CLEAN)
        self.git('init', '-q')
        self.git('config', 'user.email', 'proof@localhost')
        self.git('config', 'user.name', 'proof fixture')
        (self.repo/'hooks').mkdir()
        self.git('config', 'core.hooksPath', str(self.repo/'hooks'))
        (self.repo/'a.txt').write_text('published A\n')
        self.git('add', '-A'); self.git('commit', '-qm', 'published fixture')
        self.base = self.git('rev-parse', 'HEAD')
        shutil.copytree(ROOT/'scripts', self.repo/'scripts')
        shutil.copytree(ROOT/'.githooks', self.repo/'.githooks')
        shutil.copy(ROOT/'rust-toolchain.toml', self.repo/'rust-toolchain.toml')
        (self.repo/'scripts/lib/push-base.txt').write_text(self.base+'\n')
        (self.repo/'benches/suite').mkdir(parents=True)
        shutil.copy(ROOT/'benches/suite/requirements.txt', self.repo/'benches/suite/requirements.txt')
        (self.repo/'a.txt').write_text('finish B\n')
        self.git('add', '-A'); self.git('commit', '-qm', 'finish fixture')
        self.head = self.git('rev-parse', 'HEAD')
        self.stubs = self.home/'stubs'; self.stubs.mkdir()
        (self.stubs/'rustc').write_text('#!/bin/sh\nprintf "release: 1.98.1\\ncommit-hash: '+ 'f'*40 +'\\n"\n')
        (self.stubs/'rustc').chmod(0o755)
        cli = self.stubs/'fleet'
        cli.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
d=pathlib.Path(os.environ['FAKE_FLEET']); a=sys.argv[1:]
with (d/'calls').open('a') as f: f.write(json.dumps(a)+'\\n')
if a[0]=='submit':
 s=json.load(open(a[1])); (d/(s['id']+'.json')).write_text(json.dumps(s)); print(json.dumps({'submitted':[s['id']]}))
elif a[0]=='show': print((d/(a[1]+'.state.json')).read_text() if (d/(a[1]+'.state.json')).exists() else json.dumps({'state':'pending'}))
elif a[0]=='results': print(json.dumps({'path':str(d/a[1]/'out')}))
else: sys.exit(3)
''')
        cli.chmod(0o755)
        self.fake = self.home/'fleet'; self.fake.mkdir()
        self.env.update({'PATH': str(self.stubs)+os.pathsep+self.env['PATH'], 'FAKE_FLEET': str(self.fake),
                         'NRESE_FLEET_BIN': str(cli), 'FLEET_OWNER': 't3:fixture'})

    def git(self, *args):
        return subprocess.run(['git', '-C', str(self.repo), *args], env=self.env,
             check=True, capture_output=True, text=True).stdout.strip()

    def hook(self, refs=None, **extra):
        if refs is None:
            refs = [(self.head, 'refs/heads/refactor/engine-v2')]
        return subprocess.run(['bash', '.githooks/pre-push'], cwd=self.repo, env={**self.env, **extra},
            input=''.join(f'refs/heads/finish {sha} {target} {"0"*40}\n' for sha, target in refs),
            capture_output=True, text=True, timeout=60)

    def test_only_protected_destinations_require_push_records(self):
        cases = [
            ('finish', [(self.head, 'refs/heads/finish/release')], 0),
            ('engine', [(self.head, 'refs/heads/refactor/engine-v2')], 75),
            ('main', [(self.head, 'refs/heads/main')], 75),
            ('mixed', [(self.head, 'refs/heads/refactor/engine-v2'),
                       (self.head, 'refs/heads/finish/release')], 75),
            ('delete', [('0'*40, 'refs/heads/main')], 0),
            ('other-commit', [(self.base, 'refs/heads/finish/release')], 1),
        ]
        for name, refs, expected in cases:
            with self.subTest(name=name):
                before = (self.fake/'calls').read_text() if (self.fake/'calls').exists() else ''
                done = self.hook(refs)
                after = (self.fake/'calls').read_text() if (self.fake/'calls').exists() else ''
                if os.environ.get('FLEET_OUT'):
                    (Path(os.environ['FLEET_OUT'])/(name+'.json')).write_text(json.dumps({
                        'refs': refs, 'code': done.returncode, 'stdout': done.stdout,
                        'stderr': done.stderr, 'fleet_calls': after[len(before):]}, indent=2))
                self.assertEqual(done.returncode, expected, (done.stdout, done.stderr))
                if expected != 75:
                    self.assertEqual(after, before, 'unprotected/deleted refs must not consult fleet')

    def request(self):
        return json.loads(next((self.repo/'.git/nrese-gate-requests').glob('*/request.json')).read_text())

    def complete(self, request):
        for part, spec in request['jobs'].items():
            out = self.fake/spec['id']/'out'; out.mkdir(parents=True)
            r, rows, job = fixture(part)
            s = {'changed': [], 'dependents': [], 'os_crates': [], 'windows_packages': [],
                 '_source': {'request': request['key'], 'tree': request['tree'], 'files': request['files']}}
            required = protocol.required_for(request, part, s)
            job['spec'] = spec
            for i, row in enumerate(rows):
                row.update({k: request[k] for k in ('tree','head','base','list_hash','tooling','flags','rustc_commit','release')})
                row.update(request=request['key'], job=spec['id'], required=required, selection=s,
                           memory_cap_gb=spec['step_needs'][i]['ram_gb'], snapshot_after=request['tree'])
                row['artifact_key'] = protocol.artifact_key(request['tree'], request['release'],
                    request['rustc_commit'], request['flags'], row['target'], row['cpu'])
                row['artifact_target'] = ('target-gate-win-' if part == 'windows' else 'target-gate-') + row['artifact_key']
                row['steps'] = [{'name': name, 'code': 0} for name in required]
                (out/f'unit-{i}.json').write_text(json.dumps(row))
            (self.fake/(spec['id']+'.state.json')).write_text(json.dumps(job))

    def test_first_push_uses_published_base_and_pending_never_builds_here(self):
        self.git('update-ref', 'refs/remotes/origin/self', self.head)
        self.git('config', 'branch.'+self.git('branch','--show-current')+'.remote', '.')
        self.git('config', 'branch.'+self.git('branch','--show-current')+'.merge', 'refs/remotes/origin/self')
        done = self.hook(NRESE_ON_BUILD_HOST='1', NRESE_BUILD_HOST='some-ssh-alias')
        self.assertEqual(done.returncode, 75, done.stderr)
        r = self.request()
        self.assertEqual(r['base'], self.base)
        self.assertIn('a.txt', r['files'])
        for spec in r['jobs'].values(): self.assertEqual(spec['inputs'][0]['source']['rev'], self.head)
        self.assertFalse((self.repo/'.git/nrese-gate-passed').exists())
        self.assertEqual(self.hook().returncode, 75)
        submits = [json.loads(x) for x in (self.fake/'calls').read_text().splitlines() if json.loads(x)[0]=='submit']
        self.assertEqual(len(submits), 2, 'a pending request must not duplicate work')

    def test_both_parts_and_matching_latest_evidence_are_needed_by_the_normal_hook(self):
        self.assertEqual(self.hook().returncode, 75)
        r = self.request(); self.complete(r)
        accepted = self.hook()
        self.assertEqual(accepted.returncode, 0, accepted.stderr)
        path = self.fake/r['jobs']['windows']['id']/'out/unit-0.json'
        data = json.loads(path.read_text()); data['snapshot_after'] = '0'*40; path.write_text(json.dumps(data))
        rejected = self.hook()
        self.assertEqual(rejected.returncode, 75, rejected.stderr)  # breadcrumb cannot replay a stale pass

    def test_r1_1_all_snapshot_inputs_and_r1_2_encoded_flags_fail_before_submit(self):
        for value in ('', '--cfg\x1fone'):
            self.assertEqual(self.hook(CARGO_ENCODED_RUSTFLAGS=value).returncode, 75)
        self.assertFalse((self.fake/'calls').exists())
        (self.repo/'.cargo').mkdir(); (self.repo/'.cargo/config.toml').write_text('[build]\nrustflags=["--cfg","new"]\n')
        self.assertEqual(self.hook().returncode, 75)
        self.assertFalse((self.fake/'calls').exists())

    def test_rename_scope_keeps_both_paths_and_git_failure_is_not_empty_success(self):
        self.git('mv', 'a.txt', 'with space and "quote".txt')
        self.git('commit', '-qam', 'rename fixture')
        self.head = self.git('rev-parse', 'HEAD')
        old = Path.cwd()
        try:
            os.chdir(self.repo)
            paths, _ = protocol.scope(self.base, self.head)
            self.assertIn('a.txt', paths); self.assertIn('with space and "quote".txt', paths)
            with self.assertRaises(protocol.NoVerdict): protocol.scope('0'*40, self.head)
        finally: os.chdir(old)

    def test_r5_each_failed_snapshot_stage_stops_submission(self):
        real_git = shutil.which('git')
        for stage in ('read-tree', 'add', 'write-tree'):
            stub = self.stubs/'git'
            stub.write_text('#!/bin/sh\nfor a in "$@"; do [ "$a" != "'+stage+'" ] || exit 42; done\nexec "'+real_git+'" "$@"\n')
            stub.chmod(0o755)
            done = self.hook()
            self.assertEqual(done.returncode, 75, (stage, done.stderr))
            self.assertFalse((self.fake/'calls').exists())

    def test_r1_4_export_is_vetoed_on_the_host_before_any_build_or_output(self):
        out = self.home/'export-out'
        done = subprocess.run(['bash', 'scripts/lib/gate-job.sh', 'cargo', '0'*40, '-'],
            cwd=self.repo, env={**self.env, 'FLEET_OUT': str(out), 'FLEET_HOST': 'khaleesi-wsl'},
            capture_output=True, text=True, timeout=10)
        self.assertEqual(done.returncode, 75, done.stderr)
        self.assertFalse(out.exists())

    def test_r1_5_transport_timeout_cannot_yield_a_pass(self):
        (self.stubs/'timeout').write_text('#!/bin/sh\nexit 124\n')
        (self.stubs/'timeout').chmod(0o755)
        done = self.hook()
        self.assertEqual(done.returncode, 75)
        self.assertFalse((self.repo/'.git/nrese-gate-passed').exists())

    def test_r1_5_term_ignoring_child_is_killed_with_its_process_group(self):
        import signal
        marker = self.home/'owned-process-group'
        script = 'trap "" TERM; ps -o pgid= -p $$ > "$MARKER"; while :; do sleep 1; done'
        started = time.monotonic()
        try:
            with self.assertRaises(protocol.NoVerdict):
                client.command(['bash','-c',script], {**self.env, 'MARKER':str(marker)}, seconds=1)
            self.assertLess(time.monotonic()-started, 16, 'TERM-only deadline left its child holding the pipe')
        finally:
            if marker.exists():
                group = int(marker.read_text().strip())
                if group != os.getpgrp():
                    try: os.killpg(group, signal.SIGKILL)
                    except ProcessLookupError: pass

    def test_normal_commit_keeps_clippy_dependent_compile_and_fast_tests(self):
        crate = self.repo/'crates/a'
        (crate/'src').mkdir(parents=True)
        (crate/'Cargo.toml').write_text('[package]\nname = "a"\nversion = "0.1.0"\n')
        files = self.home/'commit-files'; files.write_text('crates/a/src/lib.rs\n')
        cargo = self.stubs/'cargo'
        cargo.write_text('''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
a=sys.argv[1:]
with Path(os.environ['CARGO_CALLS']).open('a') as f: f.write(json.dumps(a)+'\\n')
if a[:1]==['tree']: print('a v0.1.0\\nb v0.1.0')
elif a[:1]==['metadata']: print(json.dumps({'packages':[{'name':'a','metadata':{},'targets':[{'name':'a','kind':['lib']}]}]}))
elif a==['nextest','--version']: print('cargo-nextest 0.9.146')
''')
        cargo.chmod(0o755)
        # Guarded cargo is a stand-in only in this private fixture; it records commands,
        # so the assertion observes the commit contract without compiling anything.
        (self.repo/'scripts/cargo-guarded.sh').write_text('#!/bin/sh\nexec cargo "$@"\n')
        (self.repo/'scripts/cargo-guarded.sh').chmod(0o755)
        calls = self.home/'cargo-calls'
        done = subprocess.run(['bash','scripts/check.sh','commit'], cwd=self.repo,
            env={**self.env, 'CARGO_CALLS':str(calls), 'NRESE_ON_BUILD_HOST':'1',
                 'NRESE_FILES_FROM':str(files), 'NRESE_W3C_OPTIONAL':'1'},
            capture_output=True, text=True, timeout=30)
        self.assertEqual(done.returncode, 0, done.stderr)
        commands = [json.loads(row) for row in calls.read_text().splitlines()]
        self.assertIn(['fmt','--all','--check'], commands)
        self.assertIn(['clippy','--locked','-p','a','--all-targets'], commands)
        self.assertIn(['check','--locked','-q','-p','b','--lib'], commands)
        self.assertTrue(any(row[:2]==['nextest','run'] for row in commands))
        self.assertFalse(any('miri' in row for row in commands))

if __name__ == '__main__':
    unittest.main()
