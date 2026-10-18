#!/usr/bin/env python3
"""N-Triples from a generator's Turtle-flavoured output: an object written in Turtle's
numeric or boolean shorthand (BSBM's `-s nt` writes prices as `2685.34e0`, 5,696,000 of
them in bsbm-100m.nt) becomes the typed literal Turtle gives it, with the same lexical
form (Turtle 1.1 §2.5.2: INTEGER xsd:integer, DECIMAL xsd:decimal, DOUBLE xsd:double;
`true`/`false` xsd:boolean). Every other line is copied byte for byte. A line whose object
is neither shorthand nor an IRI, literal or blank node ends the run (exit 2) with its number.

    python benches/sparql/nt-numbers.py IN.nt OUT.nt     (OUT `-`: only check and hash)

Prints the counts and both files' SHA-256 (the record of the conversion) to stderr. OUT is
written beside as OUT.partial and renamed when complete.
"""
from __future__ import annotations

import hashlib
import os
import re
import sys

XSD = b"http://www.w3.org/2001/XMLSchema#"
SHORTHAND = [
    (re.compile(rb"[+-]?[0-9]+"), b"integer"),
    (re.compile(rb"[+-]?[0-9]*\.[0-9]+"), b"decimal"),
    (re.compile(rb"[+-]?([0-9]+\.[0-9]*|\.[0-9]+|[0-9]+)[eE][+-]?[0-9]+"), b"double"),
    (re.compile(rb"true|false"), b"boolean"),
]
# What an N-Triples object ends with before " .": an IRI or a typed literal's datatype, a
# plain string, a language tag, a blank node label.
ENDS = re.compile(rb'(>|"|"@[A-Za-z]+(-[A-Za-z0-9]+)*|(^|\s)_:[A-Za-z0-9_][A-Za-z0-9_.-]*)$')


def convert_line(line: bytes) -> tuple[bytes, bytes | None]:
    """The line as N-Triples and the shorthand kind it had (None when copied)."""
    body = line.rstrip(b"\r\n")
    if not body or body.startswith(b"#"):
        return line, None
    if not body.endswith(b" ."):
        raise ValueError("not a triple")
    head = body[:-2].rstrip(b" \t")
    if ENDS.search(head):
        return line, None
    cut = max(head.rfind(b" "), head.rfind(b"\t"))
    token = head[cut + 1:]
    for pattern, kind in SHORTHAND:
        if pattern.fullmatch(token):
            return head[:cut + 1] + b'"' + token + b'"^^<' + XSD + kind + b"> .\n", kind
    raise ValueError(f"object {token[:40]!r} is neither N-Triples nor Turtle shorthand")


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    source, target = sys.argv[1], sys.argv[2]
    partial = None if target == "-" else target + ".partial"
    counts: dict[bytes, int] = {}
    lines = 0
    hash_in, hash_out = hashlib.sha256(), hashlib.sha256()
    with open(source, "rb") as inp, open(partial or os.devnull, "wb") as out:
        for lines, line in enumerate(inp, start=1):
            hash_in.update(line)
            try:
                converted, kind = convert_line(line)
            except ValueError as error:
                print(f"{source}:{lines}: {error}", file=sys.stderr)
                return 2
            if kind:
                counts[kind] = counts.get(kind, 0) + 1
            hash_out.update(converted)
            out.write(converted)
    if partial:
        os.replace(partial, target)
    rewritten = ", ".join(f"{n} {k.decode()}" for k, n in sorted(counts.items())) or "none"
    print(f"lines {lines}; shorthand rewritten: {rewritten}", file=sys.stderr)
    print(f"{hash_in.hexdigest()}  {source}", file=sys.stderr)
    print(f"{hash_out.hexdigest()}  {target}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
