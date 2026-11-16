//! Keys bind named individuals only. Under the OWL 2 Direct Semantics a `HasKey` axiom
//! equates two named instances of its class that have, for each key property, the same
//! named individual (object properties) or the same literal (data properties) as value.
//! An anonymous individual, as an instance or as a value, binds nothing.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{MutationError, MutationPipeline, StoreConfig, StoreService};

use super::queries::query;
use super::versions::in_transaction;
use super::{ask, insert, pipeline, update};

/// A union on the right of a subclass axiom: outside OWL 2 RL, so beside the data it takes
/// the ontology off the RL route, to the bounds.
const OUTSIDE_RL: &str = ":Pet rdfs:subClassOf [ owl:unionOf ( :Dog :Cat ) ] . :tib a :Pet .";

/// The fingerprint of `owl2-rl`'s program, as its stores record it. Another value rebuilds
/// every `owl2-rl` store's closure at its next start.
const OWL2_RL_FINGERPRINT: &str = "e4d77523ded2917d";

/// An object key over `z`, `:a` and `:b` sharing it as value, and `more`.
fn object_key(z: &str, more: &str) -> String {
    format!(
        ":p a owl:ObjectProperty . :K owl:hasKey ( :p ) . :a a :K ; :p {z} . \
         :b a owl:NamedIndividual , :K , :D ; :p {z} . {more}"
    )
}

/// Each `(data, answer, complete)` committed and `read` asked: the rows must be `[answer]`,
/// and the answer complete or not as `complete` says (either, if `None`). The wrong ones,
/// as lines; every answer is printed with its route and status.
fn answered(cases: &[(String, &str, Option<bool>)], read: &str) -> Vec<String> {
    let mut wrong = Vec::new();
    for (data, answer, complete) in cases {
        let dl = pipeline();
        insert(&dl, data).expect("consistent");
        let route = dl.store().dl_bounds().last;
        let (rows, status) = query(&dl, read);
        let line = format!(
            "{data} / route {route}: {read} answers {rows:?}, complete {}",
            status.is_complete()
        );
        eprintln!("{line}");
        if rows != [*answer] || complete.is_some_and(|c| c != status.is_complete()) {
            wrong.push(line);
        }
    }
    wrong
}

/// An object key whose shared value is an anonymous individual (`_:z`) doesn't equate `:a`
/// and `:b`, so `:a a :D` isn't entailed; with the value named (`:z`) it is, and so it is
/// when the anonymous value is the named `:n` (`_:z owl:sameAs :n`): the key binds the
/// individual, whatever names it. An OWL 2 RL ontology: the rules decide it alone.
#[test]
fn a_key_binds_no_anonymous_value() {
    let cases = [
        ("_:z", "", "false"),
        (":z", "", "true"),
        ("_:z", "_:z owl:sameAs :n .", "true"),
    ]
    .map(|(z, more, answer)| (object_key(z, more), answer, None));
    let wrong = answered(&cases, "ASK { :a a :D }");
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A data key shared by an anonymous instance (`_:d1`) and a named one doesn't equate them,
/// so `:d2 a :E` isn't entailed; with the instance named (`:d1`) it is. Under the Direct
/// Semantics a key equates only elements in NAMED (*OWL 2 Direct Semantics* §2.3.5,
/// Table 9). Outside OWL 2 RL: the bounds decide it. Their upper bound applies the key to
/// every individual and so holds `:d2 a :E`; the exact check of that one candidate refutes
/// it, so the `false` is complete.
#[test]
fn a_key_binds_no_anonymous_instance() {
    let cases =
        [("_:d1", "false", Some(true)), (":d1", "true", None)].map(|(d1, answer, complete)| {
            let data = format!(
                ":k a owl:DatatypeProperty . :D owl:hasKey ( :k ) . \
             {d1} a :D , :E ; :k 7 . :d2 a :D ; :k 7 . {OUTSIDE_RL}"
            );
            (data, answer, complete)
        });
    let wrong = answered(&cases, "ASK { :d2 a :E }");
    assert!(wrong.is_empty(), "{wrong:#?}");
    // The anonymous instance's `false`: the upper bound's one candidate, refuted exactly.
    let dl = pipeline();
    insert(&dl, &cases[0].0).expect("consistent");
    let (_, status) = query(&dl, "ASK { :d2 a :E }");
    let refuted = status
        .bounds
        .map(|b| (b.lower, b.upper, b.refuted, b.unresolved));
    assert_eq!(
        (status.paths.as_slice(), refuted),
        (&["exact-ground-entailment"][..], Some((0, Some(1), 1, 0))),
        "{status:?}"
    );
}

/// An object key whose value's `sameAs` class is all anonymous (`_:z1`, `_:z2`) binds
/// nothing; a later commit that adds the named `:n` to the class makes it bind, and the
/// commit that deletes that statement unbinds it again. `owl:sameAs` on a blank node is
/// outside OWL 2 DL (*OWL 2 Structural Specification* §11.2), so no status is required:
/// each answer and its status is the one a store given the same statements in one commit
/// gives; in both equality modes, every identity stored and representatives stored.
#[test]
fn a_key_binds_from_the_commit_that_names_its_value() {
    let anonymous = object_key("_:z1", "_:z1 owl:sameAs _:z2 .");
    let named = object_key("_:z1", "_:z1 owl:sameAs _:z2 . _:z2 owl:sameAs :n .");
    let wrong = commits_answer_as_one_load(
        &[
            (
                format!("INSERT DATA {{ {anonymous} }}"),
                anonymous.clone(),
                "false",
            ),
            (
                "INSERT { ?z owl:sameAs :n } WHERE { :a :p ?z }".to_owned(),
                named,
                "true",
            ),
            (
                "DELETE { ?z owl:sameAs :n } WHERE { ?z owl:sameAs :n }".to_owned(),
                anonymous,
                "false",
            ),
        ],
        None,
    );
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// [`a_key_binds_from_the_commit_that_names_its_value`] inside OWL 2 DL: the values `_:z1`
/// and `_:z2` are equal through a functional property of the anonymous `_:w`, and a commit
/// names them by giving `_:w` the value `:n`. The anonymous individuals form a tree with
/// `_:w` at most once next to a named one (§11.2), and no `owl:sameAs` is asserted: an OWL 2
/// RL ontology, so each answer must also be complete.
#[test]
fn a_key_binds_from_the_commit_that_names_its_value_by_a_functional_property() {
    let data = |more: &str| {
        format!(
            ":p a owl:ObjectProperty . :K owl:hasKey ( :p ) . :a a :K ; :p _:z1 . \
             :b a owl:NamedIndividual , :K , :D ; :p _:z2 . \
             :f a owl:ObjectProperty , owl:FunctionalProperty . _:w :f _:z1 , _:z2 {more} ."
        )
    };
    let (anonymous, named) = (data(""), data(", :n"));
    let wrong = commits_answer_as_one_load(
        &[
            (
                format!("INSERT DATA {{ {anonymous} }}"),
                anonymous.clone(),
                "false",
            ),
            (
                "INSERT { ?w :f :n } WHERE { ?w :f ?z }".to_owned(),
                named,
                "true",
            ),
            (
                "DELETE { ?w :f :n } WHERE { ?w :f :n }".to_owned(),
                anonymous,
                "false",
            ),
        ],
        Some(true),
    );
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Each `(change, the same statements as data, answer)` committed in turn, `ASK { :a a :D }`
/// asked after each: the answer must be `[answer]`, and it and its status the ones of a
/// store given the data in one commit; complete or not as `complete` says (either, if
/// `None`). In both equality modes. The wrong ones, as lines; every answer is printed.
fn commits_answer_as_one_load(
    steps: &[(String, String, &str)],
    complete: Option<bool>,
) -> Vec<String> {
    let store = |compact: bool| {
        let config = StoreConfig {
            equality_compact: compact,
            ..crate::support::in_memory_store_config()
        };
        let dl = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl);
        MutationPipeline::new(
            Arc::new(StoreService::new(config).expect("store")),
            Arc::new(ReasonerService::new(dl)),
        )
    };
    let read = "ASK { :a a :D }";
    let mut wrong = Vec::new();
    for compact in [false, true] {
        let committed = store(compact);
        for (change, data, answer) in steps {
            update(&committed, change).expect("commit");
            let (rows, status) = query(&committed, read);
            let once = store(compact);
            insert(&once, data).expect("consistent");
            let (once_rows, once_status) = query(&once, read);
            let line = format!(
                "compact {compact}, {change}: {rows:?}, complete {}; in one commit {once_rows:?}, \
                 complete {}",
                status.is_complete(),
                once_status.is_complete()
            );
            eprintln!("{line}");
            if rows != [*answer]
                || once_rows != [*answer]
                || status.is_complete() != once_status.is_complete()
                || complete.is_some_and(|c| c != status.is_complete())
            {
                wrong.push(line);
            }
        }
    }
    wrong
}

/// Under `owl2-rl` the key rule (prp-key, OWL 2 Profiles §4.3, Table 5) has no condition on
/// the value: `_:z` binds `:a` and `:b`, and `:a a :D` holds. The named-only keys are
/// `owl2-dl`'s; `owl2-rl`'s program keeps the fingerprint its stores were written with.
#[test]
fn owl2_rl_keeps_its_key_rule_and_its_fingerprint() {
    let rl = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl);
    let store = StoreService::new(crate::support::in_memory_store_config()).expect("store");
    let pipeline =
        MutationPipeline::new(Arc::new(store), Arc::new(ReasonerService::new(rl.clone())));
    insert(&pipeline, &object_key("_:z", "")).expect("consistent");
    assert!(ask(&pipeline, ":a a :D"));
    let program = rl.materialised_program().expect("rules");
    assert_eq!(
        format!("{:016x}", program.fingerprint()),
        OWL2_RL_FINGERPRINT
    );
}

/// An `owl2-dl` store, every identity stored (`compact` false) or representatives stored.
fn owl2_dl(compact: bool) -> MutationPipeline {
    let config = StoreConfig {
        equality_compact: compact,
        ..crate::support::in_memory_store_config()
    };
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    )
}

/// No anonymous individual occurs in a `SameIndividual` axiom of an OWL 2 DL ontology
/// (*OWL 2 Structural Specification* §11.2), but the Direct Semantics reads every OWL 2
/// ontology (*OWL 2 Direct Semantics* §2.3.7–2.4): an anonymous individual is existential.
/// Held as a constant, every conclusion that doesn't name it stays, and a key binds it only
/// where it equals a named individual (the only use of NAMED, §2.3.5). Spec: under `owl2-dl`
/// an answer through such an equality is the entailed one and complete, for a blank node
/// equal to a named one (`:a a :D` through `_:b`) and for two blank nodes equal (`:y a :D`
/// through an inverse functional property whose values they are); and for a key's value
/// whose `sameAs` class holds no IRI (`_:z1 owl:sameAs _:z2`), which the key doesn't bind
/// (points 2 and 3 of docs/design/owl2-dl.md §16), so `:a a :D` is not entailed: `false`.
/// Each read is a class membership, which may be complete (docs/spec/reasoning-semantics.md,
/// "What may be complete"), so the decided reading is pinned by the answer and its status
/// together; in both equality modes, and read inside a transaction with nothing pending.
#[test]
fn an_anonymous_individual_in_same_as_answers_complete() {
    let all_blank_key = object_key("_:z1", "_:z1 owl:sameAs _:z2 .");
    let cases = [
        ("_:b owl:sameAs :a . _:b a :D .", "ASK { :a a :D }", "true"),
        (
            "_:b owl:sameAs _:c . :r a owl:ObjectProperty , owl:InverseFunctionalProperty . \
             :x :r _:b ; a :D . :y :r _:c .",
            "ASK { :y a :D }",
            "true",
        ),
        (all_blank_key.as_str(), "ASK { :a a :D }", "false"),
    ];
    let mut wrong = Vec::new();
    for compact in [false, true] {
        for (data, read, answer) in cases {
            let dl = owl2_dl(compact);
            insert(&dl, data).expect("accepted");
            let route = dl.store().dl_bounds().last;
            let (rows, status) = query(&dl, read);
            let (text, pending) = in_transaction(&dl, Vec::new(), read);
            let pending = pending.expect("a status");
            let line = format!(
                "compact {compact}, {data} / route {route}: {read} answers {rows:?}, complete {}; \
                 inside a transaction {text}, {}",
                status.is_complete(),
                pending.as_str()
            );
            eprintln!("{line}");
            if rows != [answer]
                || !status.is_complete()
                || !text.contains(answer)
                || !(pending.sound && pending.complete)
            {
                wrong.push(line);
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// `:A` is the union of `:B` and `:C`, both disjoint from `:D`: no `:A` is a `:D`. A union on
/// the right, so neither the OWL 2 RL rules nor the context core decide it: the hypertableau
/// does.
const NO_A_IS_A_D: &str = ":A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
     :B owl:disjointWith :D . :C owl:disjointWith :D .";

/// A data key of `:K` shared by `d1`, a `:D`, and the named `:d2`, an `:A`, and `more`: where the
/// key equates them, the data has no model.
fn keyed_d_and_a(d1: &str, more: &str) -> String {
    format!(
        "{NO_A_IS_A_D} :k a owl:DatatypeProperty . :K owl:hasKey ( :k ) . \
         {d1} a :K , :D ; :k 7 . :d2 a :K , :A ; :k 7 . {more}"
    )
}

/// The hypertableau's keys bind elements in NAMED only (*OWL 2 Direct Semantics* §2.3.5,
/// NAMED in §2.2): an anonymous individual equal to a named one is that one's element in
/// every model, so it binds; one equal to none binds nothing. Spec: the commit's consistency
/// check, by the hypertableau, rejects the data where the key's anonymous instance is equal
/// to the named `:m` (asserted, and through a functional property: a merge in the tableau),
/// as where the instance is named; it accepts the data where the instance equals no named
/// individual.
#[test]
fn the_hypertableau_binds_an_anonymous_instance_only_through_a_named_one() {
    let functional = ":f a owl:ObjectProperty , owl:FunctionalProperty . :w :f _:d1 , :m .";
    let cases = [
        (":d1", "", false),
        ("_:d1", "_:d1 owl:sameAs :m .", false),
        ("_:d1", functional, false),
        ("_:d1", "", true),
    ];
    let mut wrong = Vec::new();
    for (d1, more, consistent) in cases {
        let dl = pipeline();
        let data = keyed_d_and_a(d1, more);
        let found = match insert(&dl, &data) {
            Ok(_) => {
                let checked = dl.store().dl().status().expect("a DL status").consistency;
                format!("accepted, {} {:?}", checked.engine, checked.verdict)
            }
            Err(MutationError::Rejected(reject)) => format!("rejected: {}", reject.detail),
            Err(other) => format!("failed: {other:?}"),
        };
        let line = format!("{d1} {more}: {found}");
        eprintln!("{line}");
        let expected = match consistent {
            true => "accepted, hypertableau Consistent",
            false => "found by the hypertableau",
        };
        if !found.contains(expected) {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The exact check's keys are the hypertableau's: the anonymous `_:d1` shares a data key with
/// the named `:d2`, and `:w`, in one of two classes each of which has at most one `:r` value,
/// has the values `_:d1` and `:m`. A union in an assertion, so the OWL 2 RL rules (the lower
/// bound) don't equate `_:d1` with `:m`; the upper bound applies the key to every individual
/// and holds `:d2 a :E`. Spec: with the two classes bounding `:r`, `_:d1` is `:m` in every
/// model, so the key binds it and `:d2 a :E` is entailed (§2.3.5); without them, equality
/// with a named individual is not entailed, and neither is `:d2 a :E`. Each answer comes from the exact check of that one
/// candidate, complete.
#[test]
fn the_exact_check_binds_an_anonymous_instance_only_through_a_named_one() {
    let at_most_one_r = |class: &str| {
        format!(
            "{class} rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :r ; \
             owl:maxCardinality \"1\"^^xsd:nonNegativeInteger ] ."
        )
    };
    let data = |bounded: bool| {
        let bounds = match bounded {
            true => format!("{} {}", at_most_one_r(":F1"), at_most_one_r(":F2")),
            false => String::new(),
        };
        format!(
            ":k a owl:DatatypeProperty . :K owl:hasKey ( :k ) . :r a owl:ObjectProperty . \
             _:d1 a :K , :E ; :k 7 . :d2 a :K ; :k 7 . \
             :w a [ owl:unionOf ( :F1 :F2 ) ] ; :r _:d1 , :m . {bounds}"
        )
    };
    let mut wrong = Vec::new();
    for (bounded, answer) in [(true, "true"), (false, "false")] {
        let dl = pipeline();
        insert(&dl, &data(bounded)).expect("consistent");
        let (rows, status) = query(&dl, "ASK { :d2 a :E }");
        let line = format!(
            ":r bounded {bounded}: {rows:?}, complete {}, paths {:?}, bounds {:?}",
            status.is_complete(),
            status.paths,
            status.bounds
        );
        eprintln!("{line}");
        if rows != [answer]
            || !status.is_complete()
            || status.paths.as_slice() != ["exact-ground-entailment"]
        {
            wrong.push(line);
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
