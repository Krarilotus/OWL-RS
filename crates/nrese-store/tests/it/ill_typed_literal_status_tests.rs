//! A literal of a datatype of the OWL 2 datatype map whose lexical form is outside that
//! datatype's lexical space is ill-typed (RDF 1.1 Concepts §3.3). OWL 2 doesn't admit one
//! (Structural Specification §5.7), and under D-entailment it is unsatisfiable (RDF 1.1
//! Semantics §7.2): the store's rules see no clash in it without a range, so under
//! `owl2-dl` and `owl2-ql`, while the store holds one, no read that rests on reasoning is
//! told `complete`, the reason names the literal, and the store's consistency is `unknown`,
//! not `consistent`. Under a range the commit is refused, as before.

use std::{path::Path, sync::Arc};

use crate::support::Writes;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    ConsistencyStatus, MutationCommand, MutationPipeline, MutationTicket, Requester,
    SolutionsResultFormat, SparqlQueryRequest, SparqlUpdateRequest, StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX : <http://example.org/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

/// A read that touches no literal: the store's word on every read is withheld.
const READ: &str = "SELECT ?x WHERE { ?x a :U }";

/// The words every withheld status's reason holds.
const REASON: &str = "ill-typed";

/// The ill-typed literals, by the lexical form the reason names: an integer outside
/// `xsd:integer`'s lexical space (the case found), a boolean outside `xsd:boolean`'s, and a
/// string with a character XML 1.0 doesn't allow (the map's `xsd:string`).
const ILL_TYPED: [(&str, &str); 3] = [
    ("\"abc\"^^xsd:integer", "\"abc\""),
    ("\"yes\"^^xsd:boolean", "\"yes\""),
    ("\"zz\\u0001\"", "\"zz"),
];

const RULESETS: [&str; 2] = ["owl2-dl", "owl2-ql"];

fn mode(ruleset: &str) -> ReasoningMode {
    match ruleset {
        "owl2-dl" => ReasoningMode::Owl2Dl,
        "owl2-ql" => ReasoningMode::Owl2Ql,
        "owl2-rl" => ReasoningMode::Owl2Rl,
        other => panic!("no ruleset {other}"),
    }
}

fn open(ruleset: &str, dir: Option<&Path>) -> MutationPipeline {
    let config = dir.map_or_else(StoreConfig::in_memory, StoreConfig::on_disk);
    let store = Arc::new(StoreService::new(config).expect("store"));
    over(&store, ruleset)
}

/// A pipeline of `ruleset` over `store`, as a repository registers its mode.
fn over(store: &Arc<StoreService>, ruleset: &str) -> MutationPipeline {
    MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode(
            ruleset,
        )))),
    )
}

fn commit(pipeline: &MutationPipeline, body: &str) -> Result<(), String> {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{body}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .map(|_| ())
        .map_err(|error| format!("{error:?}"))
}

fn update(pipeline: &MutationPipeline, body: &str) {
    commit(pipeline, body).unwrap_or_else(|error| panic!("{body}: {error}"));
}

/// [`READ`]'s word and its reasons' texts.
fn read(store: &StoreService) -> (String, Vec<String>) {
    let mut request = SparqlQueryRequest::all(format!("{PREFIXES}{READ}"));
    request.solutions_format = SolutionsResultFormat::Json;
    let result = store.execute_query(&request).expect("read");
    let status = result.completeness.expect("a reasoned read has a status");
    let reasons = status.reasons.iter().map(|r| r.text.clone()).collect();
    (status.as_str().to_owned(), reasons)
}

/// What `store` says of itself, as `case: word reasons, consistency`, where it is not
/// withheld for an ill-typed literal naming `named` (`None`: naming any), with its
/// consistency `unknown`.
fn not_withheld(store: &StoreService, case: &str, named: Option<&str>) -> Option<String> {
    let (word, reasons) = read(store);
    let consistency = store.consistency();
    let reason = reasons
        .iter()
        .any(|r| r.contains(REASON) && named.is_none_or(|n| r.contains(n)));
    let withheld = word != "complete" && reason && consistency == ConsistencyStatus::Unknown;
    (!withheld).then(|| format!("{case}: {word} {reasons:?}, {consistency:?}"))
}

/// What `store` says of itself where it isn't `complete` and `consistent`.
fn not_complete(store: &StoreService, case: &str) -> Option<String> {
    let (word, reasons) = read(store);
    let consistency = store.consistency();
    (word != "complete" || consistency != ConsistencyStatus::Consistent)
        .then(|| format!("{case}: {word} {reasons:?}, {consistency:?}"))
}

fn assert_none(unmet: &[String], what: &str) {
    assert!(unmet.is_empty(), "{what}:\n{}", unmet.join("\n"));
}

/// Spec: the ruling on the reference route (an ill-typed literal leaves the store
/// `Consistent` and its reads `complete`): with `:a :d "abc"^^xsd:integer` and no range on
/// `:d`, the commit is applied, the read is withheld naming the literal, and the consistency
/// is `unknown`. The same for each ill-typed literal of [`ILL_TYPED`].
#[test]
fn a_store_holding_an_ill_typed_literal_is_told_neither_complete_nor_consistent() {
    let mut unmet = Vec::new();
    for ruleset in RULESETS {
        for (literal, named) in ILL_TYPED {
            let pipeline = open(ruleset, None);
            update(&pipeline, &format!("INSERT DATA {{ :a :d {literal} }}"));
            let case = format!("{ruleset} {literal}");
            unmet.extend(not_withheld(pipeline.store(), &case, Some(named)));
        }
    }
    assert_none(
        &unmet,
        "a store holding an ill-typed literal is told otherwise",
    );
}

/// Controls: a well-typed `"5"^^xsd:integer` keeps the word and `consistent`; under a range
/// of `xsd:integer`, the ill-typed literal's commit is refused (dt-not-type), as before.
#[test]
fn a_well_typed_literal_keeps_the_word_and_a_range_still_refuses_an_ill_typed_one() {
    let mut unmet = Vec::new();
    for ruleset in RULESETS {
        let pipeline = open(ruleset, None);
        update(&pipeline, "INSERT DATA { :a :d \"5\"^^xsd:integer }");
        unmet.extend(not_complete(
            pipeline.store(),
            &format!("{ruleset} well-typed"),
        ));
        let ranged = open(ruleset, None);
        update(&ranged, "INSERT DATA { :d rdfs:range xsd:integer }");
        if commit(&ranged, "INSERT DATA { :a :d \"abc\"^^xsd:integer }").is_ok() {
            unmet.push(format!("{ruleset}: under a range the commit is applied"));
        }
    }
    assert_none(&unmet, "a control is told otherwise");
}

/// The state holds across a restart: the store, written on disk, reopened without a rebuild,
/// still withholds its first read and its consistency; a well-typed store, reopened so, is
/// `complete` and `consistent` at its first read.
#[test]
fn a_store_reopened_without_a_rebuild_still_withholds() {
    let mut unmet = Vec::new();
    for ruleset in RULESETS {
        for (literal, ill_typed) in [
            ("\"abc\"^^xsd:integer", true),
            ("\"5\"^^xsd:integer", false),
        ] {
            let dir = tempfile::tempdir().expect("dir");
            {
                let before = open(ruleset, Some(dir.path()));
                update(&before, &format!("INSERT DATA {{ :a :d {literal} }}"));
            }
            let after = open(ruleset, Some(dir.path()));
            assert!(
                after.store().last_materialisation().is_none(),
                "{ruleset}: the reopened store was rematerialised"
            );
            let case = format!("{ruleset} {literal}");
            unmet.extend(match ill_typed {
                true => not_withheld(after.store(), &case, Some("\"abc\"")),
                false => not_complete(after.store(), &case),
            });
        }
    }
    assert_none(&unmet, "a reopened store is told otherwise");
}

/// Deleting the literal gives the word and `consistent` back; on disk, after a reopen without
/// a rebuild, too, with no rebuild (a store in memory rebuilds at its first commit anyway).
#[test]
fn deleting_the_ill_typed_literal_gives_the_word_back() {
    let mut unmet = Vec::new();
    for ruleset in RULESETS {
        let dir = tempfile::tempdir().expect("dir");
        for (case, disk) in [("in memory", None), ("reopened", Some(dir.path()))] {
            if let Some(dir) = disk {
                let before = open(ruleset, Some(dir));
                update(&before, "INSERT DATA { :a :d \"abc\"^^xsd:integer }");
            }
            let pipeline = open(ruleset, disk);
            if disk.is_none() {
                update(&pipeline, "INSERT DATA { :a :d \"abc\"^^xsd:integer }");
            }
            update(&pipeline, "DELETE DATA { :a :d \"abc\"^^xsd:integer }");
            let case = format!("{ruleset} {case}");
            unmet.extend(not_complete(pipeline.store(), &case));
            if disk.is_some() && pipeline.store().last_materialisation().is_some() {
                unmet.push(format!("{case}: rematerialised"));
            }
        }
    }
    assert_none(
        &unmet,
        "a store whose ill-typed literal is deleted is told otherwise",
    );
}

/// Two ill-typed literals, read once (withheld), then either one deleted: the other one is
/// still held, so the read stays withheld, naming it; both deleted, the word comes back. The
/// read before the delete is what the delete's commit then judges from, whichever it found.
#[test]
fn deleting_one_of_two_ill_typed_literals_keeps_the_other_withheld() {
    let both = ":a :d \"abc\"^^xsd:integer . :b :d \"yes\"^^xsd:boolean .";
    let mut unmet = Vec::new();
    for ruleset in RULESETS {
        for (gone, kept, named) in [
            (
                ":a :d \"abc\"^^xsd:integer",
                ":b :d \"yes\"^^xsd:boolean",
                "\"yes\"",
            ),
            (
                ":b :d \"yes\"^^xsd:boolean",
                ":a :d \"abc\"^^xsd:integer",
                "\"abc\"",
            ),
        ] {
            let pipeline = open(ruleset, None);
            update(&pipeline, &format!("INSERT DATA {{ {both} }}"));
            let case = format!("{ruleset} {gone} deleted");
            unmet.extend(not_withheld(
                pipeline.store(),
                &format!("{case}: before"),
                None,
            ));
            update(&pipeline, &format!("DELETE DATA {{ {gone} }}"));
            unmet.extend(not_withheld(pipeline.store(), &case, Some(named)));
            update(&pipeline, &format!("DELETE DATA {{ {kept} }}"));
            unmet.extend(not_complete(
                pipeline.store(),
                &format!("{case}, then {kept}"),
            ));
        }
    }
    assert_none(
        &unmet,
        "a store with one of two ill-typed literals left is told otherwise",
    );
}

/// A store written under `owl2-dl`, reopened without its mode (its reads not under `owl2-dl`,
/// out of scope: not told of the literal), then given `owl2-dl` again, as a repository
/// registers its mode after the open: withheld at once.
#[test]
fn a_mode_registered_after_the_open_withholds_at_once() {
    let dir = tempfile::tempdir().expect("dir");
    {
        let before = open("owl2-dl", Some(dir.path()));
        update(&before, "INSERT DATA { :a :d \"abc\"^^xsd:integer }");
    }
    let store = StoreService::new(StoreConfig::on_disk(dir.path())).expect("reopen");
    let (_, reasons) = read(&store);
    assert!(
        !reasons.iter().any(|r| r.contains(REASON)),
        "reopened without its mode, told of the ill-typed literal: {reasons:?}"
    );
    store.use_dl(true, false);
    let unmet = not_withheld(&store, "owl2-dl registered after", Some("\"abc\""));
    assert_none(
        &unmet.into_iter().collect::<Vec<_>>(),
        "the mode registered later",
    );
}

/// Under `owl2-dl`, the DL route's own verdict (what the server's readiness reports as the DL
/// consistency) isn't `consistent` either while the store holds the literal; with it deleted,
/// it is again.
#[test]
fn the_dl_verdict_is_not_consistent_while_an_ill_typed_literal_is_held() {
    let pipeline = open("owl2-dl", None);
    let verdict = || {
        let status = pipeline
            .store()
            .dl()
            .status()
            .expect("a status after a commit");
        status.consistency.verdict.as_str().to_owned()
    };
    update(&pipeline, "INSERT DATA { :a :d \"abc\"^^xsd:integer }");
    assert_ne!(verdict(), "consistent", "held");
    update(&pipeline, "DELETE DATA { :a :d \"abc\"^^xsd:integer }");
    assert_eq!(verdict(), "consistent", "deleted");
}

/// A store in quarantine that holds an ill-typed literal still revalidates on every commit,
/// as without one: imported inconsistent (two disjoint classes) with reasoning off and
/// rebuilt under `owl2-ql`, an unrelated write is accepted, a new violation refused, and a
/// repair ends the quarantine; then the store says `unknown`, as the literal is still held.
#[test]
fn a_quarantined_store_holding_an_ill_typed_literal_revalidates_on_every_commit() {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store"));
    store
        .execute_update(&SparqlUpdateRequest::new(format!(
            "{PREFIXES}INSERT DATA {{ :A owl:disjointWith :B . :x a :A , :B . \
             :a :d \"abc\"^^xsd:integer }}"
        )))
        .expect("import");
    let report = store
        .rematerialise(nrese_reasoner::rulesets::Ruleset::Owl2Ql)
        .expect("rebuild");
    assert_eq!(report.violations, 1);
    let quarantined = ConsistencyStatus::Inconsistent { violations: 1 };
    assert_eq!(store.consistency(), quarantined);
    let pipeline = over(&store, "owl2-ql");
    update(&pipeline, "INSERT DATA { :y a :A }");
    assert!(
        store.is_quarantined(),
        "an unrelated write ended the quarantine"
    );
    assert!(
        commit(&pipeline, "INSERT DATA { :y a :B }").is_err(),
        "a new violation in quarantine is applied"
    );
    update(&pipeline, "DELETE DATA { :x a :B }");
    assert!(
        !store.is_quarantined(),
        "the repair left the store in quarantine"
    );
    assert_eq!(store.consistency(), ConsistencyStatus::Unknown);
}
