//! The bounds' gap over U1's equality classes. U1 keeps its facts over the representative
//! of each `owl:sameAs` class, and a fact over a representative stands for every member.
//! It is L's, and outside the gap, only where L holds it for each of them; a fact L holds
//! for the representative alone is still gap for the other members. Each case states its
//! answer by OWL 2's Direct Semantics alone.

use super::queries::query;
use super::{insert, pipeline};

/// `A ⊑ {b}` and `a ∈ A`: every model has `a = b`, so whatever holds of b holds of a.
/// `ObjectOneOf` isn't an OWL 2 RL superclass expression, so the RL rules don't derive
/// `a = b`; U1 does. `b ∈ D` with `D ⊑ E ⊔ F` and `E, F ⊑ C` makes `b ∈ C`, which the RL
/// rules don't derive either; L has it from the taxonomy (`D ⊑ C`), for b and not for a.
/// Each order commits one of a and b first, so each is the smaller id once.
#[test]
fn a_certain_identity_makes_its_members_answers() {
    let identity = ":a a :A . :A rdfs:subClassOf [ a owl:Class ; owl:oneOf ( :b ) ] .";
    let membership = ":b a :D . :D rdfs:subClassOf [ a owl:Class ; owl:unionOf ( :E :F ) ] . \
         :E rdfs:subClassOf :C . :F rdfs:subClassOf :C .";
    for (order, first, second) in [
        ("b first", membership, identity),
        ("a first", identity, membership),
    ] {
        let dl = pipeline();
        insert(&dl, first).expect("data");
        insert(&dl, second).expect("data");
        let (rows, status) = query(&dl, "SELECT ?x { ?x a :C }");
        assert!(
            rows.iter().all(|r| r == "a" || r == "b"),
            "{order}: sound: {rows:?}"
        );
        assert!(
            !status.is_complete() || rows == ["a", "b"],
            "{order}: a complete answer holds every certain one: {rows:?} {:?}",
            status.paths
        );
        let (rows, status) = query(&dl, "ASK { :a a :C }");
        assert!(
            !status.is_complete() || rows == ["true"],
            "{order}: a is a C in every model: {rows:?} {:?}",
            status.paths
        );
    }
}

/// The same identity with the class C also an individual of A (punning): `a = b = C` in
/// every model, so a is a C, and so is the individual C (it is b). Neither is the smallest
/// id, so C is a member, never the representative, of the class U1 merges.
#[test]
fn a_class_that_is_also_a_member_answers_for_its_members() {
    let identity = ":a a :A . :A rdfs:subClassOf [ a owl:Class ; owl:oneOf ( :b ) ] . \
         :C a owl:Class, owl:NamedIndividual, :A .";
    let membership = ":b a :D . :D rdfs:subClassOf [ a owl:Class ; owl:unionOf ( :E :F ) ] . \
         :E rdfs:subClassOf :C . :F rdfs:subClassOf :C .";
    for (order, first, second) in [
        ("b first", membership, identity),
        ("a first", identity, membership),
    ] {
        let dl = pipeline();
        insert(&dl, first).expect("data");
        insert(&dl, second).expect("data");
        let (rows, status) = query(&dl, "SELECT ?x { ?x a :C }");
        assert!(
            rows.iter().all(|r| ["C", "a", "b"].contains(&r.as_str())),
            "{order}: sound: {rows:?}"
        );
        assert!(
            !status.is_complete() || rows == ["C", "a", "b"],
            "{order}: a complete answer holds every certain one: {rows:?} {:?}",
            status.paths
        );
        let (rows, status) = query(&dl, "ASK { :a a :C }");
        assert!(
            !status.is_complete() || rows == ["true"],
            "{order}: a is a C in every model: {rows:?} {:?}",
            status.paths
        );
    }
}

/// A property p that is also an individual of A (punning), with `A ⊑ {b}`: `a = b = p` in
/// every model, and `b p d`, so `a p d` and `p p d` hold too. p is a member, never the
/// representative, of the class U1 merges.
#[test]
fn a_property_that_is_also_a_member_answers_for_its_members() {
    let fact = ":b :p :d . :p a owl:ObjectProperty .";
    let identity = ":a a :A . :p a owl:NamedIndividual, :A . \
         :A rdfs:subClassOf [ a owl:Class ; owl:oneOf ( :b ) ] .";
    for (order, first, second) in [("b first", fact, identity), ("a first", identity, fact)] {
        let dl = pipeline();
        insert(&dl, first).expect("data");
        insert(&dl, second).expect("data");
        let (rows, status) = query(&dl, "SELECT ?x { ?x :p :d }");
        assert!(
            rows.iter().all(|r| ["a", "b", "p"].contains(&r.as_str())),
            "{order}: sound: {rows:?}"
        );
        assert!(
            !status.is_complete() || rows == ["a", "b", "p"],
            "{order}: a complete answer holds every certain one: {rows:?} {:?}",
            status.paths
        );
        let (rows, status) = query(&dl, "ASK { :a :p :d }");
        assert!(
            !status.is_complete() || rows == ["true"],
            "{order}: a p d holds in every model: {rows:?} {:?}",
            status.paths
        );
    }
}

/// `A ⊑ {b, c}` and `a ∈ A`: a is b or c, neither in every model. U1 may equate a with both
/// (an upper bound); `a p d` holds only where a = b, so it isn't certain, and the answers
/// to `?x p d` are b's alone.
#[test]
fn an_identity_only_the_upper_bound_makes_is_no_answer() {
    let dl = pipeline();
    insert(
        &dl,
        ":b :p :d . :a a :A . :A rdfs:subClassOf [ a owl:Class ; owl:oneOf ( :b :c ) ] .",
    )
    .expect("data");
    let (rows, status) = query(&dl, "SELECT ?x { ?x :p :d }");
    assert_eq!(rows, ["b"], "{:?}", status.reasons());
    let (rows, status) = query(&dl, "ASK { :a :p :d }");
    assert_eq!(rows, ["false"], "{:?}", status.reasons());
}

/// The DL oracle's generated case (`NRESE_FUZZ_SEED=35856`, case 13, family `disjoint`):
/// `C0 ≡ C3 ⊔ ≤0 p0.C3`, and nothing makes a3 a C3 or bars it a p0 successor in C3, so
/// `a3 ∈ C0` isn't entailed; `a2 ∈ C0` is asserted.
#[test]
fn the_generated_disjoint_union_case_answers_as_the_semantics() {
    let dl = pipeline();
    insert(&dl, include_str!("fuzz_35856_13.nt")).expect("data");
    let (rows, status) = query(
        &dl,
        "ASK { <http://example.org/fuzz#a3> a <http://example.org/fuzz#C0> }",
    );
    assert_eq!(rows, ["false"], "{:?}", status.reasons());
    let (rows, status) = query(
        &dl,
        "ASK { <http://example.org/fuzz#a2> a <http://example.org/fuzz#C0> }",
    );
    assert_eq!(rows, ["true"], "{:?}", status.reasons());
}
