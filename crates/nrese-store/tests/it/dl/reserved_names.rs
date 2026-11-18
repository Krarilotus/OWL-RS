//! A name of the reserved vocabulary declared an annotation property under `owl2-dl`. The
//! Structural Specification §5.5 lists the nine annotation properties OWL 2 builds in
//! (`rdfs:label` and the others) and says the reserved vocabulary's other IRIs "MUST NOT be used
//! to identify annotation properties"; such a declaration (`owl:sameAs a
//! owl:AnnotationProperty`, `rdf:type a owl:AnnotationProperty`) makes neither one, and the
//! reader still reads `owl:sameAs` as equality and `rdf:type` as typing. Each test holds a
//! store's reading after a history to a fresh read of its statements, given in one commit, as
//! the route `dl_bounds().last` and each read's answers and status.

use super::pipeline;

/// The SHACL and RDF prefixes, besides [`super::PREFIXES`].
const SH: &str = "PREFIX sh: <http://www.w3.org/ns/shacl#> \
     PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

/// Commits `INSERT DATA { data }`, with [`SH`] besides the usual prefixes.
fn insert(
    dl: &nrese_store::MutationPipeline,
    data: &str,
) -> Result<u64, nrese_store::MutationError> {
    super::update(dl, &format!("{SH}INSERT DATA {{ {data} }}"))
}

/// The answers of a one-variable `read` (with [`SH`]), sorted, and their status: an IRI written
/// whole (`<…>`), a literal quoted, and each blank node `_:` with the number of answers it is,
/// so that answers equal up to a renaming of blank nodes compare equal (RDF 1.1 Concepts §3.6).
fn up_to_blank_nodes(
    dl: &nrese_store::MutationPipeline,
    read: &str,
) -> (Vec<String>, super::Status) {
    let store = dl.store();
    let request = nrese_store::SparqlQueryRequest::all(format!("{}{SH}{read}", super::PREFIXES));
    let prepared = store.prepare_query(&request).expect("query");
    let mut out = Vec::new();
    let status = super::run_dl(store, &prepared, &mut out)
        .expect("query")
        .expect("a status under owl2-dl");
    let json: serde_json::Value = serde_json::from_slice(&out).expect("json results");
    let answers: Vec<(&str, &str)> = json["results"]["bindings"]
        .as_array()
        .expect("bindings")
        .iter()
        .map(|row| {
            let x = &row["x"];
            (
                x["type"].as_str().unwrap_or("-"),
                x["value"].as_str().unwrap_or("-"),
            )
        })
        .collect();
    let mut rows: Vec<String> = answers
        .iter()
        .map(|&(kind, value)| match kind {
            "bnode" => {
                let n = answers.iter().filter(|&&a| a == (kind, value)).count();
                format!("_:{n}")
            }
            "uri" => format!("<{value}>"),
            _ => format!("{value:?}"),
        })
        .collect();
    rows.sort();
    (rows, status)
}

/// U1's facts beyond L, sorted: U1's own terms (Skolem constants, fresh classes) as one name,
/// as `bounds.rs`'s `canonical` writes them (their numbering follows the clauses'), each blank
/// node `_:` with the number of facts it is in (RDF 1.1 Concepts §3.6). `None` where U1 isn't
/// built.
fn u1_facts(dl: &nrese_store::MutationPipeline) -> Option<Vec<[String; 3]>> {
    let facts = dl.store().dl_upper_facts(false)?;
    let in_facts = |b: &str| facts.iter().filter(|f| f.iter().any(|t| t == b)).count();
    let own = |t: &str| t.strip_prefix('<').is_some_and(nrese_dl::internal::is_made);
    let name = |t: String| {
        if own(&t) {
            "<U1's own>".to_owned()
        } else if t.starts_with("_:") {
            format!("_:{}", in_facts(&t))
        } else {
            t
        }
    };
    let mut named: Vec<[String; 3]> = facts.iter().map(|f| f.clone().map(name)).collect();
    named.sort();
    Some(named)
}

/// A store given `first`, then the update `then`, against a store given `statements`, the
/// result, in one commit: what differs, as lines. The fresh store gives the expected route;
/// off the rules' route the answers rest on U1, so the fresh store must have it built and the
/// later store must hold its facts. Each of `reads` is compared up to blank-node renaming,
/// with its status whole: soundness, completeness, the reasons' texts and the bounds' counts.
fn after_is_fresh(first: &str, then: &str, statements: &str, reads: &[&str]) -> Vec<String> {
    let (later, fresh) = (pipeline(), pipeline());
    insert(&later, first).expect("the first commit");
    let bounds = later.store().dl_bounds();
    super::update(&later, &format!("{SH}{then}")).expect("the later commit");
    insert(&fresh, statements).expect("one commit");
    let routes = [&later, &fresh].map(|dl| dl.store().dl_bounds().last);
    eprintln!("route {}, then {routes:?} (later, fresh)", bounds.last);
    let mut wrong = Vec::new();
    // Off the rules' route, the state before `then` has the upper bound built.
    if bounds.last != "rules" && bounds.unavailable.is_some() {
        wrong.push(format!("before the later commit: {:?}", bounds.unavailable));
    }
    // On the rules' route or off it: off it, a bound changed and one rebuilt are one route.
    if (routes[0] == "rules") != (routes[1] == "rules") {
        wrong.push(format!("routes {routes:?}"));
    }
    if routes[1] != "rules" {
        let [kept, built] = [&later, &fresh].map(u1_facts);
        if built.is_none() || kept != built {
            wrong.push(format!("U1's facts: later {kept:?}; fresh {built:?}"));
        }
    }
    for read in reads {
        let told = |dl| {
            let (rows, status) = up_to_blank_nodes(dl, read);
            format!("{rows:?}, {:?}", status.shared)
        };
        let [a, b] = [&later, &fresh].map(told);
        eprintln!("{read}: later {a}; fresh {b}");
        if a != b {
            wrong.push(format!("{read}: later {a}; fresh {b}"));
        }
    }
    wrong
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
    let wrong = after_is_fresh(
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
    let wrong = after_is_fresh(
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
    let wrong = after_is_fresh(
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
