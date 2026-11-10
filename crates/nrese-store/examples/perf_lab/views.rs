//! What held views and private reads cost (`--held-views 0,1,8`; the case
//! `snapshot-private-cost`). Per level N, with N views (`StoreService::read_snapshot`) held
//! at the level's first revision: snapshot acquisition, commits of a declared delta, reads of
//! the current data (the first after the commits apart from warm repeats), private reads in a
//! client transaction (`sessions` and `run_query_pending`, the HTTP transaction's calls) and
//! their rollback, and the resident memory the held views retain. Every answer is checked
//! after the timed windows: the held views, the current data and the private reads each hold
//! exactly their delta. Handle lifetime only: no durable named baseline.

use std::error::Error;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nrese_engine::{QuadPattern, ReadModel, Snapshot};
use nrese_rdf::{GraphName, Literal, NamedNode, NamedNodeRef, Quad};
use nrese_store::{
    CancellationToken, MutationCommand, MutationPipeline, MutationTicket, PreparedQuery, Requester,
    SparqlUpdateRequest, StatementOp, StatementsRequest,
};

use crate::modes::percentiles;

const SUBJECT: &str = "http://example.org/held/s";
const TAG: &str = "http://example.org/held/tag";
/// The declared delta: the statements a commit replaces (its tag's for the next tag's).
const DELTA: usize = 100;
/// Commits per level, while its views are held.
const COMMITS: usize = 50;
/// Acquisitions timed together (one takes well under a microsecond), and such samples.
const ACQUISITIONS: u32 = 1000;
const SAMPLES: usize = 20;
/// Transactions per level, and the warm reads in each after its first.
const SESSIONS: usize = 20;
const PRIVATE_READS: usize = 5;
const QUERY: &str = "SELECT ?s ?o WHERE { ?s <http://example.org/held/tag> ?o }";

fn quad(i: usize, tag: &str) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("{SUBJECT}{i}")),
        NamedNode::new_unchecked(TAG),
        Literal::new_simple_literal(tag),
        GraphName::DefaultGraph,
    )
}

fn data(tag: &str) -> String {
    (0..DELTA)
        .map(|i| format!("<{SUBJECT}{i}> <{TAG}> \"{tag}\" . "))
        .collect()
}

/// The update replacing tag `from`'s delta with tag `to`'s.
fn replace(from: &str, to: &str) -> String {
    format!(
        "DELETE DATA {{ {} }} ; INSERT DATA {{ {} }}",
        data(from),
        data(to)
    )
}

/// The delta statements `snapshot` holds, decoded and sorted.
fn delta_in(snapshot: &Snapshot) -> Vec<String> {
    let Some(tag) = snapshot.lookup(NamedNodeRef::new_unchecked(TAG).into()) else {
        return Vec::new();
    };
    let pattern = QuadPattern {
        predicate: Some(tag),
        ..QuadPattern::all()
    };
    let mut rows: Vec<String> = snapshot
        .quads_for_pattern_in(ReadModel::Asserted, &pattern)
        .filter_map(|q| snapshot.decode_quad(q).map(|q| q.to_string()))
        .collect();
    rows.sort_unstable();
    rows
}

/// Tag `tag`'s delta, as `delta_in` lists it and as `QUERY`'s TSV rows.
fn expected(tag: &str) -> (Vec<String>, Vec<String>) {
    let mut quads: Vec<String> = (0..DELTA).map(|i| quad(i, tag).to_string()).collect();
    let mut rows: Vec<String> = (0..DELTA)
        .map(|i| format!("<{SUBJECT}{i}>\t\"{tag}\""))
        .collect();
    quads.sort_unstable();
    rows.sort_unstable();
    (quads, rows)
}

/// A TSV answer's rows, sorted, without its header.
fn rows(tsv: &[u8]) -> Vec<String> {
    let mut rows: Vec<String> = String::from_utf8_lossy(tsv)
        .lines()
        .skip(1)
        .map(str::to_owned)
        .collect();
    rows.sort_unstable();
    rows
}

fn ms(times: &mut [Duration]) -> String {
    let (p50, p99, max) = percentiles(times);
    format!("{p50:.4}, {p99:.4}, {max:.4}")
}

fn resident_mib() -> f64 {
    nrese_exec::memory::resident_bytes().map_or(-1.0, |bytes| bytes as f64 / 1048576.0)
}

/// The levels' measurements: `views` (the last level), `view_sweep` (every level) and
/// `view_checks` (whether every level's answers were right).
pub fn measure(
    pipeline: &MutationPipeline,
    queries: &[String],
    levels: &[usize],
    runs: usize,
) -> Result<Vec<(&'static str, String)>, Box<dyn Error>> {
    let store = Arc::clone(pipeline.store());
    let commit = |update: String| -> Result<Duration, Box<dyn Error>> {
        let at = Instant::now();
        pipeline
            .apply(
                MutationCommand::Update(SparqlUpdateRequest::new(update)),
                &Requester::all(),
                &MutationTicket::new(),
            )
            .map_err(|e| e.to_string())?;
        Ok(at.elapsed())
    };
    let run = |prepared: &PreparedQuery| -> Result<(Duration, Vec<u8>), Box<dyn Error>> {
        let mut out = Vec::new();
        let at = Instant::now();
        store.run_query(prepared, &CancellationToken::new(), &mut out)?;
        Ok((at.elapsed(), out))
    };
    let lookups: Vec<PreparedQuery> = queries
        .iter()
        .map(|q| PreparedQuery::parse(&crate::request(q)))
        .collect::<Result<_, _>>()?;
    let delta_query = PreparedQuery::parse(&crate::request(QUERY))?;
    let mut tag = 0;
    // A fresh process may reopen a store an earlier one changed: its delta goes first.
    commit(format!(
        "DELETE WHERE {{ ?s <{TAG}> ?o }} ; INSERT DATA {{ {} }}",
        data("r0")
    ))?;
    let (mut levels_json, mut all_ok) = (Vec::new(), true);
    for &held in levels {
        let base = format!("r{tag}");
        let r0 = store.current_revision();
        let views: Vec<Snapshot> = (0..held)
            .map(|_| store.read_snapshot(None))
            .collect::<Result<_, _>>()?;
        let mut acquire: Vec<Duration> = (0..SAMPLES)
            .map(|_| {
                let at = Instant::now();
                for _ in 0..ACQUISITIONS {
                    drop(std::hint::black_box(store.read_snapshot(None)?));
                }
                Ok(at.elapsed() / ACQUISITIONS)
            })
            .collect::<Result<_, nrese_store::StoreError>>()?;
        let resident_before = resident_mib();
        let mut commits = Vec::with_capacity(COMMITS);
        for _ in 0..COMMITS {
            let from = format!("r{tag}");
            tag += 1;
            commits.push(commit(replace(&from, &format!("r{tag}")))?);
        }
        let latest = format!("r{tag}");
        // Reads of the current data while the views are held: each query's first execution
        // after the commits apart from its warm repeats.
        let (mut first, mut warm) = (Vec::new(), Vec::new());
        for prepared in lookups.iter().chain([&delta_query]) {
            first.push(run(prepared)?.0);
            for _ in 0..runs {
                warm.push(run(prepared)?.0);
            }
        }
        let (_, committed_answer) = run(&delta_query)?;
        let mut committed: Vec<Duration> = (0..runs.max(1))
            .map(|_| run(&delta_query).map(|(t, _)| t))
            .collect::<Result<_, _>>()?;
        // Private reads: a transaction replacing the latest delta with a proposed one.
        let sessions = store.sessions();
        let (mut private_first, mut private, mut rollback) = (Vec::new(), Vec::new(), Vec::new());
        let (mut private_answer, mut transactions_ok) = (Vec::new(), true);
        for _ in 0..SESSIONS {
            let id = sessions.begin(None)?;
            let update = SparqlUpdateRequest::new(replace(&latest, "proposed"));
            let added = sessions
                .add(&id, None, vec![StatementOp::Update(update)])
                .map_err(|e| format!("{e:?}"))?;
            transactions_ok &= added;
            for read in 0..=PRIVATE_READS {
                let mut out = Vec::new();
                let at = Instant::now();
                let ops = sessions
                    .pending(&id, None)
                    .ok_or("the transaction is gone")?;
                let pending = StatementsRequest {
                    session: Some(id.clone()),
                    ..StatementsRequest::new(ops)
                };
                store.run_query_pending(
                    &pending,
                    &delta_query,
                    &CancellationToken::new(),
                    &mut out,
                )?;
                match read {
                    0 => private_first.push(at.elapsed()),
                    _ => private.push(at.elapsed()),
                }
                private_answer = out;
            }
            let at = Instant::now();
            transactions_ok &= sessions.rollback(&id, None);
            rollback.push(at.elapsed());
        }
        let (_, after_rollback) = run(&delta_query)?;
        let resident_held = resident_mib();
        // The answers, after every timed window.
        let current = store.read_snapshot(None)?;
        let (base_quads, _) = expected(&base);
        let (latest_quads, latest_rows) = expected(&latest);
        let (_, proposed_rows) = expected("proposed");
        let views_ok = views
            .iter()
            .all(|view| view.revision() == r0 && delta_in(view) == base_quads);
        let current_ok =
            current.revision() == r0 + COMMITS as u64 && delta_in(&current) == latest_quads;
        let private_ok = transactions_ok
            && rows(&private_answer) == proposed_rows
            && rows(&committed_answer) == latest_rows
            && rows(&after_rollback) == latest_rows;
        let ok = views_ok && current_ok && private_ok;
        all_ok &= ok;
        let revision = current.revision();
        drop((current, views));
        nrese_engine::memory::release_all();
        let resident_released = resident_mib();
        let (a50, a99, amax) = percentiles(&mut acquire);
        let acquire_ns = format!("{:.1}, {:.1}, {:.1}", a50 * 1e6, a99 * 1e6, amax * 1e6);
        let (commit_p50, commit_p99, commit_max) = percentiles(&mut commits);
        eprintln!(
            "held views {held}: acquire p50 {:.0} ns, commit p50 {commit_p50:.2} ms, private read p50 {:.3} ms, \
             retained {:.1} MiB; answers ok: {ok}",
            a50 * 1e6,
            percentiles(&mut private).0,
            resident_held - resident_released
        );
        levels_json.push(format!(
            "{{\"held\": {held}, \"base_revision\": {r0}, \"revision\": {revision}, \"delta\": {DELTA}, \
             \"acquire_ns_p50_p99_max\": [{}], \"commits\": {COMMITS}, \"commit_p50_ms\": {commit_p50:.4}, \
             \"commit_p99_ms\": {commit_p99:.4}, \"commit_max_ms\": {commit_max:.4}, \
             \"read_first_ms_p50_p99_max\": [{}], \"read_warm_ms_p50_p99_max\": [{}], \
             \"committed_read_ms_p50_p99_max\": [{}], \"private_first_ms_p50_p99_max\": [{}], \
             \"private_read_p50_ms\": {:.4}, \"private_read_ms_p50_p99_max\": [{}], \
             \"rollback_ms_p50_p99_max\": [{}], \"sessions\": {SESSIONS}, \
             \"resident_before_commits_mib\": {resident_before:.1}, \"resident_held_mib\": {resident_held:.1}, \
             \"resident_released_mib\": {resident_released:.1}, \"retained_mib\": {:.1}, \
             \"views_ok\": {views_ok}, \"current_ok\": {current_ok}, \"private_ok\": {private_ok}, \"answers_ok\": {ok}}}",
            acquire_ns,
            ms(&mut first),
            ms(&mut warm),
            ms(&mut committed),
            ms(&mut private_first),
            percentiles(&mut private).0,
            ms(&mut private),
            ms(&mut rollback),
            resident_held - resident_released
        ));
    }
    Ok(vec![
        ("views", levels_json.last().cloned().unwrap_or_default()),
        ("view_sweep", format!("[{}]", levels_json.join(", "))),
        (
            "view_checks",
            format!(
                "{{\"answers_ok\": {all_ok}, \"levels\": {}}}",
                levels_json.len()
            ),
        ),
    ])
}
