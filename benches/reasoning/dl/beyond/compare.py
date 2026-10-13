"""NRESE's answers against the references' on cases of the Scope runner's codes.

    python compare.py NRESE_TSV REFS_TSV [--show N]

NRESE: index, code, certified, millis; references: index, reasoner, code, millis. A code is
`i`, `c` and ten bits, or a letter with why (undecided or failed). Prints the tally and the
first N cases of each kind of difference.
"""
import collections
import sys


def decided(code):
    return code == "i" or (code.startswith("c") and len(code) == 11 and " " not in code)


def main():
    nrese_path, refs_path = sys.argv[1], sys.argv[2]
    show = int(sys.argv[sys.argv.index("--show") + 1]) if "--show" in sys.argv else 5
    nrese = {}
    for line in open(nrese_path, encoding="utf-8"):
        f = line.rstrip("\n").split("\t")
        nrese[f[0]] = f[1]
    refs = collections.defaultdict(dict)
    for line in open(refs_path, encoding="utf-8"):
        f = line.rstrip("\n").split("\t")
        refs[f[0]][f[1]] = f[2]
    tally = collections.Counter()
    examples = collections.defaultdict(list)
    for k, code in nrese.items():
        answers = {r: c for r, c in refs.get(k, {}).items() if decided(c)}
        values = set(answers.values())
        if not decided(code):
            kind = "nrese undecided, references " + ("agree" if len(values) == 1 else "differ" if values else "none")
        elif not answers:
            kind = "no reference answered"
        elif len(values) > 1:
            kind = "references differ, NRESE with " + ",".join(sorted(r for r, c in answers.items() if c == code) or ["none"])
        elif code in values:
            kind = "agree"
        else:
            kind = "NRESE differs from " + ",".join(sorted(answers))
        tally[kind] += 1
        if len(examples[kind]) < show:
            examples[kind].append((k, code, refs.get(k, {})))
    for kind, n in tally.most_common():
        print(f"{n:6}  {kind}")
    for kind, cases in examples.items():
        if kind == "agree":
            continue
        print(f"\n== {kind}")
        for k, code, r in cases:
            print(f"  {k}: nrese {code[:80]} | " + " | ".join(f"{x} {c[:80]}" for x, c in r.items()))


main()
