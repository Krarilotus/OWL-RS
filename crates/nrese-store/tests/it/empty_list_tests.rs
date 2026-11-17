use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    ConsistencyStatus, InvalidStatements, MutationCommand, MutationCommitReport, MutationError,
    MutationPipeline, MutationTicket, ReadModel, Requester, SparqlQueryRequest,
    SparqlUpdateRequest, StoreConfig, StoreError, StoreService,
};
use serde_json::Value;

use crate::support::{in_memory_store_config, load_past_the_pipeline};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
    PREFIX owl: <http://www.w3.org/2002/07/owl#> \
    PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

const INTERSECTIONS: [(&str, &str, u64); 3] = [
    ("rdf:nil", ":C owl:intersectionOf rdf:nil . :x a :D .", 1),
    (
        "sameAs rdf:nil",
        ":C owl:intersectionOf :l . :l owl:sameAs rdf:nil . :x a :D .",
        2,
    ),
    (
        "sameAs rdf:nil with union",
        ":C owl:intersectionOf :l . :l owl:sameAs rdf:nil . \
         :U owl:unionOf :l . :x a :D . :y a :U .",
        3,
    ),
];

const LIST: &str = ":C owl:intersectionOf :l . \
    :l rdf:first :A ; rdf:rest :tail . :tail rdf:first :B ; rdf:rest rdf:nil .";
const INSTANCES: &str = ":x a :A, :B .";
const EMPTY_AFTER_EDIT: &str = ":C owl:intersectionOf rdf:nil . :x a :A, :B .";

fn check_cells(mut check: impl FnMut(&str, ReasoningMode, &mut Vec<String>)) {
    let mut failures = Vec::new();
    for equality in ["representatives", "replicate", "compact"] {
        for mode in [ReasoningMode::Owl2Rl, ReasoningMode::Owl2Dl] {
            check(equality, mode, &mut failures);
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

fn cell(failures: &mut Vec<String>, label: &str, check: impl FnOnce()) {
    if let Err(panic) = catch_unwind(AssertUnwindSafe(check)) {
        let message = panic
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| panic.downcast_ref::<&str>().copied())
            .unwrap_or("unknown panic");
        failures.push(format!("{label}: {message}"));
    }
}

fn config(equality: &str, storage: StoreConfig) -> StoreConfig {
    StoreConfig {
        equality_by_representatives: equality != "replicate",
        equality_compact: equality == "compact",
        invalid_statements: InvalidStatements::Strict,
        ..storage
    }
}

fn pipeline(store: StoreService, mode: ReasoningMode) -> MutationPipeline {
    MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn update(
    pipeline: &MutationPipeline,
    update: &str,
) -> Result<MutationCommitReport, MutationError> {
    pipeline.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{update}"))),
        &Requester::all(),
        &MutationTicket::new(),
    )
}

fn insert(pipeline: &MutationPipeline, data: &str) -> Result<MutationCommitReport, MutationError> {
    update(pipeline, &format!("INSERT DATA {{ {data} }}"))
}

fn rebuild(store: &StoreService, mode: ReasoningMode) -> Result<(), StoreError> {
    let program = ReasonerConfig::for_mode(mode)
        .materialised_program()
        .expect("materialised program");
    store.rematerialise(program).map(|_| ())
}

fn answer(store: &StoreService, query: &str, read_model: Option<ReadModel>) -> Value {
    let mut request = SparqlQueryRequest::all(format!("{PREFIXES}{query}"));
    request.read_model = read_model;
    let result = store.execute_query(&request).expect("query");
    serde_json::from_slice(&result.payload).expect("SPARQL JSON")
}

fn ask(store: &StoreService, pattern: &str, read_model: Option<ReadModel>) -> bool {
    answer(store, &format!("ASK {{ {pattern} }}"), read_model)["boolean"]
        .as_bool()
        .expect("ASK answer")
}

fn assert_list_input(store: &StoreService, data: &str, pattern: &str, count: u64) {
    let answer = answer(
        store,
        &format!("SELECT (COUNT(*) AS ?n) WHERE {{ {pattern} }}"),
        Some(ReadModel::Asserted),
    );
    assert_eq!(
        answer["results"]["bindings"][0]["n"]["value"]
            .as_str()
            .and_then(|value| value.parse::<u64>().ok()),
        Some(count),
        "asserted list statements: {answer}"
    );
    assert!(ask(store, data, Some(ReadModel::Asserted)), "{data}");
}

fn assert_intersection_input(store: &StoreService, data: &str, count: u64) {
    assert_list_input(
        store,
        data,
        "VALUES ?p { owl:intersectionOf owl:unionOf owl:sameAs } ?s ?p ?o",
        count,
    );
}

fn assert_intersection_answers(store: &StoreService, asserted_type: &str) {
    assert!(
        !ask(store, ":x a :C", None),
        "ASK over the empty intersection"
    );
    let result = answer(store, "SELECT ?c { :x a ?c }", None);
    let rows = result["results"]["bindings"].as_array().expect("types");
    assert!(
        rows.iter()
            .all(|row| row["c"]["value"] != "http://example.com/C"),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|row| row["c"]["value"] == asserted_type),
        "{rows:?}"
    );
}

/// Spec: OWL 2 Profiles §4.3 tables 6 and 9 give no grounded `cls-int1` conclusion
/// for a list with no members. Its partial rules leave the asserted type readable.
/// OWL 2 Structural Specification §8.1.1 requires at least two intersection operands.
/// A commit and the following ordinary commit leave the store usable.
#[test]
fn an_empty_intersection_can_be_committed() {
    check_cells(|equality, mode, failures| {
        for (name, data, count) in INTERSECTIONS {
            let label = format!("commit, {mode:?}, {equality}, {name}");
            cell(failures, &label, || {
                let store =
                    StoreService::new(config(equality, in_memory_store_config())).expect("store");
                let writer = pipeline(store, mode);
                let outcome = insert(&writer, data);
                assert_intersection_input(writer.store(), data, count);
                assert_intersection_answers(writer.store(), "http://example.com/D");
                assert!(
                    matches!(
                        insert(&writer, ":z a :D ."),
                        Ok(MutationCommitReport::Applied { .. })
                    ),
                    "following commit after {outcome:?}"
                );
                assert!(ask(writer.store(), ":z a :D", None));
            });
        }

        let label = format!("edit to rdf:nil, {mode:?}, {equality}");
        cell(failures, &label, || {
            let store =
                StoreService::new(config(equality, in_memory_store_config())).expect("store");
            let writer = pipeline(store, mode);
            let initial = format!("{LIST} {INSTANCES}");
            insert(&writer, &initial).expect("write a nonempty intersection");
            assert_intersection_input(writer.store(), &initial, 1);
            assert!(ask(writer.store(), ":x a :C", None));

            let edit = format!(
                "DELETE DATA {{ {LIST} }}; INSERT DATA {{ :C owl:intersectionOf rdf:nil . }}"
            );
            update(&writer, &edit).expect("empty the intersection");
            assert_intersection_input(writer.store(), EMPTY_AFTER_EDIT, 1);
            assert_intersection_answers(writer.store(), "http://example.com/A");
        });
    });
}

/// Spec: OWL 2 Profiles §4.3 tables 6 and 9 leave `cls-int1` without a binding
/// for its subject at zero members, so the partial rules derive no such type on rebuild.
/// OWL 2 Structural Specification §8.1.1 requires at least two intersection operands.
#[test]
fn an_empty_intersection_can_be_rebuilt_after_reopening() {
    check_cells(|equality, mode, failures| {
        for (name, data, count) in
            INTERSECTIONS
                .into_iter()
                .chain([("final state of the edit", EMPTY_AFTER_EDIT, 1)])
        {
            let label = format!("rebuild, {mode:?}, {equality}, {name}");
            cell(failures, &label, || {
                let dir = tempfile::tempdir().expect("directory");
                let config = || config(equality, StoreConfig::on_disk(dir.path()));
                let store = StoreService::new(config()).expect("store");
                load_past_the_pipeline(&store, &format!("{PREFIXES}{data}"));
                drop(store);
                let writer = pipeline(StoreService::new(config()).expect("reopen"), mode);
                assert_intersection_input(writer.store(), data, count);
                let _outcome = rebuild(writer.store(), mode);
                let asserted_type = if data == EMPTY_AFTER_EDIT {
                    "http://example.com/A"
                } else {
                    "http://example.com/D"
                };
                assert_intersection_answers(writer.store(), asserted_type);
            });
        }
    });
}

/// Spec: OWL 2 Profiles §4.3 tables 4–7 and 9 give no conclusions or violations
/// from absent list members or pairs. An empty key still binds two instances
/// through their class, so it equates them. As with the empty intersection in
/// table 6 and OWL 2 Structural Specification §8.1.1, the expected answers are
/// those of the partial rules, including when equality reaches the empty list.
#[test]
fn other_empty_list_rules_keep_their_answers() {
    let cases = [
        ("unionOf", ":C owl:unionOf", ":x a :D .", ":x a :C", false),
        (
            "propertyChainAxiom",
            ":p owl:propertyChainAxiom",
            ":x :q :y .",
            ":x :p ?y",
            false,
        ),
        (
            "hasKey",
            ":C owl:hasKey",
            ":x a :C . :y a :C .",
            ":x owl:sameAs :y",
            true,
        ),
        ("oneOf", ":C owl:oneOf", ":x a :D .", ":x a :C", false),
        (
            "AllDifferent members",
            ":d owl:members",
            ":d a owl:AllDifferent . :x owl:sameAs :y .",
            ":x owl:sameAs :y",
            true,
        ),
        (
            "AllDifferent distinctMembers",
            ":d owl:distinctMembers",
            ":d a owl:AllDifferent . :x owl:sameAs :y .",
            ":x owl:sameAs :y",
            true,
        ),
        (
            "AllDisjointClasses",
            ":d owl:members",
            ":d a owl:AllDisjointClasses . :x a :C, :D .",
            ":x a :C, :D",
            true,
        ),
        (
            "AllDisjointProperties",
            ":d owl:members",
            ":d a owl:AllDisjointProperties . :x :p :y ; :q :y .",
            ":x :p :y ; :q :y",
            true,
        ),
    ];
    check_cells(|equality, mode, failures| {
        for (name, axiom, facts, pattern, expected) in cases {
            for (head, alias) in [("rdf:nil", ""), (":l", ":l owl:sameAs rdf:nil .")] {
                let data = format!("{axiom} {head} . {alias} {facts}");
                let lists = format!("{{ {axiom} ?list }} UNION {{ :l owl:sameAs ?list }}");
                for rematerialise in [false, true] {
                    let path = if rematerialise { "rebuild" } else { "commit" };
                    let label = format!("{path}, {mode:?}, {equality}, {name} with {head}");
                    cell(failures, &label, || {
                        let store = StoreService::new(config(equality, in_memory_store_config()))
                            .expect("store");
                        let writer = pipeline(store, mode);
                        let store = writer.store();
                        if rematerialise {
                            load_past_the_pipeline(store, &format!("{PREFIXES}{data}"));
                        } else {
                            insert(&writer, &data).expect("commit list");
                        }
                        assert_list_input(store, &data, &lists, 1 + u64::from(!alias.is_empty()));
                        if rematerialise {
                            rebuild(store, mode).expect("rematerialise");
                        }
                        assert_eq!(ask(store, pattern, None), expected, "{pattern}");
                        assert_eq!(store.consistency(), ConsistencyStatus::Consistent);
                    });
                }
            }
        }
    });
}
