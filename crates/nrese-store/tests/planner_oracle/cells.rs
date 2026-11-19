use nrese_store::SparqlQueryRequest;

use crate::generator::{self, Case};
use crate::routes::{self, Routes, StoreRoute};
use crate::sweep::assert_case;
use crate::twin::Query;

fn drawing(shape: &'static str) {
    assert_case(&generator::drawing(shape));
}

// Each group evaluates its own expressions before its solutions join.
#[test]
fn one_blank_node_for_one_group_solution() {
    drawing("site-1");
}

#[test]
fn a_drawing_filter_keeps_all_copies_or_none() {
    drawing("site-2");
}

#[test]
fn an_optional_condition_decides_once_for_its_group() {
    drawing("optional-condition");
}

#[test]
fn duplicate_keys_copy_the_same_draw() {
    drawing("duplicate-keys");
}

#[test]
fn a_sibling_binding_does_not_repeat_a_nested_draw() {
    drawing("nested-sibling");
}

#[test]
fn an_unrelated_filter_draws_once() {
    drawing("unrelated-group");
}

#[test]
fn an_unbound_seed_meets_solutions_already_drawn() {
    drawing("undef");
}

#[test]
fn a_named_graph_copies_a_group_draw() {
    drawing("graph-duplicates");
}

#[test]
fn each_union_arm_draws_before_its_join() {
    drawing("union-arms");
}

#[test]
fn values_inside_a_group_count_only_at_that_site() {
    drawing("group-values");
}

#[test]
#[ignore = "an EXISTS that draws decides once for all the rows it tests"]
fn exists_draws_independently_for_each_tested_row() {
    drawing("exists-independent");
}

#[test]
#[ignore = "left rows with the same correlated values share one right-side draw"]
fn lateral_draws_for_each_left_row() {
    drawing("lateral-draw");
}

#[test]
fn a_path_filter_keeps_or_drops_both_copies_of_each_path() {
    drawing("path-draw");
}

#[test]
fn vector_hits_copy_the_draw_from_an_unrelated_group() {
    drawing("vector-draw");
}

#[test]
#[ignore = "SERVICE evaluates its pattern again for each batch of bound values it sends"]
fn a_remote_solution_draws_once_even_if_joined_in_batches() {
    drawing("service-batches");
}

#[test]
#[ignore = "streamed AVG can evaluate its argument separately for the sum and the count"]
fn streamed_average_uses_the_same_arguments_for_sum_and_count() {
    drawing("average-draw");
}

#[test]
fn an_optional_filter_with_an_unbound_input_preserves_left_rows() {
    // SPARQL LeftJoin keeps the left solution when the condition errors.
    let case = Case::new(
        "optional-error",
        vec!["<urn:a> <urn:p> 1 .".into()],
        Query::plain(
            "SELECT ?s ?x { ?s <urn:p> ?o OPTIONAL { VALUES ?x { 1 } FILTER(?missing > ?x) } }"
                .into(),
        ),
    );
    let routes = Routes::open(&case);
    let rows = routes.native(&case.query.text, true);
    assert_eq!(
        rows,
        [std::collections::BTreeMap::from([(
            "s".into(),
            "<urn:a>".into()
        )])]
    );
    assert_case(&case);
}

#[test]
fn explain_distinguishes_a_rewrite_from_the_as_written_plan() {
    // The join-groups rewrite joins a group holding an OPTIONAL to the triple
    // pattern beside it (the parser merges plain adjacent patterns itself).
    // Planned, EXPLAIN reports it; as written, nothing is rewritten. The one
    // subject has every statement: one solution either way.
    let case = Case::new(
        "explain-join",
        vec![
            "<urn:a> <urn:p> 1 .".into(),
            "<urn:a> <urn:q> \"z\" .".into(),
            "<urn:a> <urn:r> 2 .".into(),
        ],
        Query::plain(
            "SELECT ?s ?z { { ?s <urn:p> ?o OPTIONAL { ?s <urn:q> ?z } } ?s <urn:r> ?w }".into(),
        ),
    );
    let routes = Routes::open(&case);
    let store = routes.store(StoreRoute::QlOff);
    for as_written in [false, true] {
        let mut request = SparqlQueryRequest::all(&case.query.text);
        request.as_written = as_written;
        assert_eq!(routes::execute(store, &request).len(), 1);
        let explanation = routes::explain(store, &request);
        assert_eq!(explanation.rows, 1);
        assert_eq!(
            explanation.rewrites.contains(&"join-groups"),
            !as_written,
            "{explanation:?}"
        );
    }
}

#[test]
fn explain_agrees_with_the_mode_of_a_cold_ql_request() {
    // ASK carries an ask-limit rewrite only in planned mode. To observe the
    // cold run independently, a path reachable from a single starting node
    // has one row. The disjoint chain is too large to close as written.
    let mut data = vec![
        "<urn:C> <http://www.w3.org/2000/01/rdf-schema#subClassOf> [
            a <http://www.w3.org/2002/07/owl#Restriction> ;
            <http://www.w3.org/2002/07/owl#onProperty> <urn:r> ;
            <http://www.w3.org/2002/07/owl#someValuesFrom> <http://www.w3.org/2002/07/owl#Thing> ] .".into(),
        "<urn:t> a <urn:C> ; <urn:r> <urn:w> .".into(),
        "<urn:x> <urn:t> <urn:y> .".into(),
        "<urn:y> <urn:next> <urn:z> .".into(),
    ];
    data.extend((0..1500).map(|i| format!("<urn:chain:{i}> <urn:next> <urn:chain:{}> .", i + 1)));
    let query = Query::plain(
        "ASK { { <urn:t> <urn:r> ?anon . ?p <urn:r> ?anon . ?s ?p ?o } ?o <urn:next>+ ?end }"
            .into(),
    );
    let routes = Routes::open(&Case::new("explain-ql-mode", data, query.clone()));
    let store = routes.store(StoreRoute::QlOn);
    let mut request = SparqlQueryRequest::all(query.text);
    let unrestricted = routes::execute_bool(store, &request).unwrap();
    assert!(unrestricted, "the reachable path supplies one solution");
    let explanation = routes::explain(store, &request);
    assert_eq!(explanation.rows, 1);
    let reports_planned = explanation.rewrites.contains(&"ask-limit");

    request.memory_limit = Some(1 << 20);
    request.as_written = true;
    store.clear_query_cache();
    let forced = routes::execute_bool(store, &request).unwrap_err();
    assert!(forced.is_memory_limit(), "as-written control: {forced}");

    request.as_written = false;
    store.clear_query_cache();
    let ran_planned = match routes::execute_bool(store, &request) {
        Ok(answer) => {
            assert!(answer, "the bounded request lost the solution");
            true
        }
        Err(error) if error.is_memory_limit() => false,
        Err(error) => panic!("bounded request: {error}"),
    };
    assert_eq!(ran_planned, reports_planned, "EXPLAIN: {explanation:?}");
}
