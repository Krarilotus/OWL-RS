//! Step 9: routing by profile. The per-axiom profile checker (`nrese_owl::profile`, held
//! against the W3C test cases' annotations in nrese-owl) decides the route: an OWL 2 RL
//! ontology goes to the RL rules alone (complete for it, theorem PR1: no U1, no DL
//! engine), Horn and EL ontologies to the context core, the rest to the hypertableau.
//! Each test names the route that must be taken (the performance choice's guard).

use super::queries::query;
use super::{insert, pipeline};

#[test]
fn an_rl_ontology_is_decided_by_the_rules_alone() {
    let dl = pipeline();
    insert(
        &dl,
        ":Dog rdfs:subClassOf :Animal . :Animal owl:disjointWith :Plant . \
         :owns rdfs:range :Pet . :ann :owns :rex . :rex a :Dog .",
    )
    .expect("an RL ontology");
    let bounds = dl.store().dl_bounds();
    assert_eq!(bounds.last, "rules");
    assert_eq!(bounds.upper_facts, 0, "no U1 compiled");
    let status = dl.store().dl().status().expect("status");
    assert_eq!(status.consistency.engine, "rules");
    assert_eq!(status.consistency.verdict.as_str(), "consistent");
    // Every predicate is closed: one evaluation, complete, negation included.
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :Animal }");
    assert_eq!(rows, ["rex"]);
    assert!(status.is_complete());
    assert_eq!(status.paths, ["closed-predicates"]);
    let (rows, status) = query(
        &dl,
        "SELECT ?x { ?x a :Pet FILTER NOT EXISTS { ?x a :Plant } }",
    );
    assert_eq!(rows, ["rex"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
    // An assertion keeps the route; an RL violation is still rejected by the RL gate.
    insert(&dl, ":tom a :Dog .").expect("an assertion");
    assert_eq!(dl.store().dl_bounds().last, "rules");
    assert!(insert(&dl, ":tom a :Plant .").is_err());
}

/// OWL 2 RL's eq-ref (OWL 2 Profiles §4.3, table 4) makes every term `owl:sameAs` itself;
/// the RL rules leave it unmaterialised by design (RSH-04), so a read that can bind both
/// ends of `owl:sameAs` to one term misses its reflexive answers on the RL route: never
/// labelled complete, and refused under `exact`. Two distinct constants are decided.
#[test]
fn reflexive_same_as_reads_are_not_complete_on_the_rules_route() {
    use nrese_store::{DlAnswers, StoreError};
    let dl = pipeline();
    insert(
        &dl,
        ":Dog rdfs:subClassOf :Animal . :rex a :Dog . :ann :owns :rex .",
    )
    .expect("an RL ontology");
    assert_eq!(dl.store().dl_bounds().last, "rules");
    // Two distinct constants are decided; the one rule (default deny) withholds
    // `complete` from the reserved read: an over-mark.
    let (rows, status) = query(&dl, "ASK { :rex owl:sameAs :ann }");
    assert_eq!(rows, ["false"]);
    assert!(!status.is_complete(), "{:?}", status.reasons());
    // Every read is checked and the wrong ones reported together.
    let mut wrong = Vec::new();
    for read in [
        "ASK { :rex owl:sameAs :rex }",
        "SELECT ?x { ?x owl:sameAs :rex }",
        "SELECT ?x { :ann :owns/owl:sameAs ?x }",
    ] {
        let (_, status) = query(&dl, read);
        if status.is_complete() {
            wrong.push(format!("{read}: complete"));
        }
        let exact = super::queries::query_with(&dl, read, Some(DlAnswers::Exact));
        if !matches!(exact, Err(StoreError::Incomplete(_))) {
            wrong.push(format!("{read}: exact gave {exact:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Under negation a missing reflexive pair keeps a row the certain answers lack: `?x
/// owl:sameAs ?x` holds of `:rex` (eq-ref, OWL 2 Profiles table 4), so NOT EXISTS removes
/// it (SPARQL 1.1 §17.4.1.4) and the certain answers are none. Answered over the lower
/// bound, `:rex` is returned: never labelled sound, in the plan or any answer mode, and
/// refused under `exact` (P2-06 review r1, F1). The label holds whatever decides first: a
/// schema read beside it (review r2), or a negated property set, which reads `owl:sameAs`
/// unless it excludes it.
#[test]
fn a_reflexive_same_as_read_under_negation_is_not_sound() {
    use nrese_store::{CancellationToken, DlAnswers, SparqlQueryRequest, StoreError};
    let dl = pipeline();
    insert(&dl, ":Dog rdfs:subClassOf :Animal . :rex a :Dog .").expect("an RL ontology");
    assert_eq!(dl.store().dl_bounds().last, "rules");
    let mut wrong = Vec::new();
    // Guards (hold on the base too): a monotone read misses answers only, so it stays sound.
    for monotone in [
        "SELECT ?x { ?x owl:sameAs :rex }",
        "SELECT ?x { ?x !:owns :rex }",
        "SELECT ?x { :Dog rdfs:subClassOf :Animal . ?x owl:sameAs :rex }",
    ] {
        let (rows, status) = query(&dl, monotone);
        if !rows.is_empty() || !status.shared.sound || status.is_complete() {
            wrong.push(format!("guard {monotone}: {rows:?} {:?}", status.shared));
        }
    }
    for negated in [
        "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x owl:sameAs ?x } }",
        "SELECT ?x { :Dog rdfs:subClassOf :Animal . ?x a :Dog \
         FILTER NOT EXISTS { ?x owl:sameAs ?x } }",
        "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x !:owns ?x } }",
    ] {
        for mode in [
            None,
            Some(DlAnswers::CertainWhereComplete),
            Some(DlAnswers::Sound),
        ] {
            let (rows, status) = super::queries::query_with(&dl, negated, mode).expect("answered");
            // The lower bound's row, which the certain answers lack: answered, not certified.
            if rows != ["rex"] {
                wrong.push(format!("{negated} {mode:?}: rows {rows:?}"));
            }
            if status.shared.sound || status.is_complete() {
                wrong.push(format!("{negated} {mode:?}: {:?}", status.shared));
            }
        }
        let store = dl.store();
        let prepared = store
            .prepare_query(&SparqlQueryRequest::all(format!(
                "{}{negated}",
                super::PREFIXES
            )))
            .expect("prepared");
        let planned = store
            .plan_query(&prepared, &CancellationToken::new())
            .expect("plan")
            .completeness
            .expect("a status");
        if planned.sound || planned.complete {
            wrong.push(format!("{negated} plan: {planned:?}"));
        }
        let exact = super::queries::query_with(&dl, negated, Some(DlAnswers::Exact));
        if !matches!(exact, Err(StoreError::Incomplete(_))) {
            wrong.push(format!("{negated} exact: {exact:?}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn an_axiom_outside_rl_takes_the_ontology_off_the_rules_route() {
    let dl = pipeline();
    insert(&dl, ":Dog rdfs:subClassOf :Animal . :rex a :Dog .").expect("RL");
    assert_eq!(dl.store().dl_bounds().last, "rules");
    // A union on the right of a subclass axiom is outside RL: U1 and the DL engines.
    insert(
        &dl,
        ":Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] . :Cat rdfs:subClassOf :Animal . \
         :tib a :Pet .",
    )
    .expect("not RL");
    assert_eq!(dl.store().dl_bounds().last, "rebuilt");
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :Animal }");
    assert_eq!(rows, ["rex", "tib"], "tib through the union");
    assert!(status.is_complete(), "{:?}", status.reasons());
    // Back inside RL: the rules again.
    super::update(
        &dl,
        "DELETE WHERE { :Pet rdfs:subClassOf ?u . ?u owl:unionOf ?l . ?l ?p ?o }",
    )
    .expect("the union removed");
    super::update(
        &dl,
        "DELETE WHERE { ?l <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> ?f ; \
           <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest> ?r }",
    )
    .expect("its list removed");
    assert_eq!(dl.store().dl_bounds().last, "rules");
}

/// One data value in two lexical forms: `"1"` and `"01"` as xsd:integer, and `"1"` as
/// xsd:int and as xsd:integer (OWL 2 Structural Specification §4: xsd:int's value space
/// is part of xsd:integer's). The rule set leaves out dt-eq, so where an axiom compares
/// data values it sees two: such an ontology leaves the RL route (`routed_to_rules`), and
/// the DL path decides it by value.
const ONE_VALUE: [(&str, &str); 2] = [
    ("\"1\"^^xsd:integer", "\"01\"^^xsd:integer"),
    ("\"1\"^^xsd:int", "\"1\"^^xsd:integer"),
];

/// A union on the right of a subclass axiom: outside OWL 2 RL, so beside any data it
/// takes the ontology off the RL route to the bounds and the DL engines.
const OUTSIDE_RL: &str = ":Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] . :tib a :Pet .";

/// Each of `data` committed (with [`OUTSIDE_RL`] where `outside`), and each of `reads`,
/// all entailed by it, asked: an answer `false` labelled complete is wrong. The wrong
/// ones, as lines; every answer is printed with the route.
fn missed(data: &[String], outside: bool, reads: &[&str]) -> Vec<String> {
    let mut wrong = Vec::new();
    for data in data {
        let dl = pipeline();
        let extra = if outside { OUTSIDE_RL } else { "" };
        insert(&dl, &format!("{data} {extra}")).expect("consistent");
        let route = dl.store().dl_bounds().last;
        for read in reads {
            let (rows, status) = query(&dl, read);
            let line = format!(
                "{data} / route {route}: {read} answers {rows:?}, complete {}",
                status.is_complete()
            );
            eprintln!("{line}");
            if rows == ["false"] && status.is_complete() {
                wrong.push(line);
            }
        }
    }
    wrong
}

/// Each of `data`, inconsistent, committed (with [`OUTSIDE_RL`] where `outside`): it must
/// be rejected. The ones committed, as lines; every outcome is printed.
fn accepted(data: &[String], outside: bool) -> Vec<String> {
    let mut wrong = Vec::new();
    for data in data {
        let dl = pipeline();
        let extra = if outside { OUTSIDE_RL } else { "" };
        let result = insert(&dl, &format!("{data} {extra}"));
        let line = match &result {
            Ok(_) => format!("{data} / committed, route {}", dl.store().dl_bounds().last),
            Err(e) => format!("{data} / rejected: {e}"),
        };
        eprintln!("{line}");
        if result.is_ok() {
            wrong.push(line);
        }
    }
    wrong
}

/// prp-key: a key equates the named individuals that agree on its values (OWL 2 Direct
/// Semantics, HasKey), so `:a = :b` and `:b :p :x` is entailed from `:a :p :x`.
fn key() -> Vec<String> {
    ONE_VALUE
        .map(|(left, right)| {
            format!(
                ":Person owl:hasKey ( :ssn ) . :ssn a owl:DatatypeProperty . \
                 :p a owl:ObjectProperty . :a a :Person ; :ssn {left} ; :p :x . \
                 :b a :Person ; :ssn {right} ."
            )
        })
        .to_vec()
}

const KEY_READS: [&str; 2] = ["ASK { :b :p :x }", "ASK { :a owl:sameAs :b }"];

#[test]
fn a_key_on_one_value_in_two_lexical_forms_equates_in_rl() {
    let wrong = missed(&key(), false, &KEY_READS);
    assert!(wrong.is_empty(), "complete and wrong: {wrong:#?}");
}

#[test]
fn a_key_on_one_value_in_two_lexical_forms_equates_outside_rl() {
    let wrong = missed(&key(), true, &KEY_READS);
    assert!(wrong.is_empty(), "complete and wrong: {wrong:#?}");
}

/// cls-hv2: an individual with the value is in the hasValue class (OWL 2 Direct
/// Semantics, DataHasValue), in a subclass axiom's left side or an equivalence.
fn has_value() -> Vec<String> {
    ONE_VALUE
        .map(|(left, right)| {
            format!(
                ":age a owl:DatatypeProperty . \
                 [ a owl:Restriction ; owl:onProperty :age ; owl:hasValue {left} ] \
                   rdfs:subClassOf :One . \
                 :Adult owl:equivalentClass \
                   [ a owl:Restriction ; owl:onProperty :age ; owl:hasValue {left} ] . \
                 :a :age {right} ."
            )
        })
        .to_vec()
}

const HAS_VALUE_READS: [&str; 2] = ["ASK { :a a :One }", "ASK { :a a :Adult }"];

#[test]
fn a_data_has_value_class_holds_one_value_in_two_lexical_forms_in_rl() {
    let wrong = missed(&has_value(), false, &HAS_VALUE_READS);
    assert!(wrong.is_empty(), "complete and wrong: {wrong:#?}");
}

#[test]
fn a_data_has_value_class_holds_one_value_in_two_lexical_forms_outside_rl() {
    let wrong = missed(&has_value(), true, &HAS_VALUE_READS);
    assert!(wrong.is_empty(), "complete and wrong: {wrong:#?}");
}

/// prp-npa2: a negative data property assertion denies the value in every lexical form.
fn negative_assertion() -> Vec<String> {
    ONE_VALUE
        .map(|(left, right)| {
            format!(
                ":age a owl:DatatypeProperty . \
                 [ a owl:NegativePropertyAssertion ; owl:sourceIndividual :a ; \
                   owl:assertionProperty :age ; owl:targetValue {left} ] . \
                 :a :age {right} ."
            )
        })
        .to_vec()
}

#[test]
fn a_negative_data_assertion_denies_one_value_in_two_lexical_forms_in_rl() {
    let wrong = accepted(&negative_assertion(), false);
    assert!(wrong.is_empty(), "inconsistent and committed: {wrong:#?}");
}

#[test]
fn a_negative_data_assertion_denies_one_value_in_two_lexical_forms_outside_rl() {
    let wrong = accepted(&negative_assertion(), true);
    assert!(wrong.is_empty(), "inconsistent and committed: {wrong:#?}");
}

/// prp-pdw and prp-adp: disjoint data properties share no pair, whatever the value's form.
fn disjoint_data_properties() -> Vec<String> {
    let mut data = Vec::new();
    for (left, right) in ONE_VALUE {
        data.push(format!(
            ":p1 a owl:DatatypeProperty ; owl:propertyDisjointWith :p2 . \
             :p2 a owl:DatatypeProperty . :a :p1 {left} ; :p2 {right} ."
        ));
        data.push(format!(
            ":q1 a owl:DatatypeProperty . :q2 a owl:DatatypeProperty . \
             :q3 a owl:DatatypeProperty . \
             [ a owl:AllDisjointProperties ; owl:members ( :q1 :q2 :q3 ) ] . \
             :a :q1 {left} ; :q3 {right} ."
        ));
    }
    data
}

#[test]
fn disjoint_data_properties_share_no_value_in_two_lexical_forms_in_rl() {
    let wrong = accepted(&disjoint_data_properties(), false);
    assert!(wrong.is_empty(), "inconsistent and committed: {wrong:#?}");
}

#[test]
fn disjoint_data_properties_share_no_value_in_two_lexical_forms_outside_rl() {
    let wrong = accepted(&disjoint_data_properties(), true);
    assert!(wrong.is_empty(), "inconsistent and committed: {wrong:#?}");
}

/// A maximum qualified data cardinality (OWL 2 Direct Semantics, DataMaxCardinality with a
/// data range, `owl:onDataRange`): at most 0, or at most 1, integers of `:age`, and `:c`
/// has two. The rule set reads `owl:onClass` (cls-maxqc1-4), not `owl:onDataRange`, and
/// checking the range would type the values (dt-type2, which it leaves out).
fn qualified_data_cardinality() -> Vec<String> {
    [
        "\"0\"^^xsd:nonNegativeInteger",
        "\"1\"^^xsd:nonNegativeInteger",
    ]
    .map(|n| {
        format!(
            ":age a owl:DatatypeProperty . \
                 :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :age ; \
                   owl:maxQualifiedCardinality {n} ; owl:onDataRange xsd:integer ] . \
                 :c a :C ; :age 5 , 6 ."
        )
    })
    .to_vec()
}

#[test]
fn a_qualified_data_cardinality_is_kept_in_rl() {
    let wrong = accepted(&qualified_data_cardinality(), false);
    assert!(wrong.is_empty(), "inconsistent and committed: {wrong:#?}");
}

#[test]
fn a_qualified_data_cardinality_is_kept_outside_rl() {
    let wrong = accepted(&qualified_data_cardinality(), true);
    assert!(wrong.is_empty(), "inconsistent and committed: {wrong:#?}");
}

/// cls-maxc2 and cls-maxqc3 name their cardinality as the term `"1"^^xsd:nonNegativeInteger`;
/// the ontology's reader takes the number in any form, so `owl:maxCardinality 1` (Turtle's
/// xsd:integer), `"01"^^xsd:nonNegativeInteger` or a qualified `1` is in RL. At most one
/// `:p` equates `:x` and `:y`, so `:y :q :z` is entailed.
fn max_cardinality() -> Vec<String> {
    [
        "owl:maxCardinality 1",
        "owl:maxCardinality \"01\"^^xsd:nonNegativeInteger",
        "owl:maxQualifiedCardinality 1 ; owl:onClass :D",
    ]
    .map(|n| {
        format!(
            ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; {n} ] . \
             :p a owl:ObjectProperty . :q a owl:ObjectProperty . :D a owl:Class . \
             :c a :C ; :p :x , :y . :x a :D ; :q :z . :y a :D ."
        )
    })
    .to_vec()
}

/// Such an ontology leaves the RL route, and the DL path reads the cardinality by its
/// value: `:y :q :z` is answered true, complete.
#[test]
fn a_max_cardinality_in_another_lexical_form_equates_in_rl() {
    let mut wrong = Vec::new();
    for data in max_cardinality() {
        let dl = pipeline();
        insert(&dl, &data).expect("consistent");
        let route = dl.store().dl_bounds().last;
        let (rows, status) = query(&dl, "ASK { :y :q :z }");
        let line = format!(
            "{data} / route {route}: answers {rows:?}, complete {}",
            status.is_complete()
        );
        eprintln!("{line}");
        if rows != ["true"] || !status.is_complete() || route == "rules" {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_max_cardinality_in_another_lexical_form_equates_outside_rl() {
    let wrong = missed(&max_cardinality(), true, &["ASK { :y :q :z }"]);
    assert!(wrong.is_empty(), "complete and wrong: {wrong:#?}");
}

/// The mapping's spelling, `"1"^^xsd:nonNegativeInteger`, is the term cls-maxc2 names: the
/// ontology keeps the RL route, and the rules equate `:x` and `:y`.
#[test]
fn the_mappings_spelling_of_a_max_cardinality_keeps_the_rules_route() {
    let dl = pipeline();
    insert(
        &dl,
        ":C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; \
           owl:maxCardinality \"1\"^^xsd:nonNegativeInteger ] . \
         :p a owl:ObjectProperty . :q a owl:ObjectProperty . \
         :c a :C ; :p :x , :y . :x :q :z .",
    )
    .expect("consistent");
    assert_eq!(dl.store().dl_bounds().last, "rules");
    let (rows, status) = query(&dl, "ASK { :y :q :z }");
    assert_eq!(rows, ["true"]);
    assert!(status.is_complete(), "{:?}", status.reasons());
}

/// cls-svf1 on a data property: an individual with a value of the filler is in the class
/// (OWL 2 Direct Semantics, DataSomeValuesFrom). Its premise `5 rdf:type xsd:integer` is
/// dt-type2's, which the rule set leaves out.
fn data_some() -> Vec<String> {
    vec![
        ":age a owl:DatatypeProperty . \
         [ a owl:Restriction ; owl:onProperty :age ; owl:someValuesFrom xsd:integer ] \
           rdfs:subClassOf :Aged . \
         [ a owl:Restriction ; owl:onProperty :age ; owl:someValuesFrom rdfs:Literal ] \
           rdfs:subClassOf :Valued . \
         :a :age 5 ."
            .to_owned(),
    ]
}

const DATA_SOME_READS: [&str; 2] = ["ASK { :a a :Aged }", "ASK { :a a :Valued }"];

#[test]
fn a_data_some_values_class_holds_a_value_of_its_filler_in_rl() {
    let wrong = missed(&data_some(), false, &DATA_SOME_READS);
    assert!(wrong.is_empty(), "complete and wrong: {wrong:#?}");
}

#[test]
fn a_data_some_values_class_holds_a_value_of_its_filler_outside_rl() {
    let wrong = missed(&data_some(), true, &DATA_SOME_READS);
    assert!(wrong.is_empty(), "complete and wrong: {wrong:#?}");
}

/// prp-fp on a functional data property with one value in two lexical forms: the rule
/// equates the two terms, and dt-diff compares their values, which are one: consistent
/// (OWL 2 Direct Semantics, FunctionalDataProperty). Two values are not. Under owl2-dl in
/// RL and outside it, and under owl2-rl; every outcome is printed.
#[test]
fn a_functional_data_property_with_one_value_in_two_lexical_forms_commits() {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    use nrese_store::{MutationPipeline, StoreService};
    use std::sync::Arc;
    let owl2_rl = || {
        MutationPipeline::new(
            Arc::new(StoreService::new(crate::support::in_memory_store_config()).expect("store")),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Rl,
            ))),
        )
    };
    let mut wrong = Vec::new();
    let pairs = ONE_VALUE
        .map(|pair| (pair, true))
        .into_iter()
        .chain([(("\"1\"^^xsd:integer", "\"2\"^^xsd:integer"), false)]);
    for ((left, right), consistent) in pairs {
        let data = format!(
            ":age a owl:DatatypeProperty , owl:FunctionalProperty . :a :age {left} , {right} ."
        );
        for (mode, extra) in [("owl2-dl", ""), ("owl2-dl", OUTSIDE_RL), ("owl2-rl", "")] {
            let pipeline = if mode == "owl2-rl" {
                owl2_rl()
            } else {
                pipeline()
            };
            let result = insert(&pipeline, &format!("{data} {extra}"));
            let line = format!(
                "{mode} {left} / {right} {}: {}",
                if extra.is_empty() { "" } else { "outside RL" },
                match &result {
                    Ok(_) => "committed".to_owned(),
                    Err(e) => format!("rejected: {e}"),
                }
            );
            eprintln!("{line}");
            if result.is_ok() != consistent {
                wrong.push(line);
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
