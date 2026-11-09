//! OWL 2 QL answers through existentials (docs/design/ql-rewriting.md): queries rewritten
//! with the tree witnesses of the QL part of the snapshot's schema, over the closure the
//! store materialised.
//!
//! [`QlRewriting`] is the store's switch and its cache of the compiled schema; the
//! rewriting of a query's basic graph patterns is `native/ql.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use nrese_engine::{
    Admission, AssertedIdentity, GraphSelector, OwnState, QuadPattern, ReadModel, SharedState,
    Snapshot, SnapshotIdentity, TermId,
};
use nrese_owl::ql::Tbox;
use nrese_rdf::{NamedNodeRef, Term, vocab};

use crate::GraphAccess;

pub use nrese_owl::ql::{Closure, Limits};
pub use print::{PrintForm, Printed, print};

mod print;
#[cfg(test)]
mod tests;

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// The terms of the mapping's vocabulary ([`nrese_owl::Vocabulary`], by field) the reader
/// doesn't read as schema, each for its reason. It reads every other one: a property in
/// every statement it is the predicate of, a class (capitalised) in the `rdf:type`
/// statements of its members. So a term the mapping knows is never left out unclassified
/// (I24c-2; the test `the_reader_reads_the_mapping_vocabulary` holds the list to it).
const NOT_READ: &[&str] = &[
    // How the rest is read: declarations through `rdf:type`, lists along their cells.
    "rdf_type",
    "first",
    "rest",
    "nil",
    // Entities, not schema vocabulary: their statements are assertions.
    "owl_top_object_property",
    "owl_bottom_object_property",
    "owl_top_data_property",
    "owl_bottom_data_property",
    // Individual-level, possibly large, and none adds a schema axiom: equality is read as
    // present or not ([`has_equality`]); inequality and negative assertions conclude no
    // fact, and the classes' members are data.
    "owl_same_as",
    "owl_different_from",
    "owl_all_different",
    "owl_distinct_members",
    "owl_negative_property_assertion",
    "owl_source_individual",
    "owl_assertion_property",
    "owl_target_individual",
    "owl_target_value",
    "owl_named_individual",
    "owl_thing",
    "owl_nothing",
    // The ontology header and annotations, which the mapping reads past.
    "owl_ontology",
    "owl_imports",
    "owl_axiom",
    "owl_annotation",
    "owl_annotated_source",
    "owl_annotated_property",
    "owl_annotated_target",
];

// What the reader reads, chosen at compile time from the mapping's vocabulary
// (`nrese_owl::Vocabulary::IRIS`, `vocab::DATA_IRIS`): fixed IRIs cost nothing at run time
// (orchestrator, 8 Nov 17:28; #155's count found a `LazyLock` table's 4.6 KB of peak live
// bytes in every process).

/// Whether `a` and `b` are the same text (in a `const`).
const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// Whether vocabulary field `field` is one of [`NOT_READ`].
const fn not_read(field: &str) -> bool {
    let mut i = 0;
    while i < NOT_READ.len() {
        if same(NOT_READ[i], field) {
            return true;
        }
        i += 1;
    }
    false
}

/// Whether an IRI's local name is capitalised: a class, read through its members.
const fn capitalised(iri: &str) -> bool {
    let b = iri.as_bytes();
    let mut i = b.len();
    while i > 0 && b[i - 1] != b'#' && b[i - 1] != b'/' {
        i -= 1;
    }
    i < b.len() && b[i].is_ascii_uppercase()
}

/// Whether `iri` is one of the vocabulary's classes the reader reads.
const fn read_class(iri: &str) -> bool {
    let iris = nrese_owl::Vocabulary::IRIS;
    let mut i = 0;
    while i < iris.len() {
        let (field, term) = iris[i];
        if !not_read(field) && capitalised(term) && same(term, iri) {
            return true;
        }
        i += 1;
    }
    false
}

/// The IRIs the reader reads as classes (`class`) or as schema properties, the first `N`
/// of them, with how many there are. The properties take the data ranges' facets too
/// (`DATA_IRIS`, but `rdfs:Literal`: a datatype there and a class here).
const fn read_iris<const N: usize>(class: bool) -> ([&'static str; N], usize) {
    let iris = nrese_owl::Vocabulary::IRIS;
    let data = nrese_owl::vocab::DATA_IRIS;
    let mut out = [""; N];
    let mut n = 0;
    let mut i = 0;
    while i < iris.len() {
        let (field, iri) = iris[i];
        if !not_read(field) && capitalised(iri) == class {
            if n < N {
                out[n] = iri;
            }
            n += 1;
        }
        i += 1;
    }
    let mut j = 0;
    while !class && j < data.len() {
        if !read_class(data[j]) {
            if n < N {
                out[n] = data[j];
            }
            n += 1;
        }
        j += 1;
    }
    (out, n)
}

/// The schema properties read, in every graph.
const READ_PROPERTIES: [&str; read_iris::<0>(false).1] = read_iris(false).0;
/// The classes whose members' `rdf:type` statements are read.
const READ_CLASSES: [&str; read_iris::<0>(true).1] = read_iris(true).0;

/// The list-valued properties whose lists are read, by the vocabulary's fields.
const LISTS: &[&str] = &[
    "owl_intersection_of",
    "owl_union_of",
    "owl_disjoint_union_of",
    "owl_property_chain_axiom",
    "owl_has_key",
    "owl_one_of",
    "owl_members",
    "owl_on_properties",
    "owl_with_restrictions",
];

/// [`LISTS`]' IRIs; a field the vocabulary lacks fails the build.
const LIST_IRIS: [&str; LISTS.len()] = {
    let mut out = [""; LISTS.len()];
    let mut i = 0;
    while i < LISTS.len() {
        out[i] = nrese_owl::Vocabulary::iri(LISTS[i]);
        i += 1;
    }
    out
};

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
    /// Whether the snapshot asserts an equality of individuals, in any graph
    /// ([`has_equality`]).
    equality: bool,
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

    /// Commits `tx` with its note ([`Self::note_commit`]), which is in before the commit
    /// publishes: no reader sees the new snapshot without it, so none reads every
    /// expression again for it (P2-C1, 8 Nov: noted after publication, 40-48 of about 340
    /// new snapshots a run were read fresh). A commit that fails leaves a note to an
    /// asserted state no snapshot has, which nothing follows.
    pub fn commit(
        &self,
        tx: nrese_engine::Transaction<'_>,
    ) -> nrese_engine::EngineResult<nrese_engine::CommitSummary> {
        self.note_commit(
            tx.base().asserted_identity(),
            tx.asserted_identity_after(),
            touched_expressions(&tx),
        );
        tx.commit()
    }

    /// Notes a commit from asserted state `from` to `to` that made the membership
    /// `changes` (`None`: unknown or too many; [`touched_expressions`]): the next schema
    /// read keeps every membership those changes leave as it was. A write without a note
    /// makes the next read scan every expression.
    ///
    /// It replaces a note from `from`: a single writer commits from a state once, so an
    /// earlier note from it is of a commit that never published.
    fn note_commit(
        &self,
        from: AssertedIdentity,
        to: AssertedIdentity,
        changes: Option<Vec<MembershipChange>>,
    ) {
        if from == to {
            return;
        }
        let mut notes = self.notes.lock().unwrap_or_else(|p| p.into_inner());
        notes.retain(|n| n.from != from);
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

    /// Whether the data `snapshot` reads has the tree of probe `key` wherever it folds,
    /// memoised per view and probe (`None`: not asked, the query's questions spent;
    /// `false` then, and nothing kept). Only a published snapshot's answer is kept by the
    /// store (contract S3); any other's by its own version alone (dictionary 3.0c).
    /// Concurrent misses may compute independently; the query itself runs outside the lock.
    pub(crate) fn realised(
        &self,
        snapshot: &Snapshot,
        key: &str,
        compute: impl FnOnce() -> Option<bool>,
    ) -> bool {
        let identity = snapshot.identity();
        match snapshot.admission() {
            Admission::Shared(state) => {
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
                self.remember(&state, key, known);
                known
            }
            Admission::Own(own) => {
                let mine = own.get_or_insert_with(QlOwn::default);
                {
                    let probes = mine.probes.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(&known) = probes.get(&identity).and_then(|p| p.get(key)) {
                        return known;
                    }
                }
                let Some(known) = compute() else {
                    return false;
                };
                let mut probes = mine.probes.lock().unwrap_or_else(|p| p.into_inner());
                probes
                    .entry(identity)
                    .or_default()
                    .insert(key.to_owned(), known);
                known
            }
        }
    }

    /// Keeps probe `key`'s answer for the published snapshot `state` gives, replacing
    /// another view's probes.
    fn remember(&self, state: &SharedState<'_>, key: &str, known: bool) {
        let identity = state.snapshot().identity();
        let mut cache = self.realised.lock().unwrap_or_else(|p| p.into_inner());
        if cache.0 != Some(identity) {
            *cache = (Some(identity), HashMap::new());
        }
        cache.1.insert(key.to_owned(), known);
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
    ///
    /// Only a published snapshot changes what the store keeps (contract S3). Any other keeps
    /// what it reads in its own version (dictionary 3.0c; review r1 F1), so a pending
    /// snapshot read again reads its schema once, and the store's entries stay as they were.
    pub(crate) fn tbox(&self, snapshot: &Snapshot, access: Option<&GraphAccess>) -> Arc<Tbox> {
        let access = access.filter(|a| a.inferred_by_support);
        match snapshot.admission() {
            Admission::Shared(state) => self.shared_tbox(&state, access),
            Admission::Own(own) => self.own_tbox(&own, access),
        }
    }

    /// [`Self::tbox`] of the published snapshot `state` gives, kept in the store's entries.
    fn shared_tbox(&self, state: &SharedState<'_>, access: Option<&GraphAccess>) -> Arc<Tbox> {
        let snapshot = state.snapshot();
        let key = snapshot.identity();
        let mut cached = self.cached.lock().unwrap_or_else(|p| p.into_inner());
        let at = cached.iter().position(|c| c.access.as_ref() == access);
        if let Some(c) = at.map(|i| &cached[i]).filter(|c| c.key == key) {
            return Arc::clone(&c.tbox);
        }
        // A snapshot bounds its dictionary reads, so an identity hit above needs no
        // lookup. Across identities, unchanged statements alone are not sufficient.
        let Schema {
            thing,
            core,
            equality,
        } = Schema::of(snapshot);
        let asserted = snapshot.asserted_identity();
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
                && c.equality == equality
                && c.members.equality == key.equality
                && c.members.canonical == key.canonical
                && *c.core == *core)
                .then(|| self.changes_between(c.members.at, asserted))
                .flatten();
            if let Some(changes) = changes {
                return self.bring_forward(state, c, changes, &core, access);
            }
        }
        // Otherwise every expression's membership, read afresh.
        let (expressions, members) = fresh_members(snapshot, &core, access);
        let schema = Schema {
            thing,
            core,
            equality,
        };
        if let Some(c) = at
            .map(|i| &mut cached[i])
            .filter(|c| schema.reuses(c, &members))
        {
            rekey(state, c, members);
            return Arc::clone(&c.tbox);
        }
        let tbox = self.compile_read(snapshot, &schema, access, &expressions, &members);
        admit(
            state,
            &mut cached,
            at,
            schema.entry(access, key, expressions, members, Arc::clone(&tbox)),
        );
        tbox
    }

    /// [`Self::tbox`] of the unpublished snapshot `own` gives, kept in its version alone.
    /// An entry is this rewriting's (its closure), its reader's and its view's: views of one
    /// version in other equality modes share the slot, never an entry. A first read reuses
    /// a TBox its schema matches, the version's or the store's (read only), or compiles.
    fn own_tbox(&self, own: &OwnState<'_>, access: Option<&GraphAccess>) -> Arc<Tbox> {
        let snapshot = own.snapshot();
        let key = snapshot.identity();
        let mine = own.get_or_insert_with(QlOwn::default);
        let mut entries = mine.entries.lock().unwrap_or_else(|p| p.into_inner());
        let ours = |(closure, c): &&(Closure, Cached)| {
            *closure == self.closure && c.access.as_ref() == access
        };
        if let Some((_, c)) = entries.iter().filter(ours).find(|(_, c)| c.key == key) {
            return Arc::clone(&c.tbox);
        }
        let schema = Schema::of(snapshot);
        let (expressions, members) = fresh_members(snapshot, &schema.core, access);
        let kept = entries
            .iter()
            .filter(ours)
            .map(|(_, c)| c)
            .find(|c| schema.reuses(c, &members))
            .map(|c| Arc::clone(&c.tbox));
        let reused = kept.or_else(|| {
            let shared = self.cached.lock().unwrap_or_else(|p| p.into_inner());
            shared
                .iter()
                .filter(|c| c.access.as_ref() == access)
                .find(|c| schema.reuses(c, &members))
                .map(|c| Arc::clone(&c.tbox))
        });
        let tbox = reused.unwrap_or_else(|| {
            self.compile_read(snapshot, &schema, access, &expressions, &members)
        });
        if entries.len() == READERS {
            entries.remove(0);
        }
        let entry = schema.entry(access, key, expressions, members, Arc::clone(&tbox));
        entries.push((self.closure, entry));
        tbox
    }

    /// The TBox of a schema read afresh: `schema`'s core in the reader's graphs, with
    /// `members`.
    fn compile_read(
        &self,
        snapshot: &Snapshot,
        schema: &Schema,
        access: Option<&GraphAccess>,
        expressions: &[u64],
        members: &Members,
    ) -> Arc<Tbox> {
        let statements = with_members(
            &reader_core(snapshot, &schema.core, access),
            expressions.iter().map(|e| &members.of[e]),
        );
        Arc::new(compile(
            snapshot,
            &statements,
            self.closure,
            schema.thing,
            schema.equality,
        ))
    }

    /// Brings entry `c`'s memberships forward to the published snapshot `state` gives, by
    /// the noted `changes`, and recompiles its TBox where an expression gained or lost its
    /// last member (design §7). Its `thing`, equality flag and `core` are the snapshot's.
    fn bring_forward(
        &self,
        state: &SharedState<'_>,
        c: &mut Cached,
        changes: Vec<MembershipChange>,
        core: &[nrese_owl::Statement],
        access: Option<&GraphAccess>,
    ) -> Arc<Tbox> {
        let snapshot = state.snapshot();
        let keep = |graph: u64| {
            access.is_none_or(|access| access.allows_id(snapshot, TermId::from_raw(graph)))
        };
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
                (Some(kept), false) if kept == change.statement => member_of(snapshot, node, &keep),
                _ => continue,
            };
            changed |= next.is_some() != member.is_some();
            c.members.of.insert(node, next);
        }
        c.key = snapshot.identity();
        c.members.at = snapshot.asserted_identity();
        // Which expressions have a member is what the TBox reads (design §7).
        if !changed {
            return Arc::clone(&c.tbox);
        }
        let statements = filtered_schema(c, snapshot, core, access);
        let tbox = Arc::new(compile(
            snapshot,
            &statements,
            self.closure,
            c.thing,
            c.equality,
        ));
        c.tbox = Arc::clone(&tbox);
        tbox
    }
}

/// What an unpublished version keeps of the QL rewriting for its own readers (dictionary
/// 3.0c; review r1 F1), dropped with the version: the schema entries of every rewriting
/// (by its closure) that read it, at most [`READERS`], and each view's probes.
#[derive(Debug, Default)]
struct QlOwn {
    entries: Mutex<Vec<(Closure, Cached)>>,
    probes: Mutex<HashMap<SnapshotIdentity, HashMap<String, bool>>>,
}

/// What decides a snapshot's compiled TBox besides its memberships ([`Cached`]'s checks).
struct Schema {
    thing: Option<nrese_owl::Term>,
    core: Vec<nrese_owl::Statement>,
    equality: bool,
}

impl Schema {
    fn of(snapshot: &Snapshot) -> Self {
        Self {
            thing: snapshot
                .lookup(nrese_rdf::vocab::owl::THING.into())
                .map(TermId::raw),
            core: read_core(snapshot),
            equality: has_equality(snapshot),
        }
    }

    /// Whether entry `c`'s TBox is this schema's with `members`: what the TBox reads of
    /// them is which expressions have one (design §7).
    fn reuses(&self, c: &Cached, members: &Members) -> bool {
        c.thing == self.thing
            && c.equality == self.equality
            && *c.core == *self.core
            && present(&c.members.of) == present(&members.of)
    }

    fn entry(
        self,
        access: Option<&GraphAccess>,
        key: SnapshotIdentity,
        expressions: Vec<u64>,
        members: Members,
        tbox: Arc<Tbox>,
    ) -> Cached {
        Cached {
            access: access.cloned(),
            key,
            core: self.core.into_boxed_slice(),
            expressions: expressions.into_boxed_slice(),
            thing: self.thing,
            equality: self.equality,
            tbox,
            members,
        }
    }
}

/// The class expressions of `core` and each one's membership in `snapshot`, read afresh,
/// in the graphs `access` admits.
fn fresh_members(
    snapshot: &Snapshot,
    core: &[nrese_owl::Statement],
    access: Option<&GraphAccess>,
) -> (Vec<u64>, Members) {
    let keep = |graph: u64| {
        access.is_none_or(|access| access.allows_id(snapshot, TermId::from_raw(graph)))
    };
    let expressions = expressions_of(snapshot, core);
    #[cfg(test)]
    count(&EXPRESSIONS_WALKED, expressions.len());
    let of = expressions
        .iter()
        .map(|&node| (node, member_of(snapshot, node, &keep)))
        .collect();
    let key = snapshot.identity();
    let members = Members {
        at: snapshot.asserted_identity(),
        equality: key.equality,
        canonical: key.canonical,
        of,
    };
    (expressions, members)
}

/// Entry `c` stands for the published snapshot `state` gives, with its `members`: what
/// the TBox reads of them is unchanged.
fn rekey(state: &SharedState<'_>, c: &mut Cached, members: Members) {
    c.key = state.snapshot().identity();
    c.members = members;
}

/// Keeps `entry` for the published snapshot `state` gives, in place of its reader's entry
/// at `at`, evicting the oldest past [`READERS`].
fn admit(state: &SharedState<'_>, cached: &mut Vec<Cached>, at: Option<usize>, entry: Cached) {
    if let Some(i) = at {
        cached.remove(i);
    }
    if cached.len() == READERS {
        cached.remove(0);
    }
    cached.push(Cached {
        key: state.snapshot().identity(),
        ..entry
    });
}

/// The schema of entry `c` as its reader compiles it: `core` in the reader's graphs, with
/// `c`'s memberships.
fn filtered_schema(
    c: &Cached,
    snapshot: &Snapshot,
    core: &[nrese_owl::Statement],
    access: Option<&GraphAccess>,
) -> Vec<nrese_owl::Statement> {
    with_members(
        &reader_core(snapshot, core, access),
        c.expressions.iter().map(|e| &c.members.of[e]),
    )
}

/// `core` in the graphs `access` admits.
fn reader_core(
    snapshot: &Snapshot,
    core: &[nrese_owl::Statement],
    access: Option<&GraphAccess>,
) -> Vec<nrese_owl::Statement> {
    core.iter()
        .filter(|s| access.is_none_or(|a| a.allows_id(snapshot, TermId::from_raw(s.graph))))
        .copied()
        .collect()
}

/// Whether the snapshot asserts an equality of individuals (`owl:sameAs`), in any graph:
/// one probe. The TBox learns only that ([`Tbox::with_equality`]), never which statement:
/// no name reaches a reader who can't read its graph (I24c-2 review r1, F1), and no sampled
/// statement the mapping may leave out stands for the rest (F3). Fail-closed for readers
/// who see only some graphs.
fn has_equality(snapshot: &Snapshot) -> bool {
    iri(snapshot, OWL, "sameAs").is_some_and(|same_as| {
        snapshot
            .quads_for_pattern_in(
                ReadModel::Asserted,
                &QuadPattern {
                    subject: None,
                    predicate: Some(same_as),
                    object: None,
                    graph: GraphSelector::Any,
                },
            )
            .next()
            .is_some()
    })
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
fn touched_expressions(tx: &nrese_engine::Transaction<'_>) -> Option<Vec<MembershipChange>> {
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
    named(snapshot, &format!("{namespace}{local}"))
}

fn named(snapshot: &Snapshot, iri: &str) -> Option<TermId> {
    snapshot.lookup(NamedNodeRef::new_unchecked(iri).into())
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
    for p in READ_PROPERTIES.iter().filter_map(|p| named(snapshot, p)) {
        scan(pattern(p, None), &mut out);
    }
    let lookup = |term: NamedNodeRef<'_>| snapshot.lookup(term.into());
    if let Some(rdf_type) = lookup(vocab::rdf::TYPE) {
        for class in READ_CLASSES.iter().filter_map(|c| named(snapshot, c)) {
            scan(pattern(rdf_type, Some(class)), &mut out);
        }
    }
    // The lists: from each head, `rdf:first` and `rdf:rest` along the cells.
    let (first, rest) = (lookup(vocab::rdf::FIRST), lookup(vocab::rdf::REST));
    if let (Some(first), Some(rest)) = (first, rest) {
        let heads: Vec<u64> = LIST_IRIS
            .iter()
            .filter_map(|p| named(snapshot, p))
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
/// whose core is unchanged keeps its list. `core` is sorted, so each subject's statements
/// are adjacent, and each subject is taken once, in order.
fn expressions_of(snapshot: &Snapshot, core: &[nrese_owl::Statement]) -> Vec<u64> {
    debug_assert!(core.is_sorted());
    // The mapping's own markers, those it can't read included (an n-ary or a malformed
    // restriction), so the memberships of every node it would read as a class expression
    // reach it, and it reports what it can't read (I24c-2 review r1, F2; r2, F5).
    let id_of = |iri: &str| named(snapshot, iri).map(TermId::raw);
    let markers = nrese_owl::mapping::ClassExpressionMarkers::lookup(&id_of);
    let operators = markers.properties();
    let types: Vec<u64> = markers.types.into_iter().flatten().collect();
    let rdf_type = id_of(const { nrese_owl::Vocabulary::iri("rdf_type") });
    // A restriction has both a marker type and a marker property: matched statement by
    // statement, each was found twice, then sorted and deduplicated (i24c2-attrib3-da692156).
    core.chunk_by(|a, b| a.triple[0] == b.triple[0])
        .filter(|of| {
            TermId::from_raw(of[0].triple[0]).kind() == nrese_engine::TermKind::BlankNode
                && of.iter().any(|s| {
                    operators.contains(&s.triple[1])
                        || (Some(s.triple[1]) == rdf_type && types.contains(&s.triple[2]))
                })
        })
        .map(|of| of[0].triple[0])
        .collect()
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
    equality: bool,
) -> Tbox {
    let terms = SnapshotTerms(snapshot);
    let ontology = nrese_owl::read(statements, &terms);
    let tbox = Tbox::compile(&ontology, closure, thing);
    match equality {
        true => tbox.with_equality(closure),
        false => tbox,
    }
}
