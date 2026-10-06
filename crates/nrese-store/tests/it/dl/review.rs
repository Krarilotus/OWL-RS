//! The correctness checks of the review of 6 October: whether `complete` can be trusted.
//!
//! 1. User rules with `owl2-dl` (DL + arbitrary rules is undecidable): the rules run
//!    over the RL closure only, so no answer is claimed complete beside them.
//! 2. (a) U1 stays an upper bound where `⊥` or a disjunct the split derives must block
//!    an answer (PAGOdA's strengthening derives every disjunct; Theorem 5.5 (ii) needs a
//!    consistent ontology, which the status requires). (b) U1's representative Skolem
//!    constants are never taken for named individuals: a match through one is a
//!    candidate the exact services decide, refuted or unresolved, never `complete` by
//!    ground semantics (the trap Igne et al. 2023 §8.2.2 found in PAGOdA).
//! 3. A part of a part of a fleet: RL misses it, the bounds and the tableau find it.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, UserRules};
use nrese_store::{MutationPipeline, SparqlQueryRequest, StoreService};

use super::queries::query;
use super::{PREFIXES, insert, pipeline};

/// The `?x ?z` rows of a query, and its status.
fn pairs(dl: &MutationPipeline, q: &str) -> (Vec<(String, String)>, super::Status) {
    let store = dl.store();
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!("{PREFIXES}{q}")))
        .expect("prepared");
    let mut out = Vec::new();
    let status = super::run_dl(store, &prepared, &mut out)
        .expect("query")
        .expect("a status");
    let json: serde_json::Value = serde_json::from_slice(&out).expect("json");
    let local = |v: &serde_json::Value| {
        v["value"]
            .as_str()
            .unwrap_or("-")
            .trim_start_matches("http://example.com/")
            .to_owned()
    };
    let mut rows: Vec<(String, String)> = json["results"]["bindings"]
        .as_array()
        .expect("bindings")
        .iter()
        .map(|row| (local(&row["x"]), local(&row["z"])))
        .collect();
    rows.sort();
    (rows, status)
}

const UNION: &str = ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B rdfs:subClassOf :D . \
     :C rdfs:subClassOf :D . :x a :A . :y a :B .";

#[test]
fn with_user_rules_no_answer_is_claimed_complete() {
    let rules = UserRules::n3(
        "special.n3",
        "{ ?x a <http://example.com/D> } => { ?x a <http://example.com/Special> } .",
    )
    .expect("rules");
    let config = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .with_rules(Some(Arc::new(rules)))
        .expect("owl2-dl with rules");
    let store = StoreService::new(crate::support::in_memory_store_config()).expect("store");
    let dl = MutationPipeline::new(Arc::new(store), Arc::new(ReasonerService::new(config)));
    assert!(dl.store().dl().user_rules());
    insert(&dl, UNION).expect("data");
    // x is a D under OWL 2 DL only, so the rule never sees it: x a Special is missing
    // from both bounds. The answer is sound (y) and says it may be incomplete.
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :Special }");
    assert_eq!(rows, ["y"]);
    assert!(!status.is_complete());
    assert!(
        status.reasons().iter().any(|r| r.contains("user rules")),
        "{:?}",
        status.reasons()
    );
    // Without rules the same store kind claims what it can prove.
    let plain = pipeline();
    assert!(!plain.store().dl().user_rules());
    insert(&plain, UNION).expect("data");
    assert!(query(&plain, "SELECT ?x { ?x a :D }").1.is_complete());
}

#[test]
fn the_upper_bound_holds_where_bottom_blocks_a_disjunct() {
    let dl = pipeline();
    // x is an A, so a B or a C; it is an E, and no B is: so x is a C, and a D. The
    // strengthening derives both disjuncts (x a B too), and the clash is a fact.
    insert(
        &dl,
        ":A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B owl:disjointWith :E . \
         :C rdfs:subClassOf :D . :x a :A , :E .",
    )
    .expect("consistent");
    let upper = dl.store().dl_upper_facts(false).expect("U1");
    let has = |p: &str, o: &str| {
        upper.iter().any(|t| {
            t[0] == "<http://example.com/x>"
                && t[1] == p
                && t[2] == format!("<http://example.com/{o}>")
        })
    };
    let rdf_type = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#type>";
    assert!(
        has(rdf_type, "C") && has(rdf_type, "D"),
        "the certain answers are in U1"
    );
    assert!(has(rdf_type, "B"), "the disjunct ⊥ blocks is in U1 too");
    // The certain answer comes out, the blocked one is refuted.
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :D }");
    assert_eq!(rows, ["x"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :B }");
    assert!(rows.is_empty());
    assert!(status.is_complete());
    assert_eq!(status.bounds.expect("bounds").refuted, 1);
}

#[test]
fn a_skolem_constant_two_individuals_share_is_no_shared_successor() {
    let dl = pipeline();
    insert(
        &dl,
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; \
           owl:someValuesFrom owl:Thing ] . :a a :A . :b a :A .",
    )
    .expect("data");
    // U1 gives a and b the same r-successor (one constant per clause); in a model they
    // needn't share one. Ground semantics would answer (a, b).
    let (rows, status) = pairs(&dl, "SELECT ?x ?z { ?x :r ?y . ?z :r ?y }");
    assert_eq!(
        rows,
        [
            ("a".to_owned(), "a".to_owned()),
            ("b".to_owned(), "b".to_owned())
        ]
    );
    assert!(status.is_complete(), "{:?}", status.reasons());
    assert_eq!(
        status.bounds.expect("bounds").refuted,
        2,
        "(a, b) and (b, a)"
    );
}

#[test]
fn a_match_only_a_skolem_self_loop_gives_is_not_complete() {
    let dl = pipeline();
    insert(
        &dl,
        ":A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; \
           owl:someValuesFrom :A ] . :a a :A .",
    )
    .expect("data");
    // U1: a r c, and c (an A) r c: a loop no model needs (an infinite chain is one).
    let (rows, status) = query(&dl, "SELECT ?x { ?x :r ?y . ?y :r ?y }");
    assert!(rows.is_empty(), "{rows:?}");
    assert!(!status.is_complete());
    let bounds = status.bounds.expect("bounds");
    assert_eq!((bounds.proved, bounds.unresolved), (0, 1));
    assert!(
        status.reasons()[0].contains("cycle"),
        "{:?}",
        status.reasons()
    );
}

const FLEET: &str = ":Engine rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :partOf ; \
       owl:someValuesFrom :Car ] . \
     :Car rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :partOf ; \
       owl:someValuesFrom :Fleet ] . \
     :partOf a owl:TransitiveProperty . :e1 a :Engine .";

#[test]
fn a_part_of_a_part_of_a_fleet_is_found_through_the_bounds() {
    let q = "SELECT ?x { ?x :partOf ?f . ?f a :Fleet }";
    let dl = pipeline();
    insert(&dl, FLEET).expect("data");
    let (rows, status) = query(&dl, q);
    assert_eq!(rows, ["e1"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
    assert!(status.paths.contains(&"exact-internalisable-cq"));
    // Under owl2-rl the closure misses it, and the answer says it is sound only.
    let store = StoreService::new(crate::support::in_memory_store_config()).expect("store");
    let rl = MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    insert(&rl, FLEET).expect("data");
    let (rows, status) = query(&rl, q);
    assert!(rows.is_empty());
    assert_eq!(status.shared.as_str(), "sound-only");
    // Why: the closure's (`rules`), or the QL rewriting's where it runs over it (`ql`).
    assert!(
        status
            .shared
            .reasons
            .iter()
            .any(|r| r.source == "rules" || r.source == "ql"),
        "{:?}",
        status.shared
    );
}
