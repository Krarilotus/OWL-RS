//! What the W3C suite doesn't cover: ill-formed shapes, which statements are validated,
//! recursion, severities and messages, and the report's RDF form.

use nrese_engine::{
    EncodedTriple, Engine, EngineConfig, GraphSelector, ReadModel, Snapshot, TermId,
};
use nrese_shacl::{Component, PropertyPath, Selection, Shapes, compile, validate};
use oxrdf::{Graph, GraphNameRef, NamedNode, NamedNodeRef, QuadRef, TermRef};
use oxrdfio::{RdfFormat, RdfParser};

const PREFIXES: &str = "@prefix ex: <http://example.com/> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";
const SHAPES_GRAPH: &str = "http://example.com/shapes";
const SH: &str = "http://www.w3.org/ns/shacl#";

fn insert(engine: &Engine, turtle: &str, graph: Option<&str>) {
    let graph = graph.map(NamedNodeRef::new_unchecked);
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(format!("{PREFIXES}{turtle}").as_bytes())
    {
        let quad = quad.expect("turtle");
        tx.insert(QuadRef::new(
            &quad.subject,
            &quad.predicate,
            &quad.object,
            graph.map_or(GraphNameRef::DefaultGraph, Into::into),
        ));
    }
    tx.commit().expect("commit");
}

/// An engine with `data` in the default graph and `shapes` in the shapes graph.
fn engine(data: &str, shapes: &str) -> Engine {
    let engine = Engine::new(EngineConfig::default()).expect("engine");
    insert(&engine, data, None);
    insert(&engine, shapes, Some(SHAPES_GRAPH));
    engine
}

fn id(snapshot: &Snapshot, iri: &str) -> TermId {
    snapshot
        .lookup(NamedNodeRef::new_unchecked(iri).into())
        .unwrap_or_else(|| panic!("{iri} is unknown"))
}

fn shapes_selection(snapshot: &Snapshot) -> Selection {
    Selection::asserted(GraphSelector::Exact(id(snapshot, SHAPES_GRAPH)))
}

fn compiled(snapshot: &Snapshot) -> Shapes {
    compile(snapshot, shapes_selection(snapshot)).expect("shapes")
}

const DEFAULT: Selection = Selection::of(GraphSelector::Exact(TermId::DEFAULT_GRAPH));

fn ex(local: &str) -> String {
    format!("<http://example.com/{local}>")
}

#[test]
fn ill_formed_shapes_are_errors_that_name_the_shape() {
    for (shape, expected) in [
        (
            "ex:S sh:targetNode ex:a ; sh:minCount \"many\" .",
            "sh:minCount",
        ),
        ("ex:S sh:targetNode ex:a ; sh:path \"name\" .", "sh:path"),
        (
            "ex:S sh:targetNode ex:a ; sh:path [ sh:inversePath \"x\" ] .",
            "sh:path",
        ),
        (
            "ex:S sh:targetNode ex:a ; sh:path ( ex:p ) .",
            "two members",
        ),
        ("ex:S sh:targetNode ex:a ; sh:and ex:notAList .", "sh:and"),
        ("ex:S sh:targetNode ex:a ; sh:in [ rdf:first 1 ] .", "sh:in"),
        (
            "ex:S sh:targetNode ex:a ; sh:nodeKind ex:Thing .",
            "sh:nodeKind",
        ),
        ("ex:S sh:targetNode ex:a ; sh:pattern \"(\" .", "sh:pattern"),
        ("ex:S sh:targetNode ex:a ; sh:class \"C\" .", "sh:class"),
        (
            "ex:S sh:targetNode ex:a ; sh:path ex:p, ex:q .",
            "more than one sh:path",
        ),
        // A nested shape's error is reported too.
        (
            "ex:S sh:targetNode ex:a ; sh:property [ sh:path ex:p ; sh:maxCount -1 ] .",
            "sh:maxCount",
        ),
    ] {
        let engine = engine("ex:a ex:p ex:b .", shape);
        let snapshot = engine.snapshot();
        let errors = compile(&snapshot, shapes_selection(&snapshot)).expect_err(shape);
        assert!(
            errors.iter().any(|error| error.message.contains(expected)),
            "{shape}: {errors:?}"
        );
        assert!(!errors[0].shape.is_empty());
    }
}

/// The selection decides what is validated: one graph or all, with or without the shapes
/// graph, asserted statements or also inferred ones.
#[test]
fn the_selection_decides_which_statements_are_validated() {
    let engine = engine(
        "ex:a a ex:Person ; ex:name \"A\" .",
        "ex:Typed a sh:NodeShape ; sh:targetSubjectsOf rdf:type ;
           sh:property [ sh:path ex:name ; sh:minCount 1 ] .",
    );
    insert(&engine, "ex:b a ex:Person .", Some("http://example.com/g1"));
    // An inferred type, as the reasoner would store it.
    let mut tx = engine.transaction();
    let term = |iri: &str| tx.intern(NamedNodeRef::new_unchecked(iri).into());
    let inferred = EncodedTriple::new(
        term("http://example.com/c"),
        term("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        term("http://example.com/Agent"),
    );
    tx.insert_inferred(inferred);
    tx.commit().expect("commit");

    let snapshot = engine.snapshot();
    let shapes = compiled(&snapshot);
    let focus_nodes = |selection: Selection| -> Vec<String> {
        let mut nodes: Vec<String> = validate(&snapshot, &shapes, selection)
            .results
            .iter()
            .map(|result| result.focus_node.to_string())
            .collect();
        nodes.sort();
        nodes
    };
    // The default graph: ex:a has a name, the inferred ex:c doesn't.
    assert_eq!(focus_nodes(DEFAULT), [ex("c")]);
    // Asserted statements only: no ex:c.
    let asserted = Selection {
        model: ReadModel::Asserted,
        ..DEFAULT
    };
    assert!(focus_nodes(asserted).is_empty());
    // Every graph: ex:b in g1, and the shape itself, which is typed in the shapes graph.
    let all = Selection::of(GraphSelector::Any);
    assert_eq!(focus_nodes(all), [ex("Typed"), ex("b"), ex("c")]);
    // Every graph except the shapes graph.
    let data = all.excluding(id(&snapshot, SHAPES_GRAPH));
    assert_eq!(focus_nodes(data), [ex("b"), ex("c")]);
}

/// Shapes that refer to themselves terminate, and still find a violation down the chain.
#[test]
fn recursive_shapes_terminate() {
    let shapes = "ex:Person a sh:NodeShape ; sh:targetNode ex:a ;
        sh:property [ sh:path ex:name ; sh:minCount 1 ] ;
        sh:property [ sh:path ex:knows ; sh:node ex:Person ] .";
    let cycle = "ex:a ex:name \"A\" ; ex:knows ex:b . ex:b ex:name \"B\" ; ex:knows ex:a .";
    let engine_ok = engine(cycle, shapes);
    let snapshot = engine_ok.snapshot();
    assert!(validate(&snapshot, &compiled(&snapshot), DEFAULT).conforms());

    let broken = "ex:a ex:name \"A\" ; ex:knows ex:b . ex:b ex:knows ex:a .";
    let engine_broken = engine(broken, shapes);
    let snapshot = engine_broken.snapshot();
    let report = validate(&snapshot, &compiled(&snapshot), DEFAULT);
    assert_eq!(report.results.len(), 1, "{report:#?}");
    assert_eq!(report.results[0].component, Component::Node);
    assert_eq!(
        report.results[0].value.as_ref().map(ToString::to_string),
        Some(ex("b"))
    );
}

#[test]
fn severities_messages_and_deactivation() {
    let engine = engine(
        "ex:a ex:age 200 .",
        "ex:Age sh:targetNode ex:a ; sh:severity sh:Warning ; sh:message \"too old\"@en ;
           sh:path ex:age ; sh:maxInclusive 150 .
         ex:Custom sh:targetNode ex:a ; sh:severity ex:Blocker ; sh:path ex:age ; sh:minCount 2 .
         ex:Off sh:targetNode ex:a ; sh:deactivated true ; sh:path ex:age ; sh:maxCount 0 .",
    );
    let snapshot = engine.snapshot();
    let report = validate(&snapshot, &compiled(&snapshot), DEFAULT);
    assert!(!report.conforms(), "a warning is a result too");
    let mut found: Vec<(String, String, Vec<String>)> = report
        .results
        .iter()
        .map(|result| {
            (
                result.source_shape.to_string(),
                result.severity.to_string(),
                result.messages.iter().map(ToString::to_string).collect(),
            )
        })
        .collect();
    found.sort();
    assert_eq!(
        found,
        [
            (
                ex("Age"),
                format!("<{SH}Warning>"),
                vec!["\"too old\"@en".to_owned()]
            ),
            (ex("Custom"), ex("Blocker"), vec![]),
        ]
    );
}

/// The report as RDF: `sh:conforms`, one node per result, and paths written as SHACL
/// writes them.
#[test]
fn the_report_is_an_rdf_graph() {
    let sh = |local: &str| NamedNode::new_unchecked(format!("{SH}{local}"));
    let engine = engine(
        "ex:a ex:parent ex:b . ex:c ex:parent ex:b . ex:b ex:name 1 .",
        "ex:Siblings sh:targetNode ex:a ;
           sh:path ( ex:parent [ sh:inversePath ex:parent ] ) ; sh:maxCount 1 .
         ex:Fine sh:targetNode ex:b ; sh:path ex:name ; sh:minCount 1 .",
    );
    let snapshot = engine.snapshot();
    let report = validate(&snapshot, &compiled(&snapshot), DEFAULT);
    assert_eq!(report.results.len(), 1);
    let path = report.results[0].path.clone().expect("path");
    assert_eq!(
        path.to_string(),
        "(<http://example.com/parent>/^<http://example.com/parent>)"
    );
    assert!(matches!(path, PropertyPath::Sequence(_)));

    let graph: Graph = report.to_triples().into_iter().collect();
    let conforms: Vec<String> = graph
        .triples_for_predicate(&sh("conforms"))
        .map(|triple| triple.object.to_string())
        .collect();
    assert_eq!(
        conforms,
        ["\"false\"^^<http://www.w3.org/2001/XMLSchema#boolean>"]
    );
    let result = graph
        .triples_for_predicate(&sh("result"))
        .next()
        .expect("a result")
        .object;
    let TermRef::BlankNode(result) = result else {
        panic!("a result is a blank node");
    };
    let field = |local: &str| {
        graph
            .object_for_subject_predicate(result, &sh(local))
            .map(|term| term.to_string())
    };
    assert_eq!(field("focusNode"), Some(ex("a")));
    assert_eq!(field("sourceShape"), Some(ex("Siblings")));
    assert_eq!(
        field("sourceConstraintComponent"),
        Some(format!("<{SH}MaxCountConstraintComponent>"))
    );
    assert_eq!(field("resultSeverity"), Some(format!("<{SH}Violation>")));
    assert_eq!(field("value"), None);
    // The path is a two-member list whose second member is an inverse path.
    assert_eq!(graph.triples_for_predicate(&sh("inversePath")).count(), 1);
    let first = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#first");
    assert_eq!(graph.triples_for_predicate(&first).count(), 2);

    // A conforming report says so and has no results.
    let empty = nrese_shacl::ValidationReport::default();
    let graph: Graph = empty.to_triples().into_iter().collect();
    assert_eq!(graph.len(), 2);
    assert!(empty.conforms());
}
