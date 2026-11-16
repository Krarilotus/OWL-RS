//! Keys bind named individuals only. Under the OWL 2 Direct Semantics a `HasKey` axiom
//! equates two named instances of its class that have, for each key property, the same
//! named individual (object properties) or the same literal (data properties) as value.
//! An anonymous individual, as an instance or as a value, binds nothing.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{MutationPipeline, StoreService};

use super::queries::query;
use super::{ask, insert, pipeline};

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
/// so `:d2 a :E` isn't entailed; with the instance named (`:d1`) it is. Outside OWL 2 RL:
/// the bounds decide it. Their upper bound applies the key to every individual, so the
/// `false` is sound, not complete.
#[test]
fn a_key_binds_no_anonymous_instance() {
    let cases =
        [("_:d1", "false", Some(false)), (":d1", "true", None)].map(|(d1, answer, complete)| {
            let data = format!(
                ":k a owl:DatatypeProperty . :D owl:hasKey ( :k ) . \
             {d1} a :D , :E ; :k 7 . :d2 a :D ; :k 7 . {OUTSIDE_RL}"
            );
            (data, answer, complete)
        });
    let wrong = answered(&cases, "ASK { :d2 a :E }");
    assert!(wrong.is_empty(), "{wrong:#?}");
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
