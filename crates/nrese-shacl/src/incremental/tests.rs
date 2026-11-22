use nrese_engine::{Engine, EngineConfig, GraphSelector, Snapshot, TermId};
use nrese_rdf::NamedNode;
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::Resources;

use crate::validate::Validator;
use crate::{Selection, compile};

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
