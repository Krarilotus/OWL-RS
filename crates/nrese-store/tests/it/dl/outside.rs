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
/// simple property, `{Q}` one that isn't, `{P}` any; `{D}` a data property; `{A1}`, `{A2}` two
/// individuals; `{V}` an integer, `{W}` it in another lexical form, `{X}` any in any form.
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
    let queries = queries(&mut r);
    // One case in eight, by a draw of its own: `f:p3` as a chain of 260 fresh properties (C2).
    if Rng::new(!seed).one_in(8) {
        add("long-chain", long_chain(260), Vec::new());
    }
    // Drawn after the queries, so a seed keeps the ontology and queries of the runs before.
    let commits = (0..groups.len())
        .map(|g| (g > 0 && r.one_in(2)) as usize)
        .collect();
    let plain = |g: &Group| !["_:", "[", "("].iter().any(|b| g.turtle.contains(b));
    let deletable: Vec<usize> = (1..groups.len()).filter(|&g| plain(&groups[g])).collect();
    let deleted = (!deletable.is_empty() && r.one_in(2))
        .then(|| deletable[r.below(deletable.len() as u64) as usize]);
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
        ("{P}", format!("f:p{}", r.below(4))),
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
        out.push(Query {
            name,
            values,
            pos,
            neg,
        });
    }
    out
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

/// The store's answers to `texts` after `commits` (each an update and the state after it; a
/// deletion's, the group it takes away).
fn store(commits: &[(String, String)], texts: &[String]) -> Answers {
    let dl = pipeline_with(DlConfig {
        timeout: Some(Duration::from_secs(5)),
        max_nodes: Some(100_000),
        max_branch_points: Some(100_000),
        ..DlConfig::default()
    });
    let mut state = String::new();
    for (write, after) in commits {
        // A deletion leaves what the store held less the group's triples, as the store
        // spells them: the reference reads that, so a deletion left undone shows.
        state = if write.starts_with("DELETE") {
            let p = PREFIXES.replace("@prefix", "PREFIX").replace("> .", ">");
            let gone = held(&dl, &format!("{p} CONSTRUCT {{ {after} }} WHERE {{}}"))?;
            let mut all = held(&dl, "CONSTRUCT WHERE { ?s ?p ?o }")?;
            all.retain(|t| !gone.contains(t));
            all.join("\n")
        } else {
            after.clone()
        };
        if let Err(e) = update(&dl, &format!("PREFIX f: <{F}> PREFIX rdf: <{RDF}> {write}")) {
            let kind = match &e {
                MutationError::Rejected(r) => format!("{:?}", r.kind),
                other => format!("{other:?}"),
            };
            return Err((kind, e.to_string(), state));
        }
    }
    Ok((texts.iter().map(|q| answer(&dl, q)).collect(), state))
}

/// The triples of `construct` over the store's asserted statements, one a line.
fn held(dl: &MutationPipeline, construct: &str) -> Result<Vec<String>, (String, String, String)> {
    let mut request = SparqlQueryRequest::all(construct.to_owned());
    request.read_model = Some(nrese_store::ReadModel::Asserted);
    let (s, mut out) = (dl.store(), Vec::new());
    let prepared = s.prepare_query(&request);
    let read = prepared.and_then(|p| run_dl(s, &p, &mut out));
    read.map_err(|e| ("read".to_owned(), e.to_string(), String::new()))?;
    let held = String::from_utf8_lossy(&out);
    Ok(held.lines().map(str::to_owned).collect())
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
        // One request a line: a state read back from the store comes one triple a line.
        let turtle = turtle.replace('\n', " ");
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
    )
}

/// Per candidate row of a query: the row, its positive questions and its negated ones.
type Rows = Vec<(Vec<String>, Vec<String>, Vec<String>)>;

impl Case {
    fn kept<'a>(&'a self, skip: &'a BTreeSet<usize>) -> impl Iterator<Item = &'a Group> {
        let all = 0..self.groups.len();
        all.filter(move |i| !skip.contains(i))
            .map(move |i| &self.groups[i])
    }

    /// The commits under `skip`, each an update and the state after it as Turtle: the groups
    /// of the first commit, those of the second, then the deletion of `deleted`, with the
    /// group it takes away in place of the state; only the first NRESE_DL_STEPS if it is set.
    fn commits(&self, skip: &BTreeSet<usize>) -> Vec<(String, String)> {
        let turtle = |keep: &dyn Fn(usize) -> bool| {
            let kept = (0..self.groups.len()).filter(|&g| !skip.contains(&g) && keep(g));
            kept.map(|g| self.groups[g].turtle.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        let mut out = Vec::new();
        for c in 0..2 {
            let write = turtle(&|g| self.commits[g] == c);
            if !write.is_empty() {
                let state = turtle(&|g| self.commits[g] <= c);
                out.push((format!("INSERT DATA {{ {write} }}"), state));
            }
        }
        if let Some(d) = self.deleted.filter(|d| !skip.contains(d)) {
            let turtle = &self.groups[d].turtle;
            out.push((format!("DELETE DATA {{ {turtle} }}"), turtle.clone()));
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

    fn name(&self, i: usize) -> &'static str {
        match self.queries.get(i) {
            Some(q) => q.name,
            None if i == self.queries.len() => "inferred",
            None => "write",
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
            range.extend(a.rows.iter().map(|r| r[k].clone()));
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
    ) -> Vec<(usize, String, String)> {
        let write = self.queries.len() + 1;
        let (answers, turtle) = match got {
            Err((kind, why, state)) => {
                let (class, said) = match reference.ask(reasoner, state, &BTreeSet::new()) {
                    Reply::Inconsistent => ("agree", "inconsistent".to_owned()),
                    Reply::Answers(_) => ("wrong-refusal", "consistent".to_owned()),
                    Reply::Undecided(why) => ("reference-undecided", why),
                };
                // Declined as undecided: honest, so no failure, but never a pass; a kind the
                // check doesn't know fails the run.
                let class = match kind.as_str() {
                    "Inconsistent" => class.to_owned(),
                    "Undecided" if class == "reference-undecided" => "declined-no-answer".into(),
                    "Undecided" => format!("declined-{said}"),
                    _ => "unclassified".into(),
                };
                let detail = format!("store: refused, {kind} ({why}); {reasoner}: {said}");
                return vec![(write, class, detail + "; no query judged")];
            }
            Ok((answers, state)) => (answers, state),
        };
        let names = self.kept(skip).flat_map(|g| g.names.clone());
        let individuals: BTreeSet<String> = names.collect();
        // A row with a blank node or another literal leaves a query unjudged, but the inferred.
        let odd = |a: &&Answer| a.rows.iter().flatten().any(|x| x.starts_with('?'));
        let (mut asked, mut per, last) = (BTreeSet::new(), Vec::new(), self.queries.len());
        for (&i, a) in picked.iter().zip(answers) {
            let a = a.as_ref().ok().filter(|a| i == last || !odd(a));
            let rows = a.map(|a| self.rows(i, a, &individuals));
            for (_, p, n) in rows.iter().flatten() {
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
                return vec![(write, class.into(), detail)];
            }
            Reply::Undecided(why) => {
                return picked
                    .iter()
                    .map(|&i| (i, "reference-undecided".into(), why.clone()))
                    .collect();
            }
        };
        let judged = picked
            .iter()
            .zip(answers)
            .zip(per)
            .map(|((&i, a), rows)| match (a, rows) {
                (Ok(a), Some(rows)) => {
                    let (class, detail) = self.judge(i, a, &rows, &entailed, reasoner);
                    (i, class, detail)
                }
                (Err(why), _) => (i, "not-judged".into(), why.clone()),
                (Ok(_), None) => (i, "not-judged".into(), "a blank node or literal".into()),
            });
        judged.collect()
    }

    /// The class of the store's answer `a` to query `i`, given its candidate rows and what
    /// `reasoner` entails, and what each side gave.
    fn judge(
        &self,
        i: usize,
        a: &Answer,
        rows: &Rows,
        entailed: &BTreeMap<String, bool>,
        reasoner: &str,
    ) -> (String, String) {
        let inferred = i == self.queries.len();
        let all = |qs: &[String]| qs.iter().all(|q| entailed[q]);
        let holds = rows
            .iter()
            .filter(|(_, p, n)| all(p) && (n.is_empty() || !all(n)));
        let expected: BTreeSet<&Vec<String>> = holds.map(|(t, _, _)| t).collect();
        // An inferred statement is judged only where it is an OWL axiom over the case's names.
        let asked = |r: &Vec<String>| !inferred || rows.iter().any(|(t, _, _)| t == r);
        let wrong: Vec<_> = a
            .rows
            .iter()
            .filter(|r| asked(r) && !expected.contains(r))
            .collect();
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
        (class.into(), detail)
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
                    if !v.iter().any(|(j, c, _)| *j == i && c == class) {
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
            for (j, class, detail) in self.verdicts(skip, &picked, &got, reference, reasoner) {
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
                    let (none, picked) = (BTreeSet::new(), case.picked(case.queries.len() + 1));
                    let texts: Vec<String> = picked.iter().map(|&k| case.text(k)).collect();
                    let got = store(&case.commits(&none), &texts);
                    let hermit = case.verdicts(&none, &picked, &got, &mut reference, "hermit");
                    let twice = |c: &String| fails(c) || c.starts_with("declined");
                    let openllet = match hermit.iter().any(|(_, c, _)| twice(c)) {
                        true => case.verdicts(&none, &picked, &got, &mut reference, "openllet"),
                        false => Vec::new(),
                    };
                    let tags: BTreeSet<&str> = case.groups.iter().map(|g| g.tag).collect();
                    let tags = tags.into_iter().collect::<Vec<_>>().join(",");
                    for (i, class, detail) in &hermit {
                        let second = openllet.iter().find(|(j, _, _)| j == i).map(|(_, c, _)| c);
                        let outcome = match (fails(class), second) {
                            // A declined write: the class is HermiT's word, the outcome Openllet's.
                            (false, Some(c)) if class.starts_with("declined") => c.as_str(),
                            (false, _) => "pass",
                            (true, Some(c)) if c == class => "FAIL",
                            (true, _) => "set-aside",
                        };
                        let (shape, name) = (case.shape, case.name(*i));
                        let row = format!("{seed}\t{shape}\t{tags}\t{name}");
                        let row = format!("{row}\t{class}\t{outcome}\t{detail}");
                        writeln!(summary.lock().expect("summary"), "{row}").expect("summary.tsv");
                        // Written whole, to be read by hand: it shrinks against HermiT alone.
                        if outcome == "set-aside" {
                            case.report(out, *i, &BTreeSet::new(), &mut reference);
                        }
                        if outcome == "FAIL" {
                            failures.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
/// entail is counted under `unsound` and fails under any other label.
#[test]
#[cfg(unix)]
fn planted_answers_are_judged_by_kind_and_label() {
    let command = r#"awk -F'\t' '{ printf "c"; for (i = 3; i <= NF; i++) if ($i != "") printf "0"
        print ""; fflush() }'"#;
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
}

/// The real references, for the planted faults below (run by outside.sh before a campaign).
fn references() -> Reference {
    let command = std::env::var("NRESE_DL_REFERENCE").expect("NRESE_DL_REFERENCE");
    Reference::new(command, Duration::from_secs(60))
}

/// A deletion the store leaves undone (planted: a `DELETE DATA` that removes nothing) shows
/// as an inferred statement the references don't entail; the deletion done passes, its
/// state read back from the store over several lines.
#[test]
#[ignore = "needs the DL kit's reference reasoners: benches/reasoning/dl/outside.sh"]
fn planted_deletion_left_undone() {
    let (mut case, none) = (generate(0), BTreeSet::new());
    case.groups.truncate(2);
    case.groups[0].turtle = "f:C0 rdfs:subClassOf f:C1 . f:C1 rdfs:subClassOf f:C2 .".into();
    case.groups[1].turtle = "f:a0 a f:C0 .".into();
    (case.queries, case.commits, case.deleted) = (Vec::new(), vec![0, 0], Some(1));
    let (mut reference, texts) = (references(), vec![case.text(0)]);
    let mut verdict = |commits: Vec<(String, String)>| {
        let got = store(&commits, &texts);
        case.verdicts(&none, &[0], &got, &mut reference, "hermit")
    };
    let mut undone = case.commits(&none);
    undone[1].0 = "DELETE DATA { }".into();
    assert_eq!(verdict(case.commits(&none))[0].1, "agree");
    let v = verdict(undone);
    assert_eq!(v[0].1, "wrong", "{v:?}");
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
