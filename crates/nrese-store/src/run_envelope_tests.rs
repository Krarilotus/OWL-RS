//! Spec: each run of a prepared query draws on a memory envelope of its own, opened by the
//! store that runs it from the query's memory limit within its catalog's budget. Runs of one
//! prepared query, at once or one after another, share neither a limit nor a refusal; a
//! query prepared by one catalog's store and run by another's charges the runner's budget.

use std::io::Write;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use super::StoreService;
use crate::{CancellationToken, PreparedQuery, SparqlQueryRequest, StoreConfig};

/// A store without a result cache (every run evaluates), whose queries share a budget under
/// 256 MiB, where an envelope keeps no credit: the budget holds exactly what runs hold.
fn store() -> StoreService {
    StoreService::new(StoreConfig {
        total_query_memory_bytes: 128 << 20,
        query_cache_bytes: 0,
        ..StoreConfig::in_memory()
    })
    .unwrap()
}

/// 40,000 rows, which a run holds while it writes them.
const QUERY: &str = "SELECT ?a ?b WHERE { VALUES ?a { 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 \
     18 19 20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 \
     48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63 64 65 66 67 68 69 70 71 72 73 74 75 76 77 \
     78 79 80 81 82 83 84 85 86 87 88 89 90 91 92 93 94 95 96 97 98 99 100 101 102 103 104 105 \
     106 107 108 109 110 111 112 113 114 115 116 117 118 119 120 121 122 123 124 125 126 127 \
     128 129 130 131 132 133 134 135 136 137 138 139 140 141 142 143 144 145 146 147 148 149 \
     150 151 152 153 154 155 156 157 158 159 160 161 162 163 164 165 166 167 168 169 170 171 \
     172 173 174 175 176 177 178 179 180 181 182 183 184 185 186 187 188 189 190 191 192 193 \
     194 195 196 197 198 199 200 } VALUES ?b { 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 \
     20 21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 \
     50 51 52 53 54 55 56 57 58 59 60 61 62 63 64 65 66 67 68 69 70 71 72 73 74 75 76 77 78 79 \
     80 81 82 83 84 85 86 87 88 89 90 91 92 93 94 95 96 97 98 99 100 101 102 103 104 105 106 \
     107 108 109 110 111 112 113 114 115 116 117 118 119 120 121 122 123 124 125 126 127 128 \
     129 130 131 132 133 134 135 136 137 138 139 140 141 142 143 144 145 146 147 148 149 150 \
     151 152 153 154 155 156 157 158 159 160 161 162 163 164 165 166 167 168 169 170 171 172 \
     173 174 175 176 177 178 179 180 181 182 183 184 185 186 187 188 189 190 191 192 193 194 \
     195 196 197 198 199 200 } }";

fn prepared(store: &StoreService, limit: Option<usize>) -> PreparedQuery {
    let mut request = SparqlQueryRequest::all(QUERY);
    request.memory_limit = limit;
    store.prepare_query(&request).unwrap()
}

/// The most [`QUERY`]'s run holds, alone: the shared budget's peak over one run on a store
/// of its own.
fn one_runs_peak() -> usize {
    let store = store();
    let before = store.query_memory().unwrap().0;
    store
        .run_query(
            &prepared(&store, None),
            &CancellationToken::new(),
            std::io::sink(),
        )
        .unwrap();
    store.query_memory().unwrap().1 - before
}

/// A writer that holds its run once it has rows written (past 4 KiB: past any head of the
/// results), saying so, until it is let go.
struct HeldWriter {
    written: usize,
    reached: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
}

impl Write for HeldWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.written += bytes.len();
        if self.written > 4096
            && let Some(reached) = self.reached.take()
        {
            reached.send(()).unwrap();
            let _ = self.release.recv_timeout(Duration::from_secs(120));
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Two runs of one prepared query at once, each within its limit alone, are both answered:
/// the first, held while it writes (holding its rows), doesn't take the second's room.
#[test]
fn two_runs_of_one_prepared_query_at_once_have_a_limit_each() {
    let peak = one_runs_peak();
    let store = Arc::new(store());
    let prepared = Arc::new(prepared(&store, Some(peak + peak / 16)));
    let (reached, at_write) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let first = {
        let (store, prepared) = (Arc::clone(&store), Arc::clone(&prepared));
        std::thread::spawn(move || {
            let out = HeldWriter {
                written: 0,
                reached: Some(reached),
                release: released,
            };
            store.run_query(&prepared, &CancellationToken::new(), out)
        })
    };
    at_write
        .recv_timeout(Duration::from_secs(120))
        .expect("the first run writes");
    let second = store.run_query(&prepared, &CancellationToken::new(), std::io::sink());
    release.send(()).unwrap();
    let first = first.join().unwrap();
    println!("peak {peak}: first {first:?}, second {second:?}");
    assert!(second.is_ok(), "{second:?}");
    assert!(first.is_ok(), "{first:?}");
}

/// A run after a refused one draws on an envelope of its own, with nothing of the earlier
/// run's refusal in it.
#[test]
fn a_retry_after_a_refusal_starts_unrefused() {
    let store = store();
    let prepared = prepared(&store, Some(1 << 20));
    let first = store.settings.query_envelope(&prepared);
    assert!(first.charge(2 << 20).is_err());
    assert!(first.refused());
    let retry = store.settings.query_envelope(&prepared);
    assert!(
        retry != first,
        "the retry draws on the refused run's envelope"
    );
    assert!(!retry.refused());
    assert!(retry.charge(1 << 19).is_ok());
}

/// A query one catalog's store prepared, run by another's, charges the runner's budget.
#[test]
fn a_run_charges_the_budget_of_the_store_that_runs_it() {
    let (preparer, runner) = (store(), store());
    let prepared = prepared(&preparer, None);
    let (before, others) = (
        runner.query_memory().unwrap().0,
        preparer.query_memory().unwrap().0,
    );
    let envelope = runner.settings.query_envelope(&prepared);
    envelope.charge(1 << 20).unwrap();
    assert_eq!(runner.query_memory().unwrap().0, before + (1 << 20));
    assert_eq!(preparer.query_memory().unwrap().0, others);
}
