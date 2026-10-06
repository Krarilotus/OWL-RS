"""Removes the stale artifacts of a build directory: in each profile's `deps`, cargo keeps
every build of a crate target it ever made (`libfoo-<hash>.rlib`, `it-<hash>.exe`, ...),
one per hash, and never removes the old ones. This keeps the newest build of each target
and deletes the rest, so a directory over its budget shrinks without losing what the next
build reuses. Prints the bytes it freed.

A target is told apart by the first source file its dep-info (`<name>-<hash>.d`) lists,
not by its name: several crates have a test binary called `it`, and two versions of one
library share its name. Builds without dep-info are kept.

Usage: python scripts/lib/prune-stale.py <target dir>
"""
import re
import sys
from collections import defaultdict
from pathlib import Path

ARTIFACT = re.compile(r'^(?P<build>.+-[0-9a-f]{16})(?P<kind>(\.[A-Za-z0-9_]+)*)$')


def source_of(dep_info: Path) -> str | None:
    """The first source file a dep-info file lists: the target's root (`src/lib.rs`, a
    test's `main.rs`), with the crate's path and version in it."""
    try:
        with dep_info.open(encoding='utf-8', errors='replace') as f:
            line = f.readline()
    except OSError:
        return None
    _, _, deps = line.partition(': ')
    first = deps.split(' ', 1)[0].strip()
    return first or None


def prune(deps: Path) -> int:
    builds: dict[str, list[Path]] = defaultdict(list)
    for entry in deps.iterdir():
        m = ARTIFACT.match(entry.name)
        if m and entry.is_file():
            builds[m.group('build')].append(entry)
    # Builds grouped by target; the newest dep-info decides which build is current.
    targets: dict[str, list[tuple[float, str]]] = defaultdict(list)
    for build, files in builds.items():
        info = deps / f'{build}.d'
        source = source_of(info) if info.is_file() else None
        if source:
            stem = build.rsplit('-', 1)[0]
            targets[f'{stem}|{source}'].append((info.stat().st_mtime, build))
    freed = 0
    for versions in targets.values():
        versions.sort(reverse=True)
        for _, build in versions[1:]:
            for path in builds[build]:
                try:
                    size = path.stat().st_size
                    path.unlink()
                    freed += size
                except OSError:
                    pass  # in use by a running build or test: kept
    return freed


def main() -> None:
    target = Path(sys.argv[1])
    freed = 0
    if target.is_dir():
        for profile in target.iterdir():
            deps = profile / 'deps'
            if deps.is_dir():
                freed += prune(deps)
    print(freed)


if __name__ == '__main__':
    main()
