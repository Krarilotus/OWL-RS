//! What the bounds' tests share: a term table that serves the OWL reader, the writer and
//! the rule reasoner alike; L by the reasoner's OWL 2 RL ruleset; U1 by the reasoner
//! with the compiled program.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use nrese_dl::bounds::{self, Program, Slot};
use nrese_owl::{Make, Normalised, Ontology, Statement, Term, TermKind, Terms};
use nrese_rdf::{BlankNode, Literal, NamedNode, Term as RdfTerm, Triple as RdfTriple};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_reasoner::batch;
use nrese_reasoner::eval::Schema;
use nrese_reasoner::ir::{self, Vocabulary};
use nrese_reasoner::lists::ListVocabulary;
use nrese_reasoner::rulesets::Ruleset;

pub type Triple = [u64; 3];

/// Terms by dense id, for every component at once.
#[derive(Default)]
pub struct Table {
    pub terms: Vec<RdfTerm>,
    ids: HashMap<RdfTerm, u64>,
    blanks: u64,
}

impl Table {
    pub fn id(&mut self, term: RdfTerm) -> u64 {
        if let Some(&id) = self.ids.get(&term) {
            return id;
        }
        let id = self.terms.len() as u64;
        self.terms.push(term.clone());
        self.ids.insert(term, id);
        id
    }

    pub fn named(&mut self, iri: &str) -> u64 {
        self.id(NamedNode::new_unchecked(iri).into())
    }

    /// The N-Triples form of a term.
    pub fn text(&self, id: u64) -> String {
        match &self.terms[id as usize] {
            RdfTerm::BlankNode(_) => format!("_:b{id}"),
            other => other.to_string(),
        }
    }

    /// The triples of an RDF document.
    pub fn parse(&mut self, format: RdfFormat, text: &[u8]) -> Vec<Triple> {
        RdfParser::from_format(format)
            .for_reader(text)
            .map(|quad| {
                let t = RdfTriple::from(quad.expect("the test data parses"));
                [
                    self.id(t.subject.into()),
                    self.id(t.predicate.into()),
                    self.id(t.object),
                ]
            })
            .collect()
    }
}

impl Terms for Table {
    fn kind(&self, term: Term) -> TermKind {
        match &self.terms[term as usize] {
            RdfTerm::NamedNode(_) => TermKind::Iri,
            RdfTerm::BlankNode(_) => TermKind::Blank,
            _ => TermKind::Literal,
        }
    }

    fn lexical(&self, term: Term) -> Option<String> {
        match &self.terms[term as usize] {
            RdfTerm::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        }
    }

    fn iri(&self, iri: &str) -> Option<Term> {
        self.ids
            .get(&RdfTerm::NamedNode(NamedNode::new_unchecked(iri)))
            .copied()
    }
}

impl Make for Table {
    fn blank(&mut self) -> Term {
        self.blanks += 1;
        self.id(BlankNode::new_unchecked(format!("w{}", self.blanks)).into())
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> Term {
        self.id(Literal::new_typed_literal(lexical, NamedNode::new_unchecked(datatype)).into())
    }
}

impl Vocabulary for Table {
    fn iri(&mut self, iri: &str) -> u64 {
        self.named(iri)
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        Make::literal(self, lexical, datatype)
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        self.id(Literal::new_language_tagged_literal_unchecked(lexical, language).into())
    }
}

/// The ontology of `triples` and its clauses. The OWL vocabulary gets ids first, as a
/// store's dictionary has them: the reader looks terms up, and a term the input
/// doesn't name (`rdfs:Literal` for a data cardinality) would otherwise be missing, and
/// the axiom with it (reported to `nrese-owl`'s owner: dropped without a diagnostic).
pub fn read(table: &mut Table, triples: &[Triple]) -> (Ontology, Normalised) {
    for (_, iri) in nrese_owl::Vocabulary::iris() {
        table.named(&iri);
    }
    let table: &Table = table;
    let statements: Vec<Statement> = triples
        .iter()
        .map(|&triple| Statement { triple, graph: 0 })
        .collect();
    let ontology = nrese_owl::read(&statements, table);
    let normalised = nrese_owl::normalise(&ontology);
    (ontology, normalised)
}

/// U1 for `triples`' ontology.
pub fn compile(table: &mut Table, ontology: &Ontology, normalised: &Normalised) -> Program {
    bounds::compile(ontology, normalised, &mut |iri| table.named(iri))
}

/// [`compile`] with `options`.
pub fn compile_with(
    table: &mut Table,
    ontology: &Ontology,
    normalised: &Normalised,
    options: bounds::Options,
) -> Program {
    bounds::compile_with(ontology, normalised, options, &mut |iri| table.named(iri))
}

/// `owl:sameAs` classes: each member's representative (the smallest id) and each
/// representative's members (classes of two or more only).
#[derive(Debug, Default, Clone)]
pub struct Classes {
    representative: HashMap<u64, u64>,
    members: HashMap<u64, Vec<u64>>,
}

impl Classes {
    /// The representative of `term` (the term itself outside every class).
    pub fn representative(&self, term: u64) -> u64 {
        self.representative.get(&term).copied().unwrap_or(term)
    }

    /// The members of a representative's class (empty for a term of no class).
    pub fn members(&self, representative: u64) -> &[u64] {
        self.members.get(&representative).map_or(&[], Vec::as_slice)
    }

    fn rewrite(&self, t: Triple) -> Triple {
        t.map(|x| self.representative(x))
    }

    /// Merges the classes of each pair; whether any class changed.
    fn merge(&mut self, pairs: impl Iterator<Item = (u64, u64)>) -> bool {
        let mut changed = false;
        for (a, b) in pairs {
            let (ra, rb) = (self.representative(a), self.representative(b));
            if ra == rb {
                continue;
            }
            changed = true;
            let (keep, gone) = (ra.min(rb), ra.max(rb));
            let moved = self.members.remove(&gone).unwrap_or_else(|| vec![gone]);
            for &m in &moved {
                self.representative.insert(m, keep);
            }
            self.representative.insert(keep, keep);
            let class = self.members.entry(keep).or_insert_with(|| vec![keep]);
            class.extend(moved);
            class.sort_unstable();
        }
        changed
    }
}

/// A closure: the input with what was derived, and the consistency rules that fired.
/// `representatives` and `classes` are for a closure over `owl:sameAs` representatives;
/// with equality by copying they are the facts and no classes.
#[derive(Default)]
pub struct Closure {
    pub facts: Vec<Triple>,
    pub violations: Vec<String>,
    pub representatives: Vec<Triple>,
    pub classes: Classes,
}

/// L: the OWL 2 RL closure of `input`.
pub fn lower(table: &mut Table, input: &[Triple]) -> Closure {
    let rules = Ruleset::Owl2Rl.rules(table).expect("OWL 2 RL parses");
    let lists = ListVocabulary::new(table);
    let schema = Schema::owl(table);
    let m = batch::materialise(input, &rules, Some(&lists), &schema);
    let mut facts = input.to_vec();
    facts.extend(m.derived);
    facts.sort_unstable();
    facts.dedup();
    Closure {
        facts,
        violations: m.violations.into_iter().map(|v| v.rule).collect(),
        ..Closure::default()
    }
}

fn slot(s: Slot) -> ir::Term {
    match s {
        Slot::Var(v) => ir::Term::Var(v),
        Slot::Const(t) => ir::Term::Const(t),
    }
}

/// U1's rules as the reasoner's.
pub fn reasoner_rules(program: &Program) -> Vec<ir::Rule> {
    let atom = |a: &bounds::Atom| ir::Atom(a.0.map(slot));
    program
        .rules
        .iter()
        .map(|r| ir::Rule {
            name: r.name.clone(),
            body: r.body.iter().map(atom).collect(),
            guards: r
                .distinct
                .iter()
                .map(|&(a, b)| ir::Guard::NotEqual(slot(a), slot(b)))
                .collect(),
            head: ir::Head::Facts(r.head.iter().map(atom).collect()),
        })
        .collect()
}

/// The input with U1's facts.
pub fn upper_input(program: &Program, input: &[Triple]) -> Vec<Triple> {
    let mut input = input.to_vec();
    input.extend(program.facts.iter().map(|(f, _)| *f));
    input.sort_unstable();
    input.dedup();
    input
}

/// U1's closure of `input` (the data alone, or L's closure); see [`upper_within`].
pub fn upper(table: &mut Table, program: &Program, input: &[Triple]) -> Closure {
    upper_within(table, program, input, None).expect("no budget, so never given up")
}

/// U1's closure of `input`, with equality by representatives, within a time budget:
/// `None` if it gave up. The budget bounds the time, not the memory (the reasoner polls
/// it between rule jobs); U1's construction bounds that (`Options::max_skolems`).
///
/// **Equality.** By copying (OWL 2 RL's `eq-rep-*`, the reasoner's equality module), a
/// class of `k` terms costs `k²` copies of each fact of it in every round its class
/// grows, and U1 makes large classes: a split `⊤ ⊑ {d} ⊔ …` makes everything equal to
/// `d` (W3C's `WebOnt-description-logic-906`: 4 GB and more). So the closure is over
/// representatives (the store's way): each class is one term, the smallest id, facts
/// are rewritten to it, and the rules run without `eq-rep-*` until no class grows.
/// `representatives::materialise` does the same but never stops on U1: a rule head
/// naming a constant that is no representative (a Skolem constant) derives
/// `rep sameAs c` again in every closure, which it takes for a new equality (reported
/// to the reasoner's owner); here only a class that grows counts.
///
/// `facts` is the closure expanded to every member that is no internal term of U1 (and
/// each representative): the answers, as copying would give them, without the copies
/// over Skolem constants nobody reads.
pub fn upper_within(
    table: &mut Table,
    program: &Program,
    input: &[Triple],
    budget: Option<Duration>,
) -> Option<Closure> {
    let same_as = program.names.same_as;
    if std::env::var_os("NRESE_BOUNDS_COPYING").is_some() {
        // Equality by copying, to compare the two (small inputs only).
        let input = upper_input(program, input);
        let facts = upper_copied(table, &reasoner_rules(program), &input);
        return Some(Closure {
            representatives: facts.clone(),
            facts,
            ..Closure::default()
        });
    }
    let rules: Vec<ir::Rule> = reasoner_rules(program)
        .into_iter()
        .filter(|r| !matches!(r.name.as_str(), "eq-rep-s" | "eq-rep-p" | "eq-rep-o"))
        .collect();
    let schema = Schema::owl(table);
    let deadline = budget.map(|b| Instant::now() + b);
    let stop = move || deadline.is_some_and(|d| Instant::now() >= d);
    let pairs = |facts: &[Triple]| -> Vec<(u64, u64)> {
        facts
            .iter()
            .filter(|t| t[1] == same_as && t[0] != t[2])
            .map(|t| (t[0], t[2]))
            .collect()
    };
    let mut classes = Classes::default();
    let mut facts = upper_input(program, input);
    classes.merge(pairs(&facts).into_iter());
    let closure = loop {
        let mut rewritten: Vec<Triple> = facts.iter().map(|&t| classes.rewrite(t)).collect();
        rewritten.sort_unstable();
        rewritten.dedup();
        let m =
            batch::materialise_owned_until(rewritten.clone(), &rules, None, &schema, &stop).ok()?;
        let mut closure = rewritten;
        closure.extend(m.derived);
        // Facts a head with a merged constant derived, over its representative.
        if !classes.merge(pairs(&closure).into_iter()) {
            if classes.members.is_empty() {
                closure.sort_unstable();
                break closure;
            }
            let mut closure: Vec<Triple> = closure.iter().map(|&t| classes.rewrite(t)).collect();
            closure.sort_unstable();
            closure.dedup();
            break closure;
        }
        facts = closure;
    };
    if classes.members.is_empty() {
        return Some(Closure {
            facts: closure.clone(),
            violations: Vec::new(),
            representatives: closure,
            classes,
        });
    }
    let shown = |r: u64| -> Vec<u64> {
        match classes.members(r) {
            [] => vec![r],
            members => members
                .iter()
                .copied()
                .filter(|&m| m == r || !program.is_internal(m))
                .collect(),
        }
    };
    let mut expanded = Vec::with_capacity(closure.len());
    for &[s, p, o] in &closure {
        for s in shown(s) {
            for &p in &shown(p) {
                for &o in &shown(o) {
                    expanded.push([s, p, o]);
                }
            }
        }
    }
    expanded.sort_unstable();
    expanded.dedup();
    Some(Closure {
        facts: expanded,
        violations: Vec::new(),
        representatives: closure,
        classes,
    })
}

/// U1's closure by copying (OWL 2 RL's equality rules), every fact of every member.
pub fn upper_copied(table: &mut Table, rules: &[ir::Rule], input: &[Triple]) -> Vec<Triple> {
    let schema = Schema::owl(table);
    let m = batch::materialise(input, rules, None, &schema);
    let mut facts = input.to_vec();
    facts.extend(m.derived);
    facts.sort_unstable();
    facts.dedup();
    facts
}
