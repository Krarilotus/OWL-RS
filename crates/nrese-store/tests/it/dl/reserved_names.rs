//! A name of the reserved vocabulary declared an annotation property under `owl2-dl`. The
//! Structural Specification §5.5 lists the nine annotation properties OWL 2 builds in
//! (`rdfs:label` and the others) and says the reserved vocabulary's other IRIs "MUST NOT be used
//! to identify annotation properties"; such a declaration (`owl:sameAs a
//! owl:AnnotationProperty`, `rdf:type a owl:AnnotationProperty`) makes neither one, and the
//! reader still reads `owl:sameAs` as equality and `rdf:type` as typing. Each test holds a
//! store's reading after a history to a fresh read of its statements, given in one commit, as
//! the route `dl_bounds().last` and each read's answers and status.

use super::pipeline;
use super::shapes::{SH, shapes_insert as insert, u1_facts, up_to_blank_nodes};

/// What differs between two stores, as lines: the route (off the rules' route, a bound changed
/// and one rebuilt are one route); off it, U1's facts, which the answers rest on, so `fresh`
/// must have U1 built and `later` must hold its facts; and each of `reads`, compared up to
/// blank-node renaming, with its status whole: soundness, completeness, the reasons' texts and
/// the bounds' counts.
fn differs(
    later: &nrese_store::MutationPipeline,
    fresh: &nrese_store::MutationPipeline,
    reads: &[&str],
) -> Vec<String> {
    let routes = [later, fresh].map(|dl| dl.store().dl_bounds().last);
    eprintln!("routes {routes:?} (later, fresh)");
    let mut wrong = Vec::new();
    if (routes[0] == "rules") != (routes[1] == "rules") {
        wrong.push(format!("routes {routes:?}"));
    }
    if routes[1] != "rules" {
        let [kept, built] = [later, fresh].map(u1_facts);
        if built.is_none() || kept != built {
            wrong.push(format!("U1's facts: later {kept:?}; fresh {built:?}"));
        }
    }
    for read in reads {
        let told = |dl| {
            let (rows, status) = up_to_blank_nodes(dl, read);
            format!("{rows:?}, {:?}", status.shared)
        };
        let [a, b] = [later, fresh].map(told);
        eprintln!("{read}: later {a}; fresh {b}");
        if a != b {
            wrong.push(format!("{read}: later {a}; fresh {b}"));
        }
    }
    wrong
}

/// A store given `first`, then the update `then`, against a store given `statements`, the
/// result, in one commit, which gives the expected route: the route before `then`, the later
/// store's after it, and what [`differs`]. Off the rules' route, the state before `then` must
/// have the upper bound built.
fn after_is_fresh(
    first: &str,
    then: &str,
    statements: &str,
    reads: &[&str],
) -> (&'static str, &'static str, Vec<String>) {
    let (later, fresh) = (pipeline(), pipeline());
    insert(&later, first).expect("the first commit");
    let bounds = later.store().dl_bounds();
    eprintln!("route {} before the later commit", bounds.last);
    super::update(&later, &format!("{SH}{then}")).expect("the later commit");
    insert(&fresh, statements).expect("one commit");
    let mut wrong = Vec::new();
    if bounds.last != "rules" && bounds.unavailable.is_some() {
        wrong.push(format!("before the later commit: {:?}", bounds.unavailable));
    }
    wrong.extend(differs(&later, &fresh, reads));
    (bounds.last, later.store().dl_bounds().last, wrong)
}

/// Spec (Structural Specification §5.5: of the reserved vocabulary only the nine built-in
/// annotation properties are annotation properties, so `owl:sameAs` declared one is none; a
/// store's reading after any history equals a fresh read of its statements): with `owl:sameAs`
/// declared an annotation property, a later `:Person owl:sameAs :p2` on a class is an equality
/// on `:Person`, which an individual's class then also names: the commit takes the route and
/// the reading a fresh read of the same statements takes, each read the same answers with the
/// same status. No SHACL term takes part.
#[test]
fn an_equality_on_a_class_with_owl_same_as_declared_an_annotation_property_reads_as_a_fresh_store()
{
    let first = "owl:sameAs a owl:AnnotationProperty . :Person a owl:Class . :ann a :Person .";
    let then = ":Person owl:sameAs :p2 .";
    let (_, _, wrong) = after_is_fresh(
        first,
        &format!("INSERT DATA {{ {then} }}"),
        &format!("{first} {then}"),
        &[
            "SELECT ?x { ?x a :Person }",
            "SELECT ?x { ?x owl:sameAs :p2 }",
            "SELECT ?x { :ann a ?x }",
        ],
    );
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (Structural Specification §5.5, as above: `rdf:type` declared an annotation property
/// is none; the invariant, as above): deleting `rdf:type a owl:AnnotationProperty` beside
/// `sh:path` declared an object property and `:s sh:path :knows`, where `:knows` is one too,
/// takes the route and the reading a fresh read of the remaining statements takes, each read
/// the same answers with the same status.
#[test]
fn deleting_rdf_type_declared_an_annotation_property_reads_as_a_fresh_store() {
    let rest = "sh:path a owl:ObjectProperty . :knows a owl:ObjectProperty . :s sh:path :knows .";
    let declared = "rdf:type a owl:AnnotationProperty .";
    let (_, _, wrong) = after_is_fresh(
        &format!("{declared} {rest}"),
        &format!("DELETE DATA {{ {declared} }}"),
        rest,
        &[
            "SELECT ?x { ?x a owl:ObjectProperty }",
            "SELECT ?x { ?x sh:path ?o }",
            "SELECT ?x { ?x a owl:Thing }",
        ],
    );
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (Structural Specification §5.5, as above: `owl:sameAs` declared an annotation property
/// is none; the invariant, as above): with `owl:sameAs` declared an annotation property, a
/// later `sh:path owl:sameAs :alias` is an equality on `sh:path`: the commit takes the route
/// and the reading a fresh read of the same statements takes, each read the same answers with
/// the same status.
#[test]
fn an_equality_on_sh_path_with_owl_same_as_declared_an_annotation_property_reads_as_a_fresh_store()
{
    let first = "owl:sameAs a owl:AnnotationProperty .";
    let then = "sh:path owl:sameAs :alias .";
    let (_, _, wrong) = after_is_fresh(
        first,
        &format!("INSERT DATA {{ {then} }}"),
        &format!("{first} {then}"),
        &[
            "SELECT ?x { ?x owl:sameAs :alias }",
            "SELECT ?x { sh:path owl:sameAs ?x }",
            "SELECT ?x { ?x a owl:Thing }",
        ],
    );
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (Structural Specification §5.5: `owl:sameAs` declared an annotation property is none,
/// so the declaration gives nothing a meaning, and `sh:path owl:sameAs :alias` is an equality
/// on `sh:path`, which names it, and the SHACL content `:s sh:path :knows` is then read as any
/// other statement): a store with the declaration reads as one without it: the same route,
/// and each read the same answers with the same status.
#[test]
fn owl_same_as_declared_an_annotation_property_reads_as_a_store_without_the_declaration() {
    let rest = ":knows a owl:ObjectProperty . :s sh:path :knows . sh:path owl:sameAs :alias .";
    let (declared, without) = (pipeline(), pipeline());
    insert(
        &declared,
        &format!("owl:sameAs a owl:AnnotationProperty . {rest}"),
    )
    .expect("one");
    insert(&without, rest).expect("one commit");
    let reads = [
        "SELECT ?x { ?x sh:path ?o }",
        "SELECT ?x { ?x a owl:Thing }",
    ];
    let wrong = differs(&declared, &without, &reads);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (Structural Specification §5.5: the nine built-in annotation properties are the
/// reserved vocabulary's annotation properties, declared or not; OWL 2 Direct Semantics gives
/// annotations no meaning): with `rdfs:label` and `rdfs:comment` declared annotation
/// properties beside a class and its member, on the rules' route, a later commit of their
/// statements on both keeps the route, as a fresh read of the same statements takes it, each
/// read the same answers with the same status.
#[test]
fn a_built_in_annotation_statement_keeps_the_route_as_a_fresh_store() {
    let first = "rdfs:label a owl:AnnotationProperty . rdfs:comment a owl:AnnotationProperty . \
                 :Person a owl:Class . :ann a :Person .";
    let then = ":Person rdfs:label \"person\" . :ann rdfs:comment \"a note\" .";
    let (before, after, wrong) = after_is_fresh(
        first,
        &format!("INSERT DATA {{ {then} }}"),
        &format!("{first} {then}"),
        &[
            "SELECT ?x { ?x rdfs:label ?o }",
            "SELECT ?x { ?x rdfs:comment ?o }",
            "SELECT ?x { ?x a :Person }",
        ],
    );
    assert_eq!([before, after], ["rules", "rules"], "the route kept");
    assert!(wrong.is_empty(), "{wrong:#?}");
}
