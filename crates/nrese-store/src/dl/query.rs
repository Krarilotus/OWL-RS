//! The query path of the `owl2-dl` mode (docs/design/owl2-dl.md §8, "The query path"):
//! every answer is sound and says whether it is complete
//! ([`nrese_sparql::completeness::Completeness`]).
//!
//! 1. **Closed predicates.** Where U1 has no fact beyond L on any predicate (or class)
//!    the query reads, L's answers are the certain ones: the query runs once, over L.
//!    This holds for every operator, negation and aggregates included: they then read
//!    exactly the entailed facts.
//! 2. **Bounds.** Otherwise a monotone query (basic graph patterns, paths, joins, unions,
//!    filters, projection, `DISTINCT`, `ORDER BY`, `BIND`, `VALUES`) runs over L (`A_L`)
//!    and over L ∪ U1 (`A_U`, rows binding U1's own terms left out). `A_U ⊇` the certain
//!    answers ⊇ `A_L` (PAGOdA, Theorem 5.5 (ii)); where they agree, the answer is exact.
//! 3. **Exact services** on the gap `A_U \ A_L`, for a query that is one basic graph
//!    pattern with filters over its answer variables: a candidate row is instantiated,
//!    its ground atoms not in L are tested one by one (`ExactGroundEntailment`, by the
//!    DL engines: `¬α` makes the ontology inconsistent), and each connected part of its
//!    existential variables that is a tree is rolled up into a class expression, named
//!    terms as nominals (`ExactInternalisableCQ`). A row is proved (returned), refuted
//!    (dropped), or unresolved (dropped, counted). At most `dl.max_candidates` rows and
//!    `dl.timeout` per query.
//!
//! What can't be decided is said: an unresolved candidate, a non-monotone operator over
//! an open gap, U1 not available, consistency not proven, a reader who sees part of the
//! data. Bag semantics: L's rows keep their multiplicities; a proved row is added once.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use nrese_engine::{ReadModel, Snapshot, TermId};
use nrese_owl::{Axiom, ClassExpr, ExprId, Literal, ObjProp, Ontology};
use nrese_rdf::{NamedNodeRef, Term};
use nrese_sparql::CancellationToken;
use nrese_sparql::completeness::{Bounds as Counts, Completeness};
use nrese_sparql_syntax::Query;
use nrese_sparql_syntax::algebra::{GraphPattern, PropertyPathExpression};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern, Variable};
use nrese_sparql_syntax::visit::Node;
use rayon::prelude::*;

use super::bounds::View;
use super::consistency::{self, Verdict};
use super::entailment::{self, Entailed};
use super::{DlAnswers, DlStatus, gate, source};
use crate::query_executor::{Answers, PreparedQuery, evaluate_prepared};
use crate::{StoreError, StoreResult, StoreService};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const U1: &str = "urn:nrese:u1:";

/// What a query reads and whether it is monotone.
#[derive(Debug, Default)]
pub(crate) struct Analysis {
    /// `(predicate, class)` per triple pattern and path step: `None` for a variable (or a
    /// negated property set, which reads any predicate); the class for `rdf:type` with a
    /// constant object.
    reads: Vec<(Option<String>, Option<String>)>,
    /// The first operator that isn't monotone (or isn't under entailment).
    not_monotone: Option<&'static str>,
    /// The single basic graph pattern with filters the exact services take.
    shape: Option<Shape>,
}

/// A query that is one basic graph pattern under filters, with the answer variables.
#[derive(Debug, Clone)]
struct Shape {
    patterns: Vec<TriplePattern>,
    /// Variables the filters read.
    filtered: HashSet<String>,
    /// The answer variables (all of the pattern's for `SELECT *`; none for `ASK`).
    projected: Vec<Variable>,
}

fn path_reads(path: &PropertyPathExpression, out: &mut Vec<(Option<String>, Option<String>)>) {
    match path {
        PropertyPathExpression::NamedNode(n) => out.push((Some(n.as_str().to_owned()), None)),
        PropertyPathExpression::Reverse(p)
        | PropertyPathExpression::ZeroOrMore(p)
        | PropertyPathExpression::OneOrMore(p)
        | PropertyPathExpression::ZeroOrOne(p) => path_reads(p, out),
        PropertyPathExpression::Sequence(a, b) | PropertyPathExpression::Alternative(a, b) => {
            path_reads(a, out);
            path_reads(b, out);
        }
        PropertyPathExpression::NegatedPropertySet(_) => out.push((None, None)),
    }
}

fn pattern_reads(t: &TriplePattern) -> (Option<String>, Option<String>) {
    let predicate = match &t.predicate {
        NamedNodePattern::NamedNode(n) => Some(n.as_str().to_owned()),
        NamedNodePattern::Variable(_) => None,
    };
    let class = match (&predicate, &t.object) {
        (Some(p), TermPattern::NamedNode(c)) if p == RDF_TYPE => Some(c.as_str().to_owned()),
        _ => None,
    };
    (predicate, class)
}

fn variables_of(e: &nrese_sparql_syntax::algebra::Expression, out: &mut HashSet<String>) {
    e.find(&mut |node| {
        if let Node::Expression(nrese_sparql_syntax::algebra::Expression::Variable(v)) = node {
            out.insert(v.as_str().to_owned());
        }
        // Variables of EXISTS patterns count too: a filter with one isn't over the row.
        if let Node::Pattern(GraphPattern::Bgp { patterns }) = node {
            for t in patterns {
                for term in [&t.subject, &t.object] {
                    if let TermPattern::Variable(v) = term {
                        out.insert(v.as_str().to_owned());
                    }
                }
            }
        }
        false
    });
}

/// The basic graph pattern under filters of `pattern`, if that is all it is.
fn bgp_under_filters(pattern: &GraphPattern) -> Option<(Vec<TriplePattern>, HashSet<String>)> {
    match pattern {
        GraphPattern::Bgp { patterns } => Some((patterns.clone(), HashSet::new())),
        GraphPattern::Filter { expr, inner } => {
            let (patterns, mut filtered) = bgp_under_filters(inner)?;
            variables_of(expr, &mut filtered);
            Some((patterns, filtered))
        }
        _ => None,
    }
}

fn shape_of(query: &Query) -> Option<Shape> {
    let (pattern, ask) = match query {
        Query::Select { pattern, .. } => (pattern, false),
        Query::Ask { pattern, .. } => (pattern, true),
        _ => return None,
    };
    let mut p = pattern;
    let mut projected = None;
    loop {
        match p {
            GraphPattern::Distinct { inner } | GraphPattern::Reduced { inner } => p = inner,
            GraphPattern::Project { inner, variables } if projected.is_none() => {
                projected = Some(variables.clone());
                p = inner;
            }
            _ => break,
        }
    }
    let (patterns, filtered) = bgp_under_filters(p)?;
    let projected = match (ask, projected) {
        // An ASK's variables are all existential.
        (true, _) => Vec::new(),
        (false, Some(v)) => v,
        (false, None) => return None,
    };
    Some(Shape {
        patterns,
        filtered,
        projected,
    })
}

/// What `query` reads, whether it is monotone, and its shape for the exact services.
pub(crate) fn analyse(query: &Query) -> Analysis {
    let pattern = match query {
        Query::Select { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. }
        | Query::Ask { pattern, .. } => pattern,
    };
    let mut analysis = Analysis {
        shape: shape_of(query),
        ..Analysis::default()
    };
    if matches!(query, Query::Describe { .. }) {
        analysis.not_monotone = Some("DESCRIBE");
    }
    pattern.find(&mut |node| {
        match node {
            Node::Pattern(p) => {
                let op = match p {
                    GraphPattern::Bgp { patterns } => {
                        analysis.reads.extend(patterns.iter().map(pattern_reads));
                        None
                    }
                    GraphPattern::Path { path, .. } => {
                        path_reads(path, &mut analysis.reads);
                        None
                    }
                    GraphPattern::LeftJoin { .. } => Some("OPTIONAL"),
                    GraphPattern::Minus { .. } => Some("MINUS"),
                    GraphPattern::Lateral { .. } => Some("LATERAL"),
                    GraphPattern::Group { .. } => Some("an aggregate"),
                    GraphPattern::Slice { .. } => Some("LIMIT or OFFSET"),
                    GraphPattern::Graph { .. } => Some("GRAPH"),
                    GraphPattern::Service { .. } => Some("SERVICE"),
                    _ => None,
                };
                if analysis.not_monotone.is_none() {
                    analysis.not_monotone = op;
                }
            }
            Node::Expression(nrese_sparql_syntax::algebra::Expression::Exists(_)) => {
                if analysis.not_monotone.is_none() {
                    analysis.not_monotone = Some("EXISTS");
                }
            }
            Node::Expression(_) => {}
        }
        false
    });
    analysis
}

/// The vocabulary the bounds don't cover: U1 bounds facts about individuals (class
/// memberships, property values, equality); entailed schema statements (subclass,
/// equivalence, subproperty, disjointness axioms as triples) are classification's. A
/// variable predicate reads them too.
fn unbounded(analysis: &Analysis) -> Option<String> {
    const RESERVED: [&str; 3] = [
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "http://www.w3.org/2000/01/rdf-schema#",
        "http://www.w3.org/2002/07/owl#",
    ];
    analysis
        .reads
        .iter()
        .find_map(|(predicate, _)| match predicate {
            None => Some("a variable predicate".to_owned()),
            Some(p) if p == RDF_TYPE || p == OWL_SAME_AS => None,
            Some(p) if RESERVED.iter().any(|ns| p.starts_with(ns)) => Some(format!("<{p}>")),
            Some(_) => None,
        })
}

/// Whether every predicate and class `analysis` reads has no fact in U1 beyond L.
fn closed(analysis: &Analysis, view: &View, snapshot: &Snapshot) -> bool {
    if view.upper.is_none() {
        return false;
    }
    let id = |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    let any = view.gap_classes.is_empty() && view.gap_predicates.is_empty();
    analysis.reads.iter().all(|(predicate, class)| {
        let Some(predicate) = predicate else {
            return any;
        };
        if predicate == RDF_TYPE {
            return match class {
                Some(c) => id(c).is_none_or(|c| !view.gap_classes.contains(&c.raw())),
                None => view.gap_classes.is_empty(),
            };
        }
        id(predicate).is_none_or(|p| !view.gap_predicates.contains(&p.raw()))
    })
}

/// The ontology of a revision, kept for the exact services of its queries.
#[derive(Debug, Default)]
pub(crate) struct OntologyCache(std::sync::Mutex<Option<(u64, Arc<Ontology>)>>);

fn ontology_at(store: &StoreService, snapshot: &Snapshot) -> Arc<Ontology> {
    let mut cache = store
        .dl()
        .query_ontology
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some((revision, ontology)) = &*cache
        && *revision == snapshot.revision()
    {
        return Arc::clone(ontology);
    }
    let ontology = Arc::new(source::read_snapshot(snapshot));
    *cache = Some((snapshot.revision(), Arc::clone(&ontology)));
    ontology
}

/// The data's consistency at `snapshot`'s revision: recorded by its commit, proved by U1,
/// or checked now (once per revision).
fn consistency_at(store: &StoreService, snapshot: &Snapshot, view: &View) -> Verdict {
    if let Some(DlStatus {
        revision,
        consistency,
    }) = store.dl().status()
        && revision == snapshot.revision()
    {
        return consistency.verdict;
    }
    if view.proves_consistency {
        return Verdict::Consistent;
    }
    let ontology = ontology_at(store, snapshot);
    let checked = consistency::check(&ontology, &gate::budget(store, None));
    store.dl().record(DlStatus {
        revision: snapshot.revision(),
        consistency: checked.clone(),
    });
    checked.verdict
}

/// What can be said before running `prepared`: complete where every predicate it reads
/// is closed; otherwise the bounds decide when it runs.
pub(crate) fn plan_status(store: &StoreService, prepared: &PreparedQuery) -> Completeness {
    if prepared.access().is_some() || prepared.has_dataset() {
        return Completeness::sound_only(
            "the query reads part of the data: not checked against the bounds",
        );
    }
    let (snapshot, view) = super::bounds::view(store);
    let analysis = analyse(prepared.query());
    if let Some(what) = unbounded(&analysis) {
        return Completeness::sound_only(format!(
            "the query reads {what}: entailed schema statements aren't bounded"
        ));
    }
    if closed(&analysis, &view, &snapshot) {
        let mut status = Completeness::complete();
        status.path("closed-predicates");
        return status;
    }
    Completeness::sound_only(
        "the upper bound has candidates for what the query reads: decided when it runs",
    )
}

/// The certain answers of `prepared` as far as they can be proven, and their status.
pub(crate) fn answer(
    store: &StoreService,
    prepared: &PreparedQuery,
    cancellation: &CancellationToken,
    mode: DlAnswers,
) -> StoreResult<Outcome> {
    let outcome = decide_answers(store, prepared, cancellation, mode)?;
    let status = match &outcome {
        Outcome::Stream(status) | Outcome::Answers(_, status) => status,
    };
    if mode == DlAnswers::Exact && !status.is_complete() {
        return Err(StoreError::Incomplete(status.reasons().join("; ")));
    }
    Ok(outcome)
}

/// How a query is answered: over the lower bound as it streams (its status known before
/// it runs), or with answers collected and completed through the bounds.
pub(crate) enum Outcome {
    Stream(Completeness),
    Answers(Answers, Completeness),
}

fn decide_answers(
    store: &StoreService,
    prepared: &PreparedQuery,
    cancellation: &CancellationToken,
    mode: DlAnswers,
) -> StoreResult<Outcome> {
    let settings = store.query_settings();
    if prepared.access().is_some() {
        return Ok(Outcome::Stream(Completeness::sound_only(
            "the reader sees part of the data: answers are checked against the bounds only \
             for readers of every graph",
        )));
    }
    if prepared.has_dataset() {
        return Ok(Outcome::Stream(Completeness::sound_only(
            "the query names its dataset: answers are over those graphs, not checked \
             against the bounds",
        )));
    }
    let (snapshot, view) = super::bounds::view(store);
    let analysis = analyse(prepared.query());
    let mut status = Completeness::complete();
    match consistency_at(store, &snapshot, &view) {
        Verdict::Consistent => {}
        Verdict::Inconsistent => status
            .add("the data is inconsistent under OWL 2 DL, so it entails every answer".to_owned()),
        Verdict::Unknown(why) => status.add(format!(
            "the data's consistency under OWL 2 DL isn't known ({why})"
        )),
    }
    if let Some(what) = unbounded(&analysis) {
        status.add(format!(
            "the query reads {what}: entailed schema statements aren't bounded (the RL \
             closure's are sound; /classification has the subsumptions under OWL 2 DL)"
        ));
        return Ok(Outcome::Stream(status));
    }
    if closed(&analysis, &view, &snapshot) {
        // One evaluation, streamed: L's answers are the certain ones.
        status.path("closed-predicates");
        return Ok(Outcome::Stream(status));
    }
    if let Some(why) = &view.unavailable {
        status.add(why.clone());
        return Ok(Outcome::Stream(status));
    }
    if mode == DlAnswers::Sound {
        status.add("dl.answers = sound: the lower bound alone".to_owned());
        return Ok(Outcome::Stream(status));
    }
    if let Some(op) = analysis.not_monotone {
        status.add(format!(
            "the query uses {op}, which isn't monotone, over predicates whose lower and upper \
             bounds differ"
        ));
        return Ok(Outcome::Stream(status));
    }
    if matches!(prepared.query(), Query::Construct { .. }) {
        status.add(
            "CONSTRUCT is answered over the lower bound; it is complete only where the \
             predicates it reads are closed"
                .to_owned(),
        );
        return Ok(Outcome::Stream(status));
    }
    let lower = evaluate_prepared(&snapshot, prepared, settings, cancellation)?;
    let upper_snapshot = view.upper.as_ref().expect("available");
    let upper = evaluate_prepared(upper_snapshot, prepared, settings, cancellation)?;
    let (answers, counts, unresolved_why) = match (lower, upper) {
        (Answers::Boolean(true), _) => (Answers::Boolean(true), lower_counts(1), None),
        (Answers::Boolean(false), Answers::Boolean(false)) => {
            (Answers::Boolean(false), lower_counts(0), None)
        }
        (Answers::Boolean(false), Answers::Boolean(true)) => {
            let candidates = vec![Vec::new()];
            let decided = decide(store, &snapshot, &analysis, &[], &candidates, cancellation);
            let mut counts = Counts {
                lower: 0,
                upper: Some(1),
                ..Counts::default()
            };
            let found = match decided.verdicts[0] {
                Entailed::Yes => {
                    counts.proved = 1;
                    true
                }
                Entailed::No => {
                    counts.refuted = 1;
                    false
                }
                Entailed::Unknown(_) => {
                    counts.unresolved = 1;
                    false
                }
            };
            decided.paths.iter().for_each(|p| status.path(p));
            (Answers::Boolean(found), counts, decided.why)
        }
        (
            Answers::Solutions { variables, rows },
            Answers::Solutions {
                rows: upper_rows, ..
            },
        ) => {
            let known: HashSet<&Vec<Option<Term>>> = rows.iter().collect();
            let internal = |row: &Vec<Option<Term>>| {
                row.iter()
                    .any(|t| matches!(t, Some(Term::NamedNode(n)) if n.as_str().starts_with(U1)))
            };
            let mut gap: Vec<Vec<Option<Term>>> = upper_rows
                .into_iter()
                .filter(|row| !internal(row) && !known.contains(row))
                .collect();
            gap.sort_by_key(|row| format!("{row:?}"));
            gap.dedup();
            let lower_count = known.len() as u64;
            let mut counts = Counts {
                lower: lower_count,
                upper: Some(lower_count + gap.len() as u64),
                ..Counts::default()
            };
            let mut rows = rows;
            let mut why = None;
            if !gap.is_empty() {
                let decided = decide(store, &snapshot, &analysis, &variables, &gap, cancellation);
                decided.paths.iter().for_each(|p| status.path(p));
                why = decided.why;
                for (row, verdict) in gap.into_iter().zip(decided.verdicts) {
                    match verdict {
                        Entailed::Yes => {
                            counts.proved += 1;
                            rows.push(row);
                        }
                        Entailed::No => counts.refuted += 1,
                        Entailed::Unknown(_) => counts.unresolved += 1,
                    }
                }
            }
            (Answers::Solutions { variables, rows }, counts, why)
        }
        (lower @ Answers::Graph(_), _) => (lower, Counts::default(), None),
        (lower, _) => (lower, Counts::default(), None),
    };
    if counts.upper == Some(counts.lower) {
        status.path("bounds-equal");
    }
    if counts.unresolved > 0 {
        status.add(format!(
            "{} candidate answer(s) in the upper bound neither proved nor refuted{}",
            counts.unresolved,
            unresolved_why
                .map(|why| format!(" ({why})"))
                .unwrap_or_default()
        ));
    }
    status.bounds = Some(counts);
    Ok(Outcome::Answers(answers, status))
}

fn lower_counts(n: u64) -> Counts {
    Counts {
        lower: n,
        upper: Some(n),
        ..Counts::default()
    }
}

/// A position of an instantiated atom.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Slot {
    Const(u64),
    /// An existential variable (or blank node), by name.
    Var(String),
}

/// What decides one candidate: tests that must all hold.
enum Test {
    Axiom(Axiom),
    /// Some individual is an instance of the class.
    Nonempty(ExprId),
}

struct Decided {
    verdicts: Vec<Entailed>,
    paths: Vec<&'static str>,
    why: Option<String>,
}

/// Decides the candidate `rows` (values of `variables`) of `analysis`'s query with the
/// exact services. A row the shape can't take is unresolved.
fn decide(
    store: &StoreService,
    snapshot: &Snapshot,
    analysis: &Analysis,
    variables: &[Variable],
    rows: &[Vec<Option<Term>>],
    cancellation: &CancellationToken,
) -> Decided {
    let unresolved = |why: &str| Decided {
        verdicts: rows
            .iter()
            .map(|_| Entailed::Unknown(why.to_owned()))
            .collect(),
        paths: Vec::new(),
        why: Some(why.to_owned()),
    };
    let Some(shape) = &analysis.shape else {
        return unresolved(
            "the exact services take one basic graph pattern with filters, not this query",
        );
    };
    let config = &store.config().dl;
    let started = Instant::now();
    let base = ontology_at(store, snapshot);
    let mut ontology = (*base).clone();
    let rdf_type = snapshot.lookup(NamedNodeRef::new_unchecked(RDF_TYPE).into());
    let same_as = snapshot.lookup(NamedNodeRef::new_unchecked(OWL_SAME_AS).into());
    let thing = snapshot.lookup(NamedNodeRef::new_unchecked(OWL_THING).into());
    let data_properties = data_properties(&ontology);
    let mut paths = Vec::new();
    let mut tests: Vec<Option<Result<Vec<Test>, String>>> = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        if i >= config.max_candidates {
            tests.push(Some(Err(format!(
                "past dl.max_candidates ({})",
                config.max_candidates
            ))));
            continue;
        }
        let instantiated = instantiate(shape, variables, row, snapshot);
        let built = instantiated.and_then(|atoms| {
            tests_of(
                &mut ontology,
                snapshot,
                &atoms,
                Ids {
                    rdf_type: rdf_type.map(TermId::raw),
                    same_as: same_as.map(TermId::raw),
                    thing: thing.map(TermId::raw),
                },
                &data_properties,
                &mut paths,
            )
        });
        tests.push(Some(built));
    }
    let tx = store.engine().speculative();
    let fresh: Vec<u64> = (0..8)
        .map(|i| {
            tx.intern(NamedNodeRef::new_unchecked(&format!("urn:nrese:dl:fresh:{i}")).into())
                .raw()
        })
        .collect();
    let deadline = started + config.timeout;
    let ontology = &ontology;
    let verdicts: Vec<Entailed> = tests
        .into_par_iter()
        .map(|tests| {
            let tests = match tests.expect("built") {
                Ok(tests) => tests,
                Err(why) => return Entailed::Unknown(why),
            };
            let mut answer = Entailed::Yes;
            for test in tests {
                if cancellation.is_cancelled() || Instant::now() >= deadline {
                    return Entailed::Unknown("past dl.timeout".to_owned());
                }
                let budget = consistency::Budget {
                    timeout: deadline.saturating_duration_since(Instant::now()),
                    memory_bytes: config.memory_bytes / config.workers().max(1),
                    threads: 1,
                    cancel: None,
                };
                let found = match test {
                    Test::Axiom(axiom) => entailment::entails(ontology, &axiom, &fresh, &budget),
                    Test::Nonempty(class) => entailment::nonempty(ontology, class, &budget),
                };
                match found {
                    Entailed::Yes => {}
                    other => {
                        answer = other;
                        if answer == Entailed::No {
                            break;
                        }
                    }
                }
            }
            answer
        })
        .collect();
    let why = verdicts.iter().find_map(|v| match v {
        Entailed::Unknown(why) => Some(why.clone()),
        _ => None,
    });
    Decided {
        verdicts,
        paths,
        why,
    }
}

/// The ontology's data properties (an existential value of one is `DataSomeValuesFrom`).
fn data_properties(o: &Ontology) -> HashSet<u64> {
    let mut out = HashSet::new();
    for a in &o.axioms {
        match a {
            Axiom::Declaration(nrese_owl::EntityKind::DataProperty, p)
            | Axiom::DataPropertyAssertion(p, _, _)
            | Axiom::NegativeDataPropertyAssertion(p, _, _)
            | Axiom::DataPropertyDomain(p, _)
            | Axiom::DataPropertyRange(p, _)
            | Axiom::FunctionalDataProperty(p) => {
                out.insert(*p);
            }
            Axiom::SubDataPropertyOf(a, b) => {
                out.insert(*a);
                out.insert(*b);
            }
            _ => {}
        }
    }
    out
}

/// The query's atoms with the row's values in place: answer variables bound, the others
/// (and blank nodes) existential. A filter over a variable the row doesn't bind, or a
/// term the store doesn't know, makes the row unresolvable.
fn instantiate(
    shape: &Shape,
    variables: &[Variable],
    row: &[Option<Term>],
    snapshot: &Snapshot,
) -> Result<Vec<[Slot; 3]>, String> {
    let mut bound: HashMap<&str, &Term> = HashMap::new();
    for (v, value) in variables.iter().zip(row) {
        if let Some(value) = value
            && shape.projected.contains(v)
        {
            bound.insert(v.as_str(), value);
        }
    }
    if shape
        .filtered
        .iter()
        .any(|v| !bound.contains_key(v.as_str()))
    {
        return Err("a filter reads an existential variable".to_owned());
    }
    let constant = |t: &Term| -> Result<Slot, String> {
        snapshot
            .lookup(t.as_ref())
            .map(|id| Slot::Const(id.raw()))
            .ok_or_else(|| format!("{t} isn't in the store"))
    };
    let slot = |t: &TermPattern| -> Result<Slot, String> {
        match t {
            TermPattern::Variable(v) => match bound.get(v.as_str()) {
                Some(value) => constant(value),
                None => Ok(Slot::Var(v.as_str().to_owned())),
            },
            TermPattern::BlankNode(b) => Ok(Slot::Var(format!("_:{}", b.as_str()))),
            TermPattern::NamedNode(n) => constant(&Term::NamedNode(n.clone())),
            TermPattern::Literal(l) => constant(&Term::Literal(l.clone())),
            TermPattern::Triple(_) => Err("a triple term".to_owned()),
        }
    };
    shape
        .patterns
        .iter()
        .map(|t| {
            let predicate = match &t.predicate {
                NamedNodePattern::NamedNode(n) => constant(&Term::NamedNode(n.clone()))?,
                NamedNodePattern::Variable(v) => match bound.get(v.as_str()) {
                    Some(value) => constant(value)?,
                    None => return Err("a variable predicate".to_owned()),
                },
            };
            Ok([slot(&t.subject)?, predicate, slot(&t.object)?])
        })
        .collect()
}

#[derive(Clone, Copy)]
struct Ids {
    rdf_type: Option<u64>,
    same_as: Option<u64>,
    thing: Option<u64>,
}

fn is_literal(t: u64) -> bool {
    use nrese_engine::TermKind;
    !matches!(
        TermId::from_raw(t).kind(),
        TermKind::Iri | TermKind::BlankNode | TermKind::DefaultGraph
    )
}

/// The tests that decide one instantiated query: each ground atom not in L, and each
/// tree of existential variables rolled up.
fn tests_of(
    o: &mut Ontology,
    snapshot: &Snapshot,
    atoms: &[[Slot; 3]],
    ids: Ids,
    data_properties: &HashSet<u64>,
    paths: &mut Vec<&'static str>,
) -> Result<Vec<Test>, String> {
    let mut note = |p: &'static str| {
        if !paths.contains(&p) {
            paths.push(p);
        }
    };
    let mut tests = Vec::new();
    let mut existential = Vec::new();
    for atom in atoms {
        match atom {
            [Slot::Const(s), Slot::Const(p), Slot::Const(o_)] => {
                let in_l = snapshot.contains_in(
                    ReadModel::Materialised,
                    &nrese_engine::EncodedTriple::new(
                        TermId::from_raw(*s),
                        TermId::from_raw(*p),
                        TermId::from_raw(*o_),
                    )
                    .in_default_graph(),
                ) || snapshot
                    .quads_for_pattern_in(
                        ReadModel::Materialised,
                        &nrese_engine::QuadPattern {
                            subject: Some(TermId::from_raw(*s)),
                            predicate: Some(TermId::from_raw(*p)),
                            object: Some(TermId::from_raw(*o_)),
                            graph: nrese_engine::GraphSelector::Any,
                        },
                    )
                    .next()
                    .is_some();
                if in_l {
                    continue;
                }
                note("exact-ground-entailment");
                tests.push(Test::Axiom(ground_axiom(o, snapshot, [*s, *p, *o_], ids)?));
            }
            other => existential.push(other.clone()),
        }
    }
    if !existential.is_empty() {
        note("exact-internalisable-cq");
        tests.extend(roll_up(o, snapshot, &existential, ids, data_properties)?);
    }
    Ok(tests)
}

/// The axiom a ground atom states.
fn ground_axiom(
    o: &mut Ontology,
    snapshot: &Snapshot,
    [s, p, v]: [u64; 3],
    ids: Ids,
) -> Result<Axiom, String> {
    if Some(p) == ids.rdf_type {
        if Some(v) == ids.thing {
            return Ok(Axiom::ClassAssertion(
                ExprId(o.classes.intern(ClassExpr::Thing)),
                s,
            ));
        }
        if TermId::from_raw(v).kind() != nrese_engine::TermKind::Iri {
            return Err("a class that isn't an IRI".to_owned());
        }
        return Ok(Axiom::ClassAssertion(
            ExprId(o.classes.intern(ClassExpr::Class(v))),
            s,
        ));
    }
    if Some(p) == ids.same_as {
        return Ok(Axiom::SameIndividual(vec![s, v]));
    }
    if is_literal(v) {
        note_literal(o, snapshot, v);
        return Ok(Axiom::DataPropertyAssertion(p, s, v));
    }
    Ok(Axiom::ObjectPropertyAssertion(p, s, v))
}

/// Gives the datatype theory a literal's parts.
fn note_literal(o: &mut Ontology, snapshot: &Snapshot, v: u64) {
    if o.data.literals.contains_key(&v) {
        return;
    }
    if let Some(Term::Literal(l)) = snapshot.decode(TermId::from_raw(v)) {
        o.data.literals.insert(
            v,
            Literal {
                lexical: l.value().to_owned(),
                datatype: Some(l.datatype().as_str().to_owned()),
                language: l.language().map(str::to_owned),
            },
        );
    }
}

/// An edge of the existential part: from a variable along a property (or its inverse) to
/// a variable or a named term.
#[derive(Debug, Clone)]
struct Edge {
    from: String,
    property: ObjProp,
    to: Slot,
}

/// The existential atoms rolled up, one test per connected part (ExactInternalisableCQ):
/// a part attached to a named term `a` is `a : ∃p.E`, one with none is "some individual
/// is an `E`". A part that isn't a tree, a variable class or a variable predicate can't
/// be rolled up.
fn roll_up(
    o: &mut Ontology,
    snapshot: &Snapshot,
    atoms: &[[Slot; 3]],
    ids: Ids,
    data_properties: &HashSet<u64>,
) -> Result<Vec<Test>, String> {
    let mut labels: HashMap<String, Vec<ExprId>> = HashMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut vars: Vec<String> = Vec::new();
    let add_var = |v: &String, vars: &mut Vec<String>| {
        if !vars.contains(v) {
            vars.push(v.clone());
        }
    };
    for atom in atoms {
        let [s, p, v] = atom;
        let Slot::Const(p) = p else {
            return Err("a variable predicate".to_owned());
        };
        if Some(*p) == ids.rdf_type {
            let Slot::Var(x) = s else {
                return Err("a variable class".to_owned());
            };
            let Slot::Const(c) = v else {
                return Err("a variable class".to_owned());
            };
            add_var(x, &mut vars);
            let class = match Some(*c) == ids.thing {
                true => ClassExpr::Thing,
                false => ClassExpr::Class(*c),
            };
            labels
                .entry(x.clone())
                .or_default()
                .push(ExprId(o.classes.intern(class)));
            continue;
        }
        if Some(*p) == ids.same_as {
            return Err("owl:sameAs with an existential variable".to_owned());
        }
        if data_properties.contains(p) {
            let Slot::Var(x) = s else {
                return Err("a data property with an existential value".to_owned());
            };
            add_var(x, &mut vars);
            let expr = match v {
                Slot::Const(lit) if is_literal(*lit) => {
                    note_literal(o, snapshot, *lit);
                    ClassExpr::DataHasValue(*p, *lit)
                }
                Slot::Var(y) if atoms.iter().filter(|a| a.contains(v)).count() == 1 => {
                    let _ = y;
                    let literal =
                        nrese_owl::RangeId(o.ranges.intern(nrese_owl::DataRange::Literal));
                    ClassExpr::DataSome(*p, literal)
                }
                _ => return Err("a data value shared between atoms".to_owned()),
            };
            labels
                .entry(x.clone())
                .or_default()
                .push(ExprId(o.classes.intern(expr)));
            continue;
        }
        match (s, v) {
            (Slot::Var(x), Slot::Var(y)) => {
                add_var(x, &mut vars);
                add_var(y, &mut vars);
                edges.push(Edge {
                    from: x.clone(),
                    property: ObjProp::Named(*p),
                    to: Slot::Var(y.clone()),
                });
            }
            (Slot::Var(x), Slot::Const(a)) => {
                add_var(x, &mut vars);
                edges.push(Edge {
                    from: x.clone(),
                    property: ObjProp::Named(*p),
                    to: Slot::Const(*a),
                });
            }
            (Slot::Const(a), Slot::Var(x)) => {
                add_var(x, &mut vars);
                edges.push(Edge {
                    from: x.clone(),
                    property: ObjProp::Inverse(*p),
                    to: Slot::Const(*a),
                });
            }
            (Slot::Const(_), Slot::Const(_)) => unreachable!("ground atoms are tested apart"),
        }
    }
    // Connected parts over the variable-to-variable edges.
    let mut part: HashMap<String, usize> = vars
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, v)| (v, i))
        .collect();
    loop {
        let mut changed = false;
        for e in &edges {
            if let Slot::Var(y) = &e.to {
                let (a, b) = (part[&e.from], part[y]);
                if a != b {
                    let (keep, gone) = (a.min(b), a.max(b));
                    for p in part.values_mut() {
                        if *p == gone {
                            *p = keep;
                        }
                    }
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut parts: Vec<usize> = part.values().copied().collect();
    parts.sort_unstable();
    parts.dedup();
    let mut tests = Vec::new();
    for id in parts {
        let members: Vec<&String> = vars.iter().filter(|v| part[*v] == id).collect();
        let inner = edges
            .iter()
            .filter(|e| part[&e.from] == id && matches!(e.to, Slot::Var(_)))
            .count();
        if inner + 1 != members.len() {
            return Err("existential variables that form a cycle".to_owned());
        }
        let root_leaf = edges
            .iter()
            .position(|e| part[&e.from] == id && matches!(e.to, Slot::Const(_)));
        match root_leaf {
            Some(i) => {
                let leaf = &edges[i];
                let Slot::Const(a) = leaf.to else {
                    unreachable!()
                };
                let e = expr(o, &leaf.from, None, Some(i), &labels, &edges);
                let some = ExprId(
                    o.classes
                        .intern(ClassExpr::Some(leaf.property.inverse(), e)),
                );
                tests.push(Test::Axiom(Axiom::ClassAssertion(some, a)));
            }
            None => {
                let e = expr(o, members[0], None, None, &labels, &edges);
                tests.push(Test::Nonempty(e));
            }
        }
    }
    Ok(tests)
}

/// The class expression of variable `v` reached through edge `came` (and leaving out
/// the root edge `root`): its classes, an existential per other edge.
fn expr(
    o: &mut Ontology,
    v: &str,
    came: Option<usize>,
    root: Option<usize>,
    labels: &HashMap<String, Vec<ExprId>>,
    edges: &[Edge],
) -> ExprId {
    let mut parts: Vec<ExprId> = labels.get(v).cloned().unwrap_or_default();
    for (i, e) in edges.iter().enumerate() {
        if Some(i) == came || Some(i) == root {
            continue;
        }
        if e.from == v {
            let filler = match &e.to {
                Slot::Var(w) => expr(o, w, Some(i), root, labels, edges),
                Slot::Const(b) => ExprId(o.classes.intern(ClassExpr::OneOf(vec![*b]))),
            };
            parts.push(ExprId(
                o.classes.intern(ClassExpr::Some(e.property, filler)),
            ));
        } else if matches!(&e.to, Slot::Var(w) if w == v) {
            let filler = expr(o, &e.from, Some(i), root, labels, edges);
            parts.push(ExprId(
                o.classes
                    .intern(ClassExpr::Some(e.property.inverse(), filler)),
            ));
        }
    }
    parts.sort_unstable();
    parts.dedup();
    match parts.len() {
        0 => ExprId(o.classes.intern(ClassExpr::Thing)),
        1 => parts[0],
        _ => ExprId(o.classes.intern(ClassExpr::And(parts))),
    }
}
