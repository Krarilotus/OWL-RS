//! Step 9: routing by profile. The per-axiom profile checker (`nrese_owl::profile`, held
//! against the W3C test cases' annotations in nrese-owl) decides the route: an OWL 2 RL
//! ontology goes to the RL rules alone (complete for it, theorem PR1: no U1, no DL
//! engine), Horn and EL ontologies to the context core, the rest to the hypertableau.
//! Each test names the route that must be taken (the performance choice's guard).

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_sparql::Completeness;
use nrese_store::{
    MutationError, MutationPipeline, RejectKind, StatementOp, StoreConfig, StoreService,
};

use super::queries::query;
use super::versions::{in_transaction, ntriples};
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

/// Under `HasKey(owl:Thing (:r) ())`, `owl:Thing ⊑ ∃:r.{:a}` and `:A(:a)`, an individual
/// `:b` named only in the question is in NAMED by the letter of the Direct Semantics (§2.2,
/// §4): it shares `:a` with `:a`, so `:b : :A` and `:b = :a`. Implementations read the
/// ontology's signature only and answer false. Which reading holds is undecided, so a false
/// may not say `complete`: under the letter it misses the answer. A probe (the small-scope
/// campaign's probe 79).
#[test]
fn an_individual_named_only_in_the_question_is_not_decided_complete_under_a_key() {
    let dl = pipeline();
    let inserted = insert(
        &dl,
        ":r a owl:ObjectProperty . :A a owl:Class . :a a owl:NamedIndividual , :A . \
         owl:Thing owl:hasKey ( :r ) ; \
         rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; owl:hasValue :a ] .",
    );
    eprintln!("insert: {inserted:?}");
    let route = dl.store().dl_bounds().last;
    let mut wrong = Vec::new();
    for read in ["ASK { :b a :A }", "ASK { :b owl:sameAs :a }"] {
        let (rows, status) = query(&dl, read);
        let line = format!(
            "route {route}: {read} answers {rows:?}, status {} ({:?})",
            status.shared.as_str(),
            status.reasons()
        );
        eprintln!("{line}");
        if rows == ["false"] && status.is_complete() {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "false labelled complete: {wrong:#?}");
}

/// The keys of [`an_individual_named_only_in_the_question_is_not_decided_complete_under_a_key`].
const KEYS: &str = ":r a owl:ObjectProperty . :A a owl:Class . :a a owl:NamedIndividual , :A . \
     owl:Thing owl:hasKey ( :r ) ; \
     rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; owl:hasValue :a ] .";

/// An individual whose statements were all deleted is named only in the question again,
/// though the store's dictionary still holds its name: under the keys, a `false` about it
/// may not say `complete` either.
#[test]
fn an_individual_whose_data_was_deleted_is_not_decided_complete_under_a_key() {
    let dl = pipeline();
    insert(&dl, KEYS).expect("the keys");
    insert(&dl, ":b :note \"b\" .").expect("b named");
    super::update(&dl, "DELETE DATA { :b :note \"b\" . }").expect("b's data deleted");
    let (rows, status) = query(&dl, "ASK { :b a :A }");
    let line = format!(
        "{rows:?}, status {} ({:?})",
        status.shared.as_str(),
        status.reasons()
    );
    eprintln!("{line}");
    assert!(
        !(rows == ["false"] && status.is_complete()),
        "false labelled complete: {line}"
    );
}

/// A name the question gives at a property path's end, not in a triple, is named only in
/// the question as well: under the keys, a `false` about it may not say `complete`. Paths
/// over ordinary properties, which no other check marks: `:r`, which `:b` has by the
/// letter, and `:s`, which nothing states.
#[test]
fn an_individual_named_only_at_a_path_end_is_not_decided_complete_under_a_key() {
    let dl = pipeline();
    insert(&dl, KEYS).expect("the keys");
    let mut wrong = Vec::new();
    for read in ["ASK { :b :r+ :a }", "ASK { :b :s+ :a }"] {
        let (rows, status) = query(&dl, read);
        let line = format!("{read}: {rows:?}, status {}", status.shared.as_str());
        eprintln!("{line} ({:?})", status.reasons());
        if rows == ["false"] && status.is_complete() {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "false labelled complete: {wrong:#?}");
}

/// Where no key can reach a name the question alone gives, the read is decided as before:
/// under the keys, a question about an individual the data names (`:a a :A`, true); and
/// without keys, one about an individual named only in the question (`:b a :A`, false: no
/// key makes it equal to a named one).
#[test]
fn a_question_no_key_can_reach_is_decided_as_before() {
    let mut wrong = Vec::new();
    let without = KEYS.replace("owl:Thing owl:hasKey ( :r ) ; ", "owl:Thing ");
    for (data, read, answer) in [
        (KEYS, "ASK { :a a :A }", "true"),
        (without.as_str(), "ASK { :b a :A }", "false"),
    ] {
        let dl = pipeline();
        insert(&dl, data).expect("the ontology");
        let (rows, status) = query(&dl, read);
        let line = format!(
            "{read} over {data}: {rows:?}, status {} ({:?})",
            status.shared.as_str(),
            status.reasons()
        );
        eprintln!("{line}");
        if rows != [answer] || !status.is_complete() {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "not decided as before: {wrong:#?}");
}

/// The keys' question on the other reads with a status: inside a transaction with
/// operations pending, and of the inferred statements alone. Neither says `complete`.
#[test]
fn the_other_reads_of_an_individual_named_only_in_the_question_are_not_complete() {
    let dl = pipeline();
    insert(&dl, KEYS).expect("the keys");
    let pending = StatementOp::Add {
        data: ntriples("<http://example.com/c> <http://example.com/note> \"c\" ."),
        contexts: Vec::new(),
    };
    let (rows, inside) = in_transaction(&dl, vec![pending], "ASK { :b a :A }");
    let inside = inside.expect("a status");
    let implicit = dl
        .store()
        .prepare_query(&nrese_store::SparqlQueryRequest::all(format!(
            "{}ASK FROM <http://www.ontotext.com/implicit> {{ :b a :A }}",
            super::PREFIXES
        )))
        .expect("prepared");
    let alone = dl.store().status_without_dl(&implicit).expect("a status");
    let lines = [
        format!("pending: {rows}, {} {:?}", inside.as_str(), inside.reasons),
        format!("inferred alone: {} {:?}", alone.as_str(), alone.reasons),
    ];
    eprintln!("{lines:#?}");
    assert!(!inside.complete && !alone.complete, "{lines:#?}");
}

/// The plan's status of a question about an individual the data doesn't name, under a key
/// on the RL route, where nothing else keeps the plan from `complete`: the plan says why it
/// isn't, that reason alone.
#[test]
fn the_plan_of_an_individual_named_only_in_the_question_is_not_complete_under_a_key() {
    let dl = pipeline();
    insert(
        &dl,
        ":r a owl:ObjectProperty . :A a owl:Class ; owl:hasKey ( :r ) . :a a :A ; :r :x .",
    )
    .expect("the key");
    let route = dl.store().dl_bounds().last;
    let asked = dl
        .store()
        .prepare_query(&nrese_store::SparqlQueryRequest::all(format!(
            "{}ASK {{ :b a :A }}",
            super::PREFIXES
        )))
        .expect("prepared");
    let plan = dl
        .store()
        .plan_query(&asked, &nrese_store::CancellationToken::new())
        .expect("planned")
        .completeness
        .expect("a status");
    let line = format!("route {route}, plan: {} {:?}", plan.as_str(), plan.reasons);
    eprintln!("{line}");
    assert_eq!(route, "rules", "{line}");
    assert!(
        plan.sound && !plan.complete && plan.reasons.len() == 1,
        "{line}"
    );
    let reason = &plan.reasons[0].text;
    assert!(
        reason.starts_with("the query names <") && reason.contains("example.com/b"),
        "{line}"
    );
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
/// all entailed by it, asked: each must answer `true`, labelled complete. The wrong ones,
/// as lines; every answer is printed with the route.
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
            if rows != ["true"] || !status.is_complete() {
                wrong.push(line);
            }
        }
    }
    wrong
}

/// Each of `data`, inconsistent, committed (with [`OUTSIDE_RL`] where `outside`): it must
/// be rejected as inconsistent, by no other error. The others, as lines; every outcome is
/// printed.
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
        let inconsistent = matches!(&result, Err(nrese_store::MutationError::Rejected(r))
            if r.kind == nrese_store::RejectKind::Inconsistent);
        if !inconsistent {
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

/// The same over `rdfs:Literal` (OWL 2 Profiles §4.2.4 admits it in RL): at most `n`
/// values of `:age` at all, and `:c` has one. No rule reads `owl:onDataRange`, whatever
/// the range.
fn literal_qualified_data_cardinality(n: u32) -> String {
    format!(
        ":age a owl:DatatypeProperty . \
         :C a owl:Class ; rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :age ; \
           owl:maxQualifiedCardinality \"{n}\"^^xsd:nonNegativeInteger ; \
           owl:onDataRange rdfs:Literal ] . \
         :c a :C ; :age 5 ."
    )
}

#[test]
fn a_qualified_data_cardinality_over_any_literal_is_kept() {
    for outside in [false, true] {
        let wrong = accepted(&[literal_qualified_data_cardinality(0)], outside);
        assert!(wrong.is_empty(), "inconsistent and committed: {wrong:#?}");
        // The control: at most one, and one, is consistent.
        let dl = pipeline();
        let extra = if outside { OUTSIDE_RL } else { "" };
        let data = literal_qualified_data_cardinality(1);
        insert(&dl, &format!("{data} {extra}")).expect("consistent");
    }
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

/// A store running `owl2-rl`, the closure's own promise.
fn owl2_rl() -> nrese_store::MutationPipeline {
    use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
    use std::sync::Arc;
    nrese_store::MutationPipeline::new(
        Arc::new(
            nrese_store::StoreService::new(crate::support::in_memory_store_config())
                .expect("store"),
        ),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

/// prp-fp on a functional data property with one value in two lexical forms: the rule
/// equates the two terms, and dt-diff compares their values, which are one: consistent
/// (OWL 2 Direct Semantics, FunctionalDataProperty). Two values are not. Under owl2-dl in
/// RL and outside it, and under owl2-rl; every outcome is printed.
#[test]
fn a_functional_data_property_with_one_value_in_two_lexical_forms_commits() {
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

/// Two property names, also individuals, equated; `:p` functional; an existential.
const PUNNED_EQUALITY: &str = ":p a owl:ObjectProperty , owl:NamedIndividual , owl:FunctionalProperty ; owl:sameAs :q . \
     :q a owl:ObjectProperty , owl:NamedIndividual . :x :q :a , :b . :a a :D . :b a :D . \
     :r a owl:ObjectProperty . :D a owl:Class ; \
     rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; owl:someValuesFrom :D ] .";

const PUNNED_READS: [&str; 3] = [
    "ASK { :x :p :a }",
    "ASK { :a owl:sameAs :b }",
    "SELECT ?y { :x :p ?y }",
];

/// An annotation property with a domain, a range and a super-property.
const ANNOTATION_AXIOMS: &str = ":note a owl:AnnotationProperty ; rdfs:domain :C ; rdfs:range :C ; \
     rdfs:subPropertyOf :sup . :sup a owl:AnnotationProperty . \
     :C a owl:Class . :x a owl:NamedIndividual ; :note :v . \
     :v a owl:NamedIndividual .";

const ANNOTATION_READS: [&str; 3] = ["ASK { :x a :C }", "ASK { :v a :C }", "ASK { :x :sup :v }"];

/// Under the OWL 2 Direct Semantics, `:p owl:sameAs :q` equates the individuals `:p` and
/// `:q` (punning), not the properties of the same names: a model may give `:p` no pairs
/// while `:q` relates `:x` to two distinct individuals. So neither `:x :p :a` nor `:a
/// owl:sameAs :b` (by `:p` being functional) is entailed, and an answer `true` is a wrong
/// row: it may only come labelled `unsound`. The existential keeps it off the RL route.
#[test]
fn an_equality_of_punned_property_names_does_not_equate_their_properties() {
    let dl = pipeline();
    insert(&dl, PUNNED_EQUALITY).expect("an ontology with punned property names");
    let route = dl.store().dl_bounds().last;
    let mut wrong = Vec::new();
    for read in PUNNED_READS {
        let (rows, status) = query(&dl, read);
        let line = format!(
            "route {route}: {read} answers {rows:?}, status {} ({:?})",
            status.shared.as_str(),
            status.reasons()
        );
        eprintln!("{line}");
        let says_yes = rows == ["true"] || rows.iter().any(|r| r == "a" || r == "b");
        if says_yes && status.shared.as_str() != "unsound" {
            wrong.push(line);
        }
    }
    assert!(
        wrong.is_empty(),
        "wrong rows not labelled unsound: {wrong:#?}"
    );
}

/// An annotation property's domain, range and subproperties have no meaning for the
/// annotations under the OWL 2 Direct Semantics (annotations carry none), and theorem
/// PR1 of the OWL 2 Profiles excludes such axioms. So `:x a :C`, `:v a :C` and `:x :sup
/// "v"` aren't entailed: an answer `true` may only come labelled `unsound`, and the plan
/// says so too. Checked on the RL route and off it (a union superclass).
#[test]
fn annotation_property_axioms_entail_nothing_about_annotated_terms() {
    let mut wrong = Vec::new();
    for extra in ["", OUTSIDE_RL] {
        let dl = pipeline();
        insert(&dl, &format!("{ANNOTATION_AXIOMS} {extra}"))
            .expect("an ontology with annotation property axioms");
        let route = dl.store().dl_bounds().last;
        for read in ANNOTATION_READS {
            let (rows, status) = query(&dl, read);
            let store = dl.store();
            let request =
                nrese_store::SparqlQueryRequest::all(format!("{}{read}", super::PREFIXES));
            let planned = store
                .plan_query(
                    &store.prepare_query(&request).expect("prepared"),
                    &nrese_store::CancellationToken::new(),
                )
                .expect("planned")
                .completeness
                .expect("a status");
            let line = format!(
                "route {route}: {read} answers {rows:?}, status {} / plan {}",
                status.shared.as_str(),
                planned.as_str()
            );
            eprintln!("{line}");
            if rows == ["true"] && (status.shared.as_str() != "unsound" || planned.sound) {
                wrong.push(line);
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "wrong rows not labelled unsound: {wrong:#?}"
    );
}

/// `data` (with `extra`) committed: the RL rules clash where OWL 2 DL is consistent, and
/// this release refuses the commit as undecided (`WhyUndecided`), not as an inconsistency.
/// When OWL 2 DL decides these clashes, the tests that call this expect the commit.
fn refused_as_undecided(data: &str, extra: &str) {
    let dl = pipeline();
    let result = insert(&dl, &format!("{data} {extra}"));
    let Err(MutationError::Rejected(reject)) = &result else {
        panic!("expected the refusal, got {result:?}");
    };
    assert_eq!(reject.kind, RejectKind::Undecided, "{}", reject.detail);
    assert!(
        reject
            .detail
            .contains("annotation-property axioms or a punned name")
            && reject.detail.contains("doesn't decide")
            && !reject.detail.contains("consistency check"),
        "{}",
        reject.detail
    );
}

/// An annotation property's domain says nothing under the OWL 2 Direct Semantics about the
/// subjects it annotates; the rules' prp-dom puns `:x` into `:C`, disjoint with `:D`.
const ANNOTATION_DOMAIN: &str = ":note a owl:AnnotationProperty ; rdfs:domain :C . \
     :C a owl:Class ; owl:disjointWith :D . :D a owl:Class . \
     :x a owl:NamedIndividual , :D ; :note \"a note\" .";

/// The individuals `:A` and `:B` are equal, the classes of the same names are not
/// (punning); the rules' eq-rep-o makes `:x` a `:B` too.
const PUNNED_DISJOINT: &str = ":A a owl:Class ; owl:disjointWith :B ; owl:sameAs :B . \
     :B a owl:Class . :x a owl:NamedIndividual , :A .";

#[test]
fn an_annotation_domain_disjoint_with_the_subject_is_refused_as_undecided_in_rl() {
    refused_as_undecided(ANNOTATION_DOMAIN, "");
}

#[test]
fn an_annotation_domain_disjoint_with_the_subject_is_refused_as_undecided_outside_rl() {
    refused_as_undecided(ANNOTATION_DOMAIN, OUTSIDE_RL);
}

#[test]
fn an_equality_of_punned_disjoint_class_names_is_refused_as_undecided_in_rl() {
    refused_as_undecided(PUNNED_DISJOINT, "");
}

#[test]
fn an_equality_of_punned_disjoint_class_names_is_refused_as_undecided_outside_rl() {
    refused_as_undecided(PUNNED_DISJOINT, OUTSIDE_RL);
}

/// The control: a clash with no annotation axiom and no punned name is an inconsistency
/// under both readings, and is rejected as one on both routes.
#[test]
fn a_plain_clash_is_rejected_as_inconsistent_on_both_routes() {
    for extra in ["", OUTSIDE_RL] {
        let dl = pipeline();
        let result = insert(
            &dl,
            &format!(
                ":A a owl:Class ; owl:disjointWith :B . :B a owl:Class . \
                 :x a owl:NamedIndividual , :A , :B . {extra}"
            ),
        );
        let Err(MutationError::Rejected(reject)) = &result else {
            panic!("expected a rejection, got {result:?}");
        };
        assert_eq!(reject.kind, RejectKind::Inconsistent, "{}", reject.detail);
        assert!(
            !reject.detail.contains("doesn't decide"),
            "{}",
            reject.detail
        );
    }
}

/// A class name that is also an individual (punning, OWL 2 Structural Specification §5.9)
/// with no equality reaching it, or only its own (`:Dog owl:sameAs :Dog`, R1-F5): PR1
/// needs no punning, so the ontology leaves the RL route, and `:rex a :Dog`, entailed,
/// keeps the status the DL path gives it.
#[test]
fn a_punned_class_with_no_equality_stays_sound_off_the_rules_route() {
    for extra in ["", ":Dog owl:sameAs :Dog ."] {
        let dl = pipeline();
        insert(
            &dl,
            &format!(":Dog a owl:Class . :Dog a :Species . :rex a :Dog . {extra}"),
        )
        .expect("a punned class name");
        let route = dl.store().dl_bounds().last;
        let (rows, status) = query(&dl, "ASK { :rex a :Dog }");
        eprintln!(
            "{extra} route {route}: answers {rows:?}, status {} ({:?})",
            status.shared.as_str(),
            status.reasons()
        );
        assert_ne!(route, "rules", "a punning ontology stayed on the RL route");
        assert_eq!(rows, ["true"]);
        assert_ne!(
            status.shared.as_str(),
            "unsound",
            "{extra} {:?}",
            status.reasons()
        );
    }
}

/// Under `owl2-rl` the answer is the closure's: the punning and the annotation inputs give
/// `true` for each read (eq-rep-p, prp-fp; prp-dom, prp-rng, prp-spo1). The answer's and
/// the plan's status, printed.
#[test]
fn owl2_rl_answers_the_punning_and_annotation_reads_from_the_closure() {
    use nrese_store::{CancellationToken, SparqlQueryRequest};
    let mut wrong = Vec::new();
    for (data, reads) in [
        (PUNNED_EQUALITY, &PUNNED_READS[..2]),
        (ANNOTATION_AXIOMS, &ANNOTATION_READS[..]),
    ] {
        let rl = owl2_rl();
        insert(&rl, data).expect("committed under owl2-rl");
        let store = rl.store();
        for read in reads {
            let request = SparqlQueryRequest::all(format!("{}{read}", super::PREFIXES));
            let result = store.execute_query(&request).expect("answered");
            let planned = store
                .plan_query(
                    &store.prepare_query(&request).expect("prepared"),
                    &CancellationToken::new(),
                )
                .expect("planned");
            let status = |c: Option<nrese_sparql::Completeness>| c.map_or("none", |c| c.as_str());
            let answer = String::from_utf8(result.payload).expect("utf8");
            let line = format!(
                "{read}: true {}, status {} / plan {}",
                answer.contains("true"),
                status(result.completeness),
                status(planned.completeness)
            );
            eprintln!("{line}");
            if !answer.contains("true") {
                wrong.push(line);
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `first` committed (in OWL 2 RL, nothing punned: the RL route), then `second`, which
/// only asserts; each of `reads`, none entailed under the Direct Semantics, asked: an
/// answer `true` not labelled `unsound` is wrong. The wrong ones, as lines; every answer
/// is printed with the route.
fn yeses_after_a_later_commit(first: &str, second: &str, reads: &[&str]) -> Vec<String> {
    let dl = pipeline();
    insert(&dl, first).expect("an RL ontology");
    let before = dl.store().dl_bounds().last;
    assert_eq!(before, "rules", "the first commit is in OWL 2 RL");
    insert(&dl, second).expect("an assertion");
    let route = dl.store().dl_bounds().last;
    let mut wrong = Vec::new();
    for read in reads {
        let (rows, status) = query(&dl, read);
        let line = format!(
            "route {before} then {route}: {read} answers {rows:?}, status {}",
            status.shared.as_str()
        );
        eprintln!("{line}");
        if rows == ["true"] && status.shared.as_str() != "unsound" {
            wrong.push(line);
        }
    }
    wrong
}

/// Two property names, then an equality of them by a later commit: it makes them
/// individuals too (punning), and equates the individuals, not the properties. So
/// neither `:x :p :a` nor `:a owl:sameAs :b` (by `:p` being functional) is entailed.
#[test]
fn a_later_equality_of_property_names_does_not_equate_their_properties() {
    let wrong = yeses_after_a_later_commit(
        ":p a owl:ObjectProperty , owl:FunctionalProperty . :q a owl:ObjectProperty . \
         :x :q :a , :b .",
        ":p owl:sameAs :q .",
        &PUNNED_READS[..2],
    );
    assert!(
        wrong.is_empty(),
        "wrong rows not labelled unsound: {wrong:#?}"
    );
}

/// Two equal individuals, then a later commit that uses one as a class (punning): the
/// class `:a` isn't the class `:b`, so `:x a :b` isn't entailed.
#[test]
fn a_later_class_use_of_an_equated_individual_does_not_equate_the_classes() {
    let wrong = yeses_after_a_later_commit(
        ":a a owl:NamedIndividual ; owl:sameAs :b . :b a owl:NamedIndividual .",
        ":x a :a .",
        &["ASK { :x a :b }"],
    );
    assert!(
        wrong.is_empty(),
        "wrong rows not labelled unsound: {wrong:#?}"
    );
}

/// A class, then a later commit that makes it an individual too (punning): PR1 needs no
/// punning, so the ontology leaves the RL route, as it does when committed at once.
#[test]
fn a_later_assertion_that_puns_a_class_leaves_the_rules_route() {
    let dl = pipeline();
    insert(&dl, ":Dog a owl:Class . :rex a :Dog .").expect("an RL ontology");
    let before = dl.store().dl_bounds().last;
    insert(&dl, ":Dog a :Species .").expect("an assertion");
    let route = dl.store().dl_bounds().last;
    eprintln!("route {before} then {route}");
    assert_eq!(before, "rules", "the first commit is in OWL 2 RL");
    assert_ne!(route, "rules", "a punning ontology stayed on the RL route");
}

/// Beside each ontology of [`unmarked`]: `:y a :E`, entailed under both readings.
const ENTAILED: &str = ":y a :F . :F rdfs:subClassOf :E .";

/// Over an ontology with annotation-property axioms, and over one with an equality
/// reaching a punned name, each with [`ENTAILED`] beside it: the line of each whose `read`
/// of `?x a :E` doesn't answer `y` alone (the entailed row kept, R1-F5), or isn't `unsound`
/// with the reason.
fn unmarked(read: impl Fn(&MutationPipeline) -> (Vec<String>, Completeness)) -> Vec<String> {
    let mut wrong = Vec::new();
    for (data, reason) in [
        (ANNOTATION_AXIOMS, "annotation-property axioms"),
        (
            PUNNED_EQUALITY,
            "a class or property name and an individual equal",
        ),
    ] {
        let dl = pipeline();
        insert(&dl, &format!("{data} {ENTAILED}")).expect("committed");
        let (rows, status) = read(&dl);
        let reasons: Vec<&str> = status.reasons.iter().map(|r| r.text.as_str()).collect();
        let line = format!(
            "{reason}: {rows:?}, status {} ({reasons:?})",
            status.as_str()
        );
        eprintln!("{line}");
        let named = reasons.iter().any(|r| r.contains(reason));
        if rows != ["y"] || status.as_str() != "unsound" || !named {
            wrong.push(line);
        }
    }
    wrong
}

/// A read of the inferred statements alone (`FROM onto:implicit`) reads L, the RL closure:
/// it is `unsound` with the reason, as every other answer there is (one status
/// everywhere).
#[test]
fn a_read_of_the_inferred_statements_alone_carries_the_marks() {
    let read = "SELECT ?x FROM <http://www.ontotext.com/implicit> { ?x a :E }";
    let wrong = unmarked(|dl| {
        let (rows, status) = query(dl, read);
        (rows, status.shared)
    });
    assert!(wrong.is_empty(), "not marked: {wrong:#?}");
}

/// A read inside a transaction with an addition pending reads the committed closure, L:
/// it is `unsound` with the reason, as every other answer there is.
#[test]
fn a_read_inside_a_transaction_carries_the_marks() {
    let wrong = unmarked(|dl| {
        let add = StatementOp::Add {
            data: ntriples(
                "<http://example.com/z> <http://example.com/r> <http://example.com/w> .",
            ),
            contexts: Vec::new(),
        };
        let (out, status) = in_transaction(dl, vec![add], "SELECT ?x { ?x a :E }");
        let json: serde_json::Value = serde_json::from_str(&out).expect("json results");
        let rows = json["results"]["bindings"]
            .as_array()
            .expect("bindings")
            .iter();
        let rows = rows.map(|row| row["x"]["value"].as_str().unwrap_or("-"));
        let rows = rows.map(|x| x.trim_start_matches("http://example.com/").to_owned());
        (rows.collect(), status.expect("a status"))
    });
    assert!(wrong.is_empty(), "not marked: {wrong:#?}");
}

/// R1-F5: under compact equality the inferred statements keep one name per class of equal
/// terms, its representative (the smallest id, so the name met first), and reads expand
/// it. A class name that is also an individual equal to another is found either way: as
/// its class's representative (`:Dog` first) and as an alias (`:Canis` first). The answer
/// `true` comes `unsound` with the reason.
#[test]
fn a_punned_name_equal_to_another_marks_the_answers_under_compact_equality() {
    let mut wrong = Vec::new();
    for data in [
        ":Dog a owl:Class , :Species ; owl:sameAs :Canis . :rex a :Dog .",
        ":Canis owl:sameAs :Dog . :Dog a owl:Class , :Species . :rex a :Dog .",
    ] {
        let config = StoreConfig {
            equality_by_representatives: true,
            equality_compact: true,
            ..crate::support::in_memory_store_config()
        };
        let dl = MutationPipeline::new(
            Arc::new(StoreService::new(config).expect("store")),
            Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
                ReasoningMode::Owl2Dl,
            ))),
        );
        insert(&dl, data).expect("committed");
        let (rows, status) = query(&dl, "ASK { :rex a :Dog }");
        let line = format!(
            "{data}: {rows:?}, status {} ({:?})",
            status.shared.as_str(),
            status.reasons()
        );
        eprintln!("{line}");
        let named = status
            .reasons()
            .iter()
            .any(|r| r.contains("an individual equal"));
        if rows != ["true"] || status.shared.as_str() != "unsound" || !named {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "not marked: {wrong:#?}");
}

/// R1-F3: an addition alone can change how the committed statements read as OWL. Committed:
/// an ordinary property's domain, which entails `:x a :C` (in the committed closure). Pending:
/// only `:note a owl:AnnotationProperty`, after which the domain and the assertion are
/// annotations, which entail nothing about `:x`. The read inside the transaction still finds
/// `:x a :C` in the committed closure: that `true` may only come `unsound`. The committed
/// read after it is told what it was told before. On the rules route and off it: off it
/// (with [`OUTSIDE_RL`]) the committed read has read the committed ontology first, and the
/// pending state shares its revision.
#[test]
fn a_pending_annotation_declaration_marks_the_read_inside_the_transaction() {
    let mut wrong = Vec::new();
    for extra in ["", OUTSIDE_RL] {
        let dl = pipeline();
        let data = format!(":note rdfs:domain :C . :x :note :v . :C a owl:Class . {extra}");
        insert(&dl, &data).expect("committed");
        let read = "ASK { :x a :C }";
        let (rows, before) = query(&dl, read);
        let before = format!("{rows:?} {} {:?}", before.shared.as_str(), before.reasons());
        let declare = StatementOp::Add {
            data: ntriples(
                "<http://example.com/note> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
                 <http://www.w3.org/2002/07/owl#AnnotationProperty> .",
            ),
            contexts: Vec::new(),
        };
        let (answer, status) = in_transaction(&dl, vec![declare], read);
        let status = status.expect("a status");
        let line = format!(
            "{extra:?} pending: {answer}, status {} ({:?})",
            status.as_str(),
            status.reasons
        );
        let (rows, after) = query(&dl, read);
        let after = format!("{rows:?} {} {:?}", after.shared.as_str(), after.reasons());
        eprintln!("committed before: {before}\n{line}\ncommitted after: {after}");
        assert!(
            before.starts_with(r#"["true"]"#),
            "the committed control: {before}"
        );
        if answer.contains("true") && status.as_str() != "unsound" {
            wrong.push(line);
        }
        if before != after {
            wrong.push(format!(
                "{extra:?} the pending read changed the committed one: {after}"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
