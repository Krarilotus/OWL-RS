#!/usr/bin/env python3
"""Fleet-only ordinary push, using immutable git inputs and the existing fleet CLI.

One submit/collect per invocation; a pending run returns 75. The hook never polls or
builds locally. A later invocation consumes the same owned jobs, including on first push.
"""
from __future__ import annotations

import base64
import functools
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid

from gate_protocol import (NoVerdict, PUBLISHED_BASE, UNITS, atomic_json, check_overrides,
                           clean_env, digest, git, scope, tooling, validate_part, validate_unit)

@functools.lru_cache(maxsize=4)
def windows_tools(path, system_root):
    # Reuse the shipped selected-Git resolver, including Git's hook exec-path.
    # Native CreateProcess searches System32 before PATH for a bare executable;
    # neither a child env PATH prepend nor shutil.which can fix that search.
    source = str(Path(__file__).with_name('git-bash.ps1')).replace("'", "''")
    script = f"""$ErrorActionPreference='Stop'
. '{source}'
$bash=Resolve-GitBash
$timeout=Join-Path (Split-Path -Parent $bash) 'timeout.exe'
if (-not (Test-Path -LiteralPath $timeout -PathType Leaf)) {{
    throw "native gate requires GNU timeout from selected Git: '$timeout'"
}}
@{{bash=$bash; timeout=$timeout}} | ConvertTo-Json -Compress
"""
    powershell = str(Path(system_root)/'System32/WindowsPowerShell/v1.0/powershell.exe')
    p = subprocess.run([powershell, '-NoProfile', '-NonInteractive', '-EncodedCommand',
                        base64.b64encode(script.encode('utf-16le')).decode()],
                       env={**os.environ, 'PATH': path}, stdin=subprocess.DEVNULL,
                       capture_output=True, text=True, timeout=20)
    if p.returncode:
        raise NoVerdict('native producer tools: ' + p.stderr[-2000:])
    tools = json.loads(p.stdout)
    if any(not Path(tools[name]).is_absolute() or not Path(tools[name]).is_file()
           for name in ('bash', 'timeout')):
        raise NoVerdict('native producer tools must be existing absolute selected-Git paths')
    return tools


def command(args, env, seconds=60):
    # GNU timeout owns the whole process group, escalating TERM. Do not wrap this in a
    # pipeline that discards transport/producer failures. Available in the existing Git Bash route.
    timeout = 'timeout'
    options = {}
    if os.name == 'nt':
        tools = windows_tools(env.get('PATH', os.environ.get('PATH', '')), os.environ['SystemRoot'])
        timeout = tools['timeout']
        if args[0] in ('bash', 'bash.exe'):
            args = [tools['bash'], *args[1:]]
        options['stdin'] = subprocess.DEVNULL  # Actual EOF even at a detached native boundary.
    p = subprocess.run([timeout, '--kill-after=10', str(seconds), *args], env=env,
                       capture_output=True, text=True, timeout=seconds + 20, **options)
    if p.returncode:
        raise NoVerdict(f'{args[0]} {args[1:2]} exit {p.returncode}: {p.stderr[-2000:]}')
    return p.stdout.rstrip('\r\n')

def build_request(env):
    check_overrides(env)
    e = clean_env(env)
    head = git('rev-parse', 'HEAD', env=e).decode().strip()
    tree = git('rev-parse', 'HEAD^{tree}', env=e).decode().strip()
    captured = command(['bash', 'scripts/lib/remote.sh', 'tree'], e)
    if captured != tree:
        raise NoVerdict('ordinary push requires the complete committed source snapshot')
    base = git('show', f'{head}:scripts/lib/push-base.txt').decode().strip()
    files, list_hash = scope(base, head)
    channel = re.search(r'^channel\s*=\s*"([^"]+)"', git('show', f'{head}:rust-toolchain.toml').decode(), re.M)
    info = command(['rustc', '-vV'], e)
    release = re.search(r'^release: (.+)$', info, re.M)
    commit = re.search(r'^commit-hash: ([0-9a-f]{40})$', info, re.M)
    if not channel or not release or not commit or release[1] != channel[1]:
        raise NoVerdict('cannot bind the pinned rustc release and full commit')
    e['RUSTFLAGS'] = env.get('RUSTFLAGS', '')
    e['NRESE_TARGET_CPU'] = env.get('NRESE_TARGET_CPU', 'native')
    flags = command(['bash', 'scripts/cargo-guarded.sh', '--print-rustflags'], e)
    windows_crates = sorted({line.split(':', 1)[0] for line in
        git('show', f'{head}:scripts/lib/windows-tests.txt').decode().splitlines()
        if line.strip() and not line.startswith('#')})
    request = {'schema': 1, 'tier': 'push', 'base': base, 'head': head, 'tree': tree,
               'list_hash': list_hash, 'files': files, 'tooling': tooling(tree), 'flags': flags,
               'raw_flags': e['RUSTFLAGS'], 'target_cpu': e['NRESE_TARGET_CPU'],
               'release': release[1], 'rustc_commit': commit[1], 'w3c_optional': False,
               'windows_crates': windows_crates}
    request['key'] = digest(request)
    return request

def make_spec(request, part, owner, repo, env, hosts=None):
    os_name = 'windows' if part == 'windows' else 'linux'
    argv = ['bash', 'scripts/lib/gate-job.sh', 'push', request['tree'], request['list_hash'],
            part, '0', request['key'], request['base'], request['flags'], request['rustc_commit'], '--', *request['files']]
    if part == 'windows':
        # Existing native Windows PowerShell -> installed PortableGit adapter, with a
        # whole-process-tree deadline through the memory helper's named JobObject.
        argv = ['powershell', '-NoProfile', '-NonInteractive', '-File', 'scripts/lib/gate-unit.ps1', json.dumps(argv[1:])]
    steps = [argv]
    # Keep the asks from the existing ordinary Linux and native Windows routes. Do not
    # understate their full compile/test envelope just to admit an otherwise too-small PC.
    heavy_ram = float(env.get('NRESE_GATE_RAM_GB', '24' if part == 'windows' else '12'))
    heavy_cores = int(env.get('NRESE_GATE_CORES', '16' if part == 'windows' else '8'))
    if not (heavy_ram > 0 and heavy_cores > 0):
        raise NoVerdict('gate resource asks must be positive')
    needs = [{'cores': heavy_cores, 'ram_gb': heavy_ram}]
    job_env = {'RUSTFLAGS': request['raw_flags'], 'NRESE_TARGET_CPU': request['target_cpu'],
               'NRESE_GATE_REQUEST': request['key'], 'NRESE_GATE_HEAD': request['head'],
               'NRESE_GATE_TOOLING': request['tooling'], 'NRESE_GATE_RELEASE': request['release'],
               'NRESE_GATE_NEEDS': json.dumps(needs, separators=(',', ':')),
               'NRESE_GATE_UNIT_S': str(int(env.get('NRESE_GATE_UNIT_S', '3000')))}
    if not 1 <= int(job_env['NRESE_GATE_UNIT_S']) <= 3300:
        raise NoVerdict('unit execution bound must leave setup/shutdown room inside 60 minutes')
    # Explicit list override: ordinary untimed gates need no optional test-role filter.
    spec = {'id': f'gate-push-{part}-{request["key"][:12]}-{uuid.uuid4().hex[:8]}',
            'group': 'gate', 'kind': 'owlrs.gate', 'owner': owner, 'class': 'shareable', 'yields': True,
            'priority': int(env.get('NRESE_GATE_PRIORITY', '600')),
            'priority_reason': env.get('NRESE_GATE_REASON', 'ordinary push gate, official fleet only'),
            'est_min': 60, 'steps': steps, 'step_needs': needs, 'env': job_env,
            'requires': {'os': os_name, 'roles': [], 'cores': heavy_cores, 'ram_gb': heavy_ram,
                         'hosts': [], 'has': [], 'disk_gb': 0, 'gpus': 0},
            'inputs': [{'name': 'checkout', 'source': {'git': repo, 'rev': request['head']}, 'size_gb': 0}]}
    if hosts is not None:
        if not hosts:
            raise NoVerdict('no other native host can confirm this failure')
        spec['requires']['hosts'] = hosts
        spec['pin_reason'] = 'confirmation must exclude the host of the recorded first failure'
    return spec

class Client:
    def __init__(self, env):
        self.env = env
        self.runenv = clean_env(env)
        self.owner = env.get('FLEET_OWNER', '')
        if not self.owner.startswith('t3:'):
            raise NoVerdict('FLEET_OWNER must name this submitting thread')
        self.runenv['FLEET_OWNER'] = self.owner
        self.bin = env.get('NRESE_FLEET_BIN', 'C:/Users/Johannes/Documents/fleet-scheduler/state/owlrs/bin/fleet.exe')

    def fleet(self, *args, seconds=60):
        return json.loads(command([self.bin, *args, '--json'], self.runenv, seconds))

    def submit(self, request, part, folder, hosts=None):
        spec = make_spec(request, part, self.owner, Path.cwd().as_posix(), self.env, hosts)
        request['jobs'][part] = spec
        atomic_json(folder / 'request.json', request)  # Persist intended id before submission.
        path = folder / f'{spec["id"]}.json'
        atomic_json(path, spec)
        reply = self.fleet('submit', str(path))
        if reply.get('submitted') != [spec['id']]:
            raise NoVerdict('submission not acknowledged; retained id must be inspected, never blindly resubmitted')
        print(f'fleet gate {part}: submitted {spec["id"]}', file=sys.stderr)

    def collect(self, request, part, folder):
        spec = request['jobs'][part]
        job = self.fleet('show', spec['id'])
        atomic_json(folder / f'{part}-fleet.json', job)
        if job['state'] not in ('done', 'failed'):
            raise NoVerdict(f'{spec["id"]}: {job["state"]}; no verdict')
        response = self.fleet('results', spec['id'], seconds=300)
        out = Path(response['path'])  # Installed CLI returns the output directory itself.
        records = []
        for i in range(1):
            record = json.loads((out / f'unit-{i}.json').read_text(encoding='utf-8'))
            # A failed job stops at the failing unit; preserve preceding and failed evidence.
            code = validate_unit(request, part, i, record, job)
            records.append(record)
            if code:
                atomic_json(folder / f'{part}-evidence.json', {'job': job, 'records': records})
                return 1, job, records
        code = validate_part(request, part, records, job)
        atomic_json(folder / f'{part}-evidence.json', {'job': job, 'records': records})
        return code, job, records

def main():
    env = dict(os.environ)
    request = build_request(env)  # Unsupported flags are rejected before any cache lookup.
    client = Client(env)
    common = Path(git('rev-parse', '--path-format=absolute', '--git-common-dir').decode().strip())
    folder = common / 'nrese-gate-requests' / request['key']
    folder.mkdir(parents=True, exist_ok=True)
    lock = folder / 'lock'
    try:
        lock.mkdir()
    except FileExistsError:
        raise NoVerdict(f'gate request is owned by another invocation: {lock}')
    try:
        state = folder / 'request.json'
        if state.exists():
            saved = json.loads(state.read_text(encoding='utf-8'))
            if {k: v for k, v in saved.items() if k not in ('jobs', 'owner', 'first_failures')} != request:
                raise NoVerdict('stored request identity differs')
            if saved['owner'] != client.owner:
                raise NoVerdict('this request belongs to another owner; inspect its result without taking its jobs')
            request = saved
        else:
            request.update({'jobs': {}, 'owner': client.owner, 'first_failures': {}})
        submitted = False
        for part in UNITS:
            if part not in request['jobs']:
                client.submit(request, part, folder)
                submitted = True
        if submitted:
            print(f'no verdict; immutable request {folder}. Reinvoke after owned fleet completion.', file=sys.stderr)
            return 75
        pending = []
        for part in UNITS:
            try:
                code, job, records = client.collect(request, part, folder)
            except NoVerdict as e:
                pending.append(str(e))
                continue
            if code:
                host = records[-1]['fleet_host']
                # Existing owner decision: a Khaleesi failure requires another actual host.
                # An official recheck is new owned work, with complete first-failure evidence.
                if host.lower() in ('khaleesi', 'khaleesi-wsl'):
                    first = request['first_failures'].get(part)
                    if first and first['job']['spec']['id'] != job['spec']['id']:
                        raise NoVerdict('confirmation is still on the unconfirmed physical host')
                    request['first_failures'][part] = {'job': job, 'records': records}
                    atomic_json(folder / 'request.json', request)
                    hosts = client.fleet('hosts')
                    if not isinstance(hosts, list):
                        raise NoVerdict('fleet host catalog shape is unknown; cannot broaden a confirmation')
                    eligible = confirmation_hosts(request['jobs'][part], hosts)
                    client.submit(request, part, folder, eligible)
                    return 75
                print(f'gate failed: {job["spec"]["id"]}; evidence {folder}', file=sys.stderr)
                return 1
        if pending:
            raise NoVerdict('; '.join(pending))
        if command(['bash', 'scripts/lib/remote.sh', 'tree'], clean_env(env)) != request['tree'] or git('rev-parse', 'HEAD').decode().strip() != request['head']:
            raise NoVerdict('source changed while evidence was collected')
        # Ordinary pass file remains the public breadcrumb. It is never sufficient by itself:
        # every subsequent hook repeats full validation against fleet's latest attempts.
        summary = f'push-fleet-v1 {request["key"]} | {folder}\n'
        with (common / 'nrese-gate-passed').open('a', encoding='utf-8') as f:
            f.write(summary)
        print(f'=== push: both official fleet parts verified for {request["head"]} base {request["base"]}')
        return 0
    finally:
        lock.rmdir()

def confirmation_hosts(spec, hosts):
    # Keep confirmation off the failed physical host, using the actual job's
    # role selector rather than a hidden recipe role. Fleet still owns admission.
    roles = set(spec['requires']['roles'])
    return [h['spec']['name'] for h in hosts
            if h['spec']['os'] == spec['requires']['os']
            and roles <= set(h['spec']['roles'])
            and h['spec']['name'].lower() not in ('khaleesi', 'khaleesi-wsl')]


def spec_os(part):
    return 'windows' if part == 'windows' else 'linux'

if __name__ == '__main__':
    try:
        sys.exit(main())
    except (NoVerdict, OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as e:
        print(f'push: no verdict: {e}', file=sys.stderr)
        sys.exit(75)
