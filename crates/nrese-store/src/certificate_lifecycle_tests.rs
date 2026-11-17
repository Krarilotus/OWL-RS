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

/// A store in `dir` holding `data` (Turtle over [`PREFIXES`]'s names), reasoned under OWL 2
/// RL, its memory read through [`capacity`].
fn reasoned(dir: &Path, data: &str) -> Arc<StoreService> {
    let store = StoreService::new(StoreConfig::on_disk(dir))
        .expect("store")
        .with_capacity(capacity);
    load(&store, data);
    store.rematerialise(Ruleset::Owl2Rl).expect("reasoned");
    Arc::new(store)
}

/// Commits `DELETE DATA { data }` through `pipeline`, its revalidation stopped: the store's
/// memory can't be read from the commit's publication on. Whether the commit succeeded, and
/// whether the memory was read while it couldn't be.
fn repaired_unrevalidated(pipeline: &MutationPipeline, data: &str) -> (Result<(), String>, bool) {
    hooks::BEFORE_REVALIDATION.set(Some(Box::new(|| UNREAD.set((true, false)))));
    let repaired = pipeline.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(format!(
            "{PREFIXES}DELETE DATA {{ {data} }}"
        ))),
        &Requester::all(),
        &MutationTicket::new(),
    );
    hooks::BEFORE_REVALIDATION.set(None);
    let (_, tried) = UNREAD.get();
    UNREAD.set((false, false));
    (repaired.map(|_| ()).map_err(|e| e.to_string()), tried)
}

/// A readiness reading (`is_quarantined`) with a change of the store's assessment between
/// its reads, where the store is in quarantine before the change and after it, says
/// quarantined: whatever it read first and last, the store was never ready.
/// - Across a repair commit whose revalidation stops: before it, the data is inconsistent;
///   after it, the consistency isn't assessed again.
/// - Across a rematerialisation that records the data inconsistent: before it, a repair
///   commit's revalidation stopped and the consistency isn't assessed; after it, the data is
///   inconsistent.
#[test]
fn a_readiness_reading_across_a_change_of_quarantine_says_quarantined() {
    use crate::ConsistencyStatus::{Inconsistent, Unknown};
    let mut cells = Cells::default();
    let readiness = |quarantined: bool| match quarantined {
        true => "quarantined",
        false => "ready",
    };
    let quarantined = |h: &str| h == "quarantined";

    let dir = tempfile::tempdir().expect("dir");
    let store = reasoned(dir.path(), &format!("{DATA} :a a owl:Nothing ."));
    let before = store.consistency();
    let rl = pipeline(&store, ReasoningMode::Owl2Rl);
    let kept = Arc::new(Mutex::new(None));
    let into = Arc::clone(&kept);
    hooks::BETWEEN_QUARANTINE_READS.set(Some(Box::new(move || {
        *into.lock().unwrap() = Some(repaired_unrevalidated(&rl, ":a a owl:Nothing"));
    })));
    let read = store.is_quarantined();
    hooks::BETWEEN_QUARANTINE_READS.set(None);
    let after = store.consistency();
    match (before, kept.lock().unwrap().take(), after) {
        (Inconsistent { .. }, Some((Ok(()), true)), Unknown) => cells.check(
            "a reading across a repair commit | readiness",
            readiness(read),
            quarantined,
            "quarantined",
        ),
        (before, committed, after) => cells.unmet(
            "a reading across a repair commit",
            &format!("{before:?}, then {committed:?} (committed, the memory read), then {after:?}"),
        ),
    }

    let dir = tempfile::tempdir().expect("dir");
    let store = reasoned(
        dir.path(),
        &format!("{DATA} :a a owl:Nothing . :b a owl:Nothing ."),
    );
    let (repaired, tried) =
        repaired_unrevalidated(&pipeline(&store, ReasoningMode::Owl2Rl), ":a a owl:Nothing");
    let before = store.consistency();
    let (recorder, kept) = (Arc::clone(&store), Arc::new(Mutex::new(None)));
    let into = Arc::clone(&kept);
    hooks::BETWEEN_QUARANTINE_READS.set(Some(Box::new(move || {
        let recorded = recorder.rematerialise(Ruleset::Owl2Rl);
        *into.lock().unwrap() = Some(recorded.map(|_| ()).map_err(|e| e.to_string()));
    })));
    let read = store.is_quarantined();
    hooks::BETWEEN_QUARANTINE_READS.set(None);
    let after = store.consistency();
    match (repaired, tried, before, kept.lock().unwrap().take(), after) {
        (Ok(()), true, Unknown, Some(Ok(())), Inconsistent { violations: 1 }) => cells.check(
            "a reading across a recorded assessment | readiness",
            readiness(read),
            quarantined,
            "quarantined",
        ),
        (repaired, tried, before, recorded, after) => cells.unmet(
            "a reading across a recorded assessment",
            &format!(
                "repaired {repaired:?} (the memory read: {tried}), then {before:?}, \
                 recorded {recorded:?}, then {after:?}"
            ),
        ),
    }
    cells.finish();
}
