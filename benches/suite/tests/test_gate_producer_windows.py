"""Real native producer commands; private Git fixtures never submit or compile."""
import contextlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from test_gate_fleet_push import CLEAN, ROOT, client, protocol


@unittest.skipUnless(os.name == 'nt', 'native Windows producer executable search')
class NativeProducer(unittest.TestCase):
    def setUp(self):
        self.env = {**CLEAN, 'RUSTUP_AUTO_INSTALL':'0'}

    def test_stdout_and_eof_use_real_git_bash_and_gnu_timeout(self):
        value = 'spaces and "quotes"'
        actual = client.command(['bash', '-c', 'read -r line && exit 9; printf "%s\\n" "$1"',
                                 'producer-fixture', value], self.env)
        self.assertEqual(actual, value)
        actual = client.command([sys.executable, '-c',
            'import subprocess,sys; assert sys.stdin.read()==""; subprocess.check_call([sys.executable,"-c","pass"]); print("native EOF")'], self.env)
        self.assertEqual(actual, 'native EOF')

    def test_gnu_deadline_kills_term_ignoring_bash_tree(self):
        with tempfile.TemporaryDirectory(prefix='producer-deadline-') as directory:
            marker = Path(directory)/'bash-pid'
            code = '''import os,sys
sys.path.insert(0,os.environ['PRODUCER_LIB'])
import importlib.util
s=importlib.util.spec_from_file_location('producer',os.path.join(os.environ['PRODUCER_LIB'],'gate-client.py'))
m=importlib.util.module_from_spec(s); s.loader.exec_module(m)
try:
 m.command(['bash','-c','trap "" TERM; cat /proc/$$/winpid > "$(cygpath -u "$MARKER")"; while :; do sleep 1; done'],dict(os.environ),seconds=1)
except m.NoVerdict as e:
 print(str(e)); sys.exit(75)
sys.exit(0)
'''
            env = {**self.env, 'PRODUCER_LIB':str(ROOT/'scripts/lib'), 'MARKER':str(marker)}
            log = Path(directory)/'command.log'
            started = time.monotonic()
            result = None; pid = None; alive = None
            try:
                with log.open('wb') as stream:
                    child = subprocess.Popen([sys.executable,'-u','-c',code],env=env,
                        stdin=subprocess.DEVNULL,stdout=stream,stderr=subprocess.STDOUT)
                    try:
                        result = child.wait(timeout=35)
                    except subprocess.TimeoutExpired:
                        subprocess.run(['taskkill','/PID',str(child.pid),'/T','/F'],stdin=subprocess.DEVNULL,
                                       stdout=stream,stderr=subprocess.STDOUT,timeout=10)
                        child.wait(timeout=10)
                        self.fail('producer deadline failed: '+log.read_text(errors='replace'))
                self.assertEqual(result,75,log.read_text(errors='replace'))
                self.assertLess(time.monotonic()-started,25,'GNU process-group escalation did not finish')
                self.assertTrue(marker.exists(),log.read_text(errors='replace'))
            finally:
                if marker.exists():
                    pid = int(marker.read_text())
                    alive = subprocess.run(['powershell','-NoProfile','-NonInteractive','-Command',
                        f'if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 1 }}'],
                        stdin=subprocess.DEVNULL,capture_output=True,timeout=10).returncode
                    if alive:
                        subprocess.run(['taskkill','/PID',str(pid),'/T','/F'],stdin=subprocess.DEVNULL,
                                       capture_output=True,timeout=10)
                if os.environ.get('FLEET_OUT'):
                    Path(os.environ['FLEET_OUT'],'producer-deadline.json').write_text(json.dumps(
                        {'exit':result,'real_bash_pid':pid,'alive_before_cleanup':alive,
                         'seconds':time.monotonic()-started,'output':log.read_text(errors='replace')},indent=2))
                if alive is not None:
                    self.assertEqual(alive,0,'deadline left its recorded real Bash alive')

    def test_build_request_uses_real_snapshot_compiler_and_effective_flags(self):
        # The published base below is a full private fixture commit, not a product
        # push base or a submitted request. No source checkout/index is touched.
        with tempfile.TemporaryDirectory(prefix='producer-request-') as directory:
            repo = Path(directory)/'repo'; repo.mkdir()
            for relative in ('scripts/lib/remote.sh','scripts/cargo-guarded.sh','scripts/lib/disk.sh',
                             'scripts/lib/target-cpu.sh','scripts/lib/windows-tests.txt','rust-toolchain.toml'):
                target = repo/relative; target.parent.mkdir(parents=True,exist_ok=True)
                target.write_bytes((ROOT/relative).read_bytes())
            hooks = Path(directory)/'empty-hooks'; hooks.mkdir()
            def git(*args):
                return subprocess.run(['git','-C',str(repo),'-c','core.hooksPath='+str(hooks),
                    '-c','commit.gpgSign=false','-c','user.name=fixture','-c','user.email=fixture@localhost',*args],
                    env=self.env,stdin=subprocess.DEVNULL,check=True,capture_output=True,text=True,timeout=30).stdout.strip()
            git('init','-q'); git('add','-A'); git('commit','-qm','published fixture')
            base = git('rev-parse','HEAD')
            (repo/'scripts/lib/push-base.txt').write_text(base+'\n')
            (repo/'change.txt').write_text('complete snapshot\n')
            git('add','-A'); git('commit','-qm','candidate fixture')
            head = git('rev-parse','HEAD'); tree = git('rev-parse','HEAD^{tree}')
            with contextlib.chdir(repo), patch.dict(os.environ,self.env,clear=True):
                request = client.build_request({**self.env,'NRESE_TARGET_CPU':'portable',
                                                'RUSTFLAGS':'-C debuginfo=1'})
                self.assertEqual((request['head'],request['tree'],request['base']),(head,tree,base))
                self.assertEqual(request['flags'],'-C debuginfo=1')
                self.assertEqual(request['raw_flags'],'-C debuginfo=1')
                self.assertEqual(request['files'],['change.txt','scripts/lib/push-base.txt'])
                self.assertRegex(request['rustc_commit'],r'^[0-9a-f]{40}$')
                self.assertEqual(request['key'],protocol.digest({k:v for k,v in request.items() if k!='key'}))
                self.assertFalse((repo/'.git/nrese-gate-requests').exists())
                self.assertFalse((repo/'target').exists())
                (repo/'untracked.txt').write_text('must prevent submission\n')
                with self.assertRaisesRegex(protocol.NoVerdict,'complete committed source snapshot'):
                    client.build_request(self.env)
            if os.environ.get('FLEET_OUT'):
                Path(os.environ['FLEET_OUT'],'producer-request.json').write_text(json.dumps(
                    {'fixture_request':request,'synthetic_private_base':True,
                     'dirty_snapshot_refused':True,'submitted_jobs':0,'compiled':False},indent=2))
