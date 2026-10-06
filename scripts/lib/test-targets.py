"""Which tests a gate tier runs, from what each crate declares about its own test binaries.

Each crate names its slow test binaries in its manifest (conformance suites, fuzz
campaigns: more than ~15 s):

    [package.metadata.nrese]
    slow-tests = ["tableau_w3c"]

Usage:
  python scripts/lib/test-targets.py exclude <crate>...
      the cargo-nextest filter that leaves out the slow binaries of these crates
      (empty when they declare none)
  python scripts/lib/test-targets.py fast <crate>...
      without cargo-nextest: one `cargo test` argument line per crate, its unit tests and
      every integration test binary it doesn't declare slow
"""
import json
import subprocess
import sys


def packages(names):
    metadata = json.loads(subprocess.run(
        ['cargo', 'metadata', '--no-deps', '--format-version', '1', '--offline'],
        capture_output=True, text=True, check=True).stdout)
    for package in metadata['packages']:
        if package['name'] not in names:
            continue
        tests = {t['name'] for t in package['targets'] if 'test' in t['kind']}
        slow = set(((package.get('metadata') or {}).get('nrese') or {}).get('slow-tests', []))
        if slow - tests:
            sys.exit(f"{package['name']}: slow-tests names no test binary: {sorted(slow - tests)}")
        yield package, slow


def main() -> None:
    mode, names = sys.argv[1], set(sys.argv[2:])
    if mode == 'exclude':
        slow = [f"binary_id({p['name']}::{t})" for p, s in packages(names) for t in sorted(s)]
        if slow:
            print('not (' + ' | '.join(slow) + ')')
    elif mode == 'fast':
        for package, slow in packages(names):
            flags = []
            for target in package['targets']:
                kinds = set(target['kind'])
                if kinds & {'lib', 'rlib', 'proc-macro'}:
                    flags.append('--lib')
                elif 'bin' in kinds:
                    flags += ['--bin', target['name']]
                elif 'test' in kinds and target['name'] not in slow:
                    flags += ['--test', target['name']]
            if flags:
                print(' '.join(['-p', package['name']] + flags))
    else:
        sys.exit('mode: exclude or fast')


if __name__ == '__main__':
    main()
