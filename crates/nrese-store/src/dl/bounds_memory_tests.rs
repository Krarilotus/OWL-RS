//! No process-global setters: each test uses an independent store watch.

use super::*;
use crate::StoreConfig;

fn store(limit: u64) -> StoreService {
    // The store watch at `limit`; what is charged (the bounds' reading and keeping among it,
    // SYN-61) on a runtime of its own with room (`StoreService::with_runtime`: the config's
    // process memory stays the store's watch), so these tests reach the watch.
    let store = StoreService::with_runtime(
        StoreConfig {
            process_memory_bytes: crate::ProcessMemory::Bytes(limit),
            ..StoreConfig::in_memory()
        },
        Arc::new(crate::Runtime::new(0, 1 << 30)),
    )
    .unwrap();
    // An existential conclusion takes the U1 route, not the OWL 2 RL shortcut.
    store
        .update_str_ungated(
            "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
             owl:someValuesFrom :B ] . :a a :A . }",
        )
        .unwrap();
    store
}

/// A store with a queries' pool of its own (what the bounds read and keep draw on), holding
/// `data` written past the pipeline (no commit builds U1: the first read does).
fn pooled(data: &str) -> StoreService {
    let store = StoreService::new(StoreConfig {
        total_query_memory_bytes: 1 << 30,
        ..StoreConfig::in_memory()
    })
    .unwrap();
    store.update_str_ungated(data).unwrap();
    store
}

/// Leaves `room` bytes of the store's queries' pool free while the returned charge is held.
fn squeeze(store: &StoreService, room: usize) -> nrese_exec::Reservation {
    let pool = store.runtime().query_memory().expect("the queries' pool");
    let mut hold = nrese_exec::Reservation::new(Arc::new(store.envelope(None).part()));
    hold.charge(pool.limit() - pool.used() - room).unwrap();
    hold
}

/// An existential axiom (U1's route) and `n` members of its class: U1 has about two facts
/// each.
fn members(n: usize) -> String {
    let mut data = String::from(
        "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
             owl:someValuesFrom :B ] .",
    );
    for i in 0..n {
        data.push_str(&format!(" :a{i} a :A ."));
    }
    data.push('}');
    data
}

/// A U1 the queries' memory refused is never kept: once memory is free again, the next read of
/// the same revision builds it (SYN-61).
#[test]
fn an_upper_bound_refused_for_memory_is_built_once_memory_is_free() {
    let store = pooled(&members(2_000));
    let hold = squeeze(&store, 4096);
    let refused = report(&store);
    assert!(
        refused
            .unavailable
            .as_deref()
            .is_some_and(|why| why.contains("memory")),
        "{refused:?}"
    );
    drop(hold);
    let after = report(&store);
    assert_eq!(after.unavailable, None, "{after:?}");
    assert!(after.upper_facts > 0, "{after:?}");
}

/// What the bounds keep is charged while kept (SYN-61): the query-ontology cache's ontology
/// among it, beside U1's stack, so the charge is at least both.
#[test]
fn the_cached_ontology_is_charged_with_what_the_bounds_keep() {
    let mut data = members(1);
    data.pop();
    for i in 0..20_000 {
        data.push_str(&format!(" :a0 :note \"n{i}\" ."));
    }
    data.push('}');
    let store = pooled(&data);
    let report = report(&store);
    assert_eq!(report.unavailable, None, "{report:?}");
    let snapshot = store.engine().snapshot();
    let ontology = super::super::query::ontology_at(&store, &snapshot).unwrap();
    let cached = nrese_dl::memory::ontology_bytes(ontology.value());
    let stack = report.upper_facts * 3 * std::mem::size_of::<[u64; 3]>();
    let kept = store.dl().bounds.kept().used();
    assert!(
        kept >= cached + stack,
        "{kept} bytes kept, the cached ontology {cached}, the stack's keys {stack}"
    );
}

/// The query ontology's charge goes with its last holder, not with the cache (SYN-61): a reader
/// still holding a revision's ontology after the cache moved to the next keeps it charged, so
/// the budget holds both.
#[test]
fn a_cached_ontology_a_reader_holds_stays_charged_after_the_cache_moves_on() {
    let mut data = members(1);
    data.pop();
    for i in 0..5_000 {
        data.push_str(&format!(" :a0 :note \"n{i}\" ."));
    }
    data.push('}');
    let store = pooled(&data);
    let first = super::super::query::ontology_at(&store, &store.engine().snapshot()).unwrap();
    store
        .update_str_ungated("PREFIX : <urn:test:> INSERT DATA { :a0 :note \"more\" . }")
        .unwrap();
    let second = super::super::query::ontology_at(&store, &store.engine().snapshot()).unwrap();
    let both = nrese_dl::memory::ontology_bytes(first.value())
        + nrese_dl::memory::ontology_bytes(second.value());
    let kept = store.dl().bounds.kept().used();
    assert!(
        kept >= both,
        "{kept} bytes kept while both revisions' ontologies ({both} bytes) are held"
    );
}

/// A generation the latest revision has passed is let go, with its charge, before U1 is built
/// afresh (SYN-61), so the build needs room for itself, not beside it: when the build of the
/// revision after a write past the pipeline begins, the bounds hold no older generation.
#[test]
fn a_superseded_generation_is_let_go_before_a_fresh_build() {
    let store = Arc::new(pooled(&members(2_000)));
    let first = report(&store);
    assert_eq!(first.unavailable, None, "{first:?}");
    store
        .update_str_ungated("PREFIX : <urn:test:> INSERT DATA { :extra a :A . }")
        .unwrap();
    // The revision of the generation the bounds hold as the build begins.
    let held = Arc::new(std::sync::Mutex::new(None));
    let (at, seen) = (Arc::clone(&store), Arc::clone(&held));
    seam::set(&store.dl().bounds, seam::Point::Build, move || {
        let generation = lock(&at.dl().bounds.state).as_ref().map(|s| s.revision);
        *seen.lock().unwrap() = Some(generation);
    });
    let after = report(&store);
    assert_eq!(after.unavailable, None, "{after:?}");
    assert_eq!(
        *held.lock().unwrap(),
        Some(None),
        "the generation held as the build of revision {} began (None: no build)",
        after.revision
    );
}

/// What is published only moves forward, a generation let go included (U1-P, SYN-61). A read
/// of a later revision lets the bounds' generation go, and its own U1 is refused for memory,
/// so nothing replaces it. A build of the older revision that finishes late (with U1
/// unavailable) then publishes nothing: a commit from that revision would carry it forward
/// as the later revision's U1.
#[test]
fn a_late_build_of_a_revision_a_read_let_go_publishes_nothing() {
    let store = pooled(&members(200));
    let first = report(&store);
    assert_eq!(first.unavailable, None, "{first:?}");
    // A reader holds the first revision's ontology, so its charge stays when the cache lets
    // it go before the next cold build (N1): the squeeze below leaves U1 itself no room.
    let reader = super::super::query::ontology_at(&store, &store.engine().snapshot()).unwrap();
    store
        .update_str_ungated("PREFIX : <urn:test:> INSERT DATA { :extra a :A . }")
        .unwrap();
    let hold = squeeze(&store, 0);
    let refused = report(&store);
    drop(hold);
    drop(reader);
    let held = || lock(&store.dl().bounds.state).as_ref().map(|s| s.revision);
    assert_eq!(
        held(),
        None,
        "the bounds after the refused read: {refused:?}"
    );
    // The late build of the first revision, at its publication (a read's, afresh).
    let state = State {
        revision: first.revision,
        upper: Err(not_compiled()),
        last: "read",
        compiled: store.dl().bounds.compiled(),
        taxonomy: None,
        rules: false,
    };
    publish(&mut lock(&store.dl().bounds.state), state);
    assert_eq!(
        held(),
        None,
        "revision {}'s late build published after revision {} let it go",
        first.revision,
        refused.revision
    );
}

/// Building a revision's view from a U1 the bounds keep allocates nothing store-scale it
/// hasn't charged first (SYN-61), the view's copies of U1's facts (24 bytes a fact) among it.
/// Measured against the bounds' budget (`nrese_exec::measure`, alone): what the build holds
/// beyond its charges stays well below those copies; left uncharged, they alone would exceed
/// it. U1 is built and its taxonomy kept before, by a commit and a read; a second commit's
/// change is U1's delta, so the measured read builds the view only.
#[test]
fn a_views_copies_of_the_upper_bound_are_charged_before_they_are_made() {
    if !nrese_exec::measure::alone() {
        return;
    }
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    let store = Arc::new(
        StoreService::new(StoreConfig {
            total_query_memory_bytes: 1 << 30,
            ..StoreConfig::in_memory()
        })
        .unwrap(),
    );
    let pipeline = crate::MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    let commit = |data: String| {
        pipeline
            .apply(
                crate::MutationCommand::Update(crate::SparqlUpdateRequest::new(data)),
                &crate::Requester::all(),
                &crate::MutationTicket::new(),
            )
            .unwrap();
    };
    // Twenty existentials: twenty facts of U1 a member.
    let mut data = String::from(
        "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> INSERT DATA {",
    );
    for k in 0..20 {
        data.push_str(&format!(
            " :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p{k} ; \
             owl:someValuesFrom :B ] ."
        ));
    }
    for i in 0..2_000 {
        data.push_str(&format!(" :a{i} a :A ."));
    }
    data.push('}');
    commit(data);
    let before = report(&store);
    assert_eq!(before.unavailable, None, "{before:?}");
    commit("PREFIX : <urn:test:> INSERT DATA { :extra a :A . }".to_owned());
    let budget = Arc::clone(store.dl().bounds.kept());
    let ((), excess) = nrese_exec::measure::measure_within(&budget, || {
        let _ = view(&store);
    });
    let after = report(&store);
    assert_eq!(after.last, "delta", "{after:?}");
    let copies = after.upper_facts * std::mem::size_of::<EncodedTriple>();
    assert!(
        excess < copies / 2,
        "{excess} bytes held beyond the charge building the view of {} facts of U1 \
         ({copies} bytes of copies)",
        after.upper_facts
    );
}

/// What the bounds keep at a revision is charged to their budget while kept (SYN-61): U1's
/// own stack among it, which holds each of U1's facts three times as 24-byte keys, so the
/// charge is at least that, whatever the views' runs add.
#[test]
fn the_upper_bounds_stack_is_charged_while_it_is_kept() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let mut data = String::from(
        "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
             owl:someValuesFrom :B ] .",
    );
    for i in 0..2_000 {
        data.push_str(&format!(" :a{i} a :A ."));
    }
    data.push('}');
    store.update_str_ungated(&data).unwrap();
    let report = report(&store);
    assert_eq!(report.unavailable, None, "{report:?}");
    let keys = report.upper_facts * 3 * std::mem::size_of::<[u64; 3]>();
    let kept = store.dl().bounds.kept().used();
    assert!(
        kept >= keys,
        "{kept} bytes kept for {} facts of U1 ({keys} bytes of its stack's keys)",
        report.upper_facts
    );
}

/// U1's build reads the whole materialised view as its evaluation's input (SYN-61): that
/// scratch is charged to the bounds' budget while it is held, so the budget's peak is at least
/// the view's facts as 24-byte keys. The view is many values of one individual, so U1's own
/// facts (an individual's few, `owl:Thing` among them) stay far below it.
#[test]
fn the_upper_bounds_build_input_is_charged_while_it_is_held() {
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let mut data = String::from(
        "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ;
             owl:someValuesFrom :B ] . :a a :A .",
    );
    for i in 0..20_000 {
        data.push_str(&format!(" :a :note \"n{i}\" ."));
    }
    data.push('}');
    store.update_str_ungated(&data).unwrap();
    let report = report(&store);
    assert_eq!(report.unavailable, None, "{report:?}");
    let view = store
        .engine()
        .snapshot()
        .count_in(ReadModel::Materialised, &nrese_engine::QuadPattern::all())
        .at_least();
    let input = usize::try_from(view).unwrap() * std::mem::size_of::<[u64; 3]>();
    let peak = store.dl().bounds.kept().peak();
    assert!(
        peak >= input,
        "peak {peak} bytes while reading {view} facts ({input} bytes) for {} facts of U1",
        report.upper_facts
    );
}

#[test]
fn embedded_low_memory_bounds_are_unavailable_on_every_build_path() {
    if !nrese_exec::memory::READS_MEMORY {
        return;
    }
    let store = store(1);
    assert_eq!(
        super::super::classification::options(&store).max_memory,
        Some(1)
    );
    assert!(upper_facts(&store, true).is_none(), "fresh diagnostic U1");
    let report = report(&store);
    assert!(report.unavailable.as_deref().unwrap().contains("stopped"));
    assert!(upper_facts(&store, false).is_none(), "cached read U1");
    // A distinct store ensures prepare cannot just reuse an unavailable read result.
    let store = self::store(1);
    let tx = store.engine().speculative();
    let prepared = prepare(&store, &tx, nrese_reasoner::eval::NEVER);
    assert!(!prepared.proves_consistency);
    assert!(matches!(prepared.prepared, Prepared::Rebuilt(upper) if upper.is_err()));
}

/// REAS-3 (C17; master, 9 Nov 02:34). Spec (`GaveUp`'s docs and SYN-61: what memory refused is
/// never kept, the next read builds it again): a U1 the process's memory stopped is not kept for
/// its revision, on a read's build and on a commit's. Kept, every later reader of the revision
/// is answered by the lower bound alone (dl/query/mod.rs: the view's `unavailable`) until the
/// next commit, though memory may be free again.
#[test]
fn an_upper_bound_the_process_memory_stopped_is_not_kept() {
    if !nrese_exec::memory::READS_MEMORY {
        return;
    }
    let kept = |store: &StoreService| {
        lock(&store.dl().bounds.state)
            .as_ref()
            .is_some_and(|s| s.upper.as_ref().is_err_and(|g| !g.refused))
    };
    let store = store(1);
    assert!(
        report(&store).unavailable.is_some(),
        "the process's memory stops U1"
    );
    assert!(
        !kept(&store),
        "a read keeps no U1 the process's memory stopped"
    );
    let store = self::store(1);
    let tx = store.engine().speculative();
    let prepared = prepare(&store, &tx, nrese_reasoner::eval::NEVER);
    assert!(
        matches!(&prepared.prepared, Prepared::Rebuilt(upper) if upper.is_err()),
        "the commit's U1 is stopped too"
    );
    install(&store, prepared, tx.base().revision() + 1);
    assert!(
        !kept(&store),
        "a commit installs no U1 the process's memory stopped"
    );
}

#[test]
fn embedded_zero_memory_policy_leaves_bounds_enabled() {
    let store = store(0);
    assert!(!store.memory_stop().exceeded(), "a zero limit never stops");
    assert_eq!(
        super::super::classification::options(&store).max_memory,
        Some(0)
    );
    let fresh = upper_facts(&store, true).expect("fresh U1 with disabled watch");
    assert!(!fresh.is_empty());
    assert!(report(&store).unavailable.is_none());
    assert_eq!(upper_facts(&store, false).unwrap(), fresh);
    let tx = store.engine().speculative();
    assert!(prepare(&store, &tx, nrese_reasoner::eval::NEVER).proves_consistency);
}

#[test]
fn embedded_low_memory_stops_maintenance_of_an_existing_upper_bound() {
    if !nrese_exec::memory::READS_MEMORY {
        return;
    }
    let store = store(1);
    let mut tx = store.engine().speculative();
    source::intern_vocabulary(&tx);
    let ontology = source::read_snapshot(tx.base(), Arc::clone(store.dl().bounds.kept())).unwrap();
    let normalised = nrese_owl::normalise(&ontology);
    // Seed the existing U1 without a watch, as if the process had been below its
    // budget at the prior revision. The next preparation must carry its own watch.
    let upper = Upper::build(
        &ontology,
        &normalised,
        tx.base(),
        &|t| Some(tx.intern(t)),
        nrese_reasoner::eval::NEVER,
        store.dl().bounds.kept(),
    )
    .unwrap();
    publish(
        &mut lock(&store.dl().bounds.state),
        State {
            revision: tx.base().revision(),
            upper: Ok(Arc::new(upper)),
            last: "read",
            compiled: store.dl().bounds.compiled(),
            taxonomy: None,
            rules: false,
        },
    );
    let quad = nrese_rdf::Quad::new(
        nrese_rdf::NamedNode::new_unchecked("urn:test:b"),
        nrese_rdf::vocab::rdf::TYPE,
        nrese_rdf::NamedNode::new_unchecked("urn:test:A"),
        nrese_rdf::GraphName::DefaultGraph,
    );
    tx.insert(quad.as_ref());
    let prepared = prepare(&store, &tx, nrese_reasoner::eval::NEVER);
    assert!(!prepared.proves_consistency);
    assert!(matches!(prepared.prepared, Prepared::Rebuilt(upper) if upper.is_err()));
}

/// REAS-2 fix check r1 (R2-F3, U1). Spec (design note, rule 2: a stop is an error the
/// consumer has to handle, never an answer): U1's change for a commit ends at its stop. U1
/// built over an existential axiom and one member, then a change deleting 16 × `POLL_UNITS`
/// facts U1 doesn't hold, the stop firing at its first ask after the first of their view
/// lookups: the change gives up, and no more than one lookup is made after the stop fired.
/// Read as "not in the view", a stopped lookup kept every deleted fact after it, and the
/// keep looked each one up.
#[test]
fn a_change_s_deleted_facts_are_looked_up_until_its_stop() {
    use crate::witness::{self, Site};
    let n = 16 * u64::from(nrese_exec::POLL_UNITS);
    let store = pooled(&members(1));
    let tx = store.engine().speculative();
    source::intern_vocabulary(&tx);
    let ontology = source::read_snapshot(tx.base(), Arc::clone(store.dl().bounds.kept())).unwrap();
    let normalised = nrese_owl::normalise(&ontology);
    let upper = Upper::build(
        &ontology,
        &normalised,
        tx.base(),
        &|t| Some(tx.intern(t)),
        nrese_reasoner::eval::NEVER,
        store.dl().bounds.kept(),
    )
    .unwrap();
    // Ids no term has: facts in neither U1's program nor the view.
    let deleted: Vec<Triple> = (0..n).map(|i| [1 << 40 | i, 1 << 41, 1 << 42]).collect();
    let first = witness::FirstWork::new(Site::InView);
    let stop = || first.ask();
    let change = upper.change(tx.base(), &[], &deleted, &stop);
    let after = first.after();
    assert!(
        after <= 1,
        "{after} view lookups after the stop fired, of {n}: at most the one that saw it"
    );
    assert!(change.is_err(), "a stopped change gives up");
}
