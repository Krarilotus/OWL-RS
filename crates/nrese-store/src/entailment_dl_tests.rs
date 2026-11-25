//! The `owl2-dl` entailment check at its seams: its time is one budget, from its start, and
//! it reads one revision.

use std::sync::Arc;
use std::time::Duration;

use nrese_rdf::{BlankNode, NamedNode, Triple};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};

use crate::dl::entailment::Entailed;
use crate::service::hooks;
use crate::{
    DlConfig, MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest,
    StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// `:C ⊑ ∃p.A`, the range of `:p` is `:B`, `:A` disjoint with `:B`: `:C` is empty, so
/// `:a a ¬C` follows, and only the DL engines show it.
const EMPTY_C: &str = ":p a owl:ObjectProperty ; rdfs:range :B . :A owl:disjointWith :B . \
     :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :A ] . \
     :a a :D .";

/// A store under `owl2-dl` with `dl`, its pipeline, `data` committed.
fn dl_store(dl: DlConfig, data: &str) -> (Arc<StoreService>, MutationPipeline) {
    store_under(ReasoningMode::Owl2Dl, dl, data)
}

/// A store under `mode` with `dl`, its pipeline, `data` committed.
fn store_under(
    mode: ReasoningMode,
    dl: DlConfig,
    data: &str,
) -> (Arc<StoreService>, MutationPipeline) {
    let store = Arc::new(
        StoreService::new(StoreConfig {
            dl,
            ..StoreConfig::in_memory()
        })
        .unwrap(),
    );
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    );
    update(&pipeline, &format!("INSERT DATA {{ {data} }}"));
    (store, pipeline)
}

fn update(pipeline: &MutationPipeline, text: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{text}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap();
}

fn iri(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://example.com/{local}"))
}

/// `:a a [ owl:complementOf :class ]`.
fn not_in(a: &str, class: &str) -> Vec<Triple> {
    let complement = BlankNode::new_unchecked("c");
    vec![
        Triple::new(iri(a), nrese_rdf::vocab::rdf::TYPE, complement.clone()),
        Triple::new(complement, nrese_rdf::vocab::owl::COMPLEMENT_OF, iri(class)),
    ]
}

/// Spec (`entails_dl`: decided within `dl.timeout` per test, the operation's remaining time
/// for each nested check, `Budget::remaining`): when the premise's check leaves no time, the
/// conclusion's tests decide nothing; with the time they need, they decide it.
#[test]
fn the_conclusion_is_tested_in_what_the_premise_check_left() {
    let dl = DlConfig {
        timeout: Some(Duration::from_millis(300)),
        ..DlConfig::default()
    };
    let (store, _pipeline) = dl_store(dl, EMPTY_C);
    let conclusion = not_in("a", "C");
    assert_eq!(
        store.entails_dl(&conclusion).unwrap().answer,
        Entailed::Yes,
        "with time"
    );
    hooks::AFTER_DL_PREMISE.set(Some(Box::new(|| {
        std::thread::sleep(Duration::from_millis(600));
    })));
    let late = store.entails_dl(&conclusion).unwrap().answer;
    hooks::AFTER_DL_PREMISE.set(None);
    assert!(
        matches!(late, Entailed::Unknown(_)),
        "past the deadline: {late:?}"
    );
}

/// Spec (`entailment` module docs: what the DL engines can't decide is undecided): where the
/// DL check runs out of time after the premise's check, a complement only it shows is
/// undecided, never denied.
#[test]
fn a_dl_check_out_of_time_leaves_the_answer_undecided() {
    let dl = DlConfig {
        timeout: Some(Duration::from_millis(300)),
        ..DlConfig::default()
    };
    let (store, _pipeline) = dl_store(dl, EMPTY_C);
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    hooks::AFTER_DL_PREMISE.set(Some(Box::new(|| {
        std::thread::sleep(Duration::from_millis(600));
    })));
    let answer = store.entails(rules, &not_in("a", "C"));
    hooks::AFTER_DL_PREMISE.set(None);
    assert!(
        matches!(answer, Err(crate::StoreError::Undecided(_))),
        "{answer:?}"
    );
}

/// The ground statement `:s <predicate> <object>`, both full IRIs.
fn ground(s: &str, predicate: &str, object: &str) -> Triple {
    let named = NamedNode::new_unchecked;
    Triple::new(iri(s), named(predicate), named(object))
}

const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// Spec (`entails` under `owl2-dl`: one `dl.timeout` from the call): where the time is used
/// up before the DL check starts, it gets none and the answer to a ground conclusion
/// (`:C ⊑ ⊥`) is undecided; with the time, entailed.
#[test]
fn time_spent_before_the_dl_check_leaves_it_none() {
    let conclusion = [ground(
        "C",
        "http://www.w3.org/2000/01/rdf-schema#subClassOf",
        &format!("{OWL}Nothing"),
    )];
    let answers = [false, true].map(|late| {
        let dl = DlConfig {
            timeout: Some(Duration::from_millis(300)),
            ..DlConfig::default()
        };
        let (store, _pipeline) = dl_store(dl, EMPTY_C);
        let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
            .materialised_program()
            .unwrap();
        if late {
            hooks::AFTER_CERTIFIED.set(Some(Box::new(|| {
                std::thread::sleep(Duration::from_millis(600));
            })));
        }
        let answer = store.entails(rules, &conclusion);
        hooks::AFTER_CERTIFIED.set(None);
        match answer {
            Err(crate::StoreError::Undecided(_)) => "undecided".to_owned(),
            other => format!("{other:?}"),
        }
    });
    assert_eq!(answers, ["Ok(Entailed)".to_owned(), "undecided".to_owned()]);
}

/// Spec (`entails` under `owl2-dl`: one `dl.timeout` from the call; past it, no answer of
/// any kind): inconsistent data entails everything, but that found after the time is up is
/// no answer; within it, `InconsistentPremise`.
#[test]
fn an_inconsistent_premise_found_past_the_time_is_undecided() {
    let answers = [false, true].map(|late| {
        let dl = DlConfig {
            timeout: Some(Duration::from_millis(300)),
            ..DlConfig::default()
        };
        let (store, _pipeline) = dl_store(dl, ":a a :D .");
        // Past the gates, which would refuse it: `:x` in two disjoint classes.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.ttl");
        let prefixes = PREFIXES.replace("PREFIX", "@prefix").replace("> ", "> . ");
        std::fs::write(
            &file,
            format!("{prefixes}\n:x a :A , :B . :A owl:disjointWith :B ."),
        )
        .unwrap();
        store
            .bulk_load(&crate::BulkLoadRequest {
                files: vec![file],
                replace: false,
                graph: crate::GraphTarget::DefaultGraph,
                invalid: Some(crate::InvalidStatements::Strict),
            })
            .unwrap();
        let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
            .materialised_program()
            .unwrap();
        store.rematerialise(rules.clone()).unwrap();
        if late {
            hooks::AFTER_CERTIFIED.set(Some(Box::new(|| {
                std::thread::sleep(Duration::from_millis(600));
            })));
        }
        let answer = store.entails(
            rules,
            &[Triple::new(iri("x"), nrese_rdf::vocab::rdf::TYPE, iri("C"))],
        );
        hooks::AFTER_CERTIFIED.set(None);
        match answer {
            Err(crate::StoreError::Undecided(_)) => "undecided".to_owned(),
            other => format!("{other:?}"),
        }
    });
    assert_eq!(
        answers,
        ["Ok(InconsistentPremise)".to_owned(), "undecided".to_owned()]
    );
}

/// Spec (`entails_dl`: decided within one `dl.timeout` from the call; past it, unknown):
/// the empty conclusion is entailed, but not past the time.
#[test]
fn the_empty_conclusion_past_the_time_is_unknown() {
    let answers = [false, true].map(|late| {
        let dl = DlConfig {
            timeout: Some(Duration::from_millis(300)),
            ..DlConfig::default()
        };
        let (store, _pipeline) = dl_store(dl, EMPTY_C);
        if late {
            hooks::AFTER_DL_PREMISE.set(Some(Box::new(|| {
                std::thread::sleep(Duration::from_millis(600));
            })));
        }
        let answer = store.entails_dl(&[]).unwrap().answer;
        hooks::AFTER_DL_PREMISE.set(None);
        match answer {
            Entailed::Unknown(_) => "unknown".to_owned(),
            other => format!("{other:?}"),
        }
    });
    assert_eq!(answers, ["Yes".to_owned(), "unknown".to_owned()]);
}

/// Spec (`entails`: the mode at the call's start gives its route and, under `owl2-dl`, its one
/// `dl.timeout`): a check started outside `owl2-dl` stays on the rules' route when the mode
/// switches to `owl2-dl` during it, and answers a complement only the DL engines show with
/// the rules' `NotEntailed`, never with a DL answer found past the time.
#[test]
fn a_mode_switch_during_the_check_leaves_it_on_its_route() {
    let dl = DlConfig {
        timeout: Some(Duration::from_millis(300)),
        ..DlConfig::default()
    };
    let (store, _pipeline) = dl_store(dl, EMPTY_C);
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    store.use_dl(false, false);
    let switched = Arc::clone(&store);
    hooks::AFTER_CERTIFIED.set(Some(Box::new(move || {
        switched.use_dl(true, false);
    })));
    hooks::AFTER_DL_PREMISE.set(Some(Box::new(|| {
        std::thread::sleep(Duration::from_millis(600));
    })));
    let answer = store.entails(rules, &not_in("a", "C"));
    hooks::AFTER_CERTIFIED.set(None);
    hooks::AFTER_DL_PREMISE.set(None);
    assert_eq!(format!("{answer:?}"), "Ok(NotEntailed)");
}

/// A rule program whose conversion into one takes 600 ms.
struct SlowProgram(nrese_reasoner::RuleProgram);

impl From<SlowProgram> for nrese_reasoner::RuleProgram {
    fn from(slow: SlowProgram) -> Self {
        std::thread::sleep(Duration::from_millis(600));
        slow.0
    }
}

/// Spec (`entails` under `owl2-dl`: one `dl.timeout` from the call; past it, no answer of
/// any kind): the caller's conversion of its program is part of the call, so where it takes
/// the time, the answer is undecided; with a quick one, entailed.
#[test]
fn a_program_conversion_that_takes_the_time_leaves_the_answer_undecided() {
    let answers = [false, true].map(|slow| {
        let dl = DlConfig {
            timeout: Some(Duration::from_millis(300)),
            ..DlConfig::default()
        };
        let (store, _pipeline) = dl_store(dl, EMPTY_C);
        let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
            .materialised_program()
            .unwrap();
        let answer = match slow {
            true => store.entails(SlowProgram(rules), &not_in("a", "C")),
            false => store.entails(rules, &not_in("a", "C")),
        };
        match answer {
            Err(crate::StoreError::Undecided(_)) => "undecided".to_owned(),
            other => format!("{other:?}"),
        }
    });
    assert_eq!(answers, ["Ok(Entailed)".to_owned(), "undecided".to_owned()]);
}

/// Spec (`entails` under `owl2-dl`: one `dl.timeout` from the call; past it, no answer of
/// any kind): where the time runs out after the check has its answer and has let its
/// snapshot go, the answer is undecided; within it, entailed.
#[test]
fn an_answer_past_the_time_after_the_reads_is_undecided() {
    let answers = [false, true].map(|late| {
        let dl = DlConfig {
            timeout: Some(Duration::from_millis(300)),
            ..DlConfig::default()
        };
        let (store, _pipeline) = dl_store(dl, EMPTY_C);
        let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
            .materialised_program()
            .unwrap();
        if late {
            hooks::AFTER_ENTAILMENT_READS.set(Some(Box::new(|| {
                std::thread::sleep(Duration::from_millis(600));
            })));
        }
        let answer = store.entails(rules, &not_in("a", "C"));
        hooks::AFTER_ENTAILMENT_READS.set(None);
        match answer {
            Err(crate::StoreError::Undecided(_)) => "undecided".to_owned(),
            other => format!("{other:?}"),
        }
    });
    assert_eq!(answers, ["Ok(Entailed)".to_owned(), "undecided".to_owned()]);
}

/// The entailment answers the version and route it pinned. The subsequent DL commit puts
/// `:x a :C` in the rules' closure, but the answer uses the version without that statement.
#[test]
fn a_switch_to_owl2_dl_during_the_check_leaves_the_pinned_answer() {
    let (store, pipeline) = dl_store(DlConfig::default(), ":a a :D .");
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    store.use_dl(false, false);
    let switched = Arc::clone(&store);
    hooks::AFTER_MODE_READ.set(Some(Box::new(move || {
        switched.use_dl(true, false);
        update(
            &pipeline,
            "INSERT DATA { :note a owl:AnnotationProperty ; rdfs:domain :C . :C a owl:Class . \
             :x :note \"n\" }",
        );
    })));
    let answer = store.entails(
        rules,
        &[Triple::new(iri("x"), nrese_rdf::vocab::rdf::TYPE, iri("C"))],
    );
    hooks::AFTER_MODE_READ.set(None);
    assert_eq!(answer.unwrap(), crate::Entailment::NotEntailed);
}

/// Spec (`entailment` module docs: under `owl2-dl` the DL engines decide the whole
/// conclusion): a ground conclusion the closure holds is entailed by the DL check.
#[test]
fn a_ground_conclusion_the_closure_holds_takes_the_dl_check() {
    let (store, _pipeline) = dl_store(DlConfig::default(), ":D rdfs:subClassOf :C . :x a :D .");
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap();
    let checked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::clone(&checked);
    hooks::AFTER_DL_PREMISE.set(Some(Box::new(move || {
        seen.store(true, std::sync::atomic::Ordering::Relaxed);
    })));
    let answer = store.entails(
        rules,
        &[Triple::new(iri("x"), nrese_rdf::vocab::rdf::TYPE, iri("C"))],
    );
    hooks::AFTER_DL_PREMISE.set(None);
    assert_eq!(
        (
            format!("{answer:?}"),
            checked.load(std::sync::atomic::Ordering::Relaxed)
        ),
        ("Ok(Entailed)".to_owned(), true),
        "the answer, whether the DL check ran"
    );
}

/// Spec (`entails`: one certified closure, reads that answer only for it): `:x a :P` and
/// `:x ≠ :y` hold together in neither revision, the one with `:x a :P` (where `:x` may be
/// `:y`) nor the one after a commit that replaces it with `:x a :D` and `:y a :C` (`:D`
/// disjoint with `:C`). Under `owl2-rl`, with that commit between the closure's read and the
/// refutation, the check gives no answer (undecided), never `Entailed`.
#[test]
fn a_commit_between_the_closure_and_the_refutation_leaves_the_answer_undecided() {
    let (store, pipeline) = store_under(
        ReasoningMode::Owl2Rl,
        DlConfig::default(),
        ":D owl:disjointWith :C . :x a :P .",
    );
    let conclusion = [
        Triple::new(iri("x"), nrese_rdf::vocab::rdf::TYPE, iri("P")),
        ground("x", &format!("{OWL}differentFrom"), "http://example.com/y"),
    ];
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl)
        .materialised_program()
        .unwrap();
    hooks::BETWEEN_ENTAILMENT_HALVES.set(Some(Box::new(move || {
        update(
            &pipeline,
            "DELETE DATA { :x a :P } ; INSERT DATA { :x a :D . :y a :C }",
        );
    })));
    let answer = store.entails(rules, &conclusion);
    hooks::BETWEEN_ENTAILMENT_HALVES.set(None);
    assert!(
        matches!(answer, Err(crate::StoreError::Undecided(_))),
        "{answer:?}"
    );
}
