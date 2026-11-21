//! A write that changes how an axiom's list is read: what its answers hold, and what they
//! are told, against the OWL 2 RL rule tables; and a write that leaves a list as it is read,
//! against a rebuild of the same facts.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, QlRewritingMode, Requester,
    SolutionsResultFormat, SparqlQueryRequest, SparqlUpdateRequest, StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX ex: <http://example.org/> \
     PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#>";

const MODES: [&str; 3] = ["replication", "representatives", "compact"];

fn config(mode: &str, ql: QlRewritingMode, dir: Option<&Path>) -> StoreConfig {
    let mut config = match dir {
        Some(dir) => StoreConfig::on_disk(dir),
        None => StoreConfig::in_memory(),
    };
    config.equality_by_representatives = mode != "replication";
    config.equality_compact = mode == "compact";
    config.ql_rewriting = ql;
    config
}

fn pipeline(store: &Arc<StoreService>, mode: ReasoningMode) -> MutationPipeline {
    MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn apply(pipeline: &MutationPipeline, body: &str) -> Result<(), String> {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}\n{body}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .map(drop)
        .map_err(|e| format!("{e}"))
}

/// The answer's rows (TSV for a SELECT, the boolean for an ASK), and whether it is told
/// `complete`.
fn read(store: &StoreService, query: &str) -> (String, bool) {
    let mut request = SparqlQueryRequest::all(format!("{PREFIXES} {query}"));
    request.solutions_format = SolutionsResultFormat::Tsv;
    let result = store
        .execute_query(&request)
        .unwrap_or_else(|e| panic!("{query}: {e}"));
    let complete = (result.completeness.as_ref()).is_some_and(|c| c.as_str() == "complete");
    (String::from_utf8(result.payload).expect("utf-8"), complete)
}

const A_TO_C: &str = "<http://example.org/a>\t<http://example.org/c>";

/// A list cell's two forms: a blank node (the OWL 2 mapping's list syntax) and an IRI.
fn cells(blank: bool) -> (&'static str, &'static str) {
    match blank {
        true => ("_:l1", "_:l2"),
        false => ("ex:l1", "ex:l2"),
    }
}

/// A write that leaves an axiom's list no longer read: the rows it gave go. Spec: OWL 2
/// Profiles §4.3, Table 4, `prp-spo2`: `ex:chain owl:propertyChainAxiom (ex:p ex:q)` with
/// `ex:a ex:p ex:b`, `ex:b ex:q ex:c` gives `ex:a ex:chain ex:c`. Revision 2 adds
/// `l2 rdf:rest l1` and `l1 rdf:rest rdf:nil` (the path `(ex:p ex:q)` stays; the list is now
/// cyclic, which the rules don't read). Revision 3 deletes `l2 rdf:rest rdf:nil`: every
/// path from `l1` to `rdf:nil` now ends after `ex:p` (or repeats `ex:p ex:q` and `ex:p`), so
/// no path gives `ex:a ex:chain ex:c`: the row has no proof. After revisions 2 and 3 the
/// store leaves the cyclic list out, so rows its paths give (`ex:a ex:chain ex:b` after
/// revision 3) may be missing, and the answer is not told `complete` (the specification's
/// reasoning semantics: while a list axiom is left out, an answer that reads the inferences
/// isn't `complete`). Under each equality mode, with QL rewriting on and off, with blank and
/// IRI cells.
#[test]
fn a_row_a_list_edit_takes_the_proof_of_is_gone_and_the_answer_says_so() {
    let mut wrong = Vec::new();
    for mode in MODES {
        for ql in [QlRewritingMode::On, QlRewritingMode::Off] {
            for blank in [true, false] {
                let case = format!("{mode} ql={} blank={blank}", ql.name());
                let store = Arc::new(StoreService::new(config(mode, ql, None)).expect("store"));
                let commits = pipeline(&store, ReasoningMode::Owl2Rl);
                let (l1, l2) = cells(blank);
                let found =
                    "ex:chain owl:propertyChainAxiom ?l1 . ?l1 rdf:rest ?l2 . ?l2 rdf:first ex:q";
                let revisions = [
                    format!(
                        "INSERT DATA {{ ex:chain owl:propertyChainAxiom {l1} .
                           {l1} rdf:first ex:p . {l1} rdf:rest {l2} .
                           {l2} rdf:first ex:q . {l2} rdf:rest rdf:nil .
                           ex:a ex:p ex:b . ex:b ex:q ex:c . }}"
                    ),
                    format!(
                        "INSERT {{ ?l2 rdf:rest ?l1 . ?l1 rdf:rest rdf:nil }} WHERE {{ {found} }}"
                    ),
                    format!("DELETE {{ ?l2 rdf:rest rdf:nil }} WHERE {{ {found} }}"),
                ];
                for (i, body) in revisions.iter().enumerate() {
                    apply(&commits, body)
                        .unwrap_or_else(|e| panic!("{case}: revision {} commits: {e}", i + 1));
                    let (rows, complete) = read(&store, "SELECT ?s ?o WHERE { ?s ex:chain ?o }");
                    if i == 2 && rows.contains(A_TO_C) {
                        wrong.push(format!("{case}: ex:a ex:chain ex:c is kept with no proof"));
                    }
                    if i > 0 && complete {
                        wrong.push(format!(
                            "{case}: told complete after revision {} with the list left out",
                            i + 1
                        ));
                    }
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A write that makes an axiom's list read again: the rows the list gives are answered.
/// Spec: OWL 2 Profiles §4.3, Table 4, `prp-spo2` (under `owl2-rl`) and Table 6, `cls-int1`
/// (under `owl2-dl`'s rules route): once the delete of the cycle edge `l2 rdf:rest l1`
/// leaves the list `(first second)`, `ex:chain owl:propertyChainAxiom (ex:q1 ex:q2)` with
/// `ex:a ex:q1 ex:b`, `ex:b ex:q2 ex:c` gives `ex:a ex:chain ex:c`, and `ex:C
/// owl:intersectionOf (ex:A ex:B)` with `ex:x` an A and a B gives `ex:x rdf:type ex:C`. Each
/// answer after the delete holds that row, whatever it is told. With QL rewriting on and
/// off, with blank and IRI cells.
#[test]
fn a_row_a_list_repair_gives_is_answered() {
    let mut wrong = Vec::new();
    for ql in [QlRewritingMode::On, QlRewritingMode::Off] {
        for blank in [true, false] {
            let (l1, l2) = cells(blank);
            let list = |head: &str, first: &str, second: &str| {
                format!(
                    "{head} {l1} . {l1} rdf:first {first} . {l1} rdf:rest {l2} .
                     {l2} rdf:first {second} . {l2} rdf:rest rdf:nil . {l2} rdf:rest {l1} ."
                )
            };
            let delete = |subject: &str, predicate: &str| {
                format!(
                    "DELETE {{ ?l2 rdf:rest ?l1 }} WHERE {{ {subject} {predicate} ?l1 . \
                     ?l1 rdf:rest ?l2 . ?l2 rdf:rest ?l1 }}"
                )
            };
            let chain = (
                ReasoningMode::Owl2Rl,
                format!(
                    "{} ex:a ex:q1 ex:b . ex:b ex:q2 ex:c .",
                    list("ex:chain owl:propertyChainAxiom", "ex:q1", "ex:q2")
                ),
                delete("ex:chain", "owl:propertyChainAxiom"),
                "SELECT ?s ?o WHERE { ?s ex:chain ?o }",
                A_TO_C,
            );
            let intersection = (
                ReasoningMode::Owl2Dl,
                format!(
                    "{} ex:A rdf:type owl:Class . ex:B rdf:type owl:Class . \
                     ex:C rdf:type owl:Class . ex:x rdf:type ex:A , ex:B .",
                    list("ex:C owl:intersectionOf", "ex:A", "ex:B")
                ),
                delete("ex:C", "owl:intersectionOf"),
                "SELECT ?x WHERE { ?x rdf:type ex:C }",
                "<http://example.org/x>",
            );
            for (reasoning, before, delete, query, row) in [chain, intersection] {
                let case = format!("{reasoning:?} ql={} blank={blank}", ql.name());
                let store =
                    Arc::new(StoreService::new(config("replication", ql, None)).expect("store"));
                let commits = pipeline(&store, reasoning);
                apply(&commits, &format!("INSERT DATA {{ {before} }}"))
                    .unwrap_or_else(|e| panic!("{case}: the state before commits: {e}"));
                apply(&commits, &delete).unwrap_or_else(|e| panic!("{case}: the delete: {e}"));
                let (rows, _) = read(&store, query);
                if !rows.contains(row) {
                    wrong.push(format!("{case}: {query} lacks {row} after the repair"));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A write that only inserts, after which an axiom's list is read otherwise, is refused when
/// it clashes through a path of the list it leaves in place. Spec: OWL 2 Profiles §4.3, Table
/// 4, `eq-diff2`: `T(?x, rdf:type, owl:AllDifferent)`, `T(?x, owl:members, ?list)`,
/// `LIST[?list, ?y1, …, ?yn]` and `T(?yi, owl:sameAs, ?yj)` with i ≠ j give `false`. `ex:d`
/// is an `owl:AllDifferent` whose members are `(ex:a ex:b)`; the write adds `l2 rdf:rest l1`
/// (the list is now cyclic, and the path `(ex:a ex:b)` to `rdf:nil` stays) and `ex:a
/// owl:sameAs ex:b`. The data it leaves are inconsistent, so the write is refused and `ex:a
/// owl:sameAs ex:b` isn't stored. Under each equality mode, with blank and IRI cells.
#[test]
fn an_insert_that_relists_and_clashes_through_a_kept_path_is_refused() {
    let mut wrong = Vec::new();
    for mode in MODES {
        for blank in [true, false] {
            let case = format!("{mode} blank={blank}");
            let store = Arc::new(
                StoreService::new(config(mode, QlRewritingMode::On, None)).expect("store"),
            );
            let commits = pipeline(&store, ReasoningMode::Owl2Rl);
            let (l1, l2) = cells(blank);
            let setup = format!(
                "INSERT DATA {{ ex:d rdf:type owl:AllDifferent ; owl:members {l1} .
                   {l1} rdf:first ex:a ; rdf:rest {l2} . {l2} rdf:first ex:b ; rdf:rest rdf:nil . }}"
            );
            apply(&commits, &setup).unwrap_or_else(|e| panic!("{case}: setup: {e}"));
            let change = "INSERT { ?l2 rdf:rest ?l1 . ex:a owl:sameAs ex:b } \
                          WHERE { ex:d owl:members ?l1 . ?l1 rdf:rest ?l2 }";
            if apply(&commits, change).is_ok() {
                wrong.push(format!("{case}: the clashing write is accepted"));
            }
            if read(&store, "ASK { ex:a owl:sameAs ex:b }")
                .0
                .contains("true")
            {
                wrong.push(format!("{case}: ex:a owl:sameAs ex:b is stored"));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A list left out before a write and after it, but for another reason, can still change
/// what the closure holds: a list past the cap gets its rules from a rebuild's rounds, so its
/// inferences are in the closure, and once a write breaks the list they have no proof. Spec:
/// OWL 2 Profiles §4.3, Table 6, `cls-int1`, whose `LIST[...]` is a sequence of `rdf:first`
/// and `rdf:rest` ending in `rdf:nil`: `ex:I owl:intersectionOf (ex:C1 … ex:C7)`, each `ex:Ck owl:sameAs ex:Dk` (128
/// member sequences, past the 64 a list axiom is instantiated for), and `ex:x` in each `ex:Ck`
/// give `ex:x rdf:type ex:I`. A write that deletes `ex:n1 rdf:rest ex:n2` leaves `ex:I` with
/// no list (its first cell has no `rdf:rest`), so `cls-int1` no longer applies and nothing
/// else gives `ex:x rdf:type ex:I`. The write also inserts an unrelated fact, so it isn't one
/// that only deletes. As a later write and after a restart, under each equality mode.
#[test]
fn an_inference_of_a_list_past_the_cap_goes_when_a_write_breaks_the_list() {
    let mut cells = String::new();
    for k in 1..=7 {
        let rest = match k {
            7 => "rdf:nil".to_owned(),
            _ => format!("ex:n{}", k + 1),
        };
        cells.push_str(&format!(
            "ex:n{k} rdf:first ex:C{k} ; rdf:rest {rest} . ex:C{k} owl:sameAs ex:D{k} . ex:x rdf:type ex:C{k} . "
        ));
    }
    let setup = format!("INSERT DATA {{ ex:I owl:intersectionOf ex:n1 . {cells} }}");
    let change = "DELETE DATA { ex:n1 rdf:rest ex:n2 . } ; INSERT DATA { ex:z rdf:type ex:Z . }";
    let ask = "ASK { ex:x rdf:type ex:I }";
    let mut wrong = Vec::new();
    for mode in MODES {
        for restart in [false, true] {
            let case = format!("{mode} restart={restart}");
            let dir = tempfile::tempdir().expect("a directory");
            let open = || {
                Arc::new(
                    StoreService::new(config(mode, QlRewritingMode::On, Some(dir.path())))
                        .expect("store"),
                )
            };
            let mut store = open();
            let mut commits = pipeline(&store, ReasoningMode::Owl2Rl);
            apply(&commits, &setup).unwrap_or_else(|e| panic!("{case}: setup: {e}"));
            // The setup's own answer, so a red run fails at the change, not before it.
            if !read(&store, ask).0.contains("true") {
                wrong.push(format!("{case}: the setup lacks ex:x rdf:type ex:I"));
                continue;
            }
            if restart {
                drop(commits);
                drop(store);
                store = open();
                commits = pipeline(&store, ReasoningMode::Owl2Rl);
            }
            apply(&commits, change).unwrap_or_else(|e| panic!("{case}: the change: {e}"));
            if read(&store, ask).0.contains("true") {
                wrong.push(format!(
                    "{case}: ex:x rdf:type ex:I is kept once the list is broken"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Kind (ii): the expected value is a rebuild of the same asserted facts, the store's own
/// batch reasoner, not an independent source. An axiom's list left out (malformed, cyclic,
/// or a 300-class intersection past the rule format's 256 atoms) that no write touches gives
/// no rules to the delta or to a rebuild; ordinary writes to such a store stay incremental
/// and leave every row and the status a rebuild of the same facts gives. Data inserts and
/// deletes, with a restart between, under each equality mode.
#[test]
fn a_list_left_out_and_untouched_leaves_writes_incremental_and_equal_to_a_rebuild() {
    let background = "ex:p rdfs:subPropertyOf ex:s . ex:K1 rdfs:subClassOf ex:K2 . ";
    let chain = |cells: &str| format!("{background}ex:ch owl:propertyChainAxiom ex:m1 . {cells}");
    let long: String = {
        let mut out = format!("{background}ex:I owl:intersectionOf ex:t1 . ");
        for k in 1..=300 {
            let rest = match k {
                300 => "rdf:nil".to_owned(),
                _ => format!("ex:t{}", k + 1),
            };
            out.push_str(&format!("ex:t{k} rdf:first ex:K{k} ; rdf:rest {rest} . "));
        }
        out
    };
    let cases = [
        (
            "malformed",
            chain("ex:m1 rdf:first ex:p ; rdf:rest ex:m2 . ex:m2 rdf:first ex:q . "),
        ),
        (
            "cyclic",
            chain(
                "ex:m1 rdf:first ex:p ; rdf:rest ex:m2 . ex:m2 rdf:first ex:q ; rdf:rest ex:m1 . ",
            ),
        ),
        ("too long", long),
    ];
    let writes = [
        (
            false,
            "INSERT DATA { ex:a ex:p ex:b . ex:b ex:q ex:c . ex:a rdf:type ex:K1 . }",
        ),
        (
            true,
            "INSERT DATA { ex:c ex:p ex:d . ex:d ex:q ex:e . ex:x rdf:type ex:K2 . }",
        ),
        // Each write inserts something: the policy may rebuild one that only deletes
        // ([`nrese_reasoner`]'s `Plan::Rebuild`), whatever its lists.
        (
            false,
            "DELETE DATA { ex:a ex:p ex:b . } ; INSERT DATA { ex:u rdf:type ex:K3 . }",
        ),
        (
            true,
            "DELETE DATA { ex:d ex:q ex:e . } ; INSERT DATA { ex:y ex:p ex:z . }",
        ),
    ];
    let program = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl)
        .materialised_program()
        .expect("a reasoning mode");
    let state = |store: &StoreService| -> (BTreeSet<String>, String) {
        let (rows, _) = read(store, "SELECT ?s ?p ?o WHERE { ?s ?p ?o }");
        let rows = rows.lines().skip(1).map(str::to_owned).collect();
        (rows, format!("{:?}", store.consistency()))
    };
    let mut wrong = Vec::new();
    for (name, setup) in &cases {
        for mode in MODES {
            let dir = tempfile::tempdir().expect("a directory");
            let open = || {
                Arc::new(
                    StoreService::new(config(mode, QlRewritingMode::On, Some(dir.path())))
                        .expect("store"),
                )
            };
            let mut store = open();
            let mut commits = pipeline(&store, ReasoningMode::Owl2Rl);
            let mut asserted = vec![format!("INSERT DATA {{ {setup} }}")];
            apply(&commits, &asserted[0]).unwrap_or_else(|e| panic!("{name}: setup: {e}"));
            for (step, (restart, body)) in writes.iter().enumerate() {
                let case = format!("{name} {mode} write {}", step + 1);
                if *restart {
                    // The store closes before it is opened again.
                    drop(commits);
                    drop(store);
                    store = open();
                    commits = pipeline(&store, ReasoningMode::Owl2Rl);
                }
                apply(&commits, body).unwrap_or_else(|e| panic!("{case}: {e}"));
                asserted.push((*body).to_owned());
                let plan = commits
                    .last_reasoning_run()
                    .map(|run| format!("{:?}", run.work.plan));
                if plan.as_deref() == Some("Rebuild") {
                    wrong.push(format!(
                        "{case}: rebuilt, where the list's reading didn't change"
                    ));
                }
                let fresh = Arc::new(
                    StoreService::new(config(mode, QlRewritingMode::On, None)).expect("store"),
                );
                let replay = pipeline(&fresh, ReasoningMode::Owl2Rl);
                for b in &asserted {
                    apply(&replay, b).unwrap_or_else(|e| panic!("{case}: the replay: {e}"));
                }
                fresh.rematerialise(&program).expect("the rebuild");
                if state(&store) != state(&fresh) {
                    wrong.push(format!("{case}: the incremental state isn't the rebuild's"));
                }
            }
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
