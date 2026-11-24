use std::fmt::Write as _;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_sparql::Completeness;
use nrese_store::dl::entailment::Entailed;
use nrese_store::{MutationPipeline, SparqlQueryRequest, StoreService};

use super::entailment::{turtle, unchecked_inconsistent};
use super::{PREFIXES, insert, pipeline, queries::query};

const CHILD_DEPTH: &str = "NRESE_TEST_COMPLEMENT_DEPTH";
const MEMBERSHIP: &str = ":x a :A";
const DECLARED_INDIVIDUAL: &str = ":x a owl:NamedIndividual";

fn complement_chain(depth: usize) -> String {
    let mut data = format!(":A a owl:Class . {DECLARED_INDIVIDUAL}, _:b0 .");
    for i in 1..depth {
        write!(data, " _:b{} owl:complementOf _:b{i} .", i - 1).unwrap();
    }
    write!(data, " _:b{} owl:complementOf :A .", depth - 1).unwrap();
    data
}

fn at_each_depth(test: &str, case: impl Fn(usize)) {
    if let Ok(depth) = std::env::var(CHILD_DEPTH) {
        case(depth.parse().expect("test depth"));
        println!("complement case completed");
        return;
    }

    let (_, module) = module_path!().split_once("::").expect("test module path");
    let name = format!("{module}::{test}");
    let binary = std::env::current_exe().expect("test binary");
    let mut failed = Vec::new();
    for depth in [1_000, 10_000, 100_000] {
        let start = Instant::now();
        let child = Command::new(&binary)
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(CHILD_DEPTH, depth.to_string())
            .env_remove("RUST_MIN_STACK")
            .output()
            .expect("run the isolated case");
        let stdout = String::from_utf8_lossy(&child.stdout);
        let stderr = String::from_utf8_lossy(&child.stderr);
        eprintln!(
            "{name} depth {depth}: {} in {:.3}s",
            child.status,
            start.elapsed().as_secs_f64()
        );
        if !child.status.success() || !stdout.contains("complement case completed") {
            eprintln!("{name} depth {depth}:\n{stdout}\n{stderr}");
            failed.push(depth);
        }
    }
    assert!(failed.is_empty(), "failed at complement depths {failed:?}");
}

fn profile_pipeline(mode: ReasoningMode) -> MutationPipeline {
    let store = StoreService::new(crate::support::in_memory_store_config()).expect("store");
    let reasoner = ReasonerService::new(ReasonerConfig::for_mode(mode));
    MutationPipeline::new(Arc::new(store), Arc::new(reasoner))
}

fn ask(store: &StoreService, pattern: &str) -> (bool, Option<Completeness>) {
    let request = SparqlQueryRequest::all(format!("{PREFIXES}ASK {{ {pattern} }}"));
    let result = store.execute_query(&request).expect("query");
    let json: serde_json::Value = serde_json::from_slice(&result.payload).expect("JSON result");
    (
        json["boolean"].as_bool().expect("ASK answer"),
        result.completeness,
    )
}

/// OWL 2 Direct Semantics §§2.2.3 and 2.3.6: an even number of complements of `:A`
/// denotes `:A`. An individual asserted to belong to that expression belongs to `:A`.
#[test]
fn dl_write() {
    at_each_depth("dl_write", |depth| {
        let dl = pipeline();
        insert(&dl, &complement_chain(depth)).expect("write applies");
        assert_eq!(query(&dl, "ASK { :x a :A }").0, ["true"]);
    });
}

/// OWL 2 Profiles §3.2.5 permits class assertions of named classes. Outside that
/// profile, the QL store applies the write and reports the affected answer as incomplete
/// (the QL rewriting design, "Completeness"). Nesting preserves the answer and status.
#[test]
fn ql_write() {
    at_each_depth("ql_write", |depth| {
        let write_and_read = |depth| {
            let ql = profile_pipeline(ReasoningMode::Owl2Ql);
            insert(&ql, &complement_chain(depth)).expect("write applies");
            assert!(ask(ql.store(), DECLARED_INDIVIDUAL).0);
            ask(ql.store(), MEMBERSHIP)
        };
        let deep = write_and_read(depth);
        assert!(!deep.1.as_ref().expect("QL completeness").complete);
        assert_eq!(deep, write_and_read(2));
    });
}

/// OWL 2 Direct Semantics §§2.2.3 and 2.3.6: bulk-loaded assertions retain the same
/// meaning when the store rebuilds its reasoning. An even complement chain denotes `:A`.
#[test]
fn dl_bulk_load() {
    at_each_depth("dl_bulk_load", |depth| {
        let dl = pipeline();
        crate::support::load_past_the_pipeline(
            dl.store(),
            &format!("{PREFIXES}{}", complement_chain(depth)),
        );
        dl.store()
            .rematerialise(
                dl.reasoner()
                    .config()
                    .materialised_program()
                    .expect("rules"),
            )
            .expect("rematerialisation");
        assert_eq!(query(&dl, "ASK { :x a :A }").0, ["true"]);
    });
}

/// OWL 2 Direct Semantics §§2.2.3 and 2.5: every model of `:x a :A` satisfies the
/// assertion with `:A` replaced by an even number of complements of it.
#[test]
fn dl_entailment_consistent() {
    at_each_depth("dl_entailment_consistent", |depth| {
        let dl = pipeline();
        insert(&dl, ":A a owl:Class . :x a owl:NamedIndividual , :A .").expect("premise");
        let answer = dl
            .store()
            .entails_dl(&turtle(&complement_chain(depth)))
            .expect("entailment");
        assert_eq!(answer.answer, Entailed::Yes);
        assert_eq!(answer.premise.as_str(), "consistent");
    });
}

/// OWL 2 Direct Semantics §2.5: an inconsistent premise has no model and entails
/// every admitted conclusion. The service reads the whole conclusion before answering.
#[test]
fn dl_entailment_inconsistent() {
    at_each_depth("dl_entailment_inconsistent", |depth| {
        let dl = unchecked_inconsistent();
        let answer = dl
            .store()
            .entails_dl(&turtle(&complement_chain(depth)))
            .expect("entailment");
        assert_eq!(answer.answer, Entailed::Yes);
        assert_eq!(answer.premise.as_str(), "inconsistent");
    });
}

/// OWL 2 Mapping to RDF §2 maps each complement to a triple. The RL write control
/// accepts those triples at each depth and retains the asserted individual declaration.
#[test]
fn rl_write_control() {
    at_each_depth("rl_write_control", |depth| {
        let rl = profile_pipeline(ReasoningMode::Owl2Rl);
        insert(&rl, &complement_chain(depth)).expect("write applies");
        assert!(ask(rl.store(), DECLARED_INDIVIDUAL).0);
    });
}
