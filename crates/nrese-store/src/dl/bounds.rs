//! The bounds of the store's latest revision (docs/design/owl2-dl.md §8): the lower bound
//! L is the inferred stack (the OWL 2 RL closure); the upper bound U1 ([`super::upper`])
//! is kept here, beside the engine's stacks, and maintained per commit.
//!
//! - **On commit** ([`prepare`], [`install`]): the commit's change to U1 is computed
//!   inside the commit (by the delta executor, or a rebuild where the schema changed) and
//!   applied once the commit is done; U1 after the change also answers the commit's
//!   consistency check where it can (no clash and every `⊥` checked: consistent, with no
//!   DL engine run).
//! - **On query** ([`view`]): the revision's read view, built once per revision: a
//!   snapshot with U1's facts in its inferred stack, never published, and which
//!   predicates and classes U1 has facts on beyond L (the gap's signature). A revision U1
//!   doesn't describe (a write past the pipeline, a start) gets U1 built afresh.
//!
//! Every U1 evaluation carries this store's `MemoryWatch` through `Stop`, including
//! embedded reads and diagnostic rebuilds. A stopped U1 is unavailable, never complete;
//! zero disables the store's watch without changing the process owner's fallback.

use nrese_dl::classify::Taxonomy;
use nrese_dl::limits::Deadline;
use nrese_exec::ChargedVec;
use nrese_exec::cell::{BuildCell, Claim, End, Ended, Joined, Kind};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nrese_engine::{
    CompactionPolicy, EncodedQuad, EncodedTriple, PendingView, ReadModel, Snapshot, TermId,
    Transaction, TripleVersion,
};
use nrese_owl::Ontology;
use nrese_reasoner::eval::Stop;
use nrese_reasoner::ir::Triple;

use super::source;
use super::upper::{Change, GaveUp, Upper, UpperVersion};
use crate::{StoreService, service::ReadBasis};

#[cfg(test)]
#[path = "bounds_memory_tests.rs"]
mod memory_tests;

#[cfg(test)]
#[path = "bounds_token_tests.rs"]
mod token_tests;

#[cfg(test)]
#[path = "bounds_lock_tests.rs"]
mod lock_tests;

#[cfg(test)]
#[path = "bounds_kinds_tests.rs"]
mod kinds_tests;

#[cfg(test)]
#[path = "bounds_shapes_tests.rs"]
mod shapes_tests;

/// Where the lock tests hold U1's work mid-way: a hook run once, on the thread doing that
/// work, for the store whose bounds it names.
#[cfg(test)]
pub(crate) mod seam {
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::sync::Mutex;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Point {
        /// A commit's change of U1, U1 taken and the change not begun.
        Change,
        /// U1 built afresh for a read, not begun.
        Build,
        /// Make the next read's U1 build start past its deadline.
        BuildDeadline,
        /// The TBox's taxonomy for a read, not begun.
        Taxonomy,
        /// A read has pinned the publication's upper bound.
        Eligible,
        /// A commit built the next eligible set and is about to build the stack.
        StageStack,
        /// A commit finished the stack build attempt.
        StackAttempted,
        /// A revision's read view, not begun.
        View,
        /// A commit's ontology read again, not begun.
        Read,
    }

    type Hook = Box<dyn FnOnce() + Send>;
    /// One hook a store and point: tests running at once, each on its own store, keep theirs.
    static HOOKS: Mutex<Vec<(usize, Point, Hook)>> = Mutex::new(Vec::new());

    struct Probes {
        store: Arc<super::StoreService>,
        total: usize,
        locked: usize,
    }

    thread_local! {
        static PROBES: RefCell<Option<Probes>> = const { RefCell::new(None) };
    }

    /// Counts the read's probes on this thread, and which found its store's state locked.
    pub(super) fn count_probes(
        store: &Arc<super::StoreService>,
        read: impl FnOnce(),
    ) -> (usize, usize) {
        PROBES.set(Some(Probes {
            store: Arc::clone(store),
            total: 0,
            locked: 0,
        }));
        read();
        let probes = PROBES.take().unwrap();
        (probes.total, probes.locked)
    }

    pub(super) fn snapshot_probe() {
        PROBES.with_borrow_mut(|probes| {
            if let Some(probes) = probes {
                probes.total += 1;
                if probes.store.dl().bounds.state.try_lock().is_err() {
                    probes.locked += 1;
                }
            }
        });
    }

    fn key(bounds: &super::Bounds) -> usize {
        std::ptr::from_ref(bounds) as usize
    }

    pub(crate) fn set(bounds: &super::Bounds, point: Point, hook: impl FnOnce() + Send + 'static) {
        let mut hooks = HOOKS.lock().unwrap_or_else(|p| p.into_inner());
        hooks.retain(|(k, p, _)| !(*k == key(bounds) && *p == point));
        hooks.push((key(bounds), point, Box::new(hook)));
    }

    pub(crate) fn at(bounds: &super::Bounds, point: Point) -> bool {
        let hook = {
            let mut hooks = HOOKS.lock().unwrap_or_else(|p| p.into_inner());
            let at = hooks
                .iter()
                .position(|(k, p, _)| *k == key(bounds) && *p == point);
            at.map(|at| hooks.swap_remove(at))
        };
        if let Some((_, _, hook)) = hook {
            hook();
            return true;
        }
        false
    }
}

/// U1 at a revision.
struct State {
    revision: u64,
    /// Shared with the commit computing its change ([`prepare`]). Readers get the
    /// immutable evidence from the publication itself.
    upper: Result<Arc<Upper>, GaveUp>,
    /// How U1 got to this revision: `delta` (the delta executor), `rebuilt` (compiled
    /// and evaluated afresh on commit), `read` (afresh for a query), `kept` (U1 gave up
    /// and nothing it is compiled from changed).
    last: &'static str,
    /// Which compilation of U1 this is: a new one each time U1 is compiled afresh, kept
    /// by a commit's change. The taxonomy's key.
    compiled: u64,
    /// `Some`: the ontology is in OWL 2 RL, so the RL rules decide it (theorem PR1 of OWL 2
    /// Profiles: complete for its assertions and its consistency) and U1 isn't compiled;
    /// with the kinds the route was decided on.
    kinds: Option<Arc<Kinds>>,
}

/// The published half of U1. Taxonomy work is shared by publications of one compilation.
#[derive(Clone)]
pub(crate) struct Published {
    upper: Result<UpperVersion, GaveUp>,
    compiled: u64,
    taxonomy: Arc<Mutex<Option<KeptTaxonomy>>>,
    last: &'static str,
}

impl Published {
    fn new(bounds: &Bounds, upper: Result<UpperVersion, GaveUp>, last: &'static str) -> Self {
        Self {
            upper,
            compiled: bounds.compiled(),
            taxonomy: Arc::default(),
            last,
        }
    }

    pub(crate) fn compiled(&self) -> u64 {
        self.compiled
    }

    pub(crate) fn refused(&self) -> bool {
        self.upper.as_ref().is_err_and(|g| g.refused)
    }
}

/// The bounds' U1 (`None`: none), and the revision below which nothing is published: what
/// is published only moves forward (U1-P), a generation let go included (SYN-61).
#[derive(Default)]
struct Slot {
    current: Option<State>,
    floor: u64,
    // The report survives a U1 eviction or a commit that did not read the ontology.
    anonymous: Option<(u64, Option<String>)>,
}

impl std::ops::Deref for Slot {
    type Target = Option<State>;
    fn deref(&self) -> &Option<State> {
        &self.current
    }
}

impl Slot {
    fn record_anonymous(&mut self, revision: u64, read: Option<Option<String>>) {
        if self
            .anonymous
            .as_ref()
            .is_some_and(|(at, _)| *at > revision || (*at == revision && read.is_none()))
        {
            return;
        }
        let note = match read {
            Some(note) => note.map(|text| format!("{text} as of revision {revision}")),
            None => self.anonymous.take().and_then(|(_, note)| note),
        };
        self.anonymous = Some((revision, note));
    }

    fn get_mut(&mut self) -> Option<&mut State> {
        self.current.as_mut()
    }

    /// Lets U1 go as `revision` supersedes it: nothing older is published after.
    fn clear(&mut self, revision: u64) -> Option<State> {
        self.floor = self.floor.max(revision);
        self.current.take()
    }

    /// Lets U1 go if `revision` supersedes it.
    fn evict_below(&mut self, revision: u64) -> Option<State> {
        if self.current.as_ref().is_some_and(|s| s.revision < revision) {
            self.clear(revision)
        } else {
            None
        }
    }
}

/// Installs `state` unless the bounds describe its revision or a later one already, or let
/// a generation go for a later one: what is published only moves forward, and U1 of a
/// revision, once there, stands.
fn publish(slot: &mut Slot, state: State) {
    let forward = slot
        .current
        .as_ref()
        .is_none_or(|s| s.revision < state.revision);
    if forward && state.revision >= slot.floor {
        slot.current = Some(state);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The route the ontology's profile gives (`nrese_owl::profile`): an OWL 2 RL ontology (its
/// data's literals of RL's datatypes too) to the RL rules alone where the premise of their
/// closure holds, the graph exactly the mapping of what the reader read
/// (`nrese_owl::profile::premise`, the one place that tells it; `query::reads_differently`
/// says what a break costs). Its callers also leave the route where the closure leaves out
/// a list axiom (`StoreService::lists_left_out`), the store's own: the rules lack that
/// axiom's conclusions.
fn routed_to_rules(ontology: &Ontology) -> bool {
    nrese_owl::profile::of_ontology(ontology).rl && nrese_owl::profile::premise(ontology).exact()
}

/// What the RL route was decided on: the TBox signature as the reader typed it, the
/// built-in annotation properties among its annotation properties, the SHACL properties
/// whose statements were read as SHACL content (`Ontology::shacl`), and the ids of
/// `rdf:type`, `owl:sameAs` and `rdf:first` (`None`: not in the dictionary then).
pub(crate) struct Kinds {
    signature: nrese_owl::profile::Signature,
    shacl: std::collections::BTreeSet<u64>,
    rdf_type: Option<u64>,
    same_as: Option<u64>,
    first: Option<u64>,
}

impl Kinds {
    fn of(ontology: &Ontology, lookup: &dyn Fn(&str) -> Option<TermId>) -> Arc<Self> {
        let id = |iri: &str| lookup(iri).map(TermId::raw);
        let mut signature = nrese_owl::profile::signature(ontology);
        signature.annotations.extend(
            nrese_owl::BUILTIN_ANNOTATIONS
                .iter()
                .filter_map(|iri| id(iri)),
        );
        Arc::new(Self {
            signature,
            shacl: ontology.shacl.clone(),
            rdf_type: id("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            same_as: id("http://www.w3.org/2002/07/owl#sameAs"),
            first: id("http://www.w3.org/1999/02/22-rdf-syntax-ns#first"),
        })
    }

    /// Whether `t` is a list's cell at `tx`'s base: a blank node with an asserted `rdf:first`
    /// there (one probe of the committed index; the pending changes aren't scanned, so a
    /// commit's cost stays linear in its statements). The reader reports a cell that a
    /// statement outside its list names (`Ontology::list_cell_outside`), so such a statement
    /// is read. A cell new in the commit has its own pending `rdf:first`, which
    /// [`changes_schema`] already sends to the reader; without `rdf:first` in the dictionary
    /// at the route's revision there was no cell at the base.
    fn cell(&self, tx: &impl PendingView, t: u64) -> bool {
        let id = TermId::from_raw(t);
        let Some(first) = self.first else {
            return false;
        };
        if id.kind() != nrese_engine::TermKind::BlankNode {
            return false;
        }
        let mut pattern = nrese_engine::QuadPattern::all();
        (pattern.subject, pattern.predicate) = (Some(id), Some(TermId::from_raw(first)));
        let base = tx.base();
        let mut found = base.quads_for_pattern_in(ReadModel::Asserted, &pattern);
        found.next().is_some()
    }

    /// Whether an asserted triple, inserted or deleted, leaves every term the kinds the
    /// reader gave it, which it gives by use (`nrese_owl::profile::Signature`): a statement of
    /// a SHACL property the reader read as SHACL content gives none (it has no OWL meaning),
    /// nor one of a known annotation property that is no SHACL property; a SHACL property's
    /// first content may change how the reader reads the whole source (`Ontology::shacl`), so
    /// it reads again, whatever the property is declared; any other leaves the schema as it is,
    /// names no class or property, and no name of the reserved vocabulary, which the reader
    /// leaves out (`nrese_owl::diagnostics::reserved_name`), as subject or IRI object, and
    /// its predicate is `rdf:type` with a known class, `owl:sameAs`, or a property known
    /// only as the kind its object needs (a literal a data property, an IRI an object
    /// property). A deleted value needs a property declared its kind ([`may_retype`]): one
    /// that is data only by its literals may lose the last one, and its other axioms then
    /// read it as an object property, and the other way round. A statement that names a
    /// list's cell ([`Self::cell`]), an annotation too, is read, and so is one about the
    /// ontology's node ([`header`]) but a known annotation property's.
    fn keep(&self, tx: &impl PendingView, triple @ [s, p, o]: Triple, deleted: bool) -> bool {
        let k = &self.signature;
        if self.cell(tx, s) || self.cell(tx, o) {
            return false;
        }
        if self.shacl.contains(&p) || (k.annotations.contains(&p) && !named(tx, p, None)) {
            return true;
        }
        if changes_schema(tx, triple)
            || k.names.contains(&s)
            || reserved_named(tx, triple)
            || header(tx, s)
        {
            return false;
        }
        let iri = TermId::from_raw(o).kind() == nrese_engine::TermKind::Iri;
        if Some(p) == self.rdf_type {
            k.classes.contains(&o)
        } else if !iri {
            k.data.contains(&p)
                && !k.objects.contains(&p)
                && !may_retype(tx, triple, deleted)
                && (deleted || rl_literal(tx, o))
        } else {
            let property = k.objects.contains(&p) && !k.data.contains(&p);
            let property = property && !may_retype(tx, triple, deleted);
            !k.names.contains(&o) && (Some(p) == self.same_as || property)
        }
    }
}

/// Whether `s` is the ontology's node at `tx`'s base: typed `owl:Ontology` there (one probe
/// of the committed index). A statement about it is an annotation or an assertion by its
/// property's declarations (*Mapping to RDF* Table 10), which only the reader tells: so it is
/// read. A node typed so in the commit itself is read by that statement, OWL's vocabulary.
fn header(tx: &impl PendingView, s: u64) -> bool {
    let id = |iri: &str| tx.lookup(nrese_rdf::NamedNodeRef::new_unchecked(iri).into());
    let (Some(ty), Some(ontology)) = (
        id("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        id("http://www.w3.org/2002/07/owl#Ontology"),
    ) else {
        return false;
    };
    let mut pattern = nrese_engine::QuadPattern::all();
    (pattern.subject, pattern.predicate, pattern.object) =
        (Some(TermId::from_raw(s)), Some(ty), Some(ontology));
    let base = tx.base();
    let mut found = base.quads_for_pattern_in(ReadModel::Asserted, &pattern);
    found.next().is_some()
}

/// Whether OWL 2 RL admits the literal `o`'s datatype (`nrese_owl::profile::rl_datatype`):
/// every inline kind's is the map's but `xsd:date`'s; any other literal is read.
fn rl_literal(tx: &impl PendingView, o: u64) -> bool {
    match TermId::from_raw(o).kind() {
        nrese_engine::TermKind::Date => false,
        nrese_engine::TermKind::TypedLiteral => match tx.decode(TermId::from_raw(o)) {
            Some(nrese_rdf::Term::Literal(l)) => {
                nrese_owl::profile::rl_datatype(l.datatype().as_str())
            }
            _ => false,
        },
        _ => true,
    }
}

/// U1's place on the RL route: not compiled.
fn not_compiled() -> GaveUp {
    GaveUp::new("not compiled: the ontology is in OWL 2 RL, which the RL rules decide")
}

/// The store's bounds: U1 for the revision it describes, and the read view of the latest
/// revision a query asked for.
///
/// **No lock here is held across Rayon work, a wait, or a call into the engine**: under
/// `state` and `view` runs only serial
/// work over what is in memory. U1's change, its builds, its taxonomy and a view's runs
/// all run with none held, so whoever waits for one of these locks waits for a holder
/// that waits for nothing. A build several readers want is built once through a
/// [`BuildCell`]: a reader off the pool waits for it; one on a Rayon worker builds its
/// own (the bounded rule, `research-designs.md` §8). Lock order: the engine's writer slot,
/// the pipeline's ground cache, then `state` (a leaf); a cell's own lock, then `view` or
/// `state`; every dictionary lock after them. Inside a build cell, the end of a build nests
/// the cell's `building` mutex → the build's `state` mutex → its terminal's lock; nothing
/// takes an outer one of these while holding an inner one.
#[derive(Default)]
pub(crate) struct Bounds {
    state: Mutex<Slot>,
    view: Mutex<Option<(ReadKey, Arc<View>)>>,
    /// Read views being built, by publication (U1 built afresh among them).
    views: BuildCell<ReadKey, View>,
    /// Taxonomies being built, by U1's compilation ([`State::compiled`]).
    /// What a build hands its waiters is the taxonomy with its charge (SYN-61).
    taxonomies: BuildCell<u64, nrese_exec::cell::Kept<Taxonomy>>,
    compilations: AtomicU64,
    compaction: Compaction,
    /// What the bounds keep is charged here while they keep it (SYN-61): the store's, within
    /// the queries' memory ([`Bounds::set_kept`]); unlimited until the store sets it.
    kept: std::sync::OnceLock<Arc<nrese_exec::Budget>>,
}

type ReadKey = (nrese_engine::SnapshotIdentity, Option<u64>);

/// A single background owner prepares large merges for the next writer. Its mailbox is
/// advanced when the writer installs; a finished merge splices into that newer pair.
/// Readers own the lists in their publications and never wait for this owner.
#[derive(Default)]
struct Compaction(Arc<Mutex<Compacting>>);

#[derive(Default)]
struct Compacting {
    latest: Option<(u64, TripleVersion, TripleVersion)>,
    active: bool,
    #[cfg(test)]
    panic_before_merge: bool,
}

struct CompactionOwner<'a>(Option<&'a Mutex<Compacting>>);

impl CompactionOwner<'_> {
    /// Release while holding the mailbox so a publisher can see that it should start work.
    fn finish(&mut self, slot: &mut Compacting) {
        slot.active = false;
        self.0 = None;
    }
}

impl Drop for CompactionOwner<'_> {
    fn drop(&mut self) {
        if let Some(shared) = self.0.take() {
            lock(shared).active = false;
        }
    }
}

impl Compaction {
    fn take(&self, revision: u64) -> Option<(TripleVersion, TripleVersion)> {
        let slot = lock(&self.0);
        let (_, stack, eligible) = slot.latest.as_ref().filter(|(at, ..)| *at == revision)?;
        Some((stack.clone(), eligible.clone()))
    }

    fn publish(&self, revision: u64, upper: Option<&UpperVersion>) {
        let policy = CompactionPolicy::default();
        let mut slot = lock(&self.0);
        slot.latest = upper
            .filter(|u| {
                slot.active
                    || u.stack.needs_compaction(policy)
                    || u.eligible.needs_compaction(policy)
            })
            .map(|u| (revision, u.stack.clone(), u.eligible.clone()));
        if slot.latest.is_none() || slot.active {
            return;
        }
        slot.active = true;
        let shared = Arc::clone(&self.0);
        drop(slot);
        rayon::spawn(move || {
            let mut owner = CompactionOwner(Some(&shared));
            loop {
                let mut slot = lock(&shared);
                let Some((revision, stack, eligible)) = slot.latest.as_ref() else {
                    owner.finish(&mut slot);
                    return;
                };
                let revision = *revision;
                let stack = stack.plan_compaction(policy);
                let eligible = eligible.plan_compaction(policy);
                #[cfg(test)]
                let panic_before_merge = std::mem::take(&mut slot.panic_before_merge);
                drop(slot);
                #[cfg(test)]
                assert!(!panic_before_merge, "injected compaction panic");
                let stack = stack.map(|build| build());
                let eligible = eligible.map(|build| build());
                let mut slot = lock(&shared);
                let Some((latest, current_stack, current_eligible)) = &mut slot.latest else {
                    owner.finish(&mut slot);
                    return;
                };
                let mut again = *latest != revision;
                if let Some(Ok(splice)) = stack
                    && let Ok(Some(merged)) = splice(current_stack)
                {
                    *current_stack = merged;
                    again |= current_stack.needs_compaction(policy);
                }
                if let Some(Ok(splice)) = eligible
                    && let Ok(Some(merged)) = splice(current_eligible)
                {
                    *current_eligible = merged;
                    again |= current_eligible.needs_compaction(policy);
                }
                if !again {
                    owner.finish(&mut slot);
                    return;
                }
            }
        });
    }
}

impl Bounds {
    /// A new compilation's number.
    fn compiled(&self) -> u64 {
        self.compilations.fetch_add(1, Ordering::Relaxed)
    }

    /// The budget what the bounds keep is charged to: set once, by the store, before any read.
    pub(crate) fn set_kept(&self, budget: Arc<nrese_exec::Budget>) {
        let _ = self.kept.set(budget);
    }

    /// What the bounds keep is charged to (`Bounds::set_kept`).
    pub(crate) fn kept(&self) -> &Arc<nrese_exec::Budget> {
        self.kept
            .get_or_init(|| Arc::new(nrese_exec::Budget::unlimited()))
    }

    pub(super) fn anonymous(&self, revision: u64) -> Option<String> {
        lock(&self.state)
            .anonymous
            .as_ref()
            .filter(|(at, _)| *at == revision)
            .and_then(|(_, note)| note.clone())
    }

    pub(super) fn publish_anonymous(&self, revision: u64, ontology: &Ontology) {
        let report = anonymous_report(ontology);
        lock(&self.state).record_anonymous(revision, Some(report));
    }

    /// Whether the commit `tx` keeps the RL route its base is on ([`route_kept`]), by the kinds
    /// kept at the base's revision; taken under the state, checked without it.
    pub(crate) fn keeps_route(&self, tx: &Transaction<'_>, lists_left_out: bool) -> bool {
        let base = tx.base().revision();
        let kinds = lock(&self.state)
            .as_ref()
            .filter(|s| s.revision == base)
            .and_then(|s| s.kinds.clone());
        kinds.is_some_and(|kinds| route_kept(&kinds, tx, lists_left_out))
    }
}

/// Whether the commit `tx` keeps the RL route `kinds` was decided on: the closure it leaves
/// leaves out no list axiom, no asserted change gives a term a new kind ([`Kinds::keep`]), and
/// none is a reified triple ([`changes_reified`]).
fn route_kept(kinds: &Kinds, tx: &impl PendingView, lists_left_out: bool) -> bool {
    !lists_left_out
        && tx.inserted().all(|q| kinds.keep(tx, quad_triple(q), false))
        && tx.deleted().all(|q| kinds.keep(tx, quad_triple(q), true))
        && !changes_reified(tx)
}

/// Whether the delta executor changes `upper` for `tx`'s asserted changes, which leave what
/// the reader reads of the base as it was but for them: the transaction's part
/// ([`probed_assertions`]) and U1's ([`upper_keeps`]).
fn assertions_only(upper: &Upper, tx: &impl PendingView) -> bool {
    probed_assertions(tx).is_some_and(|triples| upper_keeps(upper, &triples))
}

/// The delta's carry-over as far as the transaction tells it, which reads the engine and the
/// dictionary: `tx`'s changed triples, inserted and deleted, where none has OWL's, RDF's or
/// RDFS's vocabulary as predicate but `rdf:type` and `owl:sameAs` ([`vocabulary_predicate`]),
/// names a reserved name or SHACL property ([`reserved_named`]) or says something about the
/// ontology's node ([`header`]), no changed value may retype its property ([`may_retype`]),
/// and none is a reified triple ([`changes_reified`]); `None` otherwise.
fn probed_assertions(tx: &impl PendingView) -> Option<Vec<Triple>> {
    let triples: Vec<Triple> = tx.inserted().chain(tx.deleted()).map(quad_triple).collect();
    let retypes = |q, deleted| may_retype(tx, quad_triple(q), deleted);
    let probed = triples
        .iter()
        .all(|&t| !vocabulary_predicate(tx, t[1]) && !reserved_named(tx, t) && !header(tx, t[0]))
        && !tx.inserted().any(|q| retypes(q, false))
        && !tx.deleted().any(|q| retypes(q, true))
        && !changes_reified(tx);
    probed.then_some(triples)
}

/// The delta's carry-over as far as U1 tells it, which reads U1 alone: no equality classes
/// (one with them is evaluated afresh: `Upper`'s module docs), and each triple only asserts
/// something about individuals over U1's signature ([`Upper::is_assertion`], which no schema
/// change passes: its sets and its term ids' kind bits).
fn upper_keeps(upper: &Upper, triples: &[Triple]) -> bool {
    upper.classes().is_empty() && triples.iter().all(|&t| upper.is_assertion(t))
}

/// Whether a changed triple of `tx`, inserted or deleted, is the main triple a node names
/// (`owl:annotatedSource`, `owl:annotatedProperty`, `owl:annotatedTarget`): an annotated
/// axiom's reification or an annotation's annotation (*Mapping to RDF* Table 17). Deleting it
/// leaves the node annotating nothing, which the reader reports (`Diagnostic::Malformed`);
/// inserting it gives a node that annotated nothing its triple back: either way what the
/// reader left out changes. Any such node counts, whatever its type: one the reader doesn't
/// take for a reification only costs a read.
///
/// The base's nodes: an edit of a node's role statements uses OWL's vocabulary as predicate,
/// which both carry-overs refuse beside this ([`changes_schema`], [`vocabulary_predicate`]),
/// so where this answer counts, the roles are the base's. Its cost: one probe of the base's
/// indexes per changed triple, and two per node it finds, at most [`REIFIED_CANDIDATES`] of
/// them; more is told as a reified triple, so the carry-over reads again; none of the
/// pending changes.
fn changes_reified(tx: &impl PendingView) -> bool {
    let base = tx.base();
    let id = |local: &str| {
        let iri = format!("http://www.w3.org/2002/07/owl#{local}");
        base.lookup(nrese_rdf::NamedNodeRef::new_unchecked(&iri).into())
    };
    let (Some(source), Some(property), Some(target)) = (
        id("annotatedSource"),
        id("annotatedProperty"),
        id("annotatedTarget"),
    ) else {
        return false;
    };
    // One past the bound: more than it are told by their count alone.
    let nodes = |s: u64| -> Vec<TermId> {
        let mut pattern = nrese_engine::QuadPattern::all();
        (pattern.predicate, pattern.object) = (Some(source), Some(TermId::from_raw(s)));
        let found = base.quads_for_pattern_in(ReadModel::Asserted, &pattern);
        found
            .map(|q| q.subject)
            .take(REIFIED_CANDIDATES + 1)
            .collect()
    };
    let holds = |node: TermId, role: TermId, term: u64| {
        let mut pattern = nrese_engine::QuadPattern::all();
        (pattern.subject, pattern.predicate, pattern.object) =
            (Some(node), Some(role), Some(TermId::from_raw(term)));
        let mut found = base.quads_for_pattern_in(ReadModel::Asserted, &pattern);
        found.next().is_some()
    };
    let mut changed = tx.inserted().chain(tx.deleted()).map(quad_triple);
    changed.any(|[s, p, o]| {
        let nodes = nodes(s);
        nodes.len() > REIFIED_CANDIDATES
            || nodes
                .iter()
                .any(|&node| holds(node, property, p) && holds(node, target, o))
    })
}

/// How many nodes naming one subject as their `owl:annotatedSource` [`changes_reified`] checks
/// per changed triple of it: so many annotated statements of one subject are rare, and past
/// them a commit reads the ontology, as any other it can't carry over, once.
const REIFIED_CANDIDATES: usize = 32;

impl Bounds {
    /// Whether what the reader left out at `tx`'s base, as the base's commit recorded it, is
    /// what it leaves out of the data as `tx`'s changes leave it: where their commit would
    /// take its record without reading the ontology (the commit's record: the RL route kept,
    /// or the delta executor's change, [`assertions_only`]), and every change is on the short
    /// list whose effect on the reader the base's declarations fix ([`listed`]). `false`
    /// where the bounds describe another revision than the base's. The list axioms the
    /// closure leaves out are no part of the record.
    ///
    /// Taken as the commit takes it ([`Self::keeps_route`]): no bounds guard is held across
    /// the engine or the dictionary (`docs/architecture/contracts.md`). Under the state only
    /// the revision check, the RL route's kinds (an `Arc`, cloned), and U1's part
    /// ([`upper_keeps`], which reads U1 alone, never keeping it: a commit installs its change
    /// in place); every probe of the transaction runs without it.
    pub(crate) fn keeps_record(&self, tx: &impl PendingView) -> bool {
        let base = tx.base().revision();
        let kinds = lock(&self.state)
            .as_ref()
            .filter(|s| s.revision == base)
            .map(|s| s.kinds.clone());
        match kinds {
            Some(Some(kinds)) => route_kept(&kinds, tx, false) && listed(tx),
            Some(None) => {
                let Some(triples) = probed_assertions(tx).filter(|_| listed(tx)) else {
                    return false;
                };
                let state = lock(&self.state);
                matches!(
                    state.as_ref().filter(|s| s.revision == base),
                    Some(State { kinds: None, upper: Ok(upper), .. })
                        if upper_keeps(upper, &triples)
                )
            }
            None => false,
        }
    }
}

/// Whether every change of `tx`, inserted or deleted, is on the short list whose effect on
/// what the reader reads the base's declarations fix, so a read inside the transaction may
/// take its base's clean record ([`Bounds::keeps_record`], beside the commit's own tests):
/// `s a C` with `C` declared `owl:Class`, `s p o` with `p` declared `owl:ObjectProperty`
/// and `o` an IRI, `s p "v"` with `p` declared `owl:DatatypeProperty`; each declared so and
/// as no other kind by the reader's own table (`nrese_owl::DECLARATION_KINDS`: `rdfs:Class`
/// is a class, `owl:NamedIndividual` an individual), its subject (and its object, an IRI)
/// named and outside RDF's, RDFS's, OWL's, XSD's and SHACL's namespaces, no reserved or SHACL
/// name ([`reserved_named`]), not the ontology's node ([`header`]) and not a reification's
/// node (no `owl:annotatedSource`). The commit's tests already refuse a reified triple
/// ([`changes_reified`]).
///
/// Why a list: the commit's tests prove "the reader reads the data as before" from guards
/// over the syntax, for any change the RL route or the delta takes, and each review of them
/// found another change that alters what the reader reports. A change on this list alters
/// nothing the reader reads but an individual's membership or value under a name whose
/// kind the base declares; any other reads its own ontology, the cost before the record
/// was taken at all, never another record.
fn listed(tx: &impl PendingView) -> bool {
    use nrese_owl::EntityKind;
    let base = tx.base();
    let id = |iri: &str| base.lookup(nrese_rdf::NamedNodeRef::new_unchecked(iri).into());
    let owl = |local: &str| id(&format!("http://www.w3.org/2002/07/owl#{local}"));
    let Some(rdf_type) = id(RDF_TYPE) else {
        return false;
    };
    let source = owl("annotatedSource");
    // The reader's declaring classes, by id, each with the kind it declares.
    let declaring: Vec<(TermId, EntityKind)> = (nrese_owl::DECLARATION_KINDS.iter())
        .filter_map(|&(iri, kind)| id(iri).map(|t| (t, kind)))
        .collect();
    let has = |s: u64, p: TermId, o: Option<TermId>| {
        let mut pattern = nrese_engine::QuadPattern::all();
        (pattern.subject, pattern.predicate, pattern.object) =
            (Some(TermId::from_raw(s)), Some(p), o);
        let mut found = base.quads_for_pattern_in(ReadModel::Asserted, &pattern);
        found.next().is_some()
    };
    // Whether `t` is declared `kind` by `admitting` and by no class of another kind.
    let only = |t: u64, admitting: Option<TermId>, kind: EntityKind| {
        admitting.is_some_and(|a| has(t, rdf_type, Some(a)))
            && (declaring.iter()).all(|&(d, k)| k == kind || !has(t, rdf_type, Some(d)))
    };
    let named = |t: u64| {
        TermId::from_raw(t).kind() == nrese_engine::TermKind::Iri
            && !iri(tx, t).is_some_and(|i| vocabulary(&i) || own_namespace(&i))
    };
    let plain = |t: u64| !header(tx, t) && !source.is_some_and(|source| has(t, source, None));
    let (class, object, data) = (owl("Class"), owl("ObjectProperty"), owl("DatatypeProperty"));
    let mut changed = tx.inserted().chain(tx.deleted()).map(quad_triple);
    changed.all(|triple @ [s, p, o]| {
        let iri_object = TermId::from_raw(o).kind() == nrese_engine::TermKind::Iri;
        let kind = || {
            if TermId::from_raw(p) == rdf_type {
                named(o) && only(o, class, EntityKind::Class)
            } else if iri_object {
                named(o) && only(p, object, EntityKind::ObjectProperty)
            } else {
                TermId::from_raw(o).kind().is_literal() && only(p, data, EntityKind::DataProperty)
            }
        };
        named(s) && kind() && plain(s) && (!iri_object || plain(o)) && !reserved_named(tx, triple)
    })
}

/// Whether `iri` is in XSD's or SHACL's namespace (beside [`vocabulary`]'s three).
fn own_namespace(iri: &str) -> bool {
    iri.starts_with("http://www.w3.org/2001/XMLSchema#")
        || iri.starts_with("http://www.w3.org/ns/shacl#")
}

impl std::fmt::Debug for Bounds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bounds").finish_non_exhaustive()
    }
}

/// What a query reads of the bounds at a revision.
pub(crate) struct View {
    pub revision: u64,
    /// L: the inferred stack with the memberships the TBox's taxonomy adds
    /// ([`super::lower`]); the snapshot itself where it adds none.
    pub lower: Snapshot,
    /// Those memberships.
    pub lower_facts: usize,
    /// L ∪ U1 as a snapshot (`None`: U1 isn't available).
    pub upper: Option<Snapshot>,
    /// Why U1 can't bound the answers (not built, gave up, or axioms it doesn't cover).
    pub unavailable: Option<String>,
    /// Classes U1 has memberships in beyond L, and the other predicates it has facts on
    /// beyond L (Skolem constants included): the gap's signature, by member of U1's
    /// `owl:sameAs` classes, as a query names them ([`parts`]).
    pub gap_classes: HashSet<u64>,
    pub gap_predicates: HashSet<u64>,
    /// Those of them with a fact beyond L that names no Skolem constant (a membership of
    /// a named individual; a property value between two): the others' facts beyond L
    /// all have a Skolem constant, which is never an answer.
    pub named_gap_classes: HashSet<u64>,
    pub named_gap_predicates: HashSet<u64>,
    /// U1's own terms among its facts: never answers.
    pub internal: HashSet<u64>,
    /// U1's facts beyond L.
    pub facts: usize,
    /// U1 has no clash and checks every `⊥`: the data is consistent.
    pub proves_consistency: bool,
    /// The ontology is in OWL 2 RL: L is complete for every predicate, and the RL rules
    /// decide its consistency, per snapshot ([`rules_prove`]): never kept in the view, which
    /// a rebuild at its revision may outlive.
    pub rules: bool,
}

fn triple(q: EncodedTriple) -> Triple {
    [q.subject.raw(), q.predicate.raw(), q.object.raw()]
}

pub(crate) fn quad_triple(q: EncodedQuad) -> Triple {
    [q.subject.raw(), q.predicate.raw(), q.object.raw()]
}

/// What a commit prepared for U1, applied by [`install`] once the commit is done.
pub(crate) enum Prepared {
    /// The delta executor's change to U1.
    Delta(Box<(Change, Published)>),
    /// U1 compiled and evaluated afresh (the schema changed, or U1 described another
    /// revision), or why that gave up.
    Rebuilt(Box<Result<Upper, GaveUp>>),
    /// U1 had given up, and nothing it was compiled from changed.
    Unavailable,
    /// The ontology is (still) in OWL 2 RL: the RL rules decide it, no U1.
    Rules(Arc<Kinds>),
}

/// A commit's preparation: what to install, and what U1 after the commit says about
/// consistency.
pub(crate) struct Preparation {
    base: u64,
    // A full read with no finding replaces an earlier note; no read keeps it.
    anonymous: Option<Option<String>>,
    pub prepared: Prepared,
    /// U1 after the commit proves the data consistent (or, on the RL route, the RL
    /// rules do: [`Preparation::decided_by`]; only where they assessed the base's own
    /// revision clean, [`rules_prove`], since otherwise their gate checks the commit's own
    /// facts only).
    pub proves_consistency: bool,
    /// `upper-bound`, or `rules` on the RL route.
    pub decided_by: &'static str,
    /// The ontology the commit leaves, where the preparation had to read it (for the
    /// consistency check, which then needn't read it again).
    pub ontology: Option<Ontology>,
}

impl Preparation {
    pub(crate) fn record_gate_anonymous(&mut self) {
        if self.anonymous.is_none()
            && let Some(ontology) = &self.ontology
        {
            self.anonymous = Some(anonymous_report(ontology));
        }
    }

    /// The read evidence travels with the engine publication, ahead of writer maintenance.
    pub(crate) fn published(&self, store: &StoreService, base: &Snapshot) -> Published {
        let bounds = &store.dl().bounds;
        match &self.prepared {
            Prepared::Delta(change) => change.1.clone(),
            Prepared::Rebuilt(upper) => Published::new(
                bounds,
                upper
                    .as_ref()
                    .as_ref()
                    .map(|u| u.version.clone())
                    .map_err(Clone::clone),
                "rebuilt",
            ),
            Prepared::Unavailable => {
                let mut base = ReadBasis::upper(base)
                    .expect("the unavailable bound belongs to the base publication");
                base.last = "kept";
                base
            }
            Prepared::Rules(_) => Published::new(bounds, Err(not_compiled()), "rules"),
        }
    }
}

/// Prepares U1 for the commit `tx` (after the RL reasoning applied its changes): the
/// change by the delta executor where only assertions over U1's signature changed, else
/// U1 afresh. Cost: the change's, or the closure's on a rebuild. `lists_left_out`: whether
/// the closure the commit leaves leaves out a list axiom (then never the RL route).
pub(crate) fn prepare(
    store: &StoreService,
    tx: &Transaction<'_>,
    stop: Stop<'_>,
    lists_left_out: bool,
) -> Preparation {
    let base = tx.base().revision();
    let pending = tx.pending_snapshot();
    let deadline = Deadline::after(store.config().dl.timeout);
    let memory = store.memory_stop();
    // Whether the memory watch stopped U1: then it is refused, never kept (REAS-3, C17).
    let memory_hit = std::sync::atomic::AtomicBool::new(false);
    let hit = &memory_hit;
    let stop = move || {
        stop()
            || deadline.passed()
            || (memory.exceeded() && {
                hit.store(true, std::sync::atomic::Ordering::Relaxed);
                true
            })
    };
    // U1 at the base, taken under the state and changed without it (U1-I): readers never
    // wait for a commit's (parallel) change.
    let compiled = ReadBasis::upper(tx.base()).map(|u| u.compiled);
    let current = lock(&store.dl().bounds.state)
        .as_ref()
        .filter(|s| s.revision == base && compiled.is_some_and(|id| s.compiled == id))
        .map(|s| (s.kinds.clone(), s.upper.clone()));
    // The commit's asserted changes, read from the transaction each time: no copy of them.
    let asserted = || tx.inserted().chain(tx.deleted()).map(quad_triple);
    let changed = || {
        asserted()
            .chain(tx.inferred_inserted().map(triple))
            .chain(tx.inferred_deleted().map(triple))
    };
    // On the RL route, a commit that gives no term a new kind leaves what the reader reads,
    // and so the route, as they were; any other is read again below.
    if let Some((Some(kinds), _)) = &current
        && route_kept(kinds, tx, lists_left_out)
    {
        return Preparation {
            base,
            prepared: Prepared::Rules(Arc::clone(kinds)),
            proves_consistency: rules_prove(tx.base()),
            decided_by: "rules",
            anonymous: None,
            ontology: None,
        };
    }
    // Off the route only: on it there is no U1, and "nothing U1 is compiled from changed"
    // would keep a route the commit may change.
    let reading = pending.identity().equality;
    let stable_equality = tx.base().identity().equality == reading
        && reading.is_none_or(|id| !changed().any(|[s, p, o]| p == id.raw() && s != o));
    if let Some((None, upper)) = current {
        match upper.as_deref() {
            Ok(upper) if stable_equality && assertions_only(upper, tx) => {
                let before = tx.base();
                let inserted: Vec<Triple> = tx
                    .inserted()
                    .map(quad_triple)
                    .chain(tx.inferred_inserted().map(triple))
                    .filter(|t| !in_view(before, *t))
                    .collect();
                let deleted: Vec<Triple> = tx
                    .deleted()
                    .map(quad_triple)
                    .chain(tx.inferred_deleted().map(triple))
                    .collect();
                #[cfg(test)]
                seam::at(&store.dl().bounds, seam::Point::Change);
                let change =
                    upper
                        .change(&pending, &inserted, &deleted, &stop)
                        .and_then(|change| {
                            if change.merges() {
                                return Ok(None);
                            }
                            let compacted = store.dl().bounds.compaction.take(base);
                            upper
                                .stage(
                                    change,
                                    changed(),
                                    &pending,
                                    store.dl().bounds.kept(),
                                    compacted,
                                    #[cfg(test)]
                                    &store.dl().bounds,
                                )
                                .map(Some)
                        });
                match change {
                    Ok(Some((version, change))) => {
                        let proves_consistency = version.proves_consistency();
                        let mut published = ReadBasis::upper(before)
                            .expect("the delta advances the base publication's upper bound");
                        published.upper = Ok(version);
                        published.last = "delta";
                        return Preparation {
                            base,
                            decided_by: "upper-bound",
                            proves_consistency,
                            prepared: Prepared::Delta(Box::new((change, published))),
                            anonymous: None,
                            ontology: None,
                        };
                    }
                    // The commit equates terms: U1 afresh, by representatives (below).
                    Ok(None) => {}
                    Err(gave_up) => {
                        return Preparation {
                            base,
                            prepared: Prepared::Rebuilt(Box::new(Err(
                                gave_up.by_memory(&memory_hit)
                            ))),
                            decided_by: "upper-bound",
                            proves_consistency: false,
                            anonymous: None,
                            ontology: None,
                        };
                    }
                }
            }
            // A change naming a reserved name or a SHACL property may change what the reader
            // reads (`reserved_named`): U1 is built again.
            Err(_) if !asserted().any(|t| changes_schema(tx, t) || reserved_named(tx, t)) => {
                return Preparation {
                    base,
                    prepared: Prepared::Unavailable,
                    decided_by: "upper-bound",
                    proves_consistency: false,
                    anonymous: None,
                    ontology: None,
                };
            }
            _ => {}
        }
    }
    source::intern_vocabulary(tx);
    #[cfg(test)]
    seam::at(&store.dl().bounds, seam::Point::Read);
    let treatments = &store.config().dl.treatments;
    let Ok(ontology) = source::read_pending(tx, Arc::clone(store.dl().bounds.kept()), treatments)
    else {
        return Preparation {
            base,
            prepared: Prepared::Rebuilt(Box::new(Err(GaveUp::refused(source::refused(
                "the commit",
            ))))),
            decided_by: "upper-bound",
            proves_consistency: false,
            anonymous: None,
            ontology: None,
        };
    };
    let anonymous = Some(anonymous_report(&ontology));
    if routed_to_rules(&ontology) && !lists_left_out {
        // The RL gate that ran before has decided the commit's consistency.
        let lookup = |iri: &str| tx.lookup(nrese_rdf::NamedNodeRef::new_unchecked(iri).into());
        return Preparation {
            base,
            prepared: Prepared::Rules(Kinds::of(&ontology, &lookup)),
            proves_consistency: rules_prove(tx.base()),
            decided_by: "rules",
            anonymous,
            ontology: None,
        };
    }
    let normalised = nrese_owl::normalise(&ontology);
    let upper = Upper::build(
        &ontology,
        &normalised,
        &pending,
        &|t| Some(tx.intern(t)),
        &stop,
        store.dl().bounds.kept(),
    )
    .map_err(|gave_up| gave_up.by_memory(&memory_hit));
    Preparation {
        base,
        decided_by: "upper-bound",
        proves_consistency: upper.as_ref().is_ok_and(|u| u.proves_consistency()),
        prepared: Prepared::Rebuilt(Box::new(upper)),
        anonymous,
        ontology: Some(ontology),
    }
}

/// Whether a statement's subject or object is a name the reader leaves out in its place, or
/// it names a SHACL property (`nrese_owl::shacl_properties`) anywhere, which can change
/// whether the reader reads SHACL content as content or as any other statement
/// (`Ontology::shacl`). The object of `rdf:type` is a class's place
/// (`nrese_owl::diagnostics::reserved_name`); the subject, and any other object, is taken as
/// an individual's (`reserved_as_individual`, which also refuses `owl:Thing` and the five
/// other names OWL 2 gives a class or a property). Where the reader reads such a statement
/// otherwise (an annotation's subject, OWL's vocabulary) that only reads again.
fn reserved_named(tx: &impl PendingView, [s, p, o]: Triple) -> bool {
    use nrese_owl::diagnostics::{reserved_as_individual, reserved_name};
    let object: fn(&str) -> bool = if iri(tx, p).is_some_and(|p| p == RDF_TYPE) {
        reserved_name
    } else {
        reserved_as_individual
    };
    named(tx, s, Some(reserved_as_individual)) || named(tx, o, Some(object)) || named(tx, p, None)
}

/// Whether the term `t` is a SHACL property (`nrese_owl::shacl_properties`) or one `reserved`
/// leaves out.
fn named(tx: &impl PendingView, t: u64, reserved: Option<fn(&str) -> bool>) -> bool {
    match tx.decode(TermId::from_raw(t)) {
        Some(nrese_rdf::Term::NamedNode(n)) => {
            let iri = n.as_str();
            nrese_owl::is_shacl_property(iri) || reserved.is_some_and(|reserved| reserved(iri))
        }
        _ => false,
    }
}

/// Whether deleting (`deleted`) or inserting a statement may change its property's kind for
/// the reader, which types an undeclared property by its uses, every statement of it counted
/// (`nrese_owl::mapping`, `is_data_property`): a data property while it has a literal value
/// and no other, an object property otherwise. So a deleted literal value of a property not
/// declared a data property may be its last; and a value that is no literal, of a property
/// not declared an object property, deleted may be its last other one, inserted its first,
/// where the property has a literal value too (one the reader reads as no assertion, about
/// the ontology's node for one: [`Kinds::keep`] and [`Upper::is_assertion`] refuse a
/// property asserted with both). That literal is looked for among at most [`RETYPE_VALUES`]
/// of its values in the base; past them, it may. An inserted literal makes a data property
/// only of one with no value, which neither carry-over knows as one. Declared in the base:
/// deleting a declaration is OWL's vocabulary, which both carry-overs refuse.
/// `rdf:type` and `owl:sameAs` type no property.
pub(crate) fn may_retype(tx: &impl PendingView, [_, p, o]: Triple, deleted: bool) -> bool {
    let id = |iri: &str| tx.lookup(nrese_rdf::NamedNodeRef::new_unchecked(iri).into());
    let (rdf_type, same_as) = (id(RDF_TYPE), id(OWL_SAME_AS));
    let property = Some(TermId::from_raw(p));
    if property == rdf_type || property == same_as {
        return false;
    }
    let literal = TermId::from_raw(o).kind().is_literal();
    if literal && !deleted {
        return false;
    }
    let kind = if literal {
        "http://www.w3.org/2002/07/owl#DatatypeProperty"
    } else {
        "http://www.w3.org/2002/07/owl#ObjectProperty"
    };
    let base = tx.base();
    let mut pattern = nrese_engine::QuadPattern::all();
    (pattern.subject, pattern.predicate, pattern.object) = (property, rdf_type, id(kind));
    let declared = pattern.predicate.is_some()
        && pattern.object.is_some()
        && (base.quads_for_pattern_in(ReadModel::Asserted, &pattern))
            .next()
            .is_some();
    if declared || literal {
        return !declared;
    }
    let mut pattern = nrese_engine::QuadPattern::all();
    pattern.predicate = property;
    let values: Vec<bool> = (base.quads_for_pattern_in(ReadModel::Asserted, &pattern))
        .map(|q| q.object.kind().is_literal())
        .take(RETYPE_VALUES + 1)
        .collect();
    values.len() > RETYPE_VALUES || values.contains(&true)
}

/// How many of an undeclared property's values [`may_retype`] looks through for a literal
/// one, per changed value: past them a commit reads the ontology, as any other it can't
/// carry over.
const RETYPE_VALUES: usize = 32;

/// Whether a changed statement may change the ontology's schema (and so U1's rules),
/// judged without U1's signature: a blank node, OWL's, RDF's or RDFS's vocabulary as
/// predicate (but `rdf:type` with another class, and `owl:sameAs`).
pub(crate) fn changes_schema(tx: &impl PendingView, [s, p, o]: Triple) -> bool {
    use nrese_engine::TermKind;
    let kind = |t: u64| TermId::from_raw(t).kind();
    if kind(s) == TermKind::BlankNode || kind(o) == TermKind::BlankNode {
        return true;
    }
    match iri(tx, p).as_deref() {
        Some(RDF_TYPE) => iri(tx, o).is_some_and(|o| vocabulary(&o)),
        _ => vocabulary_predicate(tx, p),
    }
}

/// Whether `p` is OWL's, RDF's or RDFS's vocabulary as a predicate, but `rdf:type` and
/// `owl:sameAs`: a statement of it is no assertion about individuals (the role statements
/// of a reification among them), whatever the reader made of its terms elsewhere.
fn vocabulary_predicate(tx: &impl PendingView, p: u64) -> bool {
    match iri(tx, p).as_deref() {
        Some(RDF_TYPE | OWL_SAME_AS) | None => false,
        Some(p) => vocabulary(p),
    }
}

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";

/// Whether `iri` is in OWL's, RDF's or RDFS's namespace.
fn vocabulary(iri: &str) -> bool {
    iri.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
        || iri.starts_with("http://www.w3.org/2000/01/rdf-schema#")
        || iri.starts_with("http://www.w3.org/2002/07/owl#")
}

/// The IRI the term `t` is, where it is one.
fn iri(tx: &impl PendingView, t: u64) -> Option<String> {
    match tx.decode(TermId::from_raw(t)) {
        Some(nrese_rdf::Term::NamedNode(n)) => Some(n.into_string()),
        _ => None,
    }
}

/// A gap signature by representative as one by member: every member of each
/// representative's class, U1's own terms (never a query's) left out.
fn by_member(
    representatives: HashSet<u64>,
    classes: &nrese_reasoner::representatives::EqualityClasses,
    internal: impl Fn(u64) -> bool,
) -> HashSet<u64> {
    representatives
        .into_iter()
        .flat_map(|r| {
            let class = classes.class_of(r);
            class
                .unwrap_or(&[])
                .iter()
                .copied()
                .chain(class.is_none().then_some(r))
        })
        .filter(|&t| !internal(t))
        .collect()
}

/// Whether L (`lower`, sorted) holds `fact`, one of U1's facts over representatives, for
/// every identity it stands for: the expanded reads give it each member of `classes`'
/// classes. Only then is it certain and no gap; L holding it for the representative alone
/// leaves the other members' facts to U1. Stops at the first member L lacks; a fact L holds
/// whole costs one search per expanded fact, each of them in L.
fn held_by_lower(
    lower: &[Triple],
    classes: &nrese_reasoner::representatives::EqualityClasses,
    [s, p, o]: Triple,
) -> bool {
    let (ms, mp, mo) = (
        classes.class_of(s).unwrap_or(std::slice::from_ref(&s)),
        classes.class_of(p).unwrap_or(std::slice::from_ref(&p)),
        classes.class_of(o).unwrap_or(std::slice::from_ref(&o)),
    );
    ms.iter().all(|&s| {
        mp.iter()
            .all(|&p| mo.iter().all(|&o| lower.binary_search(&[s, p, o]).is_ok()))
    })
}

pub(super) fn in_view(view: &Snapshot, [s, p, o]: Triple) -> bool {
    #[cfg(test)]
    seam::snapshot_probe();
    view.quads_for_pattern_in(
        ReadModel::Materialised,
        &nrese_engine::QuadPattern {
            subject: Some(TermId::from_raw(s)),
            predicate: Some(TermId::from_raw(p)),
            object: Some(TermId::from_raw(o)),
            graph: nrese_engine::GraphSelector::Any,
        },
    )
    .next()
    .is_some()
}

/// Installs a commit's preparation for its new `revision`.
pub(crate) fn install(
    store: &StoreService,
    preparation: Preparation,
    compiled: u64,
    revision: u64,
) {
    let bounds = &store.dl().bounds;
    if revision == preparation.base {
        return;
    }
    let mut state = lock(&bounds.state);
    state.record_anonymous(revision, preparation.anonymous);
    match preparation.prepared {
        // Refused for memory: not kept, so a read of the revision builds it again (SYN-61);
        // the generation it would have replaced is superseded, and goes with its charge.
        Prepared::Rebuilt(upper) if upper.as_ref().as_ref().is_err_and(|g| g.refused) => {
            let superseded = state.evict_below(revision);
            drop(state);
            drop(superseded);
            bounds.compaction.publish(revision, None);
            return;
        }
        Prepared::Rebuilt(upper) => publish(
            &mut state,
            State {
                revision,
                upper: upper.map(Arc::new),
                last: "rebuilt",
                compiled,
                kinds: None,
            },
        ),
        Prepared::Rules(kinds) => publish(
            &mut state,
            State {
                revision,
                upper: Err(not_compiled()),
                last: "rules",
                compiled,
                kinds: Some(kinds),
            },
        ),
        Prepared::Delta(change) => match state.get_mut() {
            Some(s) if s.revision == preparation.base => {
                if let Ok(upper) = &mut s.upper {
                    let Some(upper) = Arc::get_mut(upper) else {
                        debug_assert!(false, "U1 shared at its commit's install");
                        drop(state.clear(revision));
                        return;
                    };
                    let (change, published) = *change;
                    upper.apply(change, published.upper.expect("the staged upper bound"));
                }
                s.revision = revision;
                s.last = "delta";
                s.compiled = compiled;
            }
            // A query built U1 for the new revision (or a later one) meanwhile: it stands.
            Some(s) if s.revision >= revision => {}
            // Its base let go (or never built): the next read builds the revision afresh.
            _ => drop(state.clear(revision)),
        },
        Prepared::Unavailable => {
            if let Some(s) = state.get_mut()
                && s.revision == preparation.base
            {
                s.revision = revision;
                s.last = "kept";
            }
        }
    }
    if let Some(s) = state.as_ref().filter(|s| s.revision == revision) {
        bounds
            .compaction
            .publish(revision, s.upper.as_deref().ok().map(|u| &u.version));
    }
}

/// The read view for one pinned snapshot of the store. A failed build may retry the same
/// snapshot; a concurrent publication cannot change which version is answered.
pub(crate) fn view(store: &StoreService) -> (Snapshot, Arc<View>) {
    let snapshot = store.engine().snapshot();
    loop {
        if let Some(Ok(view)) = view_until(store, &snapshot, None, None) {
            return (snapshot, view);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Missing {
    /// The build ended with no view (a panic or an early return).
    Failed,
}

/// The pinned snapshot's view, or `None` if this caller stopped waiting for it. A build
/// claimed here finishes even if the caller's request expires, since others can join it.
pub(crate) fn view_until(
    store: &StoreService,
    snapshot: &Snapshot,
    deadline: Option<std::time::Instant>,
    stop: Option<&dyn Fn() -> bool>,
) -> Option<Result<Arc<View>, Missing>> {
    let bounds = &store.dl().bounds;
    let key = (
        snapshot.identity(),
        ReadBasis::of(snapshot).map(|basis| basis.id),
    );
    let cached = || {
        lock(&bounds.view)
            .as_ref()
            .filter(|(at, _)| *at == key)
            .map(|(_, view)| Arc::clone(view))
    };
    if let Some(view) = cached() {
        return Some(Ok(view));
    }
    let stopped = || stop.is_some_and(|stop| stop());
    if deadline.is_some_and(|at| std::time::Instant::now() >= at) || stopped() {
        return None;
    }
    let built = match bounds.views.claim(key, Kind::Opaque, || cached().map(Some)) {
        Claim::Build(builder) => {
            let (view, kept) = read(store, snapshot, key);
            builder.end(match kept {
                true => End::Kept(Arc::clone(&view)),
                false => End::Unkept(Some(Arc::clone(&view))),
            });
            Ok(view)
        }
        Claim::Wait(flight) => match flight.join(deadline, &stopped) {
            Joined::Told(Ended::End(End::Kept(view) | End::Unkept(Some(view)))) => Ok(view),
            Joined::Told(
                Ended::End(End::Empty | End::Later | End::Unkept(None)) | Ended::Abandoned,
            ) => Err(Missing::Failed),
            Joined::Busy => Ok(read(store, snapshot, key).0),
            Joined::Left => return None,
        },
    };
    Some(built)
}

fn published_for_read(store: &StoreService, snapshot: &Snapshot) -> Published {
    if let Some(published) = ReadBasis::upper(snapshot).filter(|u| !u.refused()) {
        return published;
    }
    let bounds = &store.dl().bounds;
    let revision = snapshot.revision();
    let current = || snapshot.same_version(&store.engine().snapshot());
    if current() {
        let superseded = lock(&bounds.state).evict_below(revision);
        drop(superseded);
        store.dl().query_ontology.evict_below(revision);
    }
    let (upper, kinds, anonymous) = afresh(store, snapshot);
    let last = if kinds.is_some() { "rules" } else { "read" };
    let published = Published::new(
        bounds,
        upper
            .as_ref()
            .map(|u| u.version.clone())
            .map_err(Clone::clone),
        last,
    );
    if !upper.as_ref().is_err_and(|g| g.refused) {
        let recorded = ReadBasis::record_upper(snapshot, published.clone());
        if recorded.compiled != published.compiled {
            return recorded;
        }
        // Retained and superseded readers can finish without replacing the writer's root.
        if current() {
            let mut slot = lock(&bounds.state);
            slot.record_anonymous(revision, anonymous);
            publish(
                &mut slot,
                State {
                    revision,
                    upper: upper.map(Arc::new),
                    last,
                    compiled: published.compiled,
                    kinds,
                },
            );
        }
    }
    published
}

/// Build only from the evidence of the pinned publication. No writer state is consulted.
fn read(store: &StoreService, snapshot: &Snapshot, key: ReadKey) -> (Arc<View>, bool) {
    let bounds = &store.dl().bounds;
    let revision = snapshot.revision();
    let stale = {
        let mut cached = lock(&bounds.view);
        if cached
            .as_ref()
            .is_some_and(|(at, _)| at.0.revision < revision)
        {
            cached.take()
        } else {
            None
        }
    };
    drop(stale);
    let published = published_for_read(store, snapshot);
    #[cfg(test)]
    seam::at(bounds, seam::Point::Eligible);
    let kept = bounds.kept();
    let mut keep = !published.refused();
    let rules = published.last == "rules";
    let lower = match &published.upper {
        Ok(upper) => {
            let taxonomy = taxonomy(store, snapshot, &published);
            keep &= taxonomy.is_ok();
            super::lower::memberships(snapshot, of(&taxonomy), upper.program.names.rdf_type, kept)
                .ok()
        }
        _ => Some(ChargedVec::new(Arc::clone(kept))),
    };
    let Some(parts) = lower.and_then(|lower| {
        parts(
            snapshot,
            revision,
            published.upper.as_ref(),
            rules,
            &lower,
            kept,
        )
    }) else {
        return (Arc::new(refused(snapshot, revision)), false);
    };
    #[cfg(test)]
    seam::at(bounds, seam::Point::View);
    let (view, charged) = assemble(snapshot, parts, kept);
    let view = Arc::new(view);
    if charged && keep && snapshot.same_version(&store.engine().snapshot()) {
        let mut cached = lock(&bounds.view);
        if cached
            .as_ref()
            .is_none_or(|(at, _)| at.0.revision <= revision)
        {
            *cached = Some((key, Arc::clone(&view)));
        }
    }
    (view, charged && keep)
}

/// The RL rules prove consistency only on a vouched DL version with a consistent assessment.
/// An ill-typed literal can still make the DL verdict unknown ([`holds_ill_typed`]).
pub(crate) fn rules_prove(snapshot: &Snapshot) -> bool {
    use crate::service::Certificate;
    Certificate::of(snapshot).is_some_and(|c| c.dl)
        && Certificate::assessment(snapshot, false).rules == crate::ConsistencyStatus::Consistent
}

/// Whether `snapshot`'s version holds an ill-typed literal, which OWL 2 doesn't admit; `None`
/// without a certificate, or where the search stopped (`Certificate::ill_typed`): undecided.
pub(crate) fn holds_ill_typed(store: &StoreService, snapshot: &Snapshot) -> Option<bool> {
    let memory = store.memory_stop();
    let certificate = crate::service::Certificate::of(snapshot)?;
    let found = certificate.ill_typed(snapshot, &|| memory.exceeded())?;
    Some(found.is_some())
}

/// The literal `tx` leaves, starting from its base's witness. The gate's stop can leave it undecided.
pub(crate) fn find_ill_typed_after(
    store: &StoreService,
    tx: &Transaction<'_>,
    stopped: &dyn Fn() -> bool,
) -> crate::service::IllTyped {
    let memory = store.memory_stop();
    let stop = || stopped() || memory.exceeded();
    let base = crate::service::Certificate::of(tx.base())?.ill_typed(tx.base(), &stop)?;
    super::query::ill_typed_after(tx, base, &stop)
}

fn anonymous_report(ontology: &Ontology) -> Option<String> {
    ontology
        .diagnostics
        .iter()
        .find(|d| {
            matches!(d, nrese_owl::Diagnostic::AnonymousIndividual { axioms, .. } if axioms.iter().any(Option::is_some))
        })
        .map(|d| d.describe(&super::consistency::nameless))
}

type Rebuilt = (
    Result<Upper, GaveUp>,
    Option<Arc<Kinds>>,
    Option<Option<String>>,
);

/// U1 compiled and evaluated afresh over `snapshot` (or, with the kinds it was decided on,
/// the RL route's place).
fn afresh(store: &StoreService, snapshot: &Snapshot) -> Rebuilt {
    let replica = store.dl().replica();
    // U1's names are system vocabulary: the bounds keep them for every later read, so they
    // take shared ids (dictionary lifecycle V1), never a request's private ones.
    let refused = |what| (Err(GaveUp::refused(source::refused(what))), None, None);
    if !replica && source::intern_system_vocabulary(store.engine()).is_err() {
        return refused("the vocabulary");
    }
    let latest = store.engine().snapshot();
    let kept = Arc::clone(store.dl().bounds.kept());
    let ontology = match source::read_snapshot(snapshot, kept, &store.config().dl.treatments) {
        Ok(ontology) => ontology,
        Err(_) => return refused("the revision"),
    };
    let anonymous = Some(anonymous_report(&ontology));
    ReadBasis::record_left_out(
        snapshot,
        super::query::left_out(&ontology, &super::consistency::nameless),
    );
    if routed_to_rules(&ontology) && !store.lists_left_out(snapshot) {
        let lookup =
            |iri: &str| snapshot.lookup(nrese_rdf::NamedNodeRef::new_unchecked(iri).into());
        return (
            Err(not_compiled()),
            Some(Kinds::of(&ontology, &lookup)),
            anonymous,
        );
    }
    let normalised = nrese_owl::normalise(&ontology);
    let deadline = Deadline::after(store.config().dl.timeout);
    #[cfg(test)]
    let deadline = if seam::at(&store.dl().bounds, seam::Point::BuildDeadline) {
        Deadline::at(std::time::Instant::now())
    } else {
        deadline
    };
    let memory = store.memory_stop();
    // Whether the memory watch stopped U1: then it is refused, never kept (REAS-3, C17).
    let memory_hit = std::sync::atomic::AtomicBool::new(false);
    let hit = &memory_hit;
    let stop = move || {
        deadline.passed()
            || (memory.exceeded() && {
                hit.store(true, std::sync::atomic::Ordering::Relaxed);
                true
            })
    };
    let engine = store.engine();
    let resolve = |t: nrese_rdf::TermRef<'_>| source::resolve_system(replica, engine, &latest, t);
    #[cfg(test)]
    seam::at(&store.dl().bounds, seam::Point::Build);
    let kept = store.dl().bounds.kept();
    let upper = Upper::build(&ontology, &normalised, snapshot, &resolve, &stop, kept)
        .map_err(|gave_up| gave_up.by_memory(&memory_hit));
    (upper, None, anonymous)
}

/// A taxonomy for a read: kept, with its charge, or (`Err`) one only this read uses, never
/// kept.
type ForRead = Result<KeptTaxonomy, Arc<Taxonomy>>;

/// The taxonomy a read reads, kept or not.
fn of(taxonomy: &ForRead) -> &Taxonomy {
    match taxonomy {
        Ok(kept) => kept.value(),
        Err(unkept) => unkept,
    }
}

/// A taxonomy kept for its compilation, charged to the bounds' budget (SYN-61).
type KeptTaxonomy = Arc<nrese_exec::cell::Kept<Taxonomy>>;

/// The TBox's taxonomy for U1's compilation `compiled` (`kept`: the bounds' own), built
/// once per compilation: a read off the pool waits for the build under way, one on a
/// Rayon worker builds its own (U1-B). Kept only charged to the bounds' budget: one whose
/// ontology couldn't be read, or whose charge that budget can't take, serves this read and is
/// never kept (SYN-61).
fn taxonomy(store: &StoreService, snapshot: &Snapshot, published: &Published) -> ForRead {
    if let Some(kept) = lock(&published.taxonomy).clone() {
        return Ok(kept);
    }
    let bounds = &store.dl().bounds;
    let build = || -> ForRead {
        #[cfg(test)]
        seam::at(bounds, seam::Point::Taxonomy);
        let Ok(ontology) = super::query::ontology_at(store, snapshot) else {
            return Err(refused_taxonomy(store));
        };
        let taxonomy = super::lower::tbox_taxonomy(store, ontology.value());
        let bytes = nrese_dl::memory::taxonomy_bytes(&taxonomy);
        let taxonomy = Arc::new(taxonomy);
        let budget = Arc::clone(bounds.kept());
        nrese_exec::cell::Kept::charge(Arc::clone(&taxonomy), bytes, budget)
            .map(Arc::new)
            .map_err(|_| taxonomy)
    };
    loop {
        match bounds
            .taxonomies
            .claim(published.compiled, Kind::Opaque, || {
                lock(&published.taxonomy).clone().map(Some)
            }) {
            Claim::Build(builder) => {
                let built = build();
                let Ok(charge) = built else {
                    // Given to nobody waiting: they read again.
                    builder.end(End::Unkept(None));
                    return built;
                };
                lock(&published.taxonomy).get_or_insert_with(|| Arc::clone(&charge));
                builder.end(End::Kept(Arc::clone(&charge)));
                return Ok(charge);
            }
            // The view build it runs in goes to its end: no deadline, no stop.
            Claim::Wait(flight) => match flight.join(None, &|| false) {
                // The taxonomy with its charge: kept while this read holds it.
                Joined::Told(Ended::End(End::Kept(charge) | End::Unkept(Some(charge)))) => {
                    return Ok(charge);
                }
                // The build failed, or kept nothing: again.
                Joined::Told(
                    Ended::End(End::Empty | End::Later | End::Unkept(None)) | Ended::Abandoned,
                ) => {}
                Joined::Busy | Joined::Left => return build(),
            },
        }
    }
}

/// The taxonomy of a read whose TBox couldn't be read: no subsumption (L adds nothing, which
/// is sound), its consistency unknown, incomplete.
fn refused_taxonomy(store: &StoreService) -> Arc<Taxonomy> {
    let mut empty = super::lower::tbox_taxonomy(store, &nrese_owl::Ontology::default());
    let why = source::refused("the TBox");
    empty.classification.consistency = nrese_dl::verdict::Verdict::Unknown(why.clone());
    empty.incomplete.push(why);
    Arc::new(empty)
}

/// What a view takes of U1 ([`parts`]), before its runs are built ([`assemble`]). Its lists
/// are scratch charged to the bounds' budget (SYN-61), freed once the runs are built.
struct Parts {
    /// The view, its snapshots still `snapshot` itself.
    view: View,
    /// L's memberships from the taxonomy.
    lower: ChargedVec<EncodedTriple>,
    upper: Option<UpperParts>,
}

/// What the upper view takes of U1.
struct UpperParts {
    /// L's memberships over representatives (`None`: `Parts::lower` as they are).
    over_representatives: Option<ChargedVec<EncodedTriple>>,
    /// U1's facts beyond them.
    facts: ChargedVec<EncodedTriple>,
    /// `owl:sameAs`, where U1 has classes.
    same_as: Option<u64>,
}

/// The view of `revision` where what it reads or keeps doesn't fit the queries' memory: U1
/// unavailable (answers sound only), not to be kept (SYN-61).
fn refused(snapshot: &Snapshot, revision: u64) -> View {
    View {
        revision,
        lower: snapshot.clone(),
        lower_facts: 0,
        upper: None,
        unavailable: Some(
            "the upper bound isn't available: its view doesn't fit the queries' memory".to_owned(),
        ),
        gap_classes: HashSet::new(),
        gap_predicates: HashSet::new(),
        named_gap_classes: HashSet::new(),
        named_gap_predicates: HashSet::new(),
        internal: HashSet::new(),
        facts: 0,
        proves_consistency: false,
        rules: false,
    }
}

/// The view's runs, from what it took of U1: Rayon work, with no lock held. What the view
/// keeps is charged to `kept` before it is made (SYN-61), and `true`; where `kept` can't take
/// it, the view of an unavailable U1 instead (sound only), and `false`: not to be kept.
fn assemble(snapshot: &Snapshot, parts: Parts, kept: &Arc<nrese_exec::Budget>) -> (View, bool) {
    let Parts {
        mut view,
        lower,
        upper,
    } = parts;
    let revision = view.revision;
    if !lower.is_empty() {
        // Derived from `snapshot` alone, so the view keeps its publication (dictionary 3.0b):
        // one of the method's two allowed callers (the workspace's clippy.toml).
        #[allow(clippy::disallowed_methods)]
        let derived = snapshot.with_inferred_derived_within(&lower, kept);
        match derived {
            Ok(lower) => view.lower = lower,
            Err(_) => return (refused(snapshot, revision), false),
        }
    }
    if let Some(UpperParts {
        over_representatives,
        facts,
        same_as,
    }) = upper
    {
        let mut triples = over_representatives.unwrap_or(lower);
        if triples.extend_from_slice(&facts).is_err() {
            return (refused(snapshot, revision), false);
        }
        drop(facts);
        // The other allowed caller.
        #[allow(clippy::disallowed_methods)]
        let Ok(upper_view) = snapshot.with_inferred_derived_within(&triples, kept) else {
            return (refused(snapshot, revision), false);
        };
        // Read expanded to every identity of U1's classes (the stack keeps representatives).
        view.upper = Some(match same_as {
            None => upper_view,
            Some(same_as) => upper_view.with_equality(Some(TermId::from_raw(same_as))),
        });
    }
    (view, true)
}

/// What a view of `revision` takes of U1 and of L's memberships `lower` (sorted, each once).
/// One serial pass over the published stack and eligible facts checks [`held_by_lower`],
/// then a pass over the gap signature's class members expands it (U1-I). Copies are charged
/// to `kept` as they grow (SYN-61). Returns `None` where they don't fit.
///
/// Each structure's form in subject, predicate and class object:
/// - `lower`, L's memberships beyond the RL closure: as derived, by member.
/// - U1's stack: by representative in all three, each other member stored as
///   `member owl:sameAs representative`. The upper view reads it expanded to every member
///   and looks a query's constants up by their representative (`with_equality`).
/// - The gap, U1's facts L doesn't hold for every member they stand for
///   ([`held_by_lower`]): by representative, as the stack.
/// - `gap_classes`, `gap_predicates` and their named halves: **by member**. The query
///   analysis looks them up with the query's own class and property (`analysis::closed`,
///   `closed_for_answers`), which may be any member, a class or property IRI an
///   individual too (punning). So each representative is closed over its class's members
///   here, once, U1's own terms left out ([`by_member`]).
/// - `named`: a representative is named where one of its members is.
/// - `internal`: by representative, as the stack; nothing looks it up.
fn parts(
    snapshot: &Snapshot,
    revision: u64,
    upper: Result<&UpperVersion, &GaveUp>,
    rules: bool,
    lower: &[Triple],
    kept: &Arc<nrese_exec::Budget>,
) -> Option<Parts> {
    let empty = || ChargedVec::new(Arc::clone(kept));
    if rules {
        let view = View {
            revision,
            lower: snapshot.clone(),
            lower_facts: 0,
            upper: None,
            unavailable: None,
            gap_classes: HashSet::new(),
            gap_predicates: HashSet::new(),
            named_gap_classes: HashSet::new(),
            named_gap_predicates: HashSet::new(),
            internal: HashSet::new(),
            facts: 0,
            proves_consistency: false,
            rules: true,
        };
        return Some(Parts {
            view,
            lower: empty(),
            upper: None,
        });
    }
    let upper = match upper {
        Ok(upper) => upper,
        Err(GaveUp { why, .. }) => {
            let view = View {
                revision,
                lower: snapshot.clone(),
                lower_facts: 0,
                upper: None,
                unavailable: Some(format!("the upper bound isn't available: {why}")),
                gap_classes: HashSet::new(),
                gap_predicates: HashSet::new(),
                named_gap_classes: HashSet::new(),
                named_gap_predicates: HashSet::new(),
                internal: HashSet::new(),
                facts: 0,
                proves_consistency: false,
                rules: false,
            };
            return Some(Parts {
                view,
                lower: empty(),
                upper: None,
            });
        }
    };
    let eligible = &upper.eligible;
    let rdf_type = upper.program.names.rdf_type;
    let clash = upper.program.names.clash;
    let encode = |[s, p, o]: Triple| {
        EncodedTriple::new(
            TermId::from_raw(s),
            TermId::from_raw(p),
            TermId::from_raw(o),
        )
    };
    let mut lower_triples = empty();
    for &t in lower {
        lower_triples.push(encode(t)).ok()?;
    }
    let mut view = View {
        revision,
        lower: snapshot.clone(),
        lower_facts: lower.len(),
        upper: None,
        unavailable: (!upper.program.incomplete.is_empty()).then(|| {
            format!(
                "the upper bound doesn't cover {} axiom(s) (first: {})",
                upper.program.incomplete.len(),
                upper.program.incomplete[0].1
            )
        }),
        gap_classes: HashSet::new(),
        gap_predicates: HashSet::new(),
        named_gap_classes: HashSet::new(),
        named_gap_predicates: HashSet::new(),
        internal: HashSet::new(),
        facts: upper.stack.len(),
        proves_consistency: upper.proves_consistency(),
        rules: false,
    };
    // A fact over a representative stands for its members: named where one of them is.
    let named_classes: HashSet<u64> = upper
        .classes()
        .classes()
        .filter(|(_, members)| members.iter().any(|&m| !upper.is_internal(m)))
        .map(|(representative, _)| representative)
        .collect();
    let named = |t: u64| !upper.is_internal(t) || named_classes.contains(&t);
    // L's memberships beside it, over representatives as the expanded reads want them.
    let classes = upper.classes();
    let over_representatives = match classes.is_empty() {
        true => None,
        false => {
            let mut over = empty();
            for &[s, p, o] in lower {
                over.push(encode([
                    classes.representative(s),
                    p,
                    classes.representative(o),
                ]))
                .ok()?;
            }
            Some(over)
        }
    };
    let capacity = upper.stack.len().checked_add(eligible.len())?;
    let mut triples = ChargedVec::with_capacity(Arc::clone(kept), capacity).ok()?;
    let mut take = |[s, p, o]: Triple| {
        // L's memberships from the taxonomy are certain: no gap.
        if held_by_lower(lower, classes, [s, p, o]) {
            return Some(());
        }
        for t in [s, p, o] {
            if upper.is_internal(t) {
                view.internal.insert(t);
            }
        }
        if p == clash {
            return Some(());
        }
        // By representative here; by member once every fact is seen ([`by_member`]).
        if p == rdf_type {
            view.gap_classes.insert(o);
            if named(s) {
                view.named_gap_classes.insert(o);
            }
        } else {
            view.gap_predicates.insert(p);
            if named(s) && named(o) {
                view.named_gap_predicates.insert(p);
            }
        }
        triples.push(encode([s, p, o])).ok()
    };
    for t in upper.stack.scan([None; 3]).chain(eligible.scan([None; 3])) {
        take(t)?;
    }
    for signature in [
        &mut view.gap_classes,
        &mut view.gap_predicates,
        &mut view.named_gap_classes,
        &mut view.named_gap_predicates,
    ] {
        *signature = by_member(std::mem::take(signature), classes, |t| upper.is_internal(t));
    }
    let same_as = (!classes.is_empty()).then(|| upper.same_as());
    Some(Parts {
        view,
        lower: lower_triples,
        upper: Some(UpperParts {
            over_representatives,
            facts: triples,
            same_as,
        }),
    })
}

/// The bounds at the store's latest revision, for reports and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundsReport {
    pub revision: u64,
    /// Why U1 can't bound answers (`None`: it can).
    pub unavailable: Option<String>,
    /// U1's facts beyond L.
    pub upper_facts: usize,
    /// L's memberships beyond the RL closure, from the TBox's taxonomy.
    pub lower_facts: usize,
    /// Classes and other predicates with facts in U1 beyond L.
    pub gap_classes: usize,
    pub gap_predicates: usize,
    /// How U1 got to this revision: `delta`, `rebuilt`, `read` or `kept`.
    pub last: &'static str,
    /// The literals U1 reads by value: those of predicates its rules compare.
    pub literals_by_value: usize,
    /// U1's `owl:sameAs` classes (kept by representatives) and their largest.
    pub equality_classes: usize,
    pub largest_class: usize,
}

/// The bounds at the latest revision (U1 built afresh if it doesn't describe it).
pub(crate) fn report(store: &StoreService) -> BoundsReport {
    let (snapshot, view) = self::view(store);
    let current = ReadBasis::upper(&snapshot);
    let last = current.as_ref().map_or("read", |s| s.last);
    let upper = current.as_ref().and_then(|s| s.upper.as_ref().ok());
    let literals_by_value = upper.map_or(0, |u| u.literals_by_value);
    let (equality_classes, largest_class) = upper.map_or((0, 0), |u| {
        let sizes = u.classes().classes().map(|(_, members)| members.len());
        sizes.fold((0, 0), |(n, max), size| (n + 1, max.max(size)))
    });
    BoundsReport {
        revision: view.revision,
        unavailable: view.unavailable.clone(),
        upper_facts: view.facts,
        lower_facts: view.lower_facts,
        gap_classes: view.gap_classes.len(),
        gap_predicates: view.gap_predicates.len(),
        last,
        literals_by_value,
        equality_classes,
        largest_class,
    }
}

/// U1's facts beyond L at the latest revision, as N-Triples terms, sorted: as
/// maintained, or with `afresh` evaluated anew (not installed). `None` if U1 isn't
/// available. For the differential tests of the maintenance.
pub(crate) fn upper_facts(store: &StoreService, afresh: bool) -> Option<Vec<[String; 3]>> {
    let snapshot = store.engine().snapshot();
    let facts: Vec<Triple> = if afresh {
        // Shared ids, as `afresh`'s (dictionary lifecycle V1): the decoder below, taken
        // after the build, reads them.
        let replica = store.dl().replica();
        if !replica {
            source::intern_system_vocabulary(store.engine()).ok()?;
        }
        let engine = store.engine();
        let latest = engine.snapshot();
        let kept = Arc::clone(store.dl().bounds.kept());
        let ontology =
            source::read_snapshot(&snapshot, kept, &store.config().dl.treatments).ok()?;
        let normalised = nrese_owl::normalise(&ontology);
        let memory = store.memory_stop();
        let deadline = Deadline::after(store.config().dl.timeout);
        let stop = || deadline.passed() || memory.exceeded();
        let upper = Upper::build(
            &ontology,
            &normalised,
            &snapshot,
            &|t| source::resolve_system(replica, engine, &latest, t),
            &stop,
            store.dl().bounds.kept(),
        )
        .ok()?;
        upper.stack.scan([None; 3]).collect()
    } else {
        let (snapshot, _) = view(store);
        ReadBasis::upper(&snapshot)?
            .upper
            .ok()?
            .stack
            .scan([None; 3])
            .collect()
    };
    let decoder = store.engine().snapshot();
    let decode = |t: u64| {
        decoder
            .decode(TermId::from_raw(t))
            .map_or_else(|| format!("#{t}"), |term| term.to_string())
    };
    let mut out: Vec<[String; 3]> = facts.into_iter().map(|t| t.map(decode)).collect();
    out.sort();
    Some(out)
}
