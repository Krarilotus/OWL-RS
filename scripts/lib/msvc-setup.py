"""Ordinary native gate: one installed Microsoft's developer environment.

Vswhere/VsDevCmd own discovery. The build owner owns compiler binding and helper
finalization. No installation, global environment, registry or Rust-linker edits.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys


def file_identity(path):
    path = Path(path).resolve(strict=True)
    return {'path': str(path), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def setup(environment):
    env = {key.upper(): value for key, value in environment.items()}
    # A developer prompt can append foreign include/lib paths or make VsDevCmd
    # reuse old initialization. Ordinary jobs start unselected; refuse overrides.
    exact = {'CC', 'CXX', 'HOST_CC', 'HOST_CXX', 'TARGET_CC', 'TARGET_CXX',
             'CL', '_CL_', 'LINK', '_LINK_', 'INCLUDE', 'LIB', 'LIBPATH',
             'EXTERNAL_INCLUDE', 'IFCPATH', 'VCINSTALLDIR', 'VSINSTALLDIR',
             'WINDOWSSDKDIR', 'WINDOWSSDKVERSION', 'UNIVERSALCRTSDKDIR', 'UCRTVERSION'}
    bad = [key for key in env if key in exact or key.startswith((
        'CC_', 'CXX_', 'CFLAGS', 'CXXFLAGS', 'HOST_CFLAGS', 'HOST_CXXFLAGS',
        'TARGET_CFLAGS', 'TARGET_CXXFLAGS', 'VCTOOLS', 'VSCMD_', '__VSCMD',
        '__VCVARS', 'NRESE_MSVC_'))]
    if bad:
        raise ValueError('unsupported native setup overrides: ' + ', '.join(sorted(bad)))
    vswhere = Path(env['PROGRAMFILES(X86)']) / 'Microsoft Visual Studio/Installer/vswhere.exe'
    selected = subprocess.run([str(vswhere), '-latest', '-products', '*', '-requires',
        'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath', '-utf8'],
        env=env, stdin=subprocess.DEVNULL, capture_output=True, check=True, timeout=60)
    lines = selected.stdout.decode('utf-8-sig').splitlines()
    if len(lines) != 1 or not Path(lines[0]).is_absolute():
        raise ValueError('vswhere did not select one installed Visual Studio')
    installation = Path(lines[0]).resolve(strict=True)
    devcmd = installation / 'Common7/Tools/VsDevCmd.bat'
    # CALL reparses percent expansion. Refuse paths it cannot represent literally.
    if any(c in str(devcmd) for c in '%!"\r\n'):
        raise ValueError('selected VsDevCmd path is not representable by cmd CALL')
    if 'VSCMD_SKIP_SENDTELEMETRY' not in devcmd.read_text(errors='replace'):
        raise ValueError('selected VsDevCmd lacks synchronous setup telemetry control')
    child = {**env, 'NRESE_GATE_VSDEVCMD': str(devcmd), 'VSCMD_SKIP_SENDTELEMETRY': '1'}
    # Only developer-shell telemetry is suppressed; compiler VCTIP remains enabled
    # and is finalized by the existing build owner after its pre-Cargo binding.
    # cmd parses its own command tail, not C-runtime backslash-escaped argv quotes.
    command = subprocess.list2cmdline([str(Path(env['SYSTEMROOT']) / 'System32/cmd.exe')])
    command += ' /d /u /s /c call "%NRESE_GATE_VSDEVCMD%" -no_logo -arch=x64 -host_arch=x64 >nul && set'
    result = subprocess.run(command,
        env=child, stdin=subprocess.DEVNULL, capture_output=True, check=True, timeout=60)
    developer = dict(line.split('=', 1) for line in result.stdout.decode('utf-16-le').splitlines()
                     if '=' in line and not line.startswith('='))
    developer = {key.upper(): value for key, value in developer.items()}
    if (Path(developer['VSINSTALLDIR']).resolve() != installation or
        developer.get('VSCMD_ARG_HOST_ARCH') != 'x64' or developer.get('VSCMD_ARG_TGT_ARCH') != 'x64'):
        raise ValueError('VsDevCmd returned a different installation or architecture')
    tools = Path(developer['VCTOOLSINSTALLDIR']).resolve(strict=True)
    if not tools.is_relative_to(installation / 'VC/Tools/MSVC'):
        raise ValueError('VsDevCmd returned tools outside its selected installation')
    compiler = (tools / 'bin/Hostx64/x64/cl.exe').resolve(strict=True)
    first = next((Path(p) / 'cl.exe' for p in developer['PATH'].split(';')
                  if p and (Path(p) / 'cl.exe').is_file()), None)
    if first is None or first.resolve() != compiler:
        raise ValueError('VsDevCmd PATH selects a different compiler')
    sdk = {'root': developer['WINDOWSSDKDIR'], 'version': developer['WINDOWSSDKVERSION'].rstrip('\\/'),
           'ucrt_root': developer['UNIVERSALCRTSDKDIR'], 'ucrt_version': developer['UCRTVERSION'].rstrip('\\/')}
    for root, version, component in ((sdk['root'], sdk['version'], 'um'),
                                      (sdk['ucrt_root'], sdk['ucrt_version'], 'ucrt')):
        if not version or Path(version).name != version or not all(p.is_dir() for p in (
            Path(root) / 'Include' / version / component, Path(root) / 'Lib' / version / component / 'x64')):
            raise ValueError('VsDevCmd returned an incomplete selected Windows SDK')
    # Reuse the build owner's selection contract, not a second compiler allowlist.
    spec = importlib.util.spec_from_file_location('msvc_finalize', Path(__file__).with_name('msvc-finalize.py'))
    owner = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(owner)
    pairs, overrides = owner.select_compiler(developer, str(compiler))
    names = ('PATH', 'INCLUDE', 'LIB', 'LIBPATH', 'EXTERNAL_INCLUDE', 'IFCPATH',
             'VSINSTALLDIR', 'VCINSTALLDIR', 'VCTOOLSINSTALLDIR', 'VCTOOLSVERSION',
             'WINDOWSSDKDIR', 'WINDOWSSDKVERSION', 'WINDOWSSDKLIBVERSION',
             'WINDOWSSDKBINPATH', 'WINDOWSSDKVERBINPATH', 'UNIVERSALCRTSDKDIR', 'UCRTVERSION')
    exports = {key: developer[key] for key in names if key in developer}
    exports.update(overrides, NRESE_MSVC_COMPILER=str(compiler), NRESE_MSVC_TARGET=pairs[0]['target'])
    receipt = {'version': 1, 'target': pairs[0]['target'], 'installation': str(installation),
               'vswhere': file_identity(vswhere), 'devcmd': file_identity(devcmd),
               'toolset_version': developer['VCTOOLSVERSION'], 'selected': pairs[0],
               'sdk': sdk, 'environment': dict(exports), 'developer_shell_telemetry': 'disabled for setup only'}
    # Bash must receive POSIX PATH; native subprocesses get the same paths converted
    # back by Git Bash. All other values retain Microsoft's native representation.
    converted = subprocess.run(['cygpath', '-u', '-p', exports['PATH']],
        env=env, stdin=subprocess.DEVNULL, capture_output=True, check=True, timeout=60)
    exports['PATH'] = converted.stdout.decode('utf-8').rstrip('\r\n')
    if not exports['PATH']:
        raise ValueError('empty native setup PATH conversion')
    return receipt, exports


def main():
    if len(sys.argv) != 3 or os.name != 'nt':
        raise ValueError('usage on native Windows: msvc-setup.py RECEIPT EXPORTS')
    receipt, exports = setup(os.environ)
    with Path(sys.argv[1]).open('w', encoding='utf-8', newline='\n') as stream:
        json.dump(receipt, stream, indent=2, sort_keys=True)
        stream.write('\n')
    with Path(sys.argv[2]).open('wb') as stream:
        for key, value in exports.items():
            stream.write(key.encode() + b'\0' + value.encode('utf-8') + b'\0')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print('msvc-setup: no verdict: ' + str(error), file=sys.stderr)
        sys.exit(75)
