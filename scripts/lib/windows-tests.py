#!/usr/bin/env python3
"""The tests a gate runs on Windows (scripts/lib/windows-tests.txt), for scripts/check.sh.

  windows-tests.py check             every source file measured (nrese-exec's `measure`, a
                                     counting allocator) or with code by OS (cfg(windows),
                                     cfg(unix), target_os) is in a target a line covers;
                                     prints the ones that aren't, exit 1
  windows-tests.py packages CRATE…   the given crates that have lines
  windows-tests.py expr CRATE…       the nextest filter expression of their lines
  windows-tests.py os-crates CRATE…  the given crates with code by OS (linted on Windows too)

Run from the repository's root.
"""
import pathlib
import re
import sys

LIST = pathlib.Path("scripts/lib/windows-tests.txt")
MEASURED = re.compile(r"\bmeasure::|#\[global_allocator\]")
BY_OS = re.compile(r"\bcfg!?\([^\n]*\b(windows|unix|target_os)\b")


def entries():
    """(crate, target, filter or None) of each line."""
    out = []
    for line in LIST.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        crate, target, *rest = line.split(":", 2)
        out.append((crate, target, rest[0] if rest else None))
    return out


def crate_of(path):
    """The package name and directory of the crate `path` belongs to."""
    for parent in path.parents:
        manifest = parent / "Cargo.toml"
        if manifest.is_file():
            name = re.search(r'^name = "(.*)"', manifest.read_text(encoding="utf-8"), re.M)
            return (name.group(1) if name else None), parent
    return None, None


def target_of(path, root):
    """`lib`, a test target's name, or None (binaries, examples, benches)."""
    parts = path.relative_to(root).parts
    if parts[0] == "src":
        return None if parts[1:2] in (("main.rs",), ("bin",)) else "lib"
    if parts[0] == "tests":
        return pathlib.Path(parts[1]).stem
    return None


def sources(pattern):
    """(crate, target, path) of the workspace's source files matching `pattern`."""
    for path in sorted(pathlib.Path("crates").rglob("*.rs")):
        if "target" in path.parts or not pattern.search(path.read_text(encoding="utf-8", errors="replace")):
            continue
        crate, root = crate_of(path)
        target = crate and target_of(path, root)
        if target:
            yield crate, target, path


def check():
    covered = {(c, t) for c, t, _ in entries()}
    missing = [f"{p.as_posix()} ({c}:{t})" for c, t, p in sources(MEASURED) if (c, t) not in covered]
    missing += [f"{p.as_posix()} ({c}:{t})" for c, t, p in sources(BY_OS) if (c, t) not in covered]
    for line in sorted(set(missing)):
        print(f"not in {LIST.as_posix()}: {line}")
    return 1 if missing else 0


def main(args):
    if not args:
        print(__doc__, file=sys.stderr)
        return 2
    verb, crates = args[0], set(args[1:])
    if verb == "check":
        return check()
    chosen = [e for e in entries() if e[0] in crates]
    if verb == "packages":
        print(" ".join(sorted({c for c, _, _ in chosen})))
    elif verb == "expr":
        terms = []
        for crate, target, filt in chosen:
            binary = crate if target == "lib" else f"{crate}::{target}"
            terms.append(f"(binary_id({binary}){f' & {filt}' if filt else ''})")
        print(" | ".join(terms))
    elif verb == "os-crates":
        print(" ".join(sorted({c for c, _, _ in sources(BY_OS) if c in crates})))
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
