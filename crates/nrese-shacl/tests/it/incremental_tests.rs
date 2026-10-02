//! Incremental validation (C2) against full validation: on random data and random changes,
//! the results `validate_changes` reports are exactly those full validation finds after
//! the change and not before.

use std::collections::BTreeSet;

use nrese_engine::{Engine, EngineConfig, GraphSelector, TermId};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_shacl::{Selection, ValidationReport, compile, validate, validate_changes};

const EX: &str = "http://example.com/";
const SHAPES_GRAPH: &str = "http://example.com/shapes";

const SHAPES: &str = r#"
@prefix ex: <http://example.com/> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
ex:S1 a sh:NodeShape ; sh:targetClass ex:C0 ;
  sh:property [ sh:path ex:p0 ; sh:minCount 1 ; sh:maxCount 2 ] ;
  sh:property [ sh:path ( ex:p1 ex:p2 ) ; sh:class ex:C1 ] .
ex:S2 a sh:NodeShape ; sh:targetSubjectsOf ex:p3 ;
  sh:property [ sh:path [ sh:inversePath ex:p0 ] ; sh:maxCount 1 ] ;
  sh:node ex:S3 .
ex:S3 a sh:NodeShape ;
  sh:property [ sh:path [ sh:zeroOrMorePath ex:p1 ] ; sh:nodeKind sh:IRI ] ;
  sh:property [ sh:path ex:p2 ; sh:lessThan ex:p3 ] .
ex:S4 a sh:NodeShape ; sh:targetObjectsOf ex:p2 ;
  sh:or ( [ sh:class ex:C2 ] [ sh:path ex:p0 ; sh:minCount 1 ] ) .
ex:S5 a sh:NodeShape ; sh:targetNode ex:n0 , ex:n1 ;
  sh:property [ sh:path ( [ sh:inversePath ex:p1 ] ex:p0 ) ; sh:datatype <http://www.w3.org/2001/XMLSchema#integer> ] ;
  sh:not [ sh:class ex:C1 ] .
"#;

/// `seed` varied by `NRESE_FUZZ_SEED` (a number) for bug hunts over many seeds; without
/// it, the same cases on every run.
fn fuzz(seed: u64) -> u64 {
    let fuzz: u64 = std::env::var("NRESE_FUZZ_SEED")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    seed ^ fuzz.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % n
    }
}

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn random_quad(rng: &mut Rng) -> Quad {
    let node = |rng: &mut Rng| ex(&format!("n{}", rng.below(12)));
    let subject = node(rng);
    if rng.below(4) == 0 {
        let class = ex(&format!("C{}", rng.below(3)));
        let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        return Quad::new(subject, rdf_type, class, GraphName::DefaultGraph);
    }
    let predicate = ex(&format!("p{}", rng.below(4)));
    let object: Term = if rng.below(3) == 0 {
        Literal::new_typed_literal(rng.below(5).to_string(), xsd::INTEGER).into()
    } else {
        node(rng).into()
    };
    Quad::new(subject, predicate, object, GraphName::DefaultGraph)
}

fn keys(report: &ValidationReport) -> BTreeSet<String> {
    report
        .results
        .iter()
        .map(|r| {
            format!(
                "{} | {:?} | {:?} | {} | {}",
                r.focus_node, r.path, r.value, r.source_shape, r.component
            )
        })
        .collect()
}

#[test]
fn incremental_validation_equals_the_difference_of_full_validations() {
    let mut rng = Rng(fuzz(20_261_002));
    let mut checked = 0;
    for round in 0..200 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let shapes_graph = ex("shapes");
        let parser = RdfParser::from_format(RdfFormat::Turtle);
        for quad in parser.for_slice(SHAPES.as_bytes()) {
            let quad = quad.unwrap();
            let quad = Quad::new(
                quad.subject,
                quad.predicate,
                quad.object,
                GraphName::NamedNode(shapes_graph.clone()),
            );
            tx.insert(quad.as_ref());
        }
        for _ in 0..rng.below(60) {
            tx.insert(random_quad(&mut rng).as_ref());
        }
        tx.commit().unwrap();
        let before = engine.snapshot();
        let shapes_id: TermId = before
            .lookup(NamedNode::new_unchecked(SHAPES_GRAPH).as_ref().into())
            .unwrap();
        let shapes = compile(
            &before,
            Selection::asserted(GraphSelector::Exact(shapes_id)),
        )
        .unwrap_or_else(|errors| panic!("{errors:?}"));
        let data = Selection::of(GraphSelector::Exact(TermId::DEFAULT_GRAPH));

        let mut tx = engine.transaction();
        for _ in 0..1 + rng.below(6) {
            let quad = random_quad(&mut rng);
            if rng.below(2) == 0 {
                tx.insert(quad.as_ref());
            } else {
                // Remove something that is there, mostly.
                let existing: Vec<_> = before
                    .quads_for_pattern(&nrese_engine::QuadPattern::in_graph(TermId::DEFAULT_GRAPH))
                    .collect();
                if existing.is_empty() {
                    tx.insert(quad.as_ref());
                } else {
                    let pick = existing[rng.below(existing.len() as u64) as usize];
                    tx.remove_encoded(pick);
                }
            }
        }
        let changed: Vec<_> = tx.inserted().chain(tx.deleted()).collect();
        let after = tx.pending_snapshot();
        let incremental = validate_changes(&before, &after, &shapes, data, &changed);
        let full_before = keys(&validate(&before, &shapes, data));
        let full_after = keys(&validate(&after, &shapes, data));
        let expected: BTreeSet<String> = full_after.difference(&full_before).cloned().collect();
        assert_eq!(
            keys(&incremental),
            expected,
            "round {round}: incremental results differ from the full difference"
        );
        checked += usize::from(!expected.is_empty());
    }
    assert!(checked > 20, "only {checked} rounds introduced results");
}
