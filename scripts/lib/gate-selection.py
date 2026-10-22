#!/usr/bin/env python3
"""Checked Cargo selection for the immutable push snapshot; no builds or tests."""
import json
import importlib.util
import os
from pathlib import Path
import subprocess
import sys

def run(*args):
    return subprocess.run(args, check=True, capture_output=True, text=True, timeout=120).stdout

def select(files, metadata, root, reverse):
    members = set(metadata['workspace_members'])
    packages = [p for p in metadata['packages'] if p['id'] in members]
    names = {p['name'] for p in packages}
    dirs = sorted(((Path(p['manifest_path']).parent.resolve().relative_to(root.resolve()).as_posix(), p['name'])
                   for p in packages), key=lambda p: len(p[0]), reverse=True)
    global_change = any(p in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml',
        'clippy.toml', '.clippy.toml', 'rustfmt.toml', '.rustfmt.toml', '.config/nextest.toml', 'deny.toml')
        or p.startswith('.cargo/') for p in files)
    changed = set()
    for f in files:
        match = next((n for d, n in dirs if f.startswith(d + '/')), None)
        if match:
            changed.add(match)
        elif f.startswith('crates/'):
            # Deleted/moved package ownership must not silently shrink selection. Root
            # manifest edits normally already select all; orphaned old paths do too.
            global_change = True
    if global_change:
        changed = names.copy()
    dependents = set()
    for name in sorted(changed):
        rows = reverse(name)
        if name not in rows or not rows <= names:
            raise ValueError('cargo tree returned an incomplete or foreign workspace selection')
        dependents |= rows
    return {'changed': sorted(changed), 'dependents': sorted(dependents - changed)}

def main():
    files = Path(sys.argv[1]).read_text(encoding='utf-8').splitlines()
    cache = Path(sys.argv[2])
    source = {'request': os.environ['NRESE_GATE_REQUEST'],
              'tree': run('git', 'rev-parse', 'HEAD^{tree}').strip(), 'files': files}
    metadata = json.loads(run('cargo', 'metadata', '--locked', '--no-deps', '--format-version', '1', '--offline'))
    root = Path.cwd()
    def reverse(name):
        text = run('cargo', 'tree', '-i', name, '--workspace', '--prefix', 'none', '-e', 'normal,build,dev', '--offline', '--locked')
        return {line.split()[0] for line in text.splitlines() if line.strip()}
    selected = select(files, metadata, root, reverse)
    loader = importlib.util.spec_from_file_location('windows_tests', root/'scripts/lib/windows-tests.py')
    module = importlib.util.module_from_spec(loader)
    loader.loader.exec_module(module)
    selected['os_crates'] = sorted({c for c, _, _ in module.sources(module.BY_OS) if c in selected['changed']})
    all_tests = set(selected['changed'] + selected['dependents'])
    selected['windows_packages'] = sorted({c for c, _, _ in module.entries() if c in all_tests})
    selected['_source'] = source
    from gate_protocol import atomic_json
    atomic_json(cache, selected)

if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, subprocess.SubprocessError) as e:
        print(f'no selection: {e}', file=sys.stderr)
        sys.exit(75)
