"""End only this build's pre-bound MSVC telemetry helper after synchronous Cargo.

This is compiler finalization, not the gate's worker policy. Unknown workers are
left alive and refuse completion; gate-unit still requires its JobObject empty.
"""
from __future__ import annotations

import argparse
import ctypes as C
from ctypes import wintypes as W
import hashlib
import json
import math
import os
from pathlib import Path
import re
import sys
import time


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def canonical(path):
    return os.path.normcase(os.path.realpath(path))


def select_compiler(environment, compiler=None):
    """One caller-selected native toolchain; installed alternatives grant no authority.

    CC/CXX target variants precede bare CC/CXX in cc-rs. Reject conflicting
    selectors rather than silently compiling with one installation and disposing
    another's helper. The caller applies these overrides before invoking Cargo.
    """
    # Windows environment names are case-insensitive, including when a caller
    # supplies an ordinary dict rather than os.environ.
    environment = {key.upper(): value for key, value in environment.items()}
    explicit = compiler or environment.get('NRESE_MSVC_COMPILER')
    if not explicit:
        return [], {}  # No selection: no disposal authority, strict worker refusal.
    compiler = Path(explicit).resolve(strict=True)
    if compiler.name.lower() != 'cl.exe':
        raise ValueError('explicit MSVC selection must name cl.exe')
    target = environment.get('NRESE_MSVC_TARGET', 'x86_64-pc-windows-msvc')
    if not re.fullmatch(r'(x86_64|i686|aarch64)-pc-windows-msvc', target):
        raise ValueError('explicit MSVC selection requires a native Windows target')
    keys = [kind + name for kind in ('', 'HOST_', 'TARGET_') for name in ('CC', 'CXX')]
    keys += [name + '_' + target.replace('-', '_').upper() for name in ('CC', 'CXX')]
    selectors = keys + [name + '_' + target.upper() for name in ('CC', 'CXX')]
    for key in selectors:
        value = environment.get(key)
        if value and canonical(value) != canonical(compiler):
            raise ValueError('compiler selection conflicts with ' + key)
    for key in ('INCLUDE', 'LIB'):
        directories = [p for p in environment.get(key, '').split(';') if p]
        if not directories or not all(Path(p).is_dir() for p in directories):
            raise ValueError('selected compiler requires its caller-qualified ' + key + ' directories')
    helper = compiler.parent / 'VCTIP.EXE'
    if helper.exists():
        helper = helper.resolve(strict=True)
        if helper.parent != compiler.parent:
            raise ValueError('MSVC helper resolves outside the selected compiler directory')
    else:
        helper = None
    pair = {'compiler': str(compiler), 'compiler_sha256': digest(compiler),
            'helper': str(helper) if helper else None,
            'helper_sha256': digest(helper) if helper else None, 'target': target,
            'selection': 'explicit caller selection, applied to Cargo compiler environment'}
    return [pair], {key: str(compiler) for key in keys}


class _Job:
    """Only the native primitives required for this compiler completion check."""
    def __init__(self, name):
        self.k = C.WinDLL('kernel32', use_last_error=True)
        definitions = {
            'OpenJobObjectW': (W.HANDLE, [W.DWORD, W.BOOL, W.LPCWSTR]),
            'OpenProcess': (W.HANDLE, [W.DWORD, W.BOOL, W.DWORD]),
            'CloseHandle': (W.BOOL, [W.HANDLE]),
            'GetProcessTimes': (W.BOOL, [W.HANDLE] + [C.POINTER(W.FILETIME)] * 4),
            'QueryFullProcessImageNameW': (W.BOOL, [W.HANDLE, W.DWORD, W.LPWSTR, C.POINTER(W.DWORD)]),
            'IsProcessInJob': (W.BOOL, [W.HANDLE, W.HANDLE, C.POINTER(W.BOOL)]),
            'QueryInformationJobObject': (W.BOOL, [W.HANDLE, C.c_int, C.c_void_p, W.DWORD, C.c_void_p]),
            'WaitForSingleObject': (W.DWORD, [W.HANDLE, W.DWORD]),
            'TerminateProcess': (W.BOOL, [W.HANDLE, W.UINT]),
        }
        for function, (result, args) in definitions.items():
            native = getattr(self.k, function)
            native.restype, native.argtypes = result, args
        self.handle = self.k.OpenJobObjectW(4, False, name)  # QUERY only, never terminate the job.
        if not self.handle:
            raise C.WinError(C.get_last_error())
        self.processes = {}

    def close(self):
        for handle in self.processes.values():
            self.k.CloseHandle(handle)
        self.k.CloseHandle(self.handle)

    def inspect(self, pid):
        if pid not in self.processes:
            # Hold the identity through verification/disposal; no later PID-only action.
            handle = self.k.OpenProcess(0x1000 | 0x100000 | 1, False, pid)
            if not handle:
                raise C.WinError(C.get_last_error())
            self.processes[pid] = handle
        handle = self.processes[pid]
        times = [W.FILETIME() for _ in range(4)]
        if not self.k.GetProcessTimes(handle, *(C.byref(t) for t in times)):
            raise C.WinError(C.get_last_error())
        member = W.BOOL()
        if not self.k.IsProcessInJob(handle, self.handle, C.byref(member)):
            raise C.WinError(C.get_last_error())
        length = W.DWORD(32768)  # Windows' maximum extended-length path.
        image = C.create_unicode_buffer(length.value)
        if not self.k.QueryFullProcessImageNameW(handle, 0, image, C.byref(length)):
            raise C.WinError(C.get_last_error())
        wait = self.k.WaitForSingleObject(handle, 0)
        if wait not in (0, 258):
            raise C.WinError(C.get_last_error())
        return {'pid': pid, 'creation_filetime': (times[0].dwHighDateTime << 32) | times[0].dwLowDateTime,
                'image': image.value, 'member': bool(member.value), 'live': wait == 258}

    def owners(self, name):
        match = re.fullmatch(r'nrese-build-(\d+)-(\d+)', name)
        if not match:
            raise ValueError('not an identity-named native build JobObject')
        root = self.inspect(int(match[1]))
        inspector = self.inspect(os.getpid())
        if root['creation_filetime'] != int(match[2]) or not all(x['live'] and x['member'] for x in (root, inspector)):
            raise ValueError('build caller or inspector identity/membership not verified')
        return root, inspector

    def pids(self, deadline):
        capacity = 16
        while time.monotonic() < deadline:
            buffer = C.create_string_buffer(8 + capacity * C.sizeof(C.c_size_t))
            if self.k.QueryInformationJobObject(self.handle, 3, buffer, len(buffer), None):
                count = W.DWORD.from_buffer(buffer, 4).value
                if count > capacity:
                    raise ValueError('incomplete native job member list')
                return list((C.c_size_t * count).from_buffer(buffer, 8))
            if C.get_last_error() != 234:  # ERROR_MORE_DATA
                raise C.WinError(C.get_last_error())
            capacity = max(capacity * 2, W.DWORD.from_buffer(buffer).value)
        raise TimeoutError('compiler finalization member-query deadline')


def compilation_only(arguments):
    """Recognize only owner commands that cannot execute test/application binaries."""
    args = list(arguments)
    if args[:1] in (['build'], ['check'], ['clippy']):
        return True
    listing = args[:2] == ['nextest', 'list']
    if not listing and args[:1] not in (['test'], ['bench']):
        return False
    # Values can themselves look like options (notably filtersets/config paths).
    # Unknown options grant no authority; never scan inside values or after --.
    values = {'-p', '--package', '--exclude', '--bin', '--example', '--test', '--bench',
              '-F', '--features', '--target', '--target-dir', '--manifest-path', '--config',
              '-Z', '--color', '--message-format'}
    flags = {'--workspace', '--all', '--lib', '--bins', '--examples', '--tests', '--benches',
             '--all-targets', '--all-features', '--no-default-features', '-r', '--release',
             '--frozen', '--locked', '--offline', '--ignore-rust-version', '--future-incompat-report'}
    if listing:
        values.update({'--build-jobs', '--cargo-profile', '--cargo-message-format', '--run-ignored',
            '--partition', '--platform-filter', '-E', '--filterset', '-T', '--list-type',
            '--show-progress', '--archive-file', '--archive-format', '--extract-to',
            '--cargo-metadata', '--workspace-remap', '--binaries-metadata', '--target-dir-remap',
            '--build-dir-remap', '--config-file', '--user-config-file', '--tool-config-file', '-P', '--profile'})
        flags.update({'--no-pager', '-v', '--verbose', '--cargo-quiet', '--cargo-verbose',
            '--ignore-default-filter', '--extract-overwrite', '--persist-extract-tempdir', '--override-version-check'})
    else:
        values.update({'-j', '--jobs', '--profile', '-m'})
        flags.update({'--no-run', '--no-fail-fast', '--doc', '-q', '--quiet', '-v', '-vv', '--verbose'})
    kinds, no_run, i = [], False, 2 if listing else 1
    while i < len(args):
        arg = args[i]
        if arg == '--':
            break
        key, equal, value = arg.partition('=')
        if key in values:
            if not equal:
                i += 1
                if i == len(args):
                    return False
                value = args[i]
            if key == '--list-type':
                kinds.append(value)
        elif arg in flags:
            no_run |= arg == '--no-run'
        elif arg.startswith('-'):
            return False
        i += 1
    return kinds == ['binaries-only'] if listing else no_run


def prepare(job_name, state_path, compiler=None, environment=None, compilation_only=False):
    environment = os.environ if environment is None else environment
    environment = {key.upper(): value for key, value in environment.items()}
    pairs, overrides = select_compiler(environment, compiler)
    effective = dict(environment, **overrides)
    bound_env = {key: value for key, value in effective.items() if key in overrides or
        key in ('INCLUDE', 'LIB', 'LIBPATH', 'CL', '_CL_', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS') or
        key.startswith(('CC_', 'CXX_', 'CFLAGS', 'CXXFLAGS', 'HOST_CFLAGS', 'HOST_CXXFLAGS', 'TARGET_CFLAGS', 'TARGET_CXXFLAGS')) or
        (key.startswith('CARGO_TARGET_') and key.endswith('_LINKER'))}
    job = _Job(job_name)
    try:
        root, inspector = job.owners(job_name)
        state = {'version': 1, 'job': job_name, 'root': root, 'prepared_by': inspector,
                 'pairs': pairs, 'compiler_environment': bound_env,
                 'compilation_only': compilation_only,
                 'prepared_filetime': time.time_ns() // 100 + 116444736000000000}
        raw = (json.dumps(state, indent=2) + '\n').encode()
        with Path(state_path).open('xb') as stream:
            stream.write(raw)
        return hashlib.sha256(raw).hexdigest(), overrides
    finally:
        job.close()


def bind_caller(state_path, binding, pid, caller_path):
    """Bind the finish subshell which explicitly supplies its own Win32 identity."""
    raw = Path(state_path).read_bytes()
    if hashlib.sha256(raw).hexdigest() != binding:
        raise ValueError('pre-build compiler binding changed')
    state = json.loads(raw)
    job = _Job(state['job'])
    try:
        root, inspector = job.owners(state['job'])
        caller = job.inspect(pid)
        if (root != state['root'] or not caller['live'] or not caller['member'] or
                pid in (root['pid'], inspector['pid']) or
                canonical(caller['image']) != canonical(root['image']) or
                not state['prepared_filetime'] <= caller['creation_filetime'] <= inspector['creation_filetime']):
            raise ValueError('explicit execution caller identity/membership not verified')
        record = {'binding': binding, 'job': state['job'], 'caller': caller, 'bound_by': inspector}
        raw = (json.dumps(record, indent=2) + '\n').encode()
        with Path(caller_path).open('xb') as stream:
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        return hashlib.sha256(raw).hexdigest()
    finally:
        job.close()


def finish(state_path, binding, receipt_path, cargo_exit, timeout=10, environment=None,
           caller_path=None, caller_binding=None):
    receipt = {'cargo_exit': cargo_exit, 'finalization_exit': 75, 'disposals': [], 'members': [], 'error': None}
    job = None
    created_receipt = False
    temporary = Path(str(receipt_path) + '.tmp')

    def save():
        with temporary.open('w', encoding='utf-8') as stream:
            stream.write(json.dumps(receipt, indent=2) + '\n')
            stream.flush()
            os.fsync(stream.fileno())
        temporary.replace(receipt_path)

    try:
        if not math.isfinite(timeout) or not 0 < timeout <= 10:
            raise ValueError('native compiler finalization needs a positive bounded timeout')
        deadline = time.monotonic() + timeout
        with Path(receipt_path).open('x', encoding='utf-8') as stream:
            stream.write(json.dumps(receipt))
        created_receipt = True
        raw = Path(state_path).read_bytes()
        if hashlib.sha256(raw).hexdigest() != binding:
            raise ValueError('pre-build compiler binding changed')
        state = json.loads(raw)
        receipt.update(job=state['job'], binding=binding, pairs=state['pairs'])
        receipt['compilation_only'] = state.get('compilation_only', False)
        environment = os.environ if environment is None else environment
        environment = {key.upper(): value for key, value in environment.items()}
        if len(state['pairs']) > 1 or any(environment.get(k) != v for k, v in state['compiler_environment'].items()):
            raise ValueError('selected compiler environment was not applied or changed')
        receipt['compiler_environment'] = state['compiler_environment']
        job = _Job(state['job'])
        root, inspector = job.owners(state['job'])
        if root != state['root']:
            raise ValueError('build caller identity changed')
        receipt.update(caller=root, inspector=inspector)
        owners = {root['pid'], inspector['pid']}
        if caller_path is not None or caller_binding is not None:
            raw_caller = Path(caller_path).read_bytes()
            if hashlib.sha256(raw_caller).hexdigest() != caller_binding:
                raise ValueError('execution caller binding changed')
            bound = json.loads(raw_caller)
            if bound['binding'] != binding or bound['job'] != state['job']:
                raise ValueError('execution caller belongs to a different build invocation')
            caller = job.inspect(bound['caller']['pid'])
            if (caller != bound['caller'] or not caller['member'] or not caller['live'] or
                    caller['pid'] in owners or canonical(caller['image']) != canonical(root['image']) or
                    not state['prepared_filetime'] <= caller['creation_filetime'] <= inspector['creation_filetime']):
                raise ValueError('execution caller identity/membership changed')
            # The exact held handle stays open through all membership/disposal checks.
            # No other process gains authority from its name, image or parent PID.
            receipt.update(execution_caller=caller, execution_caller_binding=caller_binding)
            owners.add(caller['pid'])
        pairs = {}
        for pair in state['pairs']:
            if digest(pair['compiler']) != pair['compiler_sha256'] or (pair['helper'] and digest(pair['helper']) != pair['helper_sha256']):
                raise ValueError('pre-bound MSVC installation bytes changed')
            if pair['helper'] and state.get('compilation_only') is True:
                pairs[canonical(pair['helper'])] = pair
        finished_at = time.time_ns() // 100 + 116444736000000000
        unknown = []
        for pid in job.pids(deadline):
            row = job.inspect(pid)
            receipt['members'].append(row)
            if not row['member']:
                raise ValueError('listed process is not a verified member')
            if not row['live'] or pid in owners:
                continue
            pair = pairs.get(canonical(row['image']))
            if pair is None or not state['prepared_filetime'] <= row['creation_filetime'] <= finished_at:
                unknown.append(row)
                continue
            # Recheck the same held handle immediately before the only destructive call.
            if job.inspect(pid) != row:
                raise ValueError('compiler helper identity/liveness changed during finalization')
            disposal = {'identity': row, 'matched_pair': pair, 'status': 'termination_intent', 'exit_code': 75}
            receipt['disposals'].append(disposal)
            save()  # No disposal without a durable, explicit intent record.
            left = int((deadline - time.monotonic()) * 1000)
            if left <= 0 or not job.k.TerminateProcess(job.processes[pid], 75):
                raise RuntimeError('owned MSVC helper termination failed or exceeded deadline')
            if job.k.WaitForSingleObject(job.processes[pid], left) != 0:
                raise TimeoutError('owned MSVC helper did not stop within finalization deadline')
            disposal['status'] = 'disposed_and_handle_signaled'
            save()
        remaining = [job.inspect(pid) for pid in job.pids(deadline)]
        receipt['remaining'] = remaining
        if 'execution_caller' in receipt and job.inspect(receipt['execution_caller']['pid']) != receipt['execution_caller']:
            raise ValueError('execution caller changed before finalization completed')
        if unknown or any(not row['member'] or (row['live'] and row['pid'] not in owners) for row in remaining):
            raise ValueError('unknown build/test workers remain; preserved for strict outer refusal')
        receipt['finalization_exit'] = 0
    except Exception as error:
        receipt['error'] = repr(error)
    finally:
        if job is not None:
            job.close()
        try:
            if created_receipt:
                save()
        except OSError as error:
            receipt['finalization_exit'] = 75
            receipt['error'] = 'completion receipt failed: ' + repr(error)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    before = sub.add_parser('prepare')
    before.add_argument('--job', required=True)
    before.add_argument('--state', type=Path, required=True)
    before.add_argument('--compiler')
    before.add_argument('--cargo', nargs=argparse.REMAINDER, default=[])
    caller = sub.add_parser('bind-caller')
    caller.add_argument('--state', type=Path, required=True)
    caller.add_argument('--binding', required=True)
    caller.add_argument('--pid', type=int, required=True)
    caller.add_argument('--record', type=Path, required=True)
    after = sub.add_parser('finish')
    after.add_argument('--state', type=Path, required=True)
    after.add_argument('--binding', required=True)
    after.add_argument('--receipt', type=Path, required=True)
    after.add_argument('--cargo-exit', type=int, required=True)
    after.add_argument('--timeout', type=float, default=10)
    after.add_argument('--caller-record', type=Path)
    after.add_argument('--caller-binding')
    args = parser.parse_args()
    if args.action == 'prepare':
        binding, overrides = prepare(args.job, args.state, args.compiler,
                                     compilation_only=compilation_only(args.cargo))
        # Data only: Bash reads NUL-delimited keys/values without eval or quoting.
        with Path(str(args.state) + '.env').open('xb') as stream:
            for key, value in overrides.items():
                stream.write((key + '\0' + value + '\0').encode())
        print(binding)
        return 0
    if args.action == 'bind-caller':
        print(bind_caller(args.state, args.binding, args.pid, args.record))
        return 0
    receipt = finish(args.state, args.binding, args.receipt, args.cargo_exit, args.timeout,
                     caller_path=args.caller_record, caller_binding=args.caller_binding)
    print(json.dumps({'cargo_exit': receipt['cargo_exit'], 'finalization_exit': receipt['finalization_exit'],
        'disposed': len(receipt['disposals']), 'receipt': str(args.receipt), 'error': receipt['error']}), file=sys.stderr)
    return receipt['finalization_exit']


if __name__ == '__main__':
    try:
        sys.exit(main())
    except Exception as error:
        print('native compiler finalization refused: ' + repr(error), file=sys.stderr)
        sys.exit(75)
