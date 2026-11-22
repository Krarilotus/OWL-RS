//! The oracle cells check the chain's rows across equality modes. This public SPARQL
//! check also verifies the documented `sound-only` status on the ASK response.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, Requester, SolutionsResultFormat,
    SparqlQueryRequest, SparqlUpdateRequest, StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX ex: <http://example.org/>
PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX owl: <http://www.w3.org/2002/07/owl#>";
const CHAIN: &str = "ex:out owl:propertyChainAxiom ex:h .
ex:h rdf:first rdfs:subClassOf ; rdf:rest ex:t .
ex:t rdf:first ex:q ; rdf:rest rdf:nil .
ex:b ex:q ex:z .";
const EDGE: &str = "ex:a rdfs:subClassOf ex:b .";

fn pipeline(store: &Arc<StoreService>, mode: ReasoningMode) -> MutationPipeline {
    MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn update(pipeline: &MutationPipeline, verb: &str, triples: &str) {
    let request = SparqlUpdateRequest::new(format!("{PREFIXES}\n{verb} DATA {{ {triples} }}"));
    pipeline
        .apply(
            MutationCommand::Update(request),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .expect("commit SPARQL update");
}

fn load(pipeline: &MutationPipeline, with_edge: bool) {
    let edge = if with_edge { EDGE } else { "" };
    update(pipeline, "INSERT", &format!("{CHAIN}\n{edge}"));
}

fn ask(store: &StoreService, case: &str, stage: &str) -> (bool, &'static str) {
    let mut request = SparqlQueryRequest::all(format!("{PREFIXES}\nASK {{ ex:a ex:out ex:z }}"));
    request.solutions_format = SolutionsResultFormat::Json;
    let result = store.execute_query(&request).expect("read SPARQL ASK");
    let answer: serde_json::Value = serde_json::from_slice(&result.payload).expect("SPARQL JSON");
    let answer = answer["boolean"].as_bool().expect("ASK boolean");
    let status = result
        .completeness
        .as_ref()
        .map_or("unavailable", |status| status.as_str());
    println!("case={case} stage={stage} ask={answer} status={status}");
    (answer, status)
}

// Spec: OWL 2 RL/RDF Table 5 prp-spo2 uses rdfs:subClassOf as the first
// chain operand. This fixture entails ex:a ex:out ex:z exactly when the edge exists.
fn check(case: &str, verb: &str, has_edge_after_commit: bool) {
    let rl = ReasoningMode::Owl2Rl;
    let dir = tempfile::tempdir().expect("store directory");
    let open =
        || Arc::new(StoreService::new(StoreConfig::on_disk(dir.path())).expect("open store"));

    let (warm, warm_status) = {
        let store = open();
        let writer = pipeline(&store, rl);
        load(&writer, !has_edge_after_commit);
        update(&writer, verb, EDGE);
        ask(&store, case, "warm")
    };
    let (cold, cold_status) = {
        let store = open();
        let _reader = pipeline(&store, rl);
        ask(&store, case, "cold")
    };
    let rebuilt = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("open fresh store"));
    load(
        &pipeline(&rebuilt, ReasoningMode::Disabled),
        has_edge_after_commit,
    );
    let _reader = pipeline(&rebuilt, rl);
    let rules = ReasonerConfig::for_mode(rl)
        .materialised_program()
        .expect("OWL 2 RL program");
    rebuilt
        .rematerialise(rules)
        .expect("rebuild final statements");
    let (expected, rebuild_status) = ask(&rebuilt, case, "rebuild");

    assert_eq!(
        expected, has_edge_after_commit,
        "rebuild disagrees with prp-spo2"
    );
    assert_eq!(
        [warm, cold],
        [expected, expected],
        "[warm, cold] disagree with rebuild"
    );
    // reasoning-semantics.md promises sound-only for standard owl2-rl answers.
    for (stage, status) in [
        ("warm", warm_status),
        ("cold", cold_status),
        ("rebuild", rebuild_status),
    ] {
        assert_eq!(status, "sound-only", "{case} {stage} ASK status");
    }
}

#[test]
fn delete_subclass_chain_operand() {
    check("delete", "DELETE", false);
}

#[test]
fn insert_subclass_chain_operand() {
    check("insert", "INSERT", true);
}
