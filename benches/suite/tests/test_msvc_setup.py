"""Spec: selected native setup owner brief, 2026-10-23.

One Microsoft-selected environment controls C/C++ and binds Windows artifacts;
Rust flags/linker and Linux artifact keys stay unchanged. Fake setup only here.
"""
import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / 'scripts/lib'))
import gate_protocol as protocol


def load_setup():
    spec = importlib.util.spec_from_file_location('msvc_setup', ROOT / 'scripts/lib/msvc-setup.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class NativeIdentity(unittest.TestCase):
    def test_old_windows_record_without_selected_setup_is_not_evidence(self):
        from test_gate_fleet_push import fixture
        request, rows, job = fixture('windows')
        rows[0].pop('native_toolchain', None)
        with self.assertRaisesRegex(protocol.NoVerdict, 'native toolchain'):
            protocol.validate_part(request, 'windows', rows, job)

    def test_linux_identity_is_byte_identical_to_existing_contract(self):
        args = ['a' * 40, '1.98.1', 'b' * 40, '-C target-cpu=native',
                'x86_64-unknown-linux-gnu', 'cpu']
        old = dict(zip(('tree', 'release', 'rustc_commit', 'flags', 'target', 'cpu'), args))
        expected = hashlib.sha256(json.dumps(old, sort_keys=True, separators=(',', ':'),
                                             ensure_ascii=True).encode()).hexdigest()
        self.assertEqual(protocol.artifact_key(*args), expected)

    def test_native_tool_and_sdk_changes_invalidate_artifacts(self):
        from test_gate_fleet_push import fixture
        request, rows, job = fixture('windows')
        row = rows[0]
        native = row['native_toolchain']
        changes = [('selected', 'compiler_sha256', '9' * 64),
                   ('selected', 'helper_sha256', '8' * 64),
                   ('sdk', 'version', '10.0.changed'),
                   ('environment', 'INCLUDE', 'C:/another/include')]
        for section, key, value in changes:
            altered = copy.deepcopy(row)
            altered['native_toolchain'][section][key] = value
            with self.subTest(section=section, key=key), self.assertRaises(protocol.NoVerdict):
                protocol.validate_unit(request, 'windows', 0, altered, job)
        self.assertEqual(protocol.validate_part(request, 'windows', rows, job), 0)


class SelectedSetup(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='selected-msvc-fake-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.module = load_setup()
        self.install = self.root / 'Visual Studio with spaces'
        self.tools = self.install / 'VC/Tools/MSVC/14.fake'
        self.compiler = self.tools / 'bin/Hostx64/x64/cl.exe'
        self.helper = self.compiler.with_name('VCTIP.EXE')
        self.devcmd = self.install / 'Common7/Tools/VsDevCmd.bat'
        self.vswhere = self.root / 'Microsoft Visual Studio/Installer/vswhere.exe'
        for path in (self.compiler, self.helper, self.devcmd, self.vswhere):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(('fake ' + path.name).encode())
        self.devcmd.write_text('fake VSCMD_SKIP_SENDTELEMETRY supported\n')
        self.sdk = self.root / 'Windows Kits/10'
        for path in (self.tools / 'include', self.tools / 'lib/x64',
                     self.sdk / 'Include/10.fake/um', self.sdk / 'Include/10.fake/ucrt',
                     self.sdk / 'Lib/10.fake/um/x64', self.sdk / 'Lib/10.fake/ucrt/x64'):
            path.mkdir(parents=True, exist_ok=True)
        self.env = {'ProgramFiles(x86)': str(self.root), 'SystemRoot': str(self.root),
                    'PATH': 'original-path', 'RUSTFLAGS': '-C target-cpu=native'}
        self.developer = {'VSINSTALLDIR': str(self.install), 'VCTOOLSINSTALLDIR': str(self.tools),
            'VCTOOLSVERSION': '14.fake', 'VSCMD_ARG_HOST_ARCH': 'x64', 'VSCMD_ARG_TGT_ARCH': 'x64',
            'WINDOWSSDKDIR': str(self.sdk), 'WINDOWSSDKVERSION': '10.fake\\',
            'UNIVERSALCRTSDKDIR': str(self.sdk), 'UCRTVERSION': '10.fake',
            'INCLUDE': str(self.tools / 'include') + ';' + str(self.sdk / 'Include/10.fake/um'),
            'LIB': str(self.tools / 'lib/x64') + ';' + str(self.sdk / 'Lib/10.fake/um/x64'),
            'LIBPATH': str(self.tools / 'lib/x64'), 'PATH': str(self.compiler.parent) + ';original-path'}

    def fake_run(self, argv, **kwargs):
        if isinstance(argv, str):
            self.assertIn('-arch=x64 -host_arch=x64', argv)
            self.assertEqual(kwargs['env']['VSCMD_SKIP_SENDTELEMETRY'], '1')
            output = ''.join(f'{key}={value}\r\n' for key, value in
                             {**kwargs['env'], **self.developer}.items()).encode('utf-16-le')
        elif Path(argv[0]).name == 'vswhere.exe':
            output = (str(self.install) + '\r\n').encode('utf-8')
        elif Path(argv[0]).name in ('cygpath', 'cygpath.exe'):
            output = b'/selected/fake/bin:/original/path\n'
        else:
            self.fail('fixture must not execute native tools: ' + repr(argv))
        return subprocess.CompletedProcess(argv, 0, output, b'')

    def test_selected_environment_controls_compiler_without_changing_rust(self):
        before = dict(self.env)
        with patch.object(self.module.subprocess, 'run', side_effect=self.fake_run):
            receipt, exports = self.module.setup(self.env)
        self.assertEqual(self.env, before)
        selected = receipt['selected']
        self.assertEqual(Path(selected['compiler']), self.compiler)
        self.assertEqual(selected['compiler_sha256'], hashlib.sha256(self.compiler.read_bytes()).hexdigest())
        self.assertEqual(selected['helper_sha256'], hashlib.sha256(self.helper.read_bytes()).hexdigest())
        self.assertEqual(exports['CC_X86_64_PC_WINDOWS_MSVC'], str(self.compiler))
        self.assertEqual(exports['CXX'], str(self.compiler))
        self.assertEqual(exports['INCLUDE'], self.developer['INCLUDE'])
        self.assertEqual(exports['LIB'], self.developer['LIB'])
        self.assertFalse(any(k.startswith(('RUST', 'CARGO')) for k in exports))
        self.assertIn('linker = "rust-lld.exe"', (ROOT / '.cargo/config.toml').read_text())

    def test_conflicting_external_overrides_refuse_before_setup(self):
        for key in ('CC', 'CXX_x86_64-pc-windows-msvc', 'HOST_CC', 'CL', '_CL_',
                    'CFLAGS', 'INCLUDE', 'LIB', 'VCToolsInstallDir', 'NRESE_MSVC_COMPILER'):
            with self.subTest(key=key), patch.object(self.module.subprocess, 'run') as run:
                with self.assertRaisesRegex(ValueError, 'override'):
                    self.module.setup({**self.env, key: 'foreign'})
                run.assert_not_called()

    @unittest.skipUnless(os.name == 'nt', 'real Windows command-line quoting, fake batch only')
    def test_batch_path_with_spaces_survives_real_cmd(self):
        self.env['SystemRoot'] = os.environ['SystemRoot']
        self.devcmd.write_text('@echo off\nrem VSCMD_SKIP_SENDTELEMETRY\n' +
            ''.join(f'set "{key}={value}"\n' for key, value in self.developer.items()) + 'exit /b 0\n')
        real_run = subprocess.run

        def run(argv, **kwargs):
            if isinstance(argv, str) or Path(argv[0]).name == 'cmd.exe':
                return real_run(argv, **kwargs)
            return self.fake_run(argv, **kwargs)

        with patch.object(self.module.subprocess, 'run', side_effect=run):
            receipt, exports = self.module.setup(self.env)
        self.assertEqual(exports['CC'], str(self.compiler))
        self.assertEqual(receipt['sdk']['version'], '10.fake')

    def test_wrong_installation_compiler_or_missing_sdk_refuses(self):
        foreign = self.root / 'foreign'
        foreign.mkdir()
        (foreign / 'cl.exe').write_bytes(b'wrong tool')
        for key, value in (('VSINSTALLDIR', str(foreign)),
                           ('PATH', str(foreign) + ';' + self.developer['PATH']),
                           ('WINDOWSSDKVERSION', 'missing'), ('VSCMD_ARG_TGT_ARCH', 'x86')):
            with self.subTest(key=key), patch.dict(self.developer, {key: value}), \
                 patch.object(self.module.subprocess, 'run', side_effect=self.fake_run):
                with self.assertRaises(ValueError):
                    self.module.setup(self.env)


class GateSetupHandoff(unittest.TestCase):
    def test_ordinary_setup_exports_selected_env_before_existing_cargo_owner(self):
        # Run the real setup block with a fake Microsoft setup result. No native
        # developer environment, compiler, Cargo, Fleet, or repository is invoked.
        source = (ROOT / 'scripts/lib/gate-job.sh').read_text()
        marker = '# Native MSVC setup: '
        block = source[source.index(marker):source.index('if ! command -v rustup')] if marker in source else ''
        with tempfile.TemporaryDirectory(prefix='native-handoff-') as directory:
            env_file = Path(directory) / 'fake.env'
            env_file.write_bytes(b'NRESE_MSVC_COMPILER\0C:/selected/cl.exe\0INCLUDE\0C:/selected/include\0LIB\0C:/selected/lib\0')
            script = '''set -euo pipefail
export PATH="/usr/bin:$PATH"
part=windows; index=0; out=$PWD
infra() { echo "$*" >&2; exit 75; }
quiet() { "$@"; }
python() { cp fake.env "$3"; printf '{}' > "$2"; }
''' + block + '''
bash -c 'test "$NRESE_MSVC_COMPILER" = C:/selected/cl.exe && test "$INCLUDE" = C:/selected/include && test "$LIB" = C:/selected/lib && test "$RUSTFLAGS" = "-C target-cpu=native"'
'''
            bash = os.environ.get('NRESE_TEST_GIT_BASH', 'bash')
            result = subprocess.run([bash, '-c', script], cwd=directory,
                env={**os.environ, 'RUSTFLAGS': '-C target-cpu=native'},
                capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == '__main__':
    unittest.main()
