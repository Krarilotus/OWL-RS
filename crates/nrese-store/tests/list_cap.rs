//! A list axiom whose members have aliases still holds, however many names they have and
//! however it came into the store.
//!
//! Spec: OWL 2 Profiles §4.3, Tables 4, 5 and 9 (`eq-diff2`, `prp-adp`, `cax-adc`): a pairwise
//! axiom over a list is broken by two different members meeting, whatever other names the
//! members have; README: with reasoning on, a write found to make the data inconsistent is
//! refused, and the store is left as it was.
//!
//! Each member here has four aliases, so over every identity the list has 5 * 5 * 5 = 125
//! member sequences. The axiom and the aliases come in by a write, by a write before a
//! restart (a new pipeline over the same store), by a write and a rematerialisation, or by a
//! bulk load and a rematerialisation; then a write brings two members together.

use std::path::PathBuf;
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    BulkLoadRequest, ConsistencyStatus, GraphTarget, MutationCommand, MutationError,
    MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest, StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX ex: <http://example.org/> \
     PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#>";
const EX: &str = "http://example.org/";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const MEMBERS: [&str; 3] = ["m1", "m2", "m3"];
/// The equality modes: every identity replicated, by representatives (the default), compact.
const MODES: [&str; 3] = ["replication", "representatives", "compact"];

/// An in-memory store's configuration under `mode`.
fn config(mode: &str) -> StoreConfig {
    let mut config = StoreConfig::in_memory();
    config.equality_by_representatives = mode != "replication";
    config.equality_compact = mode == "compact";
    config
}

fn pipeline(store: &Arc<StoreService>) -> MutationPipeline {
    MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

fn program() -> nrese_reasoner::RuleProgram {
    ReasonerConfig::for_mode(ReasoningMode::Owl2Rl)
        .materialised_program()
        .expect("a reasoning mode")
}

fn apply(pipeline: &MutationPipeline, body: &str) -> Result<(), MutationError> {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}\n{body}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .map(drop)
}

fn holds(store: &StoreService, pattern: &str) -> bool {
    let answer = store
        .execute_query_str(&format!("{PREFIXES}\nASK {{ {pattern} }}"))
        .expect("answered");
    String::from_utf8(answer.payload)
        .expect("text")
        .contains("true")
}

/// One pairwise axiom over `(ex:m1 ex:m2 ex:m3)`: its type, a fact the first write holds
/// beside it (as SPARQL and as N-Triples), the write that brings two members together, and
/// the rule that write breaks.
struct Case {
    kind: &'static str,
    other: &'static str,
    other_nt: String,
    meeting: &'static str,
    rule: &'static str,
}

/// A store holding `case`'s axiom, the aliases and its other fact, brought in `how`, and the
/// pipeline to write to next.
fn with_axiom(case: &Case, how: &str, mode: &str) -> (Arc<StoreService>, MutationPipeline) {
    let store = Arc::new(StoreService::new(config(mode)).expect("store"));
    let aliases = || {
        MEMBERS
            .iter()
            .flat_map(|m| (1..=4).map(move |i| (*m, format!("{m}-alias{i}"))))
    };
    if how == "bulk load" {
        let mut nt = format!(
            "_:ad <{RDF}type> <{OWL}{kind}> .\n_:ad <{OWL}members> _:l1 .\n\
             _:l1 <{RDF}first> <{EX}m1> .\n_:l1 <{RDF}rest> _:l2 .\n\
             _:l2 <{RDF}first> <{EX}m2> .\n_:l2 <{RDF}rest> _:l3 .\n\
             _:l3 <{RDF}first> <{EX}m3> .\n_:l3 <{RDF}rest> <{RDF}nil> .\n{other}",
            kind = case.kind,
            other = case.other_nt,
        );
        for (m, alias) in aliases() {
            nt.push_str(&format!("<{EX}{m}> <{OWL}sameAs> <{EX}{alias}> .\n"));
        }
        let file = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("list-cap-{}-{mode}.nt", case.rule));
        std::fs::write(&file, nt).expect("write the data");
        store
            .bulk_load(&BulkLoadRequest {
                files: vec![file.clone()],
                replace: false,
                graph: GraphTarget::DefaultGraph,
                invalid: None,
            })
            .expect("load");
        std::fs::remove_file(&file).expect("remove the data");
        store.rematerialise(program()).expect("rematerialise");
        let commits = pipeline(&store);
        return (store, commits);
    }
    let same: String = aliases()
        .map(|(m, alias)| format!("ex:{m} owl:sameAs ex:{alias} . "))
        .collect();
    let first = pipeline(&store);
    apply(
        &first,
        &format!(
            "INSERT DATA {{ _:ad rdf:type owl:{} . _:ad owl:members (ex:m1 ex:m2 ex:m3) . \
             {same} {} }}",
            case.kind, case.other
        ),
    )
    .unwrap_or_else(|e| panic!("{how}: the axiom and its aliases commit: {e}"));
    match how {
        "restart" => {
            drop(first);
            let commits = pipeline(&store);
            (store, commits)
        }
        "rematerialise" => {
            store.rematerialise(program()).expect("rematerialise");
            (store, first)
        }
        _ => (store, first),
    }
}

#[test]
fn two_members_of_an_aliased_list_axiom_meeting_is_refused() {
    let cases = [
        Case {
            kind: "AllDifferent",
            other: "",
            other_nt: String::new(),
            meeting: "ex:m1 owl:sameAs ex:m2 .",
            rule: "eq-diff2",
        },
        Case {
            kind: "AllDisjointClasses",
            other: "ex:x rdf:type ex:m1 .",
            other_nt: format!("<{EX}x> <{RDF}type> <{EX}m1> .\n"),
            meeting: "ex:x rdf:type ex:m2 .",
            rule: "cax-adc",
        },
        Case {
            kind: "AllDisjointProperties",
            other: "ex:x ex:m1 ex:y .",
            other_nt: format!("<{EX}x> <{EX}m1> <{EX}y> .\n"),
            meeting: "ex:x ex:m2 ex:y .",
            rule: "prp-adp",
        },
    ];
    let mut accepted = Vec::new();
    for case in &cases {
        for how in ["a write", "restart", "rematerialise", "bulk load"] {
            for mode in MODES {
                let rule = case.rule;
                let name = format!("{rule}, {how} ({mode})");
                let (store, commits) = with_axiom(case, how, mode);
                assert_eq!(
                    store.consistency(),
                    ConsistencyStatus::Consistent,
                    "{name}: the axiom and its aliases are consistent"
                );
                let revision = store.current_revision();
                // The refusal printed: what it names.
                match apply(&commits, &format!("INSERT DATA {{ {} }}", case.meeting)) {
                    Err(MutationError::Rejected(reject)) => {
                        println!("{name}: refused: {}", reject.detail);
                        assert!(
                            reject.detail.contains(rule),
                            "{name}: refused for {rule}, not: {}",
                            reject.detail
                        )
                    }
                    other => {
                        accepted.push(format!("{name}: {other:?}, {:?}", store.consistency()));
                        continue;
                    }
                }
                assert_eq!(
                    store.current_revision(),
                    revision,
                    "{name}: nothing published"
                );
                assert!(
                    !holds(&store, case.meeting),
                    "{name}: the refused fact isn't there"
                );
                apply(&commits, "INSERT DATA { ex:c ex:q ex:d . }").unwrap_or_else(|e| {
                    panic!("{name}: a write that brings no clash commits: {e}")
                });
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "each meeting is refused; accepted: {accepted:#?}"
    );
}

fn store_and_pipeline(mode: &str) -> (Arc<StoreService>, MutationPipeline) {
    let store = Arc::new(StoreService::new(config(mode)).expect("store"));
    let commits = pipeline(&store);
    (store, commits)
}

fn refused_for(result: Result<(), MutationError>, rule: &str) -> Result<(), String> {
    match result {
        Err(MutationError::Rejected(reject)) if reject.detail.contains(rule) => Ok(()),
        other => Err(format!("refused for {rule}, not {other:?}")),
    }
}

/// Four aliases of each member, as SPARQL.
fn aliases() -> String {
    MEMBERS
        .iter()
        .flat_map(|m| (1..=4).map(move |i| format!("ex:{m} owl:sameAs ex:{m}-alias{i} . ")))
        .collect()
}

/// A list is read by the names it was written with, not by an alias that goes. Each member has
/// four aliases, and `ex:a0`, interned before `ex:m1`, is `ex:m1`'s alias; then that equality
/// is deleted. After it, `ex:a0` is no member: `ex:a0 owl:sameAs ex:m2` is consistent and
/// commits, and `ex:m1 owl:sameAs ex:m2` breaks `eq-diff2` and is refused.
#[test]
fn a_list_follows_its_member_when_an_alias_goes() {
    for mode in MODES {
        let case = format!("{mode}");
        let (store, commits) = store_and_pipeline(mode);
        for body in [
            "INSERT DATA { ex:a0 ex:r ex:z . }".to_owned(),
            format!(
                "INSERT DATA {{ _:ad rdf:type owl:AllDifferent . \
                 _:ad owl:members (ex:m1 ex:m2 ex:m3) . ex:a0 owl:sameAs ex:m1 . {} }}",
                aliases()
            ),
            "DELETE DATA { ex:a0 owl:sameAs ex:m1 . }".to_owned(),
        ] {
            apply(&commits, &body).unwrap_or_else(|e| panic!("{case}: {body} commits: {e}"));
        }
        assert!(
            !holds(&store, "ex:a0 owl:sameAs ex:m1"),
            "{case}: the deleted equality is gone"
        );
        apply(&commits, "INSERT DATA { ex:a0 owl:sameAs ex:m2 . }")
            .unwrap_or_else(|e| panic!("{case}: a name no longer a member may meet one: {e}"));
        refused_for(
            apply(&commits, "INSERT DATA { ex:m1 owl:sameAs ex:m2 . }"),
            "eq-diff2",
        )
        .unwrap_or_else(|e| panic!("{case}: the members meeting: {e}"));
    }
}

/// One commit that makes two members equal through a third name (`ex:m1 owl:sameAs ex:x`,
/// `ex:x owl:sameAs ex:m2`) breaks `eq-diff2` and is refused, with or without the members'
/// four aliases each.
#[test]
fn one_commit_that_makes_two_members_equal_through_a_third_name_is_refused() {
    for aliased in [false, true] {
        for mode in MODES {
            let case = format!("aliased: {aliased}, {mode}");
            let (_store, commits) = store_and_pipeline(mode);
            let aliases = if aliased { aliases() } else { String::new() };
            apply(
                &commits,
                &format!(
                    "INSERT DATA {{ _:ad rdf:type owl:AllDifferent . \
                     _:ad owl:members (ex:m1 ex:m2 ex:m3) . {aliases} }}"
                ),
            )
            .unwrap_or_else(|e| panic!("{case}: the axiom commits: {e}"));
            refused_for(
                apply(
                    &commits,
                    "INSERT DATA { ex:m1 owl:sameAs ex:x . ex:x owl:sameAs ex:m2 . }",
                ),
                "eq-diff2",
            )
            .unwrap_or_else(|e| panic!("{case}: {e}"));
        }
    }
}

/// The axiom written after its list and its members' aliases (four each: 125 member sequences
/// over every identity), then the members meeting: refused for `eq-diff2`. The axiom's
/// commit grounds the list over a closure that holds the copies already. On the pipeline
/// that took every write, after a restart before the axiom (the commit that writes it
/// grounds it with no program cached), and after a restart before the meeting; under each
/// equality mode.
#[test]
fn an_axiom_written_after_its_members_aliases_is_broken_by_them_meeting() {
    for restart in ["none", "before the axiom", "before the meeting"] {
        for mode in MODES {
            let case = format!("restart {restart}, {mode}");
            let (store, mut commits) = store_and_pipeline(mode);
            let list = "ex:l1 rdf:first ex:m1 . ex:l1 rdf:rest ex:l2 . ex:l2 rdf:first ex:m2 . \
                        ex:l2 rdf:rest ex:l3 . ex:l3 rdf:first ex:m3 . ex:l3 rdf:rest rdf:nil .";
            apply(&commits, &format!("INSERT DATA {{ {list} {} }}", aliases()))
                .unwrap_or_else(|e| panic!("{case}: the list and the aliases commit: {e}"));
            if restart == "before the axiom" {
                commits = pipeline(&store);
            }
            apply(
                &commits,
                "INSERT DATA { _:ad rdf:type owl:AllDifferent . _:ad owl:members ex:l1 . }",
            )
            .unwrap_or_else(|e| panic!("{case}: the axiom commits: {e}"));
            if restart == "before the meeting" {
                commits = pipeline(&store);
            }
            refused_for(
                apply(&commits, "INSERT DATA { ex:m1 owl:sameAs ex:m2 . }"),
                "eq-diff2",
            )
            .unwrap_or_else(|e| panic!("{case}: the members meeting: {e}"));
        }
    }
}

/// A list cell's member that only a rule gives (`ex:altFirst rdfs:subPropertyOf rdf:first`,
/// `prp-spo1`) is read with the one written. Spec: OWL 2 Profiles §4.3, `eq-diff2` over the
/// `owl:AllDifferent` list `(ex:a ex:b)`, whose second cell has `ex:c` as a member too: the
/// graph holds `(ex:a ex:c)` as well, so `ex:a owl:sameAs ex:c` breaks it and is refused, on
/// the pipeline that took the axiom and after a restart (no program cached); under each
/// equality mode.
#[test]
fn a_member_a_rule_gives_a_list_cell_is_read_with_the_written_one() {
    for restart in [false, true] {
        for mode in MODES {
            let case = format!("restart: {restart}, {mode}");
            let (store, mut commits) = store_and_pipeline(mode);
            apply(
                &commits,
                "INSERT DATA { ex:altFirst rdfs:subPropertyOf rdf:first . \
                 _:ad rdf:type owl:AllDifferent . _:ad owl:members ex:l1 . \
                 ex:l1 rdf:first ex:a . ex:l1 rdf:rest ex:l2 . \
                 ex:l2 rdf:first ex:b . ex:l2 rdf:rest rdf:nil . ex:l2 ex:altFirst ex:c . }",
            )
            .unwrap_or_else(|e| panic!("{case}: the axiom commits: {e}"));
            assert!(
                holds(&store, "ex:l2 rdf:first ex:c"),
                "{case}: prp-spo1 gives the cell its second member"
            );
            if restart {
                commits = pipeline(&store);
            }
            refused_for(
                apply(&commits, "INSERT DATA { ex:a owl:sameAs ex:c . }"),
                "eq-diff2",
            )
            .unwrap_or_else(|e| panic!("{case}: the members meeting: {e}"));
        }
    }
}

/// The members of an aliased `owl:AllDifferent` list meeting is refused however the commit
/// that brings them together is judged: a store's first write holding the axiom, the aliases
/// and the meeting (judged by its rebuild); a later write on a store with an inverse property
/// expression (`_:i owl:inverseOf ex:p`: rebuilt on every commit); a later write on a store
/// without one (a delta). Spec: OWL 2 Profiles §4.3, `eq-diff2`; README: a write found to make
/// the data inconsistent is refused. Under each equality mode.
#[test]
fn a_meeting_of_aliased_members_is_refused_however_its_commit_is_judged() {
    let axiom = "_:ad rdf:type owl:AllDifferent . _:ad owl:members (ex:m1 ex:m2 ex:m3) .";
    let meeting = "ex:m1 owl:sameAs ex:m2 .";
    let mut accepted = Vec::new();
    for mode in MODES {
        for how in ["first write", "inverse expression", "later write"] {
            let case = format!("{how} ({mode})");
            let (_store, commits) = store_and_pipeline(mode);
            let result = match how {
                "first write" => apply(
                    &commits,
                    &format!("INSERT DATA {{ {axiom} {} {meeting} }}", aliases()),
                ),
                _ => {
                    let other = match how {
                        "inverse expression" => {
                            "ex:s rdfs:subPropertyOf [ owl:inverseOf ex:p ] . ex:x ex:p ex:y ."
                        }
                        _ => "ex:x ex:p ex:y .",
                    };
                    apply(
                        &commits,
                        &format!("INSERT DATA {{ {other} {axiom} {} }}", aliases()),
                    )
                    .unwrap_or_else(|e| panic!("{case}: the axiom and its aliases commit: {e}"));
                    apply(&commits, &format!("INSERT DATA {{ {meeting} }}"))
                }
            };
            if let Err(e) = refused_for(result, "eq-diff2") {
                accepted.push(format!("{case}: {e}"));
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "each meeting is refused: {accepted:#?}"
    );
}

/// An `owl:AllDisjointClasses` list written `(ex:A ex:B)` from its axiom's head is still
/// checked as it was written where equality merges its cells past the cap, also as written:
/// its second cell the same as `ex:n`, a node with 64 more `rdf:first` values; or its head
/// the same as 64 names, each with its own `rdf:first` and the second cell as its `rdf:rest`.
/// `ex:x` in both classes is refused, however the commit that brings it is judged: a store's
/// first write (judged by its rebuild); a later write on a store with an inverse property
/// expression (rebuilt on every commit); a later write on a store without one (a delta). Spec:
/// OWL 2 Profiles §4.3, `cax-adc`; README: a write found to make the data inconsistent is
/// refused. Under each equality mode.
#[test]
fn a_list_whose_merged_cells_are_past_the_cap_is_checked_as_written() {
    let written = "_:ad rdf:type owl:AllDisjointClasses ; owl:members ex:c1 . \
         ex:c1 rdf:first ex:A ; rdf:rest ex:c2 . ex:c2 rdf:first ex:B ; rdf:rest rdf:nil . \
         ex:x rdf:type ex:A .";
    let cell: String = (1..=64)
        .map(|k| format!("ex:n rdf:first ex:N{k} . "))
        .collect();
    let heads: String = (1..=64)
        .map(|k| format!("ex:h{k} owl:sameAs ex:c1 ; rdf:first ex:N{k} ; rdf:rest ex:c2 . "))
        .collect();
    let lists = [
        (
            "a merged cell",
            format!("{written} ex:c2 owl:sameAs ex:n . ex:n rdf:rest rdf:nil . {cell}"),
        ),
        ("merged heads", format!("{written} {heads}")),
    ];
    let clash = "ex:x rdf:type ex:B .";
    let mut accepted = Vec::new();
    for (name, list) in &lists {
        for mode in MODES {
            for how in ["first write", "inverse expression", "later write"] {
                let case = format!("{name}, {how} ({mode})");
                let (_store, commits) = store_and_pipeline(mode);
                let result = match how {
                    "first write" => apply(&commits, &format!("INSERT DATA {{ {list} {clash} }}")),
                    _ => {
                        let other = match how {
                            "inverse expression" => {
                                "ex:s rdfs:subPropertyOf [ owl:inverseOf ex:p ] . ex:x ex:p ex:y ."
                            }
                            _ => "ex:x ex:p ex:y .",
                        };
                        apply(&commits, &format!("INSERT DATA {{ {other} {list} }}"))
                            .unwrap_or_else(|e| panic!("{case}: the list commits: {e}"));
                        apply(&commits, &format!("INSERT DATA {{ {clash} }}"))
                    }
                };
                if let Err(e) = refused_for(result, "cax-adc") {
                    accepted.push(format!("{case}: {e}"));
                }
            }
        }
    }
    assert!(accepted.is_empty(), "each clash is refused: {accepted:#?}");
}

/// `count` cells `ex:{name}1 …` of a list of `ex:{prefix}k` members (`members` overrides
/// some by place), each cell in `alternatives` with a second member through `ex:alt`.
fn cells(
    name: &str,
    prefix: &str,
    count: usize,
    members: &[(usize, &str)],
    alternatives: &[usize],
) -> String {
    let mut out = String::new();
    for k in 1..=count {
        let member = members
            .iter()
            .find(|(at, _)| *at == k)
            .map_or_else(|| format!("ex:{prefix}{k}"), |(_, m)| (*m).to_owned());
        let rest = match k == count {
            true => "rdf:nil".to_owned(),
            false => format!("ex:{name}{}", k + 1),
        };
        out.push_str(&format!(
            "ex:{name}{k} rdf:first {member} ; rdf:rest {rest} . "
        ));
        if alternatives.contains(&k) {
            out.push_str(&format!("ex:{name}{k} ex:alt ex:{name}Z{k} . "));
        }
    }
    out
}

/// A list's clash that OWL 2 RL/RDF entails from the list as written, where the closure gives
/// the list more than 64 member sequences, is refused: at a store's first write (judged by its
/// rebuild) in every equality mode, and where the case says, at a later write rebuilt (an
/// inverse property expression in the store) and at a later write that is a delta. Spec: OWL 2
/// Profiles §4.3, `cax-adc` (with `prp-spo1` for `ex:alt rdfs:subPropertyOf rdf:first`, and
/// `eq-rep-o`), each clash worked out from the list as written:
/// - a promoted cell: the second cell's members come only through `ex:alt` (65 of them), then
///   `ex:c2 rdf:first ex:B` is written with `ex:x` in `ex:B`: `(ex:A ex:B)`, `ex:x` in both;
/// - a cell with 64 written members and two more through `ex:alt`, `ex:x` in `ex:A` and `ex:B`;
/// - 101 classes, seven cells with a second member through `ex:alt`, `ex:x` in the first two;
/// - 101 classes naming `ex:A` and `ex:B` at two places, `ex:A owl:sameAs ex:B`, `ex:x` in `ex:A`.
#[test]
fn a_clash_the_rule_tables_give_from_a_list_as_written_is_refused_past_the_cap() {
    let alt = "ex:alt rdfs:subPropertyOf rdf:first . ";
    let head = "_:ad rdf:type owl:AllDisjointClasses ; owl:members ex:c1 . \
                ex:c1 rdf:first ex:A ; rdf:rest ex:c2 . ";
    let b = |from: usize| -> String {
        (from..=64)
            .map(|k| format!("ex:B{k}"))
            .collect::<Vec<_>>()
            .join(" , ")
    };
    let long = |members: &[(usize, &str)]| {
        format!(
            "{alt}_:ld rdf:type owl:AllDisjointClasses ; owl:members ex:s1 . {}",
            cells("s", "S", 101, members, &[95, 96, 97, 98, 99, 100, 101])
        )
    };
    let all = ["first write", "inverse expression", "later write"];
    let cases: [(&str, String, &str, &[&str]); 4] = [
        (
            "a promoted cell",
            format!(
                "{alt}{head}ex:c2 rdf:rest rdf:nil ; ex:alt ex:B , {} . ex:x rdf:type ex:A .",
                b(1)
            ),
            "ex:c2 rdf:first ex:B . ex:x rdf:type ex:B .",
            &all[..2],
        ),
        (
            "a cell of 64 written members",
            format!(
                "{alt}{head}ex:c2 rdf:first ex:B , {} ; rdf:rest rdf:nil ; ex:alt ex:Z1 , ex:Z2 . \
                 ex:x rdf:type ex:A .",
                b(2)
            ),
            "ex:x rdf:type ex:B .",
            &all[..1],
        ),
        (
            "a long list",
            format!("{} ex:x rdf:type ex:S1 .", long(&[])),
            "ex:x rdf:type ex:S2 .",
            &all[..1],
        ),
        (
            "a long list naming one class twice",
            format!(
                "{} ex:A owl:sameAs ex:B .",
                long(&[(1, "ex:A"), (50, "ex:B")])
            ),
            "ex:x rdf:type ex:A .",
            &all,
        ),
    ];
    let mut accepted = Vec::new();
    for (name, setup, change, judges) in &cases {
        for mode in MODES {
            for how in judges.iter() {
                let case = format!("{name}, {how} ({mode})");
                let (_store, commits) = store_and_pipeline(mode);
                let result = match *how {
                    "first write" => {
                        apply(&commits, &format!("INSERT DATA {{ {setup} {change} }}"))
                    }
                    _ => {
                        let other = match *how {
                            "inverse expression" => {
                                "ex:s rdfs:subPropertyOf [ owl:inverseOf ex:p ] . ex:x ex:p ex:y ."
                            }
                            _ => "ex:x ex:p ex:y .",
                        };
                        apply(&commits, &format!("INSERT DATA {{ {other} {setup} }}"))
                            .unwrap_or_else(|e| panic!("{case}: the setup commits: {e}"));
                        apply(&commits, &format!("INSERT DATA {{ {change} }}"))
                    }
                };
                if let Err(e) = refused_for(result, "cax-adc") {
                    accepted.push(format!("{case}: {e}"));
                }
            }
        }
    }
    assert!(accepted.is_empty(), "each clash is refused: {accepted:#?}");
}
