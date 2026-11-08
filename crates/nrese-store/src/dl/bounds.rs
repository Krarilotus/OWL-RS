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
use nrese_exec::cell::{BuildCell, Claim, Kind};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use nrese_engine::{EncodedQuad, EncodedTriple, ReadModel, Snapshot, TermId, Transaction};
use nrese_owl::Ontology;
use nrese_reasoner::eval::Stop;
use nrese_reasoner::ir::Triple;

use super::source;
use super::upper::{Change, GaveUp, Upper};
use crate::StoreService;

#[cfg(test)]
#[path = "bounds_memory_tests.rs"]
mod memory_tests;

#[cfg(test)]
#[path = "bounds_token_tests.rs"]
mod token_tests;

#[cfg(test)]
#[path = "bounds_lock_tests.rs"]
mod lock_tests;

/// Where the lock tests hold U1's work mid-way: a hook run once, on the thread doing that
/// work, for the store whose bounds it names.
#[cfg(test)]
pub(crate) mod seam {
    use std::sync::Mutex;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Point {
        /// A commit's change of U1, U1 taken and the change not begun.
        Change,
        /// U1 built afresh for a read, not begun.
        Build,
        /// The TBox's taxonomy for a read, not begun.
        Taxonomy,
        /// A revision's read view, not begun.
        View,
    }

    type Hook = Box<dyn FnOnce() + Send>;
    /// One hook a store and point: tests running at once, each on its own store, keep theirs.
    static HOOKS: Mutex<Vec<(usize, Point, Hook)>> = Mutex::new(Vec::new());

    fn key(bounds: &super::Bounds) -> usize {
        std::ptr::from_ref(bounds) as usize
    }

    pub(crate) fn set(bounds: &super::Bounds, point: Point, hook: impl FnOnce() + Send + 'static) {
        let mut hooks = HOOKS.lock().unwrap_or_else(|p| p.into_inner());
        hooks.retain(|(k, p, _)| !(*k == key(bounds) && *p == point));
        hooks.push((key(bounds), point, Box::new(hook)));
    }

    pub(crate) fn at(bounds: &super::Bounds, point: Point) {
        let hook = {
            let mut hooks = HOOKS.lock().unwrap_or_else(|p| p.into_inner());
            let at = hooks
                .iter()
                .position(|(k, p, _)| *k == key(bounds) && *p == point);
            at.map(|at| hooks.swap_remove(at))
        };
        if let Some((_, _, hook)) = hook {
            hook();
        }
    }
}

/// U1 at a revision.
struct State {
    revision: u64,
    /// Shared only with the commit computing its change ([`prepare`]), which lets go
    /// before [`install`] changes it in place: a read takes what it needs of U1 under the
    /// bounds' state, never keeping it.
    upper: Result<Arc<Upper>, GaveUp>,
    /// How U1 got to this revision: `delta` (the delta executor), `rebuilt` (compiled
    /// and evaluated afresh on commit), `read` (afresh for a query), `kept` (U1 gave up
    /// and nothing it is compiled from changed).
    last: &'static str,
    /// Which compilation of U1 this is: a new one each time U1 is compiled afresh, kept
    /// by a commit's change. The taxonomy's key.
    compiled: u64,
    /// The TBox's taxonomy for L's memberships ([`super::lower`]): computed on the first
    /// read, kept as long as U1's compilation (both change only with the schema).
    taxonomy: Option<KeptTaxonomy>,
    /// The ontology is in OWL 2 RL: the RL rules decide it (theorem PR1 of OWL 2
    /// Profiles: complete for its assertions and its consistency), and U1 isn't compiled.
    rules: bool,
}

/// The bounds' U1 (`None`: none), and the revision below which nothing is published: what
/// is published only moves forward (U1-P), a generation let go included (SYN-61).
#[derive(Default)]
struct Slot {
    current: Option<State>,
    floor: u64,
}

impl std::ops::Deref for Slot {
    type Target = Option<State>;
    fn deref(&self) -> &Option<State> {
        &self.current
    }
}

impl Slot {
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

/// The route the ontology's profile gives (`nrese_owl::profile`): an OWL 2 RL ontology
/// to the RL rules alone.
fn routed_to_rules(ontology: &Ontology) -> bool {
    nrese_owl::profile::of_ontology(ontology).rl
}

/// U1's place on the RL route: not compiled.
fn not_compiled() -> GaveUp {
    GaveUp::new("not compiled: the ontology is in OWL 2 RL, which the RL rules decide")
}

/// The store's bounds: U1 for the revision it describes, and the read view of the latest
/// revision a query asked for.
///
/// **No lock here is held across Rayon work, a wait, or a call into the engine** (design
/// note `2026-11-02-p2-c6b-u1-design.md`, U1-I): under `state` and `view` runs only serial
/// work over what is in memory. U1's change, its builds, its taxonomy and a view's runs
/// all run with none held, so whoever waits for one of these locks waits for a holder
/// that waits for nothing. A build several readers want is built once through a
/// [`BuildCell`]: a reader off the pool waits for it; one on a Rayon worker builds its
/// own (the bounded rule, `research-designs.md` §8). Lock order: the engine's writer slot,
/// the pipeline's ground cache, then `state` (a leaf); a cell's own lock, then `view` or
/// `state`; every dictionary lock after them.
#[derive(Default)]
pub(crate) struct Bounds {
    state: Mutex<Slot>,
    view: Mutex<Option<Arc<View>>>,
    /// Read views being built, by revision (U1 built afresh among them).
    views: BuildCell<u64, View>,
    /// Taxonomies being built, by U1's compilation ([`State::compiled`]).
    /// What a build hands its waiters is the taxonomy with its charge (SYN-61).
    taxonomies: BuildCell<u64, nrese_exec::cell::Kept<Taxonomy>>,
    compilations: AtomicU64,
    /// What the bounds keep is charged here while they keep it (SYN-61): the store's, within
    /// the queries' memory ([`Bounds::set_kept`]); unlimited until the store sets it.
    kept: std::sync::OnceLock<Arc<nrese_exec::Budget>>,
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
    /// beyond L (Skolem constants included): the gap's signature.
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
    /// The ontology is in OWL 2 RL: L is complete for every predicate, and consistent
    /// (the RL rules decide it).
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
    Delta(Box<Change>),
    /// U1 compiled and evaluated afresh (the schema changed, or U1 described another
    /// revision), or why that gave up.
    Rebuilt(Box<Result<Upper, GaveUp>>),
    /// U1 had given up, and nothing it was compiled from changed.
    Unavailable,
    /// The ontology is (still) in OWL 2 RL: the RL rules decide it, no U1.
    Rules,
}

/// A commit's preparation: what to install, and what U1 after the commit says about
/// consistency.
pub(crate) struct Preparation {
    base: u64,
    prepared: Prepared,
    /// U1 after the commit proves the data consistent (or, on the RL route, the RL
    /// rules do: [`Preparation::decided_by`]).
    pub proves_consistency: bool,
    /// `upper-bound`, or `rules` on the RL route.
    pub decided_by: &'static str,
    /// The ontology the commit leaves, where the preparation had to read it (for the
    /// consistency check, which then needn't read it again).
    pub ontology: Option<Ontology>,
}

/// Prepares U1 for the commit `tx` (after the RL reasoning applied its changes): the
/// change by the delta executor where only assertions over U1's signature changed, else
/// U1 afresh. Cost: the change's, or the closure's on a rebuild.
pub(crate) fn prepare(store: &StoreService, tx: &Transaction<'_>, stop: Stop<'_>) -> Preparation {
    let base = tx.base().revision();
    let pending = tx.pending_snapshot();
    let deadline = Deadline::after(store.config().dl.timeout);
    let memory = store.memory_stop();
    let stop = move || stop() || deadline.passed() || memory.exceeded();
    // U1 at the base, taken under the state and changed without it (U1-I): readers never
    // wait for a commit's (parallel) change.
    let current: Option<(bool, Result<Arc<Upper>, GaveUp>)> = lock(&store.dl().bounds.state)
        .as_ref()
        .filter(|s| s.revision == base)
        .map(|s| (s.rules, s.upper.clone()));
    // The commit's asserted changes, read from the transaction each time: no copy of them.
    let asserted = || tx.inserted().chain(tx.deleted()).map(quad_triple);
    if let Some((true, _)) = current
        && !asserted().any(|t| changes_schema(tx, t))
    {
        return Preparation {
            base,
            prepared: Prepared::Rules,
            proves_consistency: true,
            decided_by: "rules",
            ontology: None,
        };
    }
    if let Some((_, upper)) = current {
        match upper.as_deref() {
            // A U1 with equality classes is evaluated afresh (`Upper`'s module docs).
            Ok(upper)
                if upper.classes().is_empty() && asserted().all(|t| upper.is_assertion(t)) =>
            {
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
                match upper.change(&pending, &inserted, &deleted, &stop) {
                    Ok(change) if !change.merges() => {
                        return Preparation {
                            base,
                            decided_by: "upper-bound",
                            proves_consistency: upper.program.proves_consistency()
                                && upper.clashes_after(&change) == 0,
                            prepared: Prepared::Delta(Box::new(change)),
                            ontology: None,
                        };
                    }
                    // The commit equates terms: U1 afresh, by representatives (below).
                    Ok(_) => {}
                    Err(gave_up) => {
                        return Preparation {
                            base,
                            prepared: Prepared::Rebuilt(Box::new(Err(gave_up))),
                            decided_by: "upper-bound",
                            proves_consistency: false,
                            ontology: None,
                        };
                    }
                }
            }
            Err(_) if !asserted().any(|t| changes_schema(tx, t)) => {
                return Preparation {
                    base,
                    prepared: Prepared::Unavailable,
                    decided_by: "upper-bound",
                    proves_consistency: false,
                    ontology: None,
                };
            }
            _ => {}
        }
    }
    source::intern_vocabulary(tx);
    let Ok(ontology) = source::read_pending(tx, Arc::clone(store.dl().bounds.kept())) else {
        return Preparation {
            base,
            prepared: Prepared::Rebuilt(Box::new(Err(GaveUp::refused(source::refused(
                "the commit",
            ))))),
            decided_by: "upper-bound",
            proves_consistency: false,
            ontology: None,
        };
    };
    if routed_to_rules(&ontology) && ontology.diagnostics.iter().all(|d| !d.is_fatal()) {
        // The RL gate that ran before has decided the commit's consistency.
        return Preparation {
            base,
            prepared: Prepared::Rules,
            proves_consistency: true,
            decided_by: "rules",
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
    );
    Preparation {
        base,
        decided_by: "upper-bound",
        proves_consistency: upper.as_ref().is_ok_and(Upper::proves_consistency),
        prepared: Prepared::Rebuilt(Box::new(upper)),
        ontology: Some(ontology),
    }
}

/// Whether a changed statement may change the ontology's schema (and so U1's rules),
/// judged without U1's signature: a blank node, OWL's, RDF's or RDFS's vocabulary as
/// predicate (but `rdf:type` with another class, and `owl:sameAs`).
pub(crate) fn changes_schema(tx: &Transaction<'_>, [s, p, o]: Triple) -> bool {
    use nrese_engine::TermKind;
    let kind = |t: u64| TermId::from_raw(t).kind();
    if kind(s) == TermKind::BlankNode || kind(o) == TermKind::BlankNode {
        return true;
    }
    let reserved = |t: u64| match tx.decode(TermId::from_raw(t)) {
        Some(nrese_rdf::Term::NamedNode(n)) => {
            let iri = n.as_str();
            iri.starts_with("http://www.w3.org/1999/02/22-rdf-syntax-ns#")
                || iri.starts_with("http://www.w3.org/2000/01/rdf-schema#")
                || iri.starts_with("http://www.w3.org/2002/07/owl#")
        }
        _ => false,
    };
    let text = |t: u64| match tx.decode(TermId::from_raw(t)) {
        Some(nrese_rdf::Term::NamedNode(n)) => n.into_string(),
        _ => String::new(),
    };
    match text(p).as_str() {
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type" => reserved(o),
        "http://www.w3.org/2002/07/owl#sameAs" => false,
        _ => reserved(p),
    }
}

fn in_view(view: &Snapshot, [s, p, o]: Triple) -> bool {
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
pub(crate) fn install(store: &StoreService, preparation: Preparation, revision: u64) {
    let bounds = &store.dl().bounds;
    let mut state = lock(&bounds.state);
    match preparation.prepared {
        // Refused for memory: not kept, so a read of the revision builds it again (SYN-61);
        // the generation it would have replaced is superseded, and goes with its charge.
        Prepared::Rebuilt(upper) if upper.as_ref().as_ref().is_err_and(|g| g.refused) => {
            let superseded = state.evict_below(revision);
            drop(state);
            drop(superseded);
        }
        Prepared::Rebuilt(upper) => publish(
            &mut state,
            State {
                revision,
                upper: upper.map(Arc::new),
                last: "rebuilt",
                compiled: bounds.compiled(),
                taxonomy: None,
                rules: false,
            },
        ),
        Prepared::Rules => publish(
            &mut state,
            State {
                revision,
                upper: Err(not_compiled()),
                last: "rules",
                compiled: bounds.compiled(),
                taxonomy: None,
                rules: true,
            },
        ),
        Prepared::Delta(change) => match state.get_mut() {
            Some(s) if s.revision == preparation.base => {
                if let Ok(upper) = &mut s.upper {
                    // Nobody else holds it: the commit's preparation let go, and reads
                    // never keep it (`State::upper`).
                    let Some(upper) = Arc::get_mut(upper) else {
                        debug_assert!(false, "U1 shared at its commit's install");
                        drop(state.clear(revision));
                        return;
                    };
                    // The stack's budget couldn't take the change: U1 is partly changed,
                    // and the next read builds it afresh (or finds it doesn't fit).
                    if upper.apply(*change).is_err() {
                        drop(state.clear(revision));
                        return;
                    }
                }
                s.revision = revision;
                s.last = "delta";
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
}

/// The read view of the store's latest revision, and its snapshot: U1 built afresh if it
/// doesn't describe that revision (O(the closure)), the view once per revision. A view
/// already built is returned without waiting for anything. A missing one is built once:
/// a read off the pool waits for the build under way, one on a Rayon worker builds its
/// own (U1-B).
pub(crate) fn view(store: &StoreService) -> (Snapshot, Arc<View>) {
    let bounds = &store.dl().bounds;
    loop {
        let snapshot = store.engine().snapshot();
        let revision = snapshot.revision();
        let cached = || {
            lock(&bounds.view)
                .as_ref()
                .filter(|view| view.revision == revision)
                .map(Arc::clone)
        };
        if let Some(view) = cached() {
            return (snapshot, view);
        }
        let built = match bounds
            .views
            .claim(revision, Kind::Opaque, || cached().map(Some))
        {
            Claim::Build(builder) => {
                let built = read(store, &snapshot);
                builder.finish(built.clone());
                built
            }
            Claim::Wait(flight) => match flight.wait() {
                Ok(built) => built,
                Err(nrese_exec::cell::Busy) => read(store, &snapshot),
            },
        };
        // None: the bounds describe a later revision than the snapshot now, or the build
        // failed. Again, from the latest snapshot.
        if let Some(view) = built {
            return (snapshot, view);
        }
    }
}

/// The view of `snapshot`'s revision, published; `None` if the bounds describe a later
/// revision. U1 is the bounds' where they describe the revision, read under their state;
/// else built afresh here and published. No lock is held across U1's build, its taxonomy
/// or the view's runs (U1-I).
fn read(store: &StoreService, snapshot: &Snapshot) -> Option<Arc<View>> {
    let bounds = &store.dl().bounds;
    let revision = snapshot.revision();
    // An older revision's view the cache keeps holds its charges: let it go before building
    // this one, so the budget isn't held by a view no read of the latest wants (SYN-61).
    // Readers keep their own.
    let stale = {
        let mut cached = lock(&bounds.view);
        match cached.as_ref() {
            Some(view) if view.revision < revision => cached.take(),
            _ => None,
        }
    };
    drop(stale);
    // The bounds' U1 at the revision: its compilation, route, `rdf:type`, taxonomy.
    let found = {
        let state = lock(&bounds.state);
        match state.as_ref() {
            Some(s) if s.revision > revision => return None,
            Some(s) if s.revision == revision => Some((
                s.compiled,
                s.rules,
                s.upper.as_ref().ok().map(|u| u.program.names.rdf_type),
                s.taxonomy.clone(),
            )),
            _ => None,
        }
    };
    let kept = bounds.kept();
    // Whether the view may be kept: nothing a budget refused is (SYN-61).
    let mut keep = true;
    let memberships = |taxonomy: &Taxonomy, rdf_type: u64| {
        super::lower::memberships(snapshot, taxonomy, rdf_type, kept).ok()
    };
    let parts = match found {
        Some((compiled, rules, rdf_type, kept_taxonomy)) => {
            let lower = match rdf_type {
                Some(rdf_type) if !rules => {
                    // Held, with its charge, while its memberships are read.
                    let taxonomy = taxonomy(store, snapshot, compiled, kept_taxonomy);
                    keep &= taxonomy.is_ok();
                    memberships(of(&taxonomy), rdf_type)
                }
                _ => Some(ChargedVec::new(Arc::clone(kept))),
            };
            match lower {
                // Serial: what the view takes of U1, under the state (a commit's install
                // may change U1 in place once it is let go).
                Some(lower) => {
                    let state = lock(&bounds.state);
                    let s = state
                        .as_ref()
                        .filter(|s| s.revision == revision && s.compiled == compiled)?;
                    parts(
                        snapshot,
                        s.revision,
                        s.upper.as_deref(),
                        s.rules,
                        &lower,
                        kept,
                    )
                }
                None => None,
            }
        }
        None => {
            // A superseded generation can't be maintained any more (no commit's base is
            // older than the latest revision): let it and its charge go before building
            // afresh, so the budget isn't held by it while the build needs room (SYN-61).
            let superseded = lock(&bounds.state).evict_below(revision);
            drop(superseded);
            // The query ontology an older revision's reading left in its cache too: the cache
            // lets it go only when the taxonomy below reads this revision's, after the build
            // has read in its room (N1).
            store.dl().query_ontology.evict_below(revision);
            let (upper, rules) = afresh(store, snapshot);
            let compiled = bounds.compiled();
            let taxonomy = match &upper {
                Ok(_) if !rules => Some(taxonomy(store, snapshot, compiled, None)),
                _ => None,
            };
            keep &= taxonomy.as_ref().is_none_or(Result::is_ok);
            let lower = match (&upper, &taxonomy) {
                (Ok(upper), Some(taxonomy)) => {
                    memberships(of(taxonomy), upper.program.names.rdf_type)
                }
                _ => Some(ChargedVec::new(Arc::clone(kept))),
            };
            let parts = lower
                .and_then(|lower| parts(snapshot, revision, upper.as_ref(), rules, &lower, kept));
            // A U1 a budget refused is not kept: the next read builds it again.
            let refused_upper = upper.as_ref().is_err_and(|g| g.refused);
            keep &= !refused_upper;
            if !refused_upper {
                let last = match rules {
                    true => "rules",
                    false => "read",
                };
                let state = State {
                    revision,
                    upper: upper.map(Arc::new),
                    last,
                    compiled,
                    taxonomy: taxonomy.and_then(Result::ok),
                    rules,
                };
                publish(&mut lock(&bounds.state), state);
            }
            parts
        }
    };
    let Some(parts) = parts else {
        return Some(Arc::new(refused(snapshot, revision)));
    };
    #[cfg(test)]
    seam::at(bounds, seam::Point::View);
    let (view, charged) = assemble(snapshot, parts, kept);
    let view = Arc::new(view);
    if !(charged && keep) {
        // Not kept: the next read of the revision tries again.
        return Some(view);
    }
    let mut cached = lock(&bounds.view);
    if cached.as_ref().is_none_or(|v| v.revision < view.revision) {
        *cached = Some(Arc::clone(&view));
    }
    Some(view)
}

/// U1 compiled and evaluated afresh over `snapshot` (or, `true`, the RL route's place).
fn afresh(store: &StoreService, snapshot: &Snapshot) -> (Result<Upper, GaveUp>, bool) {
    let tx = store.engine().speculative();
    let replica = store.dl().replica();
    if !replica {
        source::intern_vocabulary(&tx);
    }
    let ontology = match source::read_snapshot(snapshot, Arc::clone(store.dl().bounds.kept())) {
        Ok(ontology) => ontology,
        Err(_) => return (Err(GaveUp::refused(source::refused("the revision"))), false),
    };
    if routed_to_rules(&ontology) && ontology.diagnostics.iter().all(|d| !d.is_fatal()) {
        return (Err(not_compiled()), true);
    }
    let normalised = nrese_owl::normalise(&ontology);
    let deadline = Deadline::after(store.config().dl.timeout);
    let memory = store.memory_stop();
    let stop = move || deadline.passed() || memory.exceeded();
    let resolve = |t: nrese_rdf::TermRef<'_>| source::resolve(replica, &tx, t);
    #[cfg(test)]
    seam::at(&store.dl().bounds, seam::Point::Build);
    let kept = store.dl().bounds.kept();
    let upper = Upper::build(&ontology, &normalised, snapshot, &resolve, &stop, kept);
    (upper, false)
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
fn taxonomy(
    store: &StoreService,
    snapshot: &Snapshot,
    compiled: u64,
    kept: Option<KeptTaxonomy>,
) -> ForRead {
    if let Some(kept) = kept {
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
    let kept = || {
        lock(&bounds.state)
            .as_ref()
            .filter(|s| s.compiled == compiled)
            .and_then(|s| s.taxonomy.clone())
    };
    loop {
        match bounds
            .taxonomies
            .claim(compiled, Kind::Opaque, || kept().map(Some))
        {
            Claim::Build(builder) => {
                let built = build();
                let Ok(charge) = built else {
                    // Given to nobody waiting: they read again (the builder's drop wakes them).
                    return built;
                };
                if let Some(s) = lock(&bounds.state).get_mut()
                    && s.compiled == compiled
                {
                    s.taxonomy.get_or_insert_with(|| Arc::clone(&charge));
                }
                builder.finish(Some(Arc::clone(&charge)));
                return Ok(charge);
            }
            Claim::Wait(flight) => match flight.wait() {
                // The taxonomy with its charge: kept while this read holds it.
                Ok(Some(charge)) => return Ok(charge),
                // The build failed: again.
                Ok(None) => {}
                Err(nrese_exec::cell::Busy) => return build(),
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
        match snapshot.with_inferred_added_within(&lower, kept) {
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
        let Ok(upper_view) = snapshot.with_inferred_added_within(&triples, kept) else {
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

/// What a view of `revision` takes of U1 and of L's memberships `lower` (sorted, each once):
/// serial, O(U1's facts), nothing waited for (U1-I). Its copies are charged to `kept` as they
/// grow (SYN-61); `None` where it can't take them.
fn parts(
    snapshot: &Snapshot,
    revision: u64,
    upper: Result<&Upper, &GaveUp>,
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
            proves_consistency: true,
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
    let mut triples = ChargedVec::with_capacity(Arc::clone(kept), upper.stack.len()).ok()?;
    for [s, p, o] in upper.stack.iter() {
        // L's memberships from the taxonomy are certain: no gap.
        if lower.binary_search(&[s, p, o]).is_ok() {
            continue;
        }
        for t in [s, p, o] {
            if upper.is_internal(t) {
                view.internal.insert(t);
            }
        }
        if p == clash {
            continue;
        }
        if p == rdf_type {
            if !upper.is_internal(o) {
                view.gap_classes.insert(o);
                if named(s) {
                    view.named_gap_classes.insert(o);
                }
            }
        } else if !upper.is_internal(p) {
            view.gap_predicates.insert(p);
            if named(s) && named(o) {
                view.named_gap_predicates.insert(p);
            }
        }
        triples.push(encode([s, p, o])).ok()?;
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
    let (_, view) = self::view(store);
    let state = lock(&store.dl().bounds.state);
    let current = state.as_ref().filter(|s| s.revision == view.revision);
    let last = current.map_or("read", |s| s.last);
    let upper = current.and_then(|s| s.upper.as_deref().ok());
    let literals_by_value = upper.map_or(0, Upper::literals_by_value);
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
    let decode = |t: u64| {
        snapshot
            .decode(TermId::from_raw(t))
            .map_or_else(|| format!("#{t}"), |term| term.to_string())
    };
    let facts: Vec<Triple> = if afresh {
        let tx = store.engine().speculative();
        let replica = store.dl().replica();
        if !replica {
            source::intern_vocabulary(&tx);
        }
        let ontology =
            source::read_snapshot(&snapshot, Arc::clone(store.dl().bounds.kept())).ok()?;
        let normalised = nrese_owl::normalise(&ontology);
        let memory = store.memory_stop();
        let deadline = Deadline::after(store.config().dl.timeout);
        let stop = || deadline.passed() || memory.exceeded();
        let upper = Upper::build(
            &ontology,
            &normalised,
            &snapshot,
            &|t| source::resolve(replica, &tx, t),
            &stop,
            store.dl().bounds.kept(),
        )
        .ok()?;
        upper.stack.iter().collect()
    } else {
        let _ = view(store);
        let state = lock(&store.dl().bounds.state);
        state.as_ref()?.upper.as_ref().ok()?.stack.iter().collect()
    };
    let mut out: Vec<[String; 3]> = facts.into_iter().map(|t| t.map(decode)).collect();
    out.sort();
    Some(out)
}
