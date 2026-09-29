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

/// The reasoning marker: set by rematerialisation, kept by v2 commits, dropped by writes
/// that don't maintain inferences, and persistent across restarts.
#[test]
fn reasoning_marker_tracks_whether_inferences_are_current() {
    let dir = tempfile::tempdir().unwrap();
    let config = nrese_store::StoreConfig {
        mode: nrese_store::StoreMode::OnDisk,
        data_dir: dir.path().to_path_buf(),
        ontology_path: None,
        query_cache_bytes: 0,
    };
    let ruleset = nrese_reasoner::v2::rulesets::Ruleset::Owl2Rl;
    let data = format!("<{EX}Cat> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}Animal>");
    {
        let store = Arc::new(StoreService::new(config.clone()).expect("store"));
        assert_eq!(store.materialised_for(), None);
        store.rematerialise(ruleset).expect("rematerialise");
        assert_eq!(store.materialised_for().as_deref(), Some("owl2-rl"));
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
        assert_eq!(store.materialised_for().as_deref(), Some("owl2-rl"));
        // An ungated write drops it; rematerialisation restores it.
        store
            .execute_update(&SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ <{EX}tom> a <{EX}Cat> }}"
            )))
            .expect("update");
        assert_eq!(store.materialised_for(), None);
        store.rematerialise(ruleset).expect("rematerialise");
    }
    let store = StoreService::new(config).expect("reopen");
    assert_eq!(store.materialised_for().as_deref(), Some("owl2-rl"));
    assert_eq!(store.clear_inferred().expect("clear"), 1);
    assert_eq!(store.materialised_for(), None);
}
