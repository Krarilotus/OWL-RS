//! A chain into a role that holds its pairs both ways: a symmetric role, or one made its own
//! inverse. By *OWL 2 Direct Semantics* §2.3.2 (Table 6), `p q ⊑ r` puts each pair the
//! chain composes into `r`, and a symmetric `r` holds each of its pairs reversed too; so the
//! chain's pair `(x, z)` makes `(z, x)` an `r` pair, though no `r` edge is stated. Each case
//! reaches `C` through `r`'s range (Table 6) at an end only one direction gives, and the
//! read, the entailment of each candidate and the consistency check must all see it.
//!
//! The chain's first link, `p(x, b)`, comes from `x : ∃p.{b}` (§2.2.3, Table 4:
//! `ObjectSomeValuesFrom` over `ObjectOneOf`), which the OWL 2 RL rules can't turn into an
//! edge (the last test).

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::dl::Verdict;
use nrese_store::dl::entailment::Entailed;
use nrese_store::{MutationError, MutationPipeline, SparqlQueryRequest, StoreService};

use super::{PREFIXES, Status, insert, pipeline};

const DECLARED: &str = ":p a owl:ObjectProperty . :q a owl:ObjectProperty . \
     :t a owl:ObjectProperty . :r a owl:ObjectProperty . :s a owl:ObjectProperty . \
     :C a owl:Class . :D a owl:Class . :x a owl:NamedIndividual . \
     :b a owl:NamedIndividual . :c a owl:NamedIndividual . :z a owl:NamedIndividual .";

/// `x : ∃p.{b}`, so `p(x, b)` in every model; and `q(b, z)`.
const PATH: &str = ":x a [ a owl:Restriction ; owl:onProperty :p ; \
       owl:someValuesFrom [ a owl:Class ; owl:oneOf ( :b ) ] ] . :b :q :z .";

const CHAIN: &str = ":r owl:propertyChainAxiom ( :p :q ) .";
const SYMMETRIC: &str = ":r a owl:SymmetricProperty .";
const RANGE: &str = ":r rdfs:range :C .";

/// `C` and `D` disjoint, and `x` a `D`: inconsistent wherever `x` is a `C`.
const CLASH_AT_X: &str = ":C owl:disjointWith :D . :x a :D .";

fn premise(parts: &[&str]) -> String {
    let mut text = DECLARED.to_owned();
    for part in parts {
        text.push(' ');
        text.push_str(part);
    }
    text
}

/// The rows of `SELECT ?x { ?x a :C }` (local names, sorted) and their status, if any.
fn members_of_c(store_pipeline: &MutationPipeline) -> (Vec<String>, Option<Status>) {
    let store = store_pipeline.store();
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!(
            "{PREFIXES}SELECT ?x {{ ?x a :C }}"
        )))
        .expect("prepare");
    let mut out = Vec::new();
    let status = super::run_dl(store, &prepared, &mut out).expect("run");
    let json: serde_json::Value = serde_json::from_slice(&out).expect("json results");
    let mut rows: Vec<String> = json["results"]["bindings"]
        .as_array()
        .expect("bindings")
        .iter()
        .map(|row| {
            row["x"]["value"]
                .as_str()
                .unwrap_or("-")
                .trim_start_matches("http://example.com/")
                .to_owned()
        })
        .collect();
    rows.sort();
    (rows, status)
}

/// Commits `premise` under `owl2-dl` and holds its read of `C` to `entailed` (the
/// individuals among `x` and `z` that are a `C` in every model, sorted): the rows, each
/// candidate's entailment (`Yes` for a member, `No` for the other), a `complete` status,
/// and the store told consistent. Every observation is printed before the first check.
fn holds(premise: &str, entailed: &[&str]) {
    let dl = pipeline();
    insert(&dl, premise).expect("a consistent premise is committed");
    let (rows, status) = members_of_c(&dl);
    let status = status.expect("a status under owl2-dl");
    let each: Vec<(&str, Entailed)> = ["x", "z"]
        .into_iter()
        .map(|m| {
            (
                m,
                super::entailment::entailed(premise, &format!(":{m} a :C .")),
            )
        })
        .collect();
    let verdict = dl
        .store()
        .dl()
        .status()
        .expect("a DL status")
        .consistency
        .verdict;
    println!(
        "rows {rows:?}, entailed {entailed:?}; told complete {}, sound {}, paths {:?}, \
         reasons {:?}; each candidate {each:?}; consistency {verdict:?}",
        status.shared.complete,
        status.shared.sound,
        status.paths,
        status.reasons()
    );
    assert_eq!(rows, entailed, "the read's rows");
    for (m, answer) in &each {
        let expected = if entailed.contains(m) {
            Entailed::Yes
        } else {
            Entailed::No
        };
        assert_eq!(*answer, expected, "{m} a C, per candidate");
    }
    assert!(status.is_complete(), "{:?}", status.reasons());
    assert_eq!(verdict, Verdict::Consistent);
}

/// Spec: *OWL 2 Direct Semantics* §2.2.3 (Table 4) and §2.3.2 (Table 6): `p(x, b)` from
/// `x : ∃p.{b}`; `r(x, z)` from the chain `p q ⊑ r` with `q(b, z)`; `r(z, x)` from `r`
/// symmetric; so `C(z)` and `C(x)` from `r`'s range `C`. The read returns x and z, complete;
/// each is entailed; the premise is consistent.
#[test]
fn a_chain_into_a_symmetric_role_gives_both_ends_its_range() {
    holds(&premise(&[SYMMETRIC, CHAIN, RANGE, PATH]), &["x", "z"]);
}

/// Spec: Table 6: `InverseObjectProperties(r r)` holds `r` equal to its inverse, so
/// `r(x, z)` (the chain) gives `r(z, x)`, and the range gives `C(x)` and `C(z)`.
#[test]
fn a_chain_into_a_role_declared_its_own_inverse_gives_both_ends_its_range() {
    holds(
        &premise(&[":r owl:inverseOf :r .", CHAIN, RANGE, PATH]),
        &["x", "z"],
    );
}

/// Spec: Table 6: `EquivalentObjectProperties(r ObjectInverseOf(r))` holds `r` equal to its
/// inverse (Table 1), so `r(x, z)` (the chain) gives `r(z, x)`, and the range `C(x)`, `C(z)`.
#[test]
fn a_chain_into_a_role_equivalent_to_its_inverse_gives_both_ends_its_range() {
    holds(
        &premise(&[
            ":r owl:equivalentProperty [ owl:inverseOf :r ] .",
            CHAIN,
            RANGE,
            PATH,
        ]),
        &["x", "z"],
    );
}

/// Spec: Tables 1 and 6: `p q ⊑ r⁻` gives `r⁻(x, z)`, that is `r(z, x)`, so `C(x)` by the
/// range; `r` symmetric gives `r(x, z)` too, so `C(z)`.
#[test]
fn a_chain_into_the_inverse_of_a_symmetric_role_gives_both_ends_its_range() {
    holds(
        &premise(&[
            SYMMETRIC,
            "[ owl:inverseOf :r ] owl:propertyChainAxiom ( :p :q ) .",
            RANGE,
            PATH,
        ]),
        &["x", "z"],
    );
}

/// Spec: Tables 1 and 6: `p q ⊑ r⁻` gives `r(z, x)` alone, so `C(x)` by the range; nothing
/// gives `z` an `r`-predecessor, so a model has `z` outside `C`.
#[test]
fn a_chain_into_the_inverse_of_a_role_gives_its_first_end_the_range() {
    holds(
        &premise(&[
            "[ owl:inverseOf :r ] owl:propertyChainAxiom ( :p :q ) .",
            RANGE,
            PATH,
        ]),
        &["x"],
    );
}

/// Spec: Table 6: the chain `p q t ⊑ r` over `p(x, b)`, `q(b, c)`, `t(c, z)` gives
/// `r(x, z)`; `r` symmetric gives `r(z, x)`; the range gives `C(x)` and `C(z)`.
#[test]
fn a_chain_of_three_into_a_symmetric_role_gives_both_ends_its_range() {
    let path = ":x a [ a owl:Restriction ; owl:onProperty :p ; \
           owl:someValuesFrom [ a owl:Class ; owl:oneOf ( :b ) ] ] . :b :q :c . :c :t :z .";
    holds(
        &premise(&[
            SYMMETRIC,
            ":r owl:propertyChainAxiom ( :p :q :t ) .",
            RANGE,
            path,
        ]),
        &["x", "z"],
    );
}

/// Spec: Table 6: the chain gives `r(x, z)`; `r` symmetric gives `r(z, x)` (and, `r` being
/// transitive, `r(x, x)` and `r(z, z)`); the range gives `C(x)` and `C(z)`.
#[test]
fn a_chain_into_a_transitive_symmetric_role_gives_both_ends_its_range() {
    holds(
        &premise(&[
            ":r a owl:SymmetricProperty , owl:TransitiveProperty .",
            CHAIN,
            RANGE,
            PATH,
        ]),
        &["x", "z"],
    );
}

/// Spec: Tables 1 and 6: `r ≡ s⁻` and the chain `p q ⊑ s` give `s(x, z)`, so `r(z, x)`
/// and `C(x)` by `r`'s range; nothing gives `r(x, z)`, so a model has `z` outside `C`.
#[test]
fn a_chain_into_a_role_whose_inverse_has_the_range_gives_its_first_end_the_range() {
    holds(
        &premise(&[
            ":r owl:equivalentProperty [ owl:inverseOf :s ] .",
            ":s owl:propertyChainAxiom ( :p :q ) .",
            RANGE,
            PATH,
        ]),
        &["x"],
    );
}

/// Spec: Tables 1 and 6: as the case above, `r(z, x)`; `s` symmetric gives `s(z, x)` too,
/// so `r(x, z)` (`r ≡ s⁻`), and the range gives `C(z)` as well.
#[test]
fn a_chain_into_a_symmetric_role_whose_inverse_has_the_range_gives_both_ends_the_range() {
    holds(
        &premise(&[
            ":r owl:equivalentProperty [ owl:inverseOf :s ] .",
            ":s a owl:SymmetricProperty .",
            ":s owl:propertyChainAxiom ( :p :q ) .",
            RANGE,
            PATH,
        ]),
        &["x", "z"],
    );
}

/// Spec: *OWL 2 Direct Semantics* §2.3.1 (Table 5) and §2.4: with `r` symmetric, `C(x)`
/// holds in every model (the first test), so `C` disjoint from `D` and `x` a `D` leave
/// none: the commit is rejected as inconsistent.
#[test]
fn a_disjointness_only_the_reversed_chain_reaches_is_inconsistent() {
    let dl = pipeline();
    let result = insert(&dl, &premise(&[SYMMETRIC, CHAIN, RANGE, PATH, CLASH_AT_X]));
    println!("the commit: {result:?}");
    match result {
        Err(MutationError::Rejected(reject)) => {
            let explanation = reject.explanation.expect("an explanation");
            assert_eq!(explanation.violated_constraint, "owl2-dl-consistency");
        }
        other => panic!("an inconsistent commit is rejected, not {other:?}"),
    }
}

/// Spec: Table 6: without symmetry the chain gives `r(x, z)` alone, so `C(z)`; the
/// interpretation with `r = {(x, z)}` and `C = {z}` is a model, so `x` isn't a `C` and
/// `x` a `D` with `C` disjoint from `D` is consistent.
#[test]
fn without_symmetry_a_chain_gives_only_its_far_end_the_range() {
    holds(&premise(&[CHAIN, RANGE, PATH]), &["z"]);
    let dl = pipeline();
    insert(&dl, &premise(&[CHAIN, RANGE, PATH, CLASH_AT_X]))
        .expect("a consistent premise is committed");
}

fn rl_pipeline() -> MutationPipeline {
    let store = StoreService::new(crate::support::in_memory_store_config()).expect("store");
    MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

/// What the rules' route gives the same data under `owl2-rl`.
///
/// Spec: *OWL 2 Profiles* §4.3: prp-spo2, prp-symp and prp-rng (Table 5) are rules, but no
/// rule gives an edge from `x : ∃p.{b}` (cls-svf1 and cls-svf2, Table 6, go from an edge to
/// the class), and `ObjectSomeValuesFrom` is no superclass expression of OWL 2 RL (§4.2.3),
/// so Theorem PR1 doesn't apply: the closure has no `C` member, and the answer, missing
/// the entailed x and z, is not told complete. With `p(x, b)` stated, the rules give
/// `r(x, z)`, `r(z, x)` and both ends `C`.
#[test]
fn under_owl2_rl_the_rules_give_the_range_only_from_a_stated_first_link() {
    let rl = rl_pipeline();
    insert(&rl, &premise(&[SYMMETRIC, CHAIN, RANGE, PATH])).expect("data");
    let (rows, status) = members_of_c(&rl);
    println!(
        "existential: rows {rows:?}; status {:?}",
        status.as_ref().map(|s| &s.shared)
    );
    assert!(rows.is_empty(), "{rows:?}");
    assert!(
        status.as_ref().is_some_and(|s| !s.is_complete()),
        "an answer missing entailed rows is not told complete: {:?}",
        status.as_ref().map(|s| &s.shared)
    );

    let rl = rl_pipeline();
    insert(
        &rl,
        &premise(&[SYMMETRIC, CHAIN, RANGE, ":x :p :b . :b :q :z ."]),
    )
    .expect("data");
    let (rows, status) = members_of_c(&rl);
    println!(
        "stated: rows {rows:?}; status {:?}",
        status.as_ref().map(|s| &s.shared)
    );
    assert_eq!(rows, ["x", "z"]);
}
