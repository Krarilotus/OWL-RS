//! Reads where every name of the data is an answer. A variable in an individual's place binds
//! to every non-reserved name of the ontology that entails the pattern so instantiated (SPARQL
//! 1.1 Entailment Regimes §7.2, (C2) read as "occurs in O(SG)"). Where the ontology makes every
//! element a member of C0 (⊤ ⊑ C0), every name is in C0: a class or property name, which has no
//! individual view, and one the data makes an individual only by `:C2 owl:sameAs :C2`. The store
//! answers for the names it reads as individuals, so such a read is never `complete`. Each
//! case's answers are the regime's, by hand; a read of `?x :p ?e`, `?e` not answered, has a row
//! where the data entails a successor, named or not (the reading docs/spec/reasoning-semantics.md
//! names).

use std::collections::BTreeSet;
use std::sync::Arc;

use nrese_sparql::GraphAccess;
use nrese_store::{ReadScope, SparqlQueryRequest, StatementOp};

use super::queries::query_with;
use super::versions::{in_transaction, ntriples};
use super::{PREFIXES, Status, insert, pipeline};

const THING: &str = ":C0 a owl:Class . :C2 a owl:Class . :a0 a owl:NamedIndividual . \
     owl:Thing rdfs:subClassOf :C0 .";
const OWN_EQUAL: &str = ":C2 owl:sameAs :C2 . :a0 a :C2 .";
/// A union, which takes an ontology off the rules route, and its names.
const OUTSIDE_RL: &str = ":Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] . :tib a :Pet .";
const TYPE_C0: &str = "SELECT ?x { ?x a :C0 }";
/// Every pair of elements is related by `owl:topObjectProperty` (Structural Specification
/// §5.3), so its domain or range C0 holds every element. OWL 2 RL has no top property (OWL 2
/// Profiles §4.2.1), so this ontology isn't on the rules route. `owl:topDataProperty` has no
/// such twin: OWL 2 DL allows it only as a superproperty (Structural Specification §11.2).
const TOP: &str = ":C0 a owl:Class . :C1 a owl:Class . :a0 a :C1 . :a1 a owl:NamedIndividual .";
/// OWL 2 RL, on the rules route.
const RULES: &str = ":C0 a owl:Class . :C1 a owl:Class . :C1 rdfs:subClassOf :C0 . :a0 a :C1 .";

/// Seed 1086 of the outside check, shrunk: every element has a p0-successor, so is in C1, so C3
/// is empty, so every element is in ≤0 p0.C3 ⊑ C0. Without C3's axioms ([`C3`]): an element
/// outside C0 has a p0-successor in C3, one in C0 two in C1, so every element has one.
const DECLARED: &str = ":C0 a owl:Class . :C1 a owl:Class . :C2 a owl:Class . :C3 a owl:Class . \
     :p0 a owl:ObjectProperty . :p1 a owl:ObjectProperty . :p2 a owl:ObjectProperty . \
     :p3 a owl:ObjectProperty . :d0 a owl:DatatypeProperty . :d1 a owl:DatatypeProperty . \
     :a0 a owl:NamedIndividual . :a1 a owl:NamedIndividual . :a2 a owl:NamedIndividual . \
     :a3 a owl:NamedIndividual . \
     [ a owl:Restriction ; owl:onProperty :p0 ; owl:maxQualifiedCardinality \
       \"1\"^^xsd:nonNegativeInteger ; owl:onClass :C1 ] owl:disjointWith :C0 .";
const C3: &str = ":C3 owl:disjointWith [ a owl:Restriction ; owl:onProperty :p0 ; \
     owl:minQualifiedCardinality \"1\"^^xsd:nonNegativeInteger ; owl:onClass :C1 ] . \
     :p0 rdfs:domain :C1 .";
const NONE_IN_C3: &str = "[ a owl:Restriction ; owl:onProperty :p0 ; owl:maxQualifiedCardinality \
     \"0\"^^xsd:nonNegativeInteger ; owl:onClass :C3 ] rdfs:subClassOf :C0 .";
const ALL_1086: &str = "C0 C1 C2 C3 p0 p1 p2 p3 d0 d1 a0 a1 a2 a3";

/// A case: its name, its commits, the read and the regime's answers by hand (`?x`'s values, or
/// `true` for an ASK that holds).
type Case = (&'static str, Vec<String>, &'static str, &'static str);

/// Each case the store answers otherwise than `exactly` (the controls: the regime's answers,
/// told `complete`), or else (`exactly` false) where it is told `complete` with an answer
/// missing; with its route, answers and status. (An answer the regime hasn't, told sound, is
/// another claim's: `owl:topObjectProperty` itself among C0's members below is one.)
fn wrong(cases: &[Case], exactly: bool) -> Vec<String> {
    let mut wrong = Vec::new();
    for (name, commits, read, expected) in cases {
        let dl = pipeline();
        for c in commits {
            insert(&dl, c).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        }
        let route = dl.store().dl_bounds().last;
        let (rows, status) =
            query_with(&dl, read, None).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        // An ASK's `false` is no answer.
        let got: BTreeSet<&str> = rows
            .iter()
            .map(String::as_str)
            .filter(|r| *r != "false")
            .collect();
        let want: BTreeSet<&str> = expected.split_whitespace().collect();
        let missing: Vec<_> = want.difference(&got).collect();
        let extra: Vec<_> = got.difference(&want).collect();
        let told = status.shared.as_str();
        let line = format!(
            "{name} (route {route}): {rows:?}, {told}, missing {missing:?}, extra {extra:?}, {:?}",
            status.reasons()
        );
        let failed = match exactly {
            true => !missing.is_empty() || !extra.is_empty() || !status.is_complete(),
            false => !missing.is_empty() && status.is_complete(),
        };
        if failed {
            wrong.push(line);
        }
    }
    wrong
}

fn with(data: &[&str]) -> Vec<String> {
    vec![data.join(" ")]
}

/// Where every element is in C0, `?x a :C0` answers every name of the data: never `complete`
/// without them, on the rules route or off it, the name an individual or not.
#[test]
fn a_type_every_element_has_is_never_complete_without_every_name() {
    let cases: Vec<Case> = vec![
        ("pun", with(&[THING, OWN_EQUAL]), TYPE_C0, "C0 C2 a0"),
        ("plain", with(&[THING, ":a0 a :C2 ."]), TYPE_C0, "C0 C2 a0"),
        (
            "declared",
            with(&[THING, ":C2 a owl:NamedIndividual . :a0 a :C2 ."]),
            TYPE_C0,
            "C0 C2 a0",
        ),
        (
            "top-domain",
            with(&["owl:topObjectProperty rdfs:domain :C0 .", TOP]),
            TYPE_C0,
            "C0 C1 a0 a1",
        ),
        (
            "top-range",
            with(&["owl:topObjectProperty rdfs:range :C0 .", TOP]),
            TYPE_C0,
            "C0 C1 a0 a1",
        ),
        (
            "1086",
            vec![
                format!("{DECLARED} {C3}"),
                format!("{NONE_IN_C3} {OWN_EQUAL}"),
            ],
            TYPE_C0,
            ALL_1086,
        ),
        (
            "1086-plain",
            vec![
                format!("{DECLARED} {C3}"),
                format!("{NONE_IN_C3} :a0 a :C2 ."),
            ],
            TYPE_C0,
            ALL_1086,
        ),
    ];
    let outside: Vec<Case> = cases[..3]
        .iter()
        .map(|(name, c, read, _)| {
            (
                *name,
                vec![format!("{} {OUTSIDE_RL}", c[0])],
                *read,
                "C0 C2 Cat Dog Pet a0 tib",
            )
        })
        .collect();
    let mut found = wrong(&cases, false);
    found.extend(
        wrong(&outside, false)
            .into_iter()
            .map(|w| format!("with a union: {w}")),
    );
    assert!(found.is_empty(), "{found:#?}");
}

/// Every element is in C0 through an individual, which the TBox alone doesn't say: one
/// element only, a0, a member (`owl:Thing ⊑ {a0}`), or every element related by the top
/// property to a0, a member of C1 (`∃U.C1 ⊑ C0`). The TBox's taxonomy puts no class
/// equivalent to `owl:Thing`, so it can't keep these reads `complete`.
#[test]
fn a_type_every_element_has_through_an_individual_is_never_complete_without_every_name() {
    let one = "owl:Thing rdfs:subClassOf [ owl:oneOf ( :a0 ) ] .";
    let top = "[ a owl:Restriction ; owl:onProperty owl:topObjectProperty ; \
        owl:someValuesFrom :C1 ] rdfs:subClassOf :C0 .";
    let cases: Vec<Case> = vec![
        (
            "one",
            with(&[one, TOP, ":a0 a :C0 ."]),
            TYPE_C0,
            "C0 C1 a0 a1",
        ),
        ("top-some", with(&[top, TOP]), TYPE_C0, "C0 C1 a0 a1"),
        (
            "one-ask",
            with(&[one, TOP, ":a0 a :C0 ."]),
            "ASK { :C1 a :C0 }",
            "true",
        ),
    ];
    let wrong = wrong(&cases, false);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A ground question about a name with no individual view, `ASK { :C2 a :C0 }`, holds where
/// every element is in C0: its `false` is never `complete`.
#[test]
fn a_ground_type_of_a_name_with_no_individual_view_is_never_complete_false() {
    let cases: Vec<Case> = vec![
        (
            "plain",
            with(&[THING, ":a0 a :C2 ."]),
            "ASK { :C2 a :C0 }",
            "true",
        ),
        (
            "outside",
            with(&[THING, ":a0 a :C2 .", OUTSIDE_RL]),
            "ASK { :C2 a :C0 }",
            "true",
        ),
        (
            "top-domain",
            with(&["owl:topObjectProperty rdfs:domain :C0 .", TOP]),
            "ASK { :C1 a :C0 }",
            "true",
        ),
    ];
    let wrong = wrong(&cases, false);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Where every element has a p0-successor, a data value or itself as an r-successor, a read of
/// the property with its subject answered answers every name: never `complete` without them.
#[test]
fn a_property_every_element_has_is_never_complete_without_every_name() {
    let every_value = ":d1 a owl:DatatypeProperty . :C0 a owl:Class . :a0 a :C0 . owl:Thing \
        rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :d1 ; owl:hasValue 2 ] .";
    let reflexive = ":r a owl:ObjectProperty , owl:ReflexiveProperty . :C0 a owl:Class . \
        :a0 a :C0 . :x a [ owl:unionOf ( :C0 :C1 ) ] .";
    let exists = "SELECT ?x { ?x :p0 ?e }";
    let cases: Vec<Case> = vec![
        (
            "1086",
            vec![DECLARED.into(), format!("{NONE_IN_C3} {OWN_EQUAL}")],
            exists,
            ALL_1086,
        ),
        (
            "1086-plain",
            vec![DECLARED.into(), format!("{NONE_IN_C3} :a0 a :C2 .")],
            exists,
            ALL_1086,
        ),
        (
            "value",
            with(&[every_value]),
            "SELECT ?x { ?x :d1 ?v }",
            "C0 a0 d1",
        ),
        (
            "reflexive",
            with(&[reflexive]),
            "SELECT ?x { ?x :r ?y }",
            "C0 C1 a0 r x",
        ),
    ];
    let wrong = wrong(&cases, false);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Every element is in `owl:Thing` and equal to itself, whatever the ontology: a read of either
/// with an end answered or named answers every name, on the rules route and off it.
#[test]
fn what_every_element_is_is_never_complete_without_every_name() {
    let names = "C0 C1 a0 a1 Cat Dog Pet tib";
    let cases: Vec<Case> = vec![
        (
            "thing",
            with(&[TOP, OUTSIDE_RL]),
            "SELECT ?x { ?x a owl:Thing }",
            names,
        ),
        (
            "thing-ask",
            with(&[TOP, OUTSIDE_RL]),
            "ASK { :C1 a owl:Thing }",
            "true",
        ),
        (
            "same",
            with(&[TOP, OUTSIDE_RL]),
            "SELECT ?x { ?x owl:sameAs ?y }",
            names,
        ),
        (
            "rules-thing",
            with(&[RULES]),
            "SELECT ?x { ?x a owl:Thing }",
            "C0 C1 a0",
        ),
        (
            "rules-thing-ask",
            with(&[RULES]),
            "ASK { :C1 a owl:Thing }",
            "true",
        ),
        (
            "rules-same",
            with(&[RULES]),
            "SELECT ?x { ?x owl:sameAs ?y }",
            "C0 C1 a0",
        ),
    ];
    let wrong = wrong(&cases, false);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Reads where no fact holds of every element stay `complete` with the regime's answers: a
/// union over all (no name is entailed in C0), a range, a domain of a stated edge, and an OWL
/// 2 RL ontology on the rules route, where no element need be in any class but `owl:Thing`.
#[test]
fn a_read_whose_answers_are_all_individuals_stays_complete() {
    let rl = ":C0 a owl:Class . :C2 a owl:Class . :a0 a owl:NamedIndividual . \
        :p0 a owl:ObjectProperty ; rdfs:domain :C0 . :C2 :p0 :a0 .";
    let union = ":C0 a owl:Class . :C1 a owl:Class . :a0 a :C1 . \
        owl:Thing rdfs:subClassOf [ owl:unionOf ( :C0 :C1 ) ] .";
    let range = ":p0 a owl:ObjectProperty ; rdfs:range :C0 . :C0 a owl:Class . :a0 :p0 :a1 . \
        :x a [ owl:unionOf ( :C0 :C1 ) ] .";
    let rules = RULES;
    let cases: Vec<Case> = vec![
        ("union", with(&[union]), TYPE_C0, ""),
        ("range", with(&[range]), "SELECT ?x { ?x :p0 ?y }", "a0"),
        ("domain-pun", with(&[rl, OWN_EQUAL]), TYPE_C0, "C2"),
        ("domain", with(&[rl, ":a0 a :C2 ."]), TYPE_C0, "C2"),
        ("rules", with(&[rules]), TYPE_C0, "a0"),
        ("rules-ask", with(&[rules]), "ASK { :C1 a :C0 }", ""),
    ];
    let wrong = wrong(&cases, true);
    assert!(wrong.is_empty(), "{wrong:#?}");
    let dl = pipeline();
    insert(&dl, rules).expect("committed");
    assert_eq!(dl.store().dl_bounds().last, "rules");
}

/// A read with no bounds' view, one inside a transaction with an addition pending or one by a
/// reader of some graphs, is never `complete` where every name answers it.
#[test]
fn a_read_without_the_bounds_view_is_never_complete_where_every_name_answers() {
    let dl = pipeline();
    insert(&dl, &format!("{THING} :a0 a :C2 .")).expect("committed");
    let edge = "<http://example.com/z> <http://example.com/r> <http://example.com/w> .";
    let add = StatementOp::Add {
        data: ntriples(edge),
        contexts: Vec::new(),
    };
    let (_, pending) = in_transaction(&dl, vec![add], TYPE_C0);
    let pending = pending.expect("a status");
    assert!(!pending.complete, "{pending:?}");
    let scope = ReadScope::Graphs(Arc::new(GraphAccess {
        default_graph: true,
        inferred: true,
        ..GraphAccess::default()
    }));
    let request = SparqlQueryRequest::new(format!("{PREFIXES}{TYPE_C0}"), scope);
    let prepared = dl.store().prepare_query(&request).expect("prepared");
    let status: Status = super::run_dl(dl.store(), &prepared, &mut Vec::new())
        .expect("query")
        .expect("a status");
    assert!(!status.is_complete(), "{status:?}");
}
