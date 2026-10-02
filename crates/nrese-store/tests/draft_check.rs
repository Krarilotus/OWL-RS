//! Draft checks: isolated SHACL, query and reasoning answers over pinned N-Triples, with
//! the semantics of DMW's offline checks.

use nrese_store::{
    CancellationToken, DraftInputs, DraftLimits, DraftOperation, DraftOutcome, DraftStatus,
    run_draft_check,
};

const RDF_TYPE: &str = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>";
const SUBCLASS: &str = "<http://www.w3.org/2000/01/rdf-schema#subClassOf>";
const SH: &str = "http://www.w3.org/ns/shacl#";

const LIMITS: DraftLimits = DraftLimits {
    max_seconds: 30.0,
    max_results: 10,
    max_triples: 1_000,
};

fn ex(name: &str) -> String {
    format!("<https://example.test/{name}>")
}

fn sh(name: &str) -> String {
    format!("<{SH}{name}>")
}

/// A node shape on `class` whose property `name` needs at least one value.
fn name_required(class: &str) -> String {
    format!(
        "{shape} {RDF_TYPE} {node_shape} .\n\
         {shape} {target} {class} .\n\
         {shape} {property} _:p .\n\
         _:p {path} {name} .\n\
         _:p {min} \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n",
        shape = ex("PersonShape"),
        node_shape = sh("NodeShape"),
        target = sh("targetClass"),
        class = ex(class),
        property = sh("property"),
        path = sh("path"),
        name = ex("name"),
        min = sh("minCount"),
    )
}

fn check(operation: DraftOperation, inputs: DraftInputs) -> DraftOutcome {
    run_draft_check(operation, &inputs, &LIMITS, &CancellationToken::new(), None)
}

fn shacl(data: &str, schema: &str, shapes: &str) -> DraftOutcome {
    check(
        DraftOperation::Shacl,
        DraftInputs {
            data: Some(data.to_owned()),
            schema: Some(schema.to_owned()),
            shapes: Some(shapes.to_owned()),
            query: None,
        },
    )
}

fn query(data: &str, text: &str) -> DraftOutcome {
    check(
        DraftOperation::Query,
        DraftInputs {
            data: Some(data.to_owned()),
            query: Some(text.to_owned()),
            ..DraftInputs::default()
        },
    )
}

#[test]
fn a_conforming_shacl_check_is_complete_applicable_and_meta_validated() {
    let data = format!(
        "{a} {RDF_TYPE} {person} .\n{a} {name} \"Alice\" .\n",
        a = ex("alice"),
        person = ex("Person"),
        name = ex("name"),
    );
    let outcome = shacl(&data, "", &name_required("Person"));
    assert_eq!(
        outcome.terminal_status,
        DraftStatus::Completed,
        "{outcome:?}"
    );
    assert!(outcome.complete);
    assert_eq!(outcome.shacl_conforms, Some(true));
    assert_eq!(outcome.shacl_applicable, Some(true));
    assert_eq!(outcome.shacl_meta_validated, Some(true));
}

#[test]
fn a_violation_does_not_conform() {
    let data = format!("{} {RDF_TYPE} {} .\n", ex("alice"), ex("Person"));
    let outcome = shacl(&data, "", &name_required("Person"));
    assert_eq!(outcome.terminal_status, DraftStatus::Completed);
    assert_eq!(outcome.shacl_conforms, Some(false));
}

#[test]
fn a_shape_that_selects_nothing_is_not_applicable() {
    let data = format!("{} {RDF_TYPE} {} .\n", ex("rome"), ex("Place"));
    let outcome = shacl(&data, "", &name_required("Person"));
    assert_eq!(outcome.shacl_conforms, Some(true));
    assert_eq!(outcome.shacl_applicable, Some(false));
}

#[test]
fn schema_subclass_links_make_subclass_instances_focus_nodes() {
    let data = format!("{} {RDF_TYPE} {} .\n", ex("bob"), ex("Student"));
    let schema = format!("{} {SUBCLASS} {} .\n", ex("Student"), ex("Person"));
    let outcome = shacl(&data, &schema, &name_required("Person"));
    assert_eq!(outcome.shacl_applicable, Some(true));
    assert_eq!(outcome.shacl_conforms, Some(false));
}

#[test]
fn shacl_beyond_core_is_unsupported_not_evaluated() {
    let shapes = format!(
        "{}{} {} _:s .\n_:s {} \"ASK {{}}\" .\n",
        name_required("Person"),
        ex("PersonShape"),
        sh("sparql"),
        sh("ask"),
    );
    let outcome = shacl("", "", &shapes);
    assert_eq!(outcome.terminal_status, DraftStatus::Unsupported);
    assert!(!outcome.complete);
    assert!(
        outcome
            .unsupported
            .iter()
            .any(|item| item.contains("sh:sparql"))
    );
}

#[test]
fn an_ill_formed_shapes_graph_fails() {
    let shapes = format!(
        "{} {RDF_TYPE} {} .\n{} {} \"not a count\" .\n",
        ex("S"),
        sh("NodeShape"),
        ex("S"),
        sh("minCount"),
    );
    let outcome = shacl("", "", &shapes);
    assert_eq!(outcome.terminal_status, DraftStatus::Failed, "{outcome:?}");
    assert_eq!(outcome.shacl_conforms, None);
}

#[test]
fn ask_and_select_answer_with_dmw_terms() {
    let data = format!(
        "{a} {name} \"Alice\"@en .\n{a} {born} \"1452\"^^<http://www.w3.org/2001/XMLSchema#gYear> .\n",
        a = ex("alice"),
        name = ex("name"),
        born = ex("born"),
    );
    let ask = query(&data, &format!("ASK {{ {} ?p ?o }}", ex("alice")));
    assert_eq!(ask.observed_ask, Some(true));
    assert_eq!(ask.observed_rows, None);
    let select = query(
        &data,
        &format!("SELECT ?s ?o WHERE {{ ?s {} ?o }}", ex("name")),
    );
    let rows = select.observed_rows.expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["s"].kind, "uri");
    assert_eq!(rows[0]["o"].language.as_deref(), Some("en"));
    let typed = query(
        &data,
        &format!("SELECT ?y WHERE {{ ?s {} ?y }}", ex("born")),
    );
    let year = &typed.observed_rows.expect("rows")[0]["y"];
    assert_eq!(
        year.datatype.as_deref(),
        Some("http://www.w3.org/2001/XMLSchema#gYear")
    );
}

#[test]
fn more_rows_than_the_limit_fail_instead_of_truncating() {
    let data: String = (0..11)
        .map(|index| format!("{} {} \"{index}\" .\n", ex(&format!("n{index}")), ex("p")))
        .collect();
    let outcome = query(&data, "SELECT ?s WHERE { ?s ?p ?o }");
    assert_eq!(outcome.terminal_status, DraftStatus::Failed);
    assert!(outcome.observed_rows.is_none());
}

#[test]
fn graph_queries_are_unsupported() {
    let outcome = query("", "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }");
    assert_eq!(outcome.terminal_status, DraftStatus::Unsupported);
}

#[test]
fn inputs_beyond_the_triple_limit_or_not_n_triples_fail() {
    let limits = DraftLimits {
        max_triples: 1,
        ..LIMITS
    };
    let data = format!(
        "{} {} \"1\" .\n{} {} \"2\" .\n",
        ex("a"),
        ex("p"),
        ex("b"),
        ex("p")
    );
    let outcome = run_draft_check(
        DraftOperation::Query,
        &DraftInputs {
            data: Some(data),
            query: Some("ASK {}".to_owned()),
            ..DraftInputs::default()
        },
        &limits,
        &CancellationToken::new(),
        None,
    );
    assert_eq!(outcome.terminal_status, DraftStatus::Failed);
    assert!(outcome.detail.contains("max_triples"));
    let broken = query("this is not N-Triples", "ASK {}");
    assert_eq!(broken.terminal_status, DraftStatus::Failed);
}

#[test]
fn reasoning_reports_consistency_and_unsatisfiable_classes() {
    let disjoint = "<http://www.w3.org/2002/07/owl#disjointWith>";
    let class = "<http://www.w3.org/2002/07/owl#Class>";
    let schema = format!(
        "{a} {RDF_TYPE} {class} .\n{b} {RDF_TYPE} {class} .\n{c} {RDF_TYPE} {class} .\n\
         {a} {disjoint} {b} .\n{c} {SUBCLASS} {a} .\n{c} {SUBCLASS} {b} .\n",
        a = ex("A"),
        b = ex("B"),
        c = ex("C"),
    );
    let consistent = check(
        DraftOperation::Reasoning,
        DraftInputs {
            data: Some(format!("{} {RDF_TYPE} {} .\n", ex("x"), ex("A"))),
            schema: Some(schema.clone()),
            ..DraftInputs::default()
        },
    );
    assert_eq!(
        consistent.terminal_status,
        DraftStatus::Completed,
        "{consistent:?}"
    );
    assert_eq!(consistent.logical_consistent, Some(true));
    assert!(
        consistent
            .unsatisfiable_classes
            .contains(&"https://example.test/C".to_owned()),
        "{consistent:?}"
    );
    let clash = check(
        DraftOperation::Reasoning,
        DraftInputs {
            data: Some(format!(
                "{x} {RDF_TYPE} {a} .\n{x} {RDF_TYPE} {b} .\n",
                x = ex("x"),
                a = ex("A"),
                b = ex("B"),
            )),
            schema: Some(schema),
            ..DraftInputs::default()
        },
    );
    assert_eq!(clash.logical_consistent, Some(false), "{clash:?}");
}

#[test]
fn a_cancelled_check_times_out() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let outcome = run_draft_check(
        DraftOperation::Query,
        &DraftInputs {
            data: Some(format!("{} {} \"1\" .\n", ex("a"), ex("p"))),
            query: Some("SELECT * WHERE { ?s ?p ?o }".to_owned()),
            ..DraftInputs::default()
        },
        &LIMITS,
        &cancellation,
        None,
    );
    assert_eq!(outcome.terminal_status, DraftStatus::Timeout, "{outcome:?}");
}

#[test]
fn pinned_input_hashes_must_match_their_inputs() {
    use nrese_store::input_hash_mismatch;
    use sha2::{Digest, Sha256};
    let inputs = DraftInputs {
        data: Some("data".to_owned()),
        query: Some("ASK {}".to_owned()),
        ..DraftInputs::default()
    };
    let hash = |text: &str| format!("{:x}", Sha256::digest(text.as_bytes()));
    let mut hashes = std::collections::BTreeMap::from([
        ("@active_data".to_owned(), hash("data")),
        ("@query".to_owned(), hash("ASK {}")),
        ("validation".to_owned(), "echoed, not checked".to_owned()),
    ]);
    assert_eq!(input_hash_mismatch(&inputs, &hashes), None);
    hashes.insert("@active_data".to_owned(), hash("other"));
    assert!(
        input_hash_mismatch(&inputs, &hashes)
            .is_some_and(|problem| problem.contains("@active_data"))
    );
    hashes.insert("@active_data".to_owned(), hash("data"));
    hashes.insert("@active_shapes".to_owned(), hash("shapes"));
    assert!(
        input_hash_mismatch(&inputs, &hashes).is_some_and(|problem| problem.contains("missing"))
    );
}
