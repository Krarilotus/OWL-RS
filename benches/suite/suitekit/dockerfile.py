"""The source commit a Docker build context declares: every `ARG NAME=` default of its Dockerfile, one
non-empty value. Its one reader: benches/sparql/images.sh builds an image at it (through
scripts/lib/ns.sh's ns_python), and suite.py coverage judges an image's recorded source commit
against it. It imports nothing of the suite, so a shell script runs it as a file.

    python dockerfile.py DOCKERFILE NAME    prints the value; exit 2, saying why, without one
"""
from __future__ import annotations

import sys
from pathlib import Path


def declared_source_commit(dockerfile: Path, name: str) -> str:
    """The one value of every line starting `ARG NAME=` (carriage returns removed); ValueError if
    the file can't be read, no line declares it, or the lines give an empty value or two values."""
    prefix = f"ARG {name}="
    try:
        text = Path(dockerfile).read_text(encoding="utf-8")
    except (OSError, ValueError) as error:
        raise ValueError(f"{dockerfile} can't be read: {error}") from error
    values = sorted({line[len(prefix):].replace("\r", "") for line in text.split("\n") if line.startswith(prefix)})
    if len(values) != 1 or not values[0]:
        raise ValueError(f"{dockerfile} declares {name} with no single non-empty default (ARG {name}=...): "
                         f"{', '.join(repr(v) for v in values) or 'none'}")
    return values[0]


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: dockerfile.py DOCKERFILE NAME")
    try:
        value = declared_source_commit(Path(sys.argv[1]), sys.argv[2])
    except ValueError as error:
        print(f"images.sh: {error}", file=sys.stderr)
        sys.exit(2)
    sys.stdout.buffer.write(f"{value}\n".encode())  # bytes: no \r on Windows for the shell to keep
