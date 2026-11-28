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

const CLASH: &str = ":A a owl:Class . :B a owl:Class . :C a owl:Class . :D a owl:Class . \
     :Q a owl:Class . :A owl:equivalentClass [ owl:unionOf ( :B :C ) ] . \
     :B owl:disjointWith :D . :C owl:disjointWith :D . :x a :A , :D .";

fn clash_store(dl: DlConfig, query_memory_bytes: usize) -> (Arc<StoreService>, MutationPipeline) {
    let store = Arc::new(
        StoreService::new(StoreConfig {
            dl: DlConfig {
                consistency: crate::DlConsistency::Off,
                ..dl
            },
            total_query_memory_bytes: query_memory_bytes,
            ..StoreConfig::in_memory()
        })
        .unwrap(),
    );
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    update(&pipeline, &format!("INSERT DATA {{ {CLASH} }}"));
    (store, pipeline)
}

fn dl_rules() -> nrese_reasoner::RuleProgram {
    ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
        .materialised_program()
        .unwrap()
}

fn many_restrictions(count: usize) -> Vec<Triple> {
    let rdf_type = nrese_rdf::vocab::rdf::TYPE;
    let sub_class =
        nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#subClassOf");
    let restriction = nrese_rdf::NamedNode::new_unchecked(format!("{OWL}Restriction"));
    let on_property = nrese_rdf::NamedNode::new_unchecked(format!("{OWL}onProperty"));
    let some_values = nrese_rdf::NamedNode::new_unchecked(format!("{OWL}someValuesFrom"));
    let p = iri("p");
    let mut triples = Vec::with_capacity(count * 4 + 1);
    triples.push(Triple::new(
        p.clone(),
        rdf_type.into_owned(),
        nrese_rdf::NamedNode::new_unchecked(format!("{OWL}ObjectProperty")),
    ));
    for i in 0..count {
        let r = BlankNode::new_unchecked(format!("r{i}"));
        triples.push(Triple::new(
            iri(&format!("C{i}")),
            sub_class.clone(),
            r.clone(),
        ));
        triples.push(Triple::new(
            r.clone(),
            rdf_type.into_owned(),
            restriction.clone(),
        ));
        triples.push(Triple::new(r.clone(), on_property.clone(), p.clone()));
        triples.push(Triple::new(r, some_values.clone(), iri(&format!("D{i}"))));
    }
    triples
}

/// Importing a wide conclusion polls the deadline inside the import and stops
/// before completing all visits.
#[test]
fn import_stops_at_the_deadline() {
    use crate::dl::entailment::test_hooks::IMPORT_VISIT;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    // Leave generous time to read 10,000 axioms under parallel debug load; the first
    // import visit sleeps just past the deadline so the next poll interval stops it.
    let timeout = Duration::from_secs(15);
    let conclusion = many_restrictions(10_000);
    let (store, _pipeline) = clash_store(
        DlConfig {
            timeout: Some(timeout),
            ..DlConfig::default()
        },
        0,
    );
    let visits = Arc::new(AtomicUsize::new(0));
    let first_before = Arc::new(AtomicBool::new(false));
    let started = std::time::Instant::now();
    let v = Arc::clone(&visits);
    let f = Arc::clone(&first_before);
    IMPORT_VISIT.replace(Some(Box::new(move || {
        if v.fetch_add(1, Ordering::Relaxed) == 0 {
            let elapsed = started.elapsed();
            f.store(elapsed < timeout, Ordering::Relaxed);
            std::thread::sleep(
                timeout
                    .saturating_sub(elapsed)
                    .saturating_add(Duration::from_millis(50)),
            );
        }
    })));
    let result = store.entails_dl(&conclusion).unwrap();
    IMPORT_VISIT.replace(None);
    assert!(
        matches!(&result.answer, Entailed::Unknown(why) if why.contains("dl.timeout")),
        "{result:?}"
    );
    assert!(!result.holds());
    assert!(
        first_before.load(Ordering::Relaxed),
        "first visit must precede deadline"
    );
    let total = visits.load(Ordering::Relaxed);
    assert!(
        total > 0 && total <= 8_192,
        "expected <= 8192 visits, got {total}"
    );
}

/// Without a stop, importing 10,000 restriction axioms visits all 30,000
/// class-expression nodes (memo hits included) and answers Yes over an
/// inconsistent premise.
#[test]
fn import_counts_every_visit_without_stop() {
    use crate::dl::entailment::test_hooks::IMPORT_VISIT;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let conclusion = many_restrictions(10_000);
    let (store, _pipeline) = clash_store(
        DlConfig {
            timeout: Some(Duration::from_secs(60)),
            ..DlConfig::default()
        },
        0,
    );
    let visits = Arc::new(AtomicUsize::new(0));
    let v = Arc::clone(&visits);
    IMPORT_VISIT.replace(Some(Box::new(move || {
        v.fetch_add(1, Ordering::Relaxed);
    })));
    let result = store.entails_dl(&conclusion).unwrap();
    IMPORT_VISIT.replace(None);
    assert_eq!(result.answer, Entailed::Yes);
    assert!(result.holds());
    assert_eq!(result.premise, crate::dl::Verdict::Inconsistent);
    assert_eq!(visits.load(Ordering::Relaxed), 30_000);
}

fn check_shared_dag_import(conclusion: &str, levels: usize) {
    use crate::dl::entailment::test_hooks::IMPORT_VISIT;
    use nrese_rdf_io::{RdfFormat, RdfParser};
    use std::{cell::Cell, rc::Rc};

    let text = format!("{PREFIXES} PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> {conclusion}");
    let triples: Vec<_> = RdfParser::from_format(RdfFormat::Turtle)
        .for_reader(text.as_bytes())
        .map(|quad| Triple::from(quad.expect("conclusion RDF")))
        .collect();
    let (store, _pipeline) = clash_store(
        DlConfig {
            timeout: Some(Duration::from_secs(60)),
            ..DlConfig::default()
        },
        0,
    );
    // Each level has two nodes and at most two visits to the preceding level.
    // The base nodes and axiom root add at most eight visits.
    let limit = 4 * levels + 8;
    let visits = Rc::new(Cell::new(0));
    let counted = Rc::clone(&visits);
    IMPORT_VISIT.replace(Some(Box::new(move || {
        let next = counted.get() + 1;
        counted.set(next);
        assert!(next <= limit, "shared DAG exceeded {limit} import visits");
    })));
    let result = store.entails_dl(&triples).expect("entailment");
    IMPORT_VISIT.replace(None);
    assert_eq!(result.answer, Entailed::Yes, "{result:?}");
    assert!(visits.get() > 0, "the conclusion must reach import");
    eprintln!(
        "{levels} DAG levels: {} visits, bound {limit}",
        visits.get()
    );
}

/// A shared class expression is imported in work linear in its DAG depth.
#[test]
fn import_visits_a_shared_class_dag_linearly() {
    use std::fmt::Write;

    let mut dag = String::from(
        ":p a owl:ObjectProperty . :C a owl:Class . :D a owl:Class .
         _:b0 a owl:Class ; owl:complementOf :D .",
    );
    for i in 1..=40 {
        let prev = i - 1;
        write!(
            dag,
            " _:b{i} a owl:Class ; owl:intersectionOf ( _:b{prev} _:c{i} ) .
              _:c{i} a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom _:b{prev} ."
        )
        .unwrap();
    }
    dag.push_str(" :C rdfs:subClassOf _:b40 .");
    check_shared_dag_import(&dag, 40);
}

/// A shared data range is imported in work linear in its DAG depth.
#[test]
fn import_visits_a_shared_range_dag_linearly() {
    use std::fmt::Write;

    let mut dag = String::from(":d a owl:DatatypeProperty ; rdfs:range _:r32 .");
    let mut prev = "xsd:integer".to_owned();
    for i in 1..=32 {
        write!(
            dag,
            " _:r{i} a rdfs:Datatype ; owl:intersectionOf ( {prev} _:n{i} ) .
              _:n{i} a rdfs:Datatype ; owl:datatypeComplementOf {prev} ."
        )
        .unwrap();
        prev = format!("_:r{i}");
    }
    check_shared_dag_import(&dag, 32);
}

fn rdf_list(prefix: &str, items: &[nrese_rdf::Term], out: &mut Vec<Triple>) -> BlankNode {
    let first = nrese_rdf::vocab::rdf::FIRST.into_owned();
    let rest = nrese_rdf::vocab::rdf::REST.into_owned();
    let nil: nrese_rdf::Term = nrese_rdf::vocab::rdf::NIL.into_owned().into();
    let mut head = None;
    for (i, item) in items.iter().enumerate() {
        let cell = BlankNode::new_unchecked(format!("{prefix}{i}"));
        if head.is_none() {
            head = Some(cell.clone());
        }
        out.push(Triple::new(cell.clone(), first.clone(), item.clone()));
        let next = if i + 1 < items.len() {
            nrese_rdf::Term::BlankNode(BlankNode::new_unchecked(format!("{prefix}{}", i + 1)))
        } else {
            nil.clone()
        };
        out.push(Triple::new(cell, rest.clone(), next));
    }
    head.expect("nonempty list")
}

fn wide_one_of(count: usize) -> Vec<Triple> {
    let rdf_type = nrese_rdf::vocab::rdf::TYPE.into_owned();
    let sub_class =
        nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#subClassOf");
    let owl_class = nrese_rdf::NamedNode::new_unchecked(format!("{OWL}Class"));
    let owl_one_of = nrese_rdf::NamedNode::new_unchecked(format!("{OWL}oneOf"));
    let mut triples = Vec::with_capacity(count * 2 + 4);
    triples.push(Triple::new(iri("C"), rdf_type.clone(), owl_class.clone()));
    let items: Vec<nrese_rdf::Term> = (0..count).map(|i| iri(&format!("i{i}")).into()).collect();
    let head = rdf_list("l", &items, &mut triples);
    let expr = BlankNode::new_unchecked("e");
    triples.push(Triple::new(iri("C"), sub_class, expr.clone()));
    triples.push(Triple::new(expr.clone(), rdf_type, owl_class));
    triples.push(Triple::new(expr, owl_one_of, head));
    triples
}

fn wide_all_different(count: usize) -> Vec<Triple> {
    let rdf_type = nrese_rdf::vocab::rdf::TYPE.into_owned();
    let all_diff = nrese_rdf::NamedNode::new_unchecked(format!("{OWL}AllDifferent"));
    let members = nrese_rdf::NamedNode::new_unchecked(format!("{OWL}members"));
    let mut triples = Vec::with_capacity(count * 2 + 2);
    let items: Vec<nrese_rdf::Term> = (0..count).map(|i| iri(&format!("i{i}")).into()).collect();
    let head = rdf_list("d", &items, &mut triples);
    let node = BlankNode::new_unchecked("ad");
    triples.push(Triple::new(node.clone(), rdf_type, all_diff));
    triples.push(Triple::new(node, members, head));
    triples
}

/// Wide expression and axiom payloads are refused at import entry before
/// cloning or interning their operand vectors. Per-node deadline polling by
/// operand width is omitted: one node's clone takes microseconds and the entry
/// reservation already charges for it.
#[test]
fn wide_payloads_are_charged_before_they_are_copied() {
    use crate::dl::entailment::test_hooks::{IMPORT_CLONED_OPERANDS, IMPORT_ENTRY};
    use nrese_exec::Reservation;

    let wide = [
        ("oneOf", wide_one_of(16_384)),
        ("AllDifferent", wide_all_different(16_384)),
    ];
    let small = [
        ("oneOf-16", wide_one_of(16)),
        ("AllDifferent-16", wide_all_different(16)),
    ];
    let (store, _pipeline) = clash_store(DlConfig::default(), 64 << 20);
    // Headroom: memo + stack + fixed entries + half of one 16,384-member u64 vector.
    let headroom = (16_384 / 2) * std::mem::size_of::<u64>() + 8_192;

    for (name, conclusion) in &wide {
        IMPORT_CLONED_OPERANDS.set(0);
        IMPORT_ENTRY.replace(Some(Box::new(move |envelope| {
            let envelope = envelope.expect("bounded envelope");
            let take = envelope.remaining().saturating_sub(headroom);
            let mut held = Reservation::new(envelope.clone());
            held.charge(take).expect("reserve headroom");
            Some(held)
        })));
        let mem = store.entails_dl(conclusion).unwrap();
        IMPORT_ENTRY.replace(None);
        assert!(
            matches!(&mem.answer, Entailed::Unknown(why) if why.contains("memory")),
            "{name} memory: {mem:?}"
        );
        assert!(!mem.holds());
        assert_eq!(mem.premise, crate::dl::Verdict::Inconsistent);
        assert_eq!(
            IMPORT_CLONED_OPERANDS.get(),
            0,
            "{name} cloned wide payload before memory charge"
        );
    }

    // Small controls without seam.
    for (name, conclusion) in &small {
        let ok = store.entails_dl(conclusion).unwrap();
        assert_eq!(ok.answer, Entailed::Yes, "{name}: {ok:?}");
        assert!(ok.holds());
        assert_eq!(ok.premise, crate::dl::Verdict::Inconsistent);
        assert!(matches!(
            store.entails(dl_rules(), conclusion),
            Ok(crate::Entailment::InconsistentPremise)
        ));
    }
}

fn long_blanks(count: usize, label_len: usize) -> Vec<Triple> {
    let rdf_type = nrese_rdf::vocab::rdf::TYPE.into_owned();
    let p = iri("p");
    let a = iri("a");
    let mut triples = Vec::with_capacity(count + 1);
    triples.push(Triple::new(
        p.clone(),
        rdf_type,
        nrese_rdf::NamedNode::new_unchecked(format!("{OWL}ObjectProperty")),
    ));
    let pad = "x".repeat(label_len);
    for i in 0..count {
        let blank = BlankNode::new_unchecked(format!("b{i:04}_{pad}"));
        triples.push(Triple::new(blank, p.clone(), a.clone()));
    }
    triples
}

/// Spec (contracts.md, Memory): fresh-blank renaming storage is charged to the request's
/// memory envelope before retention, so distinct long blank labels cannot grow an uncharged
/// map past a tight envelope; a small control with one-attachment assertions is admitted.
#[test]
fn fresh_blank_storage_is_charged_before_retention() {
    use super::blank_test_hooks;

    let many = long_blanks(256, 2_048);
    let small = long_blanks(4, 16);

    // Tight envelope: room for the tiny CLASH premise, far below 256 * 2 KiB of blank labels.
    let (tight_store, _pipeline) = clash_store(DlConfig::default(), 96 << 10);
    blank_test_hooks::reset();
    let refused = tight_store.entails_dl(&many).unwrap();
    let (held, charged, uncharged) = blank_test_hooks::snapshot();
    assert!(
        matches!(&refused.answer, Entailed::Unknown(why) if why.contains("memory")),
        "tight envelope must refuse with memory Unknown: {refused:?}"
    );
    assert!(!refused.holds());
    assert_eq!(
        uncharged, 0,
        "renaming storage grew uncharged: held={held}, charged={charged}"
    );

    // Small admitted control with generous envelope.
    let (ok_store, _ok_pipeline) = clash_store(DlConfig::default(), 64 << 20);
    blank_test_hooks::reset();
    let admitted = ok_store.entails_dl(&small).unwrap();
    let (ctrl_held, ctrl_charged, ctrl_uncharged) = blank_test_hooks::snapshot();
    assert_eq!(
        admitted.answer,
        Entailed::Yes,
        "small control: {admitted:?}"
    );
    assert!(admitted.holds());
    assert_eq!(admitted.premise, crate::dl::Verdict::Inconsistent);
    assert_eq!(
        ctrl_uncharged, 0,
        "small control renaming storage uncharged: held={ctrl_held}, charged={ctrl_charged}"
    );
}

/// Spec (contracts.md, Memory): fresh-blank renaming must cover the simultaneous peak of
/// the owned map key, cached mapped value, and live returned candidate on both insertion
/// and cache-hit reuse, refusing before allocation when the remaining envelope is too small.
#[test]
fn fresh_blank_peak_covers_live_candidate() {
    use nrese_dl::limits::Deadline;
    use nrese_dl::memory::Scratch;
    use nrese_exec::{Envelope, Reservation};
    use std::collections::HashMap;

    const L: usize = 256 << 10;
    let label = BlankNode::new_unchecked("a".repeat(L));
    let store = StoreService::new(StoreConfig::in_memory()).unwrap();
    let snapshot = store.engine().snapshot();

    let live_peak = |blanks: &HashMap<String, BlankNode>, returned: &BlankNode| -> usize {
        blanks.capacity() * (std::mem::size_of::<(String, BlankNode)>() + 1)
            + blanks
                .iter()
                .map(|(k, v)| k.capacity() + v.as_str().len())
                .sum::<usize>()
            + returned.as_str().len()
    };

    // Tight cell: remaining envelope 2L + 64 KiB cannot hold key + cached value + returned candidate (3L).
    let tight_limit = 2 * L + (64 << 10);
    let tight_env = Envelope::exact(Some(tight_limit), None);
    let tx = crate::dl::source::private_over(store.engine(), &snapshot, Arc::new(tight_env.part()));
    let mut scratch = Scratch::new(Some(&tight_env));
    let mut blanks = super::new_blank_map(1, &mut scratch).expect("table fits");
    let mut probes = 0u64;
    match super::fresh_blank(
        &tx,
        &mut blanks,
        &mut scratch,
        &mut probes,
        Deadline::NONE,
        &label,
    ) {
        Err(nrese_engine::EngineError::MemoryLimit(_)) => {
            assert!(blanks.is_empty(), "refused insert must not retain entry");
        }
        Ok(Some(cand)) => {
            let peak = live_peak(&blanks, &cand);
            assert!(
                peak <= scratch.bytes() && peak <= tight_limit,
                "live peak {peak} exceeded reservation {} (envelope {tight_limit})",
                scratch.bytes()
            );
        }
        other => panic!("unexpected tight fresh_blank result: {other:?}"),
    }

    // Roomy control and repeated-label hit (both tight and roomy reuse).
    let roomy_limit = 4 * L + (64 << 10);
    let roomy_env = Envelope::exact(Some(roomy_limit), None);
    let tx = crate::dl::source::private_over(store.engine(), &snapshot, Arc::new(roomy_env.part()));
    let mut scratch = Scratch::new(Some(&roomy_env));
    let mut blanks = super::new_blank_map(1, &mut scratch).expect("table fits");
    let first = super::fresh_blank(
        &tx,
        &mut blanks,
        &mut scratch,
        &mut probes,
        Deadline::NONE,
        &label,
    )
    .expect("roomy insert")
    .expect("no deadline");
    let first_peak = live_peak(&blanks, &first);
    assert!(
        first_peak <= scratch.bytes() && first_peak <= roomy_limit,
        "roomy insert peak {first_peak} exceeded reservation {} (limit {roomy_limit})",
        scratch.bytes()
    );
    let first_len = first.as_str().len();
    drop(first);
    scratch.release(first_len);

    // Cache hit with less remaining headroom than one cloned candidate must refuse before cloning.
    let mut drain = Reservation::new(roomy_env.clone());
    let leave = 64 << 10;
    drain
        .charge(roomy_env.remaining().saturating_sub(leave))
        .expect("drain headroom");
    match super::fresh_blank(
        &tx,
        &mut blanks,
        &mut scratch,
        &mut probes,
        Deadline::NONE,
        &label,
    ) {
        Err(nrese_engine::EngineError::MemoryLimit(_)) => {}
        Ok(Some(hit)) => {
            let peak = live_peak(&blanks, &hit);
            assert!(
                peak <= scratch.bytes(),
                "tight cache-hit peak {peak} exceeded reservation {}",
                scratch.bytes()
            );
        }
        other => panic!("unexpected tight cache-hit result: {other:?}"),
    }
    drop(drain);

    // Roomy cache hit succeeds, stays within its reservation, and returns the cached blank.
    let second = super::fresh_blank(
        &tx,
        &mut blanks,
        &mut scratch,
        &mut probes,
        Deadline::NONE,
        &label,
    )
    .expect("roomy cache hit")
    .expect("no deadline");
    let hit_peak = live_peak(&blanks, &second);
    assert!(
        hit_peak <= scratch.bytes() && hit_peak <= roomy_limit,
        "roomy cache-hit peak {hit_peak} exceeded reservation {} (limit {roomy_limit})",
        scratch.bytes()
    );
    assert_eq!(blanks.get(label.as_str()), Some(&second));
}

/// Spec (contracts.md, Memory): conclusion blank-node renaming sizes its map once from the
/// conclusion's blank positions before allocating it, so crossing a hash-table growth
/// threshold never resizes past its charged capacity or overlaps two bucket arrays; a
/// repeated label keeps one fresh identity.
#[test]
fn fresh_blank_resize_stays_within_envelope() {
    use super::blank_test_hooks;

    let rdf_type = nrese_rdf::vocab::rdf::TYPE.into_owned();
    let p = iri("p");
    let q = iri("q");
    let a = iri("a");
    let mut distinct = Vec::with_capacity(899);
    distinct.push(Triple::new(
        p.clone(),
        rdf_type.clone(),
        nrese_rdf::NamedNode::new_unchecked(format!("{OWL}ObjectProperty")),
    ));
    distinct.push(Triple::new(
        q.clone(),
        rdf_type,
        nrese_rdf::NamedNode::new_unchecked(format!("{OWL}ObjectProperty")),
    ));
    for i in 0..897usize {
        distinct.push(Triple::new(
            BlankNode::new_unchecked(format!("b{i:07}")),
            p.clone(),
            a.clone(),
        ));
    }

    // Tight cell around the 1,024 -> 2,048 bucket threshold: never resize past charged capacity.
    let (tight_store, _pipeline) = clash_store(DlConfig::default(), 164_952);
    blank_test_hooks::reset();
    let tight = tight_store.entails_dl(&distinct).unwrap();
    let (tight_held, tight_charged, tight_uncharged) = blank_test_hooks::snapshot();
    let (tight_resizes, tight_overlap_uncharged) = blank_test_hooks::resize_snapshot();
    assert!(
        tight.answer == Entailed::Yes
            || matches!(&tight.answer, Entailed::Unknown(why) if why.contains("memory")),
        "unexpected tight answer: {tight:?}"
    );
    assert_eq!(
        tight_resizes, 0,
        "renaming map resized {tight_resizes} times under tight envelope"
    );
    assert_eq!(
        tight_uncharged, 0,
        "tight held={tight_held} exceeded charged={tight_charged}"
    );
    assert_eq!(
        tight_overlap_uncharged, 0,
        "tight resize overlap exceeded charged={tight_charged} by {tight_overlap_uncharged}"
    );

    // Roomy control: all 897 distinct blanks succeed with zero resizes and full accounting.
    let (roomy_store, _roomy_pipeline) = clash_store(DlConfig::default(), 64 << 20);
    blank_test_hooks::reset();
    let roomy = roomy_store.entails_dl(&distinct).unwrap();
    let (held, charged, uncharged) = blank_test_hooks::snapshot();
    let (resizes, overlap_uncharged) = blank_test_hooks::resize_snapshot();
    assert_eq!(roomy.answer, Entailed::Yes, "roomy 897 blanks: {roomy:?}");
    assert!(roomy.holds());
    assert_eq!(resizes, 0, "renaming map resized {resizes} times");
    assert_eq!(uncharged, 0, "held={held} exceeded charged={charged}");
    assert_eq!(
        overlap_uncharged, 0,
        "resize overlap exceeded charged={charged} by {overlap_uncharged}"
    );

    // Repeated-label control: repeating b0000000 on a second property maps to the same fresh
    // blank identity, producing a two-attachment anonymous individual (Unknown, not Yes).
    let mut repeated = distinct;
    repeated.push(Triple::new(BlankNode::new_unchecked("b0000000"), q, a));
    blank_test_hooks::reset();
    let rep = roomy_store.entails_dl(&repeated).unwrap();
    let (rep_resizes, rep_overlap) = blank_test_hooks::resize_snapshot();
    assert!(
        matches!(&rep.answer, Entailed::Unknown(why) if why.contains("anonymous")),
        "repeated blank must keep one identity and report anonymous restriction: {rep:?}"
    );
    assert_eq!(
        rep_resizes, 0,
        "repeated-label map resized {rep_resizes} times"
    );
    assert_eq!(
        rep_overlap, 0,
        "repeated-label resize overlap uncharged: {rep_overlap}"
    );
}
