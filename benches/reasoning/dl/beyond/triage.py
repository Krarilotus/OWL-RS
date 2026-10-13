"""Triage of the rule campaigns' disagreements by the references' known deviations from
SWRL's DL-safe semantics (the probes, dl-beyond-probes-1):

- HermiT: a rule with an empty head is ignored (SWRL: the head is false); a variable no
  class or property atom mentions, in body or head, isn't bound (both references).
- Openllet: a variable is bound only to individuals the assertions mention, not to a
  declared one; a variable only an equality or inequality atom mentions isn't bound; an
  inverse property in a property atom isn't read.

    python triage.py CASES NRESE REFS

A disagreement is explained where the reference NRESE differs from has a construct it
deviates on; the rest are listed.
"""
import re
import sys

cases = {}
for line in open(sys.argv[1], encoding="utf-8"):
    k, doc = line.rstrip("\n").split("\t", 1)
    cases[k] = doc
nrese = {}
for line in open(sys.argv[2], encoding="utf-8"):
    f = line.rstrip("\n").split("\t")
    nrese[f[0]] = f[1]
refs = {}
for line in open(sys.argv[3], encoding="utf-8"):
    f = line.rstrip("\n").split("\t")
    refs.setdefault(f[0], {})[f[1]] = f[2]


def decided(c):
    return c == "i" or (c.startswith("c") and len(c) == 11 and " " not in c)


def rules(doc):
    return re.findall(r"DLSafeRule\(Body\((.*?)\)Head\((.*?)\)\)", doc.replace(") Head(", ")Head("))


def features(doc):
    f = set()
    for body, head in rules(doc):
        if head.strip() == "":
            f.add("empty head")
        if "ObjectPropertyAtom(ObjectInverseOf" in body:
            f.add("inverse atom")
        variables = set(re.findall(r"Variable\((<[^>]*>)\)", body))
        bound = set()
        for atom in re.findall(r"(ClassAtom|ObjectPropertyAtom|DataPropertyAtom)\((.*?)\)(?= [A-Z]|$)", body):
            bound |= set(re.findall(r"Variable\((<[^>]*>)\)", atom[1]))
        if variables - bound:
            f.add("variable only in = or !=")
            # Not even the head's class or property atoms mention it (probes 6, 7).
            head_bound = set()
            for atom in re.findall(r"(ClassAtom|ObjectPropertyAtom|DataPropertyAtom)\((.*?)\)(?= [A-Z]|$)", head):
                head_bound |= set(re.findall(r"Variable\((<[^>]*>)\)", atom[1]))
            if variables - bound - head_bound:
                f.add("variable only in = or != in body and head")
    asserted = set(re.findall(r"Assertion\([^ ]+ (<[^>]*>)", doc)) | set(re.findall(r"Assertion\([^ ]+ <[^>]*> (<[^>]*>)", doc))
    declared = set(re.findall(r"Declaration\(NamedIndividual\((<[^>]*>)\)\)", doc))
    if declared - asserted and rules(doc):
        f.add("declared individual not asserted")
    return f


DEVIATES = {
    "hermit": {"empty head", "variable only in = or != in body and head"},
    "openllet": {"inverse atom", "variable only in = or !=", "declared individual not asserted"},
}
explained, unexplained = 0, []
for k, code in nrese.items():
    if not decided(code):
        continue
    r = {x: c for x, c in refs.get(k, {}).items() if decided(c)}
    differing = [x for x, c in r.items() if c != code]
    if not differing:
        continue
    f = features(cases[k])
    if all(DEVIATES.get(x, set()) & f for x in differing):
        explained += 1
    else:
        unexplained.append((k, code, r, sorted(f)))
print(f"disagreements explained by a known deviation: {explained}; unexplained: {len(unexplained)}")
for k, code, r, f in unexplained:
    print(f"  {k}: nrese {code} | {r} | features {f}")
