use std::fmt::Write as _;
use std::process::Command;
use std::sync::Arc;
use std::time::Instant;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_sparql::{Completeness, GraphAccess};
use nrese_store::dl::entailment::Entailed;
use nrese_store::{
    MutationCommand, MutationError, MutationPipeline, MutationTicket, ReadScope, Requester,
    SparqlQueryRequest, SparqlUpdateRequest, StoreService, WriteScope,
};

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

/// `_:b0 = :A ⊓ (:A ⊓ (… ⊓ (:A ⊓ :B)))`, `depth` intersections.
fn intersection_chain(depth: usize) -> String {
    let mut data = String::from(":A a owl:Class . :B a owl:Class .");
    for i in 0..depth {
        match i + 1 == depth {
            true => write!(data, " _:b{i} owl:intersectionOf (:A :B) .").unwrap(),
            false => write!(data, " _:b{i} owl:intersectionOf (:A _:b{}) .", i + 1).unwrap(),
        }
    }
    data
}

/// `_:r`, a data existential on `:d` whose range is `depth` data complements of
/// `xsd:integer`.
fn data_complement_chain(depth: usize) -> String {
    let mut data = String::from(
        ":d a owl:DatatypeProperty . \
         _:r a owl:Restriction ; owl:onProperty :d ; owl:someValuesFrom _:c0 .",
    );
    for i in 1..depth {
        write!(
            data,
            " _:c{} a rdfs:Datatype ; owl:datatypeComplementOf _:c{i} .",
            i - 1
        )
        .unwrap();
    }
    write!(
        data,
        " _:c{} a rdfs:Datatype ; owl:datatypeComplementOf xsd:integer .",
        depth - 1
    )
    .unwrap();
    data
}

fn at_each_depth(test: &str, case: impl Fn(usize)) {
    at_depths(test, &[1_000, 10_000, 100_000], case);
}

/// Runs `case` at each of `depths` in a child process of its own, so that a stack
/// overflow is reported per depth.
fn at_depths(test: &str, depths: &[usize], case: impl Fn(usize)) {
    if let Ok(depth) = std::env::var(CHILD_DEPTH) {
        case(depth.parse().expect("test depth"));
        println!("complement case completed");
        return;
    }

    let (_, module) = module_path!().split_once("::").expect("test module path");
    let name = format!("{module}::{test}");
    let binary = std::env::current_exe().expect("test binary");
    let mut failed = Vec::new();
    for &depth in depths {
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

/// The membership answer under `owl2-dl` with what its status says and what decided it,
/// after a later read of an asserted statement shows the store still answers.
fn dl_membership(dl: &MutationPipeline) -> (Vec<String>, Completeness, Vec<&'static str>) {
    let asserted = query(dl, &format!("ASK {{ {DECLARED_INDIVIDUAL} }}"));
    assert_eq!(asserted.0, ["true"]);
    let (answer, status) = query(dl, &format!("ASK {{ {MEMBERSHIP} }}"));
    (answer, status.shared, status.paths)
}

/// OWL 2 Mapping to RDF §2 maps each complement to a triple, at any nesting depth. A deep
/// chain's write applies, the store still answers, and the membership `:x a :A` is answered
/// with the answer, status and path of a chain of two: nesting changes neither. The
/// answer's value under the Direct Semantics is checked elsewhere.
#[test]
fn dl_write() {
    at_each_depth("dl_write", |depth| {
        let write_and_read = |depth| {
            let dl = pipeline();
            insert(&dl, &complement_chain(depth)).expect("write applies");
            dl_membership(&dl)
        };
        assert_eq!(write_and_read(depth), write_and_read(2));
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

/// As [`dl_write`], for a bulk load past the pipeline and the store's rebuild of its
/// reasoning: the deep chain loads, the rebuild succeeds, and the membership is answered as
/// for a chain of two. The answer's value is checked elsewhere.
#[test]
fn dl_bulk_load() {
    at_each_depth("dl_bulk_load", |depth| {
        let load_and_read = |depth| {
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
            dl_membership(&dl)
        };
        assert_eq!(load_and_read(depth), load_and_read(2));
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

/// OWL 2 Mapping to RDF §2 maps each data complement to a triple, at any depth. Under
/// `owl2-dl`, a write asserting a data existential over a deep chain of them is accepted
/// or refused as one over a chain of two is, and the store still answers.
#[test]
fn data_range_dl_write() {
    at_each_depth("data_range_dl_write", |depth| {
        let write_and_read = |depth| {
            let dl = pipeline();
            let data = format!(
                "{} {DECLARED_INDIVIDUAL}, _:r .",
                data_complement_chain(depth)
            );
            let applied = insert(&dl, &data).is_ok();
            (
                applied,
                query(&dl, &format!("ASK {{ {DECLARED_INDIVIDUAL} }}")).0,
            )
        };
        assert_eq!(write_and_read(depth), write_and_read(2));
    });
}

/// As [`data_range_dl_write`] under `owl2-ql`: the write applies, and the asserted
/// declaration is answered with the answer and status of a chain of two.
#[test]
fn data_range_ql_write() {
    at_each_depth("data_range_ql_write", |depth| {
        let write_and_read = |depth| {
            let ql = profile_pipeline(ReasoningMode::Owl2Ql);
            let data = format!(
                "{} {DECLARED_INDIVIDUAL}, _:r .",
                data_complement_chain(depth)
            );
            insert(&ql, &data).expect("write applies");
            ask(ql.store(), DECLARED_INDIVIDUAL)
        };
        assert_eq!(write_and_read(depth), write_and_read(2));
    });
}

/// OWL 2 Direct Semantics §2.5: whether a premise entails a membership of a data
/// existential over a deep chain of data complements is answered as for a chain of two.
#[test]
fn data_range_entailment() {
    at_each_depth("data_range_entailment", |depth| {
        let dl = pipeline();
        insert(
            &dl,
            ":d a owl:DatatypeProperty . :x a owl:NamedIndividual .",
        )
        .expect("premise");
        let entails = |depth| {
            let conclusion = format!("{} :x a _:r .", data_complement_chain(depth));
            let answer = dl
                .store()
                .entails_dl(&turtle(&conclusion))
                .expect("entailment");
            (answer.answer, answer.premise.as_str())
        };
        assert_eq!(entails(depth), entails(2));
    });
}

/// As [`ql_write`], for a complement chain ending in a nominal: the write applies and the
/// membership is answered with the answer and status of a chain of two.
#[test]
fn nominal_complement_ql_write() {
    at_each_depth("nominal_complement_ql_write", |depth| {
        let write_and_read = |depth: usize| {
            let mut data = format!(
                ":A a owl:Class . :a a owl:NamedIndividual . {DECLARED_INDIVIDUAL}, _:b0 ."
            );
            for i in 1..depth {
                write!(data, " _:b{} owl:complementOf _:b{i} .", i - 1).unwrap();
            }
            write!(
                data,
                " _:b{} owl:complementOf _:nom . _:nom owl:oneOf (:a) .",
                depth - 1
            )
            .unwrap();
            let ql = profile_pipeline(ReasoningMode::Owl2Ql);
            insert(&ql, &data).expect("write applies");
            assert!(ask(ql.store(), DECLARED_INDIVIDUAL).0);
            ask(ql.store(), MEMBERSHIP)
        };
        assert_eq!(write_and_read(depth), write_and_read(2));
    });
}

/// As [`ql_write`], for an intersection chain asserted of `:x`.
#[test]
fn intersection_ql_write() {
    at_each_depth("intersection_ql_write", |depth| {
        let write_and_read = |depth| {
            let ql = profile_pipeline(ReasoningMode::Owl2Ql);
            let data = format!(
                "{} {DECLARED_INDIVIDUAL}, _:b0 .",
                intersection_chain(depth)
            );
            insert(&ql, &data).expect("write applies");
            assert!(ask(ql.store(), DECLARED_INDIVIDUAL).0);
            ask(ql.store(), MEMBERSHIP)
        };
        assert_eq!(write_and_read(depth), write_and_read(2));
    });
}

/// OWL 2 Profiles §2.2 and §4.2: a subclass of an intersection of named classes is in
/// EL, QL and RL. Under `owl2-dl`, a deep one applies and the store answers as for a chain
/// of two. Run at 1,000 only: deeper, the rules' grounding of the chain takes time and
/// memory growing faster than the chain itself, which is not this case's subject.
#[test]
fn intersection_subclass_dl_write() {
    at_depths("intersection_subclass_dl_write", &[1_000], |depth| {
        let write_and_read = |depth| {
            let dl = pipeline();
            let data = format!(
                "{} :C a owl:Class ; rdfs:subClassOf _:b0 . {DECLARED_INDIVIDUAL}, :C .",
                intersection_chain(depth)
            );
            insert(&dl, &data).expect("write applies");
            dl_membership(&dl)
        };
        assert_eq!(write_and_read(depth), write_and_read(2));
    });
}

/// OWL 2 Direct Semantics §2.5: an odd complement chain of `:A` beside `:x a :A` has no
/// model. Under `owl2-dl`, a committer that may read and write the default graph only is
/// told the refusal its own graph shows by itself, as for a chain of three, and the store
/// still answers.
#[test]
fn refused_dl_write() {
    let default_graph_only = || {
        let access = || {
            Arc::new(GraphAccess {
                default_graph: true,
                ..GraphAccess::default()
            })
        };
        Requester::new(ReadScope::Graphs(access()), WriteScope::Graphs(access()))
    };
    at_each_depth("refused_dl_write", |depth| {
        let write_and_read = |depth| {
            let dl = pipeline();
            let data = format!("{} :x a :A .", complement_chain(depth + 1));
            let write = format!("{PREFIXES}INSERT DATA {{ {data} }}");
            let result = dl.apply(
                MutationCommand::Update(SparqlUpdateRequest::new(write)),
                &default_graph_only(),
                &MutationTicket::new(),
            );
            let kind = match result {
                Err(MutationError::Rejected(reject)) => reject.kind,
                other => panic!("expected the refusal, got {other:?}"),
            };
            (kind, query(&dl, "ASK { :x a :A }").0)
        };
        assert_eq!(write_and_read(depth), write_and_read(2));
    });
}

/// OWL 2 Structural Specification §5.3: `owl:topObjectProperty` relates every pair of
/// individuals. Under `owl2-dl`, an existential on it over a deep complement chain is
/// written and answered as one over a chain of two.
#[test]
fn universal_property_dl_write() {
    at_each_depth("universal_property_dl_write", |depth| {
        let write_and_read = |depth| {
            let dl = pipeline();
            let data = format!(
                "{} :y a owl:NamedIndividual, _:u . \
                 _:u a owl:Restriction ; owl:onProperty owl:topObjectProperty ; \
                 owl:someValuesFrom _:b0 .",
                complement_chain(depth)
            );
            insert(&dl, &data).expect("write applies");
            dl_membership(&dl)
        };
        assert_eq!(write_and_read(depth), write_and_read(2));
    });
}

/// As [`ql_write`], for a subclass axiom whose left side is a deep chain of unions: the
/// write applies and a membership of its superclass is answered as for a chain of two.
#[test]
fn union_subclass_ql_write() {
    at_each_depth("union_subclass_ql_write", |depth| {
        let write_and_read = |depth: usize| {
            let mut data = format!(":A a owl:Class . :B a owl:Class . {DECLARED_INDIVIDUAL}, :A .");
            for i in 0..depth {
                match i + 1 == depth {
                    true => write!(data, " _:u{i} owl:unionOf (:A :C) .").unwrap(),
                    false => write!(data, " _:u{i} owl:unionOf (:A _:u{}) .", i + 1).unwrap(),
                }
            }
            data.push_str(" _:u0 rdfs:subClassOf :B .");
            let ql = profile_pipeline(ReasoningMode::Owl2Ql);
            insert(&ql, &data).expect("write applies");
            assert!(ask(ql.store(), DECLARED_INDIVIDUAL).0);
            ask(ql.store(), ":x a :B")
        };
        assert_eq!(write_and_read(depth), write_and_read(2));
    });
}
