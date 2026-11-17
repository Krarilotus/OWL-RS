//! SHACL content under `owl2-dl`: a triple whose predicate is in SHACL's namespace, or an
//! `rdf:type` statement whose class is, carries no OWL meaning, so the OWL reader reads it as
//! SHACL content, as it reads an annotation. A store whose only punning comes from such
//! triples stays on the OWL 2 RL rules' route. A store that gives a SHACL term an OWL meaning
//! (an axiom on it, or a use of it as an individual) is read as before, off the route.
//! Each test names the route, `dl_bounds().last`, as the routing tests do.

use nrese_store::dl::entailment::Entailed;

use super::entailment::entailed;
use super::queries::query;
use super::{insert, pipeline};

const SH: &str = "PREFIX sh: <http://www.w3.org/ns/shacl#> \
     PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ";

/// An OWL 2 RL ontology the shapes stand beside: `:bob` is a `:Person` and `:ann`, a
/// `:Student`, is one by the subclass axiom.
const ONTOLOGY: &str = ":Student rdfs:subClassOf :Person . :knows a owl:ObjectProperty . \
     :likes a owl:ObjectProperty . :ann a :Student ; :knows :bob . :bob a :Person .";

/// Shapes with explicit targets, in the store's shapes graph: a node shape whose target is
/// the class `:Person`, and a property shape on `:knows` whose values are `:Person`s.
fn explicit_shapes() -> String {
    format!(
        "GRAPH <{}> {{ :PersonShape a sh:NodeShape ; sh:targetClass :Person ; \
         sh:property :knowsShape . :knowsShape sh:path :knows ; sh:class :Person . }}",
        nrese_store::DEFAULT_SHAPES_GRAPH
    )
}

/// Commits `INSERT DATA { data }`, with the SHACL and RDF prefixes besides the usual ones.
fn shapes_insert(
    dl: &nrese_store::MutationPipeline,
    data: &str,
) -> Result<u64, nrese_store::MutationError> {
    super::update(dl, &format!("{SH}INSERT DATA {{ {data} }}"))
}

/// Spec (design record §7, S2; review round 2, §7.2): explicit-target shapes (`sh:NodeShape`,
/// `sh:targetClass`, `sh:property`, `sh:path`, `sh:class`) beside an OWL 2 RL ontology give
/// no name an OWL kind, so the store stays on the rules' route, and a read of the ontology
/// is answered as without them. Today `sh:targetClass :Person` and `sh:path :knows` are read
/// as object property assertions, which pun `:Person` and `:knows`, and take it off.
#[test]
fn explicit_target_shapes_beside_an_rl_ontology_stay_on_the_rules_route() {
    let dl = pipeline();
    insert(&dl, ONTOLOGY).expect("an RL ontology");
    shapes_insert(&dl, &explicit_shapes()).expect("shapes");
    let route = dl.store().dl_bounds().last;
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :Person }");
    eprintln!(
        "route {route}: {rows:?}, {} {:?}",
        status.shared.as_str(),
        status.reasons()
    );
    assert_eq!(rows, ["ann", "bob"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
    assert_eq!(route, "rules", "shapes took the store off the rules' route");
}

/// Spec (§7.1, C1, and S2b): an implicit class target, a class that is also a node shape
/// (`:Person a sh:NodeShape, owl:Class`), is a type triple of SHACL's namespace: SHACL
/// content, so `:Person` is no individual by it and the store stays on the rules' route.
/// Today the type triple is a class assertion, which puns `:Person`.
#[test]
fn an_implicit_class_target_stays_on_the_rules_route() {
    let dl = pipeline();
    insert(&dl, ONTOLOGY).expect("an RL ontology");
    shapes_insert(
        &dl,
        ":Person a sh:NodeShape , owl:Class ; sh:property [ sh:path :knows ; sh:class :Person ] .",
    )
    .expect("an implicit class target");
    let route = dl.store().dl_bounds().last;
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :Person }");
    eprintln!("route {route}: {rows:?}, {}", status.shared.as_str());
    assert_eq!(rows, ["ann", "bob"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
    assert_eq!(route, "rules", "an implicit class target left the route");
}

/// Spec (§7, S2; §7.1, F1 and C3; the D16-855 brief, s4): a SHACL term given an OWL meaning
/// keeps today's reading, so the store is off the rules' route as today. Each row adds one to
/// the explicit-target shapes: a declaration of `sh:path` as an object property; a
/// sub-property axiom on `sh:class`; a subclass of `sh:NodeShape`. (An equality on `sh:path`
/// and the W3C vocabulary's content have tests of their own, below.)
#[test]
fn a_shacl_term_given_an_owl_meaning_is_read_as_today_off_the_route() {
    let rows = [
        ("declaration", "sh:path a owl:ObjectProperty ."),
        ("sub-property", "sh:class rdfs:subPropertyOf :related ."),
        ("subclass", ":A rdfs:subClassOf sh:NodeShape ."),
    ];
    let mut wrong = Vec::new();
    for (what, extra) in rows {
        let dl = pipeline();
        insert(&dl, ONTOLOGY).expect("an RL ontology");
        shapes_insert(&dl, &format!("{} {extra}", explicit_shapes())).expect(what);
        let route = dl.store().dl_bounds().last;
        eprintln!("{what}: route {route}");
        if route == "rules" {
            wrong.push(format!("{what}: stayed on the rules' route"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (§7.1, F1; OWL 2 Direct Semantics, punning: an equality of the individual `sh:path`
/// with `:p` doesn't equate the properties): with `sh:path owl:sameAs :p`, the rules' eq-rep-p
/// derives `:knowsShape :p :knows` from `:knowsShape sh:path :knows`, which OWL 2 DL doesn't
/// entail. A read of it may return that row only labelled `unsound`, as for any punned
/// equality (`routing.rs`, `an_equality_of_punned_property_names_does_not_equate_their_properties`).
#[test]
fn an_equality_on_a_shacl_predicate_gives_no_row_told_sound() {
    let dl = pipeline();
    insert(&dl, ONTOLOGY).expect("an RL ontology");
    shapes_insert(
        &dl,
        &format!(
            "{} sh:path owl:sameAs :p . :p a owl:ObjectProperty .",
            explicit_shapes()
        ),
    )
    .expect("an equality on sh:path");
    let route = dl.store().dl_bounds().last;
    let (rows, status) = query(&dl, "SELECT ?x { ?x :p :knows }");
    let line = format!(
        "route {route}: {rows:?}, {} {:?}",
        status.shared.as_str(),
        status.reasons()
    );
    eprintln!("{line}");
    assert!(
        rows.is_empty() || status.shared.as_str() == "unsound",
        "a row OWL 2 DL doesn't entail, not labelled unsound: {line}"
    );
    assert_ne!(route, "rules", "{line}");
}

/// Spec (§7.1, C3): the W3C vocabulary's own content (`shacl.ttl`: `sh:` properties typed
/// `rdf:Property`, with domains and ranges, `rdfs:Class` among them) gives SHACL terms an OWL
/// reading, so the store is read as today: off the rules' route, and told that parts of the
/// data aren't OWL 2 DL and were left out (`dl::consistency`, `left_out_by`), today's wording.
#[test]
fn the_shacl_vocabulary_loaded_is_read_as_today() {
    let dl = pipeline();
    insert(&dl, ONTOLOGY).expect("an RL ontology");
    let vocabulary = "sh:path a rdf:Property ; rdfs:domain sh:PropertyShape . \
         sh:class a rdf:Property ; rdfs:range rdfs:Class . \
         sh:targetClass a rdf:Property ; rdfs:range rdfs:Class .";
    let committed = shapes_insert(&dl, &format!("{} {vocabulary}", explicit_shapes()));
    let route = dl.store().dl_bounds().last;
    let words = match &committed {
        Ok(_) => dl
            .store()
            .dl()
            .status()
            .and_then(|s| s.consistency.verdict.reason().map(str::to_owned))
            .unwrap_or_default(),
        Err(e) => format!("{e:?}"),
    };
    eprintln!("route {route}; committed {}; {words}", committed.is_ok());
    assert!(
        words.contains("aren't OWL 2 DL and were left out"),
        "{words}"
    );
    assert_ne!(route, "rules");
}

/// Spec (§7.1, C2): the lists of SHACL (a sequence path, `sh:in`, `sh:or`) are structure no
/// axiom uses, and an inverse path is SHACL content, so none of them makes a name an
/// individual: the store stays on the rules' route. Today `sh:inversePath :knows` and
/// `sh:class :Person` inside `sh:or` are object property assertions that pun `:knows` and
/// `:Person`.
#[test]
fn shacl_paths_and_lists_make_no_pun() {
    let rows = [
        ("sequence path", "sh:path ( :knows :likes )"),
        ("inverse path", "sh:path [ sh:inversePath :knows ]"),
        ("sh:in", "sh:path :likes ; sh:in ( :bob :ann )"),
        (
            "sh:or",
            "sh:path :likes ; sh:or ( [ sh:class :Person ] [ sh:class :Student ] )",
        ),
    ];
    let mut wrong = Vec::new();
    for (what, property) in rows {
        let dl = pipeline();
        insert(&dl, ONTOLOGY).expect("an RL ontology");
        let shape =
            format!(":S a sh:NodeShape ; sh:targetNode :ann ; sh:property [ {property} ] .");
        shapes_insert(&dl, &shape).expect(what);
        let route = dl.store().dl_bounds().last;
        eprintln!("{what}: route {route}");
        if route != "rules" {
            wrong.push(format!("{what}: route {route}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (the D16-051 lead, 17 Nov; OWL 2 Direct Semantics: SHACL content, like an annotation,
/// carries no OWL meaning): a conclusion of SHACL statements only is answered as one of
/// annotations, with a premise that entails neither.
#[test]
fn a_conclusion_of_shacl_statements_is_answered_as_one_of_annotations() {
    let premise = ":a a :C .";
    let annotation = entailed(premise, ":s rdfs:label \"x\" .");
    let shacl = entailed(premise, ":s <http://www.w3.org/ns/shacl#path> :q .");
    assert_eq!(shacl, annotation, "annotation {annotation:?}");
}

/// Spec (the D16-051 lead, 17 Nov): SHACL content in a conclusion takes nothing from the rest
/// of it: with `:s a :C` not entailed, the conclusion isn't.
#[test]
fn a_conclusion_mixing_shacl_content_with_an_axiom_is_decided_by_the_axiom() {
    let conclusion = ":s <http://www.w3.org/ns/shacl#path> :q . :s a :C .";
    assert_eq!(entailed(":a a :C .", conclusion), Entailed::No);
}

/// Spec (§7.2, G1, and the master's ruling of 19:30: one status rule for every read that may
/// match content without OWL meaning): on the rules' route, `:s owl:sameAs :t` lets eq-rep-s
/// copy `:s sh:path :q` to `:t`, which OWL 2 DL doesn't entail once SHACL content carries no
/// OWL meaning. A read of `?x sh:path ?o` that returns `:t` may not be told `complete` or
/// `sound-only`.
#[test]
#[ignore = "waits on D16-861: the status rule for reads that may match content without OWL meaning"]
fn a_copied_shacl_row_is_not_told_sound() {
    let dl = pipeline();
    insert(&dl, ONTOLOGY).expect("an RL ontology");
    shapes_insert(&dl, ":s sh:path :q . :s owl:sameAs :t .").expect("a copied shape row");
    let route = dl.store().dl_bounds().last;
    let (rows, status) = query(&dl, &format!("{SH}SELECT ?x {{ ?x sh:path ?o }}"));
    let line = format!(
        "route {route}: {rows:?}, {} {:?}",
        status.shared.as_str(),
        status.reasons()
    );
    eprintln!("{line}");
    assert_eq!(route, "rules", "{line}");
    let told = status.shared.as_str();
    assert!(
        !rows.iter().any(|r| r == "t") || !matches!(told, "complete" | "sound-only"),
        "{line}"
    );
}
