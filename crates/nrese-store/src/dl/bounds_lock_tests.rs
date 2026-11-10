//! U1's locks never wait for Rayon work (design: Harness notes `2026-11-02-p2-c6b-u1-design.md`,
//! invariants U1-I, U1-B, U1-S). A lock held across a commit's change or a read's build, which
//! run on the global pool, closes a cycle with every pool worker that waits for it; a read of a
//! built revision never waits for either.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::seam::{self, Point};
use super::*;
use crate::{
    MutationCommand, MutationPipeline, MutationTicket, Requester, SparqlUpdateRequest, StoreConfig,
};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};

/// Not OWL 2 RL (an existential on the right), so U1 is compiled; a few classes, so the
/// taxonomy's waves have more than one class; 32 members, so a change has parallel parts.
const SCHEMA: &str = "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
     INSERT DATA {
       :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] .
       :B rdfs:subClassOf [ owl:unionOf ( :C :D ) ] .
       :C rdfs:subClassOf :E . :D rdfs:subClassOf :E . :F rdfs:subClassOf :A .
     }";

fn members(n: usize) -> String {
    let mut data = String::from("PREFIX : <urn:test:> INSERT DATA {");
    for i in 0..n {
        data.push_str(&format!(" :a{i} a :A ."));
    }
    data.push('}');
    data
}

fn update(pipeline: &MutationPipeline, text: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(text)),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .expect("the update commits");
}

/// A store under `owl2-dl` and its pipeline, with the schema and its members committed.
fn pipeline() -> (Arc<StoreService>, MutationPipeline) {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).unwrap());
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    update(&pipeline, SCHEMA);
    update(&pipeline, &members(32));
    (store, pipeline)
}

/// A store whose data no commit of the pipeline wrote: U1 is built by the first read.
/// `n` members: from 8,192 (about 17,000 facts of U1 and L) a view's runs are built in
/// parallel (`index::run::PARALLEL_BUILD_THRESHOLD`, 16,384 quads).
fn unbuilt(n: usize) -> Arc<StoreService> {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).unwrap());
    store.update_str_ungated(SCHEMA).unwrap();
    store.update_str_ungated(&members(n)).unwrap();
    store
}

/// U1's facts with its own terms (Skolem constants, fresh classes) as one name, each kept
/// with its count: their numbering follows the clauses', which a commit's assertions shift
/// (as `tests/it/dl/bounds.rs`'s `canonical` has it), so U1 maintained and U1 afresh name
/// them differently; a lost or extra fact still shows.
fn canonical(facts: &[[String; 3]]) -> Vec<([String; 3], usize)> {
    let mut out = std::collections::BTreeMap::new();
    for fact in facts {
        let key = fact.clone().map(|t| match t.starts_with("<urn:nrese:u1:") {
            true => "<urn:nrese:u1:*>".to_owned(),
            false => t,
        });
        *out.entry(key).or_insert(0) += 1;
    }
    out.into_iter().collect()
}

const DELETE: &str = "PREFIX : <urn:test:> DELETE DATA { :a0 a :A . :a1 a :A . :a2 a :A . }";

/// A read of the revision U1 and its view describe gets that view while a commit computes
/// its change of U1: it waits for no lock the change holds.
#[test]
fn a_read_of_a_built_revision_never_waits_for_a_commits_change() {
    let (store, pipeline) = pipeline();
    let (_, before) = view(&store);
    let got: Arc<std::sync::Mutex<Option<(u64, usize)>>> = Arc::default();
    let (reader_store, out) = (Arc::clone(&store), Arc::clone(&got));
    seam::set(&store.dl().bounds, Point::Change, move || {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (_, view) = view(&reader_store);
            let _ = tx.send((view.revision, view.facts));
        });
        // The change is held until the read answers, or for 20 s at most.
        *out.lock().unwrap() = rx.recv_timeout(Duration::from_secs(20)).ok();
    });
    update(&pipeline, DELETE);
    assert_eq!(
        *got.lock().unwrap(),
        Some((before.revision, before.facts)),
        "the read of the built revision during the commit's change"
    );
}

/// A commit's change of U1 leaves U1 as it was until the commit happens (`Upper::change`):
/// the literals it reads by value as well, which it notes as it computes the change. A
/// preparation never installed notes nothing; the commit's install keeps them.
#[test]
fn literals_a_change_reads_by_value_are_kept_only_once_its_commit_happens() {
    let store = Arc::new(StoreService::new(StoreConfig::in_memory()).unwrap());
    let pipeline = MutationPipeline::new(
        Arc::clone(&store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    );
    update(
        &pipeline,
        "PREFIX : <urn:test:> PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
         INSERT DATA {
           :A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] .
           :k a owl:DatatypeProperty . :D owl:hasKey ( :k ) .
           :d1 a :D ; :k \"1\"^^xsd:integer .
         }",
    );
    // The key's values are compared (`data_values_are_compared_by_value`); no two are
    // equal, so U1 has no classes and a commit's change is maintained, not rebuilt.
    let before = report(&store);
    assert_eq!(before.unavailable, None, "{before:?}");
    assert_eq!(before.equality_classes, 0, "{before:?}");
    let key_value = |q: &str| {
        nrese_rdf::Quad::new(
            nrese_rdf::NamedNode::new_unchecked(q),
            nrese_rdf::NamedNode::new_unchecked("urn:test:k"),
            nrese_rdf::Literal::new_typed_literal(
                "8",
                nrese_rdf::NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#integer"),
            ),
            nrese_rdf::GraphName::DefaultGraph,
        )
    };
    // Prepared, never installed: the commit didn't happen.
    let mut tx = store.engine().transaction();
    tx.insert(key_value("urn:test:d2").as_ref());
    let prepared = prepare(&store, &tx, nrese_reasoner::eval::NEVER);
    assert!(
        matches!(prepared.prepared, Prepared::Delta(_)),
        "the change's path"
    );
    drop((prepared, tx));
    assert_eq!(report(&store).literals_by_value, before.literals_by_value);
    // Committed: the new value is read by value from now on.
    update(
        &pipeline,
        "PREFIX : <urn:test:> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
         INSERT DATA { :d2 :k \"8\"^^xsd:integer . }",
    );
    let after = report(&store);
    assert_eq!(after.last, "delta", "{after:?}");
    assert_eq!(after.literals_by_value, before.literals_by_value + 1);
}

/// The child's scenario: which work is held, on a global pool of which width.
const CHILD: &str = "NRESE_U1_LOCK_CHILD";

/// Every pool worker calls U1's reads while one thread off the pool holds U1's work (a
/// commit's change, a read's build of U1, of its taxonomy, of its view) at the start of
/// its parallel part: everyone answers, at global pool widths 1 and 2. Each scenario runs
/// in a process of its own, killed at a deadline: a hang can't take the test binary with it.
/// Every row is reported. `taxonomy` at width 1 is a guard: there the taxonomy runs on
/// one worker, serially, so it never needed the pool.
#[test]
fn pool_readers_never_wait_for_u1_work_that_needs_the_pool() {
    let mut failed = Vec::new();
    for point in ["change", "build", "taxonomy", "view"] {
        for width in [1, 2] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "dl::bounds::lock_tests::child",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, format!("{point},{width}"))
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(120);
            let status = loop {
                if let Some(status) = child.try_wait().unwrap() {
                    break Some(status);
                }
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            };
            let output = child.wait_with_output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            // libtest writes `test child ... ` before the test's own output, on its line.
            let answered = stdout
                .lines()
                .find_map(|l| l.find("u1-child answered").map(|at| &l[at..]));
            match (status, answered) {
                (Some(s), Some(line)) if s.success() => {
                    println!("{point} at width {width}: {line}");
                }
                _ => failed.push(format!(
                    "{point} at width {width}: {} — {}",
                    status.map_or("no answer by the deadline".to_owned(), |s| s.to_string()),
                    String::from_utf8_lossy(&output.stderr)
                        .lines()
                        .rev()
                        .take(5)
                        .collect::<Vec<_>>()
                        .join(" | ")
                )),
            }
        }
    }
    assert!(failed.is_empty(), "{failed:#?}");
}

#[test]
#[ignore = "run by pool_readers_never_wait_for_u1_work_that_needs_the_pool, in a process of its own"]
fn child() {
    let Ok(spec) = std::env::var(CHILD) else {
        return;
    };
    let (point, width) = spec.split_once(',').unwrap();
    let width: usize = width.parse().unwrap();
    rayon::ThreadPoolBuilder::new()
        .num_threads(width)
        .build_global()
        .expect("the child's global pool, before any other use of it");
    let point = match point {
        "change" => Point::Change,
        "build" => Point::Build,
        "taxonomy" => Point::Taxonomy,
        "view" => Point::View,
        other => panic!("no point {other}"),
    };
    let (store, pipeline) = match point {
        Point::Change => {
            let (store, pipeline) = pipeline();
            let _ = view(&store);
            (store, Some(pipeline))
        }
        Point::View => (unbuilt(8192), None),
        _ => (unbuilt(32), None),
    };
    // One read per pool worker, released while the work is held.
    let answers: Arc<std::sync::Mutex<Vec<(u64, usize)>>> = Arc::default();
    let started = Arc::new(AtomicUsize::new(0));
    let (readers, out, on) = (
        Arc::clone(&store),
        Arc::clone(&answers),
        Arc::clone(&started),
    );
    seam::set(&store.dl().bounds, point, move || {
        for _ in 0..width {
            let (store, out, on) = (Arc::clone(&readers), Arc::clone(&out), Arc::clone(&on));
            rayon::spawn(move || {
                on.fetch_add(1, Ordering::SeqCst);
                let (_, view) = view(&store);
                let report = report(&store);
                out.lock().unwrap().push((view.revision, view.facts));
                out.lock()
                    .unwrap()
                    .push((report.revision, report.upper_facts));
            });
        }
        let since = Instant::now();
        while on.load(Ordering::SeqCst) < width {
            assert!(
                since.elapsed() < Duration::from_secs(30),
                "the pool never took the reads"
            );
            std::thread::yield_now();
        }
    });
    // The holder, off the pool.
    let holder = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || match pipeline {
            Some(pipeline) => update(&pipeline, DELETE),
            None => {
                let _ = view(&store);
            }
        })
    };
    holder.join().unwrap();
    let since = Instant::now();
    while answers.lock().unwrap().len() < 2 * width {
        assert!(
            since.elapsed() < Duration::from_secs(60),
            "a pool read never answered"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // What they answered is U1 as built from scratch at that revision.
    let (snapshot, now) = view(&store);
    assert_eq!(now.revision, snapshot.revision());
    let afresh = upper_facts(&store, true).expect("U1 afresh");
    assert_eq!(
        canonical(&upper_facts(&store, false).expect("U1 kept")),
        canonical(&afresh)
    );
    assert_eq!(now.facts, afresh.len());
    for &(revision, facts) in answers.lock().unwrap().iter() {
        assert!(
            revision <= now.revision,
            "{revision} after {}",
            now.revision
        );
        if revision == now.revision {
            assert_eq!(facts, now.facts, "a pool read of revision {revision}");
        }
    }
    println!(
        "u1-child answered: {} reads, revision {}, {} facts of U1",
        2 * width,
        now.revision,
        afresh.len()
    );
}
