"""Random OWL 2 EL ontologies; compares NRESE's classification with ELK's (via ROBOT).

    python el_check.py generate N DIR      writes DIR/case-K.ttl (N-Triples syntax)
    python el_check.py compare DIR         compares DIR/case-K.nrese.tsv with DIR/case-K.elk.tsv
"""
import random
import sys
from pathlib import Path

EX = "http://example.com/"
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
RDFS = "http://www.w3.org/2000/01/rdf-schema#"
OWL = "http://www.w3.org/2002/07/owl#"


def generate(seed: int) -> str:
    rnd = random.Random(seed)
    classes = [f"<{EX}C{i}>" for i in range(rnd.randint(8, 25))]
    roles = [f"<{EX}r{i}>" for i in range(rnd.randint(2, 5))]
    lines = []
    blank = [0]

    def fresh():
        blank[0] += 1
        return f"_:b{blank[0]}"

    def t(s, p, o):
        lines.append(f"{s} {p} {o} .")

    def some(role, filler):
        node = fresh()
        t(node, f"<{RDF}type>", f"<{OWL}Restriction>")
        t(node, f"<{OWL}onProperty>", role)
        t(node, f"<{OWL}someValuesFrom>", filler)
        return node

    def conj(parts):
        node = fresh()
        t(node, f"<{RDF}type>", f"<{OWL}Class>")
        head = fresh()
        t(node, f"<{OWL}intersectionOf>", head)
        for i, part in enumerate(parts):
            t(head, f"<{RDF}first>", part)
            nxt = f"<{RDF}nil>" if i == len(parts) - 1 else fresh()
            t(head, f"<{RDF}rest>", nxt)
            head = nxt
        return node

    def expr(depth=0):
        k = rnd.random()
        if depth > 1 or k < 0.5:
            return rnd.choice(classes)
        if k < 0.8:
            return some(rnd.choice(roles), expr(depth + 1))
        return conj([expr(depth + 1) for _ in range(rnd.randint(2, 3))])

    for c in classes:
        t(c, f"<{RDF}type>", f"<{OWL}Class>")
    for r in roles:
        t(r, f"<{RDF}type>", f"<{OWL}ObjectProperty>")
    for _ in range(rnd.randint(10, 35)):
        k = rnd.random()
        if k < 0.35:
            t(expr(), f"<{RDFS}subClassOf>", expr())
        elif k < 0.5:
            t(rnd.choice(classes), f"<{OWL}equivalentClass>", expr())
        elif k < 0.6:
            a, b = rnd.sample(roles, 2)
            t(a, f"<{RDFS}subPropertyOf>", b)
        elif k < 0.68:
            t(rnd.choice(roles), f"<{RDF}type>", f"<{OWL}TransitiveProperty>")
        elif k < 0.76:
            a, b, s = (rnd.choice(roles) for _ in range(3))
            head, second = fresh(), fresh()
            t(s, f"<{OWL}propertyChainAxiom>", head)
            t(head, f"<{RDF}first>", a)
            t(head, f"<{RDF}rest>", second)
            t(second, f"<{RDF}first>", b)
            t(second, f"<{RDF}rest>", f"<{RDF}nil>")
        elif k < 0.84:
            t(rnd.choice(roles), f"<{RDFS}domain>", rnd.choice(classes))
        else:
            t(rnd.choice(classes), f"<{RDFS}subClassOf>", rnd.choice(classes))
    return "\n".join(lines) + "\n"


def closure(path: Path) -> set:
    """(sub, super) pairs among named classes, equivalences both ways, transitively closed."""
    edges = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        parts = [p.strip().strip('"<>') for p in line.split("\t")]
        if len(parts) < 2 or parts[0].startswith("?"):
            continue
        a, b, *kind = parts
        if not (a.startswith(EX) and b.startswith(EX)) or a == b:
            continue
        edges.setdefault(a, set()).add(b)
        if kind and kind[0] == "eq":
            edges.setdefault(b, set()).add(a)
    pairs = set()
    for start in edges:
        seen, todo = set(), [start]
        while todo:
            for nxt in edges.get(todo.pop(), ()):
                if nxt not in seen:
                    seen.add(nxt)
                    todo.append(nxt)
        pairs |= {(start, s) for s in seen if s != start}
    return pairs


def main():
    if sys.argv[1] == "generate":
        n, out = int(sys.argv[2]), Path(sys.argv[3])
        out.mkdir(parents=True, exist_ok=True)
        for k in range(n):
            (out / f"case-{k}.ttl").write_text(generate(1000 + k), encoding="utf-8")
        return
    folder = Path(sys.argv[2])
    cases = sorted((p for p in folder.glob("case-*.ttl") if "." not in p.stem), key=lambda p: int(p.stem.split("-")[1]))
    bad = 0
    total = 0
    for case in cases:
        ours = closure(case.with_name(case.stem + ".nrese.tsv"))
        elk_file = case.with_name(case.stem + ".elk.tsv")
        if not elk_file.exists():
            print(f"{case.name}: no ELK output")
            bad += 1
            continue
        elk = closure(elk_file)
        total += len(elk)
        if ours != elk:
            bad += 1
            print(f"{case.name}: only NRESE {sorted(ours - elk)[:5]}, only ELK {sorted(elk - ours)[:5]}")
    print(f"{len(cases) - bad} of {len(cases)} cases agree; {total} subsumptions from ELK")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
