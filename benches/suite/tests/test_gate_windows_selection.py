"""Native selection and finalized nextest metadata reuse; no real Cargo builds."""
import contextlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from test_gate_fleet_push import CLEAN, ROOT, client


# Spec: selected-setup owner, 23 Oct: locked metadata -> binaries-only compile ->
# finalization -> run with the exact metadata and no rebuilding or early queries.
FAKE_CARGO = '''import json, os, pathlib, sys
root = pathlib.Path(os.environ['SELECT_OUT'])
owner, *args = sys.argv[1:]
action = 'metadata' if owner == 'cargo' else args[0] if args[0] == 'clippy' else 'nextest-' + args[1]
(root / (action + '.argv')).write_bytes(b'\\0'.join(a.encode() for a in args) + b'\\0')
with (root / 'events').open('a') as stream:
    stream.write(action + '\\n')
metadata = {'workspace_root': 'selected-workspace', 'version': 1}
binaries = {'rust-suites': {'selected': {'binary-id': 'selected-binary'}}}
packages = ['-p', 'nrese-engine', '-p', 'nrese-exec', '-p', 'nrese-server', '-p', 'nrese-store']
def read(option):
    return json.loads(pathlib.Path(args[args.index(option) + 1]).read_text())
if owner == 'cargo':
    assert args == ['metadata', '--locked', '--format-version', '1'], args
    if os.environ.get('SELECT_CARGO_FAIL'):
        sys.exit(61)
    print(json.dumps(metadata))
elif action == 'clippy':
    assert args == ['clippy', '--locked', '-p', 'nrese-engine', '-p', 'nrese-exec', '--all-targets'], args
elif action == 'nextest-list':
    assert args[:3] == ['nextest', 'list', '--locked'], args
    assert args[3:11] == packages, args
    assert args[11] == '--cargo-metadata', args
    assert args[13:] == ['--list-type', 'binaries-only', '--message-format', 'json'], args
    assert read('--cargo-metadata') == metadata, 'missing or incorrect Cargo metadata'
    if os.environ.get('SELECT_LIST_FAIL'):
        sys.exit(62)
    print(json.dumps(binaries))
    # Returning from the compile owner must precede every test query.
    (root / 'finalized').write_text(args[12])
elif action == 'nextest-run':
    assert args[:3] == ['nextest', 'run', '--binaries-metadata'], args
    assert args[4] == '--cargo-metadata' and args[6:8] == ['--no-fail-fast', '-E'], args
    assert len(args) == 9, 'reuse must not rebuild or resolve Cargo inputs'
    assert (root / 'finalized').read_text() == args[5], 'query before compile finalization'
    assert read('--cargo-metadata') == metadata, 'incorrect reused Cargo metadata'
    assert read('--binaries-metadata') == binaries, 'incorrect reused binaries metadata'
else:
    raise AssertionError('unexpected Cargo invocation: ' + repr(args))
'''


@unittest.skipUnless(os.name == 'nt' or shutil.which('bash'), 'gate dispatcher needs Bash')
class WindowsSelection(unittest.TestCase):
    @contextlib.contextmanager
    def dispatcher(self, ending, source=None, **overrides):
        bash = (client.windows_tools(CLEAN.get('PATH', ''), os.environ['SystemRoot'])['bash']
                if os.name == 'nt' else shutil.which('bash'))
        with tempfile.TemporaryDirectory(prefix='windows-selection-') as directory:
            root = Path(directory)
            for name in ('scripts/lib', 'transport', 'out'):
                (root/name).mkdir(parents=True)
            (root/'scripts/check.sh').write_text(
                source if source is not None else (ROOT/'scripts/check.sh').read_text(), newline='\n')
            for name in ('lib/windows-tests.py', 'lib/windows-tests.txt'):
                shutil.copyfile(ROOT/'scripts'/name, root/'scripts'/name)
            (root/'scripts/lib/gate-key.sh').write_text('gate_tools_check() { :; }\n', newline='\n')
            (root/'scripts/cargo-guarded.sh').write_text('''#!/usr/bin/env bash
exec python "$SELECT_ROOT/transport/cargo.py" guarded "$@"
''', encoding='utf-8', newline='\n')
            (root/'scripts/cargo-guarded.sh').chmod(0o755)
            (root/'transport/cargo.py').write_text(FAKE_CARGO, encoding='utf-8')
            # A single last line hid the bug: native command substitution removed
            # its CRLF, while interior CRs survived and broke crate lookups.
            chosen = {'changed': ['nrese-engine', 'nrese-exec'],
                      'dependents': ['nrese-server', 'nrese-store']}
            (root/'scripts/lib/gate-selection.py').write_text(
                'import json,sys\nfrom pathlib import Path\n'
                'Path(sys.argv[2]).write_text('+repr(json.dumps(chosen))+')\n', encoding='utf-8')
            for crate in chosen['changed']+chosen['dependents']:
                folder = root/'crates'/crate
                (folder/'src').mkdir(parents=True)
                (folder/'Cargo.toml').write_text('[package]\nname = "'+crate+'"\n')
                (folder/'src/lib.rs').write_text('#[cfg(windows)]\n' if crate in chosen['changed'] else '')
            # Real Python writes directly into the actual dispatcher's pipe.
            (root/'transport/sitecustomize.py').write_text('''import os,sys
sys.stdout.reconfigure(newline='\\r\\n' if os.environ['SELECT_ENDING']=='CRLF' else '\\n')
''', encoding='utf-8')
            (root/'files').write_text('crates/nrese-engine/src/lib.rs\n')
            out = root/'out'
            env = {**CLEAN, 'PATH':str(Path(sys.executable).parent)+os.pathsep+str(Path(bash).parent)+os.pathsep+CLEAN['PATH'],
                'PYTHONPATH':(root/'transport').as_posix(), 'SELECT_ROOT':root.as_posix(),
                'SELECT_OUT':out.as_posix(), 'SELECT_ENDING':ending,
                'NRESE_GATE_INNER':'unit','NRESE_GATE_UNIT':'all','NRESE_GATE_PART':'windows',
                'FLEET_JOB':'selection-fixture','FLEET_STEP':'1','NRESE_BUILD_HOST':'local',
                'NRESE_FILES_FROM':(root/'files').as_posix(),
                'NRESE_GATE_SELECTION':(out/'selection.json').as_posix(),
                'NRESE_GATE_REQUIRED':(out/'required').as_posix(),
                'NRESE_GATE_STEPS':(out/'steps').as_posix(), **overrides}
            script = '''cargo() { python "$SELECT_ROOT/transport/cargo.py" cargo "$@"; }
export -f cargo
exec bash -x scripts/check.sh push
'''
            try:
                done = subprocess.run([bash,'-c',script],cwd=root,env=env,
                    stdin=subprocess.DEVNULL,capture_output=True,timeout=30)
                (out/'dispatcher.log').write_bytes(done.stdout+done.stderr)
                yield done, out
            finally:
                if os.environ.get('FLEET_OUT'):
                    shutil.copytree(out,Path(os.environ['FLEET_OUT'])/
                                    ('selection-fixture-'+root.name),dirs_exist_ok=True)

    def test_multiple_crates_survive_native_line_endings(self):
        for ending in ('LF', 'CRLF'):
            with self.subTest(ending=ending), self.dispatcher(ending) as (done, out):
                self.assertEqual(done.returncode,0,done.stdout+done.stderr)
                self.assertEqual((out/'required').read_text().splitlines(),
                    ['tools','Windows tests list','clippy (Windows)','tests (Windows)'])
                self.assertEqual((out/'events').read_text().splitlines(),
                    ['clippy', 'metadata', 'nextest-list', 'nextest-run'])
                listing = (out/'nextest-list.argv').read_bytes().split(b'\0')[:-1]
                tests = (out/'nextest-run.argv').read_bytes().split(b'\0')[:-1]
                self.assertEqual(tests[5], listing[12], 'run must reuse the compiled Cargo graph')
                self.assertIn(b'binary_id(nrese-engine::corpus_measure)',tests[8])
                self.assertIn(b'binary_id(nrese-engine::corpus_memory)',tests[8])
                self.assertIn(b'binary_id(nrese-server) & test(/^federation::body::tests::/)',tests[8])

    def test_failed_preparation_never_queries_tests(self):
        for variable, events in (('SELECT_CARGO_FAIL', ['clippy', 'metadata']),
                                 ('SELECT_LIST_FAIL', ['clippy', 'metadata', 'nextest-list'])):
            with self.subTest(failure=variable), self.dispatcher('LF', **{variable:'1'}) as (done, out):
                self.assertNotEqual(done.returncode, 0)
                self.assertEqual((out/'events').read_text().splitlines(), events)
                self.assertFalse((out/'nextest-run.argv').exists())
                self.assertFalse((out/'finalized').exists())

    def test_fixture_rejects_missing_metadata_queries_and_rebuilding(self):
        source = (ROOT/'scripts/check.sh').read_text()
        mutations = {
            'missing metadata': ('cargo metadata --locked --format-version 1 > "$metadata"', ': > "$metadata"'),
            'wrong reused metadata': ('nextest run --binaries-metadata "$binaries" --cargo-metadata "$metadata"',
                                      'nextest run --binaries-metadata "$binaries" --cargo-metadata "$binaries"'),
            'query before finalization': ('--list-type binaries-only', '--list-type full'),
            'rebuild during reuse': ('nextest run --binaries-metadata', 'nextest run --locked --binaries-metadata'),
        }
        for label, (before, after) in mutations.items():
            self.assertIn(before, source)
            with self.subTest(mutation=label), self.dispatcher('LF', source.replace(before, after)) as (done, out):
                self.assertNotEqual(done.returncode, 0, 'fixture accepted '+label)
                self.assertIn(b'Error', done.stderr)
