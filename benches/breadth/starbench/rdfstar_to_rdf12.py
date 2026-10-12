#!/usr/bin/env python3
"""N-Triples-star (RDF-star, as Jena 4 writes it) to RDF 1.2 N-Triples, keeping the answers
of SPARQL-star queries: stdin to stdout.

RDF-star quotes a triple as a term (`<< s p o >>`), one and the same term wherever it
occurs. RDF 1.2 states it through a reifier: `r rdf:reifies <<( s p o )>>`, and Turtle 1.2
and SPARQL 1.2 read `<< s p o >>` as a reifier. If every occurrence got a reifier of its own,
two annotations of one quoted triple would no longer join on it. So each distinct quoted
triple gets exactly one reifier, a blank node named after the triple's hash; nested quotes
are converted inside out. With one reifier per triple, `<< ?s ?p ?o >> :q ?v` patterns
(StarBench's only use of quoted triples) have the same solutions in both readings.

Each reifier's `rdf:reifies` triple is written where its triple first occurs (and again at
later occurrences: an RDF graph is a set, so repeats load as one statement)."""
from __future__ import annotations

import hashlib
import re
import sys

REIFIES = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies>"
TOKEN = re.compile(r'<<(?=\s)|>>|<[^<>\s]*>|_:\S+|"(?:[^"\\]|\\.)*"(?:@[A-Za-z0-9-]+|\^\^<[^<>\s]*>)?|\.')


def convert(line: str, out: list[str]) -> str | None:
    tokens = TOKEN.findall(line)
    if not tokens:
        return None
    position = 0

    def term() -> str:
        nonlocal position
        token = tokens[position]
        position += 1
        if token != "<<":
            return token
        s, p, o = term(), term(), term()
        if tokens[position] != ">>":
            raise ValueError(f"unclosed quoted triple in: {line.strip()}")
        position += 1
        label = "_:r" + hashlib.sha1(f"{s} {p} {o}".encode()).hexdigest()[:24]
        out.append(f"{label} {REIFIES} <<( {s} {p} {o} )>> .\n")
        return label

    s, p, o = term(), term(), term()
    if position >= len(tokens) or tokens[position] != ".":
        raise ValueError(f"not one triple: {line.strip()}")
    return f"{s} {p} {o} .\n"


def main() -> int:
    written = quoted = 0
    write = sys.stdout.write
    for line in sys.stdin:
        if not line.strip() or line.startswith("#"):
            continue
        extra: list[str] = []
        triple = convert(line, extra)
        if triple is None:
            continue
        for reifier in extra:
            write(reifier)
        write(triple)
        written += 1
        quoted += len(extra)
    print(f"{written} triples, {quoted} quoted-triple occurrences reified", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
