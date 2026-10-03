"""Compares the blank-node statements of two closures up to relabelling: each blank node is
named by a hash of its incident statements with IRIs or literals (refined once through
neighbouring blank nodes), then the renamed sets are compared.

    python compare_blank_nodes.py a.nt b.nt

Used for the Nemo closure checks (nemo/README.md): compare_inferred.py leaves blank-node
statements out, this compares them.
"""
import hashlib
import re
import sys
from collections import defaultdict

TERM = re.compile(r'(<[^>]*>|_:\S+|"(?:[^"\\]|\\.)*"(?:\^\^<[^>]*>|@\S+)?)')


def read(path):
    statements = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            if "_:" not in line:
                continue
            terms = TERM.findall(line)
            if len(terms) == 3:
                statements.append(tuple(terms))
    return statements


def names(statements):
    def blank(t):
        return t.startswith("_:")

    edges = defaultdict(list)
    for s, p, o in statements:
        if blank(s):
            edges[s].append(("out", p, o))
        if blank(o):
            edges[o].append(("in", p, s))
    name = {}
    for node, incident in edges.items():
        ground = sorted(e for e in incident if not blank(e[2]))
        name[node] = hashlib.sha256(repr(ground).encode()).hexdigest()[:16]
    refined = {}
    for node, incident in edges.items():
        signature = sorted((d, p, name.get(t, t)) for d, p, t in incident)
        refined[node] = "_:h" + hashlib.sha256(repr(signature).encode()).hexdigest()[:16]
    return refined


def canonical(path):
    statements = read(path)
    name = names(statements)
    if len(set(name.values())) != len(name):
        print(f"{path}: {len(name) - len(set(name.values()))} blank nodes share a signature")
    return {tuple(name.get(t, t) for t in st) for st in statements}, len(statements)


a, na = canonical(sys.argv[1])
b, nb = canonical(sys.argv[2])
print(f"{sys.argv[1]}: {na} statements, {len(a)} after renaming")
print(f"{sys.argv[2]}: {nb} statements, {len(b)} after renaming")
print(f"only in the first: {len(a - b)}; only in the second: {len(b - a)}")
for st in sorted(a - b)[:5]:
    print("  <", *st)
for st in sorted(b - a)[:5]:
    print("  >", *st)
