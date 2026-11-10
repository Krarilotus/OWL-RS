//! [`optimise`]'s rewrite passes, by the shapes they can change
//! (Harness note 2026-11-10-small-query-fastpath-design.md, change 1).

use super::*;
use nrese_engine::{Engine, EngineConfig};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql_syntax::visit::NodeMut;

/// `?x1 :p ?y1`, and four `?y1 :q` objects: grouping by `?x` pays to pre-aggregate the
/// `:q` side (four rows, one `?y`).
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let data = "@prefix : <http://e/> .
        :x1 :p :y1 . :y1 :q :z1 , :z2 , :z3 , :z4 .
        :s1 a :Person ; :born \"1450-01-01\"^^<http://www.w3.org/2001/XMLSchema#date> .";
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(data.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// The rewrites that changed `query`'s pattern and how many passes ran, after `adapt`.
fn optimised(
    snapshot: &Snapshot,
    text: &str,
    adapt: impl Fn(&mut GraphPattern),
) -> (Vec<&'static str>, usize) {
    let query = Query::parse(text, None).unwrap_or_else(|e| panic!("{e}: {text}"));
    let options = QueryOptions::default();
    let ctx = Context::new(
        snapshot,
        &options,
        query_dataset(&query),
        query_base(&query),
    );
    let mut pattern = pattern_and_form(&query).0.clone();
    adapt(&mut pattern);
    let mut fired = Vec::new();
    PASSES_RUN.set(0);
    optimise(pattern, &mut fired, &ctx);
    (fired, PASSES_RUN.get())
}

/// Spec (design note, change 1): a query whose algebra has no node a pass can change runs
/// none of the passes. These are the entities benchmark's one-pattern COUNTs, its point
/// lookup and its GROUP BY of one pattern: 1 µs on main, 3–4 µs on engine-v2, most of the
/// difference these passes (profile smallq-attrib3-d212e607-1110).
#[test]
fn a_query_no_rewrite_can_change_runs_no_pass() {
    let engine = engine();
    let snapshot = engine.snapshot();
    for text in [
        "SELECT (COUNT(*) AS ?n) WHERE { ?s a <http://e/Person> }",
        "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
        "SELECT ?p ?o WHERE { <http://e/s1> ?p ?o }",
        "SELECT ?d (COUNT(*) AS ?n) WHERE { ?s <http://e/born> ?d } GROUP BY ?d ORDER BY DESC(?n) LIMIT 10",
    ] {
        assert_eq!(optimised(&snapshot, text, |_| {}), (vec![], 0), "{text}");
    }
}

/// Guard (passes before and after change 1): each pass still changes the smallest shape it
/// applies to, in the order [`optimise`] gives them, including a shape one pass exposes to
/// the next (a one-input UNION that join-groups unwraps around a join the group then
/// pre-aggregates: Sol design review, 10 Nov). Expected rewrites worked by hand from each
/// pass's rule (`plan::rewrite`, `pushdown`); a filter over one pattern, which no pass can
/// move, fires none.
#[test]
fn each_pass_still_changes_the_shapes_it_applies_to() {
    let engine = engine();
    let snapshot = engine.snapshot();
    let grouped =
        "SELECT ?x (COUNT(*) AS ?n) WHERE { ?x <http://e/p> ?y . ?y <http://e/q> ?z } GROUP BY ?x";
    type Case = (&'static str, fn(&mut GraphPattern), &'static [&'static str]);
    let cases: [Case; 6] = [
        // The parser's join of a path and a triple; join-groups puts the scans first.
        (
            "SELECT * WHERE { ?a <http://e/p>+ ?b . ?b <http://e/q> ?c }",
            |_| {},
            &["join-groups"],
        ),
        (grouped, |_| {}, &["eager-aggregation"]),
        (
            grouped,
            union_of_one_around_group_input,
            &["join-groups", "eager-aggregation"],
        ),
        (
            "SELECT * WHERE { ?a <http://e/p> ?b OPTIONAL { ?b <http://e/q> ?c } FILTER(?a = <http://e/x1>) }",
            |_| {},
            &["filter-pushdown"],
        ),
        (
            "SELECT * WHERE { ?a <http://e/p> ?b OPTIONAL { ?b <http://e/q> ?c FILTER(?c = <http://e/z1>) } }",
            |_| {},
            &["filter-pushdown"],
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://e/born> ?d FILTER(?d >= \"1450-01-01\"^^<http://www.w3.org/2001/XMLSchema#date>) }",
            |_| {},
            &[],
        ),
    ];
    // Every row reported, not only the first that differs.
    let wrong: Vec<String> = cases
        .into_iter()
        .filter_map(|(text, adapt, expected)| {
            let (fired, _) = optimised(&snapshot, text, adapt);
            (fired != expected).then(|| format!("{text}\n  fired {fired:?}, expected {expected:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The group's input put in a UNION of one input (no SPARQL text parses to that).
fn union_of_one_around_group_input(pattern: &mut GraphPattern) {
    if let GraphPattern::Group { inner, .. } = pattern {
        let input = std::mem::replace(&mut **inner, GraphPattern::Bgp { patterns: vec![] });
        **inner = GraphPattern::Union {
            inputs: vec![input],
        };
        return;
    }
    for child in pattern.children_mut() {
        if let NodeMut::Pattern(child) = child {
            union_of_one_around_group_input(child);
        }
    }
}
