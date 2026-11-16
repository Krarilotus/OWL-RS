use crate::support::Writes;
use std::sync::Arc;

use crate::support::in_memory_store_config;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationError, MutationPipeline, MutationTicket, SparqlUpdateRequest,
    StoreService,
};

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

    let result = pipeline.apply(insert(triple), &nrese_store::Requester::all(), &ticket);

    assert!(matches!(result, Err(MutationError::Cancelled)));
    assert!(!contains(&pipeline, triple));
}

#[test]
fn committed_mutation_cannot_be_cancelled_afterwards() {
    let pipeline = pipeline(ReasoningMode::Disabled);
    let triple = "<http://example.com/a> <http://example.com/p> <http://example.com/b>";
    let ticket = MutationTicket::new();

    pipeline
        .apply(insert(triple), &nrese_store::Requester::all(), &ticket)
        .expect("commit");

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
        .apply(
            insert(setup),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("consistent setup");

    let conflicting = "<http://example.com/x> a <http://example.com/B>";
    let result = pipeline.apply(
        insert(conflicting),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    );

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
    assert_eq!(run.status, nrese_reasoner::ReasonerRunStatus::Rejected);
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
    pipeline
        .apply(insert(&schema), &nrese_store::Requester::all(), &ticket())
        .expect("schema");
    let facts = format!(
        "<{EX}tom> a <{EX}Cat> . <{EX}a> <{EX}knows> <{EX}b> .
         <{EX}x> <{EX}ancestor> <{EX}y> . <{EX}y> <{EX}ancestor> <{EX}z> ."
    );
    pipeline
        .apply(insert(&facts), &nrese_store::Requester::all(), &ticket())
        .expect("facts");
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
            &nrese_store::Requester::all(),
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
        .apply(
            insert(&format!("<{EX}tom> a <{EX}Animal>")),
            &nrese_store::Requester::all(),
            &ticket(),
        )
        .expect("explicit");
    let inferred_before = pipeline.store().stats().expect("stats").inferred_count;
    pipeline
        .apply(
            delete(&format!("<{EX}tom> a <{EX}Animal>")),
            &nrese_store::Requester::all(),
            &ticket(),
        )
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
        .apply(
            insert(&setup),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("consistent setup");
    let conflicting = format!("<{EX}x> a <{EX}B>");
    let result = pipeline.apply(
        insert(&conflicting),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    );
    let Err(MutationError::Rejected(reject)) = result else {
        panic!("expected a rejection, got {result:?}");
    };
    assert!(reject.detail.contains("cax-dw"), "{}", reject.detail);
    assert!(!contains(&pipeline, &conflicting));
}

/// The G3 safety audit (7 October 2026): a commit the OWL 2 RL gate rejects listed its
/// premises whatever the committer could read. A committer who may read and write `open`
/// only is rejected for what the secret schema makes inconsistent. Its own graphs violate
/// nothing by themselves, so the rejection shows no premise (Q07: even its own statement
/// would tell that the secret schema relates `:A` to what it committed). A committer who
/// reads everything sees them all.
#[test]
fn an_rl_rejection_shows_no_premise_of_a_graph_the_committer_cannot_read() {
    use nrese_sparql::GraphAccess;
    use nrese_store::{ReadScope, Requester, WriteScope};
    let setup = format!(
        "GRAPH <{EX}secret> {{ <{EX}A> <http://www.w3.org/2002/07/owl#disjointWith> <{EX}B> .
           <{EX}C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}B> }}
         GRAPH <{EX}open> {{ <{EX}x> a <{EX}A> }}"
    );
    let conflicting = format!("GRAPH <{EX}open> {{ <{EX}x> a <{EX}C> }}");
    let access = Arc::new(GraphAccess {
        graphs: vec![format!("{EX}open")],
        ..GraphAccess::default()
    });
    let committers = [
        Requester::new(
            ReadScope::Graphs(Arc::clone(&access)),
            WriteScope::Graphs(Arc::clone(&access)),
        ),
        Requester::all(),
    ];
    for (restricted, committer) in [true, false].into_iter().zip(committers) {
        let pipeline = pipeline(ReasoningMode::Owl2Rl);
        pipeline
            .apply(
                insert(&setup),
                &nrese_store::Requester::all(),
                &MutationTicket::new(),
            )
            .expect("consistent setup");
        let result = pipeline.apply(insert(&conflicting), &committer, &MutationTicket::new());
        let Err(MutationError::Rejected(reject)) = result else {
            panic!("expected a rejection, got {result:?}");
        };
        let secret = |text: &str| text.contains("disjointWith") || text.contains(&format!("{EX}B"));
        if restricted {
            // Nothing of the secret graph, nor what it relates to the committer's statement.
            assert!(reject.explanation.is_none(), "{reject:?}");
            assert!(reject.attribution.is_none(), "{reject:?}");
            assert!(!secret(&reject.detail), "{}", reject.detail);
            assert!(
                !reject.detail.contains(&format!("{EX}A")),
                "{}",
                reject.detail
            );
            assert!(
                reject
                    .detail
                    .contains("Only statements in graphs you can read are shown"),
                "{}",
                reject.detail
            );
        } else {
            let explanation = reject.explanation.expect("an explanation");
            let shown: Vec<String> = explanation
                .evidence
                .iter()
                .map(|e| format!("{} {} {}", e.subject, e.predicate, e.object))
                .collect();
            assert_eq!(shown.len(), 3, "{shown:#?}");
            assert!(shown.iter().any(|s| secret(s)), "{shown:#?}");
        }
    }
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
        .apply(
            insert(&data),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("data");
    let inferred = format!("<{EX}tom> a <{EX}Animal>");
    assert!(!contains(&plain, &inferred));
    let report = plain
        .store()
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Rdfs)
        .expect("rematerialise");
    assert_eq!((report.inferred_inserted, report.inferred_deleted), (1, 0));
    assert!(contains(&plain, &inferred));
    // Unchanged data: no new revision.
    let again = plain
        .store()
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Rdfs)
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
        if pipeline
            .apply(
                command,
                &nrese_store::Requester::all(),
                &MutationTicket::new(),
            )
            .is_ok()
        {
            commits += 1;
            if !deleting {
                asserted.push((triple, graph));
            }
        } else if deleting {
            asserted.push((triple, graph));
        }
        let check = pipeline
            .store()
            .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
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
        .apply(
            insert(&data),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("data");
    let ask = |query: String, model: Option<nrese_store::ReadModel>, default_graphs: &[&str]| {
        let mut request = nrese_store::SparqlQueryRequest::all(query);
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
    let ruleset = nrese_reasoner::rulesets::Ruleset::Owl2Rl;
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
            .apply(
                insert(&data),
                &nrese_store::Requester::all(),
                &MutationTicket::new(),
            )
            .expect("commit");
        assert!(current(&store));
        // A write without reasoning drops it; rematerialisation restores it.
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
    // The inferred subclass statement and OWL 2 RL's 32 datatype axioms (dt-type1).
    assert_eq!(store.clear_inferred().expect("clear"), 1 + 32);
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
    let ruleset = nrese_reasoner::rulesets::Ruleset::Owl2Rl;
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
            &nrese_store::Requester::all(),
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
                &nrese_store::Requester::all(),
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
            &nrese_store::Requester::all(),
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
        &nrese_store::Requester::all(),
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
        .apply(
            insert(&cyclic),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("data");
    let report = store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
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
        owl.apply(
            insert(data),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
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
        .apply(
            insert(&data.join("\n")),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("data");
    store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
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
    let result = owl.apply(
        insert(&format!("<{EX}D> <{RDFS_SUB}> <{EX}C0> .")),
        &nrese_store::Requester::all(),
        &ticket,
    );
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
        &nrese_store::Requester::all(),
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
        .apply(
            insert(&fact),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
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
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("delete");
    assert!(!contains(&pipeline, &fact));
    assert!(contains(&pipeline, &axiom), "an axiom is never retracted");
    // A rematerialisation computes the same.
    let before = crate::support::inferred_statements(pipeline.store()).unwrap();
    pipeline
        .store()
        .rematerialise(nrese_reasoner::rulesets::Ruleset::RdfsFull)
        .unwrap();
    let mut after = crate::support::inferred_statements(pipeline.store()).unwrap();
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
            .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
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
                &nrese_store::Requester::all(),
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
        let mut inferred: Vec<_> = crate::support::inferred_statements(pipeline.store())
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
    let before = lean_pipeline.store().current_revision();
    lean_pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "INSERT {{ ?u <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{ex}Place> }} WHERE {{ ?u <http://www.w3.org/2002/07/owl#unionOf> ?l }}"
            ))),
            &nrese_store::Requester::all(), &MutationTicket::new(),
        )
        .unwrap();
    let after = crate::support::inferred_statements(lean_pipeline.store()).unwrap();
    assert_eq!(memberships(&after), 2);
    assert!(after.contains(&(
        format!("{ex}weimar"),
        format!("{rdf}type"),
        format!("{ex}Place")
    )));
    assert_eq!(lean_pipeline.store().current_revision(), before + 1);
    lean_pipeline
        .apply(
            insert(&format!("<{ex}cara> <{ex}livesIn> <{ex}erfurt>")),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
    let after = crate::support::inferred_statements(lean_pipeline.store()).unwrap();
    assert_eq!(
        memberships(&after),
        3,
        "the cached program restores new memberships too"
    );
    assert!(after.contains(&(
        format!("{ex}erfurt"),
        format!("{rdf}type"),
        format!("{ex}Place")
    )));
    assert_eq!(lean_pipeline.store().current_revision(), before + 2);
}

#[test]
fn reviving_a_hidden_class_rejects_its_new_violation_before_commit() {
    let store = Arc::new(
        StoreService::new(nrese_store::StoreConfig {
            hide_unnamed_classes: true,
            ..in_memory_store_config()
        })
        .unwrap(),
    );
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    let prefixes = "PREFIX ex: <http://example.com/> PREFIX owl: <http://www.w3.org/2002/07/owl#> PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "{prefixes}
            INSERT DATA {{ ex:p rdfs:range [ a owl:Class ; owl:unionOf (ex:A ex:B) ] .
                ex:s ex:p ex:x . ex:x a ex:C }}"
            ))),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
    assert!(!contains(
        &pipeline,
        "<http://example.com/p> <http://www.w3.org/2000/01/rdf-schema#range> ?u . <http://example.com/x> a ?u FILTER(isBlank(?u))"
    ));
    pipeline
        .apply(
            insert(
                "<http://example.com/unrelated> <http://example.com/note> <http://example.com/v>",
            ),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
    let before = store.current_revision();
    let result = pipeline.apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "{prefixes}
            INSERT {{ ?u owl:disjointWith ex:C }} WHERE {{ ex:p rdfs:range ?u FILTER(isBlank(?u)) }}"
            ))),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        );
    assert!(
        matches!(result, Err(MutationError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(store.current_revision(), before);
}

/// A preloaded hidden class, optionally beside an unrelated quarantined clash.
fn hidden_range_pipeline(extra: &str, shacl: bool) -> MutationPipeline {
    let store = Arc::new(
        StoreService::new(nrese_store::StoreConfig {
            hide_unnamed_classes: true,
            equality_compact: true,
            shacl_gate: if shacl {
                nrese_store::ShaclGate::Enforce(nrese_store::GateSeverity::Violation)
            } else {
                nrese_store::ShaclGate::Off
            },
            ..in_memory_store_config()
        })
        .unwrap(),
    );
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "
        PREFIX ex: <http://example.com/> PREFIX owl: <http://www.w3.org/2002/07/owl#>
        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
        INSERT DATA {{ ex:p rdfs:range [ a owl:Class ; owl:unionOf (ex:A ex:B) ] .
            ex:s ex:p ex:x . ex:x a ex:C . ex:a owl:sameAs ex:b . ex:b ex:note ex:v .
            {extra} }}
    "
        )))
        .unwrap();
    store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
        .unwrap();
    MutationPipeline::new(
        store,
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

fn revive_hidden_range(consumer: &str) -> MutationCommand {
    MutationCommand::Update(SparqlUpdateRequest::new(format!(
        "
        PREFIX ex: <http://example.com/> PREFIX owl: <http://www.w3.org/2002/07/owl#>
        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
        DELETE {{ ex:a owl:sameAs ex:b }}
        INSERT {{ ?u {consumer} }} WHERE {{ ex:p rdfs:range ?u FILTER(isBlank(?u)) }}
    "
    )))
}

#[test]
fn a_revival_and_split_preserve_quarantine_and_reject_only_new_clashes() {
    let extra = "ex:OldA owl:disjointWith ex:OldB . ex:old a ex:OldA, ex:OldB .";
    let pipeline = hidden_range_pipeline(extra, false);
    pipeline
        .apply(
            revive_hidden_range("rdfs:subClassOf ex:Place"),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("old clash is tolerated");
    assert!(matches!(
        pipeline.store().consistency(),
        nrese_store::ConsistencyStatus::Inconsistent { .. }
    ));
    assert!(contains(
        &pipeline,
        "<http://example.com/x> a <http://example.com/Place>"
    ));
    assert!(!contains(
        &pipeline,
        "<http://example.com/a> <http://example.com/note> <http://example.com/v>"
    ));
    // A separate old clash must not conceal the newly restored membership's clash.
    let pipeline = hidden_range_pipeline(extra, false);
    let before = pipeline.store().current_revision();
    let result = pipeline.apply(
        revive_hidden_range("owl:disjointWith ex:C"),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    );
    assert!(
        matches!(result, Err(MutationError::Rejected(_))),
        "{result:?}"
    );
    assert_eq!(pipeline.store().current_revision(), before);
    assert!(contains(
        &pipeline,
        "<http://example.com/a> <http://www.w3.org/2002/07/owl#sameAs> <http://example.com/b>"
    ));
}

#[test]
fn shacl_checks_restored_memberships_before_a_revival_and_split_commit() {
    let pipeline = hidden_range_pipeline(&format!("
        GRAPH <{}> {{
            ex:shape a <http://www.w3.org/ns/shacl#NodeShape> ;
              <http://www.w3.org/ns/shacl#targetNode> ex:x ;
              <http://www.w3.org/ns/shacl#property> [
                <http://www.w3.org/ns/shacl#path> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> ;
                <http://www.w3.org/ns/shacl#nodeKind> <http://www.w3.org/ns/shacl#IRI> ] .
        }}", nrese_store::DEFAULT_SHAPES_GRAPH), true);
    let before = pipeline.store().current_revision();
    let result = pipeline.apply(
        revive_hidden_range("rdfs:subClassOf ex:Place"),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    );
    let Err(MutationError::Rejected(rejection)) = result else {
        panic!("{result:?}")
    };
    assert!(rejection.detail.contains("SHACL"), "{}", rejection.detail);
    assert_eq!(pipeline.store().current_revision(), before);
}

#[test]
fn dl_commits_a_revival_and_split_with_its_final_closure() {
    let baseline = hidden_range_pipeline("", false);
    // The baseline's closure is owl2-rl's. owl2-dl's program keeps anonymous individuals out
    // of its keys and records its own fingerprint, so a commit under owl2-dl would first
    // rebuild that closure, one revision of its own. Materialised with owl2-dl's program, the
    // commit is the one revision this test counts.
    let dl = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl).materialised_program();
    baseline.store().rematerialise(dl.unwrap()).unwrap();
    let pipeline = MutationPipeline::new(
        Arc::clone(baseline.store()),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    let before = pipeline.store().current_revision();
    pipeline
        .apply(
            revive_hidden_range("rdfs:subClassOf ex:Place"),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
    assert_eq!(pipeline.store().current_revision(), before + 1);
    assert!(contains(
        &pipeline,
        "<http://example.com/x> a <http://example.com/Place>"
    ));
    assert!(contains(
        &pipeline,
        "<http://example.com/p> <http://www.w3.org/2000/01/rdf-schema#range> ?u . <http://example.com/x> a ?u FILTER(isBlank(?u))"
    ));
    assert!(!contains(
        &pipeline,
        "<http://example.com/a> <http://example.com/note> <http://example.com/v>"
    ));
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
    let ruleset = nrese_reasoner::rulesets::Ruleset::Owl2Rl;
    let error = store.rematerialise_until(ruleset, &|| true).unwrap_err();
    assert!(matches!(
        error,
        nrese_store::StoreError::MaterialisationCancelled
    ));
    assert!(store.reasoning_state().is_none());
    assert!(
        crate::support::inferred_statements(&store)
            .unwrap()
            .is_empty()
    );
    store.rematerialise(ruleset).unwrap();
    assert!(
        !crate::support::inferred_statements(&store)
            .unwrap()
            .is_empty()
    );
}

/// A commit that breaks one of OWL 2 RL's datatype rules is rejected, with the rule named;
/// one that doesn't commits. Which values break which rule is the reasoner's
/// (`engine::datatypes`' tests).
#[test]
fn a_commit_breaking_a_datatype_rule_is_rejected_with_the_rule_named() {
    let ex = "http://example.com/";
    let xsd = "http://www.w3.org/2001/XMLSchema#";
    let pipeline = pipeline(ReasoningMode::Owl2Rl);
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ <{ex}age> <http://www.w3.org/2000/01/rdf-schema#range> <{xsd}nonNegativeInteger> }}"
            ))),
            &nrese_store::Requester::all(), &MutationTicket::new(),
        )
        .unwrap();
    let error = pipeline
        .apply(
            insert(&format!("<{ex}b> <{ex}age> \"-5\"^^<{xsd}integer>")),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("dt-not-type"), "{error}");
    // An ill-typed value has no data value, so it is in no range (RDF 1.2 Concepts §3.4, OWL 2
    // Profiles table 8): rejected under owl2-rl.
    let error = pipeline
        .apply(
            insert(&format!("<{ex}b> <{ex}age> \"abc\"^^<{xsd}integer>")),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect_err("an ill-typed value under a range is an inconsistency");
    assert!(error.to_string().contains("dt-not-type"), "{error}");
    pipeline
        .apply(
            insert(&format!("<{ex}b> <{ex}age> \"5\"^^<{xsd}integer>")),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
}

/// A ranged owl2-rl commit is judged by the datatype's whole lexical space (XSDVAL review
/// r1): a dateTimeStamp needs its timezone whatever its precision (XML Schema 1.1 Part 2
/// §3.4.28.1, R1-W1), and hexBinary is hex digit pairs without a sign (§3.3.15.2, R1-W3):
/// both are rejected with dt-not-type. The review's controls commit: the long fraction with
/// a timezone, the long fraction as a dateTime (beyond what is represented: not judged), and
/// valid hexBinary in either case or empty; an ordinary dateTimeStamp without a timezone is
/// rejected. Every row is reported.
#[test]
fn ranged_commits_are_judged_by_the_whole_lexical_space() {
    let ex = "http://example.com/";
    let xsd = "http://www.w3.org/2001/XMLSchema#";
    let range = "http://www.w3.org/2000/01/rdf-schema#range";
    let rdf = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    let pipeline = pipeline(ReasoningMode::Owl2Rl);
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ <{ex}stamp> <{range}> <{xsd}dateTimeStamp> .
                               <{ex}when> <{range}> <{xsd}dateTime> .
                               <{ex}hex> <{range}> <{xsd}hexBinary> .
                               <{ex}xml> <{range}> <{rdf}XMLLiteral> }}"
            ))),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
    let fraction = "2000-01-01T00:00:00.1234567890123456789";
    let rows = [
        (
            "stamp",
            format!("\"{fraction}\"^^<{xsd}dateTimeStamp>"),
            true,
        ),
        ("hex", format!("\"+F\"^^<{xsd}hexBinary>"), true),
        (
            "stamp",
            format!("\"2000-01-01T00:00:00\"^^<{xsd}dateTimeStamp>"),
            true,
        ),
        (
            "stamp",
            format!("\"{fraction}Z\"^^<{xsd}dateTimeStamp>"),
            false,
        ),
        ("when", format!("\"{fraction}\"^^<{xsd}dateTime>"), false),
        ("hex", format!("\"0f\"^^<{xsd}hexBinary>"), false),
        ("hex", format!("\"0F\"^^<{xsd}hexBinary>"), false),
        ("hex", format!("\"\"^^<{xsd}hexBinary>"), false),
        // rdf:XMLLiteral: well-balanced, self-contained XML content (RDF 1.1 Concepts); a
        // character reference is unsigned digits (XML 1.0 §4.1); a tag ends in `>` or `/>`.
        ("xml", format!("\"&#x+41;\"^^<{rdf}XMLLiteral>"), true),
        ("xml", format!("\"<a /x></a>\"^^<{rdf}XMLLiteral>"), true),
        (
            "xml",
            format!("\"<b>&#x41;</b>\"^^<{rdf}XMLLiteral>"),
            false,
        ),
    ];
    let mut wrong = Vec::new();
    for (n, (property, literal, rejected)) in rows.iter().enumerate() {
        let result = pipeline.apply(
            insert(&format!("<{ex}s{n}> <{ex}{property}> {literal}")),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        );
        let ok = match &result {
            Err(e) => *rejected && e.to_string().contains("dt-not-type"),
            Ok(_) => !*rejected,
        };
        if !ok {
            wrong.push(format!("{property} {literal}: {result:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The reasoning marker engine-v2 621bf318 (`SEMANTICS_VERSION` 3) wrote for the store of
/// [`a_marker_from_older_datatype_semantics_is_rechecked`] after rematerialising it under
/// owl2-rl: 0 violations (fleet probe xsdval-probe-marker-56fd5f94).
const OLD_MARKER: &str =
    "ruleset owl2-rl\nfingerprint a94cf90230548146\nviolations 0\nlists nil-class\n";

/// A store whose reasoning marker a build with older datatype semantics wrote (engine-v2
/// 621bf318, which didn't judge an ill-typed literal under a range, so recorded this data
/// consistent) isn't current once the semantics change (rulesets::SEMANTICS_VERSION): on
/// reopen it is rechecked, and the inconsistency is reported (XSDVAL review r1, R1-W2).
#[test]
fn a_marker_from_older_datatype_semantics_is_rechecked() {
    let dir = tempfile::tempdir().unwrap();
    let config = nrese_store::StoreConfig {
        mode: nrese_store::StoreMode::OnDisk,
        data_dir: dir.path().to_path_buf(),
        query_cache_bytes: 0,
        ..nrese_store::StoreConfig::default()
    };
    let ruleset = nrese_reasoner::rulesets::Ruleset::Owl2Rl;
    let xsd = "http://www.w3.org/2001/XMLSchema#";
    {
        let store = StoreService::new(config.clone()).expect("store");
        store
            .execute_update(&SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ <{EX}age> <http://www.w3.org/2000/01/rdf-schema#range> <{xsd}nonNegativeInteger> .
                               <{EX}b> <{EX}age> \"abc\"^^<{xsd}integer> }}"
            )))
            .expect("import");
    }
    // The marker 621bf318 wrote for this store after rematerialising it (fleet probe
    // xsdval-probe-marker-56fd5f94).
    std::fs::write(dir.path().join("reasoning.state"), OLD_MARKER).expect("write marker");
    let store = StoreService::new(config).expect("reopen");
    assert!(
        !store
            .reasoning_state()
            .is_some_and(|state| state.is_current_for(ruleset)),
        "an older build's marker is current"
    );
    let report = store.rematerialise(ruleset).expect("rematerialise");
    assert!(report.violations >= 1, "{report:?}");
    assert_eq!(
        store.consistency(),
        nrese_store::ConsistencyStatus::Inconsistent {
            violations: report.violations
        }
    );
}

/// A full OWL 2 RL materialisation by representatives of the `owl:sameAs` classes
/// (`reasoner.equality = "representatives"`, the default) stores the same statements as
/// one with the replacement rules: on data whose equalities come from `sameAs` chains, a
/// functional and an inverse-functional property, and reach subjects, objects and a
/// predicate.
#[test]
fn materialisation_by_representatives_equals_replication() {
    let owl = "http://www.w3.org/2002/07/owl#";
    let data = format!(
        "@prefix ex: <{EX}> . @prefix owl: <{owl}> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:a owl:sameAs ex:b . ex:b owl:sameAs ex:c . ex:d owl:sameAs ex:a .
         ex:hasMother a owl:FunctionalProperty . ex:email a owl:InverseFunctionalProperty .
         ex:kim ex:hasMother ex:m1 , ex:m2 . ex:p1 ex:email \"x@y\" . ex:p2 ex:email \"x@y\" .
         ex:a ex:knows ex:kim . ex:p1 ex:age 30 . ex:c a ex:Person . ex:Person rdfs:subClassOf ex:Agent .
         ex:knows owl:sameAs ex:meets . ex:meets a owl:SymmetricProperty .
         ex:m2 ex:livesIn ex:berlin . ex:berlin owl:sameAs ex:berlinCity ."
    );
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("equality.ttl");
    std::fs::write(&file, &data).unwrap();
    let everything = |representatives: bool| {
        let config = nrese_store::StoreConfig {
            equality_by_representatives: representatives,
            ..in_memory_store_config()
        };
        let store = StoreService::new(config).expect("store");
        store
            .bulk_load(&nrese_store::BulkLoadRequest {
                files: vec![file.clone()],
                replace: false,
                graph: nrese_store::GraphTarget::DefaultGraph,
                invalid: Some(nrese_store::InvalidStatements::Strict),
            })
            .expect("load");
        store
            .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)
            .expect("rematerialise");
        let mut quads: Vec<String> = store
            .statements(
                &nrese_store::ReadContext::all(),
                &nrese_store::StatementPattern::default(),
            )
            .expect("statements")
            .iter()
            .map(ToString::to_string)
            .collect();
        quads.sort();
        quads
    };
    let replicated = everything(false);
    let by_representatives = everything(true);
    assert!(
        replicated
            .iter()
            .any(|q| q.contains("meets") && q.contains("kim")),
        "the equalities reach the data"
    );
    assert_eq!(by_representatives, replicated);
    // Without asserted sameAs: equalities only from the functional and inverse-functional
    // properties (the first pass finds them, then the representative closure).
    let derived_only = format!(
        "@prefix ex: <{EX}> . @prefix owl: <{owl}> .
         ex:hasMother a owl:FunctionalProperty . ex:email a owl:InverseFunctionalProperty .
         ex:kim ex:hasMother ex:m1 , ex:m2 . ex:p1 ex:email \"x@y\" . ex:p2 ex:email \"x@y\" .
         ex:m1 ex:knows ex:p1 . ex:p2 ex:age 30 . ex:m2 a ex:Person ."
    );
    std::fs::write(&file, &derived_only).unwrap();
    let replicated = everything(false);
    assert!(
        replicated
            .iter()
            .any(|q| q.contains("m2") && q.contains("knows") && q.contains("p2")),
        "the derived equalities reach the data"
    );
    assert_eq!(everything(true), replicated);
}

/// A retired pipeline changes nothing, its preflight included: after the new pipeline
/// (OWL 2 RL) has established its closure, a write through the old handle (RDFS) is
/// refused without first rematerialising the store with the old rules. Before, it did:
/// the inverse property's inference disappeared and the store recorded RDFS
/// (independent review of 3 October 2026, C1).
#[test]
fn a_retired_pipeline_leaves_the_new_closure_alone() {
    let store = Arc::new(StoreService::new(in_memory_store_config()).unwrap());
    let config = |mode| ReasonerConfig::for_mode(mode);
    let old = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(config(ReasoningMode::Rdfs))),
    );
    let update = |pipeline: &MutationPipeline, text: &str| {
        pipeline.apply(
            MutationCommand::Update(SparqlUpdateRequest::new(text)),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
    };
    update(
        &old,
        "INSERT DATA { <urn:p> <http://www.w3.org/2002/07/owl#inverseOf> <urn:q> . <urn:a> <urn:p> <urn:b> }",
    )
    .unwrap();
    let new = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(config(ReasoningMode::Owl2Rl))),
    );
    old.retire();
    update(&new, "INSERT DATA { <urn:c> <urn:p> <urn:d> }").unwrap();
    let owl = config(ReasoningMode::Owl2Rl)
        .materialised_program()
        .unwrap();
    let inferred = || {
        store
            .count(
                &nrese_store::ReadContext::all(),
                &nrese_store::StatementPattern::default(),
            )
            .unwrap()
    };
    let before = inferred();
    assert!(store.reasoning_is_current(&owl));
    assert!(matches!(
        update(&old, "INSERT DATA { <urn:e> <urn:p> <urn:f> }"),
        Err(nrese_store::MutationError::Retired)
    ));
    assert!(
        store.reasoning_is_current(&owl),
        "the old rules took over the store"
    );
    assert_eq!(inferred(), before, "the closure changed");
}

/// A pipeline another one took over (a repository's reasoning changed) refuses writes, so
/// none applies its rules to an inferred stack made for others; the new one writes on.
#[test]
fn a_retired_pipeline_refuses_writes() {
    let store = Arc::new(StoreService::new(in_memory_store_config()).unwrap());
    let reasoner = |mode| Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode)));
    let old = MutationPipeline::new(Arc::clone(&store), reasoner(ReasoningMode::Rdfs));
    let update = |pipeline: &MutationPipeline, text: &str| {
        pipeline.apply(
            MutationCommand::Update(SparqlUpdateRequest::new(text)),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
    };
    update(&old, "INSERT DATA { <urn:a> <urn:p> 1 }").unwrap();
    let new = MutationPipeline::new(Arc::clone(&store), reasoner(ReasoningMode::Owl2Rl));
    old.retire();
    assert!(old.is_retired());
    assert!(matches!(
        update(&old, "INSERT DATA { <urn:b> <urn:p> 2 }"),
        Err(nrese_store::MutationError::Retired)
    ));
    update(&new, "INSERT DATA { <urn:c> <urn:p> 3 }").unwrap();
    assert_eq!(
        store
            .count(
                &nrese_store::ReadContext::all().infer(false),
                &nrese_store::StatementPattern::default(),
            )
            .unwrap(),
        2
    );
}

/// `:K owl:hasKey ( :k1 … :kn )`, and `:x` and `:y` in `:K` with the same value on every
/// key property: prp-key (OWL 2 Profiles §4.3, table 5) makes them one individual, so
/// `:x :name "Y"` holds through `:y`'s (eq-rep-s). Each `:ki` is declared an object property,
/// as its values are: an undeclared key property is a QL concern of its own.
fn long_key(n: usize) -> String {
    let keys: Vec<String> = (1..=n).map(|i| format!("<{EX}k{i}>")).collect();
    let mut data = format!(
        "<{EX}K> <http://www.w3.org/2002/07/owl#hasKey> ( {} ) .",
        keys.join(" ")
    );
    for key in &keys {
        data.push_str(&format!(
            " {key} a <http://www.w3.org/2002/07/owl#ObjectProperty> ."
        ));
    }
    for x in ["x", "y"] {
        data.push_str(&format!(" <{EX}{x}> a <{EX}K> ."));
        for i in 1..=n {
            data.push_str(&format!(" <{EX}{x}> <{EX}k{i}> <{EX}v{i}> ."));
        }
    }
    data.push_str(&format!(" <{EX}y> <{EX}name> \"Y\" ."));
    data
}

/// A key's rule has two atoms per property and two more, and a rule holds at most 256:
/// a key of 127 properties is one rule, one of 128 is reported (`list-too-long`, prp-key)
/// and not built, never a failed commit (docs/spec/reasoning-semantics.md, "Lists").
#[test]
fn a_key_past_the_rule_format_is_reported_and_not_built() {
    for n in [127, 128] {
        let owl = pipeline(ReasoningMode::Owl2Rl);
        owl.apply(
            insert(&long_key(n)),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap_or_else(|error| panic!("{n} properties: {error}"));
        let equal = contains(&owl, &format!("<{EX}x> <{EX}name> \"Y\""));
        let reported = owl
            .last_reasoning_run()
            .expect("run")
            .diagnostics
            .iter()
            .any(|d| d.kind == "list-too-long" && d.rules == "prp-key");
        assert_eq!((equal, reported), (n == 127, n == 128), "{n} properties");
    }
}

/// A list axiom the rules leave out (reported, not built) is a missing rule: an answer it
/// would change isn't `complete` because of it. Under `owl2-rl`, where the QL rewriting can
/// certify an answer (`ql_rewriting = "on"`), each in a store of its own: a key past 127
/// properties, an intersection past 256 classes and a chain past 250 properties. Every row is
/// checked; the wrong ones are listed.
#[test]
fn an_unbuilt_key_leaves_no_answer_complete_under_owl2_rl() {
    let rows = [
        (
            "key 128",
            long_key(128),
            format!("<{EX}x> <{EX}name> \"Y\""),
        ),
        (
            "intersection 257",
            long_intersection(257),
            format!("<{EX}x> a <{EX}C>"),
        ),
        (
            "chain 251",
            long_chain(251),
            format!("<{EX}a0> <{EX}chain> <{EX}a251>"),
        ),
    ];
    let mut wrong = Vec::new();
    for (row, data, ask) in rows {
        let pipeline = || ql_pipeline(in_memory_store_config());
        if std::panic::catch_unwind(|| left_out_status(&pipeline(), &data, &ask)).is_err() {
            wrong.push(row);
        }
    }
    assert!(wrong.is_empty(), "{wrong:?}");
}

/// The aliased key built by a first commit is left out by a later one that adds any list fact:
/// that commit grounds the lists again over the stored closure, which holds equality's copies
/// of the key's cells (`rdf:first` to each alias), so the key's list has 2^7 member sequences,
/// past 64 (`too-many-list-variants`). The later commit's key match (`:z` with `:x`'s values)
/// is then not concluded: `:x :name "Z"` is entailed, so the answer is true or not `complete`.
#[test]
fn a_later_list_fact_leaves_an_aliased_key_out_and_no_answer_complete() {
    let pipeline = ql_pipeline(in_memory_store_config());
    let write = |data: &str| {
        let ticket = MutationTicket::new();
        (pipeline.apply(insert(data), &nrese_store::Requester::all(), &ticket)).expect("the data");
    };
    write(&aliased_key(7));
    let mut later = format!(
        "<{EX}C> <http://www.w3.org/2002/07/owl#intersectionOf> ( <{EX}A1> <{EX}A2> ) . \
         <{EX}z> a <{EX}K> . <{EX}z> <{EX}name> \"Z\" ."
    );
    for i in 1..=7 {
        later.push_str(&format!(" <{EX}z> <{EX}k{i}> <{EX}v{i}> ."));
    }
    write(&later);
    left_out_answer(pipeline.store(), &format!("<{EX}x> <{EX}name> \"Z\""));
}

/// Aliases (`owl:sameAs`) of a key's 7 properties leave the key built by the commit that
/// writes it: equality by representatives, the default, reads each cell of its list as one
/// member, not 2^7 member sequences. Its answer is true. It isn't `complete`, one commit
/// early: the commit's program, ground over the closure it stored (expanded, with equality's
/// `rdf:first` copies), leaves the key out for the next commit, and the mark follows that
/// program. A sound mark, less precise than the closure; grounding the commit's lists over
/// representatives too would make it exact. The other way is guarded elsewhere: were the mark
/// ever missing where a list is left out,
/// `a_later_list_fact_leaves_an_aliased_key_out_and_no_answer_complete` (this key, left out by
/// the next commit) and every row of
/// `an_unbuilt_key_leaves_no_answer_complete_under_owl2_rl` would answer `complete` and turn red.
#[test]
fn aliases_leave_a_key_built() {
    let pipeline = ql_pipeline(in_memory_store_config());
    let ask = format!("ASK {{ <{EX}x> <{EX}name> \"Y\" }}");
    pipeline
        .apply(
            insert(&aliased_key(7)),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("the data");
    let answer = (pipeline.store())
        .execute_query(&nrese_store::SparqlQueryRequest::all(ask))
        .expect("answered");
    let (payload, status) = (String::from_utf8(answer.payload), answer.completeness);
    println!("aliased key: {payload:?} with {status:?}");
    assert!(payload.expect("utf8").contains("true"));
    let status = status.expect("a status");
    assert!(!status.complete, "{status:?}");
    assert!(
        (status.reasons.iter()).any(|r| r.text.contains("list axiom")),
        "{status:?}"
    );
}

/// A store holding a key the rules leave out, opened again with its closure current (so
/// not rebuilt): its answers aren't `complete` either.
#[test]
fn a_reopened_store_with_an_unbuilt_key_leaves_no_answer_complete() {
    let dir = tempfile::tempdir().expect("dir");
    let ask = format!("<{EX}x> <{EX}name> \"Y\"");
    left_out_status(
        &ql_pipeline(nrese_store::StoreConfig::on_disk(dir.path())),
        &long_key(128),
        &ask,
    );
    left_out_answer(
        ql_pipeline(nrese_store::StoreConfig::on_disk(dir.path())).store(),
        &ask,
    );
}

/// A read inside a transaction whose pending operations delete a key's `owl:hasKey` fact finds
/// no list left out in its own data, and its snapshot carries the committed revision's number.
/// The transaction ends without a commit, so the committed revision is as it was: its key of
/// 128 properties, past what one rule holds, is left out, and `:x :name "Y"` is entailed (the
/// key makes `:x` and `:y` one; `:y :name "Y"`). A plain read of it after the transaction is
/// therefore true or not `complete`, whatever the pending read found.
#[test]
fn a_pending_read_leaves_the_committed_revisions_mark() {
    let pipeline = ql_pipeline(in_memory_store_config());
    let store = pipeline.store();
    (pipeline.apply(
        insert(&long_key(128)),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    ))
    .expect("the data");
    let ask = format!("<{EX}x> <{EX}name> \"Y\"");
    let prepared = (store.prepare_query(&nrese_store::SparqlQueryRequest::all(format!(
        "ASK {{ {ask} }}"
    ))))
    .expect("prepared");
    let delete = format!("DELETE WHERE {{ <{EX}K> <http://www.w3.org/2002/07/owl#hasKey> ?list }}");
    let ops = vec![nrese_store::StatementOp::Update(SparqlUpdateRequest::new(
        delete,
    ))];
    let mut pending = None;
    store
        .run_query_pending_reporting(
            &nrese_store::StatementsRequest::new(ops),
            &prepared,
            &nrese_store::CancellationToken::new(),
            &mut Vec::<u8>::new(),
            |status| pending = status,
        )
        .expect("the pending read");
    println!("pending read: {pending:?}");
    left_out_answer(store, &ask);
}

/// A revision read first with nothing left out is recorded clean. A commit on it then adds a
/// key of 128 properties, past what one rule holds: the commit itself must record the key left
/// out, as the record would otherwise carry the clean range on to the new revision unscanned.
/// `:x :name "Y"` is entailed (the key makes `:x` and `:y` one), so a read after the commit is
/// true or not `complete`.
#[test]
fn a_commit_after_a_clean_read_records_its_key_left_out() {
    let pipeline = ql_pipeline(in_memory_store_config());
    let write = |data: &str| {
        let ticket = MutationTicket::new();
        (pipeline.apply(insert(data), &nrese_store::Requester::all(), &ticket)).expect("the data");
    };
    let ask = format!("<{EX}x> <{EX}name> \"Y\"");
    write(&format!("<{EX}y> <{EX}name> \"Y\" ."));
    let clean = (pipeline.store())
        .execute_query(&nrese_store::SparqlQueryRequest::all(format!(
            "ASK {{ {ask} }}"
        )))
        .expect("answered")
        .completeness;
    println!("clean read: {clean:?}");
    assert!(
        clean.as_ref().is_some_and(|c| c.complete),
        "the first read must be complete, so that it records its revision clean: {clean:?}"
    );
    write(&long_key(128));
    left_out_answer(pipeline.store(), &ask);
}

/// [`long_key`] of 128 properties, its `owl:hasKey` fact and list in `:secret`. A reader of
/// the default graph only (inferences by support) reads the revision first; a reader of every
/// graph then reads it too. Its key is left out, and `:x :name "Y"` entailed, so the second
/// read is true or not `complete`, whatever the first reader's view found.
#[test]
fn a_restricted_read_leaves_a_hidden_keys_mark() {
    let pipeline = ql_pipeline(in_memory_store_config());
    let key = long_key(128);
    let (axiom, rest) = key.split_once(" .").expect("the key's axiom first");
    let data = format!("GRAPH <{EX}secret> {{ {axiom} . }} {rest}");
    (pipeline.apply(
        insert(&data),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    ))
    .expect("the data");
    let ask = format!("<{EX}x> <{EX}name> \"Y\"");
    restricted_read(pipeline.store(), &ask);
    left_out_answer(pipeline.store(), &ask);
}

/// The aliased key of [`a_later_list_fact_leaves_an_aliased_key_out_and_no_answer_complete`],
/// its `owl:sameAs` facts in `:secret`: the later commit leaves the key out through equality's
/// `rdf:first` copies, derived through `:secret`. A reader of the default graph only
/// (inferences by support) reads that revision first; a reader of every graph then reads it
/// too: `:x :name "Z"` is entailed, so the second read is true or not `complete`. The control,
/// a store of its own with the same data read by a reader of every graph alone, is not
/// `complete`: what differs is the restricted read first.
#[test]
fn a_restricted_read_leaves_an_aliased_keys_mark() {
    let ask = format!("<{EX}x> <{EX}name> \"Z\"");
    let control = aliased_key_with_hidden_aliases();
    let answer = (control.store())
        .execute_query(&nrese_store::SparqlQueryRequest::all(format!(
            "ASK {{ {ask} }}"
        )))
        .expect("answered");
    println!("control read: {:?}", answer.completeness);
    assert!(
        answer.completeness.as_ref().is_some_and(|c| !c.complete),
        "control: a reader of every graph alone must be told not complete: {:?}",
        answer.completeness
    );
    let pipeline = aliased_key_with_hidden_aliases();
    restricted_read(pipeline.store(), &ask);
    left_out_answer(pipeline.store(), &ask);
}

/// A transaction inserts a key of 128 properties and reads inside it: its pending operations
/// aren't reasoned over before the commit, and the key is past what one rule holds, so
/// `:x :name "Y"` (entailed through the key) isn't answered and the read isn't `complete`.
/// The committed revision has nothing left out: only the pending read's own scan finds it.
#[test]
fn a_pending_key_left_out_leaves_the_pending_read_not_complete() {
    let pipeline = ql_pipeline(in_memory_store_config());
    let store = pipeline.store();
    (pipeline.apply(
        insert(&format!("<{EX}w> <{EX}name> \"W\" .")),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    ))
    .expect("the data");
    let ask = format!("ASK {{ <{EX}x> <{EX}name> \"Y\" }}");
    let prepared =
        (store.prepare_query(&nrese_store::SparqlQueryRequest::all(ask))).expect("prepared");
    let key = format!("INSERT DATA {{ {} }}", long_key(128));
    let ops = vec![nrese_store::StatementOp::Update(SparqlUpdateRequest::new(
        key,
    ))];
    let mut answer = Vec::new();
    let mut pending = None;
    store
        .run_query_pending_reporting(
            &nrese_store::StatementsRequest::new(ops),
            &prepared,
            &nrese_store::CancellationToken::new(),
            &mut answer,
            |status| pending = status,
        )
        .expect("the pending read");
    let answer = String::from_utf8(answer).expect("utf8");
    println!("pending insert read: {answer} with {pending:?}");
    let left_out = |c: &nrese_sparql::Completeness| {
        !c.complete && (c.reasons.iter()).any(|r| r.text.contains("list axiom"))
    };
    assert!(
        pending.as_ref().is_some_and(left_out),
        "not complete for the key left out: {answer} with {pending:?}"
    );
}

/// Two readers of one revision with views of their own: one reads `:secret`, where the key's
/// list cell is equated with the cell that holds its members, the other doesn't. The first's
/// view keeps equality's `rdf:first`/`rdf:rest` copies and leaves no list out; the second's
/// drops them (derived through `:secret`), so its key's list is malformed there and its read
/// isn't `complete` for it. Each order, on a store of its own: neither reader's answer may be
/// the other's. The read asks `:absent`, a predicate the store doesn't hold: the QL rewriting
/// can't read the list at `:n1` (not a blank node), and its hazard marks a query on any held
/// term first; one on no held term is told by the list record alone. It asks for the IRI
/// `:Y`, not a string: a restricted reader's string comparison is never `complete` (a
/// property may hold other values in graphs it can't read), and the row tests the record.
#[test]
fn two_readers_of_one_revision_are_each_told_their_own_views_lists() {
    let ask = format!("ASK {{ <{EX}x> <{EX}absent> <{EX}Y> }}");
    for order in [["aliases", "default"], ["default", "aliases"]] {
        let pipeline = aliased_cell();
        for who in order {
            let graphs = match who {
                "aliases" => vec![format!("{EX}secret")],
                _ => Vec::new(),
            };
            let status = (pipeline.store())
                .execute_query(&nrese_store::SparqlQueryRequest::new(
                    ask.clone(),
                    by_support(graphs),
                ))
                .expect("answered")
                .completeness;
            println!("{order:?}, {who}: {status:?}");
            let marked = status.as_ref().is_some_and(list_left_out);
            assert_eq!(marked, who == "default", "{order:?}, {who}: {status:?}");
        }
    }
}

/// A transaction deletes the `rdf:rest` of a key's one list cell and reads inside it. The
/// committed revision leaves no list out; the pending data does (the list is malformed), and
/// only the pending read's own scan finds it: a deleted list fact is one the read must scan for.
/// The store holds no `:name`, so the QL rewriting marks nothing on the query: the record decides.
#[test]
fn a_pending_deleted_list_fact_leaves_the_pending_read_not_complete() {
    let pipeline = ql_pipeline(in_memory_store_config());
    let rdf = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    (pipeline.apply(
        insert(&format!(
            "<{EX}K> <http://www.w3.org/2002/07/owl#hasKey> <{EX}l1> . \
             <{EX}l1> <{rdf}first> <{EX}k1> . <{EX}l1> <{rdf}rest> <{rdf}nil> . \
             <{EX}k1> a <http://www.w3.org/2002/07/owl#ObjectProperty> ."
        )),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    ))
    .expect("the data");
    let delete = format!("DELETE DATA {{ <{EX}l1> <{rdf}rest> <{rdf}nil> }}");
    let status = pending_status(
        pipeline.store(),
        nrese_store::SparqlQueryRequest::all(format!("ASK {{ <{EX}x> <{EX}name> \"Y\" }}")),
        &delete,
    );
    assert!(status.as_ref().is_some_and(list_left_out), "{status:?}");
}

/// A reader of the default graph only reads inside a transaction whose pending operation
/// touches no list. Its view of the pending data drops equality's copies derived through
/// `:secret`, as its view of the revision does, so its key's list is malformed there: the
/// read isn't `complete` for it, though the published revision leaves no list out. It asks
/// `:absent`, which the store doesn't hold, for the IRI `:Y`, not a string, as in
/// [`two_readers_of_one_revision_are_each_told_their_own_views_lists`] and for its reason.
#[test]
fn a_restricted_pending_read_scans_its_own_view() {
    let pipeline = aliased_cell();
    let request = nrese_store::SparqlQueryRequest::new(
        format!("ASK {{ <{EX}x> <{EX}absent> <{EX}Y> }}"),
        by_support(Vec::new()),
    );
    let add = format!("INSERT DATA {{ <{EX}w> <{EX}name> \"W\" }}");
    let status = pending_status(pipeline.store(), request, &add);
    assert!(status.as_ref().is_some_and(list_left_out), "{status:?}");
}

/// Under compact equality, a transaction adds (RDF4J `ADD`, which deletes nothing) one
/// `owl:sameAs` for each of a committed key's seven properties, and `:z`, which agrees with
/// `:x` on the key. No list fact is added, but the pending data's equality gives the key's
/// list 2^7 member sequences, more than the rules build. `:x` and `:z` are the same by the
/// key, so `:x :name :Z` is entailed: the pending read answers it, or isn't `complete`.
#[test]
fn a_pending_equality_change_that_leaves_out_a_list_is_not_complete() {
    let pipeline = ql_pipeline(nrese_store::StoreConfig {
        equality_by_representatives: true,
        equality_compact: true,
        ..in_memory_store_config()
    });
    // N-Triples, for the ADD: `rdf:type` written out (it has no `a`).
    let (owl, a) = (
        "http://www.w3.org/2002/07/owl#",
        "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>",
    );
    let property = format!("{a} <{owl}ObjectProperty>");
    let keys: Vec<String> = (1..=7).map(|i| format!("<{EX}k{i}>")).collect();
    let mut data = format!("<{EX}K> <{owl}hasKey> ( {} ) .", keys.join(" "));
    data.push_str(&format!(
        " <{EX}x> a <{EX}K> . <{EX}seed> <{EX}name> <{EX}Z> ."
    ));
    let mut add = format!("<{EX}z> {a} <{EX}K> .\n<{EX}z> <{EX}name> <{EX}Z> .\n");
    for i in 1..=7 {
        data.push_str(&format!(
            " <{EX}k{i}> {property} . <{EX}x> <{EX}k{i}> <{EX}v{i}> ."
        ));
        add.push_str(&format!(
            "<{EX}k{i}> <{owl}sameAs> <{EX}j{i}> .\n<{EX}j{i}> {property} .\n\
             <{EX}z> <{EX}k{i}> <{EX}v{i}> .\n"
        ));
    }
    (pipeline.apply(
        insert(&data),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    ))
    .expect("the data");
    let store = pipeline.store();
    let ask = format!("ASK {{ <{EX}x> <{EX}name> <{EX}Z> }}");
    let prepared =
        (store.prepare_query(&nrese_store::SparqlQueryRequest::all(ask))).expect("prepared");
    let ops = vec![nrese_store::StatementOp::Add {
        data: nrese_store::RdfPayload {
            payload: add.into_bytes().into(),
            format: nrese_store::GraphResultFormat::NTriples,
            base_iri: None,
        },
        contexts: Vec::new(),
    }];
    let (mut answer, mut status) = (Vec::new(), None);
    store
        .run_query_pending_reporting(
            &nrese_store::StatementsRequest::new(ops),
            &prepared,
            &nrese_store::CancellationToken::new(),
            &mut answer,
            |s| status = s,
        )
        .expect("the pending read");
    let answer = String::from_utf8(answer).expect("utf8");
    println!("pending equality read: {answer} with {status:?}");
    assert!(
        answer.contains("true") || status.as_ref().is_some_and(|s| !s.complete),
        "answered without :x :name :Z and complete: {answer} with {status:?}"
    );
    if !answer.contains("true") {
        assert!(status.as_ref().is_some_and(list_left_out), "{status:?}");
    }
}

/// Under compact equality, `:hk` is the same as `owl:hasKey` and its class's representative
/// (interned first), and a transaction adds (RDF4J `ADD`) only `:K :hk :head`: no list fact
/// as written. The read of `owl:hasKey` reads `:hk` and expands it, so the list scan sees a
/// key of `:head`'s 128 properties, more than the rules build. `:x` and `:z` agree on all of
/// them, so `:x :name :Z` is entailed: the pending read answers it, or isn't `complete`. A
/// restricted reader of the same data, whose pending read always scans its own view, shows
/// what the scan sees. The store is filled with reasoning off and then closed under
/// `owl2-rl`, as a store is whose user IRIs were interned before the rules' first compile
/// (one filled with reasoning off, or preloaded): written through the `owl2-rl` pipeline
/// first, `owl:hasKey` would be the older id and `:hk` the alias, and no read would reach the
/// raw pending fact.
#[test]
fn a_pending_fact_through_an_alias_of_has_key_leaves_out_a_list_under_compact_equality() {
    let (answer, status, scanned) = alias_of_has_key(true);
    assert!(
        scanned.as_ref().is_some_and(list_left_out),
        "the scan doesn't see the key: {scanned:?}"
    );
    assert!(
        answer.contains("true") || status.as_ref().is_some_and(|s| !s.complete),
        "answered without :x :name :Z and complete: {answer} with {status:?}"
    );
    if !answer.contains("true") {
        assert!(status.as_ref().is_some_and(list_left_out), "{status:?}");
    }
}

/// [`a_pending_fact_through_an_alias_of_has_key_leaves_out_a_list_under_compact_equality`]'s
/// data with equality kept as every copy: there reads take the statements as written (no
/// expansion), and pending statements are not reasoned over, so the scan and the pending skip
/// see the same raw facts. `:K :hk :head` is no key there: the pending read and the
/// restricted reader's scan both leave no list out. The store is filled in the same order, so
/// that `:hk` is again its class's representative.
#[test]
fn a_pending_fact_through_an_alias_of_has_key_is_no_key_when_equality_keeps_every_copy() {
    let (answer, status, scanned) = alias_of_has_key(false);
    assert!(
        scanned.as_ref().is_some_and(|s| !list_left_out(s)),
        "the scan sees a list left out: {scanned:?}"
    );
    assert!(
        status.as_ref().is_some_and(|s| !list_left_out(s)),
        "{answer} with {status:?}"
    );
}

/// The committed data of [`a_pending_fact_through_an_alias_of_has_key_leaves_out_a_list_under_compact_equality`]
/// (`compact` or not), and, as `:K :hk :head` pending would leave it, the answer and status of
/// `ASK { :x :name :Z }` and the status of the same read by a restricted reader.
fn alias_of_has_key(
    compact: bool,
) -> (
    String,
    Option<nrese_sparql::Completeness>,
    Option<nrese_sparql::Completeness>,
) {
    let store = Arc::new(
        StoreService::new(nrese_store::StoreConfig {
            ql_rewriting: nrese_store::QlRewritingMode::On,
            equality_by_representatives: true,
            equality_compact: compact,
            ..in_memory_store_config()
        })
        .expect("store"),
    );
    let (rdf, owl) = (
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "http://www.w3.org/2002/07/owl#",
    );
    // `:hk` first, so that its id is the smaller one of its class: written with reasoning
    // off, which compiles no rule, so no constant of the rules is interned before it.
    let mut data = format!("<{EX}hk> <{owl}sameAs> <{owl}hasKey> .");
    let cell = |i: usize| match i {
        1 => format!("<{EX}head>"),
        129 => format!("<{rdf}nil>"),
        i => format!("<{EX}c{i}>"),
    };
    for i in 1..=128 {
        data.push_str(&format!(
            " {} <{rdf}first> <{EX}p{i}> . {} <{rdf}rest> {} . <{EX}p{i}> a <{owl}ObjectProperty> . \
             <{EX}x> <{EX}p{i}> <{EX}v{i}> . <{EX}z> <{EX}p{i}> <{EX}v{i}> .",
            cell(i),
            cell(i),
            cell(i + 1)
        ));
    }
    data.push_str(&format!(
        " <{EX}x> a <{EX}K> . <{EX}z> a <{EX}K> . <{EX}z> <{EX}name> <{EX}Z> ."
    ));
    let with = |mode| {
        MutationPipeline::new(
            Arc::clone(&store),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
        )
    };
    (with(ReasoningMode::Disabled).apply(
        insert(&data),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    ))
    .expect("the data");
    (store.rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Rl)).expect("the closure");
    let _rules = with(ReasoningMode::Owl2Rl);
    let ask = format!("ASK {{ <{EX}x> <{EX}name> <{EX}Z> }}");
    let add = || {
        nrese_store::StatementsRequest::new(vec![nrese_store::StatementOp::Add {
            data: nrese_store::RdfPayload {
                payload: format!("<{EX}K> <{EX}hk> <{EX}head> .\n")
                    .into_bytes()
                    .into(),
                format: nrese_store::GraphResultFormat::NTriples,
                base_iri: None,
            },
            contexts: Vec::new(),
        }])
    };
    let read = |request: nrese_store::SparqlQueryRequest| {
        let prepared = store.prepare_query(&request).expect("prepared");
        let (mut answer, mut status) = (Vec::new(), None);
        store
            .run_query_pending_reporting(
                &add(),
                &prepared,
                &nrese_store::CancellationToken::new(),
                &mut answer,
                |s| status = s,
            )
            .expect("the pending read");
        (String::from_utf8(answer).expect("utf8"), status)
    };
    let (answer, status) = read(nrese_store::SparqlQueryRequest::all(ask.clone()));
    let (_, scanned) = read(nrese_store::SparqlQueryRequest::new(
        ask,
        by_support(Vec::new()),
    ));
    println!(
        "alias of hasKey (compact {compact}): {answer} with {status:?}; restricted {scanned:?}"
    );
    (answer, status, scanned)
}

/// A key of one property whose list cell `:n1` holds nothing itself: it is the same
/// (`owl:sameAs`, in `:secret`) as `:n2`, which holds the member `:k1` and ends the list.
/// `:x` and `:y` agree on `:k1`; `:y :name "Y"`.
fn aliased_cell() -> MutationPipeline {
    let pipeline = ql_pipeline(in_memory_store_config());
    let (rdf, owl) = (
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "http://www.w3.org/2002/07/owl#",
    );
    let data = format!(
        "<{EX}K> <{owl}hasKey> <{EX}n1> . GRAPH <{EX}secret> {{ <{EX}n1> <{owl}sameAs> <{EX}n2> }} \
         <{EX}n2> <{rdf}first> <{EX}k1> . <{EX}n2> <{rdf}rest> <{rdf}nil> . \
         <{EX}k1> a <{owl}ObjectProperty> . <{EX}x> a <{EX}K> . <{EX}y> a <{EX}K> . \
         <{EX}x> <{EX}k1> <{EX}v> . <{EX}y> <{EX}k1> <{EX}v> . <{EX}y> <{EX}name> \"Y\" ."
    );
    (pipeline.apply(
        insert(&data),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    ))
    .expect("the data");
    pipeline
}

/// A reader of the default graph and `graphs`, inferences by support.
fn by_support(graphs: Vec<String>) -> nrese_store::ReadScope {
    nrese_store::ReadScope::Graphs(Arc::new(nrese_sparql::GraphAccess {
        graphs,
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..nrese_sparql::GraphAccess::default()
    }))
}

/// The status of `request` read inside a transaction with the one update `update` pending,
/// printed.
fn pending_status(
    store: &StoreService,
    request: nrese_store::SparqlQueryRequest,
    update: &str,
) -> Option<nrese_sparql::Completeness> {
    let prepared = store.prepare_query(&request).expect("prepared");
    let ops = vec![nrese_store::StatementOp::Update(SparqlUpdateRequest::new(
        update.to_owned(),
    ))];
    let mut status = None;
    store
        .run_query_pending_reporting(
            &nrese_store::StatementsRequest::new(ops),
            &prepared,
            &nrese_store::CancellationToken::new(),
            &mut Vec::<u8>::new(),
            |s| status = s,
        )
        .expect("the pending read");
    println!("pending read with {update}: {status:?}");
    status
}

/// Not `complete`, for a list axiom left out.
fn list_left_out(status: &nrese_sparql::Completeness) -> bool {
    !status.complete && (status.reasons.iter()).any(|r| r.text.contains("list axiom"))
}

/// [`a_restricted_read_leaves_an_aliased_keys_mark`]'s store: the aliased key of 7 properties,
/// its `owl:sameAs` facts in `:secret`, then the later commit that leaves it out.
fn aliased_key_with_hidden_aliases() -> MutationPipeline {
    let pipeline = ql_pipeline(in_memory_store_config());
    let write = |data: &str| {
        let ticket = MutationTicket::new();
        (pipeline.apply(insert(data), &nrese_store::Requester::all(), &ticket)).expect("the data");
    };
    let mut first = long_key(7);
    for i in 1..=7 {
        first.push_str(&format!(
            " GRAPH <{EX}secret> {{ <{EX}k{i}> <http://www.w3.org/2002/07/owl#sameAs> <{EX}j{i}> }} \
             <{EX}j{i}> a <http://www.w3.org/2002/07/owl#ObjectProperty> ."
        ));
    }
    write(&first);
    let mut later = format!(
        "<{EX}C> <http://www.w3.org/2002/07/owl#intersectionOf> ( <{EX}A1> <{EX}A2> ) . \
         <{EX}z> a <{EX}K> . <{EX}z> <{EX}name> \"Z\" ."
    );
    for i in 1..=7 {
        later.push_str(&format!(" <{EX}z> <{EX}k{i}> <{EX}v{i}> ."));
    }
    write(&later);
    pipeline
}

/// `ASK { ask }` by a reader of the default graph only, inferences by support (a view of its
/// own over any store with a named graph), printed.
fn restricted_read(store: &StoreService, ask: &str) {
    let reader = nrese_store::ReadScope::Graphs(Arc::new(nrese_sparql::GraphAccess {
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..nrese_sparql::GraphAccess::default()
    }));
    let answer = (store.execute_query(&nrese_store::SparqlQueryRequest::new(
        format!("ASK {{ {ask} }}"),
        reader,
    )))
    .expect("the restricted read answered");
    let payload = String::from_utf8(answer.payload).expect("utf8");
    println!(
        "restricted read: {ask}: {payload} with {:?}",
        answer.completeness
    );
}

/// `owl2-rl` with the QL rewriting on, over a store of `config`.
fn ql_pipeline(config: nrese_store::StoreConfig) -> MutationPipeline {
    let config = nrese_store::StoreConfig {
        ql_rewriting: nrese_store::QlRewritingMode::On,
        ..config
    };
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

/// `data` written, then `ASK { ask }` either true or not `complete`.
fn left_out_status(pipeline: &MutationPipeline, data: &str, ask: &str) {
    pipeline
        .apply(
            insert(data),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("the data");
    left_out_answer(pipeline.store(), ask);
}

fn left_out_answer(store: &StoreService, ask: &str) {
    let answer = store
        .execute_query(&nrese_store::SparqlQueryRequest::all(format!(
            "ASK {{ {ask} }}"
        )))
        .expect("answered");
    let payload = String::from_utf8(answer.payload).expect("utf8");
    let status = answer.completeness.as_ref();
    println!("plain read: {ask}: {payload} with {status:?}");
    assert!(
        payload.contains("true") || status.is_some_and(|c| !c.complete),
        "{ask}: {payload} with {status:?}"
    );
}

/// [`long_key`] with each key property `:ki` the same as a `:ji` (`owl:sameAs`): every cell
/// of the key's list then has two first members, and the list 2^n member sequences. Each
/// `:ji` is declared an object property too.
fn aliased_key(n: usize) -> String {
    let mut data = long_key(n);
    for i in 1..=n {
        data.push_str(&format!(
            " <{EX}k{i}> <http://www.w3.org/2002/07/owl#sameAs> <{EX}j{i}> . \
             <{EX}j{i}> a <http://www.w3.org/2002/07/owl#ObjectProperty> ."
        ));
    }
    data
}

/// `:chain owl:propertyChainAxiom ( :p1 … :pn )` and a path `:a0 :p1 :a1 … :pn :an`:
/// prp-spo2 makes `:a0 :chain :an`.
fn long_chain(n: usize) -> String {
    let links: Vec<String> = (1..=n).map(|i| format!("<{EX}p{i}>")).collect();
    let mut data = format!(
        "<{EX}chain> <http://www.w3.org/2002/07/owl#propertyChainAxiom> ( {} ) .",
        links.join(" ")
    );
    for i in 1..=n {
        data.push_str(&format!(" <{EX}a{}> <{EX}p{i}> <{EX}a{i}> .", i - 1));
    }
    data
}

/// `:C owl:intersectionOf ( :A1 … :An )` and an `:x` in every `:Ai`: cls-int1 (OWL 2
/// Profiles §4.3, table 6) makes `:x` a `:C`. Its rule has one atom per member, and a
/// rule's atoms are numbered by `u8`.
fn long_intersection(n: usize) -> String {
    let members: Vec<String> = (1..=n).map(|i| format!("<{EX}A{i}>")).collect();
    let mut data = format!(
        "<{EX}C> <http://www.w3.org/2002/07/owl#intersectionOf> ( {} ) .",
        members.join(" ")
    );
    for member in &members {
        data.push_str(&format!(" <{EX}x> a {member} ."));
    }
    data
}

/// An intersection of 256 members is one rule; one of 257 is either one too or reported
/// and not built, never a panic and never a failed commit, under `owl2-rl` and `owl2-dl`.
#[test]
fn a_long_intersection_is_built_or_reported() {
    let mut wrong = Vec::new();
    for mode in ["owl2-rl", "owl2-dl"] {
        for n in [256, 257] {
            let run = std::panic::catch_unwind(|| {
                let owl = match mode {
                    "owl2-rl" => pipeline(ReasoningMode::Owl2Rl),
                    _ => crate::dl::pipeline(),
                };
                owl.apply(
                    insert(&long_intersection(n)),
                    &nrese_store::Requester::all(),
                    &MutationTicket::new(),
                )
                .map_err(|error| error.to_string())?;
                let entailed = contains(&owl, &format!("<{EX}x> a <{EX}C>"));
                let reported = owl.last_reasoning_run().is_some_and(|run| {
                    run.diagnostics
                        .iter()
                        .any(|d| d.rules.starts_with("cls-int"))
                });
                Ok::<_, String>((entailed, reported))
            });
            match run {
                Ok(Ok((true, _))) => {}
                Ok(Ok((false, reported))) if n == 257 && reported => {}
                Ok(outcome) => wrong.push(format!("{mode}, {n} members: {outcome:?}")),
                Err(_) => wrong.push(format!("{mode}, {n} members: panicked")),
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
