"""Push scope and last-attempt evidence. No scheduler policy or pass-file trust."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import subprocess

PUBLISHED_BASE = '37263eb909e4a8a50b5f9298a53b29ec7534fff2'
UNITS = {
    'rest': ['tools', 'fmt', 'clippy', 'W3C data', 'test', 'seeds', 'locks', 'miri',
             'doc tests (nrese-exec)', 'locks (side inputs)', 'suite contracts', 'names',
             'fmt (bench harness)', 'clippy (bench harness)', 'test (bench harness)'],
    'windows': ['tools', 'Windows tests list', 'clippy (Windows)', 'tests (Windows)',
                'console dependencies', 'console typecheck', 'console tests'],
}

class NoVerdict(RuntimeError):
    pass

def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':'),
                                     ensure_ascii=True).encode()).hexdigest()

def git(*args, env=None):
    p = subprocess.run(['git', *args], env=env, capture_output=True, timeout=60)
    if p.returncode:
        raise NoVerdict(f'git {args[0]} failed ({p.returncode}): {p.stderr.decode(errors="replace")}')
    return p.stdout

def clean_env(source=os.environ):
    # The hook may have GIT_DIR/GIT_INDEX_FILE for its source. None may redirect host git,
    # fixture git or an isolated source checkout. Do not inherit recursion/test-selection flags.
    return {k: v for k, v in source.items() if not k.startswith(('GIT_', 'NRESE_', 'FLEET_', 'NEXTEST_',
            'CARGO_BUILD_', 'CARGO_TARGET_', 'CARGO_PROFILE_', 'RUSTFLAGS', 'RUST_TEST_', 'MIRIFLAGS'))
            and k not in ('CARGO_ENCODED_RUSTFLAGS', 'RUSTUP_TOOLCHAIN', 'RUSTC', 'RUSTDOC',
                         'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'RUSTDOCFLAGS')}

def check_overrides(env=os.environ):
    unsupported = {'CARGO_ENCODED_RUSTFLAGS', 'RUSTC', 'RUSTDOC', 'RUSTC_WRAPPER',
                   'RUSTC_WORKSPACE_WRAPPER', 'RUSTDOCFLAGS', 'RUSTUP_TOOLCHAIN',
                   'CARGO_BUILD_TARGET', 'CARGO_BUILD_BUILD_DIR', 'CARGO_BUILD_TARGET_DIR',
                   'CARGO_BUILD_RUSTFLAGS', 'CARGO_BUILD_RUSTC',
                   'CARGO_BUILD_RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER'}
    bad = [k for k in env if k in unsupported or k.startswith('CARGO_PROFILE_') or
           (k.startswith('CARGO_TARGET_') and k != 'CARGO_TARGET_DIR')]
    if bad:
        raise NoVerdict('unsupported effective tool overrides: ' + ', '.join(sorted(bad)))
    if env.get('NRESE_W3C_OPTIONAL'):
        raise NoVerdict('ordinary push requires W3C data')
    if env.get('NRESE_GATE_COMMIT'):
        raise NoVerdict('ordinary push requires its own complete source/tooling snapshot')

def scope(base, head):
    # Base is an explicit published object; no upstream lookup or HEAD~1 approximation.
    if len(base) != 40 or any(c not in '0123456789abcdef' for c in base):
        raise NoVerdict('push base must be a full commit object id')
    git('merge-base', '--is-ancestor', base, head)
    raw = git('diff', '--name-only', '--no-renames', '-z', base, head, '--')
    files = raw.rstrip(b'\0').split(b'\0') if raw else []
    paths = []
    for name in files:
        text = name.decode('utf-8', errors='strict')
        if any(ord(c) < 32 or ord(c) == 127 for c in text):
            raise NoVerdict('control characters in paths cannot use the gate LF list')
        paths.append(text)
    paths = sorted(set(paths))
    data = ''.join(f'{p}\n' for p in paths).encode('utf-8')
    list_hash = hashlib.sha1(
        b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
    return paths, list_hash

def tooling(tree):
    # Whole effective gate source, not merely two pin files. The whole product tree remains
    # a separate identity too; this digest makes the tooling binding explicit in evidence.
    return hashlib.sha256(git('ls-tree', '-rz', tree, '--', 'scripts', '.githooks',
                              'benches/suite', '.cargo', '.config')).hexdigest()

def check_external_cargo_config(root, env=os.environ):
    root = Path(root).resolve()
    config_dirs = [parent/'.cargo' for parent in root.parents]
    config_dirs.append(Path(env.get('CARGO_HOME', str(Path.home()/'.cargo'))).expanduser())
    for directory in config_dirs:
        for name in ('config', 'config.toml'):
            if (directory/name).is_file():
                raise NoVerdict(f'external Cargo configuration is not a snapshot input: {directory/name}')

def artifact_key(tree, release, rustc_commit, flags, target, cpu):
    # HEAD plus exit-zero Cargo cannot identify an old executable (RSH probe).
    return digest({'tree': tree, 'release': release, 'rustc_commit': rustc_commit,
                   'flags': flags, 'target': target, 'cpu': cpu})

def required_for(request, part, selection):
    files = request['files']
    changed = selection['changed']
    if selection.get('_source') != {'request': request['key'], 'tree': request['tree'], 'files': files}:
        raise NoVerdict('crate selection belongs to another snapshot or scope')
    if not all(isinstance(selection[k], list) for k in ('changed', 'dependents', 'os_crates', 'windows_packages')):
        raise NoVerdict('invalid crate selection')
    if not set(selection['os_crates']) <= set(changed):
        raise NoVerdict('Windows lint scope differs')
    expected_windows = sorted(set(changed + selection['dependents']) & set(request['windows_crates']))
    if selection['windows_packages'] != expected_windows:
        raise NoVerdict('Windows test scope differs from the complete source inventory')
    if part == 'windows':
        names = ['tools', 'Windows tests list']
        if selection['os_crates']:
            names.append('clippy (Windows)')
        if expected_windows:
            names.append('tests (Windows)')
        if any(p.startswith('apps/nrese-console/') for p in files):
            names += ['console dependencies', 'console typecheck', 'console tests']
        return names
    names = ['tools', 'fmt']
    if changed:
        names += ['clippy', 'W3C data', 'test', 'seeds', 'locks']
    names += ['miri', 'doc tests (nrese-exec)']
    if any(p.rsplit('/', 1)[-1] in ('Cargo.toml', 'Cargo.lock') for p in files):
        names.append('locks (side inputs)')
    if any(p.startswith('benches/suite/') for p in files):
        names.append('suite contracts')
    if any(p.startswith(('benches/', 'scripts/')) for p in files):
        names.append('names')
    if any(p.startswith('benches/nrese-bench-harness/') for p in files):
        names += ['fmt (bench harness)', 'clippy (bench harness)', 'test (bench harness)']
    return names

def validate_unit(request, part, index, record, job):
    """Only a whole record of the latest fleet attempt for this precise unit counts."""
    spec = job['spec']
    unit = 'all'
    if index != 0:
        raise NoVerdict('a gate part has one complete fleet attempt')
    if job['state'] not in ('done', 'failed'):
        raise NoVerdict('fleet job has no terminal verdict')
    if spec['id'] != record.get('job') or spec['steps'] != request['jobs'][part]['steps']:
        raise NoVerdict('job or immutable step inventory differs')
    if spec.get('env') != request['jobs'][part]['env']:
        raise NoVerdict('effective job environment differs')
    if spec['inputs'] != request['jobs'][part]['inputs']:
        raise NoVerdict('source inputs differ')
    for field in ('requires', 'step_needs', 'owner', 'class', 'kind', 'yields'):
        if spec.get(field) != request['jobs'][part].get(field):
            raise NoVerdict('job admission or resource identity differs')
    attempts = [a for a in job['attempts'] if a['step'] == index]
    if not attempts:
        raise NoVerdict('unit has no fleet attempt')
    last = attempts[-1]
    ended = last.get('ended')
    failure = last.get('failure')
    if (ended is None or last.get('exit') not in (0, 1) or
        (failure is not None and not (last['exit'] == 1 and failure == {'kind': 'exit', 'code': 1}))):
        raise NoVerdict('latest attempt did not complete normally')
    for previous in attempts[:-1]:
        if previous.get('ended') is None or previous['ended'] > last['started']:
            raise NoVerdict('attempt bounds overlap or are missing')
    # Strict controller/host epoch bounds. Clock disagreement produces no verdict, never
    # a tolerant stale pass. A retry cannot reuse its preceding attempt's output.
    if not (last['started'] < record['began'] <= record['ended'] <= ended):
        raise NoVerdict('record is outside the latest attempt')
    expected = {'request': request['key'], 'tree': request['tree'], 'head': request['head'],
                'base': request['base'], 'list_hash': request['list_hash'], 'tooling': request['tooling'],
                'flags': request['flags'], 'rustc_commit': request['rustc_commit'],
                'release': request['release'], 'part': part, 'unit': unit, 'step': index,
                'fleet_host': last['host'], 'tracked_changes': '', 'snapshot_after': request['tree']}
    if any(record.get(k) != v for k, v in expected.items()):
        raise NoVerdict('source, selection, flags, toolchain, host or step identity differs')
    if record.get('code') != last['exit'] or type(record.get('code')) is not int:
        raise NoVerdict('fleet and workload exit disagree')
    required = record.get('required')
    if not isinstance(required, list) or len(set(required)) != len(required) or not set(required) <= set(UNITS[part]):
        raise NoVerdict('invalid required-step inventory')
    if required != required_for(request, part, record['selection']):
        raise NoVerdict('required gate steps missing or incorrectly selected')
    steps = record.get('steps')
    valid = (isinstance(steps, list) and [s.get('name') for s in steps] == required
             and all(type(s.get('code')) is int and s['code'] >= 0 for s in steps)
             and record['code'] == int(any(s['code'] != 0 for s in steps)))
    if not valid:
        raise NoVerdict('missing, skipped or contradictory unit evidence')
    if not record.get('attempt') or not record.get('cpu') or not record.get('target'):
        raise NoVerdict('incomplete runtime provenance')
    artifact = artifact_key(request['tree'], request['release'], request['rustc_commit'],
                            request['flags'], record['target'], record['cpu'])
    prefix = 'target-gate-win-' if part == 'windows' else 'target-gate-'
    if record.get('artifact_key') != artifact or record.get('artifact_target') != prefix + artifact:
        raise NoVerdict('workspace artifacts are not isolated by complete source/tool identity')
    if ('-windows-' if part == 'windows' else '-linux-') not in record['target']:
        raise NoVerdict('runtime target differs from the adapter OS')
    if part == 'windows' and (record.get('memory_cap_gb') != spec['step_needs'][index]['ram_gb'] or not record.get('memory_job')):
        raise NoVerdict('native Windows process tree lacks its requested JobObject cap')
    return record['code']

def validate_part(request, part, records, job):
    if len(records) != 1:
        raise NoVerdict('incomplete part')
    expected_required = records[0]['required']
    expected_selection = records[0]['selection']
    for i, record in enumerate(records):
        if record['required'] != expected_required or record['selection'] != expected_selection:
            raise NoVerdict('selection changed between units')
        if validate_unit(request, part, i, record, job):
            return 1
    if job['state'] != 'done' or job['next_step'] != 1:
        raise NoVerdict('fleet terminal state contradicts completed units')
    return 0

def atomic_json(path, data):
    path = Path(path)
    part = path.with_name(path.name + f'.{os.getpid()}.part')
    with part.open('w', encoding='utf-8', newline='\n') as f:
        json.dump(data, f, sort_keys=True, indent=2)
        f.write('\n')
        f.flush()
        os.fsync(f.fileno())
    os.replace(part, path)
