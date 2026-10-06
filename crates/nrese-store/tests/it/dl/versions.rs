//! U1 is versioned with the snapshot: a reader at revision r sees U1 at r, while commits
//! run, inside a transaction, and after a restart (a replica's: the server's replication
//! tests).

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_sparql::Regime;
use nrese_store::{
    CancellationToken, GraphResultFormat, MutationPipeline, RdfPayload, SparqlQueryRequest,
    StatementOp, StatementsRequest, StoreConfig, StoreService,
};

use super::queries::query;
use super::{PREFIXES, insert, pipeline};

/// `:B` and `:C` are `:D`s: an individual in their union is one through U1 and an exact
/// service only (the TBox's taxonomy has no class for it, so L doesn't).
const SCHEMA: &str = ":B rdfs:subClassOf :D . :C rdfs:subClassOf :D .";

/// `:{name}` in the union of `:B` and `:C`.
fn in_union(name: &str) -> String {
    format!(":{name} a [ owl:unionOf ( :B :C ) ] .")
}

/// The `i…` and `j…` answers of a query's rows.
fn split(rows: &[String]) -> (usize, usize) {
    let i = rows.iter().filter(|r| r.starts_with('i')).count();
    let j = rows.iter().filter(|r| r.starts_with('j')).count();
    (i, j)
}

/// Commits alternate: `:iN` in the union with `:jN a :B` (`:iN` a `:D` through U1 and an
/// exact service only, `:jN` one in L), then `:iN a :B` (every `:D` in L again: the
/// predicate is closed). A query that decided on one revision and read another would see
/// the `j`s of one and the `i`s of the other; at one revision there are as many of both.
#[test]
fn queries_during_commits_read_u1_at_their_snapshots_revision() {
    let dl = pipeline();
    insert(&dl, SCHEMA).expect("schema");
    const COMMITS: usize = 40;
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|scope| {
        let readers: Vec<_> = (0..3)
            .map(|_| {
                scope.spawn(|| {
                    let mut reads = 0;
                    while !done.load(std::sync::atomic::Ordering::Acquire) || reads == 0 {
                        let (rows, status) = query(&dl, "SELECT ?x { ?x a :D }");
                        let (i, j) = split(&rows);
                        assert_eq!(i, j, "one revision: {rows:?} {status:?}");
                        assert!(status.is_complete(), "{:?}", status.reasons());
                        reads += 1;
                    }
                    reads
                })
            })
            .collect();
        for n in 0..COMMITS {
            insert(&dl, &format!("{} :j{n} a :B .", in_union(&format!("i{n}"))))
                .expect("a commit U1 proves consistent");
            insert(&dl, &format!(":i{n} a :B .")).expect("closing commit");
        }
        done.store(true, std::sync::atomic::Ordering::Release);
        for reader in readers {
            assert!(reader.join().expect("reader") > 0);
        }
    });
    let (rows, _) = query(&dl, "SELECT ?x { ?x a :D }");
    assert_eq!(split(&rows), (COMMITS, COMMITS));
    // And the bounds describe the latest revision, U1 maintained as evaluated afresh.
    let store = dl.store();
    assert_eq!(store.dl_bounds().revision, store.current_revision());
    assert_eq!(store.dl_upper_facts(false), store.dl_upper_facts(true));
}

/// Runs `q` inside a transaction with `ops` pending: its rows and status.
fn in_transaction(
    dl: &MutationPipeline,
    ops: Vec<StatementOp>,
    q: &str,
) -> (String, Option<nrese_sparql::Completeness>) {
    let store = dl.store();
    let prepared = store
        .prepare_query(&SparqlQueryRequest::all(format!("{PREFIXES}{q}")))
        .expect("prepared");
    let mut out = Vec::new();
    let mut status = None;
    store
        .run_query_pending_reporting(
            &StatementsRequest::new(ops),
            &prepared,
            &CancellationToken::new(),
            &mut out,
            |s| status = s,
        )
        .expect("query");
    (String::from_utf8(out).expect("utf8"), status)
}

fn ntriples(text: &str) -> RdfPayload {
    RdfPayload {
        payload: text.as_bytes().to_vec(),
        format: GraphResultFormat::NTriples,
        base_iri: None,
    }
}

#[test]
fn a_transaction_reads_u1_at_its_revision_and_says_what_its_pending_operations_do() {
    let dl = pipeline();
    insert(&dl, SCHEMA).expect("schema");
    insert(&dl, &format!("{} :j0 a :B .", in_union("i0"))).expect("data");
    let q = "SELECT ?x { ?x a :D }";
    // Nothing pending: the latest revision, through its bounds, as outside.
    let (text, status) = in_transaction(&dl, Vec::new(), q);
    let status = status.expect("a status");
    assert!(status.complete && status.sound, "{status:?}");
    assert_eq!(status.regime, Some(Regime::Owl2Dl));
    let bounds = status
        .bounds
        .unwrap_or_else(|| panic!("bounds: {status:?} {text}"));
    assert_eq!((bounds.lower, bounds.upper), (1, 2));
    assert!(text.contains("example.com/i0") && text.contains("example.com/j0"));
    // A commit meanwhile: the next read inside sees its revision's U1.
    insert(&dl, &in_union("i1")).expect("data");
    let (text, status) = in_transaction(&dl, Vec::new(), q);
    assert!(text.contains("example.com/i1"), "{text}");
    assert_eq!(status.expect("a status").bounds.expect("bounds").upper, 3);
    // Additions pending: not reasoned over before the commit, sound only.
    let add = StatementOp::Add {
        data: ntriples(
            "<http://example.com/j9> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/B> .",
        ),
        contexts: Vec::new(),
    };
    let (_, status) = in_transaction(&dl, vec![add], q);
    let status = status.expect("a status");
    assert_eq!(status.as_str(), "sound-only", "{status:?}");
    assert!(status.reasons.iter().any(|r| r.source == "dl"));
    // Deletions pending: their inferences remain, so not even sound.
    let remove = StatementOp::RemoveData {
        data: ntriples(
            "<http://example.com/j0> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/B> .",
        ),
        contexts: Vec::new(),
    };
    let (_, status) = in_transaction(&dl, vec![remove], q);
    let status = status.expect("a status");
    assert_eq!(status.as_str(), "unsound", "{status:?}");
    assert!(status.reasons.iter().any(|r| r.source == "transaction"));
}

fn on_disk(dir: &std::path::Path) -> MutationPipeline {
    let store = StoreService::new(StoreConfig::on_disk(dir)).expect("store");
    MutationPipeline::new(
        Arc::new(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Dl,
        ))),
    )
}

#[test]
fn after_a_restart_u1_is_the_one_of_the_revision_it_reopens_at() {
    let dir = tempfile::tempdir().expect("dir");
    let (revision, facts, answers) = {
        let dl = on_disk(dir.path());
        insert(&dl, SCHEMA).expect("schema");
        insert(&dl, &format!("{} :j0 a :B .", in_union("i0"))).expect("data");
        insert(&dl, &in_union("i1")).expect("data");
        let store = dl.store();
        (
            store.current_revision(),
            store.dl_upper_facts(false).expect("U1"),
            query(&dl, "SELECT ?x { ?x a :D }"),
        )
    };
    let dl = on_disk(dir.path());
    let store = dl.store();
    assert_eq!(store.current_revision(), revision);
    assert_eq!(store.dl_upper_facts(false).expect("U1"), facts);
    let (rows, status) = query(&dl, "SELECT ?x { ?x a :D }");
    assert_eq!(rows, answers.0);
    assert_eq!(status.shared, answers.1.shared);
    assert_eq!(store.dl_bounds().revision, revision);
    // Commits after the restart maintain it from there.
    insert(&dl, &in_union("i2")).expect("data");
    assert_eq!(store.dl_upper_facts(false), store.dl_upper_facts(true));
}
