//! Step 4: the upper bound U1 in a stack of its own, maintained per commit. The
//! maintained U1 is checked against U1 evaluated afresh after every commit of a random
//! sequence (the design's rule: every incremental path against a clean rebuild), and
//! each performance choice has its guard: an assertion-only commit updates U1 by the
//! delta executor, a schema change rebuilds it, and U1 after an assertion commit proves
//! consistency without a DL engine.

use nrese_owl::fuzz::Rng;

use super::{ask, insert, pipeline, update};

/// A schema with what U1 approximates: existentials (Skolem constants), a union (split),
/// disjointness (clashes), a transitive property, an inverse.
const SCHEMA: &str = ":Person rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :parent ; \
       owl:someValuesFrom :Person ] . \
     :Person owl:equivalentClass [ owl:unionOf ( :Man :Woman ) ] . \
     :Man owl:disjointWith :Woman . \
     :ancestor a owl:TransitiveProperty . :parent rdfs:subPropertyOf :ancestor . \
     :child owl:inverseOf :parent . \
     :Parent owl:equivalentClass [ a owl:Restriction ; owl:onProperty :child ; \
       owl:someValuesFrom owl:Thing ] .";

/// U1's facts with its own terms (Skolem constants, fresh classes) as one name: their
/// numbering follows the clauses', which assertions read with the ontology shift. The
/// count of each of U1's own terms' facts is kept, so a lost or extra one shows.
fn canonical(facts: &[[String; 3]]) -> Vec<([String; 3], usize)> {
    let mut out = std::collections::BTreeMap::new();
    for fact in facts {
        let key = fact.clone().map(|t| match t.starts_with("<urn:nrese:u1:") {
            true => "<urn:nrese:u1:*>".to_owned(),
            false => t,
        });
        *out.entry(key).or_insert(0) += 1;
    }
    out.into_iter().collect()
}

#[test]
fn the_maintained_upper_bound_is_the_one_evaluated_afresh() {
    let classes = ["Person", "Man", "Woman", "Parent"];
    let properties = ["parent", "child", "ancestor"];
    for seed in 0..6u64 {
        let dl = super::pipeline_with(nrese_store::DlConfig {
            consistency: nrese_store::DlConsistency::Off,
            ..nrese_store::DlConfig::default()
        });
        insert(&dl, SCHEMA).expect("schema");
        let mut rng = Rng::new(seed);
        let mut asserted: Vec<String> = Vec::new();
        for step in 0..12 {
            let fact = if rng.one_in(2) {
                format!(":i{} a :{} .", rng.below(5), classes[rng.below(4) as usize])
            } else {
                format!(
                    ":i{} :{} :i{} .",
                    rng.below(5),
                    properties[rng.below(3) as usize],
                    rng.below(5)
                )
            };
            if !asserted.is_empty() && rng.one_in(3) {
                let gone = asserted.remove(rng.below(asserted.len() as u64) as usize);
                update(&dl, &format!("DELETE DATA {{ {gone} }}")).expect("delete");
            } else {
                insert(&dl, &fact).expect("insert");
                asserted.push(fact);
            }
            let maintained = dl.store().dl_upper_facts(false).expect("U1");
            let afresh = dl.store().dl_upper_facts(true).expect("U1 afresh");
            assert_eq!(
                canonical(&maintained),
                canonical(&afresh),
                "seed {seed}, step {step}"
            );
            assert_eq!(
                dl.store().dl_bounds().last,
                "delta",
                "seed {seed}, step {step}"
            );
        }
    }
}

/// What U1 equates (a functional property over an existential, a key) gives it classes,
/// kept by representatives: a commit then evaluates U1 afresh, one without classes keeps
/// it by the delta executor. Either way the U1 a commit leaves is the one evaluated afresh.
/// `NRESE_FUZZ_CASES` sets the seeds (ADR-0011's equality work: sweep about 200).
#[test]
fn the_upper_bound_with_equality_classes_is_the_one_evaluated_afresh() {
    const EQUALITY: &str = ":Student rdfs:subClassOf [ a owl:Restriction ; \
           owl:onProperty :enrollIn ; owl:someValuesFrom :Dept ] . \
         :enrollIn a owl:FunctionalProperty . :Dept owl:hasKey ( :code ) . \
         :A rdfs:subClassOf [ owl:unionOf ( :Dept :Lab ) ] .";
    let seeds = std::env::var("NRESE_FUZZ_CASES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(8u64);
    let (mut with_classes, mut by_delta) = (0, 0);
    for seed in 0..seeds {
        let dl = super::pipeline_with(nrese_store::DlConfig {
            consistency: nrese_store::DlConsistency::Off,
            ..nrese_store::DlConfig::default()
        });
        insert(&dl, EQUALITY).expect("schema");
        let mut rng = Rng::new(seed);
        let mut asserted: Vec<String> = Vec::new();
        for step in 0..10 {
            let (a, b) = (rng.below(5), rng.below(5));
            let fact = match rng.below(4) {
                0 => format!(":i{a} a :Student ."),
                1 => format!(":i{a} :enrollIn :i{b} ."),
                2 => format!(":i{a} :code {} .", rng.below(2)),
                _ => format!(":i{a} a :A ."),
            };
            if !asserted.is_empty() && rng.one_in(3) {
                let gone = asserted.remove(rng.below(asserted.len() as u64) as usize);
                update(&dl, &format!("DELETE DATA {{ {gone} }}")).expect("delete");
            } else {
                insert(&dl, &fact).expect("insert");
                asserted.push(fact);
            }
            let maintained = dl.store().dl_upper_facts(false).expect("U1");
            let afresh = dl.store().dl_upper_facts(true).expect("U1 afresh");
            assert_eq!(
                canonical(&maintained),
                canonical(&afresh),
                "seed {seed}, step {step}"
            );
            match dl.store().dl_bounds().last {
                "delta" => by_delta += 1,
                _ => with_classes += 1,
            }
        }
    }
    // Both ways are taken.
    assert!(
        with_classes > 0 && by_delta > 0,
        "{with_classes} afresh, {by_delta} by delta"
    );
}

/// U1 equates both departments with its one constant for `∃enrollIn.Dept`: a class kept
/// by a representative. Both are departments, as OWL 2 DL has it (each student's only
/// `enrollIn` is a `Dept`), but U1's stack says so of the representative alone: read
/// expanded, U1 bounds both; read as stored, the answer would say complete without one.
/// U1's equality of the two departments is its own (an over-approximation): refuted.
#[test]
fn an_upper_bound_with_equality_classes_is_read_expanded() {
    let dl = pipeline();
    insert(
        &dl,
        ":Student rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :enrollIn ; \
           owl:someValuesFrom :Dept ] . :enrollIn a owl:FunctionalProperty . \
         :s1 a :Student ; :enrollIn :d1 . :s2 a :Student ; :enrollIn :d2 .",
    )
    .expect("data");
    let (rows, status) = super::queries::query(&dl, "SELECT ?x { ?x a :Dept }");
    assert_eq!(rows, ["d1", "d2"], "{:?}", status.reasons());
    assert!(status.is_complete(), "{:?}", status.reasons());
    let (rows, status) = super::queries::query(&dl, "SELECT ?x { :d1 owl:sameAs ?x }");
    assert!(
        !rows.contains(&"d2".to_owned()),
        "U1's equality is not an answer: {rows:?}"
    );
    assert!(status.is_complete(), "{:?}", status.reasons());
}

#[test]
fn the_upper_bound_is_never_visible_as_inferred_data() {
    let dl = pipeline();
    insert(&dl, &format!("{SCHEMA} :ann a :Person .")).expect("data");
    let report = dl.store().dl_bounds();
    assert!(report.unavailable.is_none(), "{report:?}");
    assert!(report.upper_facts > 0, "U1 has facts beyond L: {report:?}");
    // ann's parent is a Skolem constant in U1, never an answer and never a statement.
    let result = dl
        .store()
        .execute_query_str("SELECT * { ?s ?p ?o }")
        .expect("query");
    let text = String::from_utf8(result.payload).expect("utf8");
    assert!(!text.contains("urn:nrese:u1:"), "{text}");
    // That ann has a parent is certain; who isn't.
    assert!(ask(&dl, ":ann :parent ?someone"));
    let result = dl
        .store()
        .execute_query_str(&format!(
            "{}SELECT ?p {{ :ann :parent ?p }}",
            super::PREFIXES
        ))
        .expect("query");
    let text = String::from_utf8(result.payload).expect("utf8");
    assert!(!text.contains("\"value\""), "{text}");
}

#[test]
fn assertion_commits_update_the_bound_by_delta_and_schema_commits_rebuild_it() {
    let dl = pipeline();
    insert(&dl, SCHEMA).expect("schema");
    assert_eq!(dl.store().dl_bounds().last, "rebuilt");
    // Not a Person (whom U1's split union would make a Man and a Woman, a clash).
    insert(&dl, ":ann :parent :bob .").expect("an assertion");
    assert_eq!(dl.store().dl_bounds().last, "delta");
    // U1 proves the data consistent: no DL engine ran for the commit.
    let status = dl.store().dl().status().expect("status");
    assert_eq!(status.consistency.engine, "upper-bound");
    assert_eq!(status.consistency.verdict.as_str(), "consistent");
    insert(&dl, ":Parent rdfs:subClassOf :Human .").expect("a schema change");
    assert_eq!(dl.store().dl_bounds().last, "rebuilt");
    // A class U1 wasn't compiled for: rebuilt as well.
    insert(&dl, ":bob a :Martian .").expect("new vocabulary");
    assert_eq!(dl.store().dl_bounds().last, "rebuilt");
}

#[test]
fn a_clash_in_the_upper_bound_sends_the_check_to_a_dl_engine() {
    let dl = pipeline();
    insert(&dl, SCHEMA).expect("schema");
    // ann is a Person: U1 splits the union, making her a Man and a Woman, a clash; only
    // the hypertableau can tell that a model exists.
    insert(&dl, ":ann a :Person .").expect("consistent");
    let status = dl.store().dl().status().expect("status");
    assert_eq!(status.consistency.engine, "hypertableau");
    assert_eq!(status.consistency.verdict.as_str(), "consistent");
}

/// A key is a rule of U1 (over every term: more equalities, still an upper bound), so an
/// ontology with one keeps U1: it bounds the answers and proves consistency on commit,
/// where the key's axiom counted as uncovered sent every commit to the hypertableau.
#[test]
fn a_key_keeps_the_upper_bound() {
    let dl = pipeline();
    insert(
        &dl,
        ":D owl:hasKey ( :k ) . :A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . \
         :d1 a :D ; :k 7 . :d2 a :D ; :k 7 . :d1 a :E .",
    )
    .expect("data");
    let bounds = dl.store().dl_bounds();
    assert_eq!(bounds.unavailable, None, "{bounds:?}");
    insert(&dl, ":d3 a :A .").expect("an assertion");
    let status = dl.store().dl().status().expect("status");
    assert_eq!(status.consistency.engine, "upper-bound");
    // The key equates d1 and d2: d2 is an E.
    let (rows, status) = super::queries::query(&dl, "SELECT ?x { ?x a :E }");
    assert_eq!(rows, ["d1", "d2"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
}

/// U1 compares data values by value, OWL 2's identity of data values: a key over one
/// value written two ways (`"07"` and `"7"` as `xsd:integer`) equates its individuals,
/// and a `hasValue` of `7.0` as `xsd:decimal` holds for `7` as `xsd:integer`. Compared by
/// term, U1 would miss both, and the answers without them would say they are complete.
#[test]
fn data_values_are_compared_by_value() {
    let dl = pipeline();
    insert(
        &dl,
        ":D owl:hasKey ( :k ) . :A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . \
         :d1 a :D ; :k \"07\"^^xsd:integer . :d2 a :D ; :k \"7\"^^xsd:integer . :d1 a :E . \
         :Seven owl:equivalentClass [ a owl:Restriction ; owl:onProperty :age ; \
           owl:hasValue \"7.0\"^^xsd:decimal ] . \
         :p :age \"7\"^^xsd:integer ; :name \"Seven\" , \"7\" .",
    )
    .expect("data");
    // Only the literals the rules compare are read by value (the key's and the
    // `hasValue`'s: "07", "7" and 7.0), never a name nothing compares (LUBM's names,
    // e-mail addresses and telephone numbers: 107,410 literals at LUBM(10)).
    assert_eq!(dl.store().dl_bounds().literals_by_value, 3);
    for (q, want) in [
        ("SELECT ?x { ?x a :E }", vec!["d1", "d2"]),
        ("SELECT ?x { ?x a :Seven }", vec!["p"]),
    ] {
        let (rows, status) = super::queries::query(&dl, q);
        assert!(
            rows == want || !status.is_complete(),
            "{q}: {rows:?} said complete without what the equal values entail"
        );
        assert_eq!(rows, want, "{q}: {:?}", status.reasons());
        assert!(status.is_complete(), "{q}: {:?}", status.reasons());
    }
}
