//! Classification and realisation of the store's asserted ontology by the DL engines
//! (docs/design/owl2-dl.md §7; `nrese_dl::classify`): the context core where its Horn
//! stage takes the ontology, else the hypertableau driver (known and possible subsumers,
//! parallel tests). Each result says whether it is complete; what it contains is entailed.
//!
//! Complete results are kept per revision. An incomplete explicit request is retried
//! on the next call under the configured budget, including unsupported ontologies.
//! The query lower bound owns its separate schema-lifetime cache (`bounds.rs`).
//! The context core receives the store's explicit process-memory policy, including zero
//! to disable its watch; opening another store cannot change that policy.
//!
//! Where an irregular role hierarchy is all the DL engines leave out, the EL classifier
//! may answer instead ([`el_answer`]); its complete answers are kept per revision too.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use nrese_dl::classify::{self, Realisation, Taxonomy};
use nrese_dl::verdict::Verdict;
use nrese_engine::{ReadModel, Snapshot, TermId, TermKind};
use nrese_exec::cell::{BuildCell, Busy, Claim, Kind};
use nrese_owl::{Axiom, Characteristic, Ontology, Term, Unsupported};
use nrese_rdf::{LiteralRef, NamedNodeRef, TermRef};
use nrese_reasoner::classify::Classification;
use nrese_reasoner::eval::Stop;

use super::source;
use crate::StoreService;

/// The latest complete results, by revision. Kept, not charged to any envelope: each is
/// one revision's answer, replaced by the next.
#[derive(Debug, Default)]
pub(crate) struct Cache {
    taxonomy: Slot<Taxonomy>,
    realisation: Slot<Realisation>,
    /// The EL classifier's answer ([`el_answer`]), complete: a request on the same revision
    /// runs neither the DL engines nor the EL classifier again. Simultaneous first requests
    /// run the DL engines once ([`cached`]) and the EL classifier each.
    el: Slot<El>,
}

/// A revision's latest complete result, and the computations under way by revision: one
/// at a time per revision, its complete result handed to the requests waiting for it.
struct Slot<T> {
    kept: Mutex<Option<(u64, Arc<T>)>>,
    building: BuildCell<u64, T>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            kept: Mutex::new(None),
            building: BuildCell::default(),
        }
    }
}

impl<T> std::fmt::Debug for Slot<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kept = self.kept.lock().unwrap_or_else(|p| p.into_inner());
        let revision = kept.as_ref().map(|(at, _)| *at);
        f.debug_struct("Slot")
            .field("kept", &revision)
            .finish_non_exhaustive()
    }
}

/// How the engines run under the store's settings. A classification is the store's work,
/// shared per revision, not a request's: its memory is an envelope of its own within the
/// catalog's budget, which every test and the context core draw from.
pub(super) fn options(store: &StoreService) -> classify::Options {
    let config = &store.config().dl;
    let workers = nrese_exec::workers::Workers::current().limited(config.threads);
    let mut options = config.classification_options(workers, Some(store.envelope(None)));
    // A limit that can't be found runs no engine ([`run`]); here it leaves none room.
    options.max_memory = Some(store.process_memory_bytes().unwrap_or(1));
    options
}

/// The taxonomy of `snapshot`'s asserted ontology (from the cache if it is current,
/// or from a current realisation); `None` if `stop` fired while it waited ([`cached`]).
pub(crate) fn taxonomy(
    store: &StoreService,
    snapshot: &Snapshot,
    stop: Stop<'_>,
) -> Option<Arc<Taxonomy>> {
    let cache = &store.dl().classification;
    let revision = snapshot.revision();
    cached(
        &cache.taxonomy,
        revision,
        stop,
        Taxonomy::complete,
        || match latest(&cache.realisation, revision) {
            Some(realisation) => realisation.taxonomy.clone(),
            None => {
                let taxonomy = run(
                    store,
                    snapshot,
                    classify::classify,
                    Taxonomy::refused,
                    |t, why| {
                        t.classification.consistency =
                            nrese_dl::verdict::Verdict::Unknown(why.clone());
                        t.incomplete.push(why)
                    },
                );
                // What the shared workers freed is offered back before the answer is built
                // from the taxonomy ([`nrese_engine::memory::release_idle`]).
                nrese_engine::memory::release_idle();
                taxonomy
            }
        },
    )
}

/// The realisation of `snapshot`'s asserted ontology (from the cache if it is current);
/// `None` if `stop` fired while it waited ([`cached`]).
pub(crate) fn realisation(
    store: &StoreService,
    snapshot: &Snapshot,
    stop: Stop<'_>,
) -> Option<Arc<Realisation>> {
    let complete = |r: &Realisation| r.incomplete.is_empty() && r.taxonomy.complete();
    cached(
        &store.dl().classification.realisation,
        snapshot.revision(),
        stop,
        complete,
        || {
            run(
                store,
                snapshot,
                classify::realise,
                Realisation::refused,
                |r, why| {
                    r.taxonomy.classification.consistency =
                        nrese_dl::verdict::Verdict::Unknown(why.clone());
                    r.taxonomy.incomplete.push(why.clone());
                    r.incomplete.push(why)
                },
            )
        },
    )
}

/// The cached result at `revision`, else the computation under way for it, else
/// `compute`'s, kept if `complete`. A request that finds a computation under way waits for
/// it (`dl/bounds.rs`'s way) and gets its result if complete; where it ends incomplete or
/// panics, the request claims again (one replacement, the others waiting for it), and on a
/// Rayon worker (which must not wait for it) it computes its own. Publishing never lets an older in-flight request evict a newer
/// revision; same-revision races keep the first complete result. No solver runs locked.
/// `None`: `stop` fired while the request waited; the computation goes on and is kept.
fn cached<T>(
    slot: &Slot<T>,
    revision: u64,
    stop: Stop<'_>,
    complete: impl Fn(&T) -> bool,
    compute: impl FnOnce() -> T,
) -> Option<Arc<T>> {
    if let Some(result) = latest(slot, revision) {
        return Some(result);
    }
    let own = || {
        let result = Arc::new(compute());
        if complete(&result) {
            keep(slot, revision, &result);
        }
        result
    };
    let kept = || latest(slot, revision).map(Some);
    loop {
        if stop() {
            return None;
        }
        match slot.building.claim(revision, Kind::Opaque, kept) {
            Claim::Build(builder) => {
                let result = own();
                if complete(&result) {
                    builder.finish(Some(Arc::clone(&result)));
                }
                return Some(result);
            }
            Claim::Wait(build) => match build.wait_until(Some(stop)) {
                Some(Ok(Some(result))) => return Some(result),
                Some(Ok(None)) => {} // nothing shared: claim again, one replacement for all
                Some(Err(Busy)) => return Some(own()),
                None => return None,
            },
        }
    }
}

/// Keeps `result` as `revision`'s unless the slot holds a newer revision's or one already.
fn keep<T>(slot: &Slot<T>, revision: u64, result: &Arc<T>) {
    let mut slot = slot.kept.lock().unwrap_or_else(|p| p.into_inner());
    if slot.as_ref().is_none_or(|(at, _)| *at < revision) {
        *slot = Some((revision, Arc::clone(result)));
    }
}

fn latest<T>(slot: &Slot<T>, revision: u64) -> Option<Arc<T>> {
    match &*slot.kept.lock().unwrap_or_else(|p| p.into_inner()) {
        Some((at, result)) if *at == revision => Some(Arc::clone(result)),
        _ => None,
    }
}

/// `engine` over `snapshot`'s asserted ontology, within one timeout that includes
/// reading the ontology.
/// Its reading of the ontology draws on the run's envelope too (SYN-61); where that can't
/// take it, the engine's result over no axioms with its consistency unknown, `incomplete`
/// saying why (never kept: a result kept is complete). Over no axioms the engine would say
/// consistent, which nothing read decided.
/// It offers back nothing the shared workers freed: only a classification does, once this
/// returns ([`taxonomy`]): its peak is the one the release fixes; whether the other
/// operations need one is measured separately.
fn run<T>(
    store: &StoreService,
    snapshot: &Snapshot,
    engine: fn(&nrese_owl::Ontology, &classify::Options) -> T,
    refused: fn(String) -> T,
    incomplete: fn(&mut T, String),
) -> T {
    let started = Instant::now();
    // The process's limit that can't be found (its memory unread) holds no engine to it:
    // none runs, and the answer says why (never kept: fail closed).
    if let Err(unread) = store.process_memory_bytes() {
        return refused(format!(
            "the process's memory limit couldn't be found ({unread})"
        ));
    }
    let mut options = options(store);
    let memory = options.tableau.memory.clone().unwrap_or_default();
    let read = source::read_snapshot(snapshot, memory);
    options.timeout = options
        .timeout
        .map(|limit| limit.saturating_sub(started.elapsed()));
    match read {
        Ok(ontology) => engine(&ontology, &options),
        Err(_) => {
            let mut none = engine(&nrese_owl::Ontology::default(), &options);
            incomplete(&mut none, source::refused("the revision"));
            none
        }
    }
}

/// Whether the EL classifier's completion rules are complete for `o`, given that it reads
/// all of it: no assertion (the ontology then has a model iff `owl:Thing` is satisfiable),
/// and no object-property range on a property of a chain or a transitivity, or above or
/// below one through the property hierarchy. The classifier applies a range on the edges
/// class axioms create, not on those a chain composes; the EL++ update's condition on the
/// ranges above a chain's superproperty is taken conservatively, refusing every such range.
/// O(axioms + property inclusions).
fn el_complete_for(o: &Ontology) -> bool {
    if o.axioms.iter().any(Axiom::is_assertion) {
        return false;
    }
    let mut chained: Vec<Term> = Vec::new();
    let mut ranged: Vec<Term> = Vec::new();
    // Property inclusions by named property (inverses merged: wider, so safer), both ways.
    let mut up: HashMap<Term, Vec<Term>> = HashMap::new();
    let mut down: HashMap<Term, Vec<Term>> = HashMap::new();
    let mut include = |r: Term, s: Term| {
        up.entry(r).or_default().push(s);
        down.entry(s).or_default().push(r);
    };
    for axiom in &o.axioms {
        match axiom {
            Axiom::SubObjectPropertyOf(chain, sup) if chain.len() > 1 => {
                chained.extend(chain.iter().chain([sup]).map(|p| p.named()));
            }
            Axiom::SubObjectPropertyOf(chain, sup) => {
                for r in chain {
                    include(r.named(), sup.named());
                }
            }
            Axiom::EquivalentObjectProperties(ps) => {
                for pair in ps.windows(2) {
                    include(pair[0].named(), pair[1].named());
                    include(pair[1].named(), pair[0].named());
                }
            }
            Axiom::ObjectCharacteristic(Characteristic::Transitive, p) => chained.push(p.named()),
            Axiom::ObjectPropertyRange(p, _) => ranged.push(p.named()),
            _ => {}
        }
    }
    if chained.is_empty() || ranged.is_empty() {
        return true;
    }
    // Each direction walks on its own: a property the upward walk reached (an equivalent)
    // still leads the downward walk on to what is below it.
    let mut related: HashSet<Term> = HashSet::new();
    for edges in [&up, &down] {
        let mut seen: HashSet<Term> = chained.iter().copied().collect();
        let mut todo = chained.clone();
        while let Some(p) = todo.pop() {
            for &q in edges.get(&p).into_iter().flatten() {
                if seen.insert(q) {
                    todo.push(q);
                }
            }
        }
        related.extend(seen);
    }
    !ranged.iter().any(|p| related.contains(p))
}

/// The EL classifier's vocabulary in a snapshot's dictionary. A term the store doesn't hold
/// gets an id of its own, once per term, counted down from `u64::MAX`: the top four bits
/// all set are no `TermKind`'s tag, so no stored term has such an id.
struct Lookup<'a> {
    snapshot: &'a Snapshot,
    missing: HashMap<String, u64>,
    next: u64,
}

impl Lookup<'_> {
    fn id(&mut self, term: TermRef<'_>) -> u64 {
        if let Some(id) = self.snapshot.lookup(term) {
            return id.raw();
        }
        let next = &mut self.next;
        *self.missing.entry(term.to_string()).or_insert_with(|| {
            *next -= 1;
            *next
        })
    }

    /// An id no term has.
    fn fresh(&mut self) -> u64 {
        self.next -= 1;
        self.next
    }
}

impl nrese_reasoner::ir::Vocabulary for Lookup<'_> {
    fn iri(&mut self, iri: &str) -> u64 {
        self.id(NamedNodeRef::new_unchecked(iri).into())
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        self.id(
            LiteralRef::new_typed_literal(lexical, NamedNodeRef::new_unchecked(datatype)).into(),
        )
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        self.id(LiteralRef::new_language_tagged_literal_unchecked(lexical, language).into())
    }
}

/// The EL classifier's answer over a snapshot's term ids, with the consistency it decides.
#[derive(Debug)]
pub(crate) struct El {
    pub classification: Classification,
    pub consistency: Verdict,
    /// Why subsumptions may be missing: an object-property range, whose completeness the
    /// cited proof doesn't cover ([`el_outside`]).
    pub unproven: Option<String>,
}

/// What the EL classifier's answers say about themselves, outside OWL 2 DL: the input's
/// irregular role hierarchy, and the texts of what they guarantee and what not. The proof
/// cited is the EL++ calculus's (Baader, Brandt and Lutz, "Pushing the EL Envelope", IJCAI
/// 2005, Lemma 3 and Theorem 4): arbitrary role inclusions, no ranges.
pub(crate) fn el_outside() -> nrese_sparql::Outside {
    nrese_sparql::Outside {
        what: vec![Unsupported::IrregularRoleHierarchy.as_str()],
        by: "el",
        guaranteed: "Every subsumption listed holds under the OWL 2 Direct Semantics. Where \
             the input states no object-property range, none is missing (the EL++ completion \
             rules, Baader, Brandt and Lutz 2005, Lemma 3). These rules need no regular role \
             hierarchy.",
        not_guaranteed: "That other OWL 2 DL reasoners give this answer: the input breaks \
             OWL 2 DL's global restrictions; reasoners that check them refuse it. Completeness \
             where the input states an object-property range. The OWL 2 RDF-Based Semantics. \
             Realisation, which still answers only the DL engines' lower bound.",
    }
}

/// What the EL route gave where it was taken ([`el_answer`]).
pub(crate) enum ElRoute {
    /// Its complete answer (kept for the snapshot's revision).
    Answered(Arc<El>),
    /// Stopped before its end (nothing of the run is used): the DL engines'
    /// answer stands, marked with why. The store's memory error where memory stopped it;
    /// `None`: `dl.timeout` passed.
    Stopped(Option<crate::StoreError>),
}

/// The EL classifier's answer kept for `snapshot`'s revision, if there is one and the store
/// answers outside OWL 2 DL by a proven procedure now: a change of that setting never
/// serves the other value's answer.
pub(crate) fn kept_el(store: &StoreService, snapshot: &Snapshot) -> Option<Arc<El>> {
    if store.outside_profile() != crate::dl::OutsideProfile::Proven {
        return None;
    }
    latest(&store.dl().classification.el, snapshot.revision())
}

/// The EL classifier's classification of `snapshot`'s asserted statements, where `taxonomy`
/// (the DL engines') left out an irregular role hierarchy and nothing else, and the EL
/// classifier is complete for the ontology: no assertion, no range with a chain
/// ([`el_complete_for`]), nothing it skipped. Its completion rules (CR6) need no regular
/// hierarchy. Taken only where the store answers outside OWL 2 DL by a proven procedure
/// ([`crate::dl::OutsideProfile::Proven`], the default); `None` everywhere else, where
/// the DL engines' answer stands as it is. It runs under the request's stop:
/// `started` plus `dl.timeout`, and the store's memory watch with the process's limit
/// ([`StoreService::memory_stop`]), polled as `nrese_reasoner::classify::classify_parallel_within`
/// says; stopped, or with that time already passed before it would start,
/// [`ElRoute::Stopped`].
///
/// Its consistency: with no assertion, `owl:Thing`'s satisfiability, read from a fresh class
/// that no axiom names (unsatisfiable iff `owl:Thing` is), left out of the answer.
///
/// The triples it reads are charged to the classification's envelope; the classifier's
/// own structures are bounded by the stop's memory watch, a soft bound (seen at the next
/// poll, not refused in advance).
pub(crate) fn el_answer(
    store: &StoreService,
    snapshot: &Snapshot,
    taxonomy: &Taxonomy,
    started: Instant,
) -> Option<ElRoute> {
    // Outside OWL 2 DL: only where the store answers by a proven procedure.
    if store.outside_profile() != crate::dl::OutsideProfile::Proven {
        return None;
    }
    // The driver's lower bound, where an irregular role hierarchy is all it left out.
    if !taxonomy.left_out.only(Unsupported::IrregularRoleHierarchy) {
        return None;
    }
    let options = options(store);
    let memory = options.tableau.memory.clone().unwrap_or_default();
    let ontology = source::read_snapshot(snapshot, memory.clone()).ok()?;
    if !el_complete_for(&ontology) {
        return None;
    }
    // An object-property range: the answer is marked incomplete, naming the first.
    let unproven = ontology.axioms.iter().find_map(|axiom| match axiom {
        Axiom::ObjectPropertyRange(p, _) => {
            let property = snapshot.decode(TermId::from_raw(p.named()));
            let name = property.map_or_else(|| "a property".to_owned(), |t| t.to_string());
            Some(format!(
                "the input states an object-property range (on {name}): the EL classifier's \
                 completeness with ranges is not proven"
            ))
        }
        _ => None,
    });
    drop(ontology);
    let deadline = options.timeout.map(|limit| started + limit);
    let memory_stop = store.memory_stop();
    let stop = || deadline.is_some_and(|at| Instant::now() >= at) || memory_stop.exceeded();
    let room = || memory_stop.room();
    if stop() {
        let interrupted = nrese_reasoner::delta::Interrupted::default();
        return Some(ElRoute::Stopped(memory_stop.error(interrupted)));
    }
    let mut triples = nrese_exec::ChargedVec::new(memory);
    for q in snapshot.quads_for_pattern_in(ReadModel::Asserted, &source::ANY) {
        triples
            .push([q.subject.raw(), q.predicate.raw(), q.object.raw()])
            .ok()?;
    }
    let mut vocabulary = Lookup {
        snapshot,
        missing: HashMap::new(),
        next: u64::MAX,
    };
    use nrese_reasoner::ir::Vocabulary as _;
    let probe = vocabulary.fresh();
    let rdf_type = vocabulary.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let class = vocabulary.iri("http://www.w3.org/2002/07/owl#Class");
    triples.push([probe, rdf_type, class]).ok()?;
    // The DL engines' classes, declared: the EL classifier classifies the classes of what it
    // reads, and one named only where it reads nothing (a data property's domain) would
    // be missing from its answer, with its subsumers among `owl:Thing`'s equivalents.
    for &c in &taxonomy.classification.classes {
        triples.push([c, rdf_type, class]).ok()?;
    }
    triples.sort_unstable();
    let mut kept = 0;
    for i in 0..triples.len() {
        if kept == 0 || triples[i] != triples[kept - 1] {
            triples[kept] = triples[i];
            kept += 1;
        }
    }
    triples.truncate(kept);
    let named = |id: u64| id == probe || TermId::from_raw(id).kind() == TermKind::Iri;
    let operation = nrese_reasoner::eval::OperationStop::new(&stop).within(&room);
    let run = nrese_reasoner::classify::classify_parallel_within(
        &triples,
        &mut vocabulary,
        &named,
        options.workers.width(),
        &operation,
    );
    drop(triples);
    let mut classification = match run {
        Ok(classification) => classification,
        Err(interrupted) => return Some(ElRoute::Stopped(memory_stop.error(interrupted))),
    };
    if !classification.skipped.is_empty() {
        return None;
    }
    let consistency = match classification.unsatisfiable.contains(&probe) {
        true => Verdict::Inconsistent,
        false => Verdict::Consistent,
    };
    classification.unsatisfiable.retain(|&c| c != probe);
    classification.top.retain(|&c| c != probe);
    classification
        .subsumptions
        .retain(|&(a, b)| a != probe && b != probe);
    // A class the DL engines' signature lacks (one in the class axiom on an axiom's
    // reification, `owl:Axiom`): the two readers read the ontology differently, so neither
    // reading is trusted, and the DL engines' lower bound stands.
    let signature: HashSet<u64> = taxonomy.classification.classes.iter().copied().collect();
    let known = |c: &u64| signature.contains(c);
    let c = &classification;
    if !(c.unsatisfiable.iter().all(known)
        && c.top.iter().all(known)
        && c.subsumptions.iter().all(|(a, b)| known(a) && known(b)))
    {
        return None;
    }
    let el = Arc::new(El {
        classification,
        consistency,
        unproven,
    });
    keep(&store.dl().classification.el, snapshot.revision(), &el);
    Some(ElRoute::Answered(el))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DlConfig, StoreConfig};
    use nrese_reasoner::eval::NEVER;
    use std::time::Duration;

    // The tests' requests never give up, except the one test about giving up.
    fn taxonomy(store: &StoreService, snapshot: &Snapshot) -> Arc<Taxonomy> {
        super::taxonomy(store, snapshot, NEVER).unwrap()
    }
    fn realisation(store: &StoreService, snapshot: &Snapshot) -> Arc<Realisation> {
        super::realisation(store, snapshot, NEVER).unwrap()
    }
    fn cached<T>(s: &Slot<T>, r: u64, c: impl Fn(&T) -> bool, f: impl FnOnce() -> T) -> Arc<T> {
        super::cached(s, r, NEVER, c, f).unwrap()
    }

    fn store(timeout: Duration) -> StoreService {
        let store = StoreService::new(StoreConfig {
            process_memory_bytes: crate::ProcessMemory::Bytes(0),
            dl: DlConfig {
                timeout: Some(timeout),
                threads: 1,
                ..DlConfig::default()
            },
            ..StoreConfig::in_memory()
        })
        .unwrap();
        store
            .update_str_ungated(
                "PREFIX : <urn:cache:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
             PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA { :A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] .
                 :B rdfs:subClassOf :D . :C rdfs:subClassOf :D . :a a :A . }",
            )
            .unwrap();
        store
    }

    /// The irregular chain of the store's tests (`tests/it/dl/classification.rs`), with
    /// nothing else: the EL route's input.
    const IRREGULAR: &str = "PREFIX : <urn:el:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
        INSERT DATA { :r3 owl:propertyChainAxiom ( :r1 :r2 ) . :r3 rdfs:subPropertyOf :r1 .
            :r1 a owl:ObjectProperty . :r2 a owl:ObjectProperty . :r3 a owl:ObjectProperty .
            :A a owl:Class . :B a owl:Class . :C a owl:Class . :D a owl:Class .
            :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r1 ; owl:someValuesFrom :B ] .
            :B rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r2 ; owl:someValuesFrom :C ] .
            [ a owl:Restriction ; owl:onProperty :r3 ; owl:someValuesFrom :C ] rdfs:subClassOf :D . }";

    fn el_config(timeout: Duration) -> StoreConfig {
        StoreConfig {
            process_memory_bytes: crate::ProcessMemory::Bytes(0),
            dl: DlConfig {
                timeout: Some(timeout),
                threads: 1,
                outside_profile: crate::dl::OutsideProfile::Proven,
                ..DlConfig::default()
            },
            ..StoreConfig::in_memory()
        }
    }

    /// An EL run answers in time and is kept: a later request on the revision gets that
    /// answer. One stopped (by a `dl.timeout` of 1 ns, past before the run starts; by the
    /// memory unread) answers no classification and keeps nothing, and a memory stop is told
    /// apart from the deadline by the store's memory error. (The stopped stores get the first
    /// one's taxonomy: their own DL engines would stop too.)
    #[test]
    fn a_stopped_el_run_answers_nothing_and_keeps_nothing() {
        let store = StoreService::new(el_config(Duration::from_secs(3600))).unwrap();
        store.update_str_ungated(IRREGULAR).unwrap();
        let snapshot = store.engine().snapshot();
        let taxonomy = taxonomy(&store, &snapshot);
        assert!(
            taxonomy
                .left_out
                .only(nrese_owl::Unsupported::IrregularRoleHierarchy),
            "{:?}",
            taxonomy.incomplete
        );
        assert!(kept_el(&store, &snapshot).is_none());
        let Some(ElRoute::Answered(el)) = el_answer(&store, &snapshot, &taxonomy, Instant::now())
        else {
            panic!("the EL route didn't answer in time");
        };
        let kept = kept_el(&store, &snapshot).expect("an answer kept for the revision");
        assert!(Arc::ptr_eq(&el, &kept));

        let late = StoreService::new(el_config(Duration::from_nanos(1))).unwrap();
        late.update_str_ungated(IRREGULAR).unwrap();
        let snapshot = late.engine().snapshot();
        assert!(matches!(
            el_answer(&late, &snapshot, &taxonomy, Instant::now()),
            Some(ElRoute::Stopped(None))
        ));
        assert!(kept_el(&late, &snapshot).is_none());

        fn unread() -> nrese_exec::memory::Reading {
            Err(nrese_exec::memory::Unread::Failed)
        }
        // The process limit by default (not off, as `el_config` sets it): found by the
        // capacity reader, which can't read.
        let unreadable = StoreService::with_runtime(
            StoreConfig {
                process_memory_bytes: crate::ProcessMemory::Default,
                ..el_config(Duration::from_secs(3600))
            },
            Arc::new(crate::Runtime::new(0, 0)),
        )
        .unwrap()
        .with_capacity(unread);
        unreadable.update_str_ungated(IRREGULAR).unwrap();
        let snapshot = unreadable.engine().snapshot();
        let stopped = el_answer(&unreadable, &snapshot, &taxonomy, Instant::now());
        assert!(
            matches!(
                stopped,
                Some(ElRoute::Stopped(Some(
                    crate::StoreError::MemoryUnreadable { .. }
                )))
            ),
            "the memory stop isn't told apart"
        );
        assert!(kept_el(&unreadable, &snapshot).is_none());
    }

    /// D2, A-902: where the process's limit can't be found (its memory unread), no engine
    /// runs, even on a runtime of no limit supplied by the caller: the taxonomy and the
    /// realisation say why and leave consistency unknown, and neither is kept.
    #[test]
    fn a_classification_whose_limit_cant_be_found_runs_no_engine() {
        fn unread() -> nrese_exec::memory::Reading {
            Err(nrese_exec::memory::Unread::Failed)
        }
        let store = StoreService::with_runtime(
            StoreConfig::in_memory(),
            Arc::new(crate::Runtime::new(0, 0)),
        )
        .unwrap()
        .with_capacity(unread);
        store
            .update_str_ungated(
                "PREFIX : <urn:cache:> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
                 INSERT DATA { :A rdfs:subClassOf :B . :a a :A . }",
            )
            .unwrap();
        let snapshot = store.engine().snapshot();
        fn no_engine<T>(_: &nrese_owl::Ontology, _: &classify::Options) -> T {
            panic!("an engine ran after the limit couldn't be found")
        }
        let ignored = |_: &mut Taxonomy, _: String| {};
        run(&store, &snapshot, no_engine, Taxonomy::refused, ignored);
        run(
            &store,
            &snapshot,
            no_engine,
            Realisation::refused,
            |_, _| {},
        );
        let taxonomy = taxonomy(&store, &snapshot);
        let realisation = realisation(&store, &snapshot);
        for (consistency, incomplete) in [
            (&taxonomy.classification.consistency, &taxonomy.incomplete),
            (
                &realisation.taxonomy.classification.consistency,
                &realisation.incomplete,
            ),
        ] {
            assert!(
                matches!(consistency, nrese_dl::verdict::Verdict::Unknown(_)),
                "{consistency:?}"
            );
            assert!(
                incomplete.iter().any(|why| why.contains("memory limit")),
                "{incomplete:?}"
            );
        }
        let cache = &store.dl().classification;
        assert!(latest(&cache.taxonomy, snapshot.revision()).is_none());
        assert!(latest(&cache.realisation, snapshot.revision()).is_none());
    }

    /// A classification whose ontology the run's memory can't read reports its consistency
    /// unknown, never consistent (SYN-61): over the empty ontology the engines are left with,
    /// consistent would be their answer, which nothing read decided.
    #[test]
    fn a_classification_whose_ontology_does_not_fit_is_never_consistent() {
        let store = StoreService::new(StoreConfig {
            total_query_memory_bytes: 1 << 30,
            ..StoreConfig::in_memory()
        })
        .unwrap();
        let mut data = String::from(
            "PREFIX : <urn:cache:> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
             INSERT DATA { :A rdfs:subClassOf :B .",
        );
        for i in 0..2_000 {
            data.push_str(&format!(" :a{i} a :A ."));
        }
        data.push('}');
        store.update_str_ungated(&data).unwrap();
        let snapshot = store.engine().snapshot();
        // All but 4 KiB of the queries' pool held: the 2,000 statements don't fit.
        let pool = store.runtime().query_memory().expect("the queries' pool");
        let mut hold = nrese_exec::Reservation::new(Arc::new(store.envelope(None).part()));
        hold.charge(pool.limit() - pool.used() - 4096).unwrap();
        let verdicts = [
            taxonomy(&store, &snapshot)
                .classification
                .consistency
                .clone(),
            realisation(&store, &snapshot)
                .taxonomy
                .classification
                .consistency
                .clone(),
        ];
        for verdict in verdicts {
            assert!(
                matches!(verdict, nrese_dl::verdict::Verdict::Unknown(_)),
                "{verdict:?}"
            );
        }
    }

    #[test]
    fn incomplete_explicit_results_are_retried() {
        // The public configuration requires a positive timeout. One nanosecond is
        // spent before the first solver entry, so both attempts report incomplete.
        let store = store(Duration::from_nanos(1));
        let snapshot = store.engine().snapshot();
        let first = taxonomy(&store, &snapshot);
        let next = taxonomy(&store, &snapshot);
        assert!(!first.complete());
        assert!(!next.complete());
        assert!(!Arc::ptr_eq(&first, &next), "classification must retry");
        let first = realisation(&store, &snapshot);
        let next = realisation(&store, &snapshot);
        assert!(!first.incomplete.is_empty());
        assert!(!next.incomplete.is_empty());
        assert!(!Arc::ptr_eq(&first, &next), "realisation must retry");
    }

    #[test]
    fn complete_results_are_reused_and_old_requests_do_not_evict_them() {
        let store = store(Duration::from_secs(30));
        let old = store.engine().snapshot();
        store
            .update_str_ungated("INSERT DATA { <urn:cache:b> a <urn:cache:A> }")
            .unwrap();
        let current = store.engine().snapshot();
        assert!(old.revision() < current.revision());
        let classes = taxonomy(&store, &current);
        let types = realisation(&store, &current);
        assert!(classes.complete(), "{:?}", classes.incomplete);
        assert!(types.incomplete.is_empty(), "{:?}", types.incomplete);
        // An older request finishing later still receives its own snapshot's answer.
        assert!(taxonomy(&store, &old).complete());
        let old_types = realisation(&store, &old);
        assert!(old_types.incomplete.is_empty());
        assert!(old_types.individuals.len() < types.individuals.len());
        assert!(Arc::ptr_eq(&classes, &taxonomy(&store, &current)));
        assert!(Arc::ptr_eq(&types, &realisation(&store, &current)));
    }

    /// Two requests on one revision while nothing is kept run one computation: the second
    /// gets the first's result (D16-655: a client asking again after a 408 started a second
    /// classification beside the first). Counted, not timed: the first computation holds
    /// until the second request is asleep on it or has computed too (the two overlap
    /// either way); a 10 s safety limit fails the test.
    #[test]
    fn two_requests_on_one_revision_run_one_computation() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let slot = Slot::<usize>::default();
        let calls = AtomicUsize::new(0);
        let started = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            let first = s.spawn(|| {
                cached(
                    &slot,
                    7,
                    |_| true,
                    || {
                        calls.fetch_add(1, SeqCst);
                        started.wait();
                        until(|| calls.load(SeqCst) >= 2 || asleep(&slot, 7, 1));
                        1
                    },
                )
            });
            started.wait();
            let second = cached(
                &slot,
                7,
                |_| true,
                || {
                    calls.fetch_add(1, SeqCst);
                    2
                },
            );
            let first = first.join().unwrap();
            assert_eq!(calls.load(SeqCst), 1, "computations for one revision");
            assert!(Arc::ptr_eq(&first, &second));
        });
    }

    /// Whether `n` requests are asleep on `revision`'s computation under way.
    fn asleep<T>(slot: &Slot<T>, revision: u64, n: usize) -> bool {
        matches!(slot.building.claim(revision, Kind::Opaque, || None),
            Claim::Wait(build) if build.waiting() >= n)
    }

    /// Holds the caller until `event`; past a 10 s safety limit the test fails.
    fn until(event: impl Fn() -> bool) {
        let limit = Instant::now() + Duration::from_secs(10);
        while !event() {
            assert!(Instant::now() < limit, "the awaited event never came");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Holds a computation until a request waits for it (at most 10 s, then fails).
    fn until_waiting<T>(slot: &Slot<T>, revision: u64) {
        until(|| asleep(slot, revision, 1));
    }

    /// A computation that panics while a request waits for it leaves that request its own
    /// computation: no hang, no poisoned slot, and the waiter's complete result kept.
    #[test]
    fn a_panicking_computation_leaves_the_waiter_its_own() {
        let slot = Slot::<usize>::default();
        let started = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            let first = s.spawn(|| {
                cached(
                    &slot,
                    7,
                    |_| true,
                    || -> usize {
                        started.wait();
                        until_waiting(&slot, 7);
                        panic!("the computation fails")
                    },
                )
            });
            started.wait();
            let second = cached(&slot, 7, |_| true, || 2);
            let failure = first.join().expect_err("the computation panics");
            assert_eq!(
                failure.downcast_ref::<&str>(),
                Some(&"the computation fails"),
                "the computation failed in its setup, not by the injected panic"
            );
            assert_eq!(*second, 2);
            assert!(Arc::ptr_eq(&second, &latest(&slot, 7).unwrap()));
        });
    }

    /// A computation its own request's `dl.timeout` stopped (incomplete, so never kept)
    /// isn't handed on: the request waiting for it computes its own and gets that.
    #[test]
    fn a_waiter_on_a_computation_its_timeout_stopped_gets_its_own_computation() {
        let slot = Slot::<(bool, usize)>::default();
        let started = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            let first = s.spawn(|| {
                cached(
                    &slot,
                    7,
                    |r| r.0,
                    || {
                        started.wait();
                        until_waiting(&slot, 7);
                        (false, 1)
                    },
                )
            });
            started.wait();
            let second = cached(&slot, 7, |r| r.0, || (true, 2));
            assert_eq!(*first.join().unwrap(), (false, 1));
            assert_eq!(*second, (true, 2), "the waiter's own computation");
            assert!(Arc::ptr_eq(&second, &latest(&slot, 7).unwrap()));
        });
    }

    /// A request waiting for another's computation that gives up (its policy timeout passed,
    /// the client has its 408) returns while that computation still runs, with no result and
    /// no computation of its own; the computation runs on and its result is kept. (Found
    /// with D16-655: each waiter held a blocking thread until the computation ended, so a
    /// client retrying after each 408 could fill the pool.) Counted, not timed: the
    /// computation holds until the waiter has returned, or for 10 s, a guard only.
    #[test]
    fn a_waiter_whose_request_gave_up_returns_while_the_computation_runs() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        let slot = Slot::<usize>::default();
        let (calls, over, returned, gave_up) = (
            AtomicUsize::new(0),
            AtomicBool::new(false),
            AtomicBool::new(false),
            AtomicBool::new(false),
        );
        let started = std::sync::Barrier::new(2);
        std::thread::scope(|s| {
            let first = s.spawn(|| {
                cached(
                    &slot,
                    7,
                    |_| true,
                    || {
                        calls.fetch_add(1, SeqCst);
                        started.wait();
                        let until = Instant::now() + Duration::from_secs(10);
                        while !returned.load(SeqCst) && Instant::now() < until {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        over.store(true, SeqCst);
                        1
                    },
                )
            });
            started.wait();
            let second = s.spawn(|| {
                let stop = || gave_up.load(SeqCst);
                let result = super::cached(
                    &slot,
                    7,
                    &stop,
                    |_| true,
                    || {
                        calls.fetch_add(1, SeqCst);
                        2
                    },
                );
                let over_then = over.load(SeqCst);
                returned.store(true, SeqCst);
                (result, over_then)
            });
            until_waiting(&slot, 7);
            gave_up.store(true, SeqCst);
            let (result, over_then) = second.join().unwrap();
            assert!(
                !over_then,
                "the waiter returned only after the computation ended"
            );
            assert!(result.is_none(), "the waiter that gave up got a result");
            let first = first.join().unwrap();
            assert_eq!(calls.load(SeqCst), 1, "computations for one revision");
            assert!(Arc::ptr_eq(&first, &latest(&slot, 7).unwrap()));
        });
    }

    const WAITERS: usize = 4;

    /// Several requests asleep on a computation give up (their policy timeouts: each client
    /// has its 408), and the computation then ends with nothing to share, incomplete or by a
    /// panic, before they look at their stops again: each returns with no result, and none
    /// starts a computation of its own (a request that had its 408 started a
    /// fresh computation nobody could stop). Counted by computations, not timed.
    /// Not a strict witness: if every waiter's 50 ms poll sees its
    /// stop before the computation's end reaches it, none takes the terminal path, and the
    /// test can, rarely, pass on a build that is wrong there. The mutant that computes on
    /// that path (`tr`) still turns it red.
    fn stopped_waiters_take_no_replacement(panics: bool) {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        let slot = Slot::<(bool, usize)>::default();
        let (calls, gave_up) = (AtomicUsize::new(0), AtomicBool::new(false));
        let started = std::sync::Barrier::new(2);
        let stop = || gave_up.load(SeqCst);
        std::thread::scope(|s| {
            let first = s.spawn(|| {
                super::cached(
                    &slot,
                    7,
                    NEVER,
                    |r| r.0,
                    || {
                        calls.fetch_add(1, SeqCst);
                        started.wait();
                        until(|| asleep(&slot, 7, WAITERS));
                        gave_up.store(true, SeqCst);
                        if panics {
                            panic!("the computation fails");
                        }
                        (false, 1)
                    },
                )
            });
            started.wait();
            let waiters: Vec<_> = (0..WAITERS)
                .map(|_| {
                    s.spawn(|| {
                        super::cached(
                            &slot,
                            7,
                            &stop,
                            |r| r.0,
                            || {
                                calls.fetch_add(1, SeqCst);
                                (true, 2)
                            },
                        )
                    })
                })
                .collect();
            for waiter in waiters {
                let result = waiter.join().unwrap();
                assert!(result.is_none(), "a waiter that gave up got a result");
            }
            match first.join() {
                Err(failure) => {
                    assert!(panics && failure.downcast_ref() == Some(&"the computation fails"))
                }
                Ok(result) => assert!(!panics && result.is_some_and(|r| *r == (false, 1))),
            }
            assert_eq!(
                calls.load(SeqCst),
                1,
                "computations after the requests gave up"
            );
            assert!(latest(&slot, 7).is_none());
        });
    }

    #[test]
    fn stopped_waiters_on_an_incomplete_computation_take_no_replacement() {
        stopped_waiters_take_no_replacement(false);
    }

    #[test]
    fn stopped_waiters_on_a_panicking_computation_take_no_replacement() {
        stopped_waiters_take_no_replacement(true);
    }

    /// Several requests that never give up, asleep on a computation that ends incomplete: at
    /// most one replacement computation runs at a time (each ran its own,
    /// unclaimed, all at once). If a replacement completes, every request gets its result;
    /// if each ends incomplete too (a budget that runs out every time), every request still
    /// returns, having computed at most once. Counted, not timed: a computation holds until
    /// two have run at once (the fault; then all are let go) or every request that hasn't
    /// computed is asleep on it; a 10 s safety limit fails the test.
    fn interested_waiters_compute_one_at_a_time(replacement_completes: bool) {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        let slot = Slot::<(bool, usize)>::default();
        let (calls, active, most) = (
            AtomicUsize::new(0),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        );
        let started = std::sync::Barrier::new(2);
        let computation = || {
            let call = calls.fetch_add(1, SeqCst) + 1;
            most.fetch_max(active.fetch_add(1, SeqCst) + 1, SeqCst);
            let others = (WAITERS + 1).saturating_sub(call);
            until(|| most.load(SeqCst) >= 2 || asleep(&slot, 7, others));
            active.fetch_sub(1, SeqCst);
            (call > 1 && replacement_completes, call)
        };
        std::thread::scope(|s| {
            let first = s.spawn(|| {
                cached(
                    &slot,
                    7,
                    |r| r.0,
                    || {
                        started.wait();
                        computation()
                    },
                )
            });
            started.wait();
            let waiters: Vec<_> = (0..WAITERS)
                .map(|_| s.spawn(|| cached(&slot, 7, |r| r.0, computation)))
                .collect();
            let results: Vec<_> = waiters.into_iter().map(|w| w.join().unwrap()).collect();
            assert_eq!(*first.join().unwrap(), (false, 1));
            assert_eq!(most.load(SeqCst), 1, "replacement computations at once");
            if replacement_completes {
                assert!(
                    results.iter().all(|r| **r == (true, 2)),
                    "a waiter's result"
                );
                assert_eq!(
                    calls.load(SeqCst),
                    2,
                    "computations: the first and one replacement"
                );
            } else {
                assert!(results.iter().all(|r| !r.0), "a waiter's result");
                assert!(
                    calls.load(SeqCst) <= WAITERS + 1,
                    "computations over requests"
                );
            }
        });
    }

    #[test]
    fn interested_waiters_share_one_replacement_at_a_time() {
        interested_waiters_compute_one_at_a_time(true);
    }

    #[test]
    fn interested_waiters_on_computations_ending_incomplete_compute_one_at_a_time() {
        interested_waiters_compute_one_at_a_time(false);
    }
}
