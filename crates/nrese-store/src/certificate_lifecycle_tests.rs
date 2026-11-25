//! What a read is told while the certificate's life goes on around it: the engine's
//! maintenance, a rematerialisation in progress or failed, a write between a read's snapshot
//! and its status, a crash, and a version no one published. A read is told what was true of
//! the snapshot it holds, and nothing more. Each cell prints `CASE name: right …` or
//! `CASE name: WRONG …`; a cell whose setup didn't hold prints `UNMET`. Every expectation is
//! the rule's, written by hand.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::{StoreService, hooks};
use crate::dl::query::Revision;
use crate::{
    BulkLoadRequest, CancellationToken, GraphTarget, MutationCommand, MutationPipeline,
    MutationTicket, Requester, SparqlQueryRequest, SparqlUpdateRequest, StatementOp,
    StatementsRequest, StoreConfig,
};
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};

#[path = "certificate_record_tests.rs"]
mod certificate_record_tests;

const PREFIXES: &str = "PREFIX : <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// A subclass and an instance of it.
const DATA: &str = ":Dog rdfs:subClassOf :Animal . :rex a :Dog .";

/// A monotone read over [`DATA`]: over a ruleset's closure, `sound-only`.
const READ: &str = "SELECT ?x { ?x a :Animal }";

/// A read under a negation over [`DATA`]: over a ruleset's closure, `unsound`.
const NEGATED: &str = "SELECT ?x { ?x a :Dog FILTER NOT EXISTS { ?x a :Pet } }";

/// The cells' verdicts.
#[derive(Default)]
struct Cells {
    wrong: Vec<String>,
    unmet: Vec<String>,
}

impl Cells {
    /// Right if `got`, a status header, satisfies `right`.
    fn check(&mut self, name: &str, got: &str, right: impl Fn(&str) -> bool, want: &str) {
        if right(got) {
            println!("CASE {name}: right {got}");
        } else {
            println!("CASE {name}: WRONG {got}, want {want}");
            self.wrong.push(name.to_owned());
        }
    }

    /// Right if `got`'s first word is `want`.
    fn class(&mut self, name: &str, got: &str, want: &str) {
        self.check(name, got, |h| class(h) == want, want);
    }

    fn unmet(&mut self, name: &str, why: &str) {
        println!("UNMET {name}: {why}");
        self.unmet.push(name.to_owned());
    }

    fn finish(self) {
        assert!(
            self.wrong.is_empty() && self.unmet.is_empty(),
            "wrong: {:?}; unmet: {:?}",
            self.wrong,
            self.unmet
        );
    }
}

/// A status header's first word.
fn class(header: &str) -> &str {
    header.split(';').next().unwrap_or(header)
}

fn header(status: Option<nrese_sparql::Completeness>) -> String {
    status.map_or_else(|| "none".to_owned(), |s| s.header())
}

/// The status header of `query` (over [`PREFIXES`]).
fn status(store: &StoreService, query: &str) -> String {
    let request = SparqlQueryRequest::all(format!("{PREFIXES}{query}"));
    header(
        store
            .execute_query(&request)
            .expect("answered")
            .completeness,
    )
}

/// Loads `data` (Turtle over [`PREFIXES`]'s names), adding to the store's data.
fn load(store: &StoreService, data: &str) {
    let dir = tempfile::tempdir().expect("dir");
    let file = dir.path().join("data.ttl");
    let prefixes = PREFIXES.replace("PREFIX", "@prefix").replace("> ", "> . ");
    std::fs::write(&file, format!("{prefixes}\n{data}")).expect("file");
    store
        .bulk_load(&BulkLoadRequest {
            files: vec![file],
            replace: false,
            graph: GraphTarget::DefaultGraph,
            invalid: Some(crate::InvalidStatements::Strict),
        })
        .expect("load");
}

/// A store in `dir` holding [`DATA`], reasoned under OWL 2 RL.
fn certified(dir: &Path) -> StoreService {
    let store = StoreService::new(StoreConfig::on_disk(dir)).expect("store");
    load(&store, DATA);
    store.rematerialise(Ruleset::Owl2Rl).expect("reasoned");
    store
}

/// The pipeline committing to `store` under `mode`.
fn pipeline(store: &Arc<StoreService>, mode: ReasoningMode) -> MutationPipeline {
    MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

/// Commits `INSERT DATA { data }` through `pipeline`.
fn commit(pipeline: &MutationPipeline, data: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "{PREFIXES}INSERT DATA {{ {data} }}"
            ))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .expect("committed");
}

/// The status headers of [`READ`] and [`NEGATED`].
fn reads(store: &StoreService) -> [String; 2] {
    [status(store, READ), status(store, NEGATED)]
}

/// Whether `header` is OWL 2 RL's closure status for [`READ`].
fn owl2_rl(header: &str) -> bool {
    header.starts_with("sound-only; regime=owl2-rl")
}

/// Copies the directory `from` to `to`, as a crash at this point would leave it on disk.
fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Maintenance that keeps the statements keeps what certified them: a merge of runs
/// (compaction, inline in a commit) and a checkpoint served mapped. A store certified under
/// OWL 2 RL stays so through each, its last publication being that maintenance.
#[test]
fn maintenance_that_keeps_the_statements_keeps_their_certificate() {
    let mut cells = Cells::default();
    // A compaction: commits until one merges runs as it publishes.
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store"));
    let rl = pipeline(&store, ReasoningMode::Owl2Rl);
    commit(&rl, DATA);
    if !owl2_rl(&status(&store, READ)) {
        cells.unmet("a compaction", "the commit isn't certified under owl2-rl");
    }
    let merged = (0..64).any(|i| {
        let before = store.engine().stats().compactions;
        commit(&rl, &format!(":d{i} a :Dog ."));
        store.engine().stats().compactions > before
    });
    match merged {
        true => cells.check(
            "a compaction",
            &status(&store, READ),
            owl2_rl,
            "sound-only; regime=owl2-rl",
        ),
        false => cells.unmet("a compaction", "no commit merged runs"),
    }
    // A checkpoint served mapped, after a commit that kept the closure.
    let dir = tempfile::tempdir().expect("dir");
    let store = Arc::new(StoreService::new(StoreConfig::on_disk(dir.path())).expect("store"));
    let rl = pipeline(&store, ReasoningMode::Owl2Rl);
    commit(&rl, DATA);
    commit(&rl, ":tom a :Dog .");
    if !owl2_rl(&status(&store, READ)) {
        cells.unmet(
            "a checkpoint served mapped",
            "the commits aren't certified under owl2-rl",
        );
    }
    let before = store.engine().stats().checkpoints;
    store.engine().checkpoint().expect("checkpoint");
    let after = store.engine().stats();
    match after.checkpoints > before && after.index_mapped_bytes > 0 {
        true => cells.check(
            "a checkpoint served mapped",
            &status(&store, READ),
            owl2_rl,
            "sound-only; regime=owl2-rl",
        ),
        false => cells.unmet("a checkpoint served mapped", "no checkpoint was mapped"),
    }
    cells.finish();
}

/// A read during a rematerialisation reads the old version, whose statements its program
/// certified: it is told what it was told before the rebuild began, the regime naming the
/// old program (i). The same after a rebuild that fails, until a reopen (ii). A version that
/// a write without reasoning published before the rebuild (an addition, a load) carries no
/// certificate: `not-certified` while the rebuild runs and after it fails (iii). The rebuild
/// is from OWL 2 RL to RDFS; it fails where its checkpoint syncs the WAL's directory.
#[test]
fn a_read_during_a_rebuild_is_told_what_was_true_of_its_version() {
    let mut cells = Cells::default();
    let during = |store: &StoreService| {
        let seen = Arc::new(Mutex::new(None));
        let (reader, kept) = (store.clone(), Arc::clone(&seen));
        hooks::BEFORE_PUBLICATION.set(Some(Box::new(move || {
            *kept.lock().unwrap() = Some(reads(&reader));
        })));
        seen
    };
    let failing = |dir: &Path, store: &StoreService| {
        nrese_engine::fault_points::fail_dir_sync_of(Some((
            &dir.join("wal"),
            std::io::ErrorKind::Other,
        )));
        let rebuilt = store.rematerialise(Ruleset::Rdfs);
        nrese_engine::fault_points::fail_dir_sync_of(None);
        rebuilt.is_err()
    };
    // (i) and (ii): a certified version.
    for failed in [false, true] {
        let name = match failed {
            false => "5/8-i during the rebuild",
            true => "5/8-ii after a failed rebuild",
        };
        let dir = tempfile::tempdir().expect("dir");
        let store = Arc::new(certified(dir.path()));
        // A commit that keeps the closure: its record makes the rebuild start a new log
        // segment, whose directory sync is what fails.
        commit(&pipeline(&store, ReasoningMode::Owl2Rl), ":tom a :Dog .");
        let before = reads(&store);
        if !owl2_rl(&before[0]) || !before[1].starts_with("unsound; regime=owl2-rl") {
            cells.unmet(name, &format!("not certified under owl2-rl: {before:?}"));
            continue;
        }
        let got = match failed {
            false => {
                let seen = during(&store);
                store.rematerialise(Ruleset::Rdfs).expect("rebuilt");
                seen.lock().unwrap().take()
            }
            true => failing(dir.path(), &store).then(|| reads(&store)),
        };
        match got {
            Some(got) => {
                for (i, read) in ["a read", "a negated read"].into_iter().enumerate() {
                    let want = before[i].clone();
                    cells.check(
                        &format!("{name} | {read}"),
                        &got[i],
                        |h| h == want,
                        &before[i],
                    );
                }
            }
            None => cells.unmet(
                name,
                "the rebuild didn't reach its publication or didn't fail",
            ),
        }
    }
    // (iii): a version published by a write without reasoning: a commit of the pipeline
    // with reasoning disabled.
    let add = |store: &Arc<StoreService>, data: &str| {
        commit(&pipeline(store, ReasoningMode::Disabled), data);
    };
    for kind in ["an addition", "a load"] {
        let dir = tempfile::tempdir().expect("dir");
        let store = Arc::new(certified(dir.path()));
        if !owl2_rl(&status(&store, READ)) {
            cells.unmet(kind, "not certified under owl2-rl");
            continue;
        }
        match kind {
            "an addition" => add(&store, ":tom a :Dog"),
            // A load leaves the log's segment empty: an addition after it gives the rebuild
            // its new segment (a reasoning commit would rematerialise first). Both are writes
            // without reasoning: the version is no more covered than the load's.
            _ => {
                load(&store, ":tom a :Dog .");
                add(&store, ":sam a :Dog");
            }
        }
        let seen = during(&store);
        if !failing(dir.path(), &store) {
            cells.unmet(kind, "the rebuild didn't fail");
            continue;
        }
        match seen.lock().unwrap().take() {
            Some(got) => {
                cells.class(
                    &format!("5/8-iii {kind} | during the rebuild"),
                    &got[0],
                    "not-certified",
                );
            }
            None => cells.unmet(kind, "the rebuild didn't reach its publication"),
        }
        cells.class(
            &format!("5/8-iii {kind} | after the failed rebuild"),
            &status(&store, READ),
            "not-certified",
        );
    }
    cells.finish();
}

/// A read's status is the snapshot's it read: a write between the snapshot a read takes and
/// its status changes neither its answers nor its status.
/// - EXPLAIN under `owl2-dl`, off the RL route, of data with an annotation-property axiom:
///   OWL 2 DL may not entail what the RL closure holds there, so the read is `unsound`. A
///   commit removing that axiom comes after EXPLAIN's snapshot; EXPLAIN evaluated the old
///   one and is told `unsound` (N5, the decided snapshot).
/// - A read routed over the OWL 2 RL closure keeps that route when the `owl2-dl` mode is
///   switched on after its snapshot: `sound-only; regime=owl2-rl` (N5, the route's flags).
#[test]
fn a_read_is_told_the_status_of_the_snapshot_it_holds() {
    let mut cells = Cells::default();
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store"));
    let dl = Arc::new(pipeline(&store, ReasoningMode::Owl2Dl));
    commit(
        &dl,
        ":note a owl:AnnotationProperty . :note rdfs:subPropertyOf :other . \
         :Dog rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :q ; owl:someValuesFrom :Dog ] . \
         :Dog rdfs:subClassOf :Animal . :rex a :Dog .",
    );
    let before = status(&store, READ);
    if class(&before) != "unsound" {
        cells.unmet(
            "N5 EXPLAIN",
            &format!("the annotation axiom doesn't make it unsound: {before}"),
        );
    } else {
        let committer = Arc::clone(&dl);
        hooks::AFTER_DECIDED.set(Some(Box::new(move || {
            committer
                .apply(
                    MutationCommand::Update(SparqlUpdateRequest::new(format!(
                        "{PREFIXES}DELETE DATA {{ :note rdfs:subPropertyOf :other }}"
                    ))),
                    &Requester::all(),
                    &MutationTicket::new(),
                )
                .expect("committed");
        })));
        let prepared = store
            .prepare_query(&SparqlQueryRequest::all(format!("{PREFIXES}{READ}")))
            .expect("prepared");
        let explained = store
            .explain_query(&prepared, &CancellationToken::new())
            .expect("explained");
        let after = status(&store, READ);
        if class(&after) == "unsound" {
            cells.unmet(
                "N5 EXPLAIN",
                &format!("the commit left the read unsound: {after}"),
            );
        } else {
            cells.class(
                "N5 EXPLAIN, a commit after its snapshot",
                &header(explained.completeness),
                "unsound",
            );
        }
    }
    // The route: OWL 2 RL's closure, `owl2-dl` switched on after the snapshot.
    let dir = tempfile::tempdir().expect("dir");
    let store = certified(dir.path());
    let before = status(&store, READ);
    if !owl2_rl(&before) {
        cells.unmet(
            "N5 the route",
            &format!("not certified under owl2-rl: {before}"),
        );
    } else {
        let switcher = store.clone();
        hooks::AFTER_DECIDED.set(Some(Box::new(move || switcher.use_dl(true, false))));
        cells.check(
            "N5 the route, owl2-dl switched on after the snapshot",
            &status(&store, READ),
            |h| h == before,
            &before,
        );
    }
    cells.finish();
}

/// A crash during a rematerialisation from OWL 2 RL to RDFS opens to a store that claims no
/// more than is true of what is on disk:
/// - before the publication (the old certificate removed, the old statements): `not-certified`;
///   a certificate written before the statements it describes would claim RDFS here;
/// - after the publication, before the certificate is written: `not-certified`;
/// - after both: certified under RDFS.
///
/// Each crash is the data directory copied at that point and opened.
#[test]
fn a_crash_during_a_rebuild_opens_to_no_more_than_is_true() {
    let mut cells = Cells::default();
    let dir = tempfile::tempdir().expect("dir");
    let data = dir.path().join("store");
    let store = certified(&data);
    if !owl2_rl(&status(&store, READ)) {
        cells.unmet("a crash", "not certified under owl2-rl");
        cells.finish();
        return;
    }
    let copied = Arc::new(Mutex::new(Vec::new()));
    for (hook, name) in [
        (&hooks::BEFORE_PUBLICATION, "before the publication"),
        (
            &hooks::AFTER_PUBLICATION,
            "between the publication and the certificate",
        ),
    ] {
        let (from, to, done) = (data.clone(), dir.path().join(name), Arc::clone(&copied));
        hook.set(Some(Box::new(move || {
            let copy = copy_dir(&from, &to).map(|()| to);
            done.lock().unwrap().push((name, copy));
        })));
    }
    store.rematerialise(Ruleset::Rdfs).expect("rebuilt");
    let after = dir.path().join("after both");
    let copy = copy_dir(&data, &after).map(|()| after);
    copied.lock().unwrap().push(("after both", copy));
    for (name, copy) in copied.lock().unwrap().drain(..) {
        let copy = match copy {
            Ok(copy) => copy,
            Err(error) => {
                cells.unmet(name, &format!("not copied: {error}"));
                continue;
            }
        };
        let opened = StoreService::new(StoreConfig::on_disk(&copy)).expect("opened");
        let got = status(&opened, READ);
        match name {
            "after both" => cells.check(
                &format!("a crash {name}"),
                &got,
                |h| h.starts_with("sound-only; regime=rdfs"),
                "sound-only; regime=rdfs",
            ),
            _ => cells.class(&format!("a crash {name}"), &got, "not-certified"),
        }
    }
    cells.finish();
}

/// A version no one published (a private transaction's, a writer's pending one) is read
/// through a status route that is passed no pending operations: `not-certified`, whatever
/// its base's certificate says. Fail-closed by construction: the composition knows such a
/// snapshot is unpublished from the snapshot itself (`Snapshot::shared_state()` is `None`
/// for a pending, private or added-to snapshot), not from what its caller passes. The
/// route here is today's status outside the DL bounds, told the snapshot is committed;
/// the composition takes its place with no pending kind.
#[test]
fn a_version_no_one_published_read_without_its_operations_is_not_certified() {
    let mut cells = Cells::default();
    let dir = tempfile::tempdir().expect("dir");
    let store = certified(dir.path());
    store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
    if !owl2_rl(&status(&store, READ)) {
        cells.unmet("row 14", "not certified under owl2-rl");
        cells.finish();
        return;
    }
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!("{PREFIXES}{READ}")))
        .expect("prepared");
    let memory = store.settings.query_envelope(&prepared);
    let cancel = CancellationToken::new();
    let addition = format!("{PREFIXES}INSERT DATA {{ :tom a :Dog }}");
    let private = store.with_pending(
        &StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(
            addition.clone(),
        ))]),
        &crate::ReadScope::of(None),
        &memory,
        &cancel,
        |snapshot| Ok(store.status_outside_dl(&prepared, None, Revision::Committed(snapshot))),
    );
    match private {
        Ok(got) => cells.class("row 14 a private version", &header(got), "not-certified"),
        Err(error) => cells.unmet("row 14 a private version", &error.to_string()),
    }
    // The same version read through a restriction of it (a reader of the default graph and
    // the inferences it supports): a restriction keeps the snapshot unpublished.
    let restricted = crate::ReadScope::of(Some(Arc::new(nrese_sparql::GraphAccess {
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..nrese_sparql::GraphAccess::default()
    })));
    let private = store.with_pending(
        &StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(
            addition.clone(),
        ))]),
        &restricted,
        &memory,
        &cancel,
        |snapshot| Ok(store.status_outside_dl(&prepared, None, Revision::Committed(snapshot))),
    );
    match private {
        Ok(got) => cells.class(
            "row 14 a private version, restricted",
            &header(got),
            "not-certified",
        ),
        Err(error) => cells.unmet("row 14 a private version, restricted", &error.to_string()),
    }
    let mut tx = store.engine().transaction();
    let context = store.update_context(memory.clone(), &cancel);
    match crate::mutation::command::apply_sparql_update(
        &mut tx,
        &SparqlUpdateRequest::new(addition),
        &Requester::all(),
        &context,
    ) {
        Ok(()) => {
            let snapshot = tx.pending_snapshot();
            let got = store.status_outside_dl(&prepared, None, Revision::Committed(&snapshot));
            cells.class("row 14 a pending version", &header(got), "not-certified");
        }
        Err(error) => cells.unmet("row 14 a pending version", &error.to_string()),
    }
    drop(tx);
    cells.finish();
}

/// How long a test waits for another thread at a seam.
const WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// `:rex a :Animal`, which [`DATA`] entails under OWL 2 RL.
fn rex_an_animal() -> [nrese_rdf::Triple; 1] {
    [nrese_rdf::Triple::new(
        nrese_rdf::NamedNode::new_unchecked("http://example.com/rex"),
        nrese_rdf::vocab::rdf::TYPE.into_owned(),
        nrese_rdf::NamedNode::new_unchecked("http://example.com/Animal"),
    )]
}

/// What an entailment check said: its answer, or `refused: …`.
fn told(answer: crate::StoreResult<crate::Entailment>) -> String {
    match answer {
        Ok(answer) => format!("{answer:?}"),
        Err(error) => format!("refused: {error}"),
    }
}

/// An entailment check reads one snapshot: its currentness, its consistency and its read of
/// the closure are that snapshot's (R1). A clear of the inferred stack has invalidated the
/// certificate and is paused before it publishes the empty stack when the check begins; it
/// publishes between the check's test of the certificate and its read of the closure. The
/// data entails `:rex a :Animal` under OWL 2 RL before and after: the call refuses, or
/// answers from the one snapshot it read (`Entailed`); never `NotEntailed`.
#[test]
fn an_entailment_check_reads_one_snapshot_while_a_clear_publishes() {
    use std::sync::mpsc::channel;
    let mut cells = Cells::default();
    let dir = tempfile::tempdir().expect("dir");
    let store = certified(dir.path());
    let before = told(store.entails(Ruleset::Owl2Rl, &rex_an_animal()));
    if before != "Entailed" {
        cells.unmet("R1", &format!("not entailed before the clear: {before}"));
        cells.finish();
        return;
    }
    let (paused, at_seam) = channel();
    let (go, gone) = channel::<()>();
    let (done, published) = channel();
    let clearer = store.clone();
    let clear = std::thread::spawn(move || {
        hooks::BEFORE_CLEAR_PUBLICATION.set(Some(Box::new(move || {
            let _ = paused.send(());
            let _ = gone.recv_timeout(WAIT);
        })));
        let cleared = clearer.clear_inferred();
        let _ = done.send(());
        cleared
    });
    if at_seam.recv_timeout(WAIT).is_err() {
        cells.unmet("R1", "the clear didn't reach its publication");
    } else {
        let release = go.clone();
        hooks::AFTER_CERTIFIED.set(Some(Box::new(move || {
            let _ = release.send(());
            let _ = published.recv_timeout(WAIT);
        })));
        let got = told(store.entails(Ruleset::Owl2Rl, &rex_an_animal()));
        hooks::AFTER_CERTIFIED.set(None);
        cells.check(
            "R1 an entailment check across a clear's publication",
            &got,
            |h| h.starts_with("refused") || h == "Entailed",
            "a refusal, or Entailed",
        );
    }
    let _ = go.send(());
    if let Err(error) = clear.join().expect("the clear's thread") {
        cells.unmet("R1", &format!("the clear failed: {error}"));
    }
    cells.finish();
}

thread_local! {
    /// Whether this thread's stores can't read their memory, and whether one tried meanwhile.
    static UNREAD: std::cell::Cell<(bool, bool)> = const { std::cell::Cell::new((false, false)) };
}

/// The memory reading of a store given it by [`StoreService::with_capacity`]: unreadable
/// while [`UNREAD`] says so.
fn capacity() -> nrese_exec::memory::Reading {
    match UNREAD.get() {
        (true, _) => {
            UNREAD.set((true, true));
            Err(nrese_exec::memory::Unread::Failed)
        }
        _ => nrese_exec::memory::capacity(),
    }
}

/// A repair commit on a store in quarantine: its data holds `:a a owl:Nothing`, inconsistent
/// under OWL 2 RL, and a commit through OWL 2 RL's pipeline removes it. The commit inherits
/// the closure's facts, not the old version's assessment of its consistency: until a
/// revalidation writes one, the new version's consistency isn't established. `consistency()`
/// doesn't say `Inconsistent`, and `entails` refuses rather than answer `InconsistentPremise`:
/// - after the commit's publication, before its revalidation;
/// - after a revalidation stopped before its publication (the store's memory can't be read:
///   it stops, failing closed). A directory-sync fault doesn't reach this revalidation: the
///   commit kept the closure, so the stack it computes is the one stored, and nothing is written;
/// - opened from a copy of the directory taken before the revalidation: the marker's old
///   inconsistency isn't claimed of the repaired data.
///
/// In the first two the store is in quarantine (`is_quarantined`, which readiness reads): a
/// consistency not established is no reason to call it ready.
#[test]
fn a_repair_commit_in_quarantine_establishes_no_consistency_it_did_not_check() {
    let mut cells = Cells::default();
    let dir = tempfile::tempdir().expect("dir");
    let data = dir.path().join("store");
    let store = StoreService::new(StoreConfig::on_disk(&data))
        .expect("store")
        .with_capacity(capacity);
    load(&store, &format!("{DATA} :a a owl:Nothing ."));
    store.rematerialise(Ruleset::Owl2Rl).expect("reasoned");
    let store = Arc::new(store);
    let quarantined = store.consistency();
    if !matches!(quarantined, crate::ConsistencyStatus::Inconsistent { .. }) {
        cells.unmet(
            "a repair commit",
            &format!("not in quarantine: {quarantined:?}"),
        );
        cells.finish();
        return;
    }
    let rl = pipeline(&store, ReasoningMode::Owl2Rl);
    let seen = Arc::new(Mutex::new(None));
    let copy = dir.path().join("before the revalidation");
    let (reader, kept, from, to) = (
        Arc::clone(&store),
        Arc::clone(&seen),
        data.clone(),
        copy.clone(),
    );
    hooks::BEFORE_REVALIDATION.set(Some(Box::new(move || {
        let consistency = format!("{:?}", reader.consistency());
        let entailed = told(reader.entails(Ruleset::Owl2Rl, &rex_an_animal()));
        let copied = copy_dir(&from, &to);
        let committed = reader.current_revision();
        let quarantined = reader.is_quarantined();
        UNREAD.set((true, false));
        *kept.lock().unwrap() = Some((consistency, entailed, copied, committed, quarantined));
    })));
    let repaired = rl.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(format!(
            "{PREFIXES}DELETE DATA {{ :a a owl:Nothing }}"
        ))),
        &Requester::all(),
        &MutationTicket::new(),
    );
    hooks::BEFORE_REVALIDATION.set(None);
    // What the store says after the revalidation, read first, with its memory readable.
    let (_, tried) = UNREAD.get();
    UNREAD.set((false, false));
    let after = (
        format!("{:?}", store.consistency()),
        told(store.entails(Ruleset::Owl2Rl, &rex_an_animal())),
        store.is_quarantined(),
    );
    // The stop was the switch's: the revalidation read the memory while it couldn't, the same
    // call under the switch stops as a memory stop, and nothing was published meanwhile.
    UNREAD.set((true, false));
    let again = store.rematerialise(Ruleset::Owl2Rl);
    UNREAD.set((false, false));
    let not_inconsistent = |h: &str| !h.starts_with("Inconsistent");
    let refused = |h: &str| h.starts_with("refused");
    let readiness = |quarantined: bool| match quarantined {
        true => "quarantined",
        false => "ready",
    };
    let seen = seen.lock().unwrap().take();
    match (repaired, seen) {
        (Ok(_), Some((consistency, entailed, copied, committed, quarantined))) => {
            cells.check(
                "a repair commit before its revalidation | consistency()",
                &consistency,
                not_inconsistent,
                "not Inconsistent",
            );
            cells.check(
                "a repair commit before its revalidation | entails",
                &entailed,
                refused,
                "a refusal",
            );
            cells.check(
                "a repair commit before its revalidation | readiness",
                readiness(quarantined),
                |h| h == "quarantined",
                "quarantined",
            );
            let stopped = tried
                && matches!(again, Err(crate::StoreError::MemoryUnreadable { .. }))
                && store.current_revision() == committed;
            if !stopped {
                cells.unmet(
                    "a repair commit after a stopped revalidation",
                    &format!(
                        "not the switch's stop: read {tried}, again {:?}, at {} after {committed}",
                        again.map(|_| ()),
                        store.current_revision()
                    ),
                );
            } else {
                cells.check(
                    "a repair commit after a stopped revalidation | consistency()",
                    &after.0,
                    not_inconsistent,
                    "not Inconsistent",
                );
                cells.check(
                    "a repair commit after a stopped revalidation | entails",
                    &after.1,
                    refused,
                    "a refusal",
                );
                cells.check(
                    "a repair commit after a stopped revalidation | readiness",
                    readiness(after.2),
                    |h| h == "quarantined",
                    "quarantined",
                );
            }
            match copied {
                Ok(()) => {
                    let opened = StoreService::new(StoreConfig::on_disk(&copy)).expect("opened");
                    cells.check(
                        "a repair commit reopened from before its revalidation | consistency()",
                        &format!("{:?}", opened.consistency()),
                        not_inconsistent,
                        "not Inconsistent",
                    );
                }
                Err(error) => {
                    cells.unmet("a repair commit reopened", &format!("not copied: {error}"))
                }
            }
        }
        (Ok(_), None) => cells.unmet(
            "a repair commit",
            "the commit didn't reach its revalidation",
        ),
        (Err(error), _) => cells.unmet("a repair commit", &format!("the commit failed: {error}")),
    }
    cells.finish();
}

/// A change of a repository's reasoning (`Catalog::change`) rebuilds its inferences under the
/// new rules before its new write path registers them. A reader of every graph between the
/// rebuild's publication and that registration is told `not-certified`: the closure published
/// isn't that of the rules registered then. After the change it is told the new rules' status,
/// byte-equal to a store rebuilt under them with them registered.
#[test]
fn a_change_of_reasoning_is_not_certified_until_its_rules_are_registered() {
    use crate::catalog::{Catalog, DEFAULT_REPOSITORY, RepositorySettings};
    let mut cells = Cells::default();
    let (dir, other) = (
        tempfile::tempdir().expect("dir"),
        tempfile::tempdir().expect("dir"),
    );
    let store = certified(dir.path());
    let reader = store.clone();
    let served_under = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Owl2Rl));
    let catalog = Catalog::open(store, served_under).expect("catalog");
    let before = reads(&reader);
    if !owl2_rl(&before[0]) {
        cells.unmet(
            "the window",
            &format!("not certified under owl2-rl: {before:?}"),
        );
        cells.finish();
        return;
    }
    let seen = Arc::new(Mutex::new(None));
    let (inside, kept) = (reader.clone(), Arc::clone(&seen));
    hooks::AFTER_PUBLICATION.set(Some(Box::new(move || {
        *kept.lock().unwrap() = Some(reads(&inside));
    })));
    let rdfs = RepositorySettings {
        reasoning: Some("rdfs".to_owned()),
        ..RepositorySettings::default()
    };
    catalog.change(DEFAULT_REPOSITORY, rdfs).expect("changed");
    hooks::AFTER_PUBLICATION.set(None);
    let rebuilt = {
        let store = Arc::new(StoreService::new(StoreConfig::on_disk(other.path())).expect("store"));
        let _rdfs = pipeline(&store, ReasoningMode::Rdfs);
        load(&store, DATA);
        store.rematerialise(Ruleset::Rdfs).expect("reasoned");
        reads(&store)
    };
    let after = reads(&reader);
    let seen = seen.lock().unwrap().take();
    for (i, read) in ["a read", "a negated read"].into_iter().enumerate() {
        match &seen {
            Some(got) => cells.class(&format!("the window | {read}"), &got[i], "not-certified"),
            None => cells.unmet("the window", "the rebuild didn't reach its publication"),
        }
        let want = rebuilt[i].clone();
        cells.check(
            &format!("after the change | {read}"),
            &after[i],
            |h| h == want,
            &rebuilt[i],
        );
    }
    cells.finish();
}

/// A clash the rules find (cax-dw), on the rules' route under `owl2-dl`, and a class `:H`
/// nothing is asserted in.
const CLASH: &str = ":A a owl:Class . :B a owl:Class . :H a owl:Class . \
     :A owl:disjointWith :B . :w a :A , :B .";

/// A read [`CLASH`] decides: none over consistent data, every individual over inconsistent.
const IN_H: &str = "SELECT ?x { ?x a :H }";

/// Under `owl2-dl` a store leaves quarantine at the revision its repair published: the
/// revalidation after the repair changes no rows, so it keeps the revision, and the bounds
/// keep a view per revision. A read between the publication and the revalidation (the store
/// still in quarantine) builds and keeps that revision's view and is told the store is in
/// quarantine; the read after the revalidation is told what the same data, loaded clean, are
/// told.
#[test]
fn a_store_leaving_quarantine_is_not_complete_until_revalidated_then_as_clean_data() {
    let rules = || {
        ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
            .materialised_program()
            .expect("owl2-dl maintains the OWL 2 RL closure")
    };
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store"));
    let dl = pipeline(&store, ReasoningMode::Owl2Dl);
    load(&store, CLASH);
    store.rematerialise(rules()).expect("rebuilt");
    assert!(
        store.is_quarantined(),
        "the setup: {:?}",
        store.consistency()
    );
    let (reader, kept) = (Arc::clone(&store), Arc::new(Mutex::new(None)));
    let seen = Arc::clone(&kept);
    hooks::BEFORE_REVALIDATION.set(Some(Box::new(move || {
        let read = status(&reader, IN_H);
        *seen.lock().unwrap() = Some((reader.current_revision(), reader.is_quarantined(), read));
    })));
    let repaired = dl.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(format!(
            "{PREFIXES}DELETE DATA {{ :w a :B }}"
        ))),
        &Requester::all(),
        &MutationTicket::new(),
    );
    hooks::BEFORE_REVALIDATION.set(None);
    repaired.expect("the repair commits");
    let after = status(&store, IN_H);
    let clean = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store"));
    let _clean_dl = pipeline(&clean, ReasoningMode::Owl2Dl);
    load(&clean, &CLASH.replace(":w a :A , :B", ":w a :A"));
    clean.rematerialise(rules()).expect("rebuilt");
    let clean = status(&clean, IN_H);
    let (revision, quarantined, during) = kept.lock().unwrap().take().expect("the hook ran");
    let told = "sound-only; regime=owl2-dl; reasons=\"dl: the store is in quarantine (the \
                consistency rules found violations and have not found the data clean since), and \
                as inconsistent data entail every answer, no read is certified complete until it \
                leaves\"";
    assert_eq!(during, told, "the window");
    assert_eq!(after, clean);
    assert!(
        quarantined,
        "the setup: the read in between saw no quarantine"
    );
    assert_eq!(
        store.current_revision(),
        revision,
        "the setup: the revalidation took a new revision"
    );
}

/// What a read over data the full check finds inconsistent is told (`dl::query`).
const INCONSISTENT: &str = "the data is inconsistent under OWL 2 DL, so it entails every answer";

/// Under `owl2-dl` a rebuild that finds a clash publishes its revision before it records
/// the clash (`StoreService::rematerialise`). A read between the two builds and keeps that
/// revision's view; the read after the record is told what a store in quarantine is told:
/// `sound-only`, the data inconsistent.
#[test]
fn a_store_entering_quarantine_at_a_rebuild_is_told_inconsistent_after_a_read_in_between() {
    let rules = || {
        ReasonerConfig::for_mode(ReasoningMode::Owl2Dl)
            .materialised_program()
            .expect("owl2-dl maintains the OWL 2 RL closure")
    };
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store"));
    let _dl = pipeline(&store, ReasoningMode::Owl2Dl);
    load(&store, &CLASH.replace(":w a :A , :B", ":w a :A"));
    store.rematerialise(rules()).expect("rebuilt");
    assert!(
        !store.is_quarantined(),
        "the setup: {:?}",
        store.consistency()
    );
    load(&store, ":w a :B .");
    let (reader, seen) = (Arc::clone(&store), Arc::new(Mutex::new(None)));
    let kept = Arc::clone(&seen);
    hooks::AFTER_PUBLICATION.set(Some(Box::new(move || {
        *kept.lock().unwrap() = Some((reader.current_revision(), status(&reader, IN_H)));
    })));
    let rebuilt = store.rematerialise(rules());
    hooks::AFTER_PUBLICATION.set(None);
    rebuilt.expect("rebuilt");
    let after = status(&store, IN_H);
    assert!(
        class(&after) == "sound-only" && after.contains(INCONSISTENT),
        "{after}"
    );
    let (revision, _) = seen.lock().unwrap().take().expect("the hook ran");
    assert!(
        store.is_quarantined(),
        "the setup: {:?}",
        store.consistency()
    );
    assert_eq!(
        store.current_revision(),
        revision,
        "the setup: the read in between read another revision"
    );
}

/// A pending fact through an alias of `owl:hasKey` reaches the list scan of a reader by
/// support's own pending view, under compact equality: the data of
/// `tests/it/mutation_pipeline_tests.rs`'s
/// `a_pending_fact_through_an_alias_of_has_key_keeps_the_transaction_status`,
/// whose reader is told `not-certified`, so its status no longer shows the scan. `:hk`, its
/// class's representative with `owl:hasKey`, is written first with reasoning off; the pending
/// `:K :hk :head` makes a readable key of 128 properties. The scanned view is the
/// reader's own and holds none of the store's inferences (`:x a :L` among them).
#[test]
fn a_readers_pending_view_scans_a_key_through_an_alias_of_has_key() {
    let config = StoreConfig {
        ql_rewriting: crate::QlRewritingMode::On,
        equality_compact: true,
        ..StoreConfig::in_memory()
    };
    let store = Arc::new(StoreService::new(config).expect("store"));
    let (ex, owl) = ("http://example.com/", "http://www.w3.org/2002/07/owl#");
    let rdf = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
    let cell = |i: usize| match i {
        1 => format!("<{ex}head>"),
        129 => format!("<{rdf}nil>"),
        i => format!("<{ex}c{i}>"),
    };
    let mut data = format!("<{ex}hk> <{owl}sameAs> <{owl}hasKey> .");
    for i in 1..=128 {
        let (at, next) = (cell(i), cell(i + 1));
        data += &format!(" {at} <{rdf}first> <{ex}p{i}> . {at} <{rdf}rest> {next} .");
        data += &format!(" <{ex}p{i}> a <{owl}ObjectProperty> .");
    }
    // `:K` interned before the pending fact; `:x a :L`, an inference the reader doesn't see.
    data += &format!(" <{ex}x> a <{ex}K> . <{ex}K> rdfs:subClassOf <{ex}L> .");
    commit(&pipeline(&store, ReasoningMode::Disabled), &data);
    store.rematerialise(Ruleset::Owl2Rl).expect("the closure");
    let _rules = pipeline(&store, ReasoningMode::Owl2Rl);
    let reader = crate::ReadScope::of(Some(Arc::new(nrese_sparql::GraphAccess {
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..nrese_sparql::GraphAccess::default()
    })));
    let prepared =
        (store.prepare_query(&SparqlQueryRequest::all("ASK {}".to_owned()))).expect("prepared");
    let payload = format!("<{ex}K> <{ex}hk> <{ex}head> .\n")
        .into_bytes()
        .into();
    let add = StatementsRequest::new(vec![StatementOp::Add {
        data: crate::RdfPayload {
            payload,
            format: crate::GraphResultFormat::NTriples,
            base_iri: None,
        },
        contexts: Vec::new(),
    }]);
    let memory = store.settings.query_envelope(&prepared);
    let inferred = |view: &nrese_engine::Snapshot| view.len_in(nrese_engine::ReadModel::Inferred);
    let scan = |view: &nrese_engine::Snapshot| Ok((store.lists_left_out(view), inferred(view)));
    let (pending, seen) =
        (store.with_pending(&add, &reader, &memory, &CancellationToken::new(), scan))
            .expect("the reader's pending view");
    let full = store.engine.snapshot();
    assert!(inferred(&full) > 0, "the setup: no inference in the store");
    let committed = store.lists_left_out(&full);
    assert_eq!(
        (committed, pending, seen),
        (false, false, 0),
        "(committed, pending, the inferences the reader's pending view holds)"
    );
}

/// Two writers and a literal equality's condition. The
/// first writer's commit derives an equality of two literals and fails; in the gap after
/// its failure, before its write returns, a second writer commits an equality of its own.
/// The second writes through a second pipeline over the store.
/// A certificate made again afterwards (a change of the QL setting, made under the writer
/// slot) still withholds the word from a read of the second writer's values: what the
/// failed commit leaves, or takes back, is never the second's.
#[test]
fn a_failed_commit_leaves_a_later_writers_literal_equality_withheld() {
    use std::sync::atomic::{AtomicBool, Ordering};
    const XSD: &str = "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";
    fn write(
        pipeline: &MutationPipeline,
        data: &str,
        note: Option<nrese_engine::CommitNote<'_>>,
    ) -> Result<(), String> {
        let update = format!("{PREFIXES}{XSD}INSERT DATA {{ {data} }}");
        let command = MutationCommand::Update(SparqlUpdateRequest::new(update));
        (pipeline.apply_noted(command, &Requester::all(), &MutationTicket::new(), note))
            .map(drop)
            .map_err(|error| format!("{error:?}"))
    }
    let store = Arc::new(
        StoreService::new(StoreConfig {
            ql_rewriting: crate::QlRewritingMode::On,
            ..StoreConfig::in_memory()
        })
        .expect("store"),
    );
    let reasoner = ReasonerService::new(ReasonerConfig::for_mode(ReasoningMode::Owl2Rl));
    let pipeline = Arc::new(MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(reasoner),
    ));
    write(&pipeline, ":u :r :w .", None).expect("the first commit");
    // A second pipeline over the store (one that takes it over, say): a pipeline's own
    // writers wait for its ground program, which the first holds until its write returns.
    let second = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    );
    let (second, ran) = (Arc::new(second), Arc::new(AtomicBool::new(false)));
    let seen = Arc::clone(&ran);
    hooks::AFTER_FAILED_COMMIT.set(Some(Box::new(move || {
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let equated = ":g a owl:FunctionalProperty . \
                           :t :g \"2\"^^xsd:integer , \"2.0\"^^xsd:decimal .";
            let _ = done.send(write(&second, equated, None));
        });
        let outcome = finished
            .recv_timeout(WAIT)
            .expect("the second writer commits");
        outcome.expect("the second writer's commit");
        seen.store(true, Ordering::SeqCst);
    })));
    let refused: nrese_engine::CommitNote<'_> = Box::new(|_| {
        Err(nrese_engine::NoteError::Refused(std::io::Error::other(
            "refused by the test",
        )))
    });
    let first = ":p a owl:FunctionalProperty . \
                 :s :p \"1\"^^xsd:integer , \"1.0\"^^xsd:decimal .";
    let failed = write(&pipeline, first, Some(refused));
    hooks::AFTER_FAILED_COMMIT.set(None);
    assert!(
        failed.is_err(),
        "the setup: the first writer's commit didn't fail"
    );
    assert!(
        ran.load(Ordering::SeqCst),
        "the setup: no writer ran in the gap"
    );
    store.set_ql_rewriting(crate::QlRewritingMode::On);
    let request = SparqlQueryRequest::all(format!("{PREFIXES}SELECT ?v {{ :t :g ?v }}"));
    let status = store.execute_query(&request).expect("read").completeness;
    let reasons: Vec<String> = status
        .iter()
        .flat_map(|s| &s.reasons)
        .map(|r| r.text.clone())
        .collect();
    assert!(
        status.as_ref().is_some_and(|s| s.as_str() != "complete")
            && reasons
                .iter()
                .any(|r| r.contains("an owl:sameAs between literals")),
        "the second writer's equality is no longer withheld: {} {reasons:?}",
        header(status.clone())
    );
}

/// An ill-typed literal (OWL 2 Structural Specification §5.7): [`PREFIXES`] names no `xsd:`.
const ILL_TYPED: &str = ":a :d \"abc\"^^<http://www.w3.org/2001/XMLSchema#integer> .";

thread_local! {
    /// How many more memory readings of a [`counted`] store succeed; unset: every one.
    static READINGS: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// The memory reading of a store given it by [`StoreService::with_capacity`]: readable while
/// [`READINGS`] is unset, else for that many readings more, then unreadable (the store's
/// operations stop, failing closed).
fn counted() -> nrese_exec::memory::Reading {
    match READINGS.get() {
        Some(0) => Err(nrese_exec::memory::Unread::Failed),
        Some(left) => {
            READINGS.set(Some(left - 1));
            nrese_exec::memory::capacity()
        }
        None => nrese_exec::memory::capacity(),
    }
}

/// Under `owl2-ql`, a store far shorter than one poll of the search for an ill-typed literal
/// (4096 statements), its memory unreadable: the search stops before it starts, so the
/// consistency is `unknown` and the read isn't `complete`, and nothing is kept. With the memory
/// readable again, the next need searches, finds none, and `consistent` and the word come back.
#[test]
fn an_ill_typed_search_stopped_before_it_starts_keeps_nothing_and_the_next_need_searches() {
    use crate::ConsistencyStatus::{Consistent, Unknown};
    let store = StoreService::new(StoreConfig::in_memory())
        .expect("store")
        .with_capacity(counted);
    load(&store, DATA);
    store.rematerialise(Ruleset::Owl2Ql).expect("reasoned");
    READINGS.set(Some(0));
    let stopped = (store.consistency(), status(&store, READ));
    READINGS.set(None);
    let again = (store.consistency(), status(&store, READ));
    assert!(
        stopped.0 == Unknown && !stopped.1.starts_with("complete"),
        "stopped: {stopped:?}"
    );
    assert!(
        again.0 == Consistent && again.1.starts_with("complete"),
        "again: {again:?}"
    );
}

/// Under `owl2-dl`, a commit whose search for an ill-typed literal stops (the memory readable
/// for its reasoning, the first reading after its preflight, and unreadable from its DL gate
/// on) decides nothing: no verdict is recorded at its revision. With the memory readable
/// again, the next read searches, finds none, and is `complete` and `consistent`; recorded as
/// found, the revision would stay withheld.
#[test]
fn an_ill_typed_search_stopped_at_a_dl_commit_records_no_verdict_and_the_next_read_searches() {
    let store = StoreService::new(StoreConfig::in_memory())
        .expect("store")
        .with_capacity(counted);
    let store = Arc::new(store);
    let dl = pipeline(&store, ReasoningMode::Owl2Dl);
    commit(&dl, DATA);
    hooks::AFTER_PREFLIGHT.set(Some(Box::new(|| READINGS.set(Some(1)))));
    commit(&dl, ":fido a :Dog .");
    READINGS.set(None);
    let at = store.current_revision();
    let recorded = (store.dl().status()).filter(|s| s.revision == at);
    assert!(
        recorded.is_none(),
        "a verdict recorded at the stopped revision: {recorded:?}"
    );
    let read = (status(&store, READ), store.consistency());
    assert!(
        read.0.starts_with("complete") && read.1 == crate::ConsistencyStatus::Consistent,
        "after the stop: {read:?}"
    );
}

/// A consistency reading inside a registration of rules, after the rules are registered and
/// before a certificate is made under them: a store holding an ill-typed literal, materialised
/// under `owl2-rl` (its certificate that program's, which keeps no witness: `consistent`), is
/// given an `owl2-ql` pipeline. During registration a read still sees the old version's
/// verdict. Once the replacement record is published it answers unknown.
#[test]
fn a_consistency_read_during_rule_registration_answers_the_published_record() {
    use crate::ConsistencyStatus::{Consistent, Unknown};
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).expect("store"));
    commit(&pipeline(&store, ReasoningMode::Disabled), ILL_TYPED);
    store.rematerialise(Ruleset::Owl2Rl).expect("reasoned");
    assert_eq!(store.consistency(), Consistent);
    let (reader, seen) = (Arc::clone(&store), Arc::new(Mutex::new(None)));
    let into = Arc::clone(&seen);
    hooks::AFTER_RULES_REGISTERED.set(Some(Box::new(move || {
        *into.lock().unwrap() = Some(reader.consistency());
    })));
    let _ql = pipeline(&store, ReasoningMode::Owl2Ql);
    hooks::AFTER_RULES_REGISTERED.set(None);
    assert_eq!(*seen.lock().unwrap(), Some(Consistent), "read in the gap");
    assert_eq!(store.consistency(), Unknown, "read after recertification");
}

/// A commit's search for its base's ill-typed literal (`dl::query::ill_typed_after`) where an
/// earlier commit deleted the base's 20 000 occurrences, its deletions a run of their own (left
/// uncompacted: the run they were inserted in, with 100 000 other statements, is more than the
/// compaction's fanout times larger), and the commit inserts one more, its stop raised once
/// the search has begun: the search stops (nothing kept) within its poll, while it passes the
/// deleted keys, before it reaches the one inserted.
#[test]
fn an_ill_typed_search_past_deleted_occurrences_stops_within_its_poll() {
    use nrese_engine::{EncodedQuad, Engine, EngineConfig, QuadPattern, ReadModel};
    let engine = Engine::new(EngineConfig::default()).expect("engine");
    let ex = "http://example.org/";
    let integer = "\"abc\"^^<http://www.w3.org/2001/XMLSchema#integer>";
    let held = (0..20_000).map(|i| format!("<{ex}s{i}> <{ex}d> {integer} .\n"));
    let other = (0..100_000).map(|i| format!("<{ex}f{i}> <{ex}e> <{ex}g> .\n"));
    let data: String = held.chain(other).collect();
    let mut tx = engine.transaction();
    let parser = nrese_rdf_io::RdfParser::from_format(nrese_rdf_io::RdfFormat::Turtle);
    for quad in parser.for_reader(data.as_bytes()) {
        tx.insert(quad.expect("a statement").as_ref());
    }
    tx.commit().expect("inserted");
    let snapshot = engine.snapshot();
    let mut all = snapshot.quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all());
    let any = all.find(|q| q.object.kind().is_dictionary_literal());
    let any = any.expect("the literal");
    let mut held = QuadPattern::all();
    held.object = Some(any.object);
    let mut gone = engine.transaction();
    gone.remove_matching(&held).expect("deleted");
    gone.commit().expect("deleted");
    let mut tx = engine.transaction();
    let keep = tx.intern(nrese_rdf::NamedNodeRef::new_unchecked("http://example.org/keep").into());
    tx.insert_encoded(EncodedQuad::new(keep, any.predicate, any.object, any.graph));
    let asked = std::cell::Cell::new(0u32);
    let stop = || {
        asked.set(asked.get() + 1);
        asked.get() > 1
    };
    let found = crate::dl::query::ill_typed_after(&tx, Some(any.object), &stop);
    assert_eq!((found, asked.get()), (None, 2), "found, and the stop asked");
}

/// A commit on a store in quarantine whose rules equate two literals (two spellings of one
/// value under a functional property), under OWL 2 RL with the QL rewriting on. The
/// quarantine's invalidation takes the kept state and its marker before the commit publishes,
/// so the mark the equality calls for rests on the version's certificate alone. A read of the
/// equated values is `sound-only`, naming the equality:
/// - after a repair commit's publication, before its revalidation;
/// - after its revalidation, which takes the store out of quarantine;
/// - after a certificate made again (`use_reasoning_rules`) under OWL 2 RL. Under other rules
///   first, the read is `not-certified`: the certificate was made again, and none vouches.
///
/// A repair commit of an equality that fails after staging keeps the old assessed hold.
/// A read of the property still returns only the old value.
#[test]
fn a_commit_in_quarantine_that_equates_two_literals_is_never_told_complete() {
    const INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
    const DECIMAL: &str = "http://www.w3.org/2001/XMLSchema#decimal";
    const EQUALITY: &str = "an owl:sameAs between literals";
    type Answer = (Vec<(String, String)>, Option<nrese_sparql::Completeness>);
    /// A store in `dir` holding `data` and `:a a owl:Nothing`, reasoned under OWL 2 RL with the
    /// QL rewriting on: inconsistent, so in quarantine.
    fn quarantined(dir: &Path, data: &str) -> Arc<StoreService> {
        let config = StoreConfig {
            ql_rewriting: crate::QlRewritingMode::On,
            ..StoreConfig::on_disk(dir)
        };
        let store = StoreService::new(config).expect("store");
        load(&store, &format!("{data} :a a owl:Nothing ."));
        store.rematerialise(Ruleset::Owl2Rl).expect("reasoned");
        Arc::new(store)
    }
    /// The values of `?v` in `select` (over [`PREFIXES`]), as (lexical form, datatype), sorted,
    /// and the answer's status.
    fn answer(store: &StoreService, select: &str) -> Answer {
        let request = SparqlQueryRequest::all(format!("{PREFIXES}{select}"));
        let answer = store.execute_query(&request).expect("answered");
        let json: serde_json::Value = serde_json::from_slice(&answer.payload).expect("json");
        let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_owned();
        let mut values: Vec<_> = (json["results"]["bindings"].as_array().into_iter().flatten())
            .map(|b| (text(&b["v"]["value"]), text(&b["v"]["datatype"])))
            .collect();
        values.sort();
        (values, answer.completeness)
    }
    /// The status's word and its reasons, for the cell's line.
    fn said(status: &Option<nrese_sparql::Completeness>) -> String {
        let reasons: Vec<&str> = (status.iter())
            .flat_map(|s| s.reasons.iter().map(|r| r.text.as_str()))
            .collect();
        format!(
            "{} {reasons:?}",
            status.as_ref().map_or("none", |s| s.as_str())
        )
    }
    /// `sound-only`, naming the equality.
    fn withheld(status: &Option<nrese_sparql::Completeness>) -> bool {
        status.as_ref().is_some_and(|s| {
            s.as_str() == "sound-only" && s.reasons.iter().any(|r| r.text.contains(EQUALITY))
        })
    }
    let mut cells = Cells::default();
    let dir = tempfile::tempdir().expect("dir");
    let equated =
        format!(":g a owl:FunctionalProperty . :t :g \"2\"^^<{INTEGER}> , \"2.0\"^^<{DECIMAL}> .");
    let read = "SELECT ?v { :t :g ?v }";

    // The repair commit that succeeds.
    let store = quarantined(&dir.path().join("committed"), "");
    let rl = pipeline(&store, ReasoningMode::Owl2Rl);
    let before = store.is_quarantined();
    let seen = Arc::new(Mutex::new(None));
    let (reader, kept) = (Arc::clone(&store), Arc::clone(&seen));
    hooks::BEFORE_REVALIDATION.set(Some(Box::new(move || {
        *kept.lock().unwrap() = Some((reader.is_quarantined(), answer(&reader, read)));
    })));
    let repaired = rl.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(format!(
            "{PREFIXES}DELETE DATA {{ :a a owl:Nothing }} ; INSERT DATA {{ {equated} }}"
        ))),
        &Requester::all(),
        &MutationTicket::new(),
    );
    hooks::BEFORE_REVALIDATION.set(None);
    let seen = seen.lock().unwrap().take();
    match (before, repaired, seen) {
        (false, _, _) => cells.unmet("a repair commit", "the store wasn't in quarantine"),
        (true, Err(error), _) => {
            cells.unmet("a repair commit", &format!("the commit failed: {error}"))
        }
        (true, Ok(_), None) => cells.unmet(
            "a repair commit",
            "the commit didn't reach its revalidation",
        ),
        (true, Ok(_), Some((quarantined, (values, status)))) => {
            if !quarantined {
                cells.unmet(
                    "a repair commit before its revalidation",
                    "the store wasn't in quarantine",
                );
            }
            let name = "a repair commit before its revalidation";
            println!("{name}: values {values:?}");
            cells.check(name, &said(&status), |_| withheld(&status), "withheld");
            let name = "a repair commit after its revalidation";
            let left = !store.is_quarantined();
            cells.check(
                &format!("{name} | out of quarantine"),
                &left.to_string(),
                |_| left,
                "true",
            );
            let (values, status) = answer(&store, read);
            println!("{name}: values {values:?}");
            cells.check(name, &said(&status), |_| withheld(&status), "withheld");
            store.use_reasoning_rules(Some(Ruleset::Rdfs.into()));
            let (_, status) = answer(&store, read);
            let name = "a repair commit and a certificate made again under rdfs";
            let none = status
                .as_ref()
                .is_some_and(|s| s.as_str() == "not-certified" && s.reasons.is_empty());
            cells.check(name, &said(&status), |_| none, "not-certified, no reasons");
            store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
            let (values, status) = answer(&store, read);
            let name = "a repair commit and a certificate made again under owl2-rl";
            println!("{name}: values {values:?}");
            cells.check(name, &said(&status), |_| withheld(&status), "withheld");
        }
    }

    // The repair commit that fails after staging: its log write is refused.
    let store = quarantined(
        &dir.path().join("failed"),
        &format!(":s :p \"1\"^^<{INTEGER}> ."),
    );
    let rl = pipeline(&store, ReasoningMode::Owl2Rl);
    let before = store.is_quarantined();
    let refused: nrese_engine::CommitNote<'_> = Box::new(|_| {
        Err(nrese_engine::NoteError::Refused(std::io::Error::other(
            "refused by the test",
        )))
    });
    let failed = rl.apply_noted(
        MutationCommand::Update(SparqlUpdateRequest::new(format!(
            "{PREFIXES}DELETE DATA {{ :a a owl:Nothing }} ; \
             INSERT DATA {{ :p a owl:FunctionalProperty . :s :p \"1.0\"^^<{DECIMAL}> }}"
        ))),
        &Requester::all(),
        &MutationTicket::new(),
        Some(refused),
    );
    let by_the_note = |error: &crate::MutationError| {
        matches!(
            error,
            crate::MutationError::Store {
                source: crate::StoreError::Engine(nrese_engine::EngineError::CommitNoteRefused(_)),
                ..
            }
        )
    };
    match (before, failed) {
        (false, _) => cells.unmet("a failed commit", "the store wasn't in quarantine"),
        (true, Ok(_)) => cells.unmet("a failed commit", "the commit didn't fail"),
        (true, Err(error)) if !by_the_note(&error) => cells.unmet(
            "a failed commit",
            &format!("the commit failed otherwise: {error:?}"),
        ),
        (true, Err(_)) => {
            store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
            let name = "a failed commit and a certificate made again";
            let (values, status) = answer(&store, "SELECT ?v { :s :p ?v }");
            let verdict = store.consistency();
            cells.check(
                &format!("{name} | the held verdict"),
                verdict.as_str(),
                |word| word == "inconsistent" && store.is_quarantined(),
                "inconsistent and held",
            );
            let complete =
                |s: &Option<nrese_sparql::Completeness>| s.as_ref().is_some_and(|s| s.complete);
            cells.check(
                &format!("{name} | the status"),
                &said(&status),
                |_| status.is_some() && !complete(&status),
                "not complete",
            );
            let stored = vec![("1".to_owned(), INTEGER.to_owned())];
            cells.check(
                &format!("{name} | the answer"),
                &format!("{values:?}"),
                |_| values == stored,
                &format!("{stored:?}"),
            );
        }
    }
    cells.finish();
}
