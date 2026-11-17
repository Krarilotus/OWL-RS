//! SHACL content under `owl2-dl`: a triple whose predicate is a property of the SHACL
//! vocabulary carries no OWL meaning, so the OWL reader reads it as SHACL content, as it reads
//! an annotation. A store whose only punning comes from such triples stays on the OWL 2 RL
//! rules' route. A store that gives a SHACL term an OWL meaning (an axiom on it, or a use of it
//! as an individual) is read as any other, off the route; so is a class that is also a node
//! shape, whose type triple is a class assertion. Each test names the route,
//! `dl_bounds().last`, as the routing tests do.

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

/// Spec (OWL 2 Profiles §4.3, theorem PR1; SHACL content, like an annotation, carries no OWL
/// meaning): explicit-target shapes (`sh:NodeShape`, `sh:targetClass`, `sh:property`,
/// `sh:path`, `sh:class`) beside an OWL 2 RL ontology give no name an OWL kind, so nothing is
/// punned, the store stays on the rules' route, and a read of the ontology is answered as
/// without them. Read as property assertions, `sh:targetClass :Person` and `sh:path :knows`
/// would pun `:Person` and `:knows` and take it off.
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

/// Spec (OWL 2 Structural Specification §5.9, punning; OWL 2 Profiles §4.3, theorem PR1, needs
/// none): an implicit class target, a class that is also a node shape (`:Person a
/// sh:NodeShape, owl:Class`), states a class assertion of `sh:NodeShape` about `:Person`, which
/// makes the class `:Person` an individual too, so the store is off the rules' route; a read of
/// the ontology is answered as without the shape.
#[test]
fn an_implicit_class_target_stays_off_the_route() {
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
    assert_ne!(route, "rules", "a punned class stayed on the rules' route");
}

/// Spec (OWL 2 Structural Specification: a term an axiom uses has that axiom's kind): a SHACL
/// term given an OWL meaning is read as any other term, so its shapes' statements are property
/// and class assertions that pun, and the store is off the rules' route. Each row adds one to
/// the explicit-target shapes: a declaration of `sh:path` as an object property; a
/// sub-property axiom on `sh:class`. (An equality on `sh:path` and the W3C vocabulary's
/// content have tests of their own, below.)
#[test]
fn a_shacl_term_given_an_owl_meaning_is_read_as_any_term_off_the_route() {
    let rows = [
        ("declaration", "sh:path a owl:ObjectProperty ."),
        ("sub-property", "sh:class rdfs:subPropertyOf :related ."),
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

/// Spec (OWL 2 Structural Specification §5.9, punning; OWL 2 RL's eq-rep-p; under the OWL 2
/// Direct Semantics an equality of the individual `sh:path` with `:p` doesn't equate the
/// properties): with `sh:path owl:sameAs :p`, the rules' eq-rep-p
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

/// Spec (the W3C SHACL vocabulary, `shacl.ttl`, declares `sh:` properties `rdf:Property`, with
/// domains and ranges, `rdfs:Class` among them): that content gives SHACL terms an OWL
/// reading, so the store is read as any other: off the rules' route, and told that parts of
/// the data aren't OWL 2 DL and were left out (`dl::consistency`, `left_out_by`), as for any
/// reserved name used as an entity.
#[test]
fn the_shacl_vocabulary_loaded_is_read_as_any_term() {
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

/// Spec (OWL 2 Mapping to RDF Graphs: list triples no axiom reads are structure, not
/// assertions; SHACL content carries no OWL meaning): the lists of SHACL (a sequence path,
/// `sh:in`, `sh:or`) are structure no axiom uses, and an inverse path is SHACL content, so
/// none of them makes a name an individual: the store stays on the rules' route. Read as
/// property assertions, `sh:inversePath :knows` and `sh:class :Person` inside `sh:or` would
/// pun `:knows` and `:Person`.
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

/// Spec (OWL 2 Direct Semantics: annotations carry no meaning, and SHACL content none
/// either): a conclusion of SHACL statements only is answered as one of annotations, with a
/// premise that entails neither.
#[test]
fn a_conclusion_of_shacl_statements_is_answered_as_one_of_annotations() {
    let premise = ":a a :C .";
    let annotation = entailed(premise, ":s rdfs:label \"x\" .");
    let shacl = entailed(premise, ":s <http://www.w3.org/ns/shacl#path> :q .");
    assert_eq!(shacl, annotation, "annotation {annotation:?}");
}

/// Spec (OWL 2 Direct Semantics: an ontology entails a conclusion only if it entails each of
/// its axioms): SHACL content in a conclusion adds no axiom to the rest of it: with `:a a :C`
/// entailed, a conclusion of it and of `:s sh:path :q` is entailed. Read as a property
/// assertion, `:s sh:path :q` wouldn't be.
#[test]
fn a_conclusion_mixing_shacl_content_with_an_axiom_is_decided_by_the_axiom() {
    let conclusion = ":s <http://www.w3.org/ns/shacl#path> :q . :a a :C .";
    assert_eq!(entailed(":a a :C .", conclusion), Entailed::Yes);
}

/// Spec (OWL 2 Direct Semantics: a premise entails a conclusion only if each of the
/// conclusion's axioms holds in every model of it; SHACL content has no OWL meaning only where
/// nothing gives its property one): a conclusion reads SHACL content as its premise does. Where
/// the premise gives `sh:path` an OWL meaning (a declaration; a use as an individual; or its
/// SHACL content read as any other, by an OWL axiom on `sh:class`, the content's other
/// property), the conclusion `:s sh:path :q` is a property assertion the premise doesn't
/// entail; where the premise reads `sh:path` as SHACL content too, the conclusion is entailed.
#[test]
fn a_conclusion_reads_shacl_content_as_its_premise_does() {
    let (path, class) = (
        "<http://www.w3.org/ns/shacl#path>",
        "<http://www.w3.org/ns/shacl#class>",
    );
    let rows = [
        (
            "declaration",
            format!("{path} a owl:ObjectProperty ."),
            Entailed::No,
        ),
        (
            "individual",
            format!(":p a owl:ObjectProperty . :x :p {path} ."),
            Entailed::No,
        ),
        (
            "read as any other",
            format!(":a {path} :b ; {class} :C . {class} a owl:ObjectProperty ."),
            Entailed::No,
        ),
        ("SHACL content", format!(":a {path} :b ."), Entailed::Yes),
    ];
    let mut wrong = Vec::new();
    for (what, premise, want) in rows {
        let answer = entailed(&premise, &format!(":s {path} :q ."));
        eprintln!("{what}: {answer:?}");
        if answer != want {
            wrong.push(format!("{what}: {answer:?}, not {want:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (OWL 2 Direct Semantics: SHACL content, like an annotation, has no meaning, so an
/// equality entails no copy of it): on the rules' route, `:s owl:sameAs :t` lets eq-rep-s copy
/// `:s sh:path :q` to `:t`, which OWL 2 DL doesn't entail. A read that returns `:t`, through the
/// predicate `sh:path` or through any predicate, may not be told `complete` or `sound-only`.
#[test]
fn a_copied_shacl_row_is_not_told_sound() {
    let dl = pipeline();
    insert(&dl, ONTOLOGY).expect("an RL ontology");
    shapes_insert(&dl, ":s sh:path :q . :s owl:sameAs :t .").expect("a copied shape row");
    let route = dl.store().dl_bounds().last;
    let mut wrong = Vec::new();
    for read in ["SELECT ?x { ?x sh:path ?o }", "SELECT ?x { ?x ?p :q }"] {
        let (rows, status) = query(&dl, &format!("{SH}{read}"));
        let told = status.shared.as_str();
        let line = format!(
            "route {route}: {read}: {rows:?}, {told} {:?}",
            status.reasons()
        );
        eprintln!("{line}");
        assert!(
            rows.iter().any(|r| r == "t"),
            "{line}: the rules' copy is not returned"
        );
        if matches!(told, "complete" | "sound-only") {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "copied rows told sound: {wrong:#?}");
    assert_eq!(route, "rules");
}

/// Spec (OWL 2 Profiles §4.3, Table 4: eq-rep-s, eq-rep-p and eq-rep-o copy a statement only
/// through an equality of two different terms): with no equality, the rules make no copy of
/// SHACL content, so a read of it keeps its status: `?x sh:path ?o` is answered `complete` on
/// the rules' route, and a read of any predicate isn't told `unsound`.
#[test]
fn shacl_content_without_an_equality_keeps_its_status() {
    let dl = pipeline();
    insert(&dl, ONTOLOGY).expect("an RL ontology");
    shapes_insert(&dl, ":s sh:path :q .").expect("a shape row");
    let route = dl.store().dl_bounds().last;
    let (rows, constant) = query(&dl, &format!("{SH}SELECT ?x {{ ?x sh:path ?o }}"));
    let (any, variable) = query(&dl, &format!("{SH}SELECT ?x {{ ?x ?p :q }}"));
    let line = format!(
        "route {route}: sh:path {rows:?} {}; any {any:?} {} {:?}",
        constant.shared.as_str(),
        variable.shared.as_str(),
        variable.reasons()
    );
    eprintln!("{line}");
    assert_eq!(
        (rows, any),
        (vec!["s".to_owned()], vec!["s".to_owned()]),
        "{line}"
    );
    assert!(constant.is_complete(), "{line}");
    assert_ne!(variable.shared.as_str(), "unsound", "{line}");
    assert_eq!(route, "rules", "{line}");
}

/// The reads [`later_is_fresh`] compares: through `sh:path`, of `owl:Thing`, through any
/// predicate.
const READS: [&str; 3] = [
    "SELECT ?x { ?x sh:path ?o }",
    "SELECT ?x { ?x a owl:Thing }",
    "SELECT ?x { ?x ?p :q }",
];

/// A store given `first`, then `then`, in two commits, against a store given both in one,
/// each after `ontology`: the route before the later commit, and what differs: the route
/// after it and, for each of [`READS`], the answers and their status (the reasons printed).
fn later_is_fresh(ontology: &str, first: &str, then: &str) -> (&'static str, Vec<String>) {
    let (later, fresh) = (pipeline(), pipeline());
    insert(&later, ontology).expect("the ontology");
    shapes_insert(&later, first).expect("the first commit");
    let before = later.store().dl_bounds().last;
    shapes_insert(&later, then).expect("the later commit");
    insert(&fresh, ontology).expect("the ontology");
    shapes_insert(&fresh, &format!("{first} {then}")).expect("one commit");
    let routes = [
        later.store().dl_bounds().last,
        fresh.store().dl_bounds().last,
    ];
    eprintln!("route {before}, then {routes:?} (later, fresh)");
    let mut wrong = Vec::new();
    // On the rules' route or off it: off it, a bound changed and one rebuilt are one route.
    if (routes[0] == "rules") != (routes[1] == "rules") {
        wrong.push(format!("routes {routes:?}"));
    }
    for read in READS {
        let [a, b] = [&later, &fresh].map(|dl| query(dl, &format!("{SH}{read}")));
        let told = |(rows, status): &(Vec<String>, super::Status)| {
            format!("{rows:?}, {}", status.shared.as_str())
        };
        eprintln!(
            "{read}: later {}; fresh {} ({:?}; {:?})",
            told(&a),
            told(&b),
            a.1.reasons(),
            b.1.reasons()
        );
        if told(&a) != told(&b) {
            wrong.push(format!("{read}: later {}; fresh {}", told(&a), told(&b)));
        }
    }
    (before, wrong)
}

/// Spec (OWL 2 Structural Specification: a term an axiom uses has that axiom's kind; OWL 2
/// Profiles §4.3, theorem PR1): on the rules' route, SHACL content `:s sh:path :q` and then a
/// declaration of `sh:path` as an object property: `:s sh:path :q` is then a property
/// assertion, with no name punned, as in a store given both at once: the same route, and each
/// read the same answers with the same status.
#[test]
fn a_later_declaration_of_a_shacl_property_reads_as_one_commit_of_both() {
    let (before, wrong) = later_is_fresh(
        ONTOLOGY,
        ":s sh:path :q .",
        "sh:path a owl:ObjectProperty .",
    );
    assert_eq!(before, "rules", "SHACL content beside an RL ontology");
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec (OWL 2 Structural Specification §5.9: a SHACL term used as an individual is one, and
/// its SHACL content is then read as any other; the upper bound holds every entailed
/// assertion): off the rules' route, with SHACL content `:s sh:path :q`, a later `:x :p
/// sh:path` through a known object property makes `:s sh:path :q` a property assertion, whose
/// ends are individuals, as in a store given both at once: the same route, and each read the
/// same answers with the same status.
#[test]
fn a_later_use_of_a_shacl_property_as_an_individual_reads_as_one_commit_of_both() {
    let outside = ":Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] . :tib a :Pet . \
         :p a owl:ObjectProperty . :a :p :b .";
    let (before, wrong) = later_is_fresh(outside, ":s sh:path :q .", ":x :p sh:path .");
    assert_ne!(before, "rules", "off the rules' route");
    assert!(wrong.is_empty(), "{wrong:#?}");
}
