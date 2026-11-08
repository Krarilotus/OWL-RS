//! What the W3C suite doesn't cover: ill-formed shapes, which statements are validated,
//! recursion, severities and messages, and the report's RDF form.

use nrese_engine::{
    EncodedTriple, Engine, EngineConfig, GraphSelector, ReadModel, Snapshot, TermId,
};
use nrese_rdf::{Graph, GraphNameRef, NamedNode, NamedNodeRef, QuadRef, TermRef};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_shacl::{Component, PropertyPath, Selection, Shapes, ValidationReport, compile};
use nrese_sparql::{CancellationToken, QueryEvaluationError, ReadView, Resources};

/// Validation on resources that never stop it.
fn validate<V: ReadView + Sync>(view: &V, shapes: &Shapes, data: Selection) -> ValidationReport {
    nrese_shacl::validate(view, shapes, data, &Resources::default()).expect("nothing stops it")
}

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
    assert_eq!(report.results[0].component.as_str(), Component::Node.iri());
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

/// `$PATH` is the property shape's path where it is a triple pattern's predicate, and only
/// there (SHACL §5.2): a literal and a longer name keep their text, and any other use is
/// ill-formed. A sequence path joins through its middle nodes as the path written there
/// would: each solution is one result, so two ways to the same value are two results.
#[test]
fn path_is_substituted_only_as_a_predicate() {
    let literal = "ex:a ex:p \"$PATH\" .";
    let two_ways = "ex:a ex:p ex:m1, ex:m2 . ex:m1 ex:q ex:z . ex:m2 ex:q ex:z .";
    for (data, path, query, expected) in [
        (
            literal,
            "ex:p",
            r#"SELECT $this ?value WHERE { $this $PATH ?value FILTER(STR(?value) != "$PATH") }"#,
            Ok(0),
        ),
        (
            literal,
            "ex:p",
            "SELECT $this ?value WHERE { $this $PATH ?value
               OPTIONAL { ?value <http://example.com/q> ?PATHX } FILTER(!BOUND(?PATHX)) }",
            Ok(1),
        ),
        (
            literal,
            "ex:p",
            "SELECT $this ?value WHERE { $this $PATH ?value . ?value <http://example.com/q> $PATH }",
            Err("other than as a predicate"),
        ),
        (
            two_ways,
            "( ex:p ex:q )",
            "SELECT $this ?value WHERE { $this $PATH ?value }",
            Ok(2),
        ),
    ] {
        let engine = engine(
            data,
            &format!(
                "ex:S sh:targetNode ex:a ; sh:property [ sh:path {path} ; sh:sparql [ sh:select '''{query}''' ] ] ."
            ),
        );
        let snapshot = engine.snapshot();
        match (compile(&snapshot, shapes_selection(&snapshot)), expected) {
            (Ok(shapes), Ok(results)) => {
                let report = validate(&snapshot, &shapes, DEFAULT);
                assert!(report.failures.is_empty(), "{query}: {:?}", report.failures);
                assert_eq!(report.results.len(), results, "{query}");
            }
            (Err(errors), Err(problem)) => assert!(
                errors.iter().any(|error| error.message.contains(problem)),
                "{query}: {errors:?}"
            ),
            (outcome, _) => panic!("{query}: {:?}", outcome.map(|shapes| shapes.len())),
        }
    }
}

/// SHACL-SPARQL reads the statements Core reads: a shapes graph left out of the data is
/// left out of the queries' default graph too, so the two agree on a data-like statement in
/// it, and `GRAPH $shapesGraph` still reads the shapes (RSH-06).
#[test]
fn sparql_constraints_read_the_data_core_reads() {
    let engine = engine(
        "ex:a ex:p ex:b .",
        r#"ex:a ex:q ex:c .
        ex:Core sh:targetNode ex:a ; sh:property [ sh:path ex:q ; sh:maxCount 0 ] .
        ex:Sparql sh:targetNode ex:a ;
            sh:sparql [ sh:select "SELECT $this WHERE { $this <http://example.com/q> ?o }" ] .
        ex:Shapes sh:targetNode ex:a ; sh:sparql [ sh:select
            "SELECT $this WHERE { GRAPH $shapesGraph { $this <http://example.com/q> ?o } }" ] ."#,
    );
    let snapshot = engine.snapshot();
    let shapes = compiled(&snapshot);
    let data = Selection::of(GraphSelector::Any).excluding(id(&snapshot, SHAPES_GRAPH));
    let report = validate(&snapshot, &shapes, data);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let sources: Vec<String> = report
        .results
        .iter()
        .map(|result| result.source_shape.to_string())
        .collect();
    assert_eq!(
        sources,
        [ex("Shapes")],
        "only the query that reads the shapes graph"
    );
}

/// A selection's form doesn't change what a SPARQL constraint reads: its own `FROM` or
/// `FROM NAMED` is replaced by the selected data whatever the selection's form, so leaving
/// out a graph that holds nothing changes nothing, and the default graph alone is pinned as
/// every other selection is (review N11 r1, F2).
#[test]
fn a_constraints_dataset_is_the_selection_whatever_its_form() {
    for clause in [
        "FROM <http://example.com/shapes>",
        "FROM NAMED <http://example.com/shapes>",
    ] {
        let engine = engine(
            "ex:a ex:p ex:b .",
            &format!(
                "ex:Sparql sh:targetNode ex:a ; sh:sparql [ sh:select
                  \"SELECT $this {clause} WHERE {{ $this <http://example.com/p> ?o }}\" ] ."
            ),
        );
        let snapshot = engine.snapshot();
        let shapes = compiled(&snapshot);
        let results = |selection: Selection| {
            let report = validate(&snapshot, &shapes, selection);
            assert!(
                report.failures.is_empty(),
                "{clause}: {:?}",
                report.failures
            );
            report.results.len()
        };
        // ex:p is a term, but no graph is named so: leaving it out leaves the data as it is.
        let any = Selection::of(GraphSelector::Any);
        let p = id(&snapshot, "http://example.com/p");
        for selection in [any, any.excluding(p), DEFAULT] {
            assert_eq!(results(selection), 1, "{clause}, {selection:?}");
        }
    }
}

/// Pre-binding puts `$currentShape` and `$shapesGraph` in throughout the query (SHACL
/// Appendix A: every basic graph pattern is joined with the pre-bound values), subqueries
/// included, though a subquery needn't project them (§5.6.2): unprojected, they are still
/// bound inside it (research round 12, R12-2).
#[test]
fn current_shape_and_shapes_graph_are_bound_in_subqueries() {
    for query in [
        "SELECT $this WHERE { { SELECT $this WHERE {
           $this <http://example.com/p> ?v FILTER(sameTerm($currentShape, <http://example.com/S>)) } } }",
        "SELECT $this WHERE { { SELECT $this WHERE {
           $this <http://example.com/p> ?v GRAPH $shapesGraph { $currentShape a <http://www.w3.org/ns/shacl#NodeShape> } } } }",
    ] {
        let engine = engine(
            "ex:a ex:p ex:b .",
            &format!("ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''{query}''' ] ."),
        );
        let snapshot = engine.snapshot();
        let shapes = compiled(&snapshot);
        let report = validate(&snapshot, &shapes, DEFAULT);
        assert!(report.failures.is_empty(), "{query}: {:?}", report.failures);
        assert_eq!(report.results.len(), 1, "{query}");
    }
}

/// SHACL §6.2: a SPARQL-based constraint component constrains a shape once per combination of
/// the shape's values for its parameters. Two values of ex:p1 and three of ex:p2 are six
/// constraints; the ASK fails where p1 < p2: (1 2), (1 3) and (2 3), so the focus node has
/// three results. Pairing values by position, or stopping at the shorter list, finds fewer.
#[test]
fn a_component_constrains_once_per_combination_of_its_parameter_values() {
    let engine = engine(
        "ex:a ex:q ex:b .",
        "ex:C a sh:ConstraintComponent ;
           sh:parameter [ sh:path ex:p1 ] , [ sh:path ex:p2 ] ;
           sh:validator [ a sh:SPARQLAskValidator ; sh:ask '''ASK { FILTER ($p1 >= $p2) }''' ] .
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; ex:p1 1 , 2 ; ex:p2 1 , 2 , 3 .",
    );
    let snapshot = engine.snapshot();
    let report = validate(&snapshot, &compiled(&snapshot), DEFAULT);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.results.len(), 3, "{:?}", report.results);
}

/// Shapes with a constraint component of `parameters` parameters, `values` values each on
/// shape ex:S (targeting ex:a), an ASK that every combination fails, and a message of
/// `message` bytes on ex:S: `values`^`parameters` results, each with that message.
fn failing_component(parameters: usize, values: usize, message: usize) -> String {
    let declared: Vec<String> = (0..parameters)
        .map(|p| format!("[ sh:path ex:p{p} ]"))
        .collect();
    let given: Vec<String> = (0..parameters)
        .map(|p| {
            let values: Vec<String> = (0..values).map(|v| v.to_string()).collect();
            format!("ex:p{p} {}", values.join(" , "))
        })
        .collect();
    format!(
        "ex:C a sh:ConstraintComponent ; sh:parameter {} ;
           sh:validator [ a sh:SPARQLAskValidator ; sh:ask '''ASK {{ FILTER(false) }}''' ] .
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:message \"{}\" ; {} .",
        declared.join(" , "),
        "x".repeat(message),
        given.join(" ; ")
    )
}

/// SHCL-AMP r1 R1, guard: with room for them, every combination's result is reported:
/// 8^4 = 4096, each with the shape's 4 KiB message.
#[test]
fn a_component_s_results_are_all_reported_when_affordable() {
    let store = engine("ex:a ex:q ex:b .", &failing_component(4, 8, 4096));
    let snapshot = store.snapshot();
    let report = validate(&snapshot, &compiled(&snapshot), DEFAULT);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    assert_eq!(report.results.len(), 4096);
    assert!(
        report
            .results
            .iter()
            .all(|result| result.messages.len() == 1 && result.messages[0].value().len() == 4096)
    );
}

/// SHCL-AMP r1 R1: the decoded report is charged before it is built. 16^2 = 256 results are
/// a few hundred bytes each as ids, but each decodes the shape's 16 KiB message: 4 MiB of
/// message text. A 1 MiB envelope holds the results (every combination runs: 256 queries)
/// and refuses their report.
#[test]
fn a_report_past_the_envelope_is_refused() {
    let store = engine("ex:a ex:q ex:b .", &failing_component(2, 16, 16 << 10));
    let snapshot = store.snapshot();
    // A view that never cancels: it counts the queries.
    let view = CancelAfter {
        inner: &snapshot,
        token: CancellationToken::new(),
        evaluations: std::sync::atomic::AtomicUsize::new(0),
        at: usize::MAX,
        in_report: false,
    };
    let resources = Resources {
        memory: nrese_exec::Envelope::new(Some(1 << 20), None),
        ..Resources::default()
    };
    let outcome = nrese_shacl::validate(&view, &compiled(&snapshot), DEFAULT, &resources);
    assert_eq!(
        view.evaluations.load(std::sync::atomic::Ordering::SeqCst),
        256,
        "the walk ran every combination before the refusal"
    );
    assert!(
        matches!(outcome, Err(QueryEvaluationError::MemoryLimit(_))),
        "{:?}",
        outcome.map(|report| report.results.len())
    );
}

/// SHCL-AMP r1 R1: a validation's results and their decoded messages are charged to its
/// envelope. 8^4 results, each decoding a 4 KiB message, are 16 MiB of message text alone;
/// a 1 MiB envelope refuses them: the outcome is that refusal, with no report.
#[test]
fn a_component_s_results_past_the_envelope_are_refused() {
    let store = engine("ex:a ex:q ex:b .", &failing_component(4, 8, 4096));
    let snapshot = store.snapshot();
    let resources = Resources {
        memory: nrese_exec::Envelope::new(Some(1 << 20), None),
        ..Resources::default()
    };
    let outcome = nrese_shacl::validate(&snapshot, &compiled(&snapshot), DEFAULT, &resources);
    assert!(
        matches!(outcome, Err(QueryEvaluationError::MemoryLimit(_))),
        "{:?}",
        outcome.map(|report| report.results.len())
    );
}

/// SHCL-AMP R1 sol r1 S3, guard (fails on 707e24dd): what a change's validation finds in the
/// state before is charged per result found and returned per result found, though the set
/// it is compared in keeps one of each. Sixteen shapes each fail all 16^2 combinations of a
/// component with the same result (a result names no combination); an unrelated change
/// introduces nothing, and a 1 MiB envelope affords it, one shape at a time.
#[test]
fn repeated_results_of_a_change_s_validation_are_returned() {
    let values: Vec<String> = (0..16).map(|v| v.to_string()).collect();
    let values = values.join(" , ");
    let shapes: Vec<String> = (0..16)
        .map(|s| {
            format!(
                "ex:S{s} a sh:NodeShape ; sh:targetNode ex:a ; ex:p0 {values} ; ex:p1 {values} ."
            )
        })
        .collect();
    let store = engine(
        "ex:a ex:q ex:b .",
        &format!(
            "ex:C a sh:ConstraintComponent ; sh:parameter [ sh:path ex:p0 ] , [ sh:path ex:p1 ] ;
               sh:validator [ a sh:SPARQLAskValidator ; sh:ask '''ASK {{ FILTER(false) }}''' ] .
             {}",
            shapes.join("\n")
        ),
    );
    let before = store.snapshot();
    let compiled_before = compiled(&before);
    let mut tx = store.transaction();
    tx.insert(QuadRef::new(
        NamedNodeRef::new_unchecked("http://example.com/z"),
        NamedNodeRef::new_unchecked("http://example.com/q"),
        NamedNodeRef::new_unchecked("http://example.com/w"),
        GraphNameRef::DefaultGraph,
    ));
    let changed: Vec<_> = tx.inserted().collect();
    let after = tx.pending_snapshot();
    let resources = Resources {
        memory: nrese_exec::Envelope::new(Some(1 << 20), None),
        ..Resources::default()
    };
    let introduced = nrese_shacl::validate_changes(
        &before,
        &after,
        &compiled_before,
        DEFAULT,
        &changed,
        &resources,
    );
    assert!(
        matches!(&introduced, Ok(report) if report.results.is_empty()),
        "{:?}",
        introduced.map(|report| report.results.len())
    );
}

/// A view of `inner` that cancels `token` at its `at`-th query evaluation, or, with
/// `in_report`, at the first term it decodes after that evaluation (by then the walk has
/// run every query, so the decode is the report's).
struct CancelAfter<'v> {
    inner: &'v Snapshot,
    token: CancellationToken,
    evaluations: std::sync::atomic::AtomicUsize,
    at: usize,
    in_report: bool,
}

impl<'v> ReadView for CancelAfter<'v> {
    fn quads_for_pattern_in<'a>(
        &'a self,
        model: ReadModel,
        pattern: &nrese_engine::QuadPattern,
    ) -> impl Iterator<Item = nrese_engine::EncodedQuad> + use<'a, 'v> {
        self.inner.quads_for_pattern_in(model, pattern)
    }

    fn named_graphs<'a>(&'a self) -> impl Iterator<Item = TermId> + use<'a, 'v> {
        ReadView::named_graphs(self.inner)
    }

    fn contains_named_graph(&self, graph: TermId) -> bool {
        ReadView::contains_named_graph(self.inner, graph)
    }

    fn lookup(&self, term: TermRef<'_>) -> Option<TermId> {
        ReadView::lookup(self.inner, term)
    }

    fn decode(&self, id: TermId) -> Option<nrese_rdf::Term> {
        if self.in_report && self.evaluations.load(std::sync::atomic::Ordering::SeqCst) >= self.at {
            self.token.cancel();
        }
        ReadView::decode(self.inner, id)
    }

    fn decode_quad(&self, quad: nrese_engine::EncodedQuad) -> Option<nrese_rdf::Quad> {
        ReadView::decode_quad(self.inner, quad)
    }

    fn snapshot(&self) -> Option<&Snapshot> {
        Some(self.inner)
    }

    fn evaluation_snapshot(&self) -> std::borrow::Cow<'_, Snapshot> {
        let n = self
            .evaluations
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if !self.in_report && n == self.at {
            self.token.cancel();
        }
        std::borrow::Cow::Borrowed(self.inner)
    }
}

/// Validates `snapshot` against its shapes through a [`CancelAfter`] view: the outcome and
/// the number of queries evaluated.
fn cancelled_after(
    snapshot: &Snapshot,
    at: usize,
    in_report: bool,
) -> (Result<usize, QueryEvaluationError>, usize) {
    let token = CancellationToken::new();
    let view = CancelAfter {
        inner: snapshot,
        token: token.clone(),
        evaluations: std::sync::atomic::AtomicUsize::new(0),
        at,
        in_report,
    };
    let resources = Resources {
        cancellation: Some(token),
        ..Resources::default()
    };
    let outcome = nrese_shacl::validate(&view, &compiled(snapshot), DEFAULT, &resources)
        .map(|report| report.results.len());
    let evaluations = view.evaluations.load(std::sync::atomic::Ordering::SeqCst);
    (outcome, evaluations)
}

/// SHCL-AMP r1 R1, guard: a cancellation after some combinations ran stops the walk: of 4^2
/// = 16 combinations, the cancellation at the 5th query leaves the 7th unrun.
#[test]
fn a_cancellation_between_combinations_stops_the_walk() {
    let store = engine("ex:a ex:q ex:b .", &failing_component(2, 4, 16));
    let snapshot = store.snapshot();
    let (outcome, evaluations) = cancelled_after(&snapshot, 5, false);
    assert!(
        matches!(outcome, Err(QueryEvaluationError::Cancelled)),
        "{outcome:?}"
    );
    assert!(evaluations <= 6, "{evaluations} queries ran");
}

/// SHCL-AMP r1 R1: a cancellation while the report is decoded, after all 4^2 = 16
/// combinations ran, is the outcome: no report.
#[test]
fn a_cancellation_while_the_report_is_decoded_stops_it() {
    let store = engine("ex:a ex:q ex:b .", &failing_component(2, 4, 16));
    let snapshot = store.snapshot();
    let (outcome, evaluations) = cancelled_after(&snapshot, 16, true);
    assert_eq!(evaluations, 16, "every combination ran first");
    assert!(
        matches!(outcome, Err(QueryEvaluationError::Cancelled)),
        "{outcome:?}"
    );
}

/// Shapes with a component whose SELECT node validator reports `?failure` once for each of
/// `values` parameter values on ex:S (targeting ex:a), and, with `named`, a property shape
/// that ex:a fails (no ex:name): `values` failures, each the same 85-byte message.
fn failure_component(values: usize, named: bool) -> String {
    let values: Vec<String> = (0..values).map(|v| v.to_string()).collect();
    format!(
        "ex:C a sh:ConstraintComponent ; sh:parameter [ sh:path ex:p ] ;
           sh:nodeValidator [ a sh:SPARQLSelectValidator ;
             sh:select '''SELECT $this ?failure WHERE {{ BIND(true AS ?failure) }}''' ] .
         ex:S a sh:NodeShape ; sh:targetNode ex:a ; ex:p {} {} .",
        values.join(" , "),
        if named {
            "; sh:property [ sh:path ex:name ; sh:minCount 1 ]"
        } else {
            ""
        }
    )
}

/// SHCL-AMP r2 F1: the failures a validation keeps are charged once, and the report takes
/// them over with their charge. 8192 failures of 85 bytes are charged 133 bytes each kept:
/// 1,089,536 bytes, which a 1.5 MiB envelope affords once (not twice). The report is
/// returned, with every failure.
#[test]
fn a_failure_report_that_fits_once_is_returned() {
    let store = engine("ex:b ex:q ex:c .", &failure_component(8192, false));
    let snapshot = store.snapshot();
    let resources = Resources {
        memory: nrese_exec::Envelope::new(Some(3 << 19), None),
        ..Resources::default()
    };
    let outcome = nrese_shacl::validate(&snapshot, &compiled(&snapshot), DEFAULT, &resources)
        .map(|report| (report.results.len(), report.failures.len()));
    assert!(matches!(outcome, Ok((0, 8192))), "{outcome:?}");
}

/// SHCL-AMP r2 F1, incremental: a change's validation keeps the failures of the state after
/// (charged once, taken over by the report) and drops those of the state before with their
/// charge. 4096 failures in each state (544,768 bytes each): both states fit a 1.25 MiB
/// envelope while they are validated; the state after's, charged a second time beside the
/// state before's, would not.
#[test]
fn a_change_s_failures_are_charged_once_and_the_earlier_ones_returned() {
    let store = engine("ex:b ex:q ex:c .", &failure_component(4096, true));
    let before = store.snapshot();
    let compiled_before = compiled(&before);
    let mut tx = store.transaction();
    tx.insert(QuadRef::new(
        NamedNodeRef::new_unchecked("http://example.com/z"),
        NamedNodeRef::new_unchecked("http://example.com/q"),
        NamedNodeRef::new_unchecked("http://example.com/w"),
        GraphNameRef::DefaultGraph,
    ));
    let changed: Vec<_> = tx.inserted().collect();
    let after = tx.pending_snapshot();
    let resources = Resources {
        memory: nrese_exec::Envelope::new(Some(5 << 18), None),
        ..Resources::default()
    };
    let outcome = nrese_shacl::validate_changes(
        &before,
        &after,
        &compiled_before,
        DEFAULT,
        &changed,
        &resources,
    )
    .map(|report| (report.results.len(), report.failures.len()));
    assert!(matches!(outcome, Ok((0, 4096))), "{outcome:?}");
}

/// SHCL-AMP r2: whatever ends a validation, its charges end with it. The request's envelope
/// draws on a server budget; after a refusal, after a cancellation while the report was
/// decoded, and once a returned report is dropped, the server budget holds nothing.
#[test]
fn a_validation_s_charges_end_with_it() {
    let server = nrese_exec::SharedBudget::new(64 << 20);
    let envelope = || nrese_exec::Envelope::new(Some(1 << 20), Some(server.clone()));
    // Refused.
    let store = engine("ex:a ex:q ex:b .", &failing_component(4, 8, 4096));
    let snapshot = store.snapshot();
    let resources = Resources {
        memory: envelope(),
        ..Resources::default()
    };
    let refused = nrese_shacl::validate(&snapshot, &compiled(&snapshot), DEFAULT, &resources);
    assert!(matches!(refused, Err(QueryEvaluationError::MemoryLimit(_))));
    assert_eq!(server.used(), 0, "after a refusal");
    // Cancelled while the report was decoded.
    let store = engine("ex:a ex:q ex:b .", &failing_component(2, 4, 16));
    let snapshot = store.snapshot();
    let token = CancellationToken::new();
    let view = CancelAfter {
        inner: &snapshot,
        token: token.clone(),
        evaluations: std::sync::atomic::AtomicUsize::new(0),
        at: 16,
        in_report: true,
    };
    let resources = Resources {
        cancellation: Some(token),
        memory: envelope(),
        ..Resources::default()
    };
    let cancelled = nrese_shacl::validate(&view, &compiled(&snapshot), DEFAULT, &resources);
    assert!(matches!(cancelled, Err(QueryEvaluationError::Cancelled)));
    assert_eq!(server.used(), 0, "after a cancellation");
    // Returned, then dropped.
    let resources = Resources {
        memory: envelope(),
        ..Resources::default()
    };
    let report = nrese_shacl::validate(&snapshot, &compiled(&snapshot), DEFAULT, &resources)
        .expect("affordable");
    assert_eq!(report.results.len(), 16);
    assert!(server.used() > 0, "a report holds its charge");
    drop(report);
    assert_eq!(server.used(), 0, "after the report is dropped");
}

/// RSH-07: a validation runs on its caller's resources, its SHACL-SPARQL queries included,
/// nested shapes' too. When they stop it (the cancellation fired, or the envelope refused
/// a charge), the outcome is that stop: not a report of what was found before it, and not a
/// "failure" of the constraint whose query was stopped (a commit gate would pass a warning).
#[test]
fn a_stopped_validation_returns_the_stop_not_a_report() {
    let cancelled = {
        let token = CancellationToken::new();
        token.cancel();
        Resources {
            cancellation: Some(token),
            ..Resources::default()
        }
    };
    // Forty subjects of ex:p: the query's three-way product is 64,000 rows, past 64 KiB.
    let small = Resources {
        memory: nrese_exec::Envelope::new(Some(64 * 1024), None),
        ..Resources::default()
    };
    let data: String = (0..40).map(|i| format!("ex:s{i} ex:p ex:o . ")).collect();
    let data = format!("{data} ex:a a ex:Person .");
    let wide = "SELECT $this ?value ?b ?c WHERE { ?value <http://example.com/p> ?x .
        ?b <http://example.com/p> ?y . ?c <http://example.com/p> ?z }";
    let named = "ex:S a sh:NodeShape ; sh:targetClass ex:Person ;
        sh:property [ sh:path ex:name ; sh:minCount 1 ] .";
    let rows = [
        ("a Core shape, cancelled", named.to_owned(), &cancelled),
        (
            "sh:sparql past the envelope",
            format!(
                "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''{wide}''' ] ."
            ),
            &small,
        ),
        (
            "sh:sparql of a nested shape past the envelope",
            format!(
                "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:node ex:N .
                 ex:N a sh:NodeShape ; sh:sparql [ sh:select '''{wide}''' ] ."
            ),
            &small,
        ),
    ];
    for (row, shapes, resources) in rows {
        let engine = engine(&data, &shapes);
        let snapshot = engine.snapshot();
        let outcome = nrese_shacl::validate(&snapshot, &compiled(&snapshot), DEFAULT, resources);
        let stopped = match &outcome {
            Err(QueryEvaluationError::Cancelled) => resources.cancellation.is_some(),
            Err(QueryEvaluationError::MemoryLimit(_)) => resources.cancellation.is_none(),
            _ => false,
        };
        assert!(stopped, "{row}: {outcome:?}");
    }

    // What a change introduces, the same: ex:b is a person without a name.
    let engine = engine(&data, named);
    let before = engine.snapshot();
    let shapes = compiled(&before);
    let mut tx = engine.transaction();
    tx.insert(QuadRef::new(
        NamedNodeRef::new_unchecked("http://example.com/b"),
        NamedNodeRef::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        NamedNodeRef::new_unchecked("http://example.com/Person"),
        GraphNameRef::DefaultGraph,
    ));
    let changed: Vec<_> = tx.inserted().collect();
    let after = tx.pending_snapshot();
    let outcome =
        nrese_shacl::validate_changes(&before, &after, &shapes, DEFAULT, &changed, &cancelled);
    assert!(
        matches!(outcome, Err(QueryEvaluationError::Cancelled)),
        "validate_changes: {outcome:?}"
    );
}

/// SHACL §1.5: a shapes graph with `sh:entailment E`, where the processor doesn't support E,
/// makes the processor signal a failure: the report is no verdict (SYN-56, B-1401). The data
/// conforms to the shape, so only the failure can make the report not conform.
#[test]
fn an_unsupported_entailment_is_a_failure() {
    let engine = engine(
        "ex:a ex:name \"A\" .",
        "ex:S a sh:NodeShape ; sh:targetNode ex:a ;
           sh:property [ sh:path ex:name ; sh:minCount 1 ] .
         ex:S sh:entailment <http://www.w3.org/ns/entailment/RDFS> .",
    );
    let snapshot = engine.snapshot();
    let report = validate(&snapshot, &compiled(&snapshot), DEFAULT);
    assert!(report.results.is_empty(), "{:?}", report.results);
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.contains("http://www.w3.org/ns/entailment/RDFS")),
        "{:?}",
        report.failures
    );
    assert!(!report.conforms());
}

/// Whether anything applies (the vacuous-pass flag) follows what can fail (SYN-56, B-1406): a
/// node that fails a shape had something applicable. `sh:not` of a shape without constraints
/// always fails (the empty shape conforms); `sh:or ()` and `sh:xone ( [] [] )` always fail;
/// `sh:or ( [] )` and `sh:xone ( [] )` never do. Focus ex:a has no statements; ex:N needs
/// class ex:C, so ex:a fails it. Each row also checks validation agrees with its expectation.
#[test]
fn applicability_follows_what_can_fail() {
    for (row, constraint, expected) in [
        ("sh:not []", "sh:not [ ]", true),
        ("sh:or ()", "sh:or ( )", true),
        ("sh:xone ( [] [] )", "sh:xone ( [ ] [ ] )", true),
        ("guard: sh:or ( [] )", "sh:or ( [ ] )", false),
        ("guard: sh:xone ( [] )", "sh:xone ( [ ] )", false),
        ("guard: sh:or ( N N )", "sh:or ( ex:N ex:N )", true),
    ] {
        let engine = engine(
            "ex:b ex:p ex:c .",
            &format!(
                "ex:S a sh:NodeShape ; sh:targetNode ex:a ; {constraint} . ex:N sh:class ex:C ."
            ),
        );
        let snapshot = engine.snapshot();
        let shapes = compiled(&snapshot);
        let failed = !validate(&snapshot, &shapes, DEFAULT).conforms();
        assert_eq!(failed, expected, "{row}: validation");
        assert_eq!(
            nrese_shacl::applicable(&snapshot, &shapes, DEFAULT),
            expected,
            "{row}: applicable"
        );
    }
}

/// SYN-56 review r1 F4: whether a shape can fail doesn't depend on which shape is looked at
/// first. ex:B targets ex:a and needs it to conform to ex:A; ex:A needs ex:B back and has
/// `sh:in ()`, which every node fails. So ex:a fails ex:A and ex:B: validation fails, and
/// something applied. ex:A targets an absent class, so it is a root without focus nodes.
/// Roots are visited in term id order: the two declaration orders intern ex:A or ex:B first.
#[test]
fn applicability_of_recursive_shapes_does_not_depend_on_their_order() {
    let a = "ex:A a sh:NodeShape ; sh:targetClass ex:Absent ; sh:node ex:B ; sh:in ( ) .";
    let b = "ex:B a sh:NodeShape ; sh:targetNode ex:a ; sh:node ex:A .";
    for (row, shapes) in [
        ("ex:A first", format!("{a}\n{b}")),
        ("ex:B first", format!("{b}\n{a}")),
    ] {
        let engine = engine("ex:b ex:p ex:c .", &shapes);
        let snapshot = engine.snapshot();
        let shapes = compiled(&snapshot);
        assert!(
            !validate(&snapshot, &shapes, DEFAULT).conforms(),
            "{row}: validation"
        );
        assert!(
            nrese_shacl::applicable(&snapshot, &shapes, DEFAULT),
            "{row}: applicable"
        );
    }
}

/// 1000 distinct `sh:entailment` values, each a failure charged at 192 bytes: 192,000 bytes of
/// diagnostics, past a 64 KiB envelope.
fn many_entailments() -> String {
    (0..1000)
        .map(|i| format!("ex:S sh:entailment <http://example.com/entailment/regime-{i:04}> .\n"))
        .collect()
}

/// A shapes change in one transaction on `store`: the state before, its shapes, the state
/// after `removed` left the shapes graph, and that state's shapes.
fn shapes_removed(store: &Engine, removed: &str) -> (Snapshot, Shapes, Snapshot, Shapes) {
    let before = store.snapshot();
    let shapes_before = compiled(&before);
    let selection = shapes_selection(&before);
    let mut tx = store.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(format!("{PREFIXES}{removed}").as_bytes())
    {
        let quad = quad.expect("turtle");
        tx.remove(QuadRef::new(
            &quad.subject,
            &quad.predicate,
            &quad.object,
            GraphNameRef::from(NamedNodeRef::new_unchecked(SHAPES_GRAPH)),
        ));
    }
    let after = tx.pending_snapshot();
    let shapes_after = compile(&after, selection).expect("shapes");
    (before, shapes_before, after, shapes_after)
}

fn small_envelope() -> Resources {
    Resources {
        memory: nrese_exec::Envelope::new(Some(64 * 1024), None),
        ..Resources::default()
    }
}

/// SYN-56 review r1 F3: the state before a shapes change is validated only to tell which of
/// the state after's results are new; its failures are no part of the outcome. Removing a
/// shapes graph that asks for 1000 entailment regimes (diagnostics past a 64 KiB envelope)
/// leaves nothing to validate: the change is accepted with an empty report.
#[test]
fn removing_shapes_with_many_entailments_is_not_refused_for_their_diagnostics() {
    let shapes = many_entailments();
    let store = engine("ex:a ex:p ex:b .", &shapes);
    let (before, shapes_before, after, shapes_after) = shapes_removed(&store, &shapes);
    let outcome = nrese_shacl::validate_shapes_change(
        &before,
        Some(&shapes_before),
        &after,
        &shapes_after,
        DEFAULT,
        &small_envelope(),
    )
    .map(|report| (report.results.len(), report.failures.len()));
    assert!(matches!(outcome, Ok((0, 0))), "{outcome:?}");
}

/// SYN-56 review r1 F3: as above, but the state after keeps a shape whose result (ex:a has no
/// ex:name) the state before had too, so the state before is validated; its 1000 entailment
/// failures are still no part of the outcome, nor charged: nothing is introduced.
#[test]
fn a_shapes_change_compares_results_without_the_state_before_s_diagnostics() {
    let entailments = many_entailments();
    let store = engine(
        "ex:a ex:p ex:b .",
        &format!(
            "ex:S a sh:NodeShape ; sh:targetNode ex:a ;
               sh:property [ sh:path ex:name ; sh:minCount 1 ] .
             {entailments}"
        ),
    );
    let (before, shapes_before, after, shapes_after) = shapes_removed(&store, &entailments);
    let outcome = nrese_shacl::validate_shapes_change(
        &before,
        Some(&shapes_before),
        &after,
        &shapes_after,
        DEFAULT,
        &small_envelope(),
    )
    .map(|report| (report.results.len(), report.failures.len()));
    assert!(matches!(outcome, Ok((0, 0))), "{outcome:?}");
}

/// SHACL §5.3.2: a SELECT constraint's result has `sh:value` the solution's `?value` and
/// `sh:resultPath` its `?path` if that is an IRI, whatever the query computed them from: a
/// term no statement uses is still the result's term (SYN-56, B-1402).
#[test]
fn a_constraint_reports_the_terms_its_query_computed() {
    let select = |projection: &str| {
        format!(
            "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''
               SELECT $this {projection} WHERE {{ $this <http://example.com/p> ?x }}''' ] ."
        )
    };
    // The value: a literal no statement holds.
    let computed_value = engine(
        "ex:a ex:p ex:b .",
        &select("(CONCAT(\"new\", \"value\") AS ?value)"),
    );
    let snapshot = computed_value.snapshot();
    let report = validate(&snapshot, &compiled(&snapshot), DEFAULT);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let values: Vec<_> = report.results.iter().map(|r| r.value.clone()).collect();
    assert_eq!(
        values,
        [Some(
            nrese_rdf::Literal::new_simple_literal("newvalue").into()
        )],
        "computed value"
    );
    // The path: an IRI no statement uses.
    let computed_path = engine(
        "ex:a ex:p ex:b .",
        &select("(IRI(\"http://example.com/computed\") AS ?path)"),
    );
    let snapshot = computed_path.snapshot();
    let report = validate(&snapshot, &compiled(&snapshot), DEFAULT);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let paths: Vec<_> = report.results.iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        paths,
        [Some(PropertyPath::Predicate(NamedNode::new_unchecked(
            "http://example.com/computed"
        )))],
        "computed path"
    );
}

/// The change of one transaction on `store`: `insert` (N-Triples-like turtle) into `graph`
/// (`None`: the default graph); the state before, the change and the state after.
fn inserting(
    store: &Engine,
    insert: &str,
    graph: Option<&str>,
) -> (Snapshot, Vec<nrese_engine::EncodedQuad>, Snapshot) {
    let before = store.snapshot();
    let graph = graph.map(NamedNodeRef::new_unchecked);
    let mut tx = store.transaction();
    for quad in RdfParser::from_format(RdfFormat::Turtle)
        .for_slice(format!("{PREFIXES}{insert}").as_bytes())
    {
        let quad = quad.expect("turtle");
        tx.insert(QuadRef::new(
            &quad.subject,
            &quad.predicate,
            &quad.object,
            graph.map_or(GraphNameRef::DefaultGraph, GraphNameRef::from),
        ));
    }
    let changed: Vec<_> = tx.inserted().collect();
    let after = tx.pending_snapshot();
    (before, changed, after)
}

/// SYN-56 review r1 F5: a term a query computed is compared as the RDF term it is, whether or
/// not the dictionary held it: a change that stores the very term (as an unrelated statement's
/// predicate or object) leaves the result as it was, so nothing is introduced, on both routes:
/// a data change (`validate_changes`) and a change of the shapes graph
/// (`validate_shapes_change`).
#[test]
fn a_computed_term_the_change_stores_is_the_same_result() {
    let rows = [
        (
            "path",
            "(IRI(CONCAT(\"urn:computed-\", \"path\")) AS ?path)",
            "ex:z <urn:computed-path> ex:w .",
        ),
        (
            "value",
            "(CONCAT(\"computed-\", \"value\") AS ?value)",
            "ex:z ex:q \"computed-value\" .",
        ),
    ];
    for (row, projection, stored) in rows {
        let shapes = format!(
            "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''
               SELECT $this {projection} WHERE {{ }}''' ] ."
        );
        // A data change.
        let store = engine("ex:b ex:p ex:c .", &shapes);
        let (before, changed, after) = inserting(&store, stored, None);
        let compiled_before = compiled(&before);
        let introduced = nrese_shacl::validate_changes(
            &before,
            &after,
            &compiled_before,
            DEFAULT,
            &changed,
            &Resources::default(),
        )
        .expect("nothing stops it");
        assert!(
            introduced.results.is_empty(),
            "{row}, data change: {:?}",
            introduced.results
        );
        // A change of the shapes graph that stores the term.
        let store = engine("ex:b ex:p ex:c .", &shapes);
        let (before, _, after) = inserting(&store, stored, Some(SHAPES_GRAPH));
        let shapes_before = compiled(&before);
        let shapes_after = compile(&after, shapes_selection(&before)).expect("shapes");
        let introduced = nrese_shacl::validate_shapes_change(
            &before,
            Some(&shapes_before),
            &after,
            &shapes_after,
            DEFAULT,
            &Resources::default(),
        )
        .expect("nothing stops it");
        assert!(
            introduced.results.is_empty(),
            "{row}, shapes change: {:?}",
            introduced.results
        );
    }
}

/// SYN-56 Part B sol r1 S1: a path is compared as the IRI it is, whether a query computed it
/// or the shape names it. Before: property shape ex:S has path ex:p, and its constraint ex:Q
/// computes `<urn:x>` (no statement uses it) as `?path`. The change gives ex:S the path
/// `<urn:x>` and ex:Q a query that names no path, so the result's path is the shape's: the
/// same IRI, the same result, nothing introduced.
#[test]
fn a_computed_path_that_becomes_the_shape_s_path_is_the_same_result() {
    let shapes = "ex:S a sh:PropertyShape ; sh:targetNode ex:a ; sh:path ex:p ; sh:sparql ex:Q .
        ex:Q sh:select '''SELECT $this (IRI(CONCAT(\"urn:\", \"x\")) AS ?path) WHERE { }''' .";
    let store = engine("ex:b ex:q ex:c .", shapes);
    let before = store.snapshot();
    let shapes_before = compiled(&before);
    let quads = |turtle: &str| -> Vec<nrese_rdf::Quad> {
        RdfParser::from_format(RdfFormat::Turtle)
            .for_slice(format!("{PREFIXES}{turtle}").as_bytes())
            .map(|quad| quad.expect("turtle"))
            .collect()
    };
    let graph = GraphNameRef::from(NamedNodeRef::new_unchecked(SHAPES_GRAPH));
    let mut tx = store.transaction();
    for quad in quads(
        "ex:S sh:path ex:p .
         ex:Q sh:select '''SELECT $this (IRI(CONCAT(\"urn:\", \"x\")) AS ?path) WHERE { }''' .",
    ) {
        tx.remove(QuadRef::new(
            &quad.subject,
            &quad.predicate,
            &quad.object,
            graph,
        ));
    }
    for quad in quads("ex:S sh:path <urn:x> . ex:Q sh:select '''SELECT $this WHERE { }''' .") {
        tx.insert(QuadRef::new(
            &quad.subject,
            &quad.predicate,
            &quad.object,
            graph,
        ));
    }
    let after = tx.pending_snapshot();
    let shapes_after = compile(&after, shapes_selection(&before)).expect("shapes");
    let later = validate(&after, &shapes_after, DEFAULT);
    let paths: Vec<_> = later.results.iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        paths,
        [Some(PropertyPath::Predicate(NamedNode::new_unchecked(
            "urn:x"
        )))],
        "the state after's result has the shape's path"
    );
    let introduced = nrese_shacl::validate_shapes_change(
        &before,
        Some(&shapes_before),
        &after,
        &shapes_after,
        DEFAULT,
        &Resources::default(),
    )
    .expect("nothing stops it");
    assert!(
        introduced.results.is_empty(),
        "the same path, the same result: {:?}",
        introduced.results
    );
}

/// SYN-56 Part B sol r2 S3: a stored triple term holding a blank node is the same value
/// whether the query binds it or the focus node stands for it. ex:S targets the objects of
/// ex:p, the stored `<<( _:b ex:q ex:o )>>`; while a flag holds, its query binds `?value` to
/// that same triple (read from the data: a triple-term `$this` isn't substituted into the
/// query, so `BIND($this …)` inside the OPTIONAL wouldn't see it), and else binds nothing,
/// so the value is the focus node either way. Deleting the flag changes no result: nothing
/// is introduced.
#[test]
fn a_stored_triple_term_is_the_same_value_bound_or_not() {
    let shapes = "ex:S a sh:NodeShape ; sh:targetObjectsOf ex:p ; sh:sparql [ sh:select '''
        SELECT $this ?value WHERE {
          OPTIONAL { <urn:flag> <urn:on> <urn:yes> . ?x <http://example.com/p> ?value }
        }''' ] .";
    let store = engine(
        "ex:s ex:p <<( _:b ex:q ex:o )>> . <urn:flag> <urn:on> <urn:yes> .",
        shapes,
    );
    let before = store.snapshot();
    let compiled_before = compiled(&before);
    let mut tx = store.transaction();
    tx.remove(QuadRef::new(
        NamedNodeRef::new_unchecked("urn:flag"),
        NamedNodeRef::new_unchecked("urn:on"),
        NamedNodeRef::new_unchecked("urn:yes"),
        GraphNameRef::DefaultGraph,
    ));
    let changed: Vec<_> = tx.deleted().collect();
    assert_eq!(changed.len(), 1, "the flag is removed");
    let after = tx.pending_snapshot();
    // Both states have the one result, the triple term its value: bound, then the focus node.
    for (state, report) in [
        ("before", validate(&before, &compiled_before, DEFAULT)),
        ("after", validate(&after, &compiled_before, DEFAULT)),
    ] {
        assert!(report.failures.is_empty(), "{state}: {:?}", report.failures);
        let values: Vec<_> = report.results.iter().map(|r| r.value.clone()).collect();
        assert!(
            matches!(values.as_slice(), [Some(nrese_rdf::Term::Triple(_))]),
            "{state}: {values:?}"
        );
    }
    let introduced = nrese_shacl::validate_changes(
        &before,
        &after,
        &compiled_before,
        DEFAULT,
        &changed,
        &Resources::default(),
    )
    .expect("nothing stops it");
    assert!(
        introduced.results.is_empty(),
        "the same value, the same result: {:?}",
        introduced.results
    );
}

/// The same across a change of the shapes (sol r2 S3): a constraint component's property
/// validator, a SELECT reporting each value of the path, becomes an ASK that every value
/// fails. ex:s's one value, the stored `<<( _:b ex:q ex:o )>>`, is reported by both: the same
/// result, nothing introduced.
#[test]
fn a_stored_triple_term_is_the_same_value_from_select_or_ask() {
    let shapes = "ex:C a sh:ConstraintComponent ; sh:parameter [ sh:path ex:on ] ;
          sh:propertyValidator ex:V .
        ex:V sh:select '''SELECT $this ?value WHERE { $this $PATH ?value }''' .
        ex:P a sh:PropertyShape ; sh:targetNode ex:s ; sh:path ex:p ; ex:on true .";
    let store = engine("ex:s ex:p <<( _:b ex:q ex:o )>> .", shapes);
    let before = store.snapshot();
    let shapes_before = compiled(&before);
    let graph = GraphNameRef::from(NamedNodeRef::new_unchecked(SHAPES_GRAPH));
    let validator = NamedNodeRef::new_unchecked("http://example.com/V");
    let mut tx = store.transaction();
    tx.remove(QuadRef::new(
        validator,
        NamedNodeRef::new_unchecked("http://www.w3.org/ns/shacl#select"),
        nrese_rdf::LiteralRef::new_simple_literal(
            "SELECT $this ?value WHERE { $this $PATH ?value }",
        ),
        graph,
    ));
    tx.insert(QuadRef::new(
        validator,
        NamedNodeRef::new_unchecked("http://www.w3.org/ns/shacl#ask"),
        nrese_rdf::LiteralRef::new_simple_literal("ASK { FILTER(false) }"),
        graph,
    ));
    let after = tx.pending_snapshot();
    let shapes_after = compile(&after, shapes_selection(&before)).expect("shapes");
    for (state, report) in [
        ("SELECT", validate(&before, &shapes_before, DEFAULT)),
        ("ASK", validate(&after, &shapes_after, DEFAULT)),
    ] {
        assert!(report.failures.is_empty(), "{state}: {:?}", report.failures);
        let values: Vec<_> = report.results.iter().map(|r| r.value.clone()).collect();
        assert!(
            matches!(values.as_slice(), [Some(nrese_rdf::Term::Triple(_))]),
            "{state}: {values:?}"
        );
    }
    let introduced = nrese_shacl::validate_shapes_change(
        &before,
        Some(&shapes_before),
        &after,
        &shapes_after,
        DEFAULT,
        &Resources::default(),
    )
    .expect("nothing stops it");
    assert!(
        introduced.results.is_empty(),
        "the same value, the same result: {:?}",
        introduced.results
    );
}

/// A blank node a constraint's query makes (`BNODE() AS ?value`) is the result's value
/// (SHACL §5.3.2), and the same result found again is the same result: a change elsewhere
/// introduces none, though the query makes a new blank node at each evaluation (SYN-56 r1 S3).
#[test]
fn a_blank_node_a_query_made_is_the_same_result_when_found_again() {
    let data = "ex:a ex:p ex:b .";
    let shapes = "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''
        SELECT $this (BNODE() AS ?value) WHERE { $this <http://example.com/p> ?x }''' ] .";
    let store = engine(data, shapes);
    let before = store.snapshot();
    let compiled = compiled(&before);
    let report = validate(&before, &compiled, DEFAULT);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let values: Vec<_> = report.results.iter().map(|r| r.value.clone()).collect();
    assert!(
        matches!(values.as_slice(), [Some(nrese_rdf::Term::BlankNode(_))]),
        "the blank node is the value: {values:?}"
    );
    let (before, changed, after) = inserting(&store, "ex:z ex:q ex:w .", None);
    let introduced = nrese_shacl::validate_changes(
        &before,
        &after,
        &compiled,
        DEFAULT,
        &changed,
        &Resources::default(),
    )
    .expect("nothing stops it");
    assert!(
        introduced.results.is_empty(),
        "an unrelated change introduced {:?}",
        introduced.results
    );
}

/// SYN-56 review r1 F2: one blank node a query made is one node, in every result that names
/// it. The subquery makes one blank node; joined with ex:a's two ex:p values it is the value
/// of two solutions, so two results with one `sh:value` node, in the report and in its RDF
/// graph. Validated again after an unrelated change, nothing is introduced.
#[test]
fn one_blank_node_of_a_query_is_one_node_of_the_report() {
    let shapes = "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''
        SELECT $this ?value WHERE {
          { SELECT $this (BNODE() AS ?value) WHERE { } }
          $this <http://example.com/p> ?x .
        }''' ] .";
    let store = engine("ex:a ex:p ex:x1 , ex:x2 .", shapes);
    let snapshot = store.snapshot();
    let compiled = compiled(&snapshot);
    let report = validate(&snapshot, &compiled, DEFAULT);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let values: Vec<_> = report.results.iter().map(|r| r.value.clone()).collect();
    assert!(
        matches!(
            values.as_slice(),
            [Some(nrese_rdf::Term::BlankNode(a)), Some(nrese_rdf::Term::BlankNode(b))] if a == b
        ),
        "two results, one blank node: {values:?}"
    );
    let value = NamedNode::new_unchecked("http://www.w3.org/ns/shacl#value");
    let nodes: std::collections::BTreeSet<_> = report
        .to_triples()
        .into_iter()
        .filter(|triple| triple.predicate == value)
        .map(|triple| triple.object)
        .collect();
    assert_eq!(nodes.len(), 1, "one sh:value node: {nodes:?}");
    let (before, changed, after) = inserting(&store, "ex:z ex:q ex:w .", None);
    let introduced = nrese_shacl::validate_changes(
        &before,
        &after,
        &compiled,
        DEFAULT,
        &changed,
        &Resources::default(),
    )
    .expect("nothing stops it");
    assert!(
        introduced.results.is_empty(),
        "an unrelated change introduced {:?}",
        introduced.results
    );
}

/// A query's fresh blank nodes compare up to their names, result by result (SYN-56 r2 S3,
/// S4):
/// - a result that is unchanged stays the same when another result of the same constraint
///   goes: removing ex:a's ex:p ex:x1 removes the "newvalue" result and introduces nothing;
/// - one blank node met twice in a value stays one: TRIPLE(?b, ex:p, ?b) has subject = object.
#[test]
fn fresh_blank_nodes_compare_up_to_their_names() {
    let shapes = "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''
        SELECT $this (IF(?x = <http://example.com/x1>, CONCAT(\"new\", \"value\"), BNODE()) AS ?value)
        WHERE { $this <http://example.com/p> ?x } ORDER BY ?x''' ] .";
    let store = engine("ex:a ex:p ex:x1 , ex:x2 .", shapes);
    let before = store.snapshot();
    let compiled_before = compiled(&before);
    let mut tx = store.transaction();
    tx.remove(QuadRef::new(
        NamedNodeRef::new_unchecked("http://example.com/a"),
        NamedNodeRef::new_unchecked("http://example.com/p"),
        NamedNodeRef::new_unchecked("http://example.com/x1"),
        GraphNameRef::DefaultGraph,
    ));
    let changed: Vec<_> = tx.deleted().collect();
    let after = tx.pending_snapshot();
    let introduced = nrese_shacl::validate_changes(
        &before,
        &after,
        &compiled_before,
        DEFAULT,
        &changed,
        &Resources::default(),
    )
    .expect("nothing stops it");
    assert!(
        introduced.results.is_empty(),
        "removing a result introduced {:?}",
        introduced.results
    );

    let shapes = "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''
        SELECT $this (TRIPLE(?b, <http://example.com/p>, ?b) AS ?value)
        WHERE { $this <http://example.com/p> ?x BIND(BNODE() AS ?b) }''' ] .";
    let store = engine("ex:a ex:p ex:x1 .", shapes);
    let snapshot = store.snapshot();
    let report = validate(&snapshot, &compiled(&snapshot), DEFAULT);
    assert!(report.failures.is_empty(), "{:?}", report.failures);
    let same = report.results.iter().all(|result| match &result.value {
        Some(nrese_rdf::Term::Triple(triple)) => match (&triple.subject, &triple.object) {
            (nrese_rdf::NamedOrBlankNode::BlankNode(s), nrese_rdf::Term::BlankNode(o)) => s == o,
            _ => false,
        },
        _ => false,
    });
    assert!(
        same && report.results.len() == 1,
        "one blank node, subject and object: {:?}",
        report.results
    );
}

/// A query's fresh blank nodes are told apart from the data's, whatever their labels (SYN-56
/// r3 S3): with a stored blank node labelled `fresh0`, TRIPLE(fresh, ex:p, stored) and
/// TRIPLE(stored, ex:p, fresh) are different results; removing the first leaves the second
/// as it was, so the change introduces nothing.
#[test]
fn fresh_blank_nodes_are_told_apart_from_stored_ones() {
    let store = engine(
        "",
        "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''
           SELECT $this ?value WHERE {
             { $this <http://example.com/r1> ?s BIND(1 AS ?k)
               BIND(TRIPLE(BNODE(), <http://example.com/p>, ?s) AS ?value) }
             UNION
             { $this <http://example.com/r2> ?s BIND(2 AS ?k)
               BIND(TRIPLE(?s, <http://example.com/p>, BNODE()) AS ?value) }
           } ORDER BY ?k''' ] .",
    );
    let stored = nrese_rdf::BlankNodeRef::new_unchecked("fresh0");
    let quad = |predicate: &'static str| {
        QuadRef::new(
            NamedNodeRef::new_unchecked("http://example.com/a"),
            NamedNodeRef::new_unchecked(predicate),
            stored,
            GraphNameRef::DefaultGraph,
        )
    };
    let mut tx = store.transaction();
    tx.insert(quad("http://example.com/r1"));
    tx.insert(quad("http://example.com/r2"));
    tx.commit().expect("commit");
    let before = store.snapshot();
    let compiled_before = compiled(&before);
    let mut tx = store.transaction();
    tx.remove(quad("http://example.com/r1"));
    let changed: Vec<_> = tx.deleted().collect();
    let after = tx.pending_snapshot();
    let introduced = nrese_shacl::validate_changes(
        &before,
        &after,
        &compiled_before,
        DEFAULT,
        &changed,
        &Resources::default(),
    )
    .expect("nothing stops it");
    assert!(
        introduced.results.is_empty(),
        "removing a result introduced {:?}",
        introduced.results
    );
}

/// A change of the shapes introduces what the data newly violates, even where a result's
/// value is a stored blank node labelled as a fresh one the query made before (SYN-56 r4
/// S3): the query reports a fresh blank node for ex:a, and any named graph holding
/// `ex:z ex:q ex:w` by its name. Read the fresh node's label L; then a graph named `_:L`
/// gets that statement. One result is introduced: its value is the stored `_:L`.
#[test]
fn a_stored_blank_node_labelled_as_a_fresh_one_is_still_introduced() {
    let shapes = "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''
        SELECT $this ?value WHERE {
          { $this <http://example.com/p> ?x BIND(BNODE() AS ?value) }
          UNION
          { GRAPH ?value { <http://example.com/z> <http://example.com/q> <http://example.com/w> } }
        }''' ] .";
    let store = engine("ex:a ex:p ex:b .", shapes);
    let before = store.snapshot();
    let shapes_before = compiled(&before);
    let any = Selection::of(GraphSelector::Any).excluding(id(&before, SHAPES_GRAPH));
    let found = validate(&before, &shapes_before, any);
    let label = match found.results.as_slice() {
        [result] => match &result.value {
            Some(nrese_rdf::Term::BlankNode(node)) => node.as_str().to_owned(),
            other => panic!("a fresh blank node is the value: {other:?}"),
        },
        other => panic!("one result: {other:?}"),
    };
    let mut tx = store.transaction();
    tx.insert(QuadRef::new(
        NamedNodeRef::new_unchecked("http://example.com/z"),
        NamedNodeRef::new_unchecked("http://example.com/q"),
        NamedNodeRef::new_unchecked("http://example.com/w"),
        GraphNameRef::BlankNode(nrese_rdf::BlankNodeRef::new_unchecked(&label)),
    ));
    let after = tx.pending_snapshot();
    let shapes_after = compiled(&after);
    let introduced = nrese_shacl::validate_shapes_change(
        &before,
        Some(&shapes_before),
        &after,
        &shapes_after,
        any,
        &Resources::default(),
    )
    .expect("nothing stops it");
    let values: Vec<_> = introduced.results.iter().map(|r| r.value.clone()).collect();
    assert_eq!(
        values,
        [Some(nrese_rdf::Term::BlankNode(
            nrese_rdf::BlankNode::new_unchecked(label)
        ))],
        "the stored blank node's result is introduced"
    );
}

/// The same, inside a computed triple term (SYN-56 r5 S3): the query reports
/// TRIPLE(fresh, ex:p, ex:o) for ex:a, and TRIPLE(g, ex:p, ex:o) for any named graph g holding
/// `ex:z ex:q ex:w`. Read the fresh subject's label L; a graph named `_:L` then gets that
/// statement. Neither triple is stored, so both values are computed; one result is introduced,
/// the one whose subject is the stored `_:L`.
#[test]
fn a_stored_blank_node_inside_a_computed_triple_is_still_introduced() {
    let shapes = "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:sparql [ sh:select '''
        SELECT $this ?value WHERE {
          { $this <http://example.com/p> ?x
            BIND(TRIPLE(BNODE(), <http://example.com/p>, <http://example.com/o>) AS ?value) }
          UNION
          { GRAPH ?g { <http://example.com/z> <http://example.com/q> <http://example.com/w> }
            BIND(TRIPLE(?g, <http://example.com/p>, <http://example.com/o>) AS ?value) }
        }''' ] .";
    let store = engine("ex:a ex:p ex:b .", shapes);
    let before = store.snapshot();
    let shapes_before = compiled(&before);
    let any = Selection::of(GraphSelector::Any).excluding(id(&before, SHAPES_GRAPH));
    let found = validate(&before, &shapes_before, any);
    let label = match found.results.as_slice() {
        [result] => match &result.value {
            Some(nrese_rdf::Term::Triple(triple)) => match &triple.subject {
                nrese_rdf::NamedOrBlankNode::BlankNode(node) => node.as_str().to_owned(),
                other => panic!("a fresh subject: {other:?}"),
            },
            other => panic!("a triple is the value: {other:?}"),
        },
        other => panic!("one result: {other:?}"),
    };
    let mut tx = store.transaction();
    tx.insert(QuadRef::new(
        NamedNodeRef::new_unchecked("http://example.com/z"),
        NamedNodeRef::new_unchecked("http://example.com/q"),
        NamedNodeRef::new_unchecked("http://example.com/w"),
        GraphNameRef::BlankNode(nrese_rdf::BlankNodeRef::new_unchecked(&label)),
    ));
    let after = tx.pending_snapshot();
    let shapes_after = compiled(&after);
    let introduced = nrese_shacl::validate_shapes_change(
        &before,
        Some(&shapes_before),
        &after,
        &shapes_after,
        any,
        &Resources::default(),
    )
    .expect("nothing stops it");
    let subjects: Vec<String> = introduced
        .results
        .iter()
        .map(|result| match &result.value {
            Some(nrese_rdf::Term::Triple(triple)) => match &triple.subject {
                nrese_rdf::NamedOrBlankNode::BlankNode(node) => node.as_str().to_owned(),
                other => format!("{other:?}"),
            },
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(subjects, [label], "the stored subject's result");
}

/// A change of the shapes compares a result's severity (SYN-56 r5 S6): a property shape that
/// ex:a fails as a Warning, made a Violation, makes its result new, as a Violation.
#[test]
fn a_shapes_change_compares_severity() {
    let iri = NamedNodeRef::new_unchecked;
    let shapes_graph = GraphNameRef::NamedNode(iri(SHAPES_GRAPH));
    let severity = "http://www.w3.org/ns/shacl#severity";
    let violation = "http://www.w3.org/ns/shacl#Violation";
    let store = engine(
        "ex:b ex:q ex:c .",
        "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:property ex:P .
         ex:P a sh:PropertyShape ; sh:path ex:name ; sh:minCount 1 ; sh:severity sh:Warning .",
    );
    let before = store.snapshot();
    let shapes_before = compiled(&before);
    let any = Selection::of(GraphSelector::Any).excluding(id(&before, SHAPES_GRAPH));
    let mut tx = store.transaction();
    let p = iri("http://example.com/P");
    tx.remove(QuadRef::new(
        p,
        iri(severity),
        iri("http://www.w3.org/ns/shacl#Warning"),
        shapes_graph,
    ));
    tx.insert(QuadRef::new(p, iri(severity), iri(violation), shapes_graph));
    let after = tx.pending_snapshot();
    let shapes_after = compiled(&after);
    let introduced = nrese_shacl::validate_shapes_change(
        &before,
        Some(&shapes_before),
        &after,
        &shapes_after,
        any,
        &Resources::default(),
    )
    .expect("nothing stops it");
    let severities: Vec<&str> = introduced
        .results
        .iter()
        .map(|result| result.severity.as_str())
        .collect();
    assert_eq!(severities, [violation], "Warning made Violation");
}

/// A change of the shapes compares the messages in effect (SYN-56 r5 S6): an `sh:sparql`
/// constraint's message, the same as its shape's, removed leaves the result's messages as
/// they were, so nothing is new.
#[test]
fn a_shapes_change_compares_effective_messages() {
    let iri = NamedNodeRef::new_unchecked;
    let shapes_graph = GraphNameRef::NamedNode(iri(SHAPES_GRAPH));
    let store = engine(
        "ex:b ex:q ex:c .",
        "ex:S a sh:NodeShape ; sh:targetNode ex:a ; sh:message \"m\" ; sh:sparql ex:Q .
         ex:Q sh:select '''SELECT $this WHERE { }''' ; sh:message \"m\" .",
    );
    let before = store.snapshot();
    let shapes_before = compiled(&before);
    let any = Selection::of(GraphSelector::Any).excluding(id(&before, SHAPES_GRAPH));
    let mut tx = store.transaction();
    tx.remove(QuadRef::new(
        iri("http://example.com/Q"),
        iri("http://www.w3.org/ns/shacl#message"),
        nrese_rdf::LiteralRef::new_simple_literal("m"),
        shapes_graph,
    ));
    let after = tx.pending_snapshot();
    let shapes_after = compiled(&after);
    let introduced = nrese_shacl::validate_shapes_change(
        &before,
        Some(&shapes_before),
        &after,
        &shapes_after,
        any,
        &Resources::default(),
    )
    .expect("nothing stops it");
    assert!(
        introduced.results.is_empty(),
        "an override equal to the shape's messages, removed: {:?}",
        introduced.results
    );
}

/// Shapes: one property shape whose `subjects` subjects of ex:p each lack ex:required, with
/// `messages` distinct `sh:message` literals: `subjects` results, each naming every message.
fn many_messages(subjects: usize, messages: usize) -> (String, String) {
    let data: String = (0..subjects)
        .map(|i| format!("ex:s{i} ex:p ex:o .\n"))
        .collect();
    let messages: Vec<String> = (0..messages).map(|i| format!("\"m{i}\"")).collect();
    let shapes = format!(
        "ex:S a sh:PropertyShape ; sh:targetSubjectsOf ex:p ; sh:path ex:required ;
           sh:minCount 1 ; sh:message {} .",
        messages.join(" , ")
    );
    (data, shapes)
}

/// SYN-56 review r1 F1: a change of the shapes compares results without copying what every
/// result of a shape shares. 512 results, each with its shape's 256 messages (decoded, about
/// 25 KB per result): an annotation added to the shape changes no result, and the comparison
/// fits a 2 MiB envelope that the raw results of both states fit, though the decoded results
/// (about 13 MB) would not.
#[test]
fn a_shapes_change_compares_results_without_copying_their_messages() {
    let (data, shapes) = many_messages(512, 256);
    let store = engine(&data, &shapes);
    let (before, _, after) = inserting(
        &store,
        "ex:S rdfs:comment \"annotated\" .",
        Some(SHAPES_GRAPH),
    );
    let shapes_before = compiled(&before);
    let shapes_after = compile(&after, shapes_selection(&before)).expect("shapes");
    let resources = Resources {
        memory: nrese_exec::Envelope::new(Some(2 << 20), None),
        ..Resources::default()
    };
    let outcome = nrese_shacl::validate_shapes_change(
        &before,
        Some(&shapes_before),
        &after,
        &shapes_after,
        DEFAULT,
        &resources,
    )
    .map(|report| (report.results.len(), report.failures.len()));
    assert!(matches!(outcome, Ok((0, 0))), "{outcome:?}");
}
