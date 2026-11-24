//! Incremental validation (C2) against full validation: on random data and random changes,
//! the results `validate_changes` reports are exactly those full validation finds after
//! the change and not before.

use std::collections::BTreeSet;

use nrese_engine::{Engine, EngineConfig, GraphSelector, Snapshot, TermId};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_shacl::{Selection, Shapes, compile, validate, validate_changes};
use nrese_sparql::{QueryEvaluationError, Resources};

use super::change_differential::observe::{Observer, ResultKey};

const EX: &str = "http://example.com/";
const SHAPES_GRAPH: &str = "http://example.com/shapes";
const DATA: Selection = Selection::of(GraphSelector::Exact(TermId::DEFAULT_GRAPH));

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
pub(super) fn fuzz(seed: u64) -> u64 {
    let fuzz: u64 = std::env::var("NRESE_FUZZ_SEED")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    seed ^ fuzz.wrapping_mul(0x9e37_79b9_7f4a_7c15)
}
pub(super) struct Rng(pub(super) u64);

impl Rng {
    pub(super) fn below(&mut self, n: u64) -> u64 {
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

fn type_quad(subject: NamedNode, class: NamedNode) -> Quad {
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    Quad::new(subject, rdf_type, class, GraphName::DefaultGraph)
}

fn random_quad(rng: &mut Rng) -> Quad {
    let node = |rng: &mut Rng| ex(&format!("n{}", rng.below(12)));
    let class = |rng: &mut Rng| ex(&format!("C{}", rng.below(3)));
    let subject = node(rng);
    match rng.below(16) {
        0 | 4 | 8 | 12 => return type_quad(subject, class(rng)),
        1 => {
            let subclass =
                NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#subClassOf");
            return Quad::new(class(rng), subclass, class(rng), GraphName::DefaultGraph);
        }
        _ => {}
    }
    let predicate = ex(&format!("p{}", rng.below(4)));
    let object: Term = if rng.below(3) == 0 {
        Literal::new_typed_literal(rng.below(5).to_string(), xsd::INTEGER).into()
    } else {
        node(rng).into()
    };
    Quad::new(subject, predicate, object, GraphName::DefaultGraph)
}

const CYCLIC_SHAPES: &str = include_str!("fixtures/cyclic.ttl");
const SHARED_SHAPES: &str = include_str!("fixtures/shared.ttl");

#[test]
fn incremental_validation_equals_the_difference_of_full_validations() {
    differences_agree(SHAPES, 20_261_002);
}

#[test]
fn incremental_validation_equals_the_difference_under_shapes_that_refer_back() {
    differences_agree(CYCLIC_SHAPES, 20_261_122);
}

#[test]
fn incremental_validation_equals_the_difference_under_shapes_reached_from_several_targets() {
    differences_agree(SHARED_SHAPES, 20_261_152);
}

/// SHACL §2.1.6. A deactivated parent reports no result, even if the same
/// property shape later reports a result through an active parent.
#[test]
fn a_deactivated_parent_does_not_hide_a_new_result() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let (before, shapes) = load(&engine, SHARED_SHAPES, [type_quad(ex("n2"), ex("C1"))]);
    let mut tx = engine.transaction();
    tx.insert(type_quad(ex("n2"), ex("C2")).as_ref());
    let changed: Vec<_> = tx.inserted().collect();
    let after = tx.pending_snapshot();
    let report = validate_changes(
        &before,
        &after,
        &shapes,
        DATA,
        &changed,
        &Resources::default(),
    )
    .unwrap();
    assert_eq!(report.results.len(), 1);
    let result = &report.results[0];
    assert_eq!(result.focus_node, Term::from(ex("n2")));
    assert_eq!(result.source_shape, Term::from(ex("P11")));
    assert_eq!(
        result.component.as_str(),
        nrese_shacl::Component::MinCount.iri()
    );
}

/// SHACL design §7: if a whole validation or a reverse route search runs out
/// of request memory, the change has no partial report.
#[test]
fn change_validation_returns_the_stop_instead_of_partial_results() {
    let recursive = "
        @prefix ex: <http://example.com/> .
        @prefix sh: <http://www.w3.org/ns/shacl#> .
        ex:T sh:targetClass ex:C ; sh:property ex:Q .
        ex:Q sh:path ex:age ; sh:minCount 1 ; sh:property ex:Q .
    ";
    let hub = "
        @prefix ex: <http://example.com/> .
        @prefix sh: <http://www.w3.org/ns/shacl#> .
        ex:T1 sh:targetClass ex:C ; sh:property ex:Q .
        ex:T2 sh:targetClass ex:D ; sh:property ex:R .
        ex:R sh:path ex:likes ; sh:property ex:Q .
        ex:Q sh:path ex:age ; sh:minCount 1 .
    ";
    for (case, turtle, data) in [
        (
            "cyclic full validation",
            recursive,
            (0..1024)
                .map(|i| type_quad(ex(&format!("y{i}")), ex("C")))
                .collect::<Vec<_>>(),
        ),
        (
            "reverse route search",
            hub,
            (0..1024)
                .map(|i| {
                    Quad::new(
                        ex(&format!("y{i}")),
                        ex("likes"),
                        ex("v"),
                        GraphName::DefaultGraph,
                    )
                })
                .collect(),
        ),
    ] {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let (before, shapes) = load(&engine, turtle, data);
        let mut tx = engine.transaction();
        tx.insert(type_quad(ex("v"), ex("C")).as_ref());
        let changed: Vec<_> = tx.inserted().collect();
        let after = tx.pending_snapshot();
        let resources = Resources {
            memory: nrese_exec::Envelope::new(Some(16 * 1024), None),
            ..Resources::default()
        };
        let outcome = validate_changes(&before, &after, &shapes, DATA, &changed, &resources);
        assert!(
            matches!(outcome, Err(QueryEvaluationError::MemoryLimit(_))),
            "{case}"
        );
    }
}

fn load(
    engine: &Engine,
    shapes_turtle: &str,
    data: impl IntoIterator<Item = Quad>,
) -> (Snapshot, Shapes) {
    let mut tx = engine.transaction();
    let shapes_graph = ex("shapes");
    let parser = RdfParser::from_format(RdfFormat::Turtle);
    for quad in parser.for_slice(shapes_turtle.as_bytes()) {
        let quad = quad.unwrap();
        let quad = Quad::new(
            quad.subject,
            quad.predicate,
            quad.object,
            GraphName::NamedNode(shapes_graph.clone()),
        );
        tx.insert(quad.as_ref());
    }
    for quad in data {
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    let before = engine.snapshot();
    let shapes_id = before
        .lookup(NamedNode::new_unchecked(SHAPES_GRAPH).as_ref().into())
        .unwrap();
    let shapes = compile(
        &before,
        Selection::asserted(GraphSelector::Exact(shapes_id)),
    )
    .unwrap_or_else(|errors| panic!("{errors:?}"));
    (before, shapes)
}

/// On random data and random changes, under `shapes`, the results `validate_changes`
/// reports are exactly those full validation finds after the change and not before.
fn differences_agree(shapes_turtle: &str, seed: u64) {
    let mut rng = Rng(fuzz(seed));
    let mut checked = 0;
    let covered = |checked: usize| checked > 20;
    // Draw on (up to ten times as many cases) until the run covers what the assertion
    // below asks for, so that coverage never depends on the seed (the NI fuzz test's
    // fix, a901e34; seed 66810 of the SPARQL differential tests, 12 October 2026).
    for round in 0..200 * 10 {
        if round >= 200 && covered(checked) {
            break;
        }
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let data = (0..rng.below(60)).map(|_| random_quad(&mut rng));
        let (before, shapes) = load(&engine, shapes_turtle, data);

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
        let resources = Resources::default();
        let incremental = validate_changes(&before, &after, &shapes, DATA, &changed, &resources)
            .expect("nothing stops it");
        let observer = Observer::new(&before, &after, false);
        let full_before = observer
            .observe(&validate(&before, &shapes, DATA, &resources).expect("validated"))
            .unwrap()
            .result_set();
        let full_after = observer
            .observe(&validate(&after, &shapes, DATA, &resources).expect("validated"))
            .unwrap()
            .result_set();
        let expected: BTreeSet<ResultKey> = full_after.difference(&full_before).cloned().collect();
        assert_eq!(
            observer.observe(&incremental).unwrap().result_set(),
            expected,
            "round {round}: incremental results differ from the full difference"
        );
        checked += usize::from(!expected.is_empty());
    }
    assert!(covered(checked), "only {checked} rounds introduced results");
}
