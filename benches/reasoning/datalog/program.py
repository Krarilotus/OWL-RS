"""One Datalog program, read once and written in each engine's own language.

Two kinds of program are read:
- FlowLog's benchmark programs (programs/flowlog, Apache-2.0): a `.in` section of input
  relations (`.decl R(x: number, ...)` and the CSV file it reads), `.printsize` sections of
  output relations, and `.rule` sections of rules with negated atoms (`!R(...)`), the
  wildcard `_`, integer constants and the comparisons `=` and `!=`;
- programs over RDF triples in VLog's syntax, as GLog's VLDB 2021 package has them (the
  RDFox benchmark programs Claros, LUBM, DBpedia, UOBM in their L and LE variants): every
  relation `RPn` is bound to one triple pattern (`RPn(A) :- TE(A,rdf:type,<C>)` or
  `RPn(A,B) :- TE(A,<P>,B)`), the other rules relate the `RPn`, and constants are IRIs.
  Its facts are the triples themselves; a relation's facts are the closure's triples that
  match its pattern.

Each engine gets the same rules in its own syntax:
- Soufflé: the program as Soufflé reads it (`.decl`, `.input`, `.printsize` for every
  derived relation);
- Nemo: `@import` of each CSV with integer columns, the rules, `@export` of the derived
  relations (the timed run passes `-e none`);
- VLog and GLog: the rules with upper-case variables and an `edb.conf` that reads each CSV
  in memory;
- NRESE: Notation3 user rules over triples (`reasoner.rules`, mode `custom`). A unary
  relation R is `?x rdf:type <r:R>`, a binary one `?x <r:R> ?y`, a constant c the IRI
  `<n:c>`; `!=` is `log:notEqualTo`. Relations of higher arity and negation have no
  counterpart there: `NotExpressible` says which construct, and the pair is reported as
  not expressible, never run in a weaker form.
"""
from __future__ import annotations

import re
from dataclasses import dataclass, field

RDF_TYPE = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>"
LOG = "http://www.w3.org/2000/10/swap/log#"


class NotExpressible(Exception):
    """The program uses a construct the target language doesn't have."""


@dataclass
class Atom:
    rel: str
    args: list[str]  # variables, integer constants, or "_"
    negated: bool = False


@dataclass
class Compare:
    left: str
    op: str  # "=" or "!="
    right: str


@dataclass
class Rule:
    head: Atom
    body: list[Atom | Compare]


@dataclass
class Program:
    arity: dict[str, int] = field(default_factory=dict)
    inputs: dict[str, str] = field(default_factory=dict)  # relation -> CSV file name
    rules: list[Rule] = field(default_factory=list)
    # A program over triples: relation -> ("type", class IRI) or ("prop", property IRI).
    patterns: dict[str, tuple[str, str]] = field(default_factory=dict)

    @property
    def derived(self) -> list[str]:
        """The relations rules derive, in order of first appearance."""
        return list(dict.fromkeys(r.head.rel for r in self.rules))

    @property
    def over_triples(self) -> bool:
        return bool(self.patterns)


def is_iri(term: str) -> bool:
    return term.startswith("<") and term.endswith(">")


def is_constant(term: str) -> bool:
    return is_iri(term) or bool(re.fullmatch(r"-?\d+", term))


def is_variable(term: str) -> bool:
    return term != "_" and not is_constant(term)


ATOM = re.compile(r"(!?)\s*([A-Za-z_]\w*)\s*\((.*)\)\s*$", re.S)
COMPARE = re.compile(r"(\S+)\s*(!=|=)\s*(\S+)\s*$")


def split_top(text: str, sep: str) -> list[str]:
    """`text` split at `sep` outside parentheses and IRIs."""
    parts, depth, start, iri = [], 0, 0, False
    for i, c in enumerate(text):
        if iri:
            iri = c != ">"
        elif c == "<" and not text.startswith("<-", i):
            iri = True
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif c == sep and depth == 0:
            parts.append(text[start:i])
            start = i + 1
    parts.append(text[start:])
    return [p.strip() for p in parts if p.strip()]


def parse_term(text: str) -> str:
    return text.strip().lstrip("?")


def parse_literal(text: str) -> Atom | Compare:
    m = ATOM.match(text)
    if m:
        args = [parse_term(a) for a in split_top(m.group(3), ",")]
        return Atom(m.group(2), args, negated=bool(m.group(1)))
    m = COMPARE.match(text)
    if m:
        return Compare(parse_term(m.group(1)), m.group(2), parse_term(m.group(3)))
    raise ValueError(f"can't read {text!r}")


def parse(text: str) -> Program:
    """A program in FlowLog's syntax (sections `.in`, `.printsize`, `.rule`; `#` comments)."""
    program = Program()
    section, last, rules = None, None, []
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        word = line.split()[0]
        if word in (".in", ".printsize", ".rule"):
            section = word
            continue
        if word == ".decl":
            m = re.match(r"\.decl\s+([A-Za-z_]\w*)\s*\((.*)\)", line)
            last = m.group(1)
            columns = [c for c in m.group(2).split(",") if c.strip()]
            program.arity[last] = len(columns)
            continue
        if word == ".input":
            program.inputs[last] = line.split()[1]
            continue
        if section == ".rule":
            rules.append(line)
    for statement in split_top(" ".join(rules), "."):
        head, _, body = statement.partition(":-")
        program.rules.append(Rule(parse_literal(head), [parse_literal(b) for b in split_top(body, ",")]))
    check_arity(program)
    return program


def check_arity(program: Program):
    for rule in program.rules:
        for literal in [rule.head, *rule.body]:
            if not isinstance(literal, Atom):
                continue
            if program.arity.setdefault(literal.rel, len(literal.args)) != len(literal.args):
                raise ValueError(f"{literal.rel} used with {len(literal.args)} arguments, declared with "
                                 f"{program.arity[literal.rel]}")


TYPE_BINDING = re.compile(r"(\w+)\(A\)\s*:-\s*TE\(A,\s*rdf:type,\s*(<[^>]*>)\)$")
PROP_BINDING = re.compile(r"(\w+)\(A,B\)\s*:-\s*TE\(A,\s*(<[^>]*>),\s*B\)$")


def parse_triples(text: str) -> Program:
    """A program over triples in VLog's syntax (GLog's Claros, LUBM, DBpedia, UOBM files)."""
    program = Program()
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith(("%", "#")):
            continue
        if m := TYPE_BINDING.match(line):
            program.patterns[m.group(1)] = ("type", m.group(2))
            program.arity[m.group(1)] = 1
        elif m := PROP_BINDING.match(line):
            program.patterns[m.group(1)] = ("prop", m.group(2))
            program.arity[m.group(1)] = 2
        elif "TE(" in line:
            raise ValueError(f"a triple binding of another shape: {line}")
        else:
            head, _, body = line.partition(":-")
            program.rules.append(Rule(parse_literal(head), [parse_literal(b) for b in split_top(body, ",")]))
    check_arity(program)
    unbound = sorted({a.rel for r in program.rules for a in [r.head, *r.body]
                      if isinstance(a, Atom)} - set(program.patterns))
    if unbound:
        raise ValueError(f"relations without a triple pattern: {', '.join(unbound[:5])}")
    return program


def pattern_bindings(program: Program):
    """(relation, arity, predicate IRI, object IRI or None) of each bound relation."""
    for rel, (kind, iri) in program.patterns.items():
        if kind == "type":
            yield rel, 1, RDF_TYPE, iri
        else:
            yield rel, 2, iri, None


# --- Soufflé ----------------------------------------------------------------------------------

def souffle_term(term: str) -> str:
    return f'"{term}"' if is_iri(term) else term


def souffle_literal(literal: Atom | Compare) -> str:
    if isinstance(literal, Compare):
        return f"{souffle_term(literal.left)} {literal.op} {souffle_term(literal.right)}"
    return f"{'!' if literal.negated else ''}{literal.rel}({', '.join(souffle_term(a) for a in literal.args)})"


def to_souffle(program: Program) -> str:
    """Integers for FlowLog's programs; for a program over triples, the triples as symbols
    (triples.tsv: subject, predicate, object as N-Triples terms, tab-separated)."""
    kind = "symbol" if program.over_triples else "number"
    lines = []
    if program.over_triples:
        lines += [".decl TE(s: symbol, p: symbol, o: symbol)",
                  '.input TE(IO="file", filename="triples.tsv", delimiter="\\t")']
    for rel, n in program.arity.items():
        lines.append(f".decl {rel}({', '.join(f'a{i}: {kind}' for i in range(n))})")
    for rel, file in program.inputs.items():
        lines.append(f'.input {rel}(IO="file", filename="{file}", delimiter=",")')
    for rel, n, predicate, obj in pattern_bindings(program):
        if n == 1:
            lines.append(f'{rel}(a) :- TE(a, "{predicate}", "{obj}").')
        else:
            lines.append(f'{rel}(a, b) :- TE(a, "{predicate}", b).')
    for rule in program.rules:
        lines.append(f"{souffle_literal(rule.head)} :- {', '.join(souffle_literal(b) for b in rule.body)}.")
    lines += [f".printsize {rel}" for rel in program.derived]
    return "\n".join(lines) + "\n"


# --- Nemo -------------------------------------------------------------------------------------

def nemo_term(term: str, fresh) -> str:
    if term == "_":
        return f"?{next(fresh)}"
    return term if is_constant(term) else f"?{term}"


def to_nemo(program: Program) -> str:
    lines = []
    if program.over_triples:
        lines.append('@import TE :- ntriples{resource="facts.nt"} .')
        for rel, n, predicate, obj in pattern_bindings(program):
            if n == 1:
                lines.append(f"{rel}(?a) :- TE(?a, {predicate}, {obj}) .")
            else:
                lines.append(f"{rel}(?a, ?b) :- TE(?a, {predicate}, ?b) .")
    for rel, file in program.inputs.items():
        columns = ", ".join(["int"] * program.arity[rel])
        lines.append(f'@import {rel} :- csv{{resource="{file}", format=({columns})}} .')
    for rule in program.rules:
        fresh = (f"_w{i}" for i in range(10**6))
        parts = []
        for literal in rule.body:
            if isinstance(literal, Compare):
                parts.append(f"{nemo_term(literal.left, fresh)} {literal.op} {nemo_term(literal.right, fresh)}")
            else:
                args = ", ".join(nemo_term(a, fresh) for a in literal.args)
                parts.append(f"{'~' if literal.negated else ''}{literal.rel}({args})")
        head = f"{rule.head.rel}({', '.join(nemo_term(a, fresh) for a in rule.head.args)})"
        lines.append(f"{head} :- {', '.join(parts)} .")
    lines += [f'@export {rel} :- csv{{resource="{rel}.csv"}} .' for rel in program.derived]
    return "\n".join(lines) + "\n"


# --- VLog and GLog -------------------------------------------------------------------------------

def vlog_term(term: str) -> str:
    if is_iri(term):
        return term
    if is_constant(term):
        raise NotExpressible("integer constants in rules (VLog reads CSV values as strings)")
    if term == "_":
        raise NotExpressible("the wildcard _")
    return "V" + term


def to_vlog(program: Program, data_dir: str) -> tuple[str, str]:
    """(rules, edb.conf): FlowLog's programs read their CSV files in memory; a program over
    triples reads a Trident store of the facts (`DATA/trident`, `vlog load`), with the
    triple bindings as GLog's files write them. VLog's rules have no comparisons; negation
    is left out too until a run confirms VLog's stratified negation on these programs."""
    lines = []
    for rel, n, predicate, obj in pattern_bindings(program):
        if n == 1:
            lines.append(f"{rel}(A) :- TE(A,rdf:type,{obj})")
        else:
            lines.append(f"{rel}(A,B) :- TE(A,{predicate},B)")
    for rule in program.rules:
        parts = []
        for literal in rule.body:
            if isinstance(literal, Compare):
                raise NotExpressible(f"the comparison {literal.op}")
            if literal.negated:
                raise NotExpressible("negation")
            parts.append(f"{literal.rel}({','.join(vlog_term(a) for a in literal.args)})")
        lines.append(f"{rule.head.rel}({','.join(vlog_term(a) for a in rule.head.args)}) :- {','.join(parts)}")
    if program.over_triples:
        conf = ["EDB0_predname=TE", "EDB0_type=Trident", f"EDB0_param0={data_dir}/trident"]
    else:
        conf = []
        for i, (rel, file) in enumerate(program.inputs.items()):
            conf += [f"EDB{i}_predname={rel}", f"EDB{i}_type=INMEMORY", f"EDB{i}_param0={data_dir}",
                     f"EDB{i}_param1={file.removesuffix('.csv')}"]
    return "\n".join(lines) + "\n", "\n".join(conf) + "\n"


# --- NRESE: Notation3 over triples ------------------------------------------------------------------

def relation_iri(rel: str) -> str:
    return f"<r:{rel}>"


def constant_iri(value: str) -> str:
    return value if is_iri(value) else f"<n:{value}>"


def check_triples(program: Program):
    for rel, n in program.arity.items():
        if n not in (1, 2):
            raise NotExpressible(f"relation {rel} has arity {n}: NRESE's user rules are over triples "
                                 "(unary and binary relations only)")
    for rule in program.rules:
        if any(isinstance(b, Atom) and b.negated for b in rule.body):
            raise NotExpressible("negation: NRESE's user rules have no negation")


def n3_term(term: str, fresh, bound: dict[str, str]) -> str:
    if term == "_":
        return f"?{next(fresh)}"
    if is_constant(term):
        return constant_iri(term)
    return bound.get(term, f"?{term}")


def n3_pattern(program: Program, rel: str, args: list[str]) -> str:
    """The triple pattern of a relation's atom: its bound pattern in a program over
    triples, else `<r:REL>` as the class or property."""
    if rel in program.patterns:
        kind, iri = program.patterns[rel]
        return f"{args[0]} {RDF_TYPE} {iri}" if kind == "type" else f"{args[0]} {iri} {args[1]}"
    if len(args) == 1:
        return f"{args[0]} {RDF_TYPE} {relation_iri(rel)}"
    return f"{args[0]} {relation_iri(rel)} {args[1]}"


def to_n3(program: Program) -> str:
    check_triples(program)
    lines = [f"@prefix log: <{LOG}> ."]
    for rule in program.rules:
        fresh = (f"_w{i}" for i in range(10**6))
        # `x = c` binds x to the constant: the variable is replaced by its IRI.
        bound = {b.left: constant_iri(b.right) for b in rule.body
                 if isinstance(b, Compare) and b.op == "=" and is_constant(b.right) and is_variable(b.left)}
        body = []
        for literal in rule.body:
            if isinstance(literal, Atom):
                body.append(n3_pattern(program, literal.rel, [n3_term(a, fresh, bound) for a in literal.args]))
            elif literal.op == "!=":
                body.append(f"{n3_term(literal.left, fresh, bound)} log:notEqualTo {n3_term(literal.right, fresh, bound)}")
            elif not (literal.left in bound and is_constant(literal.right)):
                raise NotExpressible(f"the comparison {literal.left} = {literal.right}")
        head = n3_pattern(program, rule.head.rel, [n3_term(a, fresh, bound) for a in rule.head.args])
        lines.append(f"{{ {' . '.join(body)} }} => {{ {head} }} .")
    return "\n".join(lines) + "\n"


def rdfox_pattern(program: Program, rel: str, args: list[str]) -> str:
    """An atom as an RDFox triple pattern `[s, p, o]`: the same triple NRESE's rules match."""
    s, p, o = n3_pattern(program, rel, args).split(" ", 2)
    return f"[{s}, {p}, {o}]"


def to_rdfox(program: Program) -> str:
    """The program as RDFox rules over the triples (RDFox's Datalog: `head :- body .`),
    the relations as NRESE's triples: unary ones rdf:type, binary ones properties, `!=` a
    FILTER. Refused where NRESE's form is (arity beyond 2, negation), so both run the same
    rules on the same facts."""
    check_triples(program)
    lines = []
    for rule in program.rules:
        fresh = (f"_w{i}" for i in range(10**6))
        bound = {b.left: constant_iri(b.right) for b in rule.body
                 if isinstance(b, Compare) and b.op == "=" and is_constant(b.right) and is_variable(b.left)}
        body = []
        for literal in rule.body:
            if isinstance(literal, Atom):
                body.append(rdfox_pattern(program, literal.rel, [n3_term(a, fresh, bound) for a in literal.args]))
            elif literal.op == "!=":
                body.append(f"FILTER({n3_term(literal.left, fresh, bound)} != {n3_term(literal.right, fresh, bound)})")
            elif not (literal.left in bound and is_constant(literal.right)):
                raise NotExpressible(f"the comparison {literal.left} = {literal.right}")
        head = rdfox_pattern(program, rule.head.rel, [n3_term(a, fresh, bound) for a in rule.head.args])
        lines.append(f"{head} :- {', '.join(body)} .")
    return "\n".join(lines) + "\n"


def ntriples(rel: str, rows, arity: int):
    """The N-Triples lines of one input relation's rows (lists of integer strings)."""
    if arity == 1:
        for row in rows:
            yield f"{constant_iri(row[0])} {RDF_TYPE} {relation_iri(rel)} .\n"
    else:
        for row in rows:
            yield f"{constant_iri(row[0])} {relation_iri(rel)} {constant_iri(row[1])} .\n"


def count_query(program: Program, rel: str) -> str:
    """SPARQL that counts a derived relation's facts in NRESE's closure."""
    pattern = n3_pattern(program, rel, ["?a", "?b"][:program.arity[rel]])
    return f"SELECT (COUNT(*) AS ?n) WHERE {{ {pattern} }}"


def ntriples_terms(line: str) -> tuple[str, str, str] | None:
    """Subject, predicate and object of one N-Triples line (the object may hold spaces);
    None for a blank or comment line."""
    line = line.strip()
    if not line or line.startswith("#"):
        return None
    s, rest = line.split(" ", 1)
    p, rest = rest.lstrip().split(" ", 1)
    o = rest.strip()
    if not o.endswith("."):
        raise ValueError(f"not an N-Triples line: {line[:120]}")
    return s, p, o[:-1].rstrip()

