//! The support graph sets and the closure that decides what their cap left open, when the
//! rules change (`Supports::use_rules`) between a read's steps.

use super::*;

const EX: &str = "http://example.com/";

/// A store with `C ⊑ D` in the default graph and `w a C` in 17 graphs (g00 to g16), its
/// OWL 2 RL closure and rules in place: `w a D` has 17 support sets, one past the default cap
/// of 16, which leaves g16's out (graphs in IRI order).
fn past_the_cap() -> crate::StoreService {
    let store = crate::StoreService::new(crate::StoreConfig::in_memory()).unwrap();
    let graphs: String = (0..17)
        .map(|g| format!("GRAPH <{EX}g{g:02}> {{ <{EX}w> a <{EX}C> }} "))
        .collect();
    let reasoner = nrese_reasoner::ReasonerService::new(nrese_reasoner::ReasonerConfig::default());
    crate::MutationPipeline::new(Arc::new(store.clone()), Arc::new(reasoner))
        .execute_update(&crate::SparqlUpdateRequest::new(format!(
            "INSERT DATA {{ <{EX}C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}D> . \
             {graphs} }}"
        )))
        .unwrap();
    store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
        .unwrap();
    store.use_reasoning_rules(Some(nrese_reasoner::rulesets::Ruleset::Owl2Rl.into()));
    store
}

/// `w a D`, in `snapshot`'s ids, as an inferred statement.
fn w_a_d(snapshot: &Snapshot) -> EncodedQuad {
    let id = |iri: &str| {
        snapshot
            .lookup(nrese_rdf::NamedNode::new_unchecked(iri).as_ref().into())
            .unwrap()
    };
    EncodedQuad::new(
        id(&format!("{EX}w")),
        id("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        id(&format!("{EX}D")),
        TermId::DEFAULT_GRAPH,
    )
}

/// Spec: config.rs (`StoreConfig::support_sets`): a reader none of a statement's kept sets
/// fits is decided by the closure of its graphs, under the rules the sets come from. A
/// repository's rules can change at any time (its reasoning switched off: `use_rules(None)`),
/// also while a read runs; the read then answers by the rules its sets were computed with,
/// and nothing it makes is kept for the rules now in place (review r1 of O4, C1). g16's
/// reader, past the cap: its sets, then the rules switched off, then its view.
#[test]
fn a_rule_change_during_a_read_leaves_it_its_rules_and_keeps_nothing_of_it() {
    let store = past_the_cap();
    let supports = store.supports();
    let snapshot = store.read_snapshot(None).unwrap();
    let compilation = Compilation {
        hide_unnamed_classes: false,
        by_representatives: false,
        compact: false,
        cap: 16,
    };
    let working = Work {
        budget: Arc::new(Budget::unlimited()),
        cancellation: None,
    };
    let access = Arc::new(GraphAccess {
        graphs: vec![format!("{EX}g16")],
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    });
    // The read's steps as `Supports::view` takes them: its cell with the rules in place,
    // its sets, then (the rules switched off meanwhile) its view.
    let (_, generation) = supports.cell_for(&snapshot).unwrap();
    let sets = supports
        .compute(&snapshot, &generation, compilation, &working)
        .unwrap()
        .unwrap();
    supports.use_rules(None);
    let rules = generation.rules.as_ref().expect("the rules in place");
    let within = || supports.within(&snapshot, &access, rules, compilation, &working);
    let view = supports
        .build_view(
            &snapshot,
            &access,
            &sets,
            within,
            generation.epoch,
            &working,
        )
        .unwrap();
    assert!(
        view.contains_in(ReadModel::Inferred, &w_a_d(&snapshot)),
        "g16's reader derives w a D"
    );
    let state = supports.state();
    assert!(
        state.readers.is_empty() && state.computed.is_none(),
        "kept for the rules now in place"
    );
}

/// Spec: an update's `WHERE` reads what its requester's queries read ([`Reading`], review r1 of
/// PR #50, F1), under the update's cancellation (the orchestrator's case 3 of F1). Its read
/// source, cancelled while it makes the requester's view, fails as cancelled and keeps
/// nothing; a requester of every graph reads the data as stored, and has none.
#[test]
fn an_updates_read_source_stops_with_its_cancellation() {
    let store = past_the_cap();
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let context = store.update_context(store.envelope(None), &cancelled);
    let reader = crate::Requester::new(
        crate::ReadScope::Graphs(Arc::new(GraphAccess {
            graphs: vec![format!("{EX}g16")],
            default_graph: true,
            inferred: true,
            inferred_by_support: true,
            ..GraphAccess::default()
        })),
        crate::WriteScope::All,
    );
    assert!(
        context
            .update_options(&crate::Requester::all(), None)
            .read
            .is_none()
    );
    let read = context
        .update_options(&reader, None)
        .read
        .expect("a reader by support reads its view");
    let before = store.support_statistics();
    let tx = store.engine().speculative();
    let state = nrese_sparql::ReadSource::state(&*read, &tx);
    assert!(
        matches!(state, Err(nrese_sparql::QueryEvaluationError::Cancelled)),
        "{:?}",
        state.map(|_| ())
    );
    assert_eq!(store.support_statistics(), before, "the view was made");
}

/// A seeded generator (splitmix64), for the property tests.
struct Seeded(u64);

impl Seeded {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        (z ^ (z >> 31)) % n
    }
}

/// The spec of a pending read (D5; what `Reading::pending` gathered before it was delta-sized):
/// the pending snapshot, its inferred statements those of the reader's view of the base the
/// changes leave: a base-inferred one while the transaction still holds it as inferred, one the
/// view adds while no change asserts it where the reader reads it. Every statement scanned.
fn gathered(reading: &Reading, tx: &Transaction<'_>) -> Snapshot {
    if tx.pending() == (0, 0) && tx.inferred_pending() == (0, 0) {
        return reading.committed(tx.base()).unwrap();
    }
    if !reading.through_view(tx.base()) {
        return tx.pending_snapshot();
    }
    let view = reading.committed(tx.base()).unwrap();
    let mut kept: Vec<EncodedQuad> = view
        .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
        .filter(
            |quad| match tx.base().contains_in(ReadModel::Inferred, quad) {
                true => tx.contains_in(ReadModel::Inferred, quad),
                false => asserted(tx, &reading.access, triple(*quad)) != Asserted::InReach,
            },
        )
        .collect();
    kept.sort_unstable();
    tx.pending_snapshot()
        .with_inferred_only(&kept, &Arc::new(Budget::unlimited()))
        .unwrap()
}

/// A snapshot's asserted and inferred statements, sorted, each list checked against the count
/// the snapshot keeps of it.
fn statements(snapshot: &Snapshot) -> [Vec<EncodedQuad>; 2] {
    [ReadModel::Asserted, ReadModel::Inferred].map(|model| {
        let mut quads: Vec<EncodedQuad> = snapshot
            .quads_for_pattern_in(model, &QuadPattern::all())
            .collect();
        quads.sort_unstable();
        assert_eq!(
            snapshot.len_in(model),
            quads.len() as u64,
            "{model:?}: the count kept"
        );
        quads
    })
}

/// Spec (D5, review r1 of PR #50): a pending read costs its delta, and reads exactly what the
/// gathering read (`gathered`), by support and seeing every inference. Seeded stores of 10 to 90
/// subjects typed in a class chain (`C0 ⊑ … ⊑ C4`, in the default graph) across five graphs,
/// some memberships also asserted in a graph the reader can't read (its own graphs derive them:
/// the view adds them). Each store gets three rounds of changes, checked after each (a request's
/// later operations): inserts and deletes of memberships in every graph and the default graph,
/// among them base-inferred statements asserted now, view-added ones asserted where the reader
/// reads, and deletions; each kind must occur.
#[test]
fn a_pending_read_is_the_gathered_one() {
    use nrese_rdf::{GraphName, NamedNode, Quad};
    let iri = |local: String| NamedNode::new_unchecked(format!("urn:t:{local}"));
    let graph = |g: u64| match g {
        0 => GraphName::DefaultGraph,
        g => GraphName::NamedNode(iri(format!("g{g}"))),
    };
    let member = |s: u64, c: u64, g: u64| {
        Quad::new(
            iri(format!("s{s}")),
            NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            iri(format!("C{c}")),
            graph(g),
        )
    };
    let [mut base_inferred, mut view_added, mut deleted] = [0_usize; 3];
    let [mut adding_views, mut restricting_views] = [0_usize; 2];
    for seed in 0..48_u64 {
        let mut rng = Seeded(seed);
        let subjects = 10 + 10 * (seed % 9);
        // Graphs 1 to 5; the reader reads 1, 2 and the default graph, and for two seeds in
        // eight every graph, so that nothing is out of its reach and its view only restricts.
        let reach = match seed % 8 >= 6 {
            true => 5,
            false => 2,
        };
        let mut data: Vec<Quad> = Vec::new();
        for s in 0..subjects {
            for _ in 0..1 + rng.below(3) {
                data.push(member(s, rng.below(3), 1 + rng.below(5)));
            }
            if rng.below(3) == 0 {
                // Asserted out of reach: the view adds it where the reader's graphs derive it.
                data.push(member(s, 3 + rng.below(2), 3 + rng.below(3)));
            }
        }
        let schema: String = (0..4)
            .map(|c| {
                format!(
                    "<urn:t:C{c}> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:t:C{}> . ",
                    c + 1
                )
            })
            .collect();
        let quads: String = data
            .iter()
            .map(|q| {
                let triple = format!("{} {} {}", q.subject, q.predicate, q.object);
                match &q.graph_name {
                    GraphName::DefaultGraph => format!("{triple} . "),
                    named => format!("GRAPH {named} {{ {triple} }} "),
                }
            })
            .collect();
        let store = crate::StoreService::new(crate::StoreConfig::in_memory()).unwrap();
        let reasoner =
            nrese_reasoner::ReasonerService::new(nrese_reasoner::ReasonerConfig::default());
        crate::MutationPipeline::new(Arc::new(store.clone()), Arc::new(reasoner))
            .execute_update(&crate::SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ {schema} {quads} }}"
            )))
            .unwrap();
        store
            .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
            .unwrap();
        store.use_reasoning_rules(Some(nrese_reasoner::rulesets::Ruleset::Owl2Rl.into()));
        let access = Arc::new(GraphAccess {
            graphs: (1..=reach).map(|g| format!("urn:t:g{g}")).collect(),
            default_graph: true,
            inferred: true,
            inferred_by_support: seed % 2 == 0,
            ..GraphAccess::default()
        });
        let reading = Reading::of(
            store.supports(),
            store.support_compilation(),
            &crate::ReadScope::Graphs(Arc::clone(&access)),
            &store.envelope(None),
            None,
        )
        .1
        .unwrap();
        let mut tx = store.engine().speculative();
        let view = reading.committed(tx.base()).unwrap();
        // Dictionary 3.0, sol-loop r1 S1: a view showing facts its published base doesn't
        // infer (asserted out of the reader's reach) is an addition and unpublished; one
        // that only hides its base's inferences keeps the base's publication.
        let adds = view
            .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
            .any(|quad| !tx.base().contains_in(ReadModel::Inferred, &quad));
        assert_eq!(
            view.shared_state().is_some(),
            !adds,
            "seed {seed}: a view adding facts {adds} has a token"
        );
        adding_views += usize::from(adds);
        restricting_views += usize::from(!adds);
        for round in 0..3 {
            for _ in 0..1 + rng.below(2 + 4 * (seed % 5)) {
                if rng.below(4) == 0 && !data.is_empty() {
                    let gone = data.swap_remove(rng.below(data.len() as u64) as usize);
                    deleted += usize::from(tx.remove(gone.as_ref()));
                    continue;
                }
                let quad = member(rng.below(subjects), rng.below(5), rng.below(6));
                if let Some(fact) = (|| {
                    let id = |term| tx.lookup(term);
                    Some(EncodedQuad::new(
                        id(quad.subject.as_ref().into())?,
                        id(quad.predicate.as_ref().into())?,
                        id(quad.object.as_ref())?,
                        TermId::DEFAULT_GRAPH,
                    ))
                })() {
                    let in_view = view.contains_in(ReadModel::Inferred, &fact);
                    let of_base = tx.base().contains_in(ReadModel::Inferred, &fact);
                    let readable = access.allows_graph(&quad.graph_name);
                    base_inferred +=
                        usize::from(in_view && of_base && quad.graph_name.is_default_graph());
                    view_added += usize::from(in_view && !of_base && readable);
                }
                tx.insert(quad.as_ref());
                data.push(quad);
            }
            assert_eq!(
                statements(&reading.pending(&tx).unwrap()),
                statements(&gathered(&reading, &tx)),
                "seed {seed}, round {round}"
            );
        }
    }
    assert!(
        base_inferred > 0 && view_added > 0 && deleted > 0,
        "the kinds of change covered: base-inferred asserted {base_inferred}, view-added \
         asserted in reach {view_added}, deletions {deleted}"
    );
    assert!(
        adding_views > 0 && restricting_views > 0,
        "both kinds of view covered: adding {adding_views}, restricting {restricting_views}"
    );
}

/// DC-07: an evaluation the process limit stopped because the process's memory couldn't be
/// read fails its read as "memory unreadable", not as the limit; one stopped past the
/// limit stays the limit's.
#[test]
fn a_view_stopped_unread_fails_as_memory_unreadable() {
    let following = Following::new(&Work {
        budget: Arc::new(Budget::unlimited()),
        cancellation: None,
    });
    let unread = following.failure(Stopped::MemoryUnreadable);
    assert_eq!(unread, Unavailable::MemoryUnreadable);
    assert!(matches!(
        crate::StoreError::from(unread),
        crate::StoreError::MemoryUnreadable { .. }
    ));
    assert!(matches!(
        crate::StoreError::from(following.failure(Stopped::ProcessMemory)),
        crate::StoreError::ProcessMemoryLimit { .. }
    ));
}

/// REAS-2 (master, 8 Nov 05:29: a counted test per leaf that filters): the closure after a
/// change counts the removed facts its scan passes over (sweep M13), not only those it
/// delivers. Over three poll intervals of removed facts, none matching, a poller whose stop
/// has fired polls within the scan and is stopped by it; uncounted, it would never poll.
#[test]
fn the_closure_after_a_change_counts_the_removed_facts_it_passes_over() {
    let n = 3 * u64::from(nrese_exec::POLL_UNITS);
    let store = crate::StoreService::new(crate::StoreConfig::in_memory()).unwrap();
    let snapshot = store.engine().snapshot();
    let removed: Vec<Triple> = (0..n).map(|i| [i, 5, i]).collect();
    let after = After {
        snapshot: &snapshot,
        removed: &removed,
    };
    let fired = std::sync::atomic::AtomicBool::new(false);
    let stop = || fired.load(std::sync::atomic::Ordering::Relaxed);
    let poll = Poller::new(&stop).expect("not stopped yet");
    fired.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut delivered = 0;
    let scanned = nrese_reasoner::eval::Source::scan(
        &after,
        [None, Some(6), None],
        Seg::All,
        &poll,
        &mut |_| delivered += 1,
    );
    assert!(scanned.is_err(), "a scan its poller stopped says so");
    assert_eq!(delivered, 0, "no removed fact matches");
    assert!(
        poll.stopped(),
        "{n} removed facts passed over without the poller polling"
    );
}

/// REAS-2 review r1 F1: the closure after a change estimates its matches (the update's job
/// planning) by reading the removed facts, each counted as it is read: 8 × `POLL_UNITS` of
/// them ask the stop at least once every `POLL_UNITS` plus one run (the longest run counted
/// at once, `POLL_UNITS`). Counted all at once, the whole list was one ask.
#[test]
fn the_closure_after_a_change_estimates_within_a_run_of_the_stop() {
    let interval = nrese_exec::POLL_UNITS as usize;
    let n = 8 * interval as u64;
    let store = crate::StoreService::new(crate::StoreConfig::in_memory()).unwrap();
    let snapshot = store.engine().snapshot();
    let removed: Vec<Triple> = (0..n).map(|i| [i, 5, i]).collect();
    let after = After {
        snapshot: &snapshot,
        removed: &removed,
    };
    let asks = std::sync::atomic::AtomicUsize::new(0);
    let stop = || {
        asks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        false
    };
    let poll = Poller::new(&stop).expect("never stopped");
    let estimate =
        nrese_reasoner::eval::Source::estimate(&after, [None, Some(6), None], Seg::All, &poll)
            .expect("never stopped");
    assert_eq!(
        estimate, 0,
        "no removed fact matches, the snapshot holds none"
    );
    let run = interval;
    let asks = asks.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        asks >= n as usize / (interval + run),
        "{asks} asks of the stop for {n} removed facts read: more than {interval} plus one run \
         of {run} read between two of them"
    );
}

/// REAS-2 review r1 F4: a restricted reader's view is brought to a commit from the changed
/// facts (`SupportSets::changes_since`), each counted before it is decided, and its store
/// probes too. g16's reader, its sets computed, then a commit of 3,000 memberships of `C` in
/// a graph it can't read: with their `x a D`, 6,000 changed facts that both versions hide,
/// under the update's limit (a quarter of the earlier sets plus 10,000 touched facts), so
/// the sets are updated, not recomputed; each is decided, then probed four times in the
/// second pass. After the poller is made, the stop is asked at least once every
/// `POLL_UNITS` plus one run of those decisions and probes. Hidden by both, a fact was
/// decided with no count, and the second pass's probes had none.
#[test]
fn a_restricted_view_s_changes_are_counted_as_they_are_read() {
    let interval = nrese_exec::POLL_UNITS as usize;
    let n = 3_000;
    let store = past_the_cap();
    // The update's pipeline sets its rules on the store (a new epoch), so it is made first,
    // under the rules the sets are computed with.
    let reasoner = nrese_reasoner::ReasonerService::new(nrese_reasoner::ReasonerConfig::for_mode(
        nrese_reasoner::ReasoningMode::for_ruleset(nrese_reasoner::rulesets::Ruleset::Owl2Rl),
    ));
    let pipeline = crate::MutationPipeline::new(Arc::new(store.clone()), Arc::new(reasoner));
    let supports = store.supports();
    let compilation = Compilation {
        hide_unnamed_classes: false,
        by_representatives: false,
        compact: false,
        cap: 16,
    };
    let working = Work {
        budget: Arc::new(Budget::unlimited()),
        cancellation: None,
    };
    let before = store.read_snapshot(None).unwrap();
    let (_, generation) = supports.cell_for(&before).unwrap();
    let earlier = supports
        .compute(&before, &generation, compilation, &working)
        .unwrap()
        .unwrap();
    let members: String = (0..n).map(|i| format!("<{EX}x{i}> a <{EX}C> . ")).collect();
    pipeline
        .execute_update(&crate::SparqlUpdateRequest::new(format!(
            "INSERT DATA {{ GRAPH <{EX}hidden> {{ {members} }} }}"
        )))
        .unwrap();
    let after = store.read_snapshot(None).unwrap();
    let (_, generation) = supports.cell_for(&after).unwrap();
    let sets = supports
        .compute(&after, &generation, compilation, &working)
        .unwrap()
        .unwrap();
    let changed = sets.since.as_ref().map_or(0, |(_, changed)| changed.len());
    assert!(
        changed >= n,
        "{changed} facts changed: the sets weren't updated"
    );
    let access = GraphAccess {
        graphs: vec![format!("{EX}g16")],
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    };
    let asks = std::sync::atomic::AtomicUsize::new(0);
    let stop = || {
        asks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        false
    };
    let changes = sets
        .changes_since(&after, &earlier, &before, &access, &working, &stop)
        .expect("never stopped");
    assert!(changes.is_some(), "the view is brought to the commit");
    // Its poller's creation asks once; then each fact's decision and its four probes.
    let asked = asks.load(std::sync::atomic::Ordering::Relaxed) - 1;
    let (work, run) = (5 * changed, interval);
    assert!(
        asked >= work / (interval + run),
        "{asked} asks of the stop for {changed} changed facts ({work} decisions and probes): more \
         than {interval} plus one run of {run} between two of them"
    );
}

/// REAS-2 fix check r2 (R3-F3, SW109). Spec (design note, rule 2: a stop is an error the
/// consumer has to handle, never an answer): the re-derived facts of a snapshot, brought to it
/// from the earlier ones through the commit's touched facts (`compute_rederived`'s fallback),
/// end once the reading query is cancelled while their closure is asked about. The earlier
/// re-derived facts computed, then one commit of 4 × `POLL_UNITS` memberships of `C`, then the
/// next snapshot's, the query cancelled at the first touched fact asked about: no more than
/// `POLL_UNITS` touched facts asked about (the poller sees the cancellation at its next ask),
/// and the read fails as cancelled. A stopped ask read as "not in the closure", and every later
/// touched fact was asked about and kept as removed. A run without cancellation, after it,
/// asks about every touched fact: the fallback is the path taken.
#[test]
fn a_cached_rederived_fallback_ends_at_its_cancellation() {
    use crate::witness::{self, Site};
    let interval = nrese_exec::POLL_UNITS as usize;
    let n = 4 * interval;
    let store = past_the_cap();
    let reasoner = nrese_reasoner::ReasonerService::new(nrese_reasoner::ReasonerConfig::for_mode(
        nrese_reasoner::ReasoningMode::for_ruleset(nrese_reasoner::rulesets::Ruleset::Owl2Rl),
    ));
    let pipeline = crate::MutationPipeline::new(Arc::new(store.clone()), Arc::new(reasoner));
    let supports = store.supports();
    let compilation = Compilation {
        hide_unnamed_classes: false,
        by_representatives: false,
        compact: false,
        cap: 16,
    };
    let unlimited = || Work {
        budget: Arc::new(Budget::unlimited()),
        cancellation: None,
    };
    let before = store.read_snapshot(None).unwrap();
    let (_, generation) = supports.cell_for(&before).unwrap();
    supports
        .compute_rederived(&before, &generation, compilation, &unlimited())
        .unwrap();
    let members: String = (0..n).map(|i| format!("<{EX}x{i}> a <{EX}C> . ")).collect();
    pipeline
        .execute_update(&crate::SparqlUpdateRequest::new(format!(
            "INSERT DATA {{ {members} }}"
        )))
        .unwrap();
    let after = store.read_snapshot(None).unwrap();
    let (_, generation) = supports.cell_for(&after).unwrap();
    let token = nrese_sparql::CancellationToken::new();
    let cancelled = Work {
        budget: Arc::new(Budget::unlimited()),
        cancellation: Some(token.clone()),
    };
    let start = witness::get(Site::FallbackProbe);
    witness::on_first(Site::FallbackProbe, Box::new(move || token.cancel()));
    let stopped = supports.compute_rederived(&after, &generation, compilation, &cancelled);
    let asked = witness::get(Site::FallbackProbe) - start;
    let whole = {
        let start = witness::get(Site::FallbackProbe);
        supports
            .compute_rederived(&after, &generation, compilation, &unlimited())
            .unwrap();
        witness::get(Site::FallbackProbe) - start
    };
    assert!(
        whole >= n as u64,
        "{whole} touched facts asked about uncancelled: the fallback wasn't taken"
    );
    assert!(
        asked <= interval as u64 + 1,
        "{asked} touched facts asked about once cancelled at the first, of {whole}: more than \
         {interval} plus the one that saw it"
    );
    assert!(
        matches!(stopped, Err(Unavailable::Cancelled)),
        "a cancelled read fails as cancelled"
    );
}
