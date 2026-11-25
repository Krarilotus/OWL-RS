use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, UserRules};
use nrese_store::{ConsistencyStatus, MutationPipeline, StoreConfig, StoreService};

use crate::support::{inferred_statements, load_past_the_pipeline};

const PREFIXES: &str = "PREFIX ex: <http://example.org/>\n\
    PREFIX owl: <http://www.w3.org/2002/07/owl#>\n\
    PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>\n\
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>\n";
const UNION: &str = "_:u owl:unionOf (ex:A ex:B) . ex:k ex:consumer _:u .";
const DROP_CONSUMER: &str = "DELETE { ex:k ex:consumer ?u } WHERE { ex:k ex:consumer ?u }";
const DOMAIN_MEMBER: &str = "ex:p rdfs:domain ?u . ex:x a ?u . FILTER(isBlank(?u))";
const DEFINE_UNION: &str = "INSERT { ?u owl:unionOf ex:l } WHERE { ex:p rdfs:domain ?u }";

#[derive(Clone, Copy, Debug)]
enum History {
    Warm,
    Cold,
    Reopened,
}

const HISTORIES: [History; 3] = [History::Warm, History::Cold, History::Reopened];

fn pipeline(store: Arc<StoreService>, reasoner: &ReasonerConfig) -> MutationPipeline {
    MutationPipeline::new(store, Arc::new(ReasonerService::new(reasoner.clone())))
}

fn with_store<T>(
    history: History,
    reasoner: &ReasonerConfig,
    data: &str,
    check: impl FnOnce(&MutationPipeline) -> T,
) -> T {
    let directory = tempfile::tempdir().expect("store directory");
    let config = StoreConfig {
        hide_unnamed_classes: true,
        ..StoreConfig::on_disk(directory.path())
    };
    let store = Arc::new(StoreService::new(config.clone()).expect("open"));
    load_past_the_pipeline(&store, &format!("{PREFIXES}{data}"));
    let writer = pipeline(store, reasoner);
    let rules = reasoner.materialised_program().expect("rules");
    writer
        .store()
        .rematerialise(&rules)
        .expect("initial closure");
    write(&writer, "INSERT DATA { ex:setup ex:note ex:ready }");
    let writer = match history {
        History::Warm => writer,
        History::Cold => pipeline(Arc::clone(writer.store()), reasoner),
        History::Reopened => {
            drop(writer);
            pipeline(
                Arc::new(StoreService::new(config).expect("reopen")),
                reasoner,
            )
        }
    };
    assert!(
        writer.store().reasoning_is_current(&rules),
        "{history:?} base"
    );
    check(&writer)
}

fn owl_rl() -> ReasonerConfig {
    ReasonerConfig::for_mode(ReasoningMode::Owl2Rl)
}

fn write(writer: &MutationPipeline, update: &str) {
    writer
        .execute_update_str(&format!("{PREFIXES}{update}"))
        .expect("write");
}

fn holds(writer: &MutationPipeline, pattern: &str) -> bool {
    let answer = writer
        .store()
        .execute_query_str(&format!("{PREFIXES}ASK {{ {pattern} }}"))
        .expect("answer");
    serde_json::from_slice::<serde_json::Value>(&answer.payload).expect("ASK JSON")["boolean"]
        .as_bool()
        .expect("an ASK answer")
}

/// Spec: the same asserted statements give the same inferred statements after
/// removing a union's last consumer, whether maintained or rematerialised.
#[test]
fn removing_the_last_consumer_keeps_the_rematerialised_closure() {
    with_store(History::Warm, &owl_rl(), UNION, |writer| {
        assert!(holds(writer, "ex:k ex:consumer ?u . ?u owl:unionOf ?l"));
        write(writer, DROP_CONSUMER);
        write(writer, "INSERT DATA { ex:x a ex:A }");

        let store = writer.store();
        let mut maintained = inferred_statements(store).expect("incremental closure");
        store
            .rematerialise(owl_rl().materialised_program().expect("rules"))
            .expect("full closure");
        let mut rebuilt = inferred_statements(store).expect("full closure");
        maintained.sort_unstable();
        rebuilt.sort_unstable();
        let extra: Vec<_> = maintained
            .iter()
            .filter(|t| rebuilt.binary_search(t).is_err())
            .collect();
        let missing: Vec<_> = rebuilt
            .iter()
            .filter(|t| maintained.binary_search(t).is_err())
            .collect();
        assert!(
            extra.is_empty() && missing.is_empty(),
            "extra {extra:?}, missing {missing:?}"
        );
    });
}

/// Spec: a union that loses its last consumer loses the inferred memberships
/// it already held, for each history of opening the writer.
#[test]
fn removing_the_last_consumer_withdraws_existing_memberships() {
    let answers = HISTORIES.map(|history| {
        with_store(
            history,
            &owl_rl(),
            &format!("{UNION} ex:y a ex:A ."),
            |writer| {
                let membership = "?u owl:unionOf ?l . ex:y a ?u";
                assert!(holds(writer, membership), "{history:?} base membership");
                write(writer, DROP_CONSUMER);
                (history, holds(writer, membership))
            },
        )
    });
    assert!(answers.iter().all(|(_, present)| !present), "{answers:?}");
}

/// Spec: declaring an already-used anonymous domain a hidden union withdraws
/// its old memberships and keeps subsequent memberships hidden.
#[test]
fn naming_an_existing_domain_as_a_union_withdraws_memberships() {
    let answers = HISTORIES.map(|history| {
        with_store(
            history,
            &owl_rl(),
            "ex:p rdfs:domain _:u . ex:x ex:p ex:o . ex:l rdf:first ex:A ; rdf:rest rdf:nil .",
            |writer| {
                assert!(holds(writer, DOMAIN_MEMBER), "{history:?} base membership");
                write(writer, DEFINE_UNION);
                let existing = holds(writer, DOMAIN_MEMBER);
                write(writer, "INSERT DATA { ex:y ex:p ex:o }");
                let subsequent = holds(
                    writer,
                    "ex:p rdfs:domain ?u . ?subject a ?u . VALUES ?subject { ex:x ex:y }",
                );
                (history, existing, subsequent)
            },
        )
    });
    assert!(
        answers
            .iter()
            .all(|(_, existing, subsequent)| !existing && !subsequent),
        "{answers:?}"
    );
}

/// Spec: declaring a used anonymous domain a hidden union through a fresh
/// blank-node list withdraws its old memberships at publication.
#[test]
fn defining_a_union_with_a_fresh_list_withdraws_memberships() {
    let answers = HISTORIES.map(|history| {
        with_store(
            history,
            &owl_rl(),
            "ex:p rdfs:domain _:u . ex:x ex:p ex:y .",
            |writer| {
                assert!(holds(writer, DOMAIN_MEMBER), "{history:?} base membership");
                write(
                    writer,
                    "INSERT { ?u owl:unionOf _:l . _:l rdf:first ex:A ; rdf:rest _:t .
                          _:t rdf:first ex:B ; rdf:rest rdf:nil }
                 WHERE { ex:p rdfs:domain ?u FILTER(isBlank(?u)) }",
                );
                (
                    history,
                    holds(writer, &format!("{DOMAIN_MEMBER} ?u owl:unionOf ?h")),
                )
            },
        )
    });
    assert!(answers.iter().all(|(_, present)| !present), "{answers:?}");
}

fn repair_answers(reasoner: &ReasonerConfig, data: &str, repair: &str, effect: &str) {
    let answers = HISTORIES.map(|history| {
        with_store(history, reasoner, data, |writer| {
            assert!(
                writer.store().is_quarantined(),
                "{history:?} base quarantine"
            );
            assert!(holds(writer, DOMAIN_MEMBER), "{history:?} base membership");
            let verdict = writer.execute_update_str(&format!("{PREFIXES}{repair}"));
            let answer = verdict.map(|_| {
                (
                    holds(writer, effect),
                    holds(writer, DOMAIN_MEMBER),
                    writer.store().consistency(),
                )
            });
            (history, answer.map_err(|error| error.to_string()))
        })
    });
    assert!(
        answers.iter().all(|(_, answer)| answer.is_ok()),
        "the repair must be accepted: {answers:?}"
    );
    for (history, answer) in answers {
        assert_eq!(
            answer.unwrap(),
            (true, false, ConsistencyStatus::Consistent),
            "{history:?}"
        );
    }
}

/// Spec: a repair that hides a used union in quarantine is judged by the settled
/// closure, where a custom consistency rule cannot see the withdrawn membership.
#[test]
fn repair_with_a_custom_consistency_rule_uses_the_settled_closure() {
    let rules = UserRules::pie(
        "repair.pie",
        r#"
Rules {
Id: domain
  <http://example.org/p> <http://www.w3.org/2000/01/rdf-schema#domain> c
  x <http://example.org/p> y
  ----------------
  x <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> c

Consistency: old
  <http://example.org/bad> <http://example.org/flag> <http://example.org/on>
  ----------------

Consistency: transient
  x <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> c
  c <http://www.w3.org/2002/07/owl#unionOf> l
  x <http://example.org/flag> <http://example.org/on>
  ----------------
}
"#,
    )
    .expect("PIE rules");
    let reasoner = ReasonerConfig::for_mode(ReasoningMode::Custom)
        .with_rules(Some(Arc::new(rules)))
        .expect("custom rules");
    repair_answers(
        &reasoner,
        "ex:p rdfs:domain _:u . _:u owl:unionOf (ex:A ex:B) .
         ex:x ex:p ex:o . ex:k ex:consumer _:u . ex:bad ex:flag ex:on .",
        "DELETE { ex:k ex:consumer ?u . ex:bad ex:flag ex:on }
         INSERT { ex:x ex:flag ex:on }
         WHERE { ex:k ex:consumer ?u . ex:bad ex:flag ex:on }",
        "ex:x ex:flag ex:on",
    );
}

/// Spec: a repair that hides a used union in quarantine is judged by the settled
/// closure, where an OWL disjointness axiom inferred by a user rule holds.
#[test]
fn repair_with_an_inferred_disjointness_axiom_uses_the_settled_closure() {
    let rules = UserRules::n3(
        "repair.n3",
        "@prefix ex: <http://example.org/> .
         @prefix owl: <http://www.w3.org/2002/07/owl#> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         { ?p rdfs:domain ?u . ?u owl:unionOf ?l } => { ?u owl:disjointWith ex:C } .",
    )
    .expect("N3 rules");
    let reasoner = owl_rl()
        .with_rules(Some(Arc::new(rules)))
        .expect("user rules");
    repair_answers(
        &reasoner,
        "ex:p rdfs:domain _:u . ex:x ex:p ex:o ; a ex:C .
         ex:l rdf:first ex:A ; rdf:rest rdf:nil .
         ex:D owl:disjointWith ex:E . ex:a a ex:D, ex:E .",
        "DELETE { ex:a a ex:E }
         INSERT { ?u owl:unionOf ex:l } WHERE { ex:p rdfs:domain ?u }",
        "ex:p rdfs:domain ?u . ?u owl:disjointWith ex:C",
    );
}
