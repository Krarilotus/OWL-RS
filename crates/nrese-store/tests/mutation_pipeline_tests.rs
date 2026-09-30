mod support;

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationError, MutationPipeline, MutationTicket, SparqlUpdateRequest,
    StoreService,
};
use support::in_memory_store_config;

fn pipeline(mode: ReasoningMode) -> MutationPipeline {
    let store = StoreService::new(in_memory_store_config()).expect("store");
    MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn insert(triple: &str) -> MutationCommand {
    MutationCommand::Update(SparqlUpdateRequest::new(format!(
        "INSERT DATA {{ {triple} }}"
    )))
}

fn contains(pipeline: &MutationPipeline, triple: &str) -> bool {
    let result = pipeline
        .store()
        .execute_query_str(&format!("ASK {{ {triple} }}"))
        .expect("ask");
    String::from_utf8(result.payload)
        .expect("utf8")
        .contains("true")
}

/// Regression for audit finding F2: a mutation whose caller gave up must never commit.
#[test]
fn cancelled_ticket_prevents_commit() {
    let pipeline = pipeline(ReasoningMode::Disabled);
    let triple = "<http://example.com/ghost> <http://example.com/p> <http://example.com/o>";
    let ticket = MutationTicket::new();
    assert!(ticket.cancel());

    let result = pipeline.apply(insert(triple), &ticket);

    assert!(matches!(result, Err(MutationError::Cancelled)));
    assert!(!contains(&pipeline, triple));
}

#[test]
fn committed_mutation_cannot_be_cancelled_afterwards() {
    let pipeline = pipeline(ReasoningMode::Disabled);
    let triple = "<http://example.com/a> <http://example.com/p> <http://example.com/b>";
    let ticket = MutationTicket::new();

    pipeline.apply(insert(triple), &ticket).expect("commit");

    assert!(
        !ticket.cancel(),
        "a started commit must not report as cancelled"
    );
    assert!(contains(&pipeline, triple));
}

#[test]
fn gate_rejection_is_recorded_and_not_committed() {
    let pipeline = pipeline(ReasoningMode::Owl2Rl);
    let setup = "<http://example.com/A> <http://www.w3.org/2002/07/owl#disjointWith> <http://example.com/B> .
                 <http://example.com/x> a <http://example.com/A> .";
    pipeline
        .apply(insert(setup), &MutationTicket::new())
        .expect("consistent setup");

    let conflicting = "<http://example.com/x> a <http://example.com/B>";
    let result = pipeline.apply(insert(conflicting), &MutationTicket::new());

    let Err(MutationError::Rejected(reject)) = result else {
        panic!("expected a rejection, got {result:?}");
    };
    assert!(!contains(&pipeline, conflicting));
    // The explanation names the rule and its premises, and the commit's own statement.
    let explanation = reject.explanation.as_ref().expect("explanation");
    assert_eq!(explanation.violated_constraint, "cax-dw");
    assert_eq!(explanation.focus_resource, "http://example.com/x");
    assert_eq!(explanation.evidence.len(), 3);
    let trigger = reject
        .attribution
        .as_ref()
        .and_then(|a| a.likely_commit_trigger())
        .expect("trigger");
    assert_eq!(trigger.2, "http://example.com/B");
    let run = pipeline.last_reasoning_run().expect("recorded");
    assert_eq!(run.status, nrese_core::ReasonerRunStatus::Rejected);
}

fn delete(triple: &str) -> MutationCommand {
    MutationCommand::Update(SparqlUpdateRequest::new(format!(
        "DELETE DATA {{ {triple} }}"
    )))
}

const EX: &str = "http://example.com/";

/// Reasoner v2 (R4): commits keep the inferred stack equal to the closure, and queries see
/// it through the default (materialised) read model.
#[test]
fn owl2_rl_inferences_follow_commits() {
    let pipeline = pipeline(ReasoningMode::Owl2Rl);
    let ticket = MutationTicket::new;
    let schema = format!(
        "<{EX}Cat> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}Animal> .
         <{EX}knows> a <http://www.w3.org/2002/07/owl#SymmetricProperty> .
         <{EX}ancestor> a <http://www.w3.org/2002/07/owl#TransitiveProperty> ."
    );
    pipeline.apply(insert(&schema), &ticket()).expect("schema");
    let facts = format!(
        "<{EX}tom> a <{EX}Cat> . <{EX}a> <{EX}knows> <{EX}b> .
         <{EX}x> <{EX}ancestor> <{EX}y> . <{EX}y> <{EX}ancestor> <{EX}z> ."
    );
    pipeline.apply(insert(&facts), &ticket()).expect("facts");
    for inferred in [
        format!("<{EX}tom> a <{EX}Animal>"),
        format!("<{EX}b> <{EX}knows> <{EX}a>"),
        format!("<{EX}x> <{EX}ancestor> <{EX}z>"),
    ] {
        assert!(contains(&pipeline, &inferred), "missing {inferred}");
    }
    // Deleting the support retracts the inference.
    pipeline
        .apply(
            delete(&format!("<{EX}y> <{EX}ancestor> <{EX}z>")),
            &ticket(),
        )
        .expect("delete");
    assert!(!contains(
        &pipeline,
        &format!("<{EX}x> <{EX}ancestor> <{EX}z>")
    ));
    assert!(contains(&pipeline, &format!("<{EX}tom> a <{EX}Animal>")));
    // An asserted statement that is also derivable counts as asserted only.
    pipeline
        .apply(insert(&format!("<{EX}tom> a <{EX}Animal>")), &ticket())
        .expect("explicit");
    let inferred_before = pipeline.store().stats().expect("stats").inferred_count;
    pipeline
        .apply(delete(&format!("<{EX}tom> a <{EX}Animal>")), &ticket())
        .expect("delete explicit");
    // Still derivable, so still visible: it moved back to the inferred stack.
    assert!(contains(&pipeline, &format!("<{EX}tom> a <{EX}Animal>")));
    let inferred_after = pipeline.store().stats().expect("stats").inferred_count;
    assert_eq!(inferred_after, inferred_before + 1);
}

#[test]
fn owl2_rl_rejects_inconsistent_commits() {
    let pipeline = pipeline(ReasoningMode::Owl2Rl);
    let setup = format!(
        "<{EX}A> <http://www.w3.org/2002/07/owl#disjointWith> <{EX}B> .
         <{EX}x> a <{EX}A> ."
    );
    pipeline
        .apply(insert(&setup), &MutationTicket::new())
        .expect("consistent setup");
    let conflicting = format!("<{EX}x> a <{EX}B>");
    let result = pipeline.apply(insert(&conflicting), &MutationTicket::new());
    let Err(MutationError::Rejected(reject)) = result else {
        panic!("expected a rejection, got {result:?}");
    };
    assert!(reject.detail.contains("cax-dw"), "{}", reject.detail);
    assert!(!contains(&pipeline, &conflicting));
}

#[test]
fn rematerialise_installs_the_closure_of_existing_data() {
    // Data committed without reasoning, then the ruleset is switched on.
    let plain = pipeline(ReasoningMode::Disabled);
    let data = format!(
        "<{EX}Cat> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}Animal> .
         <{EX}tom> a <{EX}Cat> ."
    );
    plain
        .apply(insert(&data), &MutationTicket::new())
        .expect("data");
    let inferred = format!("<{EX}tom> a <{EX}Animal>");
    assert!(!contains(&plain, &inferred));
    let report = plain
        .store()
        .rematerialise(nrese_reasoner::v2::rulesets::Ruleset::Rdfs)
        .expect("rematerialise");
    assert_eq!((report.inferred_inserted, report.inferred_deleted), (1, 0));
    assert!(contains(&plain, &inferred));
    // Unchanged data: no new revision.
    let again = plain
        .store()
        .rematerialise(nrese_reasoner::v2::rulesets::Ruleset::Rdfs)
        .expect("again");
    assert_eq!(again.revision, report.revision);
}

/// Reasoner v2's commit path (the delta executor over the engine) against rematerialisation:
/// after every random commit, recomputing the closure changes nothing. Includes named
/// graphs: a triple deleted from one graph but asserted in another stays a fact.
#[test]
fn owl2_rl_commits_keep_the_inferred_stack_exact() {
    let pipeline = pipeline(ReasoningMode::Owl2Rl);
    let mut state = 0x5DEE_CE66_D1CE_4E5Du64;
    let mut next = move |n: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % n
    };
    let rdfs = "http://www.w3.org/2000/01/rdf-schema#";
    let owl = "http://www.w3.org/2002/07/owl#";
    let random_triple = |next: &mut dyn FnMut(u64) -> u64| -> String {
        let c = |n: u64| format!("<{EX}C{n}>");
        let p = |n: u64| format!("<{EX}p{n}>");
        let i = |n: u64| format!("<{EX}i{n}>");
        match next(9) {
            0 => format!("{} <{rdfs}subClassOf> {}", c(next(4)), c(next(4))),
            1 => format!("{} <{rdfs}subPropertyOf> {}", p(next(3)), p(next(3))),
            2 => format!("{} <{rdfs}domain> {}", p(next(3)), c(next(4))),
            3 => {
                let kind = ["TransitiveProperty", "SymmetricProperty"][next(2) as usize];
                format!("{} a <{owl}{kind}>", p(next(3)))
            }
            4 => format!("{} <{owl}inverseOf> {}", p(next(3)), p(next(3))),
            5 | 6 => format!("{} a {}", i(next(6)), c(next(4))),
            _ => format!("{} {} {}", i(next(6)), p(next(3)), i(next(6))),
        }
    };
    let mut asserted: Vec<(String, Option<u64>)> = Vec::new();
    let (mut commits, mut changed) = (0, 0);
    // NRESE_FUZZ_CASES widens the sweep locally.
    let rounds: u64 = std::env::var("NRESE_FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    for _ in 0..rounds {
        let deleting = !asserted.is_empty() && next(3) == 0;
        let (triple, graph) = if deleting {
            asserted.swap_remove(next(asserted.len() as u64) as usize)
        } else {
            let graph = (next(3) == 0).then(|| next(2));
            (random_triple(&mut next), graph)
        };
        let data = match graph {
            Some(g) => format!("GRAPH <{EX}g{g}> {{ {triple} }}"),
            None => triple.clone(),
        };
        let command = if deleting {
            delete(&data)
        } else {
            insert(&data)
        };
        if pipeline.apply(command, &MutationTicket::new()).is_ok() {
            commits += 1;
            if !deleting {
                asserted.push((triple, graph));
            }
        } else if deleting {
            asserted.push((triple, graph));
        }
        let check = pipeline
            .store()
            .rematerialise(nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl)
            .expect("rematerialise");
        assert_eq!(
            (check.inferred_inserted, check.inferred_deleted),
            (0, 0),
            "after {commits} commits the inferred stack differs from the closure; last: {} {data}
asserted: {asserted:#?}",
            if deleting { "DELETE" } else { "INSERT" }
        );
        changed += usize::from(check.inferred > 0);
    }
    assert!(
        commits > 100 && changed > 50,
        "{commits} commits, {changed} with inferences"
    );
}

/// Read models (reasoner-v2 design §4.3): asserted, inferred or both, per request, by
/// parameter or GraphDB's pseudo-graphs `onto:explicit` / `onto:implicit`.
#[test]
fn queries_choose_asserted_inferred_or_both() {
    let pipeline = pipeline(ReasoningMode::Owl2Rl);
    let data = format!(
        "<{EX}Cat> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}Animal> .
         <{EX}tom> a <{EX}Cat> ."
    );
    pipeline
        .apply(insert(&data), &MutationTicket::new())
        .expect("data");
    let ask = |query: String, model: Option<nrese_store::ReadModel>, default_graphs: &[&str]| {
        let mut request = nrese_store::SparqlQueryRequest::new(query);
        request.read_model = model;
        request.default_graphs = default_graphs.iter().map(|g| (*g).to_owned()).collect();
        let result = pipeline.store().execute_query(&request).expect("ask");
        String::from_utf8(result.payload)
            .expect("utf8")
            .contains("true")
    };
    let inferred = format!("ASK {{ <{EX}tom> a <{EX}Animal> }}");
    let asserted = format!("ASK {{ <{EX}tom> a <{EX}Cat> }}");
    use nrese_store::ReadModel::{Asserted, Inferred, Materialised};
    for (model, sees_inferred, sees_asserted) in [
        (None, true, true),
        (Some(Materialised), true, true),
        (Some(Asserted), false, true),
        (Some(Inferred), true, false),
    ] {
        assert_eq!(
            ask(inferred.clone(), model, &[]),
            sees_inferred,
            "{model:?}"
        );
        assert_eq!(
            ask(asserted.clone(), model, &[]),
            sees_asserted,
            "{model:?}"
        );
    }
    let explicit = "http://www.ontotext.com/explicit";
    let implicit = "http://www.ontotext.com/implicit";
    let from = |graph: &str, body: &str| format!("ASK FROM <{graph}> {{ {body} }}");
    let tom_animal = format!("<{EX}tom> a <{EX}Animal>");
    let tom_cat = format!("<{EX}tom> a <{EX}Cat>");
    assert!(!ask(from(explicit, &tom_animal), None, &[]));
    assert!(ask(from(explicit, &tom_cat), None, &[]));
    assert!(ask(from(implicit, &tom_animal), None, &[]));
    assert!(!ask(from(implicit, &tom_cat), None, &[]));
    // The protocol's default-graph-uri works the same way.
    assert!(!ask(format!("ASK {{ {tom_animal} }}"), None, &[explicit]));
}

/// The reasoning state: set by rematerialisation, kept by v2 commits, dropped by writes
/// that don't maintain inferences, and persistent across restarts.
#[test]
fn reasoning_state_tracks_whether_inferences_are_current() {
    let dir = tempfile::tempdir().unwrap();
    let config = nrese_store::StoreConfig {
        mode: nrese_store::StoreMode::OnDisk,
        data_dir: dir.path().to_path_buf(),
        query_cache_bytes: 0,
        ..nrese_store::StoreConfig::default()
    };
    let ruleset = nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl;
    let current = |store: &StoreService| {
        store
            .reasoning_state()
            .is_some_and(|state| state.is_current_for(ruleset))
    };
    let data = format!("<{EX}Cat> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}Animal>");
    {
        let store = Arc::new(StoreService::new(config.clone()).expect("store"));
        assert!(!current(&store));
        store.rematerialise(ruleset).expect("rematerialise");
        assert!(current(&store));
        // A v2 commit keeps it.
        let pipeline = MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Rl,
            ))),
        );
        pipeline
            .apply(insert(&data), &MutationTicket::new())
            .expect("commit");
        assert!(current(&store));
        // An ungated write drops it; rematerialisation restores it.
        store
            .execute_update(&SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ <{EX}tom> a <{EX}Cat> }}"
            )))
            .expect("update");
        assert!(!current(&store));
        store.rematerialise(ruleset).expect("rematerialise");
    }
    let store = StoreService::new(config.clone()).expect("reopen");
    assert!(current(&store));
    assert_eq!(
        store.consistency(),
        nrese_store::ConsistencyStatus::Consistent
    );
    assert_eq!(store.clear_inferred().expect("clear"), 1);
    assert!(!current(&store));
    drop(store);
    // A state file from a build with other semantics (another fingerprint) isn't current.
    let path = dir.path().join("reasoning.state");
    std::fs::write(
        &path,
        "ruleset owl2-rl\nfingerprint 0000000000000001\nviolations 0\n",
    )
    .expect("write state");
    let store = StoreService::new(config).expect("reopen");
    assert!(store.reasoning_state().is_some());
    assert!(!current(&store), "semantics changed: not current");
}

/// Inconsistent data imported without reasoning puts the store in quarantine once it is
/// materialised: unrelated consistent commits are accepted (and revalidate), new
/// violations are still rejected, and a repair ends the quarantine.
#[test]
fn inconsistent_baselines_are_quarantined_until_repaired() {
    let store = Arc::new(StoreService::new(nrese_store::StoreConfig::in_memory()).expect("store"));
    // Imported with reasoning off: x is in two disjoint classes.
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "INSERT DATA {{ <{EX}A> <http://www.w3.org/2002/07/owl#disjointWith> <{EX}B> .
                           <{EX}x> a <{EX}A> , <{EX}B> }}"
        )))
        .expect("import");
    let ruleset = nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl;
    let report = store.rematerialise(ruleset).expect("rematerialise");
    assert_eq!(report.violations, 1);
    assert_eq!(
        store.consistency(),
        nrese_store::ConsistencyStatus::Inconsistent { violations: 1 }
    );
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    // An unrelated consistent write is accepted; the store stays in quarantine.
    pipeline
        .apply(
            insert(&format!("<{EX}y> a <{EX}A>")),
            &MutationTicket::new(),
        )
        .expect("unrelated commit");
    assert!(matches!(
        store.consistency(),
        nrese_store::ConsistencyStatus::Inconsistent { .. }
    ));
    // A new violation is still rejected.
    assert!(
        pipeline
            .apply(
                insert(&format!("<{EX}y> a <{EX}B>")),
                &MutationTicket::new()
            )
            .is_err()
    );
    // The repair: x leaves B.
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "DELETE DATA {{ <{EX}x> a <{EX}B> }}"
            ))),
            &MutationTicket::new(),
        )
        .expect("repair");
    assert_eq!(
        store.consistency(),
        nrese_store::ConsistencyStatus::Consistent
    );
}

/// A write asserting that an individual differs from itself is rejected (eq-ref's
/// consequence, checked without materialising `x sameAs x`).
#[test]
fn self_difference_is_rejected_on_commit() {
    let store = Arc::new(StoreService::new(nrese_store::StoreConfig::in_memory()).expect("store"));
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    let result = pipeline.apply(
        insert(&format!(
            "<{EX}x> <http://www.w3.org/2002/07/owl#differentFrom> <{EX}x>"
        )),
        &MutationTicket::new(),
    );
    assert!(result.is_err(), "{result:?}");
    assert_eq!(store.stats().expect("stats").quad_count, 0);
}

/// Ontology axioms the reasoner can't use are reported, with decoded terms: a full
/// materialisation reports all of them, a commit those it introduced.
#[test]
fn unusable_list_axioms_are_reported() {
    const OWL: &str = "http://www.w3.org/2002/07/owl#";
    const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    let store = Arc::new(StoreService::new(nrese_store::StoreConfig::in_memory()).expect("store"));
    let plain = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Disabled,
        ))),
    );
    let cyclic = format!(
        "<{EX}D> <{OWL}unionOf> <{EX}c> . <{EX}c> <{RDF}first> <{EX}A> . <{EX}c> <{RDF}rest> <{EX}c> ."
    );
    plain
        .apply(insert(&cyclic), &MutationTicket::new())
        .expect("data");
    let report = store
        .rematerialise(nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl)
        .expect("rematerialise");
    assert_eq!(report.diagnostics_total, 1);
    let diagnostic = &report.diagnostics[0];
    assert_eq!(
        (
            diagnostic.kind,
            diagnostic.rules,
            diagnostic.subject.as_str()
        ),
        ("cyclic-list", "cls-uni", format!("{EX}D").as_str())
    );
    assert_eq!(diagnostic.node.as_deref(), Some(format!("{EX}c").as_str()));
    assert!(
        diagnostic.message.contains("returns to node"),
        "{}",
        diagnostic.message
    );
    assert_eq!(
        store.last_materialisation().expect("recorded").diagnostics,
        report.diagnostics
    );

    let owl = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    let run = |data: &str| {
        owl.apply(insert(data), &MutationTicket::new())
            .expect("commit");
        owl.last_reasoning_run().expect("run")
    };
    let unrelated = run(&format!("<{EX}x> <{EX}p> <{EX}y> ."));
    assert!(
        unrelated.diagnostics.is_empty(),
        "{:?}",
        unrelated.diagnostics
    );
    let broken = run(&format!(
        "<{EX}C> <{OWL}intersectionOf> <{EX}a> . <{EX}a> <{RDF}first> <{EX}A> ."
    ));
    assert_eq!(broken.diagnostics_total, 1, "{:?}", broken.diagnostics);
    assert_eq!(broken.diagnostics[0].kind, "malformed-list");
    assert_eq!(
        broken.diagnostics[0].node.as_deref(),
        Some(format!("{EX}a").as_str())
    );
}

/// A write cancelled while its reasoning runs (a request timeout) stops promptly: nothing
/// of it is committed, neither asserted nor inferred, and the writer is free again.
#[test]
fn cancelled_reasoning_commits_stop_promptly_and_change_nothing() {
    const RDFS_SUB: &str = "http://www.w3.org/2000/01/rdf-schema#subClassOf";
    let store = Arc::new(StoreService::new(nrese_store::StoreConfig::in_memory()).expect("store"));
    let plain = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Disabled,
        ))),
    );
    // A class chain C0 ⊑ … ⊑ C100 and 10,000 instances of D: linking D into the chain
    // derives a million types.
    let mut data: Vec<String> = (0..100)
        .map(|i| format!("<{EX}C{i}> <{RDFS_SUB}> <{EX}C{}> .", i + 1))
        .collect();
    data.extend((0..10_000).map(|j| format!("<{EX}x{j}> a <{EX}D> .")));
    plain
        .apply(insert(&data.join("\n")), &MutationTicket::new())
        .expect("data");
    store
        .rematerialise(nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl)
        .expect("rematerialise");
    let owl = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    let count = || {
        let result = store
            .execute_query_str("SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }")
            .expect("count");
        String::from_utf8(result.payload).expect("utf8")
    };
    let (revision, before) = (store.current_revision(), count());

    let ticket = MutationTicket::new();
    let canceller = {
        let ticket = ticket.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            assert!(ticket.cancel(), "the commit must not have started");
            std::time::Instant::now()
        })
    };
    let result = owl.apply(insert(&format!("<{EX}D> <{RDFS_SUB}> <{EX}C0> .")), &ticket);
    let returned = std::time::Instant::now();
    let cancelled = canceller.join().expect("canceller");
    assert!(
        matches!(result, Err(MutationError::Cancelled)),
        "{result:?}"
    );
    let latency = returned.saturating_duration_since(cancelled);
    assert!(latency < std::time::Duration::from_secs(2), "{latency:?}");
    assert_eq!(store.current_revision(), revision);
    assert_eq!(count(), before);
    assert!(!contains(&owl, &format!("<{EX}x0> a <{EX}C0>")));

    owl.apply(
        insert(&format!("<{EX}y> a <{EX}C0> .")),
        &MutationTicket::new(),
    )
    .expect("the writer is free");
    assert!(contains(&owl, &format!("<{EX}y> a <{EX}C100>")));
}

/// `rdfs-full`: the axiomatic triples hold from the first commit on, their consequences
/// are drawn, and deleting the data a rule derived an axiom from again doesn't retract it.
#[test]
fn full_rdfs_keeps_its_axioms() {
    let pipeline = pipeline(ReasoningMode::RdfsFull);
    let rdf = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    let rdfs = "http://www.w3.org/2000/01/rdf-schema#";
    let axiom = format!("<{rdf}type> <{rdf}type> <{rdf}Property>");
    let fact = format!("<http://example.com/tom> <{rdf}type> <http://example.com/Cat>");
    pipeline
        .apply(insert(&fact), &MutationTicket::new())
        .expect("commit");
    assert!(contains(&pipeline, &axiom));
    // rdfs4a, rdf:type's axiomatic domain, and rdfs1 for xsd:string.
    assert!(contains(
        &pipeline,
        &format!("<http://example.com/tom> <{rdf}type> <{rdfs}Resource>")
    ));
    assert!(contains(
        &pipeline,
        &format!("<http://www.w3.org/2001/XMLSchema#string> <{rdfs}subClassOf> <{rdfs}Literal>")
    ));
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "DELETE DATA {{ {fact} }}"
            ))),
            &MutationTicket::new(),
        )
        .expect("delete");
    assert!(!contains(&pipeline, &fact));
    assert!(contains(&pipeline, &axiom), "an axiom is never retracted");
    // A rematerialisation computes the same.
    let before = support::inferred_statements(pipeline.store()).unwrap();
    pipeline
        .store()
        .rematerialise(nrese_reasoner::v2::rulesets::Ruleset::RdfsFull)
        .unwrap();
    let mut after = support::inferred_statements(pipeline.store()).unwrap();
    let mut before = before;
    before.sort();
    after.sort();
    assert_eq!(before, after);
}

/// `reasoner.unnamed_classes = "skip"` (W7): memberships in an unnamed union range that
/// nothing consumes stay out of the store, on rematerialisation and on commits, while
/// everything else equals the full closure; a commit that makes the class consumed
/// brings them back.
#[test]
fn unnamed_union_memberships_are_left_out_on_request() {
    let rdf = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    let ex = "http://example.com/";
    let ontology = format!(
        "@prefix ex: <{ex}> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:livesIn rdfs:range [ a owl:Class ; owl:unionOf ( ex:City ex:Village ) ] .
         ex:livesIn rdfs:domain ex:Person .
         ex:Person rdfs:subClassOf ex:Agent .
         ex:ann ex:livesIn ex:jena ."
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("o.ttl");
    std::fs::write(&path, ontology).unwrap();
    let run = |hide: bool| -> (MutationPipeline, Vec<(String, String, String)>) {
        let config = nrese_store::StoreConfig {
            hide_unnamed_classes: hide,
            ..in_memory_store_config().with_ontology(path.clone())
        };
        let store = std::sync::Arc::new(StoreService::new(config).unwrap());
        store
            .rematerialise(nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl)
            .unwrap();
        let pipeline = MutationPipeline::new(
            std::sync::Arc::clone(&store),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Rl,
            ))),
        );
        pipeline
            .apply(
                insert(&format!("<{ex}bob> <{ex}livesIn> <{ex}weimar>")),
                &MutationTicket::new(),
            )
            .unwrap();
        // Blank node labels differ between stores.
        let label = |t: String| {
            if t.starts_with("_:") {
                "_:b".to_owned()
            } else {
                t
            }
        };
        let mut inferred: Vec<_> = support::inferred_statements(pipeline.store())
            .unwrap()
            .into_iter()
            .map(|(s, p, o)| (label(s), p, label(o)))
            .collect();
        inferred.sort();
        (pipeline, inferred)
    };
    let (_, full) = run(false);
    let (lean_pipeline, lean) = run(true);
    let memberships = |statements: &[(String, String, String)]| -> usize {
        statements
            .iter()
            .filter(|(_, p, o)| p == &format!("{rdf}type") && o.starts_with("_:"))
            .count()
    };
    assert_eq!(
        memberships(&full),
        2,
        "jena and weimar, in the full closure"
    );
    assert_eq!(memberships(&lean), 0);
    let without: Vec<_> = full
        .iter()
        .filter(|(_, p, o)| !(p == &format!("{rdf}type") && o.starts_with("_:")))
        .cloned()
        .collect();
    assert_eq!(lean, without, "everything else is the same");
    // Making the class consumed brings its memberships back (and what they entail).
    lean_pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "INSERT {{ ?u <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{ex}Place> }} WHERE {{ ?u <http://www.w3.org/2002/07/owl#unionOf> ?l }}"
            ))),
            &MutationTicket::new(),
        )
        .unwrap();
    let after = support::inferred_statements(lean_pipeline.store()).unwrap();
    assert_eq!(memberships(&after), 2);
    assert!(after.contains(&(
        format!("{ex}weimar"),
        format!("{rdf}type"),
        format!("{ex}Place")
    )));
}

/// A stopped rematerialisation changes nothing and says so.
#[test]
fn a_stopped_rematerialisation_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("o.ttl");
    std::fs::write(
        &path,
        "@prefix ex: <http://example.com/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A rdfs:subClassOf ex:B . ex:x a ex:A .",
    )
    .unwrap();
    let store = StoreService::new(in_memory_store_config().with_ontology(path)).unwrap();
    let ruleset = nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl;
    let error = store.rematerialise_until(ruleset, &|| true).unwrap_err();
    assert!(matches!(
        error,
        nrese_store::StoreError::MaterialisationCancelled
    ));
    assert!(store.reasoning_state().is_none());
    assert!(support::inferred_statements(&store).unwrap().is_empty());
    store.rematerialise(ruleset).unwrap();
    assert!(!support::inferred_statements(&store).unwrap().is_empty());
}

/// OWL 2 RL's datatype consistency (dt-not-type, dt-diff): a value outside a data
/// property's range datatype, and a functional data property with two different values,
/// are inconsistencies, on materialisation and on commits; equal values and values in
/// range are not.
#[test]
fn datatype_consistency() {
    let ex = "http://example.com/";
    let xsd = "http://www.w3.org/2001/XMLSchema#";
    let schema = format!(
        "@prefix ex: <{ex}> . @prefix owl: <http://www.w3.org/2002/07/owl#> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . @prefix xsd: <{xsd}> .
         ex:age a owl:DatatypeProperty, owl:FunctionalProperty ; rdfs:range xsd:nonNegativeInteger .
         ex:weight rdfs:range xsd:double .
         ex:name rdfs:range xsd:string .
         "
    );
    let violations = |data: &str| -> usize {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o.ttl");
        std::fs::write(&path, format!("{schema}{data}")).unwrap();
        let store = StoreService::new(in_memory_store_config().with_ontology(path)).unwrap();
        store
            .rematerialise(nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl)
            .unwrap()
            .violations
    };
    assert_eq!(
        violations("ex:a ex:age 42 ; ex:weight 1.5e0 ; ex:name \"Ann\" ."),
        0
    );
    // Equal values under two lexical forms are one value.
    assert_eq!(violations("ex:a ex:age 42, \"042\"^^xsd:integer ."), 0);
    assert!(violations("ex:a ex:age 42, 43 .") > 0, "two ages");
    assert!(violations("ex:a ex:age -1 .") > 0, "below the range");
    assert!(violations("ex:a ex:age \"forty\" .") > 0, "a string");
    assert!(
        violations("ex:a ex:weight 70 .") > 0,
        "an integer is no double"
    );
    assert!(
        violations("ex:a ex:name \"Ann\"@en .") > 0,
        "a language string is no xsd:string"
    );

    // On a commit: rejected, with the rule named.
    let pipeline = pipeline(ReasoningMode::Owl2Rl);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.ttl");
    std::fs::write(&path, &schema).unwrap();
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ <{ex}age> <http://www.w3.org/2000/01/rdf-schema#range> <{xsd}nonNegativeInteger> }}"
            ))),
            &MutationTicket::new(),
        )
        .unwrap();
    let error = pipeline
        .apply(
            insert(&format!("<{ex}b> <{ex}age> \"-5\"^^<{xsd}integer>")),
            &MutationTicket::new(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("dt-not-type"), "{error}");
    pipeline
        .apply(
            insert(&format!("<{ex}b> <{ex}age> \"5\"^^<{xsd}integer>")),
            &MutationTicket::new(),
        )
        .unwrap();
}
