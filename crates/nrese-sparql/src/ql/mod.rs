//! OWL 2 QL answers through existentials (docs/design/ql-rewriting.md): queries rewritten
//! with the tree witnesses of the QL part of the snapshot's schema, over the closure the
//! store materialised.
//!
//! [`QlRewriting`] is the store's switch and its cache of the compiled schema; the
//! rewriting of a query's basic graph patterns is `native/ql.rs`.

use std::collections::HashMap;
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

/// Membership changes a commit's note holds at most; past it, every expression is read
/// again.
const TOUCHED: usize = 1024;

/// One change a commit made to a membership of a class expression ([`touched_expressions`]):
/// the `rdf:type` statement asserted or retracted, its object the expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MembershipChange {
    statement: nrese_owl::Statement,
    added: bool,
}

/// One commit's changes to memberships, from one asserted state to the next (`None`: too
/// many, or unknown: every expression is read again).
#[derive(Debug)]
struct Note {
    from: AssertedIdentity,
    to: AssertedIdentity,
    changes: Option<Vec<MembershipChange>>,
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
    /// The schema without its memberships, in every graph ([`read_core`]): what a new
    /// snapshot's must equal for the entry to stand. Equality is linear in its size, as
    /// hashing was; a hash collision must never select another TBox.
    core: Box<[nrese_owl::Statement]>,
    /// The class expressions `core` makes, sorted.
    expressions: Box<[u64]>,
    /// Compilation also needs the dictionary's id of Thing, even if no schema axiom
    /// mentions it. Ordinary data can introduce that id without changing the schema.
    thing: Option<nrese_owl::Term>,
    tbox: Arc<Tbox>,
    /// Each expression's membership in the compiled schema, for the next snapshot's read.
    members: Members,
}

impl Cached {
    /// The schema it was compiled from: `core` in the reader's graphs, with the members.
    #[cfg(test)]
    fn schema(&self, snapshot: &Snapshot) -> Vec<nrese_owl::Statement> {
        let core: Vec<nrese_owl::Statement> = self
            .core
            .iter()
            .filter(|s| {
                self.access
                    .as_ref()
                    .is_none_or(|a| a.allows_id(snapshot, TermId::from_raw(s.graph)))
            })
            .copied()
            .collect();
        with_members(&core, self.expressions.iter().map(|e| &self.members.of[e]))
    }
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

    /// Notes a commit from asserted state `from` to `to` that made the membership
    /// `changes` (`None`: unknown or too many; [`touched_expressions`]): the next schema
    /// read keeps every membership those changes leave as it was. Only for a commit that
    /// happened; a write without a note makes the next read scan every expression.
    pub fn note_commit(
        &self,
        from: AssertedIdentity,
        to: AssertedIdentity,
        changes: Option<Vec<MembershipChange>>,
    ) {
        if from == to {
            return;
        }
        let mut notes = self.notes.lock().unwrap_or_else(|p| p.into_inner());
        if notes.len() == NOTES {
            notes.pop_front();
        }
        notes.push_back(Note { from, to, changes });
    }

    /// The membership changes between asserted states `from` and `to`, in commit order:
    /// `Some` where notes lead from one to the other, `None` (every expression) otherwise.
    fn changes_between(
        &self,
        from: AssertedIdentity,
        to: AssertedIdentity,
    ) -> Option<Vec<MembershipChange>> {
        let notes = self.notes.lock().unwrap_or_else(|p| p.into_inner());
        let mut out = Vec::new();
        let mut at = from;
        while at != to {
            let note = notes.iter().find(|n| n.from == at)?;
            out.extend(note.changes.as_ref()?.iter().copied());
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
        let asserted = snapshot.asserted_identity();
        let core = read_core(snapshot);
        // An entry of this reader whose core and view mode are this snapshot's, with notes
        // from its memberships' asserted state to this one: its memberships are brought
        // forward by the noted changes, in place (P2-C1 probes, 8 Nov: rebuilding every
        // expression's took 188 us a snapshot, rescanning each touched one 1 us). What the
        // schema keeps of an expression is that it has a member, one the reader may see
        // and the OWL reader keeps: an added statement stands in where there was none, and
        // only the removal of the kept one is read again.
        if let Some(i) = at {
            let c = &mut cached[i];
            let changes = (c.thing == thing
                && c.members.equality == key.equality
                && c.members.canonical == key.canonical
                && *c.core == *core)
                .then(|| self.changes_between(c.members.at, asserted))
                .flatten();
            if let Some(changes) = changes {
                let mut changed = false;
                for change in changes {
                    let node = change.statement.triple[2];
                    // Not an expression of the core: nothing the schema reads.
                    let Some(member) = c.members.of.get(&node).copied() else {
                        continue;
                    };
                    #[cfg(test)]
                    count(&EXPRESSIONS_WALKED, 1);
                    let next = match (member, change.added) {
                        (None, true) => {
                            eligible(snapshot, &change.statement, &keep).then_some(change.statement)
                        }
                        (Some(kept), false) if kept == change.statement => {
                            member_of(snapshot, node, &keep)
                        }
                        _ => continue,
                    };
                    changed |= next.is_some() != member.is_some();
                    c.members.of.insert(node, next);
                }
                c.key = key;
                c.members.at = asserted;
                // Which expressions have a member is what the TBox reads (design §7).
                if !changed {
                    return Arc::clone(&c.tbox);
                }
                let statements = filtered_schema(c, snapshot, &core, access);
                let tbox = Arc::new(compile(snapshot, &statements, self.closure, thing));
                c.tbox = Arc::clone(&tbox);
                return tbox;
            }
        }
        // Otherwise every expression's membership, read afresh.
        let expressions = expressions_of(snapshot, &core);
        #[cfg(test)]
        count(&EXPRESSIONS_WALKED, expressions.len());
        let of: HashMap<u64, Member> = expressions
            .iter()
            .map(|&node| (node, member_of(snapshot, node, &keep)))
            .collect();
        let members = Members {
            at: asserted,
            equality: key.equality,
            canonical: key.canonical,
            of,
        };
        if let Some(c) = at.map(|i| &mut cached[i]).filter(|c| {
            c.thing == thing && *c.core == *core && present(&c.members.of) == present(&members.of)
        }) {
            c.key = key;
            c.members = members;
            return Arc::clone(&c.tbox);
        }
        let mut reader_core = core.clone();
        if let Some(access) = access {
            reader_core.retain(|s| access.allows_id(snapshot, TermId::from_raw(s.graph)));
        }
        let statements = with_members(&reader_core, expressions.iter().map(|e| &members.of[e]));
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
            core: core.into_boxed_slice(),
            expressions: expressions.into_boxed_slice(),
            thing,
            tbox: Arc::clone(&tbox),
            members,
        });
        tbox
    }
}

/// The schema of entry `c` as its reader compiles it: `core` in the reader's graphs, with
/// `c`'s memberships.
fn filtered_schema(
    c: &Cached,
    snapshot: &Snapshot,
    core: &[nrese_owl::Statement],
    access: Option<&GraphAccess>,
) -> Vec<nrese_owl::Statement> {
    let reader_core: Vec<nrese_owl::Statement> = core
        .iter()
        .filter(|s| access.is_none_or(|a| a.allows_id(snapshot, TermId::from_raw(s.graph))))
        .copied()
        .collect();
    with_members(&reader_core, c.expressions.iter().map(|e| &c.members.of[e]))
}

/// The expressions with a member in `of`, sorted: what the TBox reads of the memberships.
fn present(of: &HashMap<u64, Member>) -> Vec<u64> {
    let mut out: Vec<u64> = of
        .iter()
        .filter(|(_, m)| m.is_some())
        .map(|(&e, _)| e)
        .collect();
    out.sort_unstable();
    out
}

/// The membership changes `tx` makes: the `rdf:type` statements with a blank object it
/// asserts or retracts (`None` past [`TOUCHED`], or where `rdf:type` is new to it), for
/// [`QlRewriting::note_commit`]. Costs the transaction's changes.
pub fn touched_expressions(tx: &nrese_engine::Transaction<'_>) -> Option<Vec<MembershipChange>> {
    let Some(rdf_type) = iri(tx.base(), RDF, "type") else {
        // No `rdf:type` term before it: every statement it adds may be one.
        return tx.inserted().next().is_none().then(Vec::new);
    };
    let mut out: Vec<MembershipChange> = Vec::new();
    let changes = tx
        .inserted()
        .map(|q| (q, true))
        .chain(tx.deleted().map(|q| (q, false)));
    for (quad, added) in changes {
        if quad.predicate == rdf_type && quad.object.kind() == nrese_engine::TermKind::BlankNode {
            out.push(MembershipChange {
                statement: nrese_owl::Statement {
                    triple: [quad.subject.raw(), quad.predicate.raw(), quad.object.raw()],
                    graph: quad.graph.raw(),
                },
                added,
            });
            if out.len() > TOUCHED {
                return None;
            }
        }
    }
    // Removals first: a commit that replaces the kept member reads it again once.
    out.sort_by_key(|c| c.added);
    Some(out)
}

fn iri(snapshot: &Snapshot, namespace: &str, local: &str) -> Option<TermId> {
    snapshot.lookup(NamedNodeRef::new_unchecked(&format!("{namespace}{local}")).into())
}

/// The asserted statements of the schema, in every graph, sorted: the schema properties',
/// the declarations', the cells of the lists they name, and per class expression one
/// statement of an individual in it, from a graph `keep` admits.
fn schema_statements(snapshot: &Snapshot, keep: &dyn Fn(u64) -> bool) -> Vec<nrese_owl::Statement> {
    let core = read_core(snapshot);
    let members: Vec<Member> = expressions_of(snapshot, &core)
        .iter()
        .map(|&node| member_of(snapshot, node, keep))
        .collect();
    with_members(&core, members.iter())
}

#[cfg(test)]
thread_local! {
    /// What schema reads did on this thread (the reuse tests count it): memberships
    /// scanned, expressions walked, membership statements written into a schema.
    static MEMBER_SCANS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static EXPRESSIONS_WALKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MEMBERS_WRITTEN: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn count(cell: &'static std::thread::LocalKey<std::cell::Cell<usize>>, n: usize) {
    cell.set(cell.get() + n);
}

/// What an expression's membership is ([`schema_statements`]): one statement, or none.
type Member = Option<nrese_owl::Statement>;

/// The schema without its memberships, sorted, in every graph; and the class expressions
/// its statements make (blank subjects of a restriction's or a set operator's property),
/// sorted.
fn read_core(snapshot: &Snapshot) -> Vec<nrese_owl::Statement> {
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
    out
}

/// The class expressions `core` makes ([`read_core`]): blank subjects of a restriction's
/// or a set operator's property, sorted. Read from the core, never from data: an entry
/// whose core is unchanged keeps its list.
fn expressions_of(snapshot: &Snapshot, core: &[nrese_owl::Statement]) -> Vec<u64> {
    let operators: Vec<u64> = EXPRESSIONS
        .iter()
        .filter_map(|local| iri(snapshot, OWL, local))
        .map(TermId::raw)
        .collect();
    let mut out: Vec<u64> = core
        .iter()
        .filter(|s| operators.contains(&s.triple[1]))
        .map(|s| s.triple[0])
        .filter(|&node| TermId::from_raw(node).kind() == nrese_engine::TermKind::BlankNode)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The membership the schema reads of class expression `node` (design §7: individuals
/// asserted to be in it have anonymous individuals of their own): one `rdf:type`
/// statement, the first in a graph `keep` admits whose individual the OWL reader keeps (it
/// leaves out a reserved name, `rdf:type a _:e`: Sol r4 S1); `None` without one. What the
/// hazards read of it is the expression, not who is in it.
fn member_of(snapshot: &Snapshot, node: u64, keep: &dyn Fn(u64) -> bool) -> Member {
    #[cfg(test)]
    count(&MEMBER_SCANS, 1);
    let rdf_type = iri(snapshot, RDF, "type")?;
    snapshot
        .quads_for_pattern_in(
            ReadModel::Asserted,
            &QuadPattern {
                subject: None,
                predicate: Some(rdf_type),
                object: Some(TermId::from_raw(node)),
                graph: GraphSelector::Any,
            },
        )
        .map(|q| nrese_owl::Statement {
            triple: [q.subject.raw(), q.predicate.raw(), q.object.raw()],
            graph: q.graph.raw(),
        })
        .find(|s| eligible(snapshot, s, keep))
}

/// Whether membership `statement` is one the schema may keep for its reader: in a graph
/// `keep` admits, its individual a name the OWL reader keeps (not a reserved one).
fn eligible(
    snapshot: &Snapshot,
    statement: &nrese_owl::Statement,
    keep: &dyn Fn(u64) -> bool,
) -> bool {
    keep(statement.graph)
        && !matches!(snapshot.decode(TermId::from_raw(statement.triple[0])),
            Some(Term::NamedNode(n)) if nrese_owl::diagnostics::reserved_name(n.as_str()))
}

/// `core` with the memberships `members`, sorted: the schema the TBox is compiled from.
fn with_members<'a>(
    core: &[nrese_owl::Statement],
    members: impl Iterator<Item = &'a Member>,
) -> Vec<nrese_owl::Statement> {
    let mut out = core.to_vec();
    let before = out.len();
    out.extend(members.flatten().copied());
    #[cfg(test)]
    count(&MEMBERS_WRITTEN, out.len() - before);
    #[cfg(not(test))]
    let _ = before;
    out.sort();
    out.dedup();
    out
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
