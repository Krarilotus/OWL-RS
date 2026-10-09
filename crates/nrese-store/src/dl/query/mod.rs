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
const U1: &str = "urn:nrese:u1:";

mod analysis;
mod exact;
mod gap;
use analysis::*;
pub(crate) use exact::{Ids, ground_axiom};

/// The ontology of a revision, kept for the exact services of its queries.
#[derive(Debug, Default)]
pub(crate) struct OntologyCache(std::sync::Mutex<Option<(u64, Arc<Ontology>)>>);

pub(crate) fn ontology_at(store: &StoreService, snapshot: &Snapshot) -> Arc<Ontology> {
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
fn consistency_at(
    store: &StoreService,
    snapshot: &Snapshot,
    view: &View,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Verdict {
    if cancellation.is_cancelled() {
        return Verdict::Unknown("cancelled".to_owned());
    }
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
    if Instant::now() >= deadline {
        return Verdict::Unknown("past dl.timeout".to_owned());
    }
    let cancel = nrese_dl::tableau::Cancel::from_flag(cancellation.flag());
    let ontology = ontology_at(store, snapshot);
    if cancellation.is_cancelled() {
        return Verdict::Unknown("cancelled".to_owned());
    }
    let budget = gate::remaining_budget(&gate::budget(store, Some(cancel)), deadline);
    if budget.timeout.is_zero() {
        return Verdict::Unknown("past dl.timeout".to_owned());
    }
    let checked = consistency::check(&ontology, &budget);
    // Request-local cancellation or expiry must not replace shared revision status.
    if cancellation.is_cancelled() {
        return Verdict::Unknown("cancelled".to_owned());
    }
    if Instant::now() >= deadline {
        return Verdict::Unknown("past dl.timeout".to_owned());
    }
    store.dl().record(DlStatus {
        revision: snapshot.revision(),
        consistency: checked.clone(),
    });
    checked.verdict
}

/// The bounds' counts as a query's evaluation finds them: the shared status keeps lower,
/// upper and unresolved; [`DlDetail`] the proved and refuted.
#[derive(Debug, Clone, Copy, Default)]
struct Counts {
    lower: u64,
    upper: Option<u64>,
    proved: u64,
    refuted: u64,
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
/// is closed; otherwise the bounds decide when it runs.
pub(crate) fn plan_status(store: &StoreService, prepared: &PreparedQuery) -> Completeness {
    plan_status_built(store, prepared).completeness
}

fn plan_status_built(store: &StoreService, prepared: &PreparedQuery) -> Status {
    if prepared.access().is_some() || prepared.has_dataset() {
        return Status::sound_only(
            "the query reads part of the data: not checked against the bounds",
        );
    }
    let (snapshot, view) = super::bounds::view(store);
    let analysis = analyse(prepared.query());
    if let Some(what) = unbounded(&analysis) {
        return Status::sound_only(format!(
            "the query reads {what}: entailed schema statements aren't bounded"
        ));
    }
    if closed(&analysis, &view, &snapshot) {
        let mut status = Status::complete();
        status.path("closed-predicates");
        return status;
    }
    Status::sound_only(
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
    if cancellation.is_cancelled() {
        return Err(nrese_sparql::QueryEvaluationError::Cancelled.into());
    }
    let outcome = decide_answers(store, prepared, cancellation, mode)?;
    if cancellation.is_cancelled() {
        return Err(nrese_sparql::QueryEvaluationError::Cancelled.into());
    }
    let status = match &outcome {
        Outcome::Stream(status, _, _) | Outcome::Answers(_, status, _) => status,
    };
    if mode == DlAnswers::Exact && !status.complete {
        let unresolved = match &outcome {
            Outcome::Stream(_, _, detail) | Outcome::Answers(_, _, detail) => {
                detail.unresolved.clone()
            }
        };
        return Err(StoreError::Incomplete(Box::new(crate::IncompleteAnswer {
            status: status.clone(),
            unresolved,
        })));
    }
    Ok(outcome)
}

#[cfg(test)]
#[path = "../query_tests.rs"]
mod cancellation_tests;

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
    /// The store's latest snapshot as the reader sees it: for a status decided without the
    /// bounds (sound only, whatever the revision).
    Latest,
    /// The snapshot the status was decided on, where L adds nothing to it (cacheable).
    At(Snapshot),
    /// L's view of that snapshot, with the memberships the taxonomy adds (uncached).
    Lower(Snapshot),
}

fn decide_answers(
    store: &StoreService,
    prepared: &PreparedQuery,
    cancellation: &CancellationToken,
    mode: DlAnswers,
) -> StoreResult<Outcome> {
    let deadline = Instant::now() + store.config().dl.timeout;
    let settings = store.query_settings();
    if prepared.access().is_some() {
        return Ok(Status::sound_only(
            "the reader sees part of the data: answers are checked against the bounds only \
             for readers of every graph",
        )
        .stream(Read::Latest));
    }
    if prepared.has_dataset() {
        return Ok(Status::sound_only(
            "the query names its dataset: answers are over those graphs, not checked \
             against the bounds",
        )
        .stream(Read::Latest));
    }
    let (snapshot, view) = super::bounds::view(store);
    let analysis = analyse(prepared.query());
    let mut status = Status::complete();
    let consistency = consistency_at(store, &snapshot, &view, cancellation, deadline);
    if cancellation.is_cancelled() {
        return Err(nrese_sparql::QueryEvaluationError::Cancelled.into());
    }
    match consistency {
        Verdict::Consistent => {}
        Verdict::Inconsistent => status
            .add("the data is inconsistent under OWL 2 DL, so it entails every answer".to_owned()),
        Verdict::Unknown(why) => status.add(format!(
            "the data's consistency under OWL 2 DL isn't known ({why})"
        )),
    }
    if store.dl().user_rules() {
        // The rules run over the RL closure only (DL-safe in effect: their variables bind
        // to the store's terms), never over what OWL 2 DL entails beyond it nor over U1:
        // a conclusion they'd draw from such a fact is missing from both bounds.
        status.add(
            "user rules run with OWL 2 DL: their conclusions from what only OWL 2 DL entails \
             aren't computed, so no answer is claimed complete"
                .to_owned(),
        );
    }
    if let Some(what) = unbounded(&analysis) {
        status.add(format!(
            "the query reads {what}: entailed schema statements aren't bounded (the RL \
             closure's are sound; /classification has the subsumptions under OWL 2 DL)"
        ));
        return Ok(status.stream(lower_view(&view)));
    }
    if closed(&analysis, &view, &snapshot) {
        // One evaluation, streamed: L's answers are the certain ones.
        status.path("closed-predicates");
        return Ok(status.stream(lower_view(&view)));
    }
    if closed_for_answers(&analysis, &view, &snapshot) {
        // The same, where the gap has only individuals no answer can name.
        status.path("skolem-only-gap");
        #[cfg(debug_assertions)]
        return Ok(status.answers(check_closed_for_answers(
            &view,
            prepared,
            settings,
            cancellation,
        )?));
        #[cfg(not(debug_assertions))]
        return Ok(status.stream(lower_view(&view)));
    }
    if let Some(why) = &view.unavailable {
        status.add(why.clone());
        return Ok(status.stream(lower_view(&view)));
    }
    if mode == DlAnswers::Sound {
        status.add("dl.answers = sound: the lower bound alone".to_owned());
        return Ok(status.stream(lower_view(&view)));
    }
    if let Some(op) = analysis.not_monotone {
        status.add(format!(
            "the query uses {op}, which isn't monotone, over predicates whose lower and upper \
             bounds differ"
        ));
        return Ok(status.stream(lower_view(&view)));
    }
    if matches!(prepared.query(), Query::Construct { .. }) {
        status.add(
            "CONSTRUCT is answered over the lower bound; it is complete only where the \
             predicates it reads are closed"
                .to_owned(),
        );
        return Ok(status.stream(lower_view(&view)));
    }
    use crate::query_executor::{bound_budget, evaluate_bound};
    use nrese_sparql::TypedResults;
    let lower = evaluate_bound(&view.lower, prepared, settings, cancellation, 0)?;
    // Sound lower truth needs neither the upper evaluation nor a candidate search.
    if matches!(lower, TypedResults::Boolean(true)) {
        status.path("lower-true-ask");
        status.set_counts(lower_counts(1));
        return Ok(status.answers(lower));
    }
    let retained = match &lower {
        TypedResults::Solutions(rows) => rows.reserved_bytes(),
        _ => 0,
    };
    let upper_snapshot = view.upper.as_ref().expect("available");
    let upper = evaluate_bound(upper_snapshot, prepared, settings, cancellation, retained)?;
    let (answers, counts, unresolved_why) = match (lower, upper) {
        (TypedResults::Boolean(false), TypedResults::Boolean(false)) => {
            (Answers::Boolean(false), lower_counts(0), None)
        }
        (TypedResults::Boolean(false), TypedResults::Boolean(true)) => {
            let candidates = vec![Vec::new()];
            let decided = exact::decide_until(
                store,
                &view.lower,
                &analysis,
                &[],
                &candidates,
                cancellation,
                deadline,
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
            let budget = bound_budget(
                prepared,
                settings,
                retained.saturating_add(upper.reserved_bytes()),
            );
            let mut pair = lower.align(upper, Arc::clone(&budget))?;
            let limit = store.config().dl.max_candidates;
            let gap = gap::select(
                &mut pair,
                limit.saturating_add(super::UNRESOLVED_SHOWN),
                &budget,
                cancellation,
            )?;
            let mut counts = Counts {
                lower: gap.lower,
                upper: Some(gap.lower + gap.total),
                unresolved: gap.total.saturating_sub(limit as u64),
                ..Counts::default()
            };
            let variables = pair.variables().to_vec();
            let mut why = None;
            let admitted = limit.min(gap.rows.len());
            // Bounds, candidate production and all batches share the entry deadline.
            for start in (0..admitted).step_by(64) {
                let end = (start + 64).min(admitted);
                let mut batch_memory = gap::Scratch::new(&budget);
                batch_memory.charge(gap.bytes[start..end].iter().sum())?;
                let rows: Vec<_> = gap.rows[start..end]
                    .iter()
                    .map(|&i| pair.upper_row(i))
                    .collect();
                let decided = exact::decide_until(
                    store,
                    &view.lower,
                    &analysis,
                    &variables,
                    &rows,
                    cancellation,
                    deadline,
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
            }
            if gap.total > limit as u64 {
                why.get_or_insert_with(|| format!("past dl.max_candidates ({limit})"));
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
