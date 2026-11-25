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
//! an open gap, a reflexive `owl:sameAs` read (eq-ref is in neither bound), U1 not available, consistency not proven, a reader who sees part of the
//! data. Bag semantics: L's rows keep their multiplicities; a proved row is added once.

use nrese_dl::limits::{Deadline, Limit};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nrese_engine::{QuadPattern, ReadModel, Snapshot, TermId};
use nrese_owl::{Axiom, ClassExpr, ExprId, Literal, ObjProp, Ontology};
use nrese_rdf::{NamedNodeRef, Term};
use nrese_sparql::CancellationToken;
use nrese_sparql::completeness::{Bounds, Completeness};
use nrese_sparql_syntax::Query;
use nrese_sparql_syntax::algebra::{GraphPattern, PropertyPathExpression};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern, Variable};
use nrese_sparql_syntax::visit::Node;

use super::bounds::View;
use super::consistency::{self, Verdict};
use super::entailment::{self, Entailed};
use super::{DlAnswers, DlDetail, DlStatus, gate, source};
use crate::query_executor::{Answers, PreparedQuery};
use crate::{StoreError, StoreResult, StoreService};

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const OWL_SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
const OWL_THING: &str = "http://www.w3.org/2002/07/owl#Thing";

mod analysis;
mod exact;
mod gap;
use analysis::*;
pub(crate) use exact::{Ids, ground_axiom};

/// The ontology of a revision, kept for the exact services of its queries, charged to the
/// bounds' budget while kept (SYN-61); and the mark it gave a committed revision
/// ([`reads_differently`]), so reads at that revision one after another pass over it once.
#[derive(Debug, Default)]
pub(crate) struct OntologyCache(
    std::sync::Mutex<Option<(u64, KeptOntology)>>,
    std::sync::Mutex<Option<(u64, Option<String>)>>,
);

impl OntologyCache {
    /// Lets the cache's entry go if it is of a revision below `revision`, so that its charge
    /// doesn't hold the room a cold build of `revision` reads in (N1); a reader still holding
    /// it keeps its charge. Never waits: a reader holding the cache is either reading another
    /// revision, the old entry let go first ([`ontology_at`]), or returning its own entry,
    /// which then stays for this build (availability only).
    pub(crate) fn evict_below(&self, revision: u64) {
        let older = |entry: &mut (u64, KeptOntology)| entry.0 < revision;
        let obsolete = match self.0.try_lock() {
            Ok(mut cache) => cache.take_if(older),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                poisoned.into_inner().take_if(older)
            }
            Err(std::sync::TryLockError::WouldBlock) => None,
        };
        drop(obsolete);
    }
}

/// An ontology with its charge: the charge goes with the last holder, the cache or a reader,
/// never before (SYN-61). Readers hold this, not the bare ontology.
pub(crate) type KeptOntology = Arc<nrese_exec::cell::Kept<Ontology>>;

/// The asserted ontology at `snapshot`'s revision, read once per revision; its reading's
/// scratch and the ontology itself charged to the bounds' budget (the store's, within the
/// queries' memory: SYN-61), the ontology for as long as anyone holds it. `Err` where that
/// can't take them, with nothing kept.
pub(crate) fn ontology_at(
    store: &StoreService,
    snapshot: &Snapshot,
) -> Result<KeptOntology, nrese_exec::BudgetExceeded> {
    let mut cache = store
        .dl()
        .query_ontology
        .0
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if let Some((revision, ontology)) = &*cache
        && *revision == snapshot.revision()
    {
        return Ok(Arc::clone(ontology));
    }
    // The previous revision's, let go by the cache before this one is read and charged (a
    // reader still holding it keeps its charge).
    drop(cache.take());
    let held = read_ontology(store, snapshot)?;
    *cache = Some((snapshot.revision(), Arc::clone(&held)));
    Ok(held)
}

/// The asserted ontology of `snapshot`, read and charged as [`ontology_at`] reads it, and
/// held by its reader alone: for a transaction's pending state, whose revision number is
/// its base's and so names other data in the cache.
fn read_ontology(
    store: &StoreService,
    snapshot: &Snapshot,
) -> Result<KeptOntology, nrese_exec::BudgetExceeded> {
    let kept = store.dl().bounds.kept();
    let treatments = &store.config().dl.treatments;
    let ontology = Arc::new(source::read_snapshot(
        snapshot,
        Arc::clone(kept),
        treatments,
    )?);
    let bytes = nrese_dl::memory::ontology_bytes(&ontology);
    Ok(Arc::new(nrese_exec::cell::Kept::charge(
        ontology,
        bytes,
        Arc::clone(kept),
    )?))
}

/// The data's consistency at `snapshot`'s revision: recorded by its commit, proved by U1,
/// or checked now (once per revision) within the request's envelope `memory`. A recorded
/// or proved verdict survives the request's expiry, not its cancellation; a verdict its
/// envelope limited isn't recorded either.
fn consistency_at(
    store: &StoreService,
    snapshot: &Snapshot,
    view: &View,
    cancellation: &CancellationToken,
    deadline: Deadline,
    memory: &nrese_exec::Envelope,
) -> Verdict {
    if cancellation.is_cancelled() {
        return Verdict::Unknown("cancelled".to_owned());
    }
    if let Some(known) = consistency_known(store, snapshot, view) {
        return known;
    }
    let stopped = || {
        if cancellation.is_cancelled() {
            Some(Verdict::Unknown("cancelled".to_owned()))
        } else {
            deadline
                .passed()
                .then(|| Verdict::Unknown(Limit::Time.reached("the deadline passed")))
        }
    };
    if let Some(stopped) = stopped() {
        return stopped;
    }
    let cancel = nrese_dl::tableau::Cancel::from_flag(cancellation.flag());
    let Ok(ontology) = ontology_at(store, snapshot) else {
        return Verdict::Unknown(source::refused("the revision"));
    };
    let budget = gate::budget(store, Some(cancel), memory).remaining(deadline);
    if let Some(stopped) = stopped() {
        return stopped;
    }
    let checked = consistency::check(ontology.value(), &budget);
    // Request-local cancellation, expiry or memory must not replace shared revision
    // status.
    if let Some(stopped) = stopped() {
        return stopped;
    }
    if memory.refused() {
        return checked.verdict;
    }
    store.dl().record(DlStatus {
        revision: snapshot.revision(),
        consistency: checked.clone(),
        left_out: Some(left_out(ontology.value(), &consistency::nameless)),
    });
    checked.verdict
}

/// The data's consistency at `snapshot`'s revision where it is known without a check:
/// recorded by its commit, or proved by U1, or by the rules for this snapshot's revision, unknown
/// there where it holds an ill-typed literal ([`gate::ILL_TYPED`]) or the search for one stopped
/// ([`UNCHECKED`], never recorded: a later read searches again).
fn consistency_known(store: &StoreService, snapshot: &Snapshot, view: &View) -> Option<Verdict> {
    match store.dl().status() {
        Some(DlStatus {
            revision,
            consistency,
            ..
        }) if revision == snapshot.revision() => Some(consistency.verdict),
        _ if view.proves_consistency => Some(Verdict::Consistent),
        _ if !(view.rules && super::bounds::rules_prove(snapshot)) => None,
        _ => Some(match super::bounds::holds_ill_typed(store, snapshot) {
            Some(false) => Verdict::Consistent,
            Some(true) => Verdict::Unknown(gate::ILL_TYPED.to_owned()),
            None => Verdict::Unknown(UNCHECKED.to_owned()),
        }),
    }
}

/// Why an answer isn't complete for the data's consistency, if it isn't.
fn consistency_reason(verdict: Verdict) -> Option<String> {
    match verdict {
        Verdict::Consistent => None,
        Verdict::Inconsistent => {
            Some("the data is inconsistent under OWL 2 DL, so it entails every answer".to_owned())
        }
        Verdict::Unknown(why) => Some(format!(
            "the data's consistency under OWL 2 DL isn't known ({why})"
        )),
    }
}

/// The rules run over the RL closure only (DL-safe in effect: their variables bind to the
/// store's terms), never over what OWL 2 DL entails beyond it nor over U1: a conclusion
/// they'd draw from such a fact is missing from both bounds.
const USER_RULES: &str = "user rules run with OWL 2 DL: their conclusions from what only OWL 2 \
                          DL entails aren't computed, so no answer is claimed complete";

/// The bounds' counts as a query's evaluation finds them: the shared status keeps lower,
/// upper and unresolved; [`DlDetail`] the proved and refuted.
#[derive(Debug, Clone, Copy, Default)]
struct Counts {
    lower: u64,
    upper: Option<u64>,
    proved: u64,
    refuted: u64,
    by_labels: u64,
    unresolved: u64,
}

/// A DL answer's status as it is built: the shared [`Completeness`] (reasons from `dl`)
/// and what decided it ([`DlDetail`]).
#[derive(Debug)]
struct Status {
    completeness: Completeness,
    detail: DlDetail,
}

impl Default for Status {
    fn default() -> Self {
        Self {
            completeness: Completeness::under(nrese_sparql::Regime::Owl2Dl),
            detail: DlDetail::default(),
        }
    }
}

impl Status {
    fn complete() -> Self {
        Self::default()
    }

    fn sound_only(reason: impl Into<String>) -> Self {
        let mut status = Self::default();
        status.add(reason.into());
        status
    }

    fn add(&mut self, reason: String) {
        self.completeness.incomplete("dl", reason);
    }

    /// Names an unresolved candidate (the first [`super::UNRESOLVED_SHOWN`]).
    fn unresolved(&mut self, candidate: String) {
        if self.detail.unresolved.len() < super::UNRESOLVED_SHOWN {
            self.detail.unresolved.push(candidate);
        }
    }

    fn path(&mut self, path: &'static str) {
        if !self.detail.paths.contains(&path) {
            self.detail.paths.push(path);
        }
    }

    fn set_counts(&mut self, c: Counts) {
        self.detail.proved = c.proved;
        self.detail.refuted = c.refuted;
        self.detail.refuted_by_labels = c.by_labels;
        self.completeness.bounds = c.upper.map(|upper| Bounds {
            lower: c.lower,
            upper,
            unresolved: c.unresolved,
        });
    }

    fn stream(self, read: Read) -> Outcome {
        Outcome::Stream(self.completeness, read, self.detail)
    }

    fn answers(self, answers: Answers) -> Outcome {
        Outcome::Answers(answers, self.completeness, self.detail)
    }
}

/// What can be said before running `prepared`: complete where every predicate it reads
/// is closed; otherwise the bounds decide when it runs. `Cancelled` where the request stopped
/// while it waited for another request's build of the bounds' view, which runs on and is kept;
/// sound-only ([`DEADLINE`]) where the request's time was up instead.
pub(crate) fn plan_status(
    store: &StoreService,
    snapshot: &Snapshot,
    prepared: &PreparedQuery,
    cancellation: &CancellationToken,
) -> StoreResult<Completeness> {
    let analysis = analyse(prepared.query());
    let decided = decided(store, snapshot, prepared, cancellation)?;
    let completeness = match decided.as_ref() {
        Ok(at) => plan_status_built(store, prepared, &analysis, at).completeness,
        Err(why) => fallback(prepared, why).completeness,
    };
    Ok(composed(store, prepared, snapshot, &decided, completeness))
}

/// The status when the DL bounds view isn't used ([`PART`], [`LATER`], [`DEADLINE`]): `why`,
/// its cause without "answers over the RL closure" unless the read surely reads the closure
/// ([`reads_store_closure`]); for a reader of every graph that can't read it
/// ([`may_read_store_closure`]), also that its dataset leaves the closure out. A reader of
/// some graphs keeps [`PART`] alone: it never reads the bounds, and under `owl2-dl` it is
/// told `not-certified` whatever the reason (`status_for_its_reader` in `service.rs`), so no
/// reason here reaches it.
fn fallback(prepared: &PreparedQuery, why: &str) -> Status {
    let mut status = match why.strip_suffix(OVER_THE_CLOSURE) {
        Some(cause) if !reads_store_closure(prepared) => {
            Status::sound_only(format!("{cause}{NOT_CHECKED}"))
        }
        _ => Status::sound_only(why),
    };
    if let Some(reason) = prepared.dataset_omission_reason()
        && prepared.access().is_none()
        && !may_read_store_closure(prepared)
    {
        status.add(reason.to_owned());
    }
    status
}

/// Whether `prepared` surely reads the store's default graph, where the closure's inferred
/// statements are: its dataset keeps it, or its query names it by `GRAPH`
/// ([`reads_store_default_by_graph`]).
pub(crate) fn reads_store_closure(prepared: &PreparedQuery) -> bool {
    prepared.dataset_omission_reason().is_none()
        || reads_store_default_by_graph(prepared.query(), false)
}

/// Whether `prepared` may read the store's default graph: as [`reads_store_closure`], or by
/// a variable graph (`GRAPH ?g`), which may be bound to its name.
fn may_read_store_closure(prepared: &PreparedQuery) -> bool {
    prepared.dataset_omission_reason().is_none()
        || reads_store_default_by_graph(prepared.query(), true)
}

/// The bounds' view of `snapshot` its answers and their status are decided on, or
/// why there is none ([`PART`], [`LATER`], [`FAILED`], [`DEADLINE`]). Never another
/// snapshot's view.
type Decided = Result<(Snapshot, Arc<View>), &'static str>;

/// A reader of some graphs reads its own view of the snapshot, not the bounds'.
const PART: &str = "the reader sees part of the data: answers are checked against the bounds \
                    only for readers of every graph";

/// The end of [`LATER`] and [`DEADLINE`]: what a read without the bounds' view answers over,
/// where it surely reads the closure ([`fallback`]).
macro_rules! over_the_closure {
    () => {
        ": answers over the RL closure, not checked against the bounds"
    };
}
const OVER_THE_CLOSURE: &str = over_the_closure!();

/// [`OVER_THE_CLOSURE`] for a read that may leave the closure out.
const NOT_CHECKED: &str = ": not checked against the bounds";

/// An older revision whose bounds a later commit replaced reads its RL closure.
const LATER: &str = concat!(
    "a later commit's bounds replaced this revision's while it was read",
    over_the_closure!()
);

/// A read whose request's time was up before the view of its revision was built reads its RL
/// closure: the build, another read's or none begun, isn't waited for.
const DEADLINE: &str = concat!(
    "the request's time was up before the bounds' view of this revision was built",
    over_the_closure!()
);

/// A read whose wait ended with the build of its revision's view failed (a panic, an early
/// return) reads its RL closure.
const FAILED: &str = "the bounds' build of this revision's view failed: answers over the RL \
                      closure, not checked against the bounds";

/// [`Decided`] of `snapshot` for `prepared`.
fn decided(
    store: &StoreService,
    snapshot: &Snapshot,
    prepared: &PreparedQuery,
    cancellation: &CancellationToken,
) -> StoreResult<Decided> {
    if prepared.access().is_some() {
        return Ok(Err(PART));
    }
    use super::bounds::Missing;
    let stop = || cancellation.is_cancelled();
    let Some(view) = super::bounds::view_until(store, snapshot, prepared.deadline(), Some(&stop))
    else {
        // The operator's stop is the read's error; the request's time is a reason.
        cancellation.check()?;
        return Ok(Err(DEADLINE));
    };
    Ok(match view {
        Ok(view) => Ok((snapshot.clone(), view)),
        Err(Missing::Later) => Err(LATER),
        Err(Missing::Failed) => Err(FAILED),
    })
}

/// The exact services' deadline for `prepared`: `dl.timeout` from now, or the request's own
/// where that comes first.
fn deadline_of(store: &StoreService, prepared: &PreparedQuery) -> Deadline {
    let dl = Deadline::after(store.config().dl.timeout);
    let sooner = prepared
        .deadline()
        .filter(|at| dl.instant().is_none_or(|by| *at < by));
    sooner.map_or(dl, Deadline::at)
}

/// `status` composed once (`StoreService::assess`) on the data it was decided on: the
/// bounds' view, else `snapshot`; before any exact refusal (N4, N5).
fn composed(
    store: &StoreService,
    prepared: &PreparedQuery,
    snapshot: &Snapshot,
    decided: &Decided,
    status: Completeness,
) -> Completeness {
    let committed = Revision::Committed(snapshot);
    let at = decided.as_ref().map_or(committed, Revision::Decided);
    let assessed = store.assess(prepared, at, Some(status));
    assessed.unwrap_or_else(Completeness::not_certified)
}

/// The data a status is composed for ([`mark`]): the view the answers were decided on, or
/// the snapshot a read without the bounds ran on, committed or with a transaction's
/// operations pending (whose revision number is its base's, so it is read afresh).
#[derive(Clone, Copy)]
pub(crate) enum Revision<'a> {
    Decided(&'a (Snapshot, Arc<View>)),
    Committed(&'a Snapshot),
    Pending {
        snapshot: &'a Snapshot,
        /// Whether one of the operations may delete.
        deletes: bool,
        /// Whether they keep their base's record of what the reader left out
        /// (`dl::bounds::Bounds::keeps_record`).
        keeps_record: bool,
    },
}

impl Revision<'_> {
    pub(crate) fn snapshot(&self) -> &Snapshot {
        match self {
            Revision::Decided((snapshot, _)) => snapshot,
            Revision::Committed(snapshot) | Revision::Pending { snapshot, .. } => snapshot,
        }
    }
}

/// Every answer `unsound` where OWL 2 DL may not entail what the RL closure (L) holds
/// ([`reads_differently`]) in the data `at` it was read from, never a later revision's:
/// composed before certification by the one composition of a read's status
/// (`StoreService::assess`), for both DL entry points, a read without the bounds
/// (`StoreService::status_without_dl`) and a read inside a transaction with operations
/// pending (`StoreService::run_query_pending_reporting`). It reads every graph, and a reader
/// of some graphs is told no status under `owl2-dl` (`status_for_its_reader`); the ontology
/// it reads is the asserted statements', which a reader's view leaves as they are. And
/// at most sound-only where `prepared` names an individual the data doesn't and the
/// ontology `at` has keys, or wasn't read within its budget: whether a key reaches an
/// individual named only in a query is unsettled (the Direct Semantics' NAMED).
pub(crate) fn mark(
    store: &StoreService,
    prepared: &PreparedQuery,
    at: Revision<'_>,
    completeness: &mut Completeness,
) {
    if let Some(reason) = reads_differently(store, at) {
        completeness.unsound("dl", reason);
    }
    let snapshot = at.snapshot();
    let held = |iri: &str| {
        let Some(t) = snapshot.lookup(NamedNodeRef::new_unchecked(iri).into()) else {
            return false;
        };
        let (mut by_subject, mut by_object) = (QuadPattern::all(), QuadPattern::all());
        (by_subject.subject, by_object.object) = (Some(t), Some(t));
        let any = |p| {
            snapshot
                .quads_for_pattern_in(ReadModel::Materialised, p)
                .next()
        };
        any(&by_subject).or_else(|| any(&by_object)).is_some()
    };
    let read = match at {
        Revision::Pending { .. } => read_ontology,
        _ => ontology_at,
    };
    if let Some(iri) = named_outside(prepared.query(), held)
        && read(store, snapshot).ok().is_none_or(|o| {
            o.value()
                .axioms
                .iter()
                .any(|a| matches!(a, Axiom::HasKey(..)))
        })
    {
        completeness.incomplete(
            "dl",
            format!(
                "the query names <{iri}>, which the data doesn't, and the ontology has keys \
                 (or wasn't read within its budget): whether OWL 2 DL's keys reach an \
                 individual named only in a query is unsettled, so an answer may be missing"
            ),
        );
    }
}

/// Why a `complete` read [`every_name`] finds may miss answers, as the exact services decide
/// it for U1's element for every element, within the read's deadline, cancellation and
/// memory: its one answer variable bound to the element (an `ASK`: its ground atoms as they
/// stand). The element is a name in no axiom, and OWL 2 has no unique-name assumption, so it
/// may denote any element: `O ⊨ Q(fresh)` iff `O ⊨ ⊤ ⊑ Q`. `None`: refuted, so no name
/// answers the query that the store's individuals don't. Undecided: two answer variables or
/// more, one beside a name the atom holds of the element or reaching the answer through an
/// expression, or a query the exact services don't take. A proof under a filter is only of
/// the pattern (the exact services don't evaluate filters), so it says "may"; a refutation
/// still holds, as a filter only drops rows. The reasons hold of either name the store has
/// no individual for.
fn stand_in(
    store: &StoreService,
    view: &View,
    analysis: &Analysis,
    (what, named): (String, bool),
    (cancel, by): (&CancellationToken, Deadline),
    memory: &nrese_exec::Envelope,
) -> Option<String> {
    let domain = NamedNodeRef::new_unchecked(&nrese_dl::internal::u1("domain")).into_owned();
    let posed = |s: &&Shape| s.projected.is_empty() || (s.projected.len() == 1 && !named);
    #[cfg(test)]
    crate::service::hooks::run(&crate::service::hooks::BEFORE_STAND_IN);
    let verdict = analysis.shape.as_ref().filter(posed).and_then(|s| {
        let (v, lower) = (&s.projected, &view.lower);
        let rows = [vec![Some(Term::NamedNode(domain)); v.len()]];
        let mut decided = exact::decide_until(store, lower, analysis, v, &rows, cancel, by, memory);
        decided.verdicts.pop().map(|verdict| (verdict, s.filter))
    });
    let (how, is) = match verdict {
        Some((Entailed::No, _)) => return None,
        Some((Entailed::Yes, false)) => ("holds of every element", "is"),
        _ => ("may hold of every element (undecided)", "may be"),
    };
    Some(format!(
        "<{what}> {how}: every name the store has no individual for (a class's or property's \
         name, or one the data names only in an equality to itself) {is} an answer, and the \
         store returns none"
    ))
}

const ONLY: &str = "OWL 2 DL may not entail what the OWL 2 RL rules found here, and the \
                    answers are read from their closure; use the owl2-rl mode, whose \
                    promise is the rules' reading";

/// Why L may hold what OWL 2 DL doesn't entail, where it may: theorem PR1 of OWL 2
/// Profiles, by which L's statements are entailed, needs no annotation-property axioms
/// (OWL 2 DL gives them no meaning for the terms annotated; the rules use them) and no
/// punning, of which only an equality reaching a class or property name reads otherwise
/// (OWL 2 DL equates the individuals only; the rules the names too). Off the RL route only,
/// which both leave (`routed_to_rules` needs neither): what the store's indexes decide
/// ([`by_index`]), else one read of the ontology `at` ([`ontology_at`], a pending one's of
/// its own), whose refusal leaves it unknown. First [`left_out`], as recorded (pending: the
/// base's, taken where it tells something, and where it tells nothing only where the pending
/// changes keep it, as their commit would).
fn reads_differently(store: &StoreService, at: Revision<'_>) -> Option<String> {
    let (snapshot, pending, keeps_record) = match at {
        Revision::Decided((_, view)) if view.rules => return None,
        Revision::Decided((snapshot, _)) => (snapshot, false, true),
        Revision::Committed(snapshot) => (snapshot, false, true),
        Revision::Pending {
            snapshot,
            keeps_record,
            ..
        } => (snapshot, true, keeps_record),
    };
    // Named where its commit wrote it: only a reader of every graph is given the status this
    // reason is in (`status_for_its_reader`, service.rs). U1's proof of consistency is no
    // record: it says nothing of what the rules read beyond the ontology (an unused structure).
    let left_out = (store.dl().status())
        .filter(|s| s.revision == snapshot.revision())
        .and_then(|s| s.left_out);
    // A pending read's record is its base's: one that tells nothing says nothing of pending
    // changes that may change what the reader reads (a node typed the ontology reads its
    // statements otherwise, a literal gives an object property a second kind), so it reads
    // its own, but where they keep the record (`Bounds::keeps_record`). The indexes are the
    // pending state's own.
    match (left_out, by_index(snapshot)) {
        (Some(Some(why)), _) | (_, Some(Some(why))) => return Some(why),
        (Some(None), Some(None)) if keeps_record => return None,
        _ => {}
    }
    let marked = &store.dl().query_ontology.1;
    let marked = || marked.lock().unwrap_or_else(|p| p.into_inner());
    if !pending
        && let Some((revision, why)) = &*marked()
        && *revision == snapshot.revision()
    {
        return why.clone();
    }
    #[cfg(test)]
    cancellation_tests::ONTOLOGY_PASSES.set(cancellation_tests::ONTOLOGY_PASSES.get() + 1);
    let read = if pending { read_ontology } else { ontology_at };
    let Ok(ontology) = read(store, snapshot) else {
        return Some(format!(
            "whether OWL 2 DL reads the ontology otherwise than the OWL 2 RL rules isn't known \
             ({}): {ONLY}",
            source::refused("the revision")
        ));
    };
    let why = by_ontology(snapshot, ontology.value());
    if !pending {
        *marked() = Some((snapshot.revision(), why.clone()));
    }
    why
}

/// What the store's indexes decide of [`reads_differently`] without the ontology: `Some`
/// with the reason or with none, `None` where only the ontology decides. Annotation-property
/// axioms exactly, by the reader's rule: an annotation property (declared by the reader's
/// table, OWL 1's `owl:OntologyProperty` too, or built in) as
/// the subject of `rdfs:domain`, `rdfs:range` or `rdfs:subPropertyOf`, or as the object of
/// one whose subject is none; the smallest named, probed per property. A
/// pun needs a term equal to another: without one there is none; with one, the ontology
/// decides.
fn by_index(snapshot: &Snapshot) -> Option<Option<String>> {
    use nrese_owl::vocab::RDFS;
    let id = |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    let asserted =
        |s, p, o| snapshot.quads_for_pattern_in(ReadModel::Asserted, &any_graph(s, p, o));
    let properties: Vec<_> = annotation_properties(snapshot, |_| true).collect();
    let schema = ["domain", "range", "subPropertyOf"].map(|l| id(&format!("{RDFS}{l}")));
    let axioms = |s: TermId| {
        let of = |p| asserted(Some(s), Some(p), None).next().is_some();
        let under = schema[2].map(|p| asserted(None, Some(p), Some(s)));
        let mut under = under.into_iter().flatten();
        schema.into_iter().flatten().any(of) || under.any(|q| !properties.contains(&q.subject))
    };
    if let Some(property) = properties.iter().copied().filter(|&s| axioms(s)).min() {
        return Some(Some(annotation_reason(snapshot, property.raw())));
    }
    (!equal_terms(snapshot)).then_some(None)
}

/// The annotation properties by the reader's rule: declared by its table (OWL 1's
/// `owl:OntologyProperty` too) in a graph `readable` takes, IRIs that may name one only
/// ([`may_name_annotation`]), then the built-ins the store knows; lazily, a declaring type at
/// a time.
fn annotation_properties(
    snapshot: &Snapshot,
    readable: impl Fn(TermId) -> bool,
) -> impl Iterator<Item = TermId> {
    use nrese_owl::{BUILTIN_ANNOTATIONS as BUILTIN, EntityKind};
    let id = move |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    let (ty, kinds) = (id(RDF_TYPE), nrese_owl::DECLARATION_KINDS.into_iter());
    let typed = kinds.filter(|&(_, k)| k == EntityKind::AnnotationProperty);
    let typed = typed.filter_map(move |(iri, _)| ty.and(id(iri)));
    let quads =
        move |a| snapshot.quads_for_pattern_in(ReadModel::Asserted, &any_graph(None, ty, Some(a)));
    let named = typed.flat_map(quads).filter(move |q| readable(q.graph));
    let named = named
        .map(|q| q.subject)
        .filter(move |&s| may_name_annotation(snapshot, s));
    named.chain(BUILTIN.into_iter().filter_map(id))
}

/// Whether `term` is an IRI that may name an annotation property, by the reader's rule
/// ([`nrese_owl::may_name_annotation`]).
pub(crate) fn may_name_annotation(snapshot: &Snapshot, term: TermId) -> bool {
    let iri = snapshot.decode(term);
    matches!(iri, Some(Term::NamedNode(n)) if nrese_owl::may_name_annotation(n.as_str()))
}

/// Whether `snapshot` holds `owl:sameAs` between two different terms: the first found
/// decides, as the closure leaves the reflexive ones out but a class member's.
fn equal_terms(snapshot: &Snapshot) -> bool {
    let same_as = snapshot.lookup(NamedNodeRef::new_unchecked(OWL_SAME_AS).into());
    same_as.is_some_and(|p| {
        let pattern = any_graph(None, Some(p), None);
        let mut equal = snapshot.quads_for_pattern_in(ReadModel::Materialised, &pattern);
        equal.any(|q| q.object != q.subject)
    })
}

/// The statements of any graph with these terms.
fn any_graph(s: Option<TermId>, p: Option<TermId>, o: Option<TermId>) -> QuadPattern {
    let mut pattern = QuadPattern::all();
    (pattern.subject, pattern.predicate, pattern.object) = (s, p, o);
    pattern
}

/// [`reads_differently`]'s reason where `property` has annotation-property axioms.
fn annotation_reason(snapshot: &Snapshot, property: u64) -> String {
    let name = snapshot.decode(TermId::from_raw(property));
    let name = name.map_or_else(|| format!("term {property}"), |term| term.to_string());
    format!(
        "{name} has annotation-property axioms (a domain, a range, or a sub-property on either \
         side), which OWL 2 DL gives no meaning for the terms it annotates: {ONLY}, or remove them"
    )
}

/// [`reads_differently`]'s reason where the reader left out what the rules may read, or the
/// rules read a part otherwise than OWL 2 DL does (a name of two kinds, a structure no axiom
/// uses: `nrese_owl::Diagnostic::reads_otherwise`), its terms as `name` writes them: what
/// the premise of the rules' closure tells, `None` where it holds.
pub(crate) fn left_out(ontology: &Ontology, name: &dyn Fn(u64) -> String) -> Option<String> {
    if let Some(why) = consistency::left_out_by(ontology, nrese_owl::Diagnostic::leaves_out, name) {
        return Some(format!(
            "{why}, whose statements the OWL 2 RL rules may still read: {ONLY}, or remove them"
        ));
    }
    let otherwise: Vec<_> = (ontology.diagnostics.iter())
        .filter(|d| d.reads_otherwise())
        .collect();
    let first = otherwise.first()?.describe(name);
    Some(format!(
        "{} part(s) of the data aren't OWL 2 DL, and the OWL 2 RL rules read them otherwise; \
         first: {first}: {ONLY}, or remove them, or declare each name as one kind",
        otherwise.len()
    ))
}

/// [`reads_differently`] of `snapshot`'s whole `ontology`.
fn by_ontology(snapshot: &Snapshot, ontology: &Ontology) -> Option<String> {
    let name = |t: u64| {
        let term = snapshot.decode(TermId::from_raw(t));
        term.map_or_else(|| format!("term {t}"), |term| term.to_string())
    };
    if let Some(why) = left_out(ontology, &consistency::nameless) {
        return Some(why);
    }
    if let Some(&property) = ontology.annotation_schema.first() {
        return Some(annotation_reason(snapshot, property));
    }
    let same_as = snapshot.lookup(NamedNodeRef::new_unchecked(OWL_SAME_AS).into())?;
    let equated = nrese_owl::profile::punned(ontology)
        .into_iter()
        .find(|&t| {
            let equal = nrese_engine::QuadPattern {
                subject: Some(TermId::from_raw(t)),
                predicate: Some(same_as),
                object: None,
                graph: nrese_engine::GraphSelector::Any,
            };
            snapshot
                .quads_for_pattern_in(ReadModel::Materialised, &equal)
                .any(|q| q.object.raw() != t)
        })?;
    Some(format!(
        "{} is a class or property name and an individual equal to another, which OWL 2 DL \
         equates as individuals only: {ONLY}, or give the individual its own IRI",
        name(equated)
    ))
}

/// Why a read [`denied`] names may miss rows.
const DENIED: &str = "OWL 2 entails statements of the rdf:, rdfs: and owl: vocabulary that \
                      no closure, rewriting or bound holds in full";

/// Why a constant [`typed_constant`] names may miss rows.
const SPELLING: &str = "a stored value written another way doesn't match it, though OWL 2 \
                        entails the row by its value";

/// Why a value [`compared_values`] names may give wrong rows.
const COMPARED: &str = "two spellings of one value are two terms to the store, which compares \
                        terms, though OWL 2 compares values";

/// Why a read of a store in quarantine (its certificate's violations) may miss rows: fixed
/// words, nothing of the clash, for a reader of some graphs too (`/readyz` tells anyone).
const QUARANTINE: &str = "the store is in quarantine (the consistency rules found violations and \
                          have not found the data clean since), and as inconsistent data entail \
                          every answer, no read is certified complete until it leaves";

/// The one rule of every path's status under an OWL 2 regime, composed last onto it (by
/// both DL entry points, [`plan_status`] and [`answer`], after the bounds; over a ruleset's
/// closure, with or without the QL rewriting): each step only lowers what the status
/// asserts. Under `rdfs` and `custom` the regime is the closure's own: what it lacks isn't
/// entailed. A read [`denied`] names misses rows, and so does
/// a typed constant ([`typed_constant`]); a data value compared by term ([`compared_values`])
/// through a property that holds a literal other than a string, in `snapshot` (the data
/// read; unknown, or read for a restricted reader: every property), can give wrong rows;
/// a store in quarantine may miss rows ([`QUARANTINE`]), whoever reads it;
/// and where rows may be missing, an operator
/// under which a missing row can make a returned one wrong ([`negation`]) makes the answers
/// unsound. Each from `source`.
pub(crate) fn certify_closure(
    completeness: &mut Completeness,
    (query, reads): (&Query, &Reads),
    source: &'static str,
    snapshot: Option<&Snapshot>,
) {
    certify(completeness, (query, reads), source, false, snapshot);
}

/// The QL rewriting's own report as it leaves nrese-sparql (run, EXPLAIN, plan and the printed
/// query), with the whole composition ([`certify_closure`]) on it: the answer's status is this
/// report (`StoreService::status_outside_dl`), so the two are one value. Under `owl2-rl`, where
/// the closure leaves out a list axiom (`store`'s `lists_left_out`, asked only of a report still
/// `complete` after the composition's other steps, so an earlier reason keeps its place), whose
/// conclusions an answer may miss, it isn't `complete` ([`LIST`]); then the store's conditions
/// of the certificate `snapshot` carries ([`withhold`], as the answer's composition takes
/// them, with its sources), and the operator rule, last.
pub(crate) fn certify_report(
    completeness: Option<&mut Completeness>,
    query: (&Query, &Reads),
    snapshot: Option<&Snapshot>,
    store: &StoreService,
) {
    let Some(completeness) = completeness else {
        return;
    };
    if !lowerings(completeness, query, "ql", false, snapshot) {
        return;
    }
    let rl = completeness.regime == Some(nrese_sparql::Regime::Owl2Rl);
    if completeness.complete && rl && snapshot.is_some_and(|s| store.lists_left_out(s)) {
        completeness.incomplete("rules", LIST.to_owned());
    }
    // With or without a certificate (`print-query --schema` prints over schema files nothing
    // reasons over): the import is read from the statements; the literal equality, from it.
    if let Some(snapshot) = snapshot {
        let certificate = crate::service::Certificate::of(snapshot);
        let source = if certificate.is_some_and(|c| c.dl) {
            "dl"
        } else {
            "rules"
        };
        conditions(completeness, query, source, (snapshot, certificate), store);
    }
    negation_rule(completeness, query, "ql");
}

/// Why a QL report under `owl2-rl` isn't `complete` where the closure leaves out a list.
const LIST: &str =
    "a list axiom the rules can't build is left out of the closure (a list diagnostic)";

/// What [`certify`] reads of a query, which depends on the query and its reader alone, as the
/// reasons it gives: each found on its first use and kept with the prepared query
/// (`PreparedQuery::reads`), so a query run again walks and formats nothing.
#[derive(Debug, Clone, Default)]
pub(crate) struct Reads {
    /// [`denied`]'s reason, off the bounds' path and on it.
    denied: [std::sync::OnceLock<Option<String>>; 2],
    /// [`negation`]'s.
    negation: std::sync::OnceLock<Option<String>>,
    /// [`typed_constant`]'s.
    constant: std::sync::OnceLock<Option<String>>,
    /// [`compared_values`]'.
    compared: std::sync::OnceLock<Vec<(String, Vec<Option<String>>)>>,
    /// [`predicates`]', for [`annotation_rule`].
    predicates: std::sync::OnceLock<Vec<Option<String>>>,
    /// The reader's graphs, where their view is restricted: [`certify`] then takes every
    /// property to hold values other than strings, so the status doesn't follow what hidden
    /// graphs hold, and reads triple terms in their graphs alone.
    access: Option<std::sync::Arc<nrese_sparql::GraphAccess>>,
}

impl Reads {
    /// Nothing found yet, for a reader of every graph (`None`) or of `access`.
    pub(crate) fn new(access: Option<std::sync::Arc<nrese_sparql::GraphAccess>>) -> Self {
        Self {
            access,
            ..Self::default()
        }
    }
}

/// [`certify_closure`], on the bounds' path where `bounds` (what [`denied`] allows there).
pub(crate) fn certify(
    completeness: &mut Completeness,
    query: (&Query, &Reads),
    source: &'static str,
    bounds: bool,
    snapshot: Option<&Snapshot>,
) {
    if lowerings(completeness, query, source, bounds, snapshot) {
        negation_rule(completeness, query, source);
    }
}

/// [`certify`]'s steps before the operator rule; `false` (none taken) where the regime isn't
/// one of OWL 2's.
fn lowerings(
    completeness: &mut Completeness,
    (query, reads): (&Query, &Reads),
    source: &'static str,
    bounds: bool,
    snapshot: Option<&Snapshot>,
) -> bool {
    use nrese_sparql::Regime::{Owl2Dl, Owl2Ql, Owl2Rl};
    if !matches!(completeness.regime, Some(Owl2Dl | Owl2Ql | Owl2Rl)) {
        return false;
    }
    let read = reads.denied[usize::from(bounds)].get_or_init(|| {
        denied(query, bounds).map(|what| format!("the query reads {what}: {DENIED}"))
    });
    if let Some(reason) = read {
        completeness.incomplete(source, reason.clone());
    }
    if crate::PreparedQuery::uses_rdf12(query) {
        let why = "the query uses a triple term or a base direction, which OWL 2 doesn't define";
        completeness.incomplete(source, why.to_owned());
    }
    // The graphs the reader reads: the test the evaluation restricts their dataset by. The
    // inferred statements live in the default graph.
    let access = reads.access.as_deref();
    if snapshot.is_none_or(|s| {
        let read = |g| access.is_none_or(|a| a.allows_id(s, g));
        s.holds_triple_terms(ReadModel::Asserted, read)
            || (access.is_none_or(|a| a.inferred)
                && s.holds_triple_terms(ReadModel::Inferred, read))
    }) {
        let why = "the data read holds a triple term, which OWL 2 doesn't define";
        completeness.incomplete(source, why.to_owned());
    }
    let constant = reads
        .constant
        .get_or_init(|| typed_constant(query).map(|c| format!("the query names {c}: {SPELLING}")));
    if let Some(reason) = constant {
        completeness.incomplete(source, reason.clone());
    }
    // From the snapshot as given: a restricted reader's is dropped below.
    let quarantined = snapshot
        .and_then(crate::service::Certificate::of)
        .is_some_and(|c| c.state.violations > 0);
    let given = snapshot;
    let snapshot = snapshot.filter(|_| access.is_none());
    let typed = |predicate: &Option<String>| {
        snapshot.is_none_or(|s| match predicate {
            Some(p) => s
                .lookup(NamedNodeRef::new_unchecked(p).into())
                .is_some_and(|p| s.holds_typed_literals(ReadModel::Materialised, Some(p))),
            None => s.holds_typed_literals(ReadModel::Materialised, None),
        })
    };
    let compared = reads.compared.get_or_init(|| compared_values(query));
    if let Some((what, _)) = compared.iter().find(|(_, by)| by.iter().any(typed)) {
        completeness.incomplete(source, format!("the query compares {what}: {COMPARED}"));
    }
    if quarantined && completeness.complete {
        completeness.incomplete(source, QUARANTINE.to_owned());
    }
    annotation_rule(completeness, (query, reads), source, given);
    true
}

/// Where the status speaks of the OWL 2 Direct Semantics (`owl2-dl`, or the QL rewriting's,
/// not a ruleset's own closure), a read that may return the rules' copy of an annotation
/// statement ([`copied`]) makes the answers unsound: OWL 2 DL gives annotations no meaning, so
/// an equality or an annotation axiom entails no copy. From `snapshot` as given (with none,
/// every read but `rdf:type`); for a reader of some graphs, membership from its own graphs.
/// Every read inside a transaction meets it here too, through [`certify_closure`].
fn annotation_rule(
    completeness: &mut Completeness,
    (query, reads): (&Query, &Reads),
    source: &'static str,
    snapshot: Option<&Snapshot>,
) {
    if completeness.regime != Some(nrese_sparql::Regime::Owl2Dl) && source != "ql" {
        return;
    }
    let read = reads.predicates.get_or_init(|| predicates(query));
    let access = reads.access.as_deref();
    let found = match snapshot {
        _ if access.is_some_and(|a| !a.inferred) => None,
        Some(s) => copied(s, access, read),
        None => read.first(),
    };
    let Some(p) = found else { return };
    let what = p.as_ref().map(|p| format!("reads <{p}>"));
    let what = what.unwrap_or_else(|| "may read any predicate".into());
    completeness.unsound(source, format!("the query {what} {COPIES}"));
}

/// Why a read of literals may miss rows while the rules equate a literal with another
/// term ([`crate::service::Literal::Set`]).
const LITERAL_EQUALITY: &str = "the rules equate two literals as one value (an owl:sameAs \
                                between literals, which no store can hold), so a statement \
                                of one may lack its copy with the other";

/// [`LITERAL_EQUALITY`] as a reader by support is told it ([`may_equate_literals`]): whether the
/// rules equate two literals is read from every graph.
const LITERAL_EQUALITY_UNTOLD: &str = "the rules may equate two literals as one value (an \
                                       owl:sameAs between literals, which no store can hold), \
                                       so a statement of one may lack its copy with the other: \
                                       a reader of some graphs isn't told whether they do";

/// [`LITERAL_EQUALITY`] where it is unknown ([`crate::service::Literal::Unknown`]),
/// told every reader alike.
const LITERAL_EQUALITY_UNKNOWN: &str = "the store was opened without a rebuild of its \
                                        inferences, so its rules may have equated two literals \
                                        as one value (an owl:sameAs between literals, which no \
                                        store can hold) and a statement of one may lack its copy \
                                        with the other, until a rebuild (rematerialise) finds none";

/// Whether the graphs a reader by support reads (`access`) may give its view an equality of two
/// literals, read from its view alone (`snapshot`: what its graphs assert, and the inferences
/// they support). An inference it sees has its support there, so an equality it misses copies
/// of rests on premises there. The rules that conclude `owl:sameAs` read either one of these
/// (a functional or inverse-functional property, a maximum cardinality, qualified or not, a
/// key; an exact cardinality counted too) or an `owl:sameAs` (eq-sym, eq-trans: one with a
/// literal), or conclude a property a schema term names (eq-rep-p, prp-spo1, prp-eqp, prp-inv,
/// prp-symp, prp-trp, prp-spo2, cls-hv1), which is `owl:sameAs` only where a statement names it
/// as a subject or an object. Each is read in the view, declarations the rules derive included.
/// Rules other than a built-in ruleset's (`state`) may conclude one from anything.
fn may_equate_literals(
    snapshot: &Snapshot,
    access: &nrese_sparql::GraphAccess,
    state: &crate::ReasoningState,
) -> bool {
    const OWL: &str = "http://www.w3.org/2002/07/owl#";
    if nrese_reasoner::rulesets::Ruleset::from_name(&state.ruleset).is_none() {
        return true;
    }
    let id = |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    let owl = |name: &str| id(&format!("{OWL}{name}"));
    // A statement of its view: asserted in a graph it reads, or inferred where it sees them.
    let held = |pattern: QuadPattern| {
        (snapshot.quads_for_pattern_in(ReadModel::Asserted, &pattern))
            .any(|q| access.allows_id(snapshot, q.graph))
            || (access.inferred && snapshot.exists_in(ReadModel::Inferred, &pattern))
    };
    let stated =
        |p: Option<TermId>, o: Option<TermId>| p.is_some_and(|p| held(any_graph(None, Some(p), o)));
    let ty = id(RDF_TYPE);
    let typed = ["FunctionalProperty", "InverseFunctionalProperty"];
    let axioms = [
        "maxCardinality",
        "maxQualifiedCardinality",
        "cardinality",
        "qualifiedCardinality",
        "hasKey",
    ];
    let same = owl("sameAs");
    (typed.iter()).any(|class| owl(class).is_some_and(|class| stated(ty, Some(class))))
        || axioms.iter().any(|p| stated(owl(p), None))
        || same.is_some_and(|same| {
            holds_literals(snapshot, Some(same), Some(access))
                || held(any_graph(Some(same), None, None))
                || held(any_graph(None, None, Some(same)))
        })
}

/// The conditions of the store under which a read that rests on reasoning isn't told
/// `complete`, whatever its route (the closure, the QL rewriting, the bounds): one list,
/// applied by `StoreService::assess`, and by [`certify_report`] to the QL rewriting's report,
/// to a status under an OWL 2 regime, then the operator rule ([`negation_rule`]) over what it
/// took away. Each entry: the condition, the reads it touches, its reason. A reader of some
/// graphs is told nothing of the graphs it can't read.
/// - The rules equate a literal with another term (`state`): a read of a property that may
///   hold literals in `snapshot` ([`holds_literals`]; a variable predicate, any),
///   [`LITERAL_EQUALITY`]. That condition is the whole store's, so a reader by support is told
///   [`LITERAL_EQUALITY_UNTOLD`] of a read of a literal its view holds where its own graphs may
///   equate one ([`may_equate_literals`]), whether the rules do or not, and a reader who sees
///   every inference, never `complete` here (a rewriting's
///   assessment is withheld from it, the closure alone is `sound-only`, and `owl2-dl` is
///   `not-certified` to every reader of some graphs: `status_for_its_reader`), is told nothing
///   of it. Where it is unknown since an open without a rebuild, every reader is told
///   [`LITERAL_EQUALITY_UNKNOWN`], with no view consulted: every such open sets it, so it
///   tells nothing of the data.
/// - An import no loaded ontology answers ([`unread_import`]; for a reader of some graphs, in
///   the graphs it reads): every read.
/// - A reader by support under `owl2-ql` may miss conclusions the store-wide annotation
///   declarations kept out of the closure. Its grant alone decides the cap on every read.
/// - Under `owl2-dl` and `owl2-ql`, an ill-typed literal the version holds, as `certificate`
///   finds it on its first need under `store`'s memory stop (`Certificate::ill_typed`): every
///   read, [`ILL_TYPED`], naming the literal where the reader's graphs hold it; [`UNCHECKED`]
///   while the search hasn't ended.
pub(crate) fn withhold(
    completeness: &mut Completeness,
    (query, reads): (&Query, &Reads),
    source: &'static str,
    (snapshot, certificate): (&Snapshot, &crate::service::Certificate),
    store: &StoreService,
) {
    let read = (snapshot, Some(certificate));
    if conditions(completeness, (query, reads), source, read, store) {
        negation_rule(completeness, (query, reads), source);
    }
}

/// [`withhold`]'s conditions, without the operator rule; `false` (none taken) where the regime
/// isn't one of OWL 2's.
fn conditions(
    completeness: &mut Completeness,
    (query, reads): (&Query, &Reads),
    source: &'static str,
    (snapshot, certificate): (&Snapshot, Option<&crate::service::Certificate>),
    store: &StoreService,
) -> bool {
    use nrese_sparql::Regime::{Owl2Dl, Owl2Ql, Owl2Rl};
    if !matches!(completeness.regime, Some(Owl2Dl | Owl2Ql | Owl2Rl)) {
        return false;
    }
    use crate::service::Literal;
    let access = reads.access.as_deref();
    let literals = |predicate: &Option<String>| match predicate {
        Some(p) => (snapshot.lookup(NamedNodeRef::new_unchecked(p).into()))
            .is_some_and(|p| holds_literals(snapshot, Some(p), access)),
        None => holds_literals(snapshot, None, access),
    };
    let read = reads.predicates.get_or_init(|| predicates(query));
    let told = certificate.and_then(|c| match access {
        _ if c.literal == Literal::Unknown => Some(LITERAL_EQUALITY_UNKNOWN),
        None => (c.literal == Literal::Set).then_some(LITERAL_EQUALITY),
        Some(access) if access.inferred_by_support => {
            (may_equate_literals(snapshot, access, &c.state)).then_some(LITERAL_EQUALITY_UNTOLD)
        }
        Some(_) => None,
    });
    if let Some(reason) = told.filter(|_| read.iter().any(literals)) {
        completeness.incomplete(source, reason.to_owned());
    }
    if let Some(target) = unread_import(snapshot, access) {
        completeness.incomplete(
            source,
            format!(
                "the store holds owl:imports {target}, and no ontology with that IRI is \
                 loaded: its axioms take no part in reasoning"
            ),
        );
    }
    if completeness.regime == Some(Owl2Ql)
        && access.is_some_and(|a| a.inferred && a.inferred_by_support)
    {
        completeness.incomplete(
            source,
            "the reader can't read every graph, so its OWL 2 QL answers may be incomplete"
                .to_owned(),
        );
    }
    let ill_typed = certificate.filter(|_| matches!(completeness.regime, Some(Owl2Dl | Owl2Ql)));
    let found = ill_typed.map(|c| {
        let memory = store.memory_stop();
        c.ill_typed(snapshot, &|| memory.exceeded())
    });
    match found {
        Some(Some(None)) | None => {}
        Some(None) => completeness.incomplete(source, UNCHECKED.to_owned()),
        Some(Some(Some(witness))) => {
            let mut pattern = QuadPattern::all();
            pattern.object = Some(witness);
            let seen = (snapshot.quads_for_pattern_in(ReadModel::Asserted, &pattern))
                .any(|q| access.is_none_or(|a| a.allows_id(snapshot, q.graph)));
            let what = (seen.then(|| snapshot.decode(witness)).flatten())
                .map_or_else(|| "a literal".into(), |t| format!("the literal {t}"));
            completeness.incomplete(source, format!("the store holds {what}: {ILL_TYPED}"));
        }
    }
    true
}

/// Why no read is `complete` while the store holds an ill-typed literal
/// (`Certificate::ill_typed`), and while it isn't known whether it holds one.
const ILL_TYPED: &str = "it is ill-typed (its lexical form is outside its datatype's lexical \
                         space), which OWL 2 doesn't admit (Structural Specification §5.7), so \
                         the data's consistency isn't known";
pub(crate) const UNCHECKED: &str = "the data are not yet checked for an ill-typed literal, which OWL 2 \
                         doesn't admit, so their consistency isn't known";

/// Whether a version reading under `owl2-dl` (`dl`) or under the rules program named `ruleset`
/// keeps the witness of an ill-typed literal: under `owl2-dl` and a program on `owl2-ql`, user
/// rules beside it or not (`owl2-ql+custom:…`, its base the name's first part, as the
/// reasoner reads it), where [`withhold`] reads it, and nowhere else, so no other mode scans.
pub(crate) fn keeps_ill_typed(dl: bool, ruleset: &str) -> bool {
    dl || ruleset.split('+').next() == Some("owl2-ql")
}

/// Whether `id` is an ill-typed literal (RDF 1.1 Concepts §3.3): a datatype of the OWL 2
/// datatype map, a lexical form outside its lexical space, as the map's owner decides it
/// (`nrese_xsd::owl::Value::parse`, which the RL datatype rules and the DL reader read too).
/// An inline literal is a canonical form of its datatype, so only the dictionary's are read.
pub(crate) fn is_ill_typed(id: TermId, decode: &dyn Fn(TermId) -> Option<Term>) -> bool {
    use nrese_xsd::owl::{Datatype, LiteralError, Value};
    if !id.kind().is_dictionary_literal() {
        return false;
    }
    let Some(Term::Literal(literal)) = decode(id) else {
        return false;
    };
    Datatype::from_iri(literal.datatype().as_str()).is_some_and(|datatype| {
        let value = Value::parse(literal.value(), datatype, literal.language());
        matches!(value, Err(LiteralError::IllTyped(_)))
    })
}

/// The ill-typed literal ([`is_ill_typed`]) of what the commit `tx` publishes, its base's being
/// `base`: the base's while `tx` still holds it, else one `tx` inserts, else, where `tx`
/// deleted the base's, one a scan of what `tx` commits finds ([`ill_typed`]); `None` where
/// `stop`, asked before each walk and during it ([`polled`]), ended it.
pub(crate) fn ill_typed_after(
    tx: &nrese_engine::Transaction<'_>,
    base: Option<TermId>,
    stop: &dyn Fn() -> bool,
) -> Option<Option<TermId>> {
    let decode = |id: TermId| tx.decode(id);
    let poll = polled(stop);
    let stopped = || poll.ask();
    if stopped() {
        return None;
    }
    // The base's occurrences through its counted scan, which counts the keys it passes,
    // deleted ones too (`nrese_engine::visit`); then those `tx` inserts.
    let kept = base.filter(|&witness| {
        let mut pattern = QuadPattern::all();
        pattern.object = Some(witness);
        let (counted, visit) = (tx.base().counted(), |units: u64| !poll.spend(units));
        let mut stored =
            (counted.quads(ReadModel::Asserted, &pattern, &visit)).map_while(Result::ok);
        stored.any(|q| tx.contains_in(ReadModel::Asserted, &q))
            || (tx.inserted().take_while(|_| !stopped())).any(|q| q.object == witness)
    });
    let mut judged = |o: &TermId| stopped() || is_ill_typed(*o, &decode);
    let found = kept.or_else(|| (tx.inserted().map(|q| q.object)).find(&mut judged));
    match (found, base) {
        _ if stopped() => None,
        (None, Some(_)) if stop() => None,
        (None, Some(_)) => ill_typed(&tx.pending_snapshot(), stop),
        _ => Some(found),
    }
}

/// `stop` asked at the first unit of work and then once 4096 more are spent, and kept once it
/// says stop: the poll of a walk, which a refusal ends with nothing kept. A walk's step spends
/// one ([`Polled::ask`]); an engine's counted read, what it counts ([`Polled::spend`]). Asked
/// by shared reference, so one poll serves the walks and the reads of one search.
struct Polled<'a> {
    stop: &'a dyn Fn() -> bool,
    spent: std::cell::Cell<u64>,
    due: std::cell::Cell<u64>,
    stopped: std::cell::Cell<bool>,
}
fn polled(stop: &dyn Fn() -> bool) -> Polled<'_> {
    let (spent, due, stopped) = Default::default();
    Polled {
        stop,
        spent,
        due,
        stopped,
    }
}
impl Polled<'_> {
    fn ask(&self) -> bool {
        self.spend(1)
    }
    fn spend(&self, units: u64) -> bool {
        if !self.stopped.get() && self.spent.get() >= self.due.get() {
            self.stopped.set((self.stop)());
            self.due.set(self.spent.get() + 4096);
        }
        self.spent.set(self.spent.get() + units);
        self.stopped.get()
    }
}

/// An ill-typed literal ([`is_ill_typed`]) an asserted statement of `snapshot` holds as its
/// object, in any graph: one walk of the object-first index over the dictionary's literals,
/// each judged once (every asserted statement, where the index can't answer by range); `None`
/// where `stop`, asked before the walk and during it ([`polled`]), ended it. One thread.
pub(crate) fn ill_typed(snapshot: &Snapshot, stop: &dyn Fn() -> bool) -> Option<Option<TermId>> {
    use nrese_engine::{TermKind, quad::Permutation};
    let poll = polled(stop);
    let stopped = || poll.ask();
    if stopped() {
        return None;
    }
    let decode = |id: TermId| snapshot.decode(id);
    let mut last = None;
    let mut judged =
        |o: &TermId| stopped() || (last.replace(*o) != Some(*o) && is_ill_typed(*o, &decode));
    let all = QuadPattern::all();
    let kinds = [
        TermKind::String,
        TermKind::LangString,
        TermKind::TypedLiteral,
    ];
    let ranges = kinds.map(|kind| {
        let (low, high) = TermId::kind_range(kind);
        snapshot.scan_range_in(ReadModel::Asserted, &all, Permutation::Ospg, low, high)
    });
    let found = match ranges {
        [Some(a), Some(b), Some(c)] => a.chain(b).chain(c).map(|q| q.object).find(&mut judged),
        _ => (snapshot.quads_for_pattern_in(ReadModel::Asserted, &all))
            .map(|q| q.object)
            .find(&mut judged),
    };
    // The stop asked again at the end, whatever the poll's count: the range scan can pass
    // deleted keys within one step uncounted, and a stop it ran past leaves nothing found.
    (!(stopped() || stop())).then_some(found)
}

/// The W3C vocabulary documents, whose meaning is built in: an import of one counts as read.
const BUILT_IN_ONTOLOGIES: [&str; 4] = [
    "http://www.w3.org/1999/02/22-rdf-syntax-ns",
    "http://www.w3.org/2000/01/rdf-schema",
    "http://www.w3.org/2002/07/owl",
    "http://www.w3.org/2001/XMLSchema",
];

/// The first ontology an asserted `owl:imports` names that the statements a read reasons over
/// don't hold (OWL 2 Structural Specification §3.4): no `<t> a owl:Ontology` and no
/// `?o owl:versionIRI <t>` among them, and not a W3C vocabulary ([`BUILT_IN_ONTOLOGIES`], with
/// or without its trailing `#`). The store follows no import, so that ontology's axioms take
/// no part in reasoning. Every `owl:imports` the store holds is checked, an imported
/// ontology's own too, so the closure of imports is checked link by link. A document address
/// is no ontology IRI: it is read only where a header names it. The statements read are the
/// whole store's, or, for a reader of some graphs (`access`), its own graphs': a reader by
/// support reasons over them alone, and one who sees every inference is told nothing of a
/// graph it can't read. One index lookup where the store never held `owl:imports`.
fn unread_import(
    snapshot: &Snapshot,
    access: Option<&nrese_sparql::GraphAccess>,
) -> Option<String> {
    let id = |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    let imports = id("http://www.w3.org/2002/07/owl#imports")?;
    let readable = |g: TermId| access.is_none_or(|a| a.allows_id(snapshot, g));
    let held = |pattern: QuadPattern| {
        (snapshot.quads_for_pattern_in(ReadModel::Asserted, &pattern)).any(|q| readable(q.graph))
    };
    let (ty, ontology) = (id(RDF_TYPE), id("http://www.w3.org/2002/07/owl#Ontology"));
    let version = id("http://www.w3.org/2002/07/owl#versionIRI");
    let pattern = any_graph(None, Some(imports), None);
    (snapshot.quads_for_pattern_in(ReadModel::Asserted, &pattern))
        .filter(|q| readable(q.graph))
        .map(|q| q.object)
        .find(|&target| {
            let built_in = matches!(snapshot.decode(target), Some(Term::NamedNode(iri))
                if BUILT_IN_ONTOLOGIES.contains(&iri.as_str().trim_end_matches('#')));
            let header = || {
                ty.zip(ontology)
                    .is_some_and(|(ty, o)| held(any_graph(Some(target), Some(ty), Some(o))))
            };
            let versioned =
                || version.is_some_and(|v| held(any_graph(None, Some(v), Some(target))));
            !(built_in || header() || versioned())
        })
        .map(|target| {
            snapshot
                .decode(target)
                .map_or_else(|| format!("term {}", target.raw()), |t| t.to_string())
        })
}

/// Whether a statement read with `predicate` (any, where `None`) has a literal object, of
/// any kind: strings have spellings too (`"Rex"` and `"Rex"^^xsd:token`, `"Rex"@en` and
/// `"Rex@en"^^rdf:PlainLiteral`). The literal kinds' ids are two ranges, so this is up to
/// two seeks in the predicate-first index (object-first without a predicate); `true` where
/// the index can't answer by range. For a reader of some graphs (`access`), a statement it
/// reads: asserted in its graphs, or an inferred one where it sees them (`snapshot` is its
/// view: a reader by support's holds the inferences its graphs support).
fn holds_literals(
    snapshot: &Snapshot,
    predicate: Option<TermId>,
    access: Option<&nrese_sparql::GraphAccess>,
) -> bool {
    use nrese_engine::{TermKind, quad::Permutation};
    let mut pattern = QuadPattern::all();
    pattern.predicate = predicate;
    let index = predicate.map_or(Permutation::Ospg, |_| Permutation::Posg);
    let held = |model, readable: &dyn Fn(TermId) -> bool| {
        [
            (TermKind::String, TermKind::TypedLiteral),
            (TermKind::DerivedInteger, TermKind::DerivedInteger),
        ]
        .into_iter()
        .any(|(first, last)| {
            let (low, high) = (TermId::kind_range(first).0, TermId::kind_range(last).1);
            (snapshot.scan_range_in(model, &pattern, index, low, high))
                .is_none_or(|mut quads| quads.any(|q| readable(q.graph)))
        })
    };
    match access {
        None => held(ReadModel::Materialised, &|_| true),
        Some(access) => {
            held(ReadModel::Asserted, &|g| access.allows_id(snapshot, g))
                || (access.inferred && held(ReadModel::Inferred, &|_| true))
        }
    }
}

/// Why a read [`annotation_rule`] names may give wrong rows.
const COPIES: &str = "and the closure may hold the rules' copies of annotation statements (by \
                      an equality or an annotation axiom), which OWL 2 DL doesn't entail";

/// The first of `read` that may return a copy in `snapshot`: an annotation property (one of
/// [`annotation_properties`], for `None`; a constant by its own declarations where it may name
/// one, or as built in,
/// none where the store doesn't know the IRI), or, under an equality, a SHACL property, with
/// statements held where an equality between two different terms is, or inferred ones. For a
/// reader by support (`access`), held only in the graphs it reads: a copy it sees has its
/// support there, or copies an inferred statement.
fn copied<'a>(
    snapshot: &Snapshot,
    access: Option<&nrese_sparql::GraphAccess>,
    read: &'a [Option<String>],
) -> Option<&'a Option<String>> {
    use ReadModel::{Inferred, Materialised};
    use nrese_owl::{BUILTIN_ANNOTATIONS as BUILTIN, DECLARATION_KINDS, EntityKind};
    let readable = |g: TermId| access.is_none_or(|a| a.allows_id(snapshot, g));
    let by_support = access.is_some_and(|a| a.inferred_by_support);
    let id = |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    let asserted = |pattern| snapshot.quads_for_pattern_in(ReadModel::Asserted, &pattern);
    let equal = std::cell::OnceCell::new();
    let equal = || *equal.get_or_init(|| equal_terms(snapshot));
    // A reader by support sees the copy of a statement in its own graphs, or of an inferred one.
    let copies = |p| {
        let equal = equal();
        let (of, held) = (any_graph(None, Some(p), None), equal && !by_support);
        let model = if held { Materialised } else { Inferred };
        let seen = equal && by_support && asserted(of).any(|q| readable(q.graph));
        seen || snapshot.exists_in(model, &of)
    };
    let kind = |k: &str| DECLARATION_KINDS.contains(&(k, EntityKind::AnnotationProperty));
    let declares = |q: nrese_engine::EncodedQuad| {
        let object = snapshot.decode(q.object);
        readable(q.graph) && matches!(object, Some(Term::NamedNode(k)) if kind(k.as_str()))
    };
    // SHACL content (`nrese_owl::shacl_properties`) is copied only through an equality of two
    // different terms (OWL 2 Profiles §4.3, Table 4: eq-rep-s, eq-rep-p, eq-rep-o): an OWL axiom
    // on a SHACL term makes the reader read the store as any other, and a copy is then entailed.
    // So with no equality its properties aren't probed. Nor are the properties of statements
    // about the ontology's node ([`header_properties`]), which the reader reads as annotations
    // where they are declared no object or data property: undeclared, they name no annotation
    // property by a declaration, and an equality copies them.
    read.iter().find(|p| match p.as_deref() {
        None => {
            annotation_properties(snapshot, readable).any(copies)
                || (equal()
                    && (nrese_owl::shacl_properties())
                        .filter_map(|i| id(&i))
                        .chain(header_properties(snapshot, readable))
                        .any(copies))
        }
        Some(iri) => id(iri).is_some_and(|p| {
            let typed = id(RDF_TYPE).map(|ty| asserted(any_graph(Some(p), Some(ty), None)));
            let shacl = nrese_owl::is_shacl_property(iri) && equal();
            let header = || equal() && header_properties(snapshot, readable).any(|h| h == p);
            let declared = || typed.into_iter().flatten().any(declares);
            (BUILTIN.contains(&iri)
                || shacl
                || (nrese_owl::may_name_annotation(iri) && declared())
                || header())
                && copies(p)
        }),
    })
}

/// The properties of the asserted statements about a node typed `owl:Ontology` (the
/// ontology's header), each statement and typing in a graph `readable` takes, but an object
/// or data property's, whose statement there is an assertion (*Mapping to RDF* Table 10; the
/// reader's rule): one declared so in a graph `readable` takes, or built in; lazily, a node
/// at a time.
fn header_properties(
    snapshot: &Snapshot,
    readable: impl Fn(TermId) -> bool + Copy,
) -> impl Iterator<Item = TermId> {
    use nrese_owl::vocab::OWL;
    let id = move |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    let (ty, ontology) = (id(RDF_TYPE), id(&format!("{OWL}Ontology")));
    let asserted =
        move |s, p, o| snapshot.quads_for_pattern_in(ReadModel::Asserted, &any_graph(s, p, o));
    let declaring = ["ObjectProperty", "DatatypeProperty"].map(|k| id(&format!("{OWL}{k}")));
    let built_in = [
        "topObjectProperty",
        "bottomObjectProperty",
        "topDataProperty",
        "bottomDataProperty",
    ]
    .map(|p| id(&format!("{OWL}{p}")));
    let logical = move |p: TermId| {
        built_in.contains(&Some(p))
            || (declaring.into_iter().flatten())
                .any(|k| asserted(Some(p), ty, Some(k)).any(|q| readable(q.graph)))
    };
    let nodes = ty
        .zip(ontology)
        .into_iter()
        .flat_map(move |(ty, ontology)| {
            asserted(None, Some(ty), Some(ontology)).filter(move |q| readable(q.graph))
        });
    nodes.flat_map(move |node| {
        (asserted(Some(node.subject), None, None))
            .filter(move |q| readable(q.graph) && Some(q.predicate) != ty)
            .map(|q| q.predicate)
            .filter(move |&p| !logical(p))
    })
}

/// Where rows may be missing, an operator under which a missing row can make a returned
/// one wrong makes the answers unsound ([`negation`]): the last step of [`certify`], and of
/// a read inside a transaction whatever the regime, whose pending rows the closure lacks. Its
/// reason is given once: a read it already lowered (its route's composition, before the
/// pending operations') isn't told it again from another source.
pub(crate) fn negation_rule(
    completeness: &mut Completeness,
    (query, reads): (&Query, &Reads),
    source: &'static str,
) {
    if completeness.complete {
        return;
    }
    let operator = reads.negation.get_or_init(|| {
        negation(query).map(|op| {
            format!("the query uses {op} over answers that may be missing: a row may be wrong")
        })
    });
    let given = |reason: &String| completeness.reasons.iter().any(|r| r.text == *reason);
    if let Some(reason) = operator.as_ref().filter(|reason| !given(reason)) {
        completeness.unsound(source, reason.clone());
    }
}

fn plan_status_built(
    store: &StoreService,
    prepared: &PreparedQuery,
    analysis: &Analysis,
    (snapshot, view): &(Snapshot, Arc<View>),
) -> Status {
    if prepared.has_dataset() {
        return Status::sound_only(prepared.dl_dataset_reason());
    }
    // What the run adds whatever path decides ([`decide_answers`]); a consistency the run
    // would check is not known yet.
    let mut status = Status::complete();
    let consistency = match consistency_known(store, snapshot, view) {
        Some(known) => consistency_reason(known),
        None => Some(
            "the data's consistency under OWL 2 DL isn't checked yet: decided when it runs"
                .to_owned(),
        ),
    };
    if let Some(why) = consistency {
        status.add(why);
    }
    if crate::service::Certificate::route(snapshot, store.dl()).1 {
        status.add(USER_RULES.to_owned());
    }
    if let Some(what) = unbounded(analysis) {
        status.add(format!(
            "the query reads {what}: entailed schema statements aren't bounded"
        ));
        return status;
    }
    if let Some(what) = untyped_same_as(prepared.query(), view) {
        status.add(format!("the query reads {what}: {DENIED}"));
        return status;
    }
    // Answered over L alone; [`certify_closure`] says what that misses.
    if analysis.reflexive_same_as {
        return status;
    }
    if let Some((what, _)) = every_name(analysis, view) {
        let every = "may hold of every element: decided when it runs";
        status.add(format!("<{what}> {every}"));
    }
    if closed(analysis, view, snapshot) {
        status.path("closed-predicates");
        return status;
    }
    status.add(
        "the upper bound has candidates for what the query reads: decided when it runs".to_owned(),
    );
    status
}

/// The certain answers of `prepared` as far as they can be proven, and their status.
/// Everything the request holds draws from its envelope `memory`: the bounds'
/// evaluations and retained results, the gap, the exact services' compilation scratch and
/// engines.
pub(crate) fn answer(
    store: &StoreService,
    snapshot: &Snapshot,
    prepared: &PreparedQuery,
    cancellation: &CancellationToken,
    memory: &nrese_exec::Envelope,
    mode: DlAnswers,
) -> StoreResult<Outcome> {
    cancellation.check()?;
    let analysis = analyse(prepared.query());
    // One view for the answers and their status: a commit in between changes neither.
    let decided = decided(store, snapshot, prepared, cancellation)?;
    // The exact services' one deadline, the every-name question's included: at the request's,
    // what they haven't decided is told undecided, and the checks below see only a stop.
    let read = (cancellation, deadline_of(store, prepared));
    let mut outcome = match decided.as_ref() {
        Ok(at) => decide_answers(store, prepared, &analysis, at, read, memory, mode)?,
        Err(why) => fallback(prepared, why).stream(Read::At(snapshot.clone())),
    };
    #[cfg(test)]
    crate::service::hooks::run(&crate::service::hooks::AFTER_DECIDED);
    cancellation.check()?;
    // An ASK the store answered `true` misses nothing, so nothing every name could add changes
    // it: it isn't put to the element. One answered `false` is, as is one streamed.
    let held = matches!(&outcome, Outcome::Answers(Answers::Boolean(true), ..));
    let (Outcome::Stream(status, _, detail) | Outcome::Answers(_, status, detail)) = &mut outcome;
    *status = composed(store, prepared, snapshot, &decided, status.clone());
    if status.complete
        && !held
        && let Ok((_, view)) = &decided
        && let Some(what) = every_name(&analysis, view)
        && let Some(why) = stand_in(store, view, &analysis, what, read, memory)
    {
        status.incomplete("dl", why);
    }
    // A cancellation while the element was posed is the read's, not a reason.
    cancellation.check()?;
    refuse_unless_complete(mode, status, || detail.unresolved.clone())?;
    Ok(outcome)
}

/// The one rule of exact answers: under [`DlAnswers::Exact`] an answer whose `status` isn't
/// complete is refused, with that status and the candidates left `unresolved`. Both the
/// bounds' path ([`answer`]) and the reads past the bounds, before they run
/// (`StoreService::refuse_exact`), ask it.
pub(crate) fn refuse_unless_complete(
    mode: DlAnswers,
    status: &Completeness,
    unresolved: impl FnOnce() -> Vec<String>,
) -> StoreResult<()> {
    if mode == DlAnswers::Exact && !status.complete {
        return Err(StoreError::Incomplete(Box::new(crate::IncompleteAnswer {
            status: status.clone(),
            unresolved: unresolved(),
        })));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../query_tests.rs"]
mod cancellation_tests;

#[cfg(test)]
#[path = "../envelope_tests.rs"]
mod envelope_tests;

/// How a query is answered: over the lower bound as it streams (its status known before
/// it runs), or with answers collected and completed through the bounds.
pub(crate) enum Outcome {
    /// Streamed over what [`Read`] says.
    Stream(Completeness, Read, DlDetail),
    Answers(Answers, Completeness, DlDetail),
}

/// What a streamed answer reads: the revision its status was decided on, never a later
/// one (a commit in between could add what only U1 finds).
pub(crate) enum Read {
    /// The snapshot the status was decided on, where L adds nothing to it (cacheable): for a
    /// reader of some graphs (whose status under `owl2-dl` is withheld,
    /// `status_for_its_reader`) its own view of it.
    At(Snapshot),
    /// L's view of that snapshot, with the memberships the taxonomy adds (uncached).
    Lower(Snapshot),
}

fn decide_answers(
    store: &StoreService,
    prepared: &PreparedQuery,
    analysis: &Analysis,
    (snapshot, view): &(Snapshot, Arc<View>),
    (cancellation, deadline): (&CancellationToken, Deadline),
    memory: &nrese_exec::Envelope,
    mode: DlAnswers,
) -> StoreResult<Outcome> {
    let settings = store.query_settings();
    if prepared.has_dataset() {
        // Over the decided snapshot, every graph's for this reader: the status's.
        return Ok(
            Status::sound_only(prepared.dl_dataset_reason()).stream(Read::At(snapshot.clone()))
        );
    }
    let mut status = Status::complete();
    // A version no certificate vouches for is told `not-certified` whatever its consistency
    // (`composed`): no check is spent on it.
    let consistency = match crate::service::Certificate::of(snapshot) {
        Some(_) => consistency_at(store, snapshot, view, cancellation, deadline, memory),
        None => Verdict::Unknown("not certified".to_owned()),
    };
    cancellation.check()?;
    if let Some(why) = consistency_reason(consistency) {
        status.add(why);
    }
    if crate::service::Certificate::route(snapshot, store.dl()).1 {
        status.add(USER_RULES.to_owned());
    }
    if let Some(what) = unbounded(analysis) {
        status.add(format!(
            "the query reads {what}: entailed schema statements aren't bounded (the RL \
             closure's are sound; /classification has the subsumptions under OWL 2 DL)"
        ));
        return Ok(status.stream(lower_view(view)));
    }
    if let Some(what) = untyped_same_as(prepared.query(), view) {
        status.add(format!("the query reads {what}: {DENIED}"));
        return Ok(status.stream(lower_view(view)));
    }
    // Answered over L alone; [`certify_closure`] says what that misses.
    if analysis.reflexive_same_as {
        return Ok(status.stream(lower_view(view)));
    }
    if closed(analysis, view, snapshot) {
        // One evaluation, streamed: L's answers are the certain ones.
        status.path("closed-predicates");
        return Ok(status.stream(lower_view(view)));
    }
    if closed_for_answers(analysis, view, snapshot) {
        // The same, where the gap has only individuals no answer can name.
        status.path("skolem-only-gap");
        #[cfg(debug_assertions)]
        return Ok(status.answers(check_closed_for_answers(
            view,
            prepared,
            settings,
            cancellation,
            memory,
        )?));
        #[cfg(not(debug_assertions))]
        return Ok(status.stream(lower_view(view)));
    }
    if let Some(why) = &view.unavailable {
        status.add(why.clone());
        return Ok(status.stream(lower_view(view)));
    }
    if mode == DlAnswers::Sound {
        status.add("dl.answers = sound: the lower bound alone".to_owned());
        return Ok(status.stream(lower_view(view)));
    }
    if let Some(op) = analysis.not_monotone {
        status.add(format!(
            "the query uses {op}, which isn't monotone, over predicates whose lower and upper \
             bounds differ"
        ));
        return Ok(status.stream(lower_view(view)));
    }
    if matches!(prepared.query(), Query::Construct { .. }) {
        status.add(
            "CONSTRUCT is answered over the lower bound; it is complete only where the \
             predicates it reads are closed"
                .to_owned(),
        );
        return Ok(status.stream(lower_view(view)));
    }
    use crate::query_executor::evaluate_bound;
    use nrese_sparql::TypedResults;
    let lower = evaluate_bound(&view.lower, prepared, settings, cancellation, memory)?;
    // Sound lower truth needs neither the upper evaluation nor a candidate search.
    if matches!(lower, TypedResults::Boolean(true)) {
        status.path("lower-true-ask");
        status.set_counts(lower_counts(1));
        return Ok(status.answers(lower));
    }
    // The lower result keeps its reservation in the envelope while the upper evaluates.
    let upper_snapshot = view.upper.as_ref().expect("available");
    let upper = evaluate_bound(upper_snapshot, prepared, settings, cancellation, memory)?;
    let (answers, counts, unresolved_why) = match (lower, upper) {
        (TypedResults::Boolean(false), TypedResults::Boolean(false)) => {
            (Answers::Boolean(false), lower_counts(0), None)
        }
        (TypedResults::Boolean(false), TypedResults::Boolean(true)) => {
            let candidates = vec![Vec::new()];
            let decided = exact::decide_until(
                store,
                &view.lower,
                analysis,
                &[],
                &candidates,
                cancellation,
                deadline,
                memory,
            );
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
                    status.unresolved("ASK".to_owned());
                    false
                }
            };
            decided.paths.iter().for_each(|p| status.path(p));
            (Answers::Boolean(found), counts, decided.why)
        }
        (TypedResults::Solutions(lower), TypedResults::Solutions(upper)) => {
            // The gap's work, a part of the request beside the retained bounds.
            let budget = Arc::new(memory.part());
            let mut pair = lower.align(upper, Arc::clone(&budget))?;
            // A query's candidate budget, unlimited where none is set.
            let config = &store.config().dl;
            let limit = config.max_candidates.unwrap_or(usize::MAX);
            // The labels' one deadline: a quarter of what the read has left, the selection included.
            let labels_deadline = Deadline::after(deadline.within(config.timeout).map(|t| t / 4));
            // The labels read the whole gap; where it has no room, the capped one, with no labels.
            let whole = match config.model_labels {
                true => match gap::select(&mut pair, usize::MAX, &budget, cancellation) {
                    Err(e) if e.is_memory_limit() || e.is_server_memory_limit() => None,
                    other => Some(other?),
                },
                false => None,
            };
            if config.model_labels && whole.is_none() {
                status.path("capped-gap");
            }
            let labelled = whole.is_some();
            let mut gap = match whole {
                Some(gap) => gap,
                None => gap::select(
                    &mut pair,
                    limit.saturating_add(super::UNRESOLVED_SHOWN),
                    &budget,
                    cancellation,
                )?,
            };
            let variables = pair.variables().to_vec();
            let refuted = match labelled {
                true => exact::refuted_by_labels(
                    store,
                    &view.lower,
                    analysis,
                    &variables,
                    gap.rows.len(),
                    &|i| pair.upper_row(gap.rows[i]),
                    cancellation,
                    labels_deadline,
                    memory,
                ),
                false => nrese_exec::ChargedVec::new(memory.clone()),
            };
            // The capped read's costliest batch, in bytes: no batch past its rows costs more.
            let most = |b: &[usize]| b.chunks(64).map(|c| c.iter().sum::<usize>()).max();
            let first = limit.min(gap.bytes.len());
            let prefix = most(&gap.bytes[..first]).unwrap_or(0);
            // The capped read's batches first, each as the rows the labels leave of it.
            let kept = |s| (s..first.min(s + 64)).filter(|&i| refuted.get(i) != Some(&true));
            let mut chunks: Vec<_> = (0..first).step_by(64).map(|s| kept(s).count()).collect();
            chunks.retain(|&k| k > 0);
            chunks.reverse();
            let (mut rows, mut bytes) = (refuted.iter(), refuted.iter());
            gap.rows.retain(|_| rows.next() != Some(&true));
            gap.bytes.retain(|_| bytes.next() != Some(&true));
            let by_labels = refuted.iter().filter(|&&r| r).count() as u64;
            drop(refuted);
            if labelled {
                gap.keep(limit.saturating_add(super::UNRESOLVED_SHOWN));
            }
            let left = gap.total - by_labels;
            let n = limit.min(gap.rows.len());
            let mut counts = Counts {
                lower: gap.lower,
                upper: Some(gap.lower + gap.total),
                refuted: by_labels,
                by_labels,
                ..Counts::default()
            };
            let (mut why, mut stop, mut start) = (None, None, 0);
            pair.reserve_appends(n)?;
            // Bounds, candidate production and all batches share the entry deadline.
            while start < n {
                // Past the capped read's batches: up to 64 rows within its costliest one's bytes,
                // ending the tests where the next row doesn't fit or the request has no room.
                let past = chunks.is_empty();
                let end = match chunks.pop() {
                    Some(kept) => start + kept,
                    None => (start..=n.min(start + 64))
                        .rfind(|&e| gap.bytes[start..e].iter().sum::<usize>() <= prefix)
                        .unwrap_or(start),
                };
                if end == start {
                    stop = Some(format!(
                        "the next candidate's row costs more than {prefix} bytes"
                    ));
                    break;
                }
                let mut batch_memory = nrese_exec::Reservation::new(&*budget);
                let charged = batch_memory.charge(gap.bytes[start..end].iter().sum());
                if past && let Err(e) = &charged {
                    stop = Some(Limit::Envelope.reached(e));
                    break;
                }
                charged?;
                let rows: Vec<_> = gap.rows[start..end]
                    .iter()
                    .map(|&i| pair.upper_row(i))
                    .collect();
                let decided = exact::decide_until(
                    store,
                    &view.lower,
                    analysis,
                    &variables,
                    &rows,
                    cancellation,
                    deadline,
                    memory,
                );
                decided.paths.iter().for_each(|p| status.path(p));
                if why.is_none() {
                    why = decided.why;
                }
                for ((&index, row), verdict) in
                    gap.rows[start..end].iter().zip(&rows).zip(decided.verdicts)
                {
                    match verdict {
                        Entailed::Yes => {
                            counts.proved += 1;
                            pair.append_upper(index)?;
                        }
                        Entailed::No => counts.refuted += 1,
                        Entailed::Unknown(_) => {
                            counts.unresolved += 1;
                            status.unresolved(render_row(&variables, row));
                        }
                    }
                }
                start = end;
            }
            let admitted = start;
            counts.unresolved += left.saturating_sub(admitted as u64);
            if left > admitted as u64 {
                // A stop is named beside an earlier batch's reason; without one, the limit is.
                why = match (why, stop) {
                    (Some(w), Some(s)) if w != s => Some(format!("{w}; {s}")),
                    (w, s) => w.or(s).or_else(|| Some(Limit::Candidates.reached(limit))),
                };
                for &index in &gap.rows[admitted..] {
                    if status.detail.unresolved.len() == super::UNRESOLVED_SHOWN {
                        break;
                    }
                    status.unresolved(render_row(&variables, &pair.upper_row(index)));
                }
            }
            (TypedResults::Solutions(pair.into_lower()), counts, why)
        }
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
    status.set_counts(counts);
    Ok(status.answers(answers))
}

/// A candidate row as `?x=<…> ?y=<…>` (unbound variables left out).
fn render_row(variables: &[Variable], row: &[Option<Term>]) -> String {
    let bound: Vec<String> = variables
        .iter()
        .zip(row)
        .filter_map(|(v, t)| t.as_ref().map(|t| format!("?{}={t}", v.as_str())))
        .collect();
    bound.join(" ")
}

/// The lower bound's view: the snapshot the bounds describe, with L's memberships where
/// it adds some.
fn lower_view(view: &View) -> Read {
    match view.lower_facts {
        0 => Read::At(view.lower.clone()),
        _ => Read::Lower(view.lower.clone()),
    }
}

fn lower_counts(n: u64) -> Counts {
    Counts {
        lower: n,
        upper: Some(n),
        ..Counts::default()
    }
}
