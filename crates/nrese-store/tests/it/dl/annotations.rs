//! Annotations under `owl2-dl`: the OWL 2 Direct Semantics gives them no meaning (Direct
//! Semantics §2, "ignored"), and an annotation is attached to an IRI, not to the individual
//! it names (Structural Specification §10.2.1), so `:x owl:sameAs :y` doesn't make
//! `:y :note "a"` of `:x :note "a"`. The rules' closure holds such copies (`eq-rep-s`,
//! `eq-rep-o`, an annotation axiom's), so a read that may return one is told `unsound`
//! (docs/spec/reasoning-semantics.md, "What may be `complete`"). A read of a store without
//! an equality or an annotation axiom keeps its status.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_sparql::{Completeness, GraphAccess};
use nrese_store::{
    CancellationToken, MutationPipeline, ReadScope, SparqlQueryRequest, StatementOp, StoreConfig,
    StoreService,
};

use super::versions::{in_transaction, ntriples};
use super::{PREFIXES, insert, pipeline, pipeline_of};

/// `q` (with [`PREFIXES`]) on `store` for a reader of every graph: its answer as written
/// (results JSON, or the described graph) and its status.
fn told(store: &StoreService, q: &str) -> (String, Completeness) {
    let request = SparqlQueryRequest::all(format!("{PREFIXES}{q}"));
    let prepared = store.prepare_query(&request).expect("prepared");
    let mut out = Vec::new();
    let status = super::run_dl(store, &prepared, &mut out).expect("query");
    let status = status.expect("a status under owl2-dl").shared;
    (String::from_utf8(out).expect("utf8"), status)
}

/// The store of the read kinds: a declared `:note` and the built-in `rdfs:label` on `:x`,
/// which equals `:y`; `:q`, an object property, links `:v` to `:x` and `:y` to `:w`.
const KINDS: &str = ":note a owl:AnnotationProperty . :x :note \"a\" . :x rdfs:label \"b\" . \
                     :x owl:sameAs :y . :v :q :x . :y :q :w .";

/// [`KINDS`] committed to an `owl2-dl` store, on the rules' route (setup).
fn kinds() -> MutationPipeline {
    let dl = pipeline();
    insert(&dl, KINDS).expect("the data");
    assert_eq!(dl.store().dl_bounds().last, "rules", "setup: off the route");
    dl
}

/// Each `(read, status)` whose status isn't `unsound`, as a line.
fn not_unsound<'a>(told: impl IntoIterator<Item = (&'a str, Completeness)>) -> Vec<String> {
    told.into_iter()
        .filter(|(_, status)| status.as_str() != "unsound")
        .map(|(read, status)| format!("{read}: {}", status.header()))
        .collect()
}

/// Spec (Direct Semantics §2; Structural Specification §10.2.1; decided on 17 Nov):
/// under `owl2-dl`, on the rules' route, every read that may return the rules' copy of
/// an annotation statement is `unsound`: a constant annotation predicate, declared or built in,
/// as `SELECT` and in a path; a variable predicate; a negated property set; `DESCRIBE`; and a
/// pattern under `NOT EXISTS`, where a copy removes a right answer.
#[test]
fn every_kind_of_read_that_may_return_a_copied_annotation_is_unsound() {
    let dl = kinds();
    let (rows, _) = told(dl.store(), "SELECT ?s { ?s :note \"a\" }");
    assert!(
        rows.contains("http://example.com/y"),
        "setup: no copy: {rows}"
    );
    let reads = [
        "SELECT ?s ?o { ?s :note ?o }",
        "SELECT ?s ?o { ?s rdfs:label ?o }",
        "SELECT ?s ?p ?o { ?s ?p ?o }",
        "SELECT ?s ?o { ?s :note+ ?o }",
        "SELECT ?s ?o { ?s (:q|:note) ?o }",
        "SELECT ?s ?o { ?s :q/:note ?o }",
        "SELECT ?s ?o { ?s !(:q) ?o }",
        "DESCRIBE :y",
        "SELECT ?s { ?s :q ?o FILTER NOT EXISTS { ?s :note \"a\" } }",
    ];
    let wrong = not_unsound(reads.map(|q| (q, told(dl.store(), q).1)));
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (docs/spec/reasoning-semantics.md, "What may be `complete`": a predicate of your own may
/// be): on the same store, a read of an object property of its own is `complete`; the rule
/// reaches annotation reads only.
#[test]
fn an_object_property_read_beside_a_copied_annotation_stays_complete() {
    let dl = kinds();
    let (_, status) = told(dl.store(), "SELECT ?s ?o { ?s :q ?o }");
    assert_eq!(status.as_str(), "complete", "{}", status.header());
}

/// Spec (every status under `owl2-dl`, on every path): a constant
/// annotation read is `unsound` off the rules' route too (the bounds, after one axiom outside
/// OWL 2 RL), in EXPLAIN and in a plan, as a read of the closure alone without the bounds, and
/// inside a transaction.
#[test]
fn a_copied_annotation_read_is_unsound_on_every_owl2_dl_path() {
    let q = "SELECT ?s ?o { ?s :note ?o }";
    let dl = kinds();
    let store = dl.store();
    let prepare = |q: &str| {
        let request = SparqlQueryRequest::all(format!("{PREFIXES}{q}"));
        store.prepare_query(&request).expect("prepared")
    };
    let token = CancellationToken::new();
    let implicit = "SELECT ?s ?o FROM <http://www.ontotext.com/implicit> { ?s :note ?o }";
    // One pending addition the read doesn't touch: the read is pending, and nothing deleted.
    let unrelated = StatementOp::Add {
        data: ntriples("<http://example.com/z> <http://example.com/r> <http://example.com/w> ."),
        contexts: Vec::new(),
    };
    let paths = [
        (
            "explain",
            store
                .explain_query(&prepare(q), &token)
                .expect("explained")
                .completeness,
        ),
        (
            "plan",
            store
                .plan_query(&prepare(q), &token)
                .expect("planned")
                .completeness,
        ),
        (
            "the closure alone",
            store.status_without_dl(&prepare(implicit)),
        ),
        (
            "in a transaction",
            in_transaction(&dl, vec![unrelated], q).1,
        ),
    ];
    let mut wrong: Vec<String> = paths
        .into_iter()
        .filter_map(|(path, status)| match status {
            Some(status) if status.as_str() == "unsound" => None,
            other => Some(format!("{path}: {:?}", other.map(|s| s.header()))),
        })
        .collect();
    let off = pipeline();
    let union = ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :x a :A .";
    insert(&off, &format!("{KINDS} {union}")).expect("the data");
    assert_ne!(off.store().dl_bounds().last, "rules", "setup: on the route");
    wrong.extend(not_unsound([("off the route", told(off.store(), q).1)]));
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (docs/spec/reasoning-semantics.md:86, a restricted reader's status says nothing about
/// graphs they can't read): under `owl2-dl` a reader of
/// some graphs is told `not-certified` (the redaction), and the same with `:note`'s
/// declaration in a graph it can't read as without the declaration: status and reasons alike.
#[test]
fn a_reader_who_cant_see_the_declaration_is_told_as_without_it() {
    let reader = ReadScope::Graphs(Arc::new(GraphAccess {
        default_graph: true,
        inferred: true,
        ..GraphAccess::default()
    }));
    let told = |declaration: &str| {
        let dl = pipeline();
        let data = format!("{declaration} :x :note \"a\" . :x owl:sameAs :y .");
        insert(&dl, &data).expect("the data");
        let q = format!("{PREFIXES}SELECT ?s ?o {{ ?s :note ?o }}");
        let request = SparqlQueryRequest::new(q, reader.clone());
        let result = dl.store().execute_query(&request).expect("query");
        let rows = String::from_utf8(result.payload).expect("utf8");
        assert!(
            rows.contains("http://example.com/y"),
            "setup: no copy seen: {rows}"
        );
        result.completeness.expect("a status").header()
    };
    let hidden = told("GRAPH :hidden { :note a owl:AnnotationProperty }");
    assert_eq!(hidden, "not-certified");
    assert_eq!(
        hidden,
        told(""),
        "told otherwise than without the declaration"
    );
}

/// Spec (OWL 2 Structural Specification §2.4: `rdf:type` is reserved vocabulary, no entity;
/// Direct Semantics: a class assertion's copy through an equality is entailed):
/// the reader leaves a declaration of a reserved name out
/// (`leave_out_reserved`), so in the ontology as read `rdf:type` is no annotation property,
/// and nothing derives from the left-out declaration. Beside an equality, `?x a :C` is told
/// as on the same data without `rdf:type a owl:AnnotationProperty`, status and reasons alike.
#[test]
fn rdf_type_declared_an_annotation_property_is_not_read_as_one() {
    let q = "SELECT ?x { ?x a :C }";
    assert_eq!(
        beside_rdf_type(RDF_TYPE_DECLARED, q),
        beside_rdf_type("", q)
    );
}

/// `rdf:type` declared an annotation property.
const RDF_TYPE_DECLARED: &str =
    "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type> a owl:AnnotationProperty .";

/// The header `q` is told on `declaration` beside `:x a :C . :x owl:sameAs :y`, loaded past
/// the pipeline and rebuilt under `owl2-dl`.
fn beside_rdf_type(declaration: &str, q: &str) -> String {
    let dl = pipeline();
    let data = format!("{PREFIXES}{declaration} :x a :C . :x owl:sameAs :y .");
    crate::support::load_past_the_pipeline(dl.store(), &data);
    let program = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl).materialised_program();
    dl.store()
        .rematerialise(program.expect("the rules"))
        .expect("rebuilt");
    told(dl.store(), q).1.header()
}

/// Spec (as [`rdf_type_declared_an_annotation_property_is_not_read_as_one`]: in the ontology
/// as read `rdf:type` is no annotation property): a read of any predicate beside that
/// declaration and an equality is told as on the same data without the declaration, status
/// and reasons alike; no copy of an annotation statement is among its rows.
#[test]
fn rdf_type_declared_an_annotation_property_gives_a_read_of_any_predicate_no_copies() {
    let q = "SELECT ?s ?p ?o { ?s ?p ?o }";
    assert_eq!(
        beside_rdf_type(RDF_TYPE_DECLARED, q),
        beside_rdf_type("", q)
    );
}

/// One case of the grid: the data, the equality (empty: none), and the reads.
struct Case {
    name: String,
    facts: String,
    equality: &'static str,
    /// Each read with its expected status class.
    reads: Vec<(String, &'static str)>,
    /// A term only a copy gives the first read's rows (setup), if any.
    copy: Option<&'static str>,
}

/// The grid's cases for `ann` (declared by `decl`): A, the equality on the subject; B, on
/// the object; each read `unsound`. D, no equality: each read as the page says today, a
/// predicate of the data's own `complete` (`own`), the rest `sound-only`.
fn cases(ann: &str, decl: &str, own: bool) -> [Case; 3] {
    let constant = if own { "complete" } else { "sound-only" };
    let reads = |r: &[(String, &'static str)]| r.to_vec();
    [
        Case {
            name: format!("A {ann}"),
            facts: format!("{decl}:x {ann} \"a\" ."),
            equality: ":x owl:sameAs :y .",
            reads: reads(&[
                (format!("SELECT ?s ?o {{ ?s {ann} ?o }}"), "unsound"),
                ("SELECT ?p ?o { :y ?p ?o }".into(), "unsound"),
                ("DESCRIBE :y".into(), "unsound"),
                (format!("ASK {{ :y {ann} \"a\" }}"), "unsound"),
            ]),
            copy: Some("http://example.com/y"),
        },
        Case {
            name: format!("B {ann}"),
            facts: format!("{decl}:x {ann} :v ."),
            equality: ":v owl:sameAs :w .",
            reads: reads(&[
                (format!("SELECT ?s ?o {{ ?s {ann} ?o }}"), "unsound"),
                ("SELECT ?s ?p { ?s ?p :w }".into(), "unsound"),
                ("DESCRIBE :x".into(), "unsound"),
                (format!("ASK {{ :x {ann} :w }}"), "unsound"),
            ]),
            copy: Some("http://example.com/w"),
        },
        Case {
            name: format!("D {ann}"),
            facts: format!("{decl}:x {ann} \"a\" ."),
            equality: "",
            reads: reads(&[
                (format!("SELECT ?s ?o {{ ?s {ann} ?o }}"), constant),
                ("SELECT ?p ?o { :x ?p ?o }".into(), "sound-only"),
                ("DESCRIBE :x".into(), "sound-only"),
            ]),
            copy: None,
        },
    ]
}

/// Spec (as above): under every
/// equality setting, written in one commit, in two (the equality last) or loaded and rebuilt,
/// a read that may return a copy (A, B; a declared `:note` and the built-in `rdfs:label`) is
/// `unsound`, `compact`'s `ASK` included. A
/// read of the same data without an equality (D) keeps its status: a declared `:note` read
/// `complete`, the rest `sound-only`.
#[test]
fn the_outside_checks_grid_under_every_equality_setting_and_way_of_writing() {
    let program = || {
        ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
            .materialised_program()
            .expect("owl2-dl keeps the OWL 2 RL closure")
    };
    let note = cases(":note", ":note a owl:AnnotationProperty . ", true);
    let label = cases("rdfs:label", "", false);
    let mut wrong = Vec::new();
    for equality in ["representatives", "replicate", "compact"] {
        for case in note.iter().chain(&label) {
            for way in ["one commit", "two commits", "load and rebuild"] {
                let dl = pipeline_of(StoreConfig {
                    equality_by_representatives: equality != "replicate",
                    equality_compact: equality == "compact",
                    ..crate::support::in_memory_store_config()
                });
                match way {
                    "one commit" => {
                        insert(&dl, &format!("{} {}", case.facts, case.equality)).expect("data");
                    }
                    "two commits" => {
                        insert(&dl, &case.facts).expect("the facts");
                        if !case.equality.is_empty() {
                            insert(&dl, case.equality).expect("the equality");
                        }
                    }
                    _ => {
                        let turtle = format!("{PREFIXES}{} {}", case.facts, case.equality);
                        crate::support::load_past_the_pipeline(dl.store(), &turtle);
                        dl.store().rematerialise(program()).expect("rebuilt");
                    }
                }
                let at = format!("{equality} | {} | {way}", case.name);
                for (i, (q, expected)) in case.reads.iter().enumerate() {
                    let (rows, status) = told(dl.store(), q);
                    if let Some(copy) = case.copy.filter(|_| i == 0) {
                        assert!(rows.contains(copy), "setup: {at}: no copy in {q}: {rows}");
                    }
                    if status.as_str() != *expected {
                        wrong.push(format!("{at} | {q}: {}, not {expected}", status.header()));
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
