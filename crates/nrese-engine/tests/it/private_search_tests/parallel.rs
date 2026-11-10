//! Parallel readers of one retained pending snapshot.

use super::*;

/// The own-slot corpora under parallel readers (DICT2 9 Nov 06:35: no lock held across a
/// build that runs on the pool). Sixteen tasks on a pool of four threads search one retained
/// pending snapshot (a writer's, then a private one), cold, text and IRI in every scope,
/// each building or finding its corpora and `Local` while the others do; the walks run on
/// the same pool. Every answer equals the oracle's, and all finish within 120 s (a hang
/// fails here rather than holding the job).
#[test]
fn parallel_readers_of_one_pending_snapshot_answer_within_a_deadline() {
    let truth = oracle().snapshot();
    let want = answers(&truth);
    for private in [false, true] {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        base(&engine);
        let published = engine.snapshot();
        let mut tx = engine.transaction();
        let mut own = engine
            .speculative_over(&published, Arc::new(nrese_exec::Budget::unlimited()))
            .unwrap();
        let pending = match private {
            false => {
                for quad in inserted() {
                    tx.insert(quad.as_ref());
                }
                for quad in removed() {
                    tx.remove(quad.as_ref());
                }
                tx.pending_snapshot()
            }
            true => {
                for quad in inserted() {
                    own.try_insert(quad.as_ref()).unwrap();
                }
                for quad in removed() {
                    own.remove(quad.as_ref());
                }
                own.pending_snapshot().unwrap()
            }
        };
        let (done, finished) = std::sync::mpsc::channel();
        let readers = std::thread::spawn({
            let pending = pending.clone();
            move || {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(4)
                    .build()
                    .unwrap();
                let results = std::sync::Mutex::new(Vec::new());
                pool.scope(|scope| {
                    for _ in 0..16 {
                        scope.spawn(|_| {
                            let got = answers(&pending);
                            results.lock().unwrap().push(got);
                        });
                    }
                });
                let _ = done.send(results.into_inner().unwrap());
            }
        });
        let results = finished
            .recv_timeout(std::time::Duration::from_secs(120))
            .unwrap_or_else(|_| panic!("private {private}: readers still running after 120 s"));
        readers.join().unwrap();
        assert_eq!(results.len(), 16);
        let mut failed = Vec::new();
        for got in &results {
            rows_equal(got, &want, &format!("private {private}"), &mut failed);
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }
}
