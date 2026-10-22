"""Native multiline Python output must preserve the selected Windows work."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from test_gate_fleet_push import CLEAN, ROOT, client


@unittest.skipUnless(os.name == 'nt' or shutil.which('bash'), 'gate dispatcher needs Bash')
class WindowsSelection(unittest.TestCase):
    def test_multiple_crates_survive_native_line_endings(self):
        bash = (client.windows_tools(CLEAN.get('PATH', ''), os.environ['SystemRoot'])['bash']
                if os.name == 'nt' else shutil.which('bash'))
        with tempfile.TemporaryDirectory(prefix='windows-selection-') as directory:
            root = Path(directory)
            for name in ('scripts/lib', 'transport', 'out'):
                (root/name).mkdir(parents=True)
            for name in ('check.sh', 'lib/windows-tests.py', 'lib/windows-tests.txt'):
                shutil.copyfile(ROOT/'scripts'/name, root/'scripts'/name)
            (root/'scripts/lib/gate-key.sh').write_text('gate_tools_check() { :; }\n', newline='\n')
            (root/'scripts/cargo-guarded.sh').write_text('''#!/usr/bin/env bash
printf '%s\\0' "$@" > "$SELECT_OUT/$1.argv"
''', encoding='utf-8', newline='\n')
            (root/'scripts/cargo-guarded.sh').chmod(0o755)
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
            try:
                for ending in ('LF', 'CRLF'):
                    with self.subTest(ending=ending):
                        out = root/'out'/ending; out.mkdir()
                        env = {**CLEAN, 'PATH':str(Path(sys.executable).parent)+os.pathsep+str(Path(bash).parent)+os.pathsep+CLEAN['PATH'],
                            'PYTHONPATH':(root/'transport').as_posix(),
                            'SELECT_OUT':out.as_posix(), 'SELECT_ENDING':ending,
                            'NRESE_GATE_INNER':'unit','NRESE_GATE_UNIT':'all','NRESE_GATE_PART':'windows',
                            'FLEET_JOB':'selection-fixture','FLEET_STEP':'1','NRESE_BUILD_HOST':'local',
                            'NRESE_FILES_FROM':(root/'files').as_posix(),
                            'NRESE_GATE_SELECTION':(out/'selection.json').as_posix(),
                            'NRESE_GATE_REQUIRED':(out/'required').as_posix(),
                            'NRESE_GATE_STEPS':(out/'steps').as_posix()}
                        done = subprocess.run([bash,'-x','scripts/check.sh','push'],cwd=root,env=env,
                            stdin=subprocess.DEVNULL,capture_output=True,timeout=30)
                        (out/'dispatcher.log').write_bytes(done.stdout+done.stderr)
                        self.assertEqual(done.returncode,0,done.stdout+done.stderr)
                        self.assertEqual((out/'required').read_text().splitlines(),
                            ['tools','Windows tests list','clippy (Windows)','tests (Windows)'])
                        lint = (out/'clippy.argv').read_bytes().split(b'\0')[:-1]
                        tests = (out/'nextest.argv').read_bytes().split(b'\0')[:-1]
                        self.assertEqual(lint,[b'clippy',b'--locked',b'-p',b'nrese-engine',
                            b'-p',b'nrese-exec',b'--all-targets'])
                        self.assertEqual(tests[:13],[b'nextest',b'run',b'--locked',b'--no-fail-fast',
                            b'-p',b'nrese-engine',b'-p',b'nrese-exec',b'-p',b'nrese-server',b'-p',b'nrese-store',b'-E'])
                        self.assertIn(b'binary_id(nrese-engine::corpus_measure)',tests[13])
                        self.assertIn(b'binary_id(nrese-engine::corpus_memory)',tests[13])
                        self.assertIn(b'binary_id(nrese-server) & test(/^federation::body::tests::/)',tests[13])
            finally:
                if os.environ.get('FLEET_OUT'):
                    shutil.copytree(root/'out',Path(os.environ['FLEET_OUT'])/
                                    ('selection-fixture-'+root.name),dirs_exist_ok=True)
