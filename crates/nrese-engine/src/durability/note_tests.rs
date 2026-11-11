//! A commit's note in a journal beside the log (`Transaction::note_commit`, Q06 remainder):
//! called once with the revision the commit publishes, before any byte of its record is
//! written, and never where no other commit may take a revision a journal may name. New API:
//! these are spec tests with no red of their own on the base (it has no note).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use super::tests::{commit, config, contents, quad};
use super::{fault, wal};
use crate::{Engine, EngineConfig, EngineError, NoteError};

/// How a row of [`no_commit_takes_a_revision_a_journal_may_name`] notes its commit.
type Noting = fn(&mut crate::Transaction<'_>);

/// The bytes of the store's log.
fn logged(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(wal::wal_dir(dir))
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum()
}

/// The note's revision, or `u64::MAX` while it isn't called.
fn noted() -> Arc<AtomicU64> {
    Arc::new(AtomicU64::new(u64::MAX))
}

/// The note is called once, with the revision the commit then publishes, while nothing of its
/// record is in the log; the record follows it. Without changes, or speculative, it isn't
/// called (the commit publishes nothing, or can't commit).
#[test]
fn a_note_is_called_with_the_published_revision_before_its_record() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    commit(&engine, 1).unwrap();
    let before = logged(dir.path());
    let revision = noted();
    let at_note = Arc::new(AtomicU64::new(u64::MAX));
    let mut tx = engine.transaction();
    tx.insert(quad(2).as_ref());
    let (r, bytes, path) = (
        Arc::clone(&revision),
        Arc::clone(&at_note),
        dir.path().to_owned(),
    );
    tx.note_commit(Box::new(move |n| {
        assert_eq!(r.swap(n, Ordering::SeqCst), u64::MAX, "called once");
        bytes.store(logged(&path), Ordering::SeqCst);
        Ok(())
    }));
    let summary = tx.commit().unwrap();
    let mut wrong = Vec::new();
    if revision.load(Ordering::SeqCst) != summary.revision {
        wrong.push(format!(
            "noted {}, published {}",
            revision.load(Ordering::SeqCst),
            summary.revision
        ));
    }
    if at_note.load(Ordering::SeqCst) != before {
        wrong.push("the record's bytes were in the log when the note ran".to_owned());
    }
    if logged(dir.path()) <= before {
        wrong.push("no record after the note".to_owned());
    }
    // Nothing to publish: not called.
    let unchanged = noted();
    let mut tx = engine.transaction();
    let n = Arc::clone(&unchanged);
    tx.note_commit(Box::new(move |r| {
        n.store(r, Ordering::SeqCst);
        Ok(())
    }));
    let kept = tx.commit().unwrap();
    if unchanged.load(Ordering::SeqCst) != u64::MAX || kept.revision != summary.revision {
        wrong.push("an empty commit called its note or took a revision".to_owned());
    }
    // Speculative: refused, not called.
    let speculative = noted();
    // The shared kind until V1b: only a `Transaction` has a commit to refuse (a private
    // transaction has none, by type).
    #[allow(clippy::disallowed_methods)]
    let mut tx = engine.speculative_shared();
    tx.insert(quad(3).as_ref());
    let n = Arc::clone(&speculative);
    tx.note_commit(Box::new(move |r| {
        n.store(r, Ordering::SeqCst);
        Ok(())
    }));
    if !matches!(tx.commit(), Err(EngineError::Speculative))
        || speculative.load(Ordering::SeqCst) != u64::MAX
    {
        wrong.push("a speculative transaction committed or called its note".to_owned());
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// A refusing note refuses the commit with nothing written; the log stays usable and the next
/// commit takes the same revision. After a restart, the refused commit isn't there.
#[test]
fn a_refusing_note_leaves_the_log_as_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let engine = Engine::open(dir.path(), config()).unwrap();
    commit(&engine, 1).unwrap();
    let before = logged(dir.path());
    let mut tx = engine.transaction();
    tx.insert(quad(2).as_ref());
    tx.note_commit(Box::new(|_| {
        Err(NoteError::Refused(std::io::Error::other("refused")))
    }));
    let refused = tx.commit();
    let mut wrong = Vec::new();
    if !matches!(refused, Err(EngineError::CommitNoteRefused(_))) {
        wrong.push(format!("the commit: {refused:?}"));
    }
    if logged(dir.path()) != before {
        wrong.push("the refused commit wrote to the log".to_owned());
    }
    match commit(&engine, 3) {
        Ok(summary) if summary.revision == 2 => {}
        other => wrong.push(format!("the next commit: {other:?}, not revision 2")),
    }
    drop(engine);
    let engine = Engine::open(dir.path(), config()).unwrap();
    if contents(&engine) != [quad(1), quad(3)].into_iter().collect() {
        wrong.push(format!("after a restart: {:?}", contents(&engine)));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Wherever a journal may name the revision, no later commit in the process takes it: a note
/// whose journal is unknown, a record write that fails after the note, and a note that
/// panics each leave the log poisoned. After a restart, the store holds what was durable and
/// takes commits again. Rows are judged on their own.
#[test]
fn no_commit_takes_a_revision_a_journal_may_name() {
    let rows: [(&str, Noting); 3] = [
        ("an unknown journal", |tx| {
            tx.note_commit(Box::new(|_| {
                Err(NoteError::Unknown(std::io::Error::other("unknown")))
            }))
        }),
        ("the record's write fails after the note", |tx| {
            tx.note_commit(Box::new(|_| {
                fault::FAIL_RECORD_WRITE.set(true);
                Ok(())
            }))
        }),
        ("the note panics", |tx| {
            tx.note_commit(Box::new(|_| panic!("a note that panics (a test's)")))
        }),
    ];
    let mut wrong = Vec::new();
    for (row, note) in rows {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::open(dir.path(), config()).unwrap();
        commit(&engine, 1).unwrap();
        let mut tx = engine.transaction();
        tx.insert(quad(2).as_ref());
        note(&mut tx);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tx.commit()));
        fault::FAIL_RECORD_WRITE.set(false);
        if matches!(failed, Ok(Ok(_))) {
            wrong.push(format!("row {row}: committed"));
        }
        let next = commit(&engine, 3);
        eprintln!("row {row}: the next commit {next:?}");
        if !matches!(next, Err(EngineError::WalPoisoned)) {
            wrong.push(format!(
                "row {row}: a later commit wasn't refused: {next:?}"
            ));
        }
        drop(engine);
        let engine = Engine::open(dir.path(), config()).unwrap();
        if contents(&engine) != [quad(1)].into_iter().collect() {
            wrong.push(format!(
                "row {row}: after a restart {:?}",
                contents(&engine)
            ));
        }
        if commit(&engine, 4).is_err() {
            wrong.push(format!("row {row}: no commit after the restart"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// In memory there is no log to poison: an unknown journal refuses the commit as a refusal
/// does, and the next commit takes the same revision. A note that returns `Ok` sees the
/// published revision.
#[test]
fn in_memory_a_note_refuses_or_sees_its_revision() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    commit(&engine, 1).unwrap();
    let mut tx = engine.transaction();
    tx.insert(quad(2).as_ref());
    tx.note_commit(Box::new(|_| {
        Err(NoteError::Unknown(std::io::Error::other("unknown")))
    }));
    let mut wrong = Vec::new();
    let refused = tx.commit();
    if !matches!(refused, Err(EngineError::CommitNoteRefused(_))) {
        wrong.push(format!("the unknown journal's commit: {refused:?}"));
    }
    let revision = noted();
    let mut tx = engine.transaction();
    tx.insert(quad(3).as_ref());
    let n = Arc::clone(&revision);
    tx.note_commit(Box::new(move |r| {
        n.store(r, Ordering::SeqCst);
        Ok(())
    }));
    match tx.commit() {
        Ok(summary) if summary.revision == 2 && revision.load(Ordering::SeqCst) == 2 => {}
        other => wrong.push(format!(
            "the next commit: {other:?}, noted {}",
            revision.load(Ordering::SeqCst)
        )),
    }
    if contents(&engine) != [quad(1), quad(3)].into_iter().collect() {
        wrong.push(format!("contents {:?}", contents(&engine)));
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}
