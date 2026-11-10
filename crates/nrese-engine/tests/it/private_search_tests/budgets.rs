//! Budgets: what a private search builds is charged before it is made, to the budget its
//! caller's capability names, and refused without room.

use super::*;

/// Sol-loop r1 B1: a private search's own index is charged as an upper bound of what it
/// allocates. One own literal of 10,000 distinct words (about 60 KB of text; its index
/// holds 10,000 words with their postings, well past 256 KiB) within a 256 KiB request is
/// refused; within 64 MiB it is found. Red before the fix: the old estimate (three bytes a
/// text byte) charged about 180 KB and answered.
#[test]
fn a_private_search_charges_its_own_index_before_it_builds_it() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let published = engine.snapshot();
    let mut private = engine
        .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
        .unwrap();
    let text: Vec<String> = (0..10_000).map(|i| format!("w{i:04}")).collect();
    let text = format!("alpha {}", text.join(" "));
    private
        .try_insert(label("big", &text, None).as_ref())
        .unwrap();
    let pending = private.pending_snapshot().unwrap();
    let alpha = query("alpha", false, false);
    let search = |limit: usize| {
        let budget = Arc::new(nrese_exec::Budget::new(limit));
        pending.text_search_in(
            ReadModel::Materialised,
            &TextScope::EVERY,
            &alpha,
            Some(&budget),
        )
    };
    let small = search(256 << 10);
    assert!(small.is_err(), "within 256 KiB: {small:?}");
    let found = search(64 << 20).unwrap();
    assert!(
        decoded(&pending, &found).iter().any(|(t, _)| *t == text),
        "within 64 MiB"
    );
}

/// Sol-loop r2 B1: a word alone in its shard costs its index a whole B-tree root. One own
/// literal of 36 one-character words (`0` to `9`, `a` to `z`: 36 shards) needs about 29 KB
/// for its shard maps and roots alone; within 32 KiB it is refused (the r1 charge, about
/// 27.7 KB, admitted it), within 64 MiB found.
#[test]
fn a_private_search_charges_a_root_for_each_sparse_shard() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let published = engine.snapshot();
    let mut private = engine
        .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
        .unwrap();
    let words: Vec<String> = ('0'..='9').chain('a'..='z').map(String::from).collect();
    let text = words.join(" ");
    private
        .try_insert(label("sparse", &text, None).as_ref())
        .unwrap();
    let pending = private.pending_snapshot().unwrap();
    let z = query("z", false, false);
    let search = |limit: usize| {
        let budget = Arc::new(nrese_exec::Budget::new(limit));
        pending.text_search_in(
            ReadModel::Materialised,
            &TextScope::EVERY,
            &z,
            Some(&budget),
        )
    };
    let small = search(32 << 10);
    assert!(small.is_err(), "within 32 KiB: {small:?}");
    let found = search(64 << 20).unwrap();
    assert!(
        decoded(&pending, &found).iter().any(|(t, _)| *t == text),
        "within 64 MiB"
    );
}

/// Attack item 12: a private search within a budget too small for what it builds is
/// refused, never answered in part; within enough, it answers as the oracle does. Text, IRI
/// and vectors, the corpora cold.
#[test]
fn a_private_search_without_room_is_refused() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    base(&engine);
    let published = engine.snapshot();
    let mut private = engine
        .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
        .unwrap();
    for quad in inserted() {
        private.try_insert(quad.as_ref()).unwrap();
    }
    for quad in removed() {
        private.remove(quad.as_ref());
    }
    let pending = private.pending_snapshot().unwrap();
    let truth = oracle().snapshot();
    let model = ReadModel::Materialised;
    let text = query("alpha zeta", false, false);
    let tiny = Arc::new(nrese_exec::Budget::new(64));
    let mut failed = Vec::new();
    if pending
        .text_search_in(model, &TextScope::EVERY, &text, Some(&tiny))
        .is_ok()
    {
        failed.push("text within 64 bytes answered".to_owned());
    }
    if pending
        .iri_search_in(
            model,
            &TextScope::EVERY,
            &query("zebra", false, false),
            Some(&tiny),
        )
        .is_ok()
    {
        failed.push("IRI within 64 bytes answered".to_owned());
    }
    let near = nrese_engine::VectorQuery::new(vec![1.0, 0.0], 2);
    if pending
        .vector_search_in(model, &TextScope::EVERY, &near, &|_| true, &tiny)
        .is_ok()
    {
        failed.push("vectors within 64 bytes answered".to_owned());
    }
    let enough = Arc::new(nrese_exec::Budget::new(64 << 20));
    let got = pending
        .text_search_in(model, &TextScope::EVERY, &text, Some(&enough))
        .map(|found| decoded(&pending, &found));
    let want = truth
        .text_search_in(model, &TextScope::EVERY, &text, None)
        .map(|found| decoded(&truth, &found));
    if got.as_ref().ok() != want.as_ref().ok() || got.is_err() {
        failed.push(format!("within enough: {got:?} != {want:?}"));
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

/// V3 package review r1 B1: the global text and IRI entry points of a private snapshot
/// charge what they build to the budget its caller's capability names: its reader's
/// (attached with `reading_within`, as a SPARQL query attaches its own) where it has one,
/// else its private transaction's. The own documents: a literal of 10,000 distinct words and
/// an IRI whose local name splits into 10,000 words, cold. Rows: a reader within 256 KiB or
/// a transaction within 1 MiB refuse, before they build; within 64 MiB each finds its
/// document, and its budget holds the kept corpus and index while the snapshot lives and
/// has its charge back when it is dropped. Red on `a2873720`: the global entry points
/// charged an unlimited budget of their own, and answered.
#[test]
fn a_private_global_search_charges_its_readers_or_its_transactions_budget() {
    let words: Vec<String> = (0..10_000).map(|i| format!("w{i:04}")).collect();
    let text = format!("alpha {}", words.join(" "));
    let camel: String = (0..10_000).map(|i| format!("xW{i}")).collect();
    let iri = ex(&camel);
    struct Row {
        reader: Option<usize>,
        transaction: usize,
        refused: bool,
    }
    let rows = [
        Row {
            reader: Some(256 << 10),
            transaction: usize::MAX,
            refused: true,
        },
        Row {
            reader: Some(64 << 20),
            transaction: usize::MAX,
            refused: false,
        },
        Row {
            reader: None,
            transaction: 1 << 20,
            refused: true,
        },
        Row {
            reader: None,
            transaction: 64 << 20,
            refused: false,
        },
    ];
    let mut failed = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        for iris in [false, true] {
            let context = format!("row {at}, iris {iris}");
            let engine = Engine::new(EngineConfig::default()).unwrap();
            base(&engine);
            let published = engine.snapshot();
            let transaction = Arc::new(match row.transaction {
                usize::MAX => nrese_exec::Budget::unlimited(),
                limit => nrese_exec::Budget::new(limit),
            });
            let mut private = engine
                .speculative_over(&published, Arc::clone(&transaction))
                .unwrap();
            private
                .try_insert(label("big", &text, None).as_ref())
                .unwrap();
            private
                .try_insert(
                    Quad::new(iri.clone(), ex("p"), ex("o"), GraphName::DefaultGraph).as_ref(),
                )
                .unwrap();
            let pending = private.pending_snapshot().unwrap();
            let reader_budget = row
                .reader
                .map(|limit| Arc::new(nrese_exec::Budget::new(limit)));
            let reader = match &reader_budget {
                Some(budget) => pending.reading_within(Arc::clone(budget)).unwrap(),
                None => pending.clone(),
            };
            let charged = reader_budget.as_ref().unwrap_or(&transaction);
            let before = charged.used();
            let found = match iris {
                false => reader.text_search(&query("alpha", false, false)),
                true => reader.iri_search(&query("w17x", false, false)),
            };
            match (row.refused, found) {
                (true, Ok(found)) => failed.push(format!("{context}: answered {found:?}")),
                (false, Err(refused)) => failed.push(format!("{context}: refused {refused:?}")),
                (false, Ok(found)) => {
                    let names = decoded(&reader, &found);
                    let want = match iris {
                        false => text.clone(),
                        true => iri.as_str().to_owned(),
                    };
                    if !names.iter().any(|(name, _)| *name == want) {
                        failed.push(format!("{context}: its document not found"));
                    }
                    if charged.used() <= before {
                        failed.push(format!("{context}: nothing held on its budget"));
                    }
                }
                (true, Err(_)) => {}
            }
            drop((reader, pending));
            if charged.used() > before {
                failed.push(format!(
                    "{context}: {} bytes still charged after the snapshot dropped",
                    charged.used() - before
                ));
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}
