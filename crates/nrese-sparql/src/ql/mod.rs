//! OWL 2 QL answers through existentials (docs/design/ql-rewriting.md): queries rewritten
//! with the tree witnesses of the QL part of the snapshot's schema, over the closure the
//! store materialised.
//!
//! [`QlRewriting`] is the store's switch and its cache of the compiled schema; the
//! rewriting of a query's basic graph patterns is `native/ql.rs`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use nrese_engine::{
    AssertedIdentity, GraphSelector, QuadPattern, ReadModel, Snapshot, SnapshotIdentity, TermId,
};
use nrese_owl::ql::Tbox;
use nrese_rdf::{NamedNodeRef, Term};

use crate::GraphAccess;

pub use nrese_owl::ql::{Closure, Limits};
pub use print::{PrintForm, Printed, print};

mod print;
#[cfg(test)]
mod tests;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// The schema properties read, in every graph.
const SCHEMA: &[(&str, &str)] = &[
    (RDFS, "subClassOf"),
    (OWL, "equivalentClass"),
    (RDFS, "subPropertyOf"),
    (OWL, "equivalentProperty"),
    (OWL, "inverseOf"),
    (RDFS, "domain"),
    (RDFS, "range"),
    (OWL, "onProperty"),
    (OWL, "someValuesFrom"),
    (OWL, "allValuesFrom"),
    (OWL, "hasValue"),
    (OWL, "minCardinality"),
    (OWL, "minQualifiedCardinality"),
    (OWL, "cardinality"),
    (OWL, "qualifiedCardinality"),
    (OWL, "onClass"),
    (OWL, "onDataRange"),
    (OWL, "intersectionOf"),
    (OWL, "unionOf"),
    (OWL, "disjointUnionOf"),
    // What the rewriting doesn't follow, for the completeness it reports (design §7).
    (OWL, "propertyChainAxiom"),
    (OWL, "hasKey"),
    (OWL, "oneOf"),
    (OWL, "complementOf"),
    (OWL, "hasSelf"),
    (OWL, "maxCardinality"),
    (OWL, "maxQualifiedCardinality"),
];

/// The classes whose members' `rdf:type` statements are read: what the mapping needs to
/// know about the schema's terms.
const DECLARATIONS: &[(&str, &str)] = &[
    (OWL, "Restriction"),
    (OWL, "Class"),
    (RDFS, "Class"),
    (RDFS, "Datatype"),
    (OWL, "ObjectProperty"),
    (OWL, "DatatypeProperty"),
    (OWL, "AnnotationProperty"),
    (OWL, "SymmetricProperty"),
    (OWL, "ReflexiveProperty"),
    (OWL, "TransitiveProperty"),
    (OWL, "FunctionalProperty"),
    (OWL, "InverseFunctionalProperty"),
];

/// The properties that make their blank subject a class expression (`nrese-owl`'s
/// mapping): a restriction's and the set operators'.
const EXPRESSIONS: &[&str] = &[
    "onProperty",
    "intersectionOf",
    "unionOf",
    "complementOf",
    "oneOf",
];

/// The list-valued properties whose lists are read.
const LISTS: &[&str] = &[
    "intersectionOf",
    "unionOf",
    "disjointUnionOf",
    "propertyChainAxiom",
    "hasKey",
    "oneOf",
];

/// What the QL rewriting did to one query (EXPLAIN, and with every answer).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QlReport {
    /// Basic graph patterns rewritten.
    pub patterns: usize,
    /// Tree witnesses not folded: the data has their tree wherever they would fold, so
    /// they add no answer (design §3).
    pub realised: usize,
    /// Questions asked of the data for that (not those its cache answered).
    pub checks: usize,
    /// Their tree witnesses.
    pub witnesses: usize,
    /// Branches of their unions.
    pub branches: usize,
    /// Triple patterns of their rewritings, alternatives counted.
    pub atoms: usize,
    /// The bounds reached ([`Limits`]): those patterns ran as written.
    pub limits: Vec<&'static str>,
    /// Whether answers through anonymous individuals may be missing (reasons from `ql`).
    pub completeness: crate::Completeness,
}

/// A store's QL rewriting: what its closure applies, the bounds, and the schema compiled
/// for the last snapshots read (one per reader's graph access, design §1).
#[derive(Debug)]
pub struct QlRewriting {
    closure: Closure,
    limits: Limits,
    cached: Mutex<Vec<Cached>>,
    /// Whether the data has a witness's tree wherever it folds, per probe, for one
    /// snapshot view (design §3).
    realised: Mutex<(Option<SnapshotIdentity>, HashMap<String, bool>)>,
    /// The latest commits' changes to memberships of class expressions
    /// ([`Self::note_commit`]), oldest first, at most [`NOTES`].
    notes: Mutex<std::collections::VecDeque<Note>>,
}

/// Schemas compiled for different readers kept at once.
const READERS: usize = 8;

/// Commits whose membership changes are kept ([`QlRewriting::note_commit`]).
const NOTES: usize = 64;

/// Expressions a commit's note names at most; past it, every expression is read again.
const TOUCHED: usize = 1024;

/// One commit's changes to memberships: from one asserted state to the next, the class
/// expressions an `rdf:type` statement was added to or removed from (`None`: too many,
/// all of them).
#[derive(Debug)]
struct Note {
    from: AssertedIdentity,
    to: AssertedIdentity,
    touched: Option<Vec<u64>>,
}

/// What a cached schema's memberships were read at: reused only for the same asserted
/// statements by the same ids (equality and canonical mode), notes leading from it.
#[derive(Debug)]
struct Members {
    at: AssertedIdentity,
    equality: Option<TermId>,
    canonical: bool,
    of: HashMap<u64, Member>,
}

#[derive(Debug)]
struct Cached {
    /// The graphs the schema was read from: every graph (`None`), or a reader's who sees
    /// only the inferences its graphs support.
    access: Option<GraphAccess>,
    /// The exact snapshot view it was read from, including pending changes and masks.
    key: SnapshotIdentity,
    /// The exact sorted input, moved from the schema scan. Equality is linear in the
    /// schema size, as hashing was; a hash collision must never select another TBox.
    schema: Box<[nrese_owl::Statement]>,
    /// Compilation also needs the dictionary's id of Thing, even if no schema axiom
    /// mentions it. Ordinary data can introduce that id without changing the schema.
    thing: Option<nrese_owl::Term>,
    tbox: Arc<Tbox>,
    /// Each expression's membership in `schema`, for the next snapshot's read.
    members: Members,
}

impl QlRewriting {
    /// Rewriting over a closure that applies what `closure` says, with the default bounds.
    pub fn new(closure: Closure) -> Self {
        Self {
            closure,
            limits: Limits::default(),
            cached: Mutex::new(Vec::new()),
            realised: Mutex::new((None, HashMap::new())),
            notes: Mutex::new(std::collections::VecDeque::new()),
        }
    }

    /// Notes a commit from asserted state `from` to `to` that added or removed the
    /// memberships of `touched` (`None`: unknown or too many; [`touched_expressions`]):
    /// the next schema read reuses the other expressions' memberships. Only for a commit
    /// that happened; a write without a note makes the next read scan every expression.
    pub fn note_commit(
        &self,
        from: AssertedIdentity,
        to: AssertedIdentity,
        touched: Option<Vec<u64>>,
    ) {
        if from == to {
            return;
        }
        let mut notes = self.notes.lock().unwrap_or_else(|p| p.into_inner());
        if notes.len() == NOTES {
            notes.pop_front();
        }
        notes.push_back(Note { from, to, touched });
    }

    /// The expressions whose memberships may differ between asserted states `from` and
    /// `to`: `Some` of them where notes lead from one to the other, `None` (every one)
    /// otherwise.
    fn touched_between(
        &self,
        from: AssertedIdentity,
        to: AssertedIdentity,
    ) -> Option<HashSet<u64>> {
        let notes = self.notes.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = HashSet::new();
        let mut at = from;
        while at != to {
            let note = notes.iter().find(|n| n.from == at)?;
            out.extend(note.touched.as_ref()?.iter().copied());
            at = note.to;
        }
        Some(out)
    }

    /// Whether the data at snapshot `identity` has the tree of probe `key`
    /// wherever it folds, memoised per view and probe (`None`: not asked, the query's
    /// questions spent; `false` then, and nothing kept). Concurrent misses may compute
    /// independently; the query itself runs outside the cache lock.
    pub(crate) fn realised(
        &self,
        identity: SnapshotIdentity,
        key: &str,
        compute: impl FnOnce() -> Option<bool>,
    ) -> bool {
        {
            let cache = self.realised.lock().unwrap_or_else(|p| p.into_inner());
            if cache.0 == Some(identity)
                && let Some(&known) = cache.1.get(key)
            {
                return known;
            }
        }
        // Computed without the lock: the check is a query of its own.
        let Some(known) = compute() else {
            return false;
        };
        let mut cache = self.realised.lock().unwrap_or_else(|p| p.into_inner());
        if cache.0 != Some(identity) {
            *cache = (Some(identity), HashMap::new());
        }
        cache.1.insert(key.to_owned(), known);
        known
    }

    /// The same with other bounds (tests make them small).
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// What the closure it rewrites over applies.
    pub fn closure(&self) -> Closure {
        self.closure
    }

    /// What decides its rewritings besides the snapshot's TBox: the closure and the
    /// bounds (part of the result cache's key of an answer).
    pub(crate) fn fingerprint(&self) -> String {
        format!("{:?} {:?}", self.closure, self.limits)
    }

    pub(crate) fn limits(&self) -> &Limits {
        &self.limits
    }

    /// The QL part of `snapshot`'s schema, compiled. Read once per snapshot, and compiled
    /// only when the schema or compiler vocabulary changes; other data commits reuse it.
    ///
    /// Graph access (design §1) matches what the reader sees of the RL closure: a reader
    /// who sees only the inferences its graphs support (`inferred = "supported"`) gets the
    /// schema of those graphs only; one who sees every inference (`"visible"`) the whole
    /// schema, as the closure it reads was computed with it.
    pub(crate) fn tbox(&self, snapshot: &Snapshot, access: Option<&GraphAccess>) -> Arc<Tbox> {
        let access = access.filter(|a| a.inferred_by_support);
        let key = snapshot.identity();
        let mut cached = self.cached.lock().unwrap_or_else(|p| p.into_inner());
        let at = cached.iter().position(|c| c.access.as_ref() == access);
        if let Some(c) = at.map(|i| &cached[i]).filter(|c| c.key == key) {
            return Arc::clone(&c.tbox);
        }
        // A snapshot bounds its dictionary reads, so an identity hit above needs no
        // lookup. Across identities, unchanged statements alone are not sufficient.
        let thing = snapshot
            .lookup(nrese_rdf::vocab::owl::THING.into())
            .map(TermId::raw);
        let keep = |graph: u64| {
            access.is_none_or(|access| access.allows_id(snapshot, TermId::from_raw(graph)))
        };
        // The memberships the entry read, where nothing a commit since changed them can
        // differ: the same view mode, and notes from its asserted state to this one.
        let asserted = snapshot.asserted_identity();
        let reuse = at.map(|i| &cached[i].members).and_then(|m| {
            (m.equality == key.equality && m.canonical == key.canonical)
                .then(|| self.touched_between(m.at, asserted).map(|t| (m, t)))
                .flatten()
        });
        let known = |node: u64| {
            let (members, touched) = reuse.as_ref()?;
            (!touched.contains(&node))
                .then(|| members.of.get(&node).copied())
                .flatten()
        };
        let (mut statements, of) = read_schema(snapshot, &keep, &known);
        let members = Members {
            at: asserted,
            equality: key.equality,
            canonical: key.canonical,
            of,
        };
        if let Some(access) = access {
            statements.retain(|s| access.allows_id(snapshot, TermId::from_raw(s.graph)));
        }
        if let Some(c) = at
            .map(|i| &mut cached[i])
            .filter(|c| c.thing == thing && c.schema.as_ref() == statements)
        {
            c.key = key;
            c.members = members;
            return Arc::clone(&c.tbox);
        }
        let tbox = Arc::new(compile(snapshot, &statements, self.closure, thing));
        if let Some(i) = at {
            cached.remove(i);
        }
        if cached.len() == READERS {
            cached.remove(0);
        }
        cached.push(Cached {
            access: access.cloned(),
            key,
            schema: statements.into_boxed_slice(),
            thing,
            tbox: Arc::clone(&tbox),
            members,
        });
        tbox
    }
}

/// The class expressions `tx` changes a membership of: the blank objects of the
/// `rdf:type` statements it asserts or retracts, sorted (`None` past [`TOUCHED`]), for
/// [`QlRewriting::note_commit`]. Costs the transaction's changes.
pub fn touched_expressions(tx: &nrese_engine::Transaction<'_>) -> Option<Vec<u64>> {
    let Some(rdf_type) = iri(tx.base(), RDF, "type") else {
        // No `rdf:type` term before it: every statement it adds may be one.
        return tx.inserted().next().is_none().then(Vec::new);
    };
    let mut out: Vec<u64> = Vec::new();
    for quad in tx.inserted().chain(tx.deleted()) {
        if quad.predicate == rdf_type && quad.object.kind() == nrese_engine::TermKind::BlankNode {
            out.push(quad.object.raw());
            if out.len() > TOUCHED {
                return None;
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Some(out)
}

fn iri(snapshot: &Snapshot, namespace: &str, local: &str) -> Option<TermId> {
    snapshot.lookup(NamedNodeRef::new_unchecked(&format!("{namespace}{local}")).into())
}

/// The asserted statements of the schema, in every graph, sorted: the schema properties',
/// the declarations', the cells of the lists they name, and per class expression one
/// statement of an individual in it, from a graph `keep` admits.
fn schema_statements(snapshot: &Snapshot, keep: &dyn Fn(u64) -> bool) -> Vec<nrese_owl::Statement> {
    read_schema(snapshot, keep, &|_| None).0
}

#[cfg(test)]
thread_local! {
    /// Memberships [`read_schema`] scanned on this thread (the reuse tests count them).
    static MEMBER_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// What an expression's membership is ([`schema_statements`]): one statement, or none.
type Member = Option<nrese_owl::Statement>;

/// [`schema_statements`], with each expression's membership taken from `known` where it
/// knows it (`Some`), scanned otherwise; and the memberships, for the next read.
fn read_schema(
    snapshot: &Snapshot,
    keep: &dyn Fn(u64) -> bool,
    known: &dyn Fn(u64) -> Option<Member>,
) -> (Vec<nrese_owl::Statement>, HashMap<u64, Member>) {
    let mut members: HashMap<u64, Member> = HashMap::new();
    let mut out = Vec::new();
    let scan = |pattern: QuadPattern, out: &mut Vec<nrese_owl::Statement>| {
        out.extend(
            snapshot
                .quads_for_pattern_in(ReadModel::Asserted, &pattern)
                .map(|q| nrese_owl::Statement {
                    triple: [q.subject.raw(), q.predicate.raw(), q.object.raw()],
                    graph: q.graph.raw(),
                }),
        );
    };
    let pattern = |predicate: TermId, object: Option<TermId>| QuadPattern {
        subject: None,
        predicate: Some(predicate),
        object,
        graph: GraphSelector::Any,
    };
    for &(namespace, local) in SCHEMA {
        if let Some(p) = iri(snapshot, namespace, local) {
            scan(pattern(p, None), &mut out);
        }
    }
    if let Some(rdf_type) = iri(snapshot, RDF, "type") {
        for &(namespace, local) in DECLARATIONS {
            if let Some(class) = iri(snapshot, namespace, local) {
                scan(pattern(rdf_type, Some(class)), &mut out);
            }
        }
        // Individuals asserted to be in a class expression (`:p a [ owl:onProperty … ]`),
        // which have anonymous individuals of their own (design §7). What the hazards read
        // of them is the expression, not who is in it: one `rdf:type` statement per blank
        // node the schema read makes an expression, in a graph `keep` lets the reader see.
        // The schema stays the expressions' size, and a membership that doesn't change
        // which expressions have one keeps the compiled TBox (P2-C1 timing, 7 Nov: every
        // membership read made 10,000 of them a 10 ms read after each commit).
        let operators: Vec<u64> = EXPRESSIONS
            .iter()
            .filter_map(|local| iri(snapshot, OWL, local))
            .map(TermId::raw)
            .collect();
        let mut expressions: Vec<u64> = out
            .iter()
            .filter(|s| operators.contains(&s.triple[1]))
            .map(|s| s.triple[0])
            .filter(|&node| TermId::from_raw(node).kind() == nrese_engine::TermKind::BlankNode)
            .collect();
        expressions.sort_unstable();
        expressions.dedup();
        for node in expressions {
            if let Some(member) = known(node) {
                out.extend(member);
                members.insert(node, member);
                continue;
            }
            #[cfg(test)]
            MEMBER_SCANS.set(MEMBER_SCANS.get() + 1);
            let member = snapshot
                .quads_for_pattern_in(
                    ReadModel::Asserted,
                    &pattern(rdf_type, Some(TermId::from_raw(node))),
                )
                // One the reader keeps: it leaves out a reserved name as the individual
                // (`rdf:type a _:e`), and another member must stand in (Sol r4 S1).
                .find(|q| {
                    keep(q.graph.raw())
                        && !matches!(snapshot.decode(q.subject),
                            Some(Term::NamedNode(n))
                                if nrese_owl::diagnostics::reserved_name(n.as_str()))
                });
            let member = member.map(|q| nrese_owl::Statement {
                triple: [q.subject.raw(), q.predicate.raw(), q.object.raw()],
                graph: q.graph.raw(),
            });
            out.extend(member);
            members.insert(node, member);
        }
    }
    // The lists: from each head, `rdf:first` and `rdf:rest` along the cells.
    let (first, rest) = (iri(snapshot, RDF, "first"), iri(snapshot, RDF, "rest"));
    if let (Some(first), Some(rest)) = (first, rest) {
        let heads: Vec<u64> = LISTS
            .iter()
            .filter_map(|local| iri(snapshot, OWL, local))
            .flat_map(|p| {
                out.iter()
                    .filter(move |s| s.triple[1] == p.raw())
                    .map(|s| s.triple[2])
            })
            .collect();
        let mut seen = std::collections::HashSet::new();
        for head in heads {
            let mut cell = head;
            while TermId::from_raw(cell).kind() == nrese_engine::TermKind::BlankNode
                && seen.insert(cell)
            {
                let mut next = None;
                for predicate in [first, rest] {
                    let before = out.len();
                    scan(
                        QuadPattern {
                            subject: Some(TermId::from_raw(cell)),
                            ..pattern(predicate, None)
                        },
                        &mut out,
                    );
                    if predicate == rest {
                        next = out[before..].first().map(|s| s.triple[2]);
                    }
                }
                match next {
                    Some(n) => cell = n,
                    None => break,
                }
            }
        }
    }
    out.sort();
    out.dedup();
    (out, members)
}

/// The snapshot's terms, as the mapping reads them.
struct SnapshotTerms<'a>(&'a Snapshot);

impl nrese_owl::Terms for SnapshotTerms<'_> {
    fn kind(&self, term: nrese_owl::Term) -> nrese_owl::TermKind {
        match TermId::from_raw(term).kind() {
            nrese_engine::TermKind::Iri => nrese_owl::TermKind::Iri,
            nrese_engine::TermKind::BlankNode => nrese_owl::TermKind::Blank,
            _ => nrese_owl::TermKind::Literal,
        }
    }

    fn lexical(&self, term: nrese_owl::Term) -> Option<String> {
        match self.0.decode(TermId::from_raw(term))? {
            Term::Literal(l) => Some(l.value().to_owned()),
            _ => None,
        }
    }

    fn iri(&self, iri: &str) -> Option<nrese_owl::Term> {
        self.0
            .lookup(NamedNodeRef::new_unchecked(iri).into())
            .map(TermId::raw)
    }

    fn datatype(&self, term: nrese_owl::Term) -> Option<String> {
        match self.0.decode(TermId::from_raw(term))? {
            Term::Literal(l) => Some(l.datatype().as_str().to_owned()),
            _ => None,
        }
    }

    fn iri_text(&self, term: nrese_owl::Term) -> Option<String> {
        match self.0.decode(TermId::from_raw(term))? {
            Term::NamedNode(n) => Some(n.into_string()),
            _ => None,
        }
    }
}

fn compile(
    snapshot: &Snapshot,
    statements: &[nrese_owl::Statement],
    closure: Closure,
    thing: Option<nrese_owl::Term>,
) -> Tbox {
    let terms = SnapshotTerms(snapshot);
    let ontology = nrese_owl::read(statements, &terms);
    Tbox::compile(&ontology, closure, thing)
}
