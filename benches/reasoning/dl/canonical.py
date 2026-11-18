#!/usr/bin/env python3
"""The canonical taxonomy (docs/design/owl2-dl.md §11) from a subsumption closure, so that
every system compares by one format and one hash with the reference runner's output.

    python canonical.py SIGNATURE CLOSURE OUT      # prints the SHA-256

- SIGNATURE: the classes, one IRI per line, or a reference `.tax` file (its `=` lines).
- CLOSURE: `sub<TAB>super` lines, every subsumption between named classes (as the EL
  classifier's example writes them), `C<TAB>owl:Nothing` for unsatisfiable classes.

The format: `= rep member` per class, the representative being the smallest IRI of its
equivalence class (`owl:Nothing` for unsatisfiable classes, `owl:Thing` for those
equivalent to it); `< sub super` per direct subsumption between representatives; sorted.
"""

import hashlib
import sys

THING = "http://www.w3.org/2002/07/owl#Thing"
NOTHING = "http://www.w3.org/2002/07/owl#Nothing"
ALIASES = {"owl:Thing": THING, "owl:Nothing": NOTHING}


def signature(path):
    classes = set()
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line:
            continue
        if line.startswith("= "):
            classes.add(line.split(" ")[2])
        elif not line.startswith("< "):
            classes.add(line)
    return classes | {THING, NOTHING}


def canonical(classes, pairs):
    """The canonical text of a closure `pairs` over `classes`."""
    sup = {c: {c} for c in classes}
    for a, b in pairs:
        a, b = ALIASES.get(a, a), ALIASES.get(b, b)
        for c in (a, b):
            sup.setdefault(c, {c})
        sup[a].add(b)
    # Unsatisfiable: below owl:Nothing; equivalent to owl:Thing: above it.
    for c in sup:
        if NOTHING in sup[c]:
            sup[c] = set(sup)
        sup[c].add(THING)
    sup[THING] |= {c for c in sup if THING in sup[c] and c in sup[THING]}
    rep = {}
    for c in sup:
        if NOTHING in sup[c]:
            rep[c] = NOTHING
            continue
        equal = {d for d in sup[c] if c in sup[d]}
        rep[c] = THING if THING in equal and c in sup[THING] else min(equal)
    lines = {f"= {rep[c]} {c}" for c in sup}
    for r in set(rep.values()):
        if r == NOTHING:
            continue
        above = {rep[d] for d in sup[r]} - {r, NOTHING}
        if r == THING:
            continue
        direct = {a for a in above if not any(a != b and a in {rep[d] for d in sup[b]} for b in above)}
        for a in direct or {THING}:
            lines.add(f"< {r} {a}")
    return "\n".join(sorted(lines)) + "\n"


def compare(reference_text, pairs):
    """NRESE's closure `pairs` against a reference `.tax` text: `"true"`, `"false"`, or
    `"signature-differs"` when the closure names classes the reference lacks. A reference
    over fewer classes read fewer axioms (an OWL API reader drops axioms about undeclared
    terms), so its taxonomy isn't the same ontology's: on 6 October 2026 the fast suite's
    el-classify counted the 2,000 undeclared defined classes as 4,526 wrong subsumptions."""
    classes = set()
    for line in reference_text.splitlines():
        if line.startswith("= "):
            classes.add(line.split(" ")[2])
    classes |= {THING, NOTHING}
    named = {ALIASES.get(c, c) for pair in pairs for c in pair}
    if named - classes:
        return "signature-differs"
    ours = set(canonical(classes, pairs).splitlines())
    return "true" if ours == set(reference_text.splitlines()) else "false"


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    classes = signature(sys.argv[1])
    pairs = [tuple(l.rstrip("\n").split("\t")[:2]) for l in open(sys.argv[2], encoding="utf-8") if "\t" in l]
    text = canonical(classes, pairs)
    open(sys.argv[3], "w", encoding="utf-8", newline="\n").write(text)
    print(hashlib.sha256(text.encode()).hexdigest())


if __name__ == "__main__":
    main()
