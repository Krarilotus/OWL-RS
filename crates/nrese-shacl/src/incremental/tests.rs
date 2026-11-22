use std::collections::BTreeSet;

use nrese_engine::{Engine, EngineConfig, GraphSelector, Snapshot, TermId};
use nrese_rdf::NamedNode;
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::{ReadView, Resources};

use crate::model::Shapes;
use crate::validate::Validator;
use crate::{Selection, ValidationReport, compile};

const DEFAULT: Selection = Selection::asserted(GraphSelector::Exact(TermId::DEFAULT_GRAPH));
const SHARED_SHAPES: &str = include_str!("../../tests/it/fixtures/shared.ttl");
const CYCLIC_SHAPES: &str = include_str!("../../tests/it/fixtures/cyclic.ttl");

fn snapshot(turtle: &str) -> Snapshot {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine.snapshot()
}

/// SHACL design §7: full validation for a cycle only when an active target can
/// reach it through active shapes.
#[test]
fn only_cycles_reachable_from_an_active_target_need_full_validation() {
    const LUBM: &str = include_str!("../../../../benches/fast/shapes/lubm.ttl");
    let detached_cycle = "
        <urn:loop> a sh:NodeShape ; sh:node <urn:loop> .
    ";
    let through_deactivated = "
        <urn:target> sh:targetNode <urn:x> ; sh:node <urn:disabled> .
        <urn:disabled> sh:deactivated true ; sh:node <urn:loop> .
        <urn:loop> a sh:NodeShape ; sh:node <urn:loop> .
    ";
    for (case, turtle, expected) in [
        ("shared", SHARED_SHAPES.to_owned(), false),
        ("LUBM", LUBM.to_owned(), false),
        ("targeted cycle", CYCLIC_SHAPES.to_owned(), true),
        ("untargeted cycle", format!("{LUBM}{detached_cycle}"), false),
        (
            "deactivated route",
            format!("@prefix sh: <http://www.w3.org/ns/shacl#> . {through_deactivated}"),
            false,
        ),
    ] {
        let shapes = compile(&snapshot(&turtle), DEFAULT).unwrap();
        assert_eq!(shapes.has_cycles, expected, "{case}");
    }
}

fn keys(report: &ValidationReport) -> BTreeSet<String> {
    report.results.iter().map(|r| format!("{r:?}")).collect()
}

fn full<V: ReadView + Sync>(view: &V, shapes: &Shapes, resources: &Resources) -> BTreeSet<String> {
    keys(&crate::validate(view, shapes, DEFAULT, resources).unwrap())
}

/// The change's results are full validation's after less its before (SHACL design §7),
/// also past the reverse route search's limit, where the change validation compares
/// with whole validations of the candidate's targeted ancestors instead. `ex:Q`'s result
/// at `ex:v` is reached from `ex:A` once `ex:v` is a `ex:C`; it was there before only if
/// `ex:y1`, one of `ex:v`'s likers, is a `ex:D` (a route through `ex:B` and `ex:R`).
#[test]
fn past_the_route_limit_the_change_validation_answers_as_full_validation() {
    let shapes = "
        @prefix ex: <http://example.com/> .
        @prefix sh: <http://www.w3.org/ns/shacl#> .
        @prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
        ex:A sh:targetClass ex:C ; sh:property ex:Q .
        ex:B sh:targetClass ex:D ; sh:property ex:R .
        ex:R sh:path ex:likes ; sh:property ex:Q .
        ex:Q sh:path ex:age ; sh:datatype xsd:integer .
        ex:v ex:age \"x\" .
        ex:y1 ex:likes ex:v . ex:y2 ex:likes ex:v . ex:y3 ex:likes ex:v .
        ex:y4 ex:likes ex:v . ex:y5 ex:likes ex:v .
    ";
    for (case, extra, introduced) in [
        ("there before through ex:B", "ex:y1 a ex:D .", 0),
        ("new", "", 1),
    ] {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let turtle = format!("{shapes} @prefix ex: <http://example.com/> . {extra}");
        for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(turtle.as_bytes()) {
            tx.insert(quad.unwrap().as_ref());
        }
        tx.commit().unwrap();
        let before = engine.snapshot();
        let shapes = compile(&before, DEFAULT).unwrap();
        let mut tx = engine.transaction();
        let change = "@prefix ex: <http://example.com/> . ex:v a ex:C .";
        for quad in RdfParser::from_format(RdfFormat::Turtle).for_slice(change.as_bytes()) {
            tx.insert(quad.unwrap().as_ref());
        }
        let changed: Vec<_> = tx.inserted().collect();
        let after = tx.pending_snapshot();
        let resources = Resources::default();
        let expected: BTreeSet<String> = full(&after, &shapes, &resources)
            .difference(&full(&before, &shapes, &resources))
            .cloned()
            .collect();
        assert_eq!(expected.len(), introduced, "{case}: the full difference");
        // 2 visits: the walk from (ex:Q, ex:v) stops at ex:R's likers.
        for limit in [super::MAX_AFFECTED, 2] {
            let report = super::validate_changes_within(
                &before, &after, &shapes, DEFAULT, &changed, &resources, limit,
            )
            .unwrap();
            assert_eq!(keys(&report), expected, "{case}, route limit {limit}");
        }
    }
}

/// SHACL Core targets designate the same foci in a point lookup as in a full
/// enumeration, including class targets inherited through rdfs:subClassOf.
#[test]
fn a_target_point_lookup_agrees_with_full_enumeration() {
    let view = snapshot(
        "
        @prefix ex: <http://example.com/> .
        @prefix sh: <http://www.w3.org/ns/shacl#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        ex:Node sh:targetNode ex:explicit .
        ex:Class sh:targetClass ex:Super .
        ex:Subject sh:targetSubjectsOf ex:p .
        ex:Object sh:targetObjectsOf ex:p .
        ex:sub a ex:Sub .
        ex:direct a ex:Super .
        ex:Sub rdfs:subClassOf ex:Super .
        ex:subject ex:p ex:object .
        ",
    );
    let shapes = compile(&view, DEFAULT).unwrap();
    let resources = Resources::default();
    let validator = Validator::new(&view, &shapes, DEFAULT, &resources);
    let nodes = ["explicit", "sub", "direct", "subject", "object"].map(|name| {
        let node = NamedNode::new_unchecked(format!("http://example.com/{name}"));
        view.lookup(node.as_ref().into()).unwrap()
    });
    for shape in shapes.targeted() {
        let shape = &shapes.shapes[shape];
        let listed = validator.focus_nodes(shape);
        for node in nodes {
            assert_eq!(
                validator.is_focus(shape, node),
                listed.contains(&node),
                "shape {:?}, focus {node:?}",
                shape.node,
            );
        }
    }
}
