//! `owl2-dl` against outside reasoners (docs/spec/reasoning-semantics.md): a `complete`
//! answer misses no certain answer, every answer is sound unless it says `unsound`, L's facts
//! are entailed, and only an inconsistent ontology's commit is refused. Generated ontologies
//! (`nrese_owl::fuzz`, whole or kept to OWL 2 RL or EL, with [`TEMPLATES`]) are answered by
//! the store and by HermiT, a question per candidate row (the DL kit's `nrese.dl.Entail`), and
//! by Openllet where those two differ. Neither reference is an oracle, so a failure is a
//! difference both share; one they split on is set aside. Each failure is shrunk, group of
//! triples by group, and written out.
//!
//! Run with `--ignored` through benches/reasoning/dl/outside.sh, with `NRESE_DL_REFERENCE`
//! (the command starting `Entail`), `NRESE_FUZZ_SEED`, `NRESE_FUZZ_CASES`, `NRESE_DL_WORKERS`,
//! `NRESE_DL_WAIT` (seconds before a reference is given up) and `NRESE_DL_OUT`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
use nrese_owl::mapping::Ontology;
use nrese_owl::model::Axiom;
use nrese_store::{DlConfig, MutationError, MutationPipeline, SparqlQueryRequest};
use serde_json::Value;

use super::queries::Table;
use super::{pipeline_with, run_dl, update};

const F: &str = "http://example.org/fuzz#";
const INT: &str = "http://www.w3.org/2001/XMLSchema#integer";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const IMPLICIT: &str = "http://www.ontotext.com/implicit";
const PREFIXES: &str = "@prefix f: <http://example.org/fuzz#> . \
    @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> . \
    @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . \
    @prefix owl: <http://www.w3.org/2002/07/owl#> . \
    @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .";

/// The constructs `fuzz` doesn't draw, one a line (name | Turtle | the names it uses as
/// individuals), each in a case with probability 1/4. `{C1}`, `{C2}` are classes; `{S}` a
/// simple property, `{Q}` one that isn't, `{P}` any but the chain's target p3 (a reference gap
/// under a key); `{D}` a data property; `{A1}`, `{A2}` two individuals; `{V}` an integer, `{W}`
/// it in another lexical form, `{X}` any in any form.
const TEMPLATES: &str = "
lexical | {A1} {D} {W} . |
cardinality | {C1} rdfs:subClassOf [ a owl:Restriction ; owl:onProperty {S} ; owl:maxCardinality 1 ] . f:a0 a {C1} ; {S} f:a1 , f:a2 . |
key-data | {C1} owl:hasKey ( {D} ) . f:a1 a {C1} ; {D} {V} . f:a2 a {C1} ; {D} {W} . |
key-object | {C1} owl:hasKey ( {P} ) . f:a1 a {C1} ; {P} f:a3 . f:a2 a {C1} ; {P} f:a3 . |
negative | [] a owl:NegativePropertyAssertion ; owl:sourceIndividual {A1} ; owl:assertionProperty {D} ; owl:targetValue {X} . |
data-sub | f:d1 rdfs:subPropertyOf f:d0 . |
data-disjoint | f:d0 owl:propertyDisjointWith f:d1 . |
pun-property | {S} owl:sameAs {Q} ; a owl:FunctionalProperty . f:a0 {S} f:a1 ; {Q} f:a2 . | {S} {Q}
pun-class | {C1} owl:sameAs {C2} . f:a0 a {C1} . | {C1} {C2}
pun-individual | {C1} owl:sameAs {A1} . f:a0 a {C1} . | {C1}
annotation | f:ann a owl:AnnotationProperty ; rdfs:domain {C1} ; rdfs:range {C2} ; rdfs:subPropertyOf f:sup . {A1} f:ann {A2} . |
annotation-sup | f:sup a owl:AnnotationProperty . |
disjoint | {C1} owl:disjointWith {C2} . {A1} a {C2} . |
same | {A1} owl:sameAs {A2} . |
different | {A1} owl:differentFrom {A2} . |
chain | f:p3 owl:propertyChainAxiom ( f:p1 f:p2 ) . f:a0 f:p1 f:a1 . f:a1 f:p2 f:a2 . |
";

/// Keys over anonymous individuals, one a line (name | Turtle as in [`TEMPLATES`] | the question
/// the key answers, an atom of [`QUERIES`] over names | who judges it), one of them in one case
/// in three. `hermit`: HermiT alone; `both`: a control, where the references must agree.
/// OWL 2 DL has no anonymous individual in `owl:sameAs`: such a class comes of a functional `f:pk`.
const KEYS: &str = "
anon-object | {C1} owl:hasKey ( {P} ) . f:a1 a {C1} ; {P} _:k1 . f:a2 a {C1} ; {P} _:k1 . | S f:a1 f:a2 | hermit
anon-subject | {C1} owl:hasKey ( {D} ) . f:a1 a {C1} ; {D} {V} . _:k2 a {C1} , {C2} ; {D} {W} . | T {C2} f:a1 | hermit
anon-value-class | {C1} owl:hasKey ( {P} ) . f:pk a owl:ObjectProperty , owl:FunctionalProperty . f:a1 a {C1} ; {P} _:k3 . f:a2 a {C1} ; {P} _:k4 . [] f:pk _:k3 , _:k4 . | S f:a1 f:a2 | hermit
named-value-class | {C1} owl:hasKey ( {P} ) . f:a1 a {C1} ; {P} f:a3 . f:a2 a {C1} ; {P} f:a0 . f:a3 owl:sameAs f:a0 . | S f:a1 f:a2 | both
mixed-value-class | {C1} owl:hasKey ( {P} ) . f:pk a owl:ObjectProperty , owl:FunctionalProperty . f:a1 a {C1} ; {P} _:k5 . f:a2 a {C1} ; {P} _:k6 . [] f:pk _:k5 , _:k6 , f:a3 . | S f:a1 f:a2 | both
named-subject-class | {C1} owl:hasKey ( {P} ) . f:pk a owl:ObjectProperty , owl:FunctionalProperty . _:k7 a {C1} ; {P} f:a3 . [] f:pk _:k7 , f:a1 . f:a2 a {C1} ; {P} f:a3 . | S f:a1 f:a2 | both
anon-subject-class | {C1} owl:hasKey ( {P} ) . f:pk a owl:ObjectProperty , owl:FunctionalProperty . _:k8 a {C1} ; {P} f:a3 . _:k9 a {C2} . [] f:pk _:k8 , _:k9 . f:a2 a {C1} ; {P} f:a3 . | T {C2} f:a2 | hermit
named-two | {C1} owl:hasKey ( {P} {D} ) . f:a1 a {C1} ; {P} f:a3 ; {D} {V} . f:a2 a {C1} ; {P} f:a3 ; {D} {W} . | S f:a1 f:a2 | both
anon-two | {C1} owl:hasKey ( {P} {D} ) . f:a1 a {C1} ; {P} _:k10 ; {D} {V} . f:a2 a {C1} ; {P} _:k10 ; {D} {W} . | S f:a1 f:a2 | hermit
";

/// The query shapes, one a line: a name, then atoms, `!` before the negated ones. An atom is
/// its kind as `Entail` takes it (`T` a type, `P` an object and `D` a data property assertion,
/// `S` sameAs, `E` and `F` an existential through either) and its terms: `C` a class, `p` a
/// property, `d` a data property, `T` `owl:Thing`; `x`, `y` answer variables, `v` one over
/// data values; `i` an individual, or one named only in the question; `a`, `b` two different
/// individuals; `l` an integer in some lexical form.
const QUERIES: &str = "
type | T C x
type-ask | T C i
prop | P p x y
prop-subject | P p i x
prop-ask | P p i i
data | D d x v
data-ask | D d i l
data-const | D d x l
same | S x y
same-ask | S a b
exists | E p T x
exists-class | E p C x
exists-ask | E p C i
exists-data | F d x
not-type | T C x ! T C x
not-prop | P p x y ! P p x y
not-data | T C x ! D d x l
not-exists | T C x ! E p T x
";

/// Triples kept or removed together, and the names they use as individuals.
struct Group {
    tag: &'static str,
    turtle: String,
    names: Vec<String>,
}

/// A query term: an answer variable, an IRI, or an `xsd:integer` in a lexical form.
#[derive(Clone)]
enum T {
    V(usize),
    I(String),
    L(String, u64),
}

/// An atom of [`QUERIES`]: its kind and terms; an existential variable is rolled up into
/// its class expression.
type Atom = (char, Vec<T>);

struct Query {
    name: &'static str,
    /// Per answer variable: whether it ranges over data values.
    values: Vec<bool>,
    pos: Vec<Atom>,
    neg: Vec<Atom>,
    /// A [`KEYS`] row's question: judged by HermiT alone (`true`) or a control (`false`).
    alone: Option<bool>,
}

struct Case {
    seed: u64,
    shape: &'static str,
    groups: Vec<Group>,
    queries: Vec<Query>,
    /// Per group, its commit: 0 or 1; the declarations are in the first.
    commits: Vec<usize>,
    /// A group without blank nodes or lists, deleted in a third commit.
    deleted: Option<usize>,
}

fn n(kind: &str, i: u64) -> String {
    format!("{F}{kind}{i}")
}

fn generate(seed: u64) -> Case {
    let mut r = Rng::new(seed);
    let mut table = Table::default();
    for (_, iri) in nrese_owl::Vocabulary::iris() {
        table.id(format!("<{iri}>"));
    }
    let sizes = Sizes {
        data_properties: 2,
        individuals: 4,
        ..Sizes::default()
    };
    let sig = Signature::new(sizes, &mut |name| match name {
        Name::Iri(iri) => table.id(format!("<{iri}>")),
        Name::Integer(i) => table.id(format!("\"{i}\"^^<{INT}>")),
    });
    let mut profile = Profile::sroiq();
    profile.axioms = 12;
    let o = fuzz::ontology(&mut r, &sig, profile);
    let shape = ["whole", "rl", "el"][r.below(3) as usize];
    let vocabulary = nrese_owl::Vocabulary::new(&|iri| table.ids.get(&format!("<{iri}>")).copied());
    let with = |axioms: Vec<Axiom>| Ontology {
        sources: vec![Vec::new(); axioms.len()],
        axioms,
        ..o.clone()
    };
    let mut turtle = |axioms: Vec<Axiom>| -> String {
        let triples = nrese_owl::write(&with(axioms), &vocabulary, &mut table);
        let text = |t: &[u64; 3]| t.map(|x| table.text[x as usize].clone()).join(" ") + " .";
        triples.iter().map(text).collect::<Vec<_>>().join(" ")
    };
    let declaration = |a: &Axiom| matches!(a, Axiom::Declaration(..));
    let (decls, axioms): (Vec<_>, Vec<_>) = o.axioms.iter().cloned().partition(declaration);
    let mut groups = Vec::new();
    let mut add = |tag, turtle, names| groups.push(Group { tag, turtle, names });
    let individuals: Vec<String> = (0..4).map(|i| n("a", i)).collect();
    add("declarations", turtle(decls.clone()), individuals);
    for a in axioms {
        let p = nrese_owl::profile::of_ontology(&with([decls.clone(), vec![a.clone()]].concat()));
        if shape == "whole" || (shape == "rl" && p.rl) || (shape == "el" && p.el) {
            add("axiom", turtle(vec![a]), Vec::new());
        }
    }
    for line in TEMPLATES.lines() {
        let mut parts = line.split('|').map(str::trim);
        let (Some(tag), Some(template), Some(names)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if r.one_in(4) {
            let [turtle, names] = fill([template, names], &mut r);
            let names = names.split_whitespace().map(|x| x.replace("f:", F));
            add(tag, turtle, names.collect());
        }
    }
    let mut queries = queries(&mut r);
    // One case in eight, by a draw of its own: `f:p3` as a chain of 260 fresh properties (C2).
    if Rng::new(!seed).one_in(8) {
        add("long-chain", long_chain(260), Vec::new());
    }
    // Drawn after the queries, so a seed keeps the ontology and queries of the runs before.
    let mut commits: Vec<usize> = (0..groups.len())
        .map(|g| (g > 0 && r.one_in(2)) as usize)
        .collect();
    let plain = |g: &Group| !["_:", "[", "("].iter().any(|b| g.turtle.contains(b));
    let deletable: Vec<usize> = (1..groups.len()).filter(|&g| plain(&groups[g])).collect();
    let deleted = (!deletable.is_empty() && r.one_in(2))
        .then(|| deletable[r.below(deletable.len() as u64) as usize]);
    // In one case in three, one row of [`KEYS`] (two could make each other's `f:a1`, `f:a2` the
    // same), from a stream of its own after every other draw, so a seed keeps its case.
    let mut k = Rng::new(seed ^ 0x6b65_7973);
    let rows: Vec<&'static str> = KEYS.lines().filter(|l| l.contains('|')).collect();
    if k.one_in(3) {
        let line = rows[k.below(rows.len() as u64) as usize];
        let parts: Vec<&'static str> = line.split('|').map(str::trim).collect();
        let [tag, template, question, by] = parts[..] else {
            panic!("a row of KEYS: {line}");
        };
        let [turtle, question] = fill([template, question], &mut k);
        let names = Vec::new();
        groups.push(Group { tag, turtle, names });
        commits.push(k.one_in(2) as usize);
        let mut q = query(tag, &question, &mut k);
        q.alone = Some(by == "hermit");
        queries.push(q);
    }
    Case {
        seed,
        shape,
        groups,
        queries,
        commits,
        deleted,
    }
}

/// `f:p3` as a chain of `n` fresh properties, along a path of blank nodes from `f:a0` to `f:a1`.
fn long_chain(n: usize) -> String {
    let chain: Vec<String> = (0..n).map(|i| format!("f:c{i}")).collect();
    let mut turtle = format!("f:p3 owl:propertyChainAxiom ( {} ) .", chain.join(" "));
    for (i, c) in chain.iter().enumerate() {
        turtle += &format!(" {c} a owl:ObjectProperty . _:l{i} {c} _:l{} .", i + 1);
    }
    turtle
        .replace("_:l0 ", "f:a0 ")
        .replace(&format!("_:l{n} "), "f:a1 ")
}

/// `texts` with each placeholder of [`TEMPLATES`] replaced, the same way in each.
fn fill<const K: usize>(texts: [&str; K], r: &mut Rng) -> [String; K] {
    let lit = |v: u64, form: u64| format!("\"{}{v}\"^^xsd:integer", ["", "0", "+"][form as usize]);
    let (a1, v) = (r.below(4), r.below(3));
    let names = [
        ("{C1}", format!("f:C{}", r.below(4))),
        ("{C2}", format!("f:C{}", r.below(4))),
        ("{S}", format!("f:p{}", r.below(2))),
        ("{Q}", format!("f:p{}", 2 + r.below(2))),
        ("{P}", format!("f:p{}", r.below(4) % 3)),
        ("{D}", format!("f:d{}", r.below(2))),
        ("{A1}", format!("f:a{a1}")),
        ("{A2}", format!("f:a{}", (a1 + 1 + r.below(3)) % 4)),
        ("{V}", lit(v, 0)),
        ("{W}", lit(v, 1 + r.below(2))),
        ("{X}", lit(r.below(3), r.below(3))),
    ];
    texts.map(|t| names.iter().fold(t.to_owned(), |t, (k, x)| t.replace(k, x)))
}

/// Twelve queries drawn from [`QUERIES`].
fn queries(r: &mut Rng) -> Vec<Query> {
    let lines = QUERIES.lines();
    let shapes: Vec<_> = lines.filter_map(|l| l.split_once(" | ")).collect();
    let mut out = Vec::new();
    for _ in 0..12 {
        let (name, spec) = shapes[r.below(shapes.len() as u64) as usize];
        out.push(query(name, spec, r));
    }
    out
}

/// A query of `spec` (a line of [`QUERIES`], or atoms over names), its terms drawn.
fn query(name: &'static str, spec: &str, r: &mut Rng) -> Query {
    let (mut values, mut pos, mut neg, mut a) = (Vec::new(), Vec::new(), Vec::new(), 0);
    let (mut tokens, mut negated) = (spec.split_whitespace(), false);
    while let Some(kind) = tokens.next() {
        if kind == "!" {
            negated = true;
            continue;
        }
        let arity = if "TSF".contains(kind) { 2 } else { 3 };
        let terms = tokens.by_ref().take(arity);
        let terms: Vec<T> = terms.map(|t| draw(t, r, &mut a, &mut values)).collect();
        let atom = (kind.chars().next().expect("a kind"), terms);
        if negated { &mut neg } else { &mut pos }.push(atom);
    }
    let alone = None;
    Query {
        name,
        values,
        pos,
        neg,
        alone,
    }
}

/// A term of [`QUERIES`], drawn; `a` keeps the last `a` for `b`.
fn draw(t: &str, r: &mut Rng, a: &mut u64, values: &mut Vec<bool>) -> T {
    let mut var = |i: usize, value: bool| {
        values.resize(values.len().max(i + 1), false);
        values[i] |= value;
        T::V(i)
    };
    match t {
        "C" => T::I(n("C", r.below(4))),
        "p" if r.one_in(10) => T::I(format!("{F}sup")),
        "p" => T::I(n("p", r.below(4))),
        "d" => T::I(n("d", r.below(2))),
        "T" => T::I(THING.into()),
        "x" => var(0, false),
        "y" => var(1, false),
        "v" => var(1, true),
        "i" if r.one_in(5) => T::I(n("fresh", 0)),
        "i" => T::I(n("a", r.below(4))),
        "a" => {
            *a = r.below(4);
            T::I(n("a", *a))
        }
        "b" => T::I(n("a", (*a + 1 + r.below(3)) % 4)),
        t if t.starts_with("f:") => T::I(t.replace("f:", F)),
        _ => {
            let v = r.below(3);
            T::L(format!("{}{v}", ["", "0", "+"][r.below(3) as usize]), v)
        }
    }
}

fn term(t: &T) -> String {
    match t {
        T::V(i) => format!("?v{i}"),
        T::I(iri) => format!("<{iri}>"),
        T::L(lex, _) => format!("\"{lex}\"^^<{INT}>"),
    }
}

fn pattern(atoms: &[Atom], e: &mut usize) -> String {
    let mut out = Vec::new();
    for (kind, terms) in atoms {
        *e += 1;
        let s: Vec<String> = terms.iter().map(term).collect();
        out.push(match (*kind, terms.as_slice()) {
            ('T', _) => format!("{} <{RDF}type> {} .", s[1], s[0]),
            ('P' | 'D', _) => format!("{} {} {} .", s[1], s[0], s[2]),
            ('S', [T::V(_), T::V(_)]) => {
                format!("{0} <{OWL}sameAs> {1} . FILTER({0} != {1})", s[0], s[1])
            }
            ('S', _) => format!("{} <{OWL}sameAs> {} .", s[0], s[1]),
            ('E', [_, T::I(c), _]) if c.as_str() == THING => format!("{} {} ?e{e} .", s[2], s[0]),
            ('E', _) => format!("{} {} ?e{e} . ?e{e} <{RDF}type> {} .", s[2], s[0], s[1]),
            _ => format!("{} {} ?e{e} .", s[1], s[0]),
        });
    }
    out.join(" ")
}

fn sparql(q: &Query) -> String {
    let mut e = 0;
    let mut body = pattern(&q.pos, &mut e);
    if !q.neg.is_empty() {
        body = format!("{body} FILTER NOT EXISTS {{ {} }}", pattern(&q.neg, &mut e));
    }
    let vars: Vec<String> = (0..q.values.len()).map(|i| format!("?v{i}")).collect();
    match vars.is_empty() {
        true => format!("ASK {{ {body} }}"),
        false => format!("SELECT {} {{ {body} }}", vars.join(" ")),
    }
}

/// The question `Entail` answers for an atom with the answer variables bound to `row`.
fn question((kind, terms): &Atom, row: &[String]) -> String {
    let g = |t: &T| match t {
        T::V(i) => row[*i].clone(),
        T::I(iri) => iri.clone(),
        T::L(_, v) => v.to_string(),
    };
    let terms: Vec<String> = terms.iter().map(g).collect();
    format!("{kind} {}", terms.join(" "))
}

/// A served inferred statement as a question, where it is an OWL axiom over the case's names.
fn inferred(row: &[String]) -> Option<String> {
    let (s, p, o) = (&row[0], &row[1], &row[2]);
    let named = |x: &str| x.starts_with(F);
    if !named(s) {
        return None;
    }
    match p.as_str() {
        x if x == format!("{RDF}type") => named(o).then(|| format!("T {o} {s}")),
        x if x == format!("{OWL}sameAs") => (named(o) && o != s).then(|| format!("S {s} {o}")),
        x if x == format!("{RDFS}subClassOf") => (named(o) && o != s).then(|| format!("U {s} {o}")),
        x if x.starts_with(&format!("{F}d")) => {
            o.parse::<u64>().is_ok().then(|| format!("D {p} {s} {o}"))
        }
        x if named(x) && named(o) => Some(format!("P {p} {s} {o}")),
        _ => None,
    }
}

/// The store's answer: the status's label and paths, and the rows, each term an IRI, an
/// integer's value, or `?` and its text (a blank node or another literal: not judged).
struct Answer {
    label: String,
    paths: String,
    rows: BTreeSet<Vec<String>>,
}

/// The answers and the state the reference reads; or a failed commit: its kind as the store
/// types it (never read from its sentence), why, and the state it would have made.
type Answers = Result<(Vec<Result<Answer, String>>, String), (String, String, String)>;

/// The store's answers to `texts` after `commits` (each an update and the state the check
/// expects after it, passed on to the references: nothing they read comes from the store).
fn store(commits: &[(String, String)], texts: &[String]) -> Answers {
    let dl = pipeline_with(DlConfig {
        timeout: Some(Duration::from_secs(5)),
        max_nodes: Some(100_000),
        max_branch_points: Some(100_000),
        ..DlConfig::default()
    });
    for (write, after) in commits {
        if let Err(e) = update(&dl, &format!("PREFIX f: <{F}> PREFIX rdf: <{RDF}> {write}")) {
            let kind = match &e {
                MutationError::Rejected(r) => format!("{:?}", r.kind),
                other => format!("{other:?}"),
            };
            return Err((kind, e.to_string(), after.clone()));
        }
    }
    let state = commits.last().map_or(String::new(), |c| c.1.clone());
    Ok((texts.iter().map(|q| answer(&dl, q)).collect(), state))
}

/// The triples of a case's Turtle (every token spaced, as [`TEMPLATES`] and the writer give
/// it), each term in full, each blank-node label marked with `scope`: a write's labels are its
/// own (SPARQL 1.1 Update §3.1.1). The check's own reading, so a state never comes from the store.
fn triples(turtle: &str, scope: &str) -> Vec<[String; 3]> {
    let tokens = turtle.split_whitespace().peekable();
    let mut r = Reader {
        tokens,
        out: Vec::new(),
        fresh: 0,
        scope,
    };
    while r.tokens.peek().is_some() {
        let s = r.node();
        r.props(&s);
        r.tokens.next(); // `.`
    }
    r.out
}

struct Reader<'a> {
    tokens: std::iter::Peekable<std::str::SplitWhitespace<'a>>,
    out: Vec<[String; 3]>,
    fresh: usize,
    scope: &'a str,
}

impl Reader<'_> {
    fn blank(&mut self) -> String {
        self.fresh += 1;
        format!("_:n{}{}", self.fresh, self.scope)
    }

    /// A term, a `[ … ]` or a `( … )`, and the triples it makes.
    fn node(&mut self) -> String {
        match self.tokens.next().expect("a term") {
            "[]" => self.blank(),
            "[" => {
                let b = self.blank();
                self.props(&b);
                self.tokens.next(); // `]`
                b
            }
            "(" => self.list(),
            "a" => format!("<{RDF}type>"),
            t if t.starts_with("_:") => format!("{t}{}", self.scope),
            t if t.starts_with('<') => t.to_owned(),
            t if t.starts_with('"') => t.replace("^^xsd:integer", &format!("^^<{INT}>")),
            t if t.parse::<u64>().is_ok() => format!("\"{t}\"^^<{INT}>"),
            t => {
                let prefixes = [("f:", F), ("rdf:", RDF), ("rdfs:", RDFS), ("owl:", OWL)];
                let (p, iri) = prefixes.iter().find(|(p, _)| t.starts_with(p)).expect(t);
                format!("<{iri}{}>", &t[p.len()..])
            }
        }
    }

    /// The rest of a list after its `(`.
    fn list(&mut self) -> String {
        if self.tokens.next_if_eq(&")").is_some() {
            return format!("<{RDF}nil>");
        }
        let (b, first) = (self.blank(), self.node());
        let rest = self.list();
        self.out.push([b.clone(), format!("<{RDF}first>"), first]);
        self.out.push([b.clone(), format!("<{RDF}rest>"), rest]);
        b
    }

    /// A subject's predicates and objects, with `;` and `,` between them.
    fn props(&mut self, s: &str) {
        let mut p = self.node();
        loop {
            let o = self.node();
            self.out.push([s.to_owned(), p.clone(), o]);
            match self.tokens.next_if(|t| *t == "," || *t == ";") {
                Some(",") => {}
                Some(_) => p = self.node(),
                None => return,
            }
        }
    }
}

/// The store's answer to `q`; `none` is the label where no reasoning path can leave
/// answers out.
fn answer(dl: &MutationPipeline, q: &str) -> Result<Answer, String> {
    let (s, mut out) = (dl.store(), Vec::new());
    let prepared = s.prepare_query(&SparqlQueryRequest::all(q.to_owned()));
    let status = run_dl(s, &prepared.map_err(|e| e.to_string())?, &mut out);
    let status = status.map_err(|e| e.to_string())?;
    let json: Value = serde_json::from_slice(&out).map_err(|e| e.to_string())?;
    let (label, paths) = status.map_or(("none".into(), String::new()), |s| {
        (s.shared.as_str().to_owned(), s.paths.join("+"))
    });
    let key = |x: &Value| {
        let value = x["value"].as_str().unwrap_or("");
        let int = (x["datatype"] == INT).then(|| value.trim().parse::<i128>().ok());
        match (x["type"] == "uri", int.flatten()) {
            (true, _) => value.to_owned(),
            (false, Some(v)) => v.to_string(),
            (false, None) => format!("?{value}"),
        }
    };
    let vars = json["head"]["vars"].as_array().cloned().unwrap_or_default();
    let row = |b: &Value| {
        vars.iter()
            .map(|v| key(&b[v.as_str().unwrap_or("")]))
            .collect()
    };
    let rows = match &json["boolean"] {
        Value::Bool(true) => BTreeSet::from([Vec::new()]),
        Value::Bool(false) => BTreeSet::new(),
        _ => json["results"]["bindings"]
            .as_array()
            .into_iter()
            .flatten()
            .map(row)
            .collect(),
    };
    Ok(Answer { label, paths, rows })
}

/// HermiT or Openllet through `Entail`, one JVM at a time, a fresh one after a timeout.
struct Reference {
    command: String,
    wait: Duration,
    child: Option<(Child, ChildStdin, mpsc::Receiver<String>)>,
}

enum Reply {
    Inconsistent,
    Answers(BTreeMap<String, bool>),
    Undecided(String),
}

impl Reference {
    fn new(command: String, wait: Duration) -> Self {
        let child = None;
        Self {
            command,
            wait,
            child,
        }
    }

    fn ask(&mut self, reasoner: &str, turtle: &str, questions: &BTreeSet<String>) -> Reply {
        let q: Vec<&str> = questions.iter().map(String::as_str).collect();
        let line = format!("{reasoner}\t{PREFIXES} {turtle}\t{}\n", q.join("\t"));
        let (_, stdin, rx) = self.child.get_or_insert_with(|| {
            let mut child = Command::new("sh")
                .args(["-c", self.command.as_str()])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("the reference starts");
            let (tx, rx) = mpsc::channel();
            let lines = BufReader::new(child.stdout.take().expect("its output")).lines();
            std::thread::spawn(move || lines.map_while(Result::ok).try_for_each(|l| tx.send(l)));
            let stdin = child.stdin.take().expect("its input");
            (child, stdin, rx)
        });
        // A pipe to a child is not buffered: nothing to flush.
        let sent = stdin.write_all(line.as_bytes());
        let reply = sent.ok().and_then(|()| rx.recv_timeout(self.wait).ok());
        match reply.as_deref() {
            Some("i") => Reply::Inconsistent,
            Some(bits) if bits.starts_with('c') && bits.len() == q.len() + 1 => {
                let answers = q.iter().zip(bits[1..].chars());
                Reply::Answers(answers.map(|(s, b)| ((*s).to_owned(), b == '1')).collect())
            }
            other => {
                // A timeout halts the JVM, anything else leaves it unknown: start afresh.
                if let Some((mut child, _, _)) = self.child.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                Reply::Undecided(other.unwrap_or("no answer before the deadline").to_owned())
            }
        }
    }
}

/// The failures among the verdicts' classes; the others are `agree` or a counted outcome.
fn fails(class: &str) -> bool {
    matches!(
        class,
        "missing" | "wrong" | "wrong-refusal" | "wrong-acceptance" | "unclassified"
    ) || class.starts_with("undecided-")
}

/// A verdict on a query or the write: its index, class, what each side gave, and the answer's
/// keys (the rows expected, each wrong and each missing row; a write's is its class).
type Verdict = (usize, String, String, BTreeSet<String>);

/// A row's outcome from HermiT's verdict and Openllet's, if asked. A failure needs both to fail
/// it alike on the same answer: the same expected rows or one wrong or missing row in common.
fn outcome(hermit: &Verdict, openllet: Option<&Verdict>, alone: Option<bool>) -> String {
    let (_, class, _, keys) = hermit;
    let alike = |(_, c, _, k): &Verdict| c == class && !k.is_disjoint(keys);
    let out = match (fails(class), openllet) {
        // A control's references must agree: else the premise below is wrong. One giving
        // up while the other agrees with the store is listed apart.
        (_, Some((_, c, _, _))) if alone == Some(false) && c != class => {
            match [class, c].iter().any(|x| *x == "reference-undecided") {
                true if fails(class) || fails(c) => "FAIL",
                true => "control-undecided",
                false => "control-split",
            }
        }
        // A declined write: the class is HermiT's word, the outcome Openllet's.
        (false, Some((_, c, _, _))) if class.starts_with("declined") => c.as_str(),
        // Unjudged, so never a pass.
        (false, _) if class == "not-judged" || class == "reference-undecided" => class.as_str(),
        (false, _) => ["pass", "pass-one"][(alone == Some(true)) as usize],
        // A key binds named individuals only: HermiT follows that, Openllet reads a blank
        // node as a named individual.
        (true, _) if alone == Some(true) => "FAIL-one",
        (true, Some(o)) if alike(o) => "FAIL",
        (true, _) => "set-aside",
    };
    out.to_owned()
}

/// Per candidate row of a query: the row, its positive questions and its negated ones.
type Rows = Vec<(Vec<String>, Vec<String>, Vec<String>)>;

impl Case {
    /// Whether a key names an object property that isn't simple (Structural Specification
    /// §11.1: a chain into it or a transitive property under it). OWL 2 DL allows it: §11.2's
    /// list of axioms that need simple properties has no HasKey (w3c2012owl2syntax.txt:
    /// 4966-4970). But both references miss a key over a value only a chain implies, and a
    /// wrong "consistent" moves every answer. Keys come from the rows above, as a list of
    /// names; the generator draws no equivalent properties and pairs inverses among simple ones.
    fn reference_gap(&self) -> bool {
        let all: Vec<&str> = self.groups.iter().map(|g| g.turtle.as_str()).collect();
        let mut text = all.join(" ").replace('>', "");
        for (iri, short) in [(F, "f:"), (OWL, "owl:"), (RDFS, "rdfs:")] {
            text = text.replace(&format!("<{iri}"), short);
        }
        let t: Vec<&str> = text.split_whitespace().collect();
        let w: Vec<&[&str]> = t.windows(3).collect();
        let inverse = w
            .iter()
            .filter(|x| x[1] == "owl:inverseOf" && x[0].starts_with("_:"));
        let inverse: BTreeMap<&str, &str> = inverse.map(|x| (x[0], x[2])).collect();
        let of = |x: &str| inverse.get(x).unwrap_or(&x).to_string();
        let composite = w
            .iter()
            .filter(|x| x[1] == "owl:propertyChainAxiom" || x[2] == "owl:TransitiveProperty");
        let mut not_simple: BTreeSet<String> = composite.map(|x| of(x[0])).collect();
        let sub = w.iter().filter(|x| x[1] == "rdfs:subPropertyOf");
        let sub: Vec<(String, String)> = sub.map(|x| (of(x[0]), of(x[2]))).collect();
        for _ in 0..sub.len() {
            for (a, b) in &sub {
                if not_simple.contains(a) {
                    not_simple.insert(b.clone());
                }
            }
        }
        let keys = (2..t.len()).filter(|&i| t[i - 2] == "owl:hasKey");
        let mut keys = keys.flat_map(|i| t[i..].iter().take_while(|x| **x != ")"));
        keys.any(|p| not_simple.contains(&of(p)))
    }

    fn kept<'a>(&'a self, skip: &'a BTreeSet<usize>) -> impl Iterator<Item = &'a Group> {
        let all = 0..self.groups.len();
        all.filter(move |i| !skip.contains(i))
            .map(move |i| &self.groups[i])
    }

    /// The commits under `skip`, each an update and the state after it, the triples the
    /// check reads in the writes: the groups of the first commit, those of the second, then
    /// the deletion of `deleted`, its triples taken away; only the first NRESE_DL_STEPS if set.
    fn commits(&self, skip: &BTreeSet<usize>) -> Vec<(String, String)> {
        let turtle = |c: usize| {
            let kept = (0..self.groups.len()).filter(|g| !skip.contains(g));
            let kept = kept.filter(|&g| self.commits[g] == c);
            kept.map(|g| self.groups[g].turtle.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let text = |s: &BTreeSet<[String; 3]>| -> String {
            s.iter().map(|t| t.join(" ") + " . ").collect()
        };
        let (mut out, mut state) = (Vec::new(), BTreeSet::new());
        for c in 0..2 {
            let write = turtle(c);
            if !write.is_empty() {
                state.extend(triples(&write, &format!("c{c}")));
                out.push((format!("INSERT DATA {{ {write} }}"), text(&state)));
            }
        }
        if let Some(d) = self.deleted.filter(|d| !skip.contains(d)) {
            let (turtle, gone) = (&self.groups[d].turtle, triples(&self.groups[d].turtle, ""));
            state.retain(|t| !gone.contains(t));
            out.push((format!("DELETE DATA {{ {turtle} }}"), text(&state)));
        }
        out.truncate(env("NRESE_DL_STEPS").map_or(usize::MAX, |n| n as usize));
        out
    }

    /// Query `i`: the case's queries, then the read of the inferred statements; one more
    /// stands for the write itself.
    fn text(&self, i: usize) -> String {
        match self.queries.get(i) {
            Some(q) => sparql(q),
            None => format!("SELECT ?v0 ?v1 ?v2 FROM <{IMPLICIT}> {{ ?v0 ?v1 ?v2 }}"),
        }
    }

    /// A query's own name: its kind, then `-2`, `-3` for the kind's later queries in the case.
    fn name(&self, i: usize) -> String {
        let last = ["write", "inferred"][(i == self.queries.len()) as usize];
        let kind = self.queries.get(i).map_or(last, |q| q.name);
        let earlier = self.queries.iter().take(i).filter(|q| q.name == kind);
        match earlier.count() {
            0 => kind.to_owned(),
            k => format!("{kind}-{}", k + 1),
        }
    }

    /// The queries to run for a verdict on `i`: all of them for the write, else `i`.
    fn picked(&self, i: usize) -> Vec<usize> {
        match i > self.queries.len() {
            true => (0..=self.queries.len()).collect(),
            false => vec![i],
        }
    }

    fn rows(&self, i: usize, a: &Answer, individuals: &BTreeSet<String>) -> Rows {
        let Some(q) = self.queries.get(i) else {
            let asked = |r: &Vec<String>| Some((r.clone(), vec![inferred(r)?], Vec::new()));
            return a.rows.iter().filter_map(asked).collect();
        };
        let mut tuples = vec![Vec::new()];
        for (k, &value) in q.values.iter().enumerate() {
            let mut range: BTreeSet<String> = match value {
                true => (0..3).map(|v: u64| v.to_string()).collect(),
                false => individuals.clone(),
            };
            // All but a term no question can name: a blank node or another literal.
            let named = a.rows.iter().map(|r| &r[k]).filter(|x| !x.starts_with('?'));
            range.extend(named.cloned());
            let next = tuples.iter().flat_map(|t| {
                range
                    .iter()
                    .map(move |x| [t.clone(), vec![x.clone()]].concat())
            });
            tuples = next.collect();
        }
        let pair = |(k, t): &Atom| *k == 'S' && matches!(t.as_slice(), [T::V(_), T::V(_)]);
        let distinct = q.pos.iter().any(pair);
        let ask = |t: &Vec<String>, atoms: &[Atom]| -> Vec<String> {
            atoms.iter().map(|a| question(a, t)).collect()
        };
        let kept = tuples.into_iter().filter(|t| !(distinct && t[0] == t[1]));
        kept.map(|t| (t.clone(), ask(&t, &q.pos), ask(&t, &q.neg)))
            .collect()
    }

    /// The verdict, with what each side gave, on each query of `picked` (one past the
    /// inferred statements: the write) under `reasoner`, the store having answered `got`.
    fn verdicts(
        &self,
        skip: &BTreeSet<usize>,
        picked: &[usize],
        got: &Answers,
        reference: &mut Reference,
        reasoner: &str,
    ) -> Vec<Verdict> {
        let (write, on_write) = (self.queries.len() + 1, BTreeSet::from(["write".to_owned()]));
        let (answers, turtle) = match got {
            Err((kind, why, state)) => {
                let (class, said) = match reference.ask(reasoner, state, &BTreeSet::new()) {
                    Reply::Inconsistent => ("agree", "inconsistent".to_owned()),
                    Reply::Answers(_) => ("wrong-refusal", "consistent".to_owned()),
                    Reply::Undecided(why) => ("reference-undecided", why),
                };
                // Declined as undecided: honest, so no failure, but never a pass; a kind the
                // check doesn't know fails the run. As OWL 2 DL reads the case otherwise than
                // the rules, only with a punned name or an annotation property's axiom.
                let pun = |g: &Group| g.tag == "annotation" || g.tag.starts_with("pun-");
                let unfounded = |text: &str| why.contains(text) && !self.kept(skip).any(pun);
                let class = match kind.as_str() {
                    "Inconsistent" => class.to_owned(),
                    "Undecided" if unfounded("reads differently") => "undecided-unfounded".into(),
                    "Undecided" if unfounded("within its budget") => "undecided-unchecked".into(),
                    "Undecided" if class == "reference-undecided" => "declined-no-answer".into(),
                    "Undecided" => format!("declined-{said}"),
                    _ => "unclassified".into(),
                };
                let detail = format!("store: refused, {kind} ({why}); {reasoner}: {said}");
                return vec![(write, class, detail + "; no query judged", on_write)];
            }
            Ok((answers, state)) => (answers, state),
        };
        let names = self.kept(skip).flat_map(|g| g.names.clone());
        let known: BTreeSet<String> = names.collect();
        let (mut asked, mut per) = (BTreeSet::new(), Vec::new());
        for (&i, a) in picked.iter().zip(answers) {
            let rows = a.as_ref().map_or(Vec::new(), |a| self.rows(i, a, &known));
            for (_, p, n) in &rows {
                asked.extend(p.iter().chain(n).cloned());
            }
            per.push(rows);
        }
        let entailed = match reference.ask(reasoner, turtle, &asked) {
            Reply::Answers(m) => m,
            Reply::Inconsistent => {
                let complete = answers.iter().flatten().any(|a| a.label == "complete");
                let class = ["accepted-inconsistent", "wrong-acceptance"][complete as usize];
                let detail = format!("store: accepted; {reasoner}: inconsistent");
                return vec![(write, class.into(), detail, on_write)];
            }
            Reply::Undecided(why) => {
                let undecided = "reference-undecided".to_owned();
                let v = |&i: &usize| (i, undecided.clone(), why.clone(), BTreeSet::new());
                return picked.iter().map(v).collect();
            }
        };
        let judged = picked.iter().zip(answers).zip(per);
        let judged = judged.map(|((&i, a), rows)| match a {
            Ok(a) => {
                let (class, detail, keys) = Self::judge(a, &rows, &entailed, reasoner);
                (i, class, detail, keys)
            }
            Err(why) => (i, "not-judged".into(), why.clone(), BTreeSet::new()),
        });
        judged.collect()
    }

    /// The class of the store's answer `a`, given its candidate rows and what `reasoner`
    /// entails; what each side gave; and the answer's keys.
    fn judge(
        a: &Answer,
        rows: &Rows,
        entailed: &BTreeMap<String, bool>,
        reasoner: &str,
    ) -> (String, String, BTreeSet<String>) {
        let all = |qs: &[String]| qs.iter().all(|q| entailed[q]);
        let holds = rows
            .iter()
            .filter(|(_, p, n)| all(p) && (n.is_empty() || !all(n)));
        let expected: BTreeSet<&Vec<String>> = holds.map(|(t, _, _)| t).collect();
        // A row no question asks (a blank node, another literal, an inferred statement that
        // isn't an OWL axiom over the case's names) is counted unchecked; every other is judged.
        let asks = |r: &&Vec<String>| rows.iter().any(|(t, _, _)| t == *r);
        let (asked, unchecked): (Vec<_>, Vec<_>) = a.rows.iter().partition(asks);
        let wrong: Vec<_> = asked.iter().filter(|r| !expected.contains(*r)).collect();
        let missing: Vec<_> = expected.iter().filter(|r| !a.rows.contains(**r)).collect();
        // Is the certification honest: a row the references don't entail passes only under
        // `unsound`, counted apart, wrong or right (the coarse mark's price).
        let class = match (a.label.as_str(), wrong.is_empty(), missing.is_empty()) {
            ("unsound", right, _) => ["unsound-wrong", "unsound-right"][right as usize],
            (_, false, _) => "wrong",
            ("complete" | "none", true, false) => "missing",
            ("sound-only", true, false) => "sound-only-missing",
            ("complete" | "none" | "sound-only", true, true) => "agree",
            (other, _, _) => other,
        };
        let mut detail = format!("store: {} [{}] {:?}; {reasoner}:", a.label, a.paths, a.rows);
        detail += &format!(" {expected:?}");
        detail += &format!("; wrong {wrong:?}; missing {missing:?}");
        detail += &format!("; unchecked {}", unchecked.len());
        let keys = wrong.iter().map(|r| format!("wrong {r:?}"));
        let keys = keys.chain(missing.iter().map(|r| format!("missing {r:?}")));
        let keys = keys.chain([format!("{expected:?}")]).collect();
        (class.into(), detail, keys)
    }

    /// Removes groups while the verdict on query `i` stays `class` under HermiT.
    fn shrink(&self, i: usize, class: &str, reference: &mut Reference) -> BTreeSet<usize> {
        let picked = self.picked(i);
        let texts: Vec<String> = picked.iter().map(|&k| self.text(k)).collect();
        let mut skip = BTreeSet::new();
        loop {
            let before = skip.len();
            for g in 1..self.groups.len() {
                if skip.insert(g) {
                    let got = store(&self.commits(&skip), &texts);
                    let v = self.verdicts(&skip, &picked, &got, reference, "hermit");
                    if !v.iter().any(|(j, c, ..)| *j == i && c == class) {
                        skip.remove(&g);
                    }
                }
            }
            if skip.len() == before {
                return skip;
            }
        }
    }

    /// The shrunk case as one Turtle file: the query and what the store and both references
    /// gave, as comments, then the triples kept.
    fn report(&self, out: &Path, i: usize, skip: &BTreeSet<usize>, reference: &mut Reference) {
        let picked = self.picked(i);
        let texts: Vec<String> = picked.iter().map(|&k| self.text(k)).collect();
        let got = store(&self.commits(skip), &texts);
        let (seed, shape, name) = (self.seed, self.shape, self.name(i));
        let query = match i > self.queries.len() {
            true => "the commits of these triples".into(),
            false => self.text(i),
        };
        let steps = env("NRESE_DL_STEPS").map_or(String::new(), |n| format!(", {n} steps"));
        let mut ttl = format!("# seed {seed}, shape {shape}{steps}, {name} (query {i}): {query}\n");
        for reasoner in ["hermit", "openllet"] {
            for (j, class, detail, _) in self.verdicts(skip, &picked, &got, reference, reasoner) {
                let detail = detail.replace('\n', " ");
                ttl += &format!("# {reasoner}\t{}\t{class}\t{detail}\n", self.name(j));
            }
        }
        ttl += &format!("{PREFIXES}\n");
        let xsd = "http://www.w3.org/2001/XMLSchema#";
        let short = [
            (F, "f:"),
            (RDFS, "rdfs:"),
            (RDF, "rdf:"),
            (OWL, "owl:"),
            (xsd, "xsd:"),
        ];
        let short = |x: &str| {
            short.iter().fold(x.replace('>', ""), |x, (iri, p)| {
                x.replace(&format!("<{iri}"), p)
            })
        };
        for g in (0..self.groups.len()).filter(|g| !skip.contains(g)) {
            let (tag, turtle) = (self.groups[g].tag, short(&self.groups[g].turtle));
            let turtle = turtle.replace(" . ", " .\n");
            let gone = [", deleted in the last commit", ""][(self.deleted != Some(g)) as usize];
            ttl += &format!("# {tag}, commit {}{gone}\n{turtle}\n", self.commits[g] + 1);
        }
        std::fs::write(out.join(format!("fail-{seed}-{i}.ttl")), ttl).expect("the case");
    }
}

fn env(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

#[test]
#[ignore = "needs the DL kit's reference reasoners: benches/reasoning/dl/outside.sh"]
fn dl_answers_agree_with_outside_reasoners() {
    let command = std::env::var("NRESE_DL_REFERENCE").expect("NRESE_DL_REFERENCE");
    let out = PathBuf::from(std::env::var("NRESE_DL_OUT").expect("NRESE_DL_OUT"));
    std::fs::create_dir_all(&out).expect("NRESE_DL_OUT");
    let first = env("NRESE_FUZZ_SEED").unwrap_or(0);
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(50);
    let workers = env("NRESE_DL_WORKERS").unwrap_or(1);
    let wait = Duration::from_secs(env("NRESE_DL_WAIT").unwrap_or(60));
    let file = |name: &str| Mutex::new(std::fs::File::create(out.join(name)).expect(name));
    let (summary, unwritten) = (&file("summary.tsv"), &file("unwritten.tsv"));
    let shrunk = Mutex::new(BTreeMap::<String, usize>::new());
    let failures = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for w in 0..workers {
            let (out, shrunk, failures) = (&out, &shrunk, &failures);
            let mut reference = Reference::new(command.clone(), wait);
            s.spawn(move || {
                for seed in (first + w..first + cases).step_by(workers as usize) {
                    let case = generate(seed);
                    let tags: BTreeSet<&str> = case.groups.iter().map(|g| g.tag).collect();
                    let tags = tags.into_iter().collect::<Vec<_>>().join(",");
                    let tags = tags + ["", ",deletion"][case.deleted.is_some() as usize];
                    if case.reference_gap() {
                        let why = "a key over a property that isn't simple";
                        let row = format!("{seed}\t{}\t{tags}\tcase\treference-gap", case.shape);
                        let mut file = summary.lock().expect("summary");
                        writeln!(file, "{row}\treference-gap\t{why}").expect("summary.tsv");
                        continue;
                    }
                    let (none, picked) = (BTreeSet::new(), case.picked(case.queries.len() + 1));
                    let texts: Vec<String> = picked.iter().map(|&k| case.text(k)).collect();
                    let got = store(&case.commits(&none), &texts);
                    let hermit = case.verdicts(&none, &picked, &got, &mut reference, "hermit");
                    let twice = |c: &String| fails(c) || c.starts_with("declined");
                    let control = case.queries.iter().any(|q| q.alone == Some(false));
                    let openllet = match control || hermit.iter().any(|(_, c, ..)| twice(c)) {
                        true => case.verdicts(&none, &picked, &got, &mut reference, "openllet"),
                        false => Vec::new(),
                    };
                    for v in &hermit {
                        let (i, class, detail) = (&v.0, &v.1, &v.2);
                        let openllet = openllet.iter().find(|o| o.0 == *i);
                        let alone = case.queries.get(*i).and_then(|q| q.alone);
                        let outcome = outcome(v, openllet, alone);
                        let (shape, name) = (case.shape, case.name(*i));
                        let row = format!("{seed}\t{shape}\t{tags}\t{name}\t{class}\t{outcome}");
                        let second =
                            openllet.map_or("not asked".into(), |o| format!("{}, {}", o.1, o.2));
                        let row = format!("{row}\t{detail}; openllet: {second}");
                        writeln!(summary.lock().expect("summary"), "{row}").expect("summary.tsv");
                        // Written whole, to be read by hand: it shrinks against HermiT alone.
                        if outcome == "set-aside" || outcome == "control-split" {
                            case.report(out, *i, &BTreeSet::new(), &mut reference);
                        }
                        if outcome.starts_with("FAIL") || outcome == "control-split" {
                            failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                        if outcome.starts_with("FAIL") {
                            // Shrunk: 20 per verdict and query; written: 3 per template set left;
                            // every other failure listed in unwritten.tsv, with why.
                            let count = |key: String| {
                                let mut seen = shrunk.lock().expect("shrunk");
                                *seen.entry(key).and_modify(|n| *n += 1).or_insert(1)
                            };
                            let mut why = "20 of its verdict and query were shrunk".to_owned();
                            if count(format!("{class} {name}")) <= 20 {
                                let skip = case.shrink(*i, class, &mut reference);
                                let left = case.kept(&skip).map(|g| g.tag);
                                let left = left.filter(|t| !["axiom", "declarations"].contains(t));
                                let left: Vec<&str> = left.collect();
                                if count(format!("{class} {name} {}", left.join(","))) <= 3 {
                                    case.report(out, *i, &skip, &mut reference);
                                    continue;
                                }
                                why = format!("3 shrunk to {} were written", left.join(","));
                            }
                            let mut file = unwritten.lock().expect("unwritten");
                            writeln!(file, "{seed}\t{name}\t{class}\t{why}").expect("unwritten");
                        }
                    }
                }
            });
        }
    });
    let failures = failures.into_inner();
    let why = "answers differ from what both outside reasoners entail";
    assert_eq!(failures, 0, "{failures} {why}: {}", out.display());
}

/// Planted answers, with a stand-in reference that calls all data consistent and entails
/// nothing. A refusal is read by its kind: one declined as undecided is counted as such,
/// neither a pass nor a wrong verdict; one of a kind the check doesn't know fails the run.
/// A read is judged by its label, the inferred statements' too: a row the references don't
/// entail is counted under `unsound` and fails under any other label. A refusal as undecided
/// of declarations alone (no pun, no annotation property's axiom) fails, apart by its reason.
/// A row missing under `complete` fails, and a wrong one beside a blank node.
#[test]
#[cfg(unix)]
fn planted_answers_are_judged_by_kind_and_label() {
    let command = r#"awk -W interactive -F'\t' '{ printf "c"; for (i = 3; i <= NF; i++)
        if ($i != "") printf "0"; print ""; fflush() }'"#;
    let mut nothing = Reference::new(command.to_owned(), Duration::from_secs(10));
    let (case, none) = (generate(0), BTreeSet::new());
    let mut verdict = |picked: &[usize], got: &Answers| {
        let v = case.verdicts(&none, picked, got, &mut nothing, "hermit");
        v[0].1.clone()
    };
    let refused = |kind: &str| Err((kind.to_owned(), "planted".to_owned(), String::new()));
    assert_eq!(verdict(&[], &refused("Undecided")), "declined-consistent");
    assert_eq!(verdict(&[], &refused("Inconsistent")), "wrong-refusal");
    assert!(fails(&verdict(&[], &refused("Planted"))));
    let rows = BTreeSet::from([vec![n("a", 0), format!("{RDF}type"), n("C", 0)]]);
    let read = |label: &str| {
        let (label, paths, rows) = (label.to_owned(), String::new(), rows.clone());
        Ok((vec![Ok(Answer { label, paths, rows })], String::new()))
    };
    let inferred = [case.queries.len()];
    assert_eq!(verdict(&inferred, &read("unsound")), "unsound-wrong");
    for label in ["sound-only", "complete", "not-certified"] {
        assert_eq!(verdict(&inferred, &read(label)), "wrong", "{label}");
    }
    let plain: BTreeSet<usize> = (1..case.groups.len()).collect();
    let mut undecided = |why: &str| {
        let got = Err(("Undecided".to_owned(), why.to_owned(), String::new()));
        let v = case.verdicts(&plain, &[], &got, &mut nothing, "hermit");
        v[0].1.clone()
    };
    assert_eq!(undecided("reads differently"), "undecided-unfounded");
    assert_eq!(undecided("within its budget"), "undecided-unchecked");
    let keyed = |key: &str| {
        let mut case = generate(0);
        case.groups.truncate(1);
        let chain = "f:p3 owl:propertyChainAxiom ( f:p1 f:p2 ) .";
        case.groups[0].turtle = format!("{chain} f:C1 owl:hasKey ( {key} ) .");
        case.reference_gap()
    };
    assert!(keyed("f:p3") && !keyed("f:p0"));
    let c = generate(173); // three same-ask queries, two data-ask
    let names: BTreeSet<String> = (0..c.queries.len() + 2).map(|i| c.name(i)).collect();
    assert_eq!(names.len(), c.queries.len() + 2, "{names:?}");
    // Answers to a type query over a0..a3, judged against what is planted as entailed.
    let mut typed = generate(0);
    typed.queries = vec![query("type", "T f:C0 x", &mut Rng::new(0))];
    let a = |i: u64| n("a", i);
    let judge = |served: &[String], yes: &[String]| -> Verdict {
        let (label, paths) = ("complete".to_owned(), String::new());
        let rows = served.iter().map(|x| vec![x.clone()]).collect();
        let answer = Answer { label, paths, rows };
        let rows = typed.rows(0, &answer, &(0..4).map(a).collect());
        let e = rows.iter().map(|r| (r.1[0].clone(), yes.contains(&r.0[0])));
        let (class, detail, keys) = Case::judge(&answer, &rows, &e.collect(), "hermit");
        (0, class, detail, keys)
    };
    assert_eq!(judge(&[], &[a(0)]).1, "missing");
    // A wrong named row beside a blank node: the one judged, the other counted unchecked.
    let v = judge(&[a(1), "?_:b0".into()], &[]);
    assert_eq!(v.1, "wrong", "{v:?}");
    assert!(v.2.ends_with("unchecked 1"), "{v:?}");
    // Two references fail an answer alike only on a row in common.
    let missed = |yes: &[String]| judge(&[a(0)], yes);
    let (one, two) = (missed(&[a(0), a(1)]), missed(&[a(0), a(2)]));
    assert_eq!(outcome(&one, Some(&two), None), "set-aside");
    let three = missed(&[a(0), a(1), a(2)]);
    assert_eq!(outcome(&one, Some(&three), None), "FAIL");
    let unjudged = (0, "not-judged".to_owned(), String::new(), BTreeSet::new());
    assert_eq!(outcome(&unjudged, None, None), "not-judged");
}

/// The real references, for the planted faults below (run by outside.sh before a campaign).
fn references() -> Reference {
    let command = std::env::var("NRESE_DL_REFERENCE").expect("NRESE_DL_REFERENCE");
    Reference::new(command, Duration::from_secs(60))
}

/// Writes the store gets wrong (planted): a deletion left undone (a `DELETE DATA` that removes
/// nothing), and an insert lost before a deletion of other triples. Each shows at the answer,
/// as the references read the state the writes should leave; the writes done agree.
#[test]
#[ignore = "needs the DL kit's reference reasoners: benches/reasoning/dl/outside.sh"]
fn planted_writes_lost() {
    let (mut case, none) = (generate(0), BTreeSet::new());
    case.groups.truncate(3);
    case.groups[0].turtle = "f:C0 rdfs:subClassOf f:C1 . f:C1 rdfs:subClassOf f:C2 .".into();
    case.groups[1].turtle = "f:a0 a f:C0 .".into();
    case.groups[2].turtle = "f:a1 a f:C0 .".into();
    case.queries = vec![query("type", "T f:C2 x", &mut Rng::new(0))];
    (case.commits, case.deleted) = (vec![0, 0, 0], Some(1));
    let (mut reference, texts) = (references(), vec![case.text(0)]);
    let mut verdict = |commits: Vec<(String, String)>| {
        let got = store(&commits, &texts);
        let v = case.verdicts(&none, &[0], &got, &mut reference, "hermit");
        v[0].1.clone()
    };
    let (mut undone, mut lost) = (case.commits(&none), case.commits(&none));
    undone[1].0 = "DELETE DATA { }".into();
    lost[0].0 = lost[0].0.replace("f:a1 a f:C0 .", "");
    assert_eq!(verdict(case.commits(&none)), "agree");
    assert_eq!(verdict(undone), "wrong");
    assert_eq!(verdict(lost), "missing");
}

/// A long chain's clash the store accepted (planted: a `complete` answer over the chain of
/// 260 properties and a negative assertion it contradicts) is a wrong acceptance.
#[test]
#[ignore = "needs the DL kit's reference reasoners: benches/reasoning/dl/outside.sh"]
fn planted_long_chain_clash_accepted() {
    let clash = "f:p3 a owl:ObjectProperty . [] a owl:NegativePropertyAssertion ; \
        owl:sourceIndividual f:a0 ; owl:assertionProperty f:p3 ; owl:targetIndividual f:a1 .";
    let (label, paths, rows) = ("complete".to_owned(), String::new(), BTreeSet::new());
    let state = format!("{} {clash}", long_chain(260));
    let accepted = Ok((vec![Ok(Answer { label, paths, rows })], state));
    let (case, none) = (generate(0), BTreeSet::new());
    let v = case.verdicts(&none, &[0], &accepted, &mut references(), "hermit");
    assert_eq!(v[0].1, "wrong-acceptance", "{v:?}");
}
