//! What held views and private reads cost (`--held-views 0,1,8`, `--staggered-views 8:100000`;
//! the case `snapshot-private-cost`). Per level N, with N views (`StoreService::read_snapshot`)
//! held at the level's first revision: snapshot acquisition, commits of a declared delta, reads
//! of the current data (the first after the commits apart from warm repeats), private reads in
//! a client transaction (`sessions` and `run_query_pending`, the HTTP transaction's calls) and
//! their rollback, and the live bytes dropping the views frees on this thread (counted at the
//! allocator for this thread alone, the store's background maintenance settled first; memory
//! another thread still holds isn't counted, so a lower bound of what the views retain). The
//! staggered level holds one view before each of its commits, each replacing a large delta,
//! and gives each commit's time and the live bytes each view's drop frees, oldest first: what
//! one frees depends on the views still held beside it.
//!
//! Every timed answer is kept and checked after the timed windows against rows the declared
//! deltas specify, never against another answer: each current read's, each private read's and
//! the read after the rollbacks; the held views, the current data and the staggered views each
//! hold exactly their delta. Commits run as a client's would: SPARQL UPDATE text parsed in the
//! timed window, durable sync per commit, the engine's default background maintenance. Handle
//! lifetime only: no durable named baseline.

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

/// Commits per level, while its views are held.
const COMMITS: usize = 50;
/// Acquisitions timed together (one takes well under a microsecond), and such samples.
const ACQUISITIONS: u32 = 1000;
const SAMPLES: usize = 20;
/// Transactions per level, and the warm reads in each after its first.
const SESSIONS: usize = 20;
const PRIVATE_READS: usize = 5;
const QUERY: &str = "SELECT ?s ?o WHERE { ?s <http://example.org/held/tag> ?o }";

/// A declared delta: `rows` statements `<{subject}{i}> <{predicate}> "{tag}"`, one set per tag;
/// a commit replaces one tag's set with the next's.
struct Delta {
    subject: &'static str,
    predicate: &'static str,
    rows: usize,
}

/// The levels' delta, the one `QUERY` reads.
const SMALL: Delta = Delta {
    subject: "http://example.org/held/s",
    predicate: "http://example.org/held/tag",
    rows: 100,
};

impl Delta {
    fn data(&self, tag: &str) -> String {
        (0..self.rows)
            .map(|i| format!("<{}{i}> <{}> \"{tag}\" . ", self.subject, self.predicate))
            .collect()
    }

    /// Any earlier process's statements out, then tag `tag`'s in.
    fn reset(&self, tag: &str) -> String {
        format!(
            "DELETE WHERE {{ ?s <{}> ?o }} ; INSERT DATA {{ {} }}",
            self.predicate,
            self.data(tag)
        )
    }

    /// The update replacing tag `from`'s statements with tag `to`'s.
    fn replace(&self, from: &str, to: &str) -> String {
        format!(
            "DELETE DATA {{ {} }} ; INSERT DATA {{ {} }}",
            self.data(from),
            self.data(to)
        )
    }

    /// Tag `tag`'s statements as N-Quads lines, sorted.
    fn quads(&self, tag: &str) -> Vec<String> {
        let mut quads: Vec<String> = (0..self.rows)
            .map(|i| {
                Quad::new(
                    NamedNode::new_unchecked(format!("{}{i}", self.subject)),
                    NamedNode::new_unchecked(self.predicate),
                    Literal::new_simple_literal(tag),
                    GraphName::DefaultGraph,
                )
                .to_string()
            })
            .collect();
        quads.sort_unstable();
        quads
    }

    /// Tag `tag`'s statements as `SELECT ?s ?o`'s TSV rows, sorted.
    fn rows(&self, tag: &str) -> Vec<String> {
        let mut rows: Vec<String> = (0..self.rows)
            .map(|i| format!("<{}{i}>\t\"{tag}\"", self.subject))
            .collect();
        rows.sort_unstable();
        rows
    }

    /// The statements with its predicate `snapshot` holds, decoded and sorted.
    fn held_by(&self, snapshot: &Snapshot) -> Vec<String> {
        let Some(predicate) = snapshot.lookup(NamedNodeRef::new_unchecked(self.predicate).into())
        else {
            return Vec::new();
        };
        let pattern = QuadPattern {
            predicate: Some(predicate),
            ..QuadPattern::all()
        };
        let mut quads: Vec<String> = snapshot
            .quads_for_pattern_in(ReadModel::Asserted, &pattern)
            .filter_map(|q| snapshot.decode_quad(q).map(|q| q.to_string()))
            .collect();
        quads.sort_unstable();
        quads
    }
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

fn bytes(freed: Option<i64>) -> String {
    freed.map_or("null".to_owned(), |b| b.to_string())
}

type Commit<'a> = dyn Fn(String) -> Result<Duration, Box<dyn Error>> + 'a;

/// The measurements: `views` (the last level), `view_sweep` (every level), `staggered_views`
/// and `view_checks` (whether every answer was right).
pub fn measure(
    pipeline: &MutationPipeline,
    levels: &[usize],
    staggered: Option<&str>,
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
    let delta_query = PreparedQuery::parse(&crate::tsv_request(QUERY))?;
    let run = || -> Result<(Duration, Vec<u8>), Box<dyn Error>> {
        let mut out = Vec::new();
        let at = Instant::now();
        store.run_query(&delta_query, &CancellationToken::new(), &mut out)?;
        Ok((at.elapsed(), out))
    };
    let mut tag = 0;
    // A fresh process may reopen a store an earlier one changed: its delta goes first.
    commit(SMALL.reset("r0"))?;
    store.settle();
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
        let mut commits = Vec::with_capacity(COMMITS);
        for _ in 0..COMMITS {
            let from = format!("r{tag}");
            tag += 1;
            commits.push(commit(SMALL.replace(&from, &format!("r{tag}")))?);
        }
        let latest = format!("r{tag}");
        // Reads of the current data while the views are held: the first after the commits,
        // then its warm repeats. Every answer kept.
        let current: Vec<(Duration, Vec<u8>)> =
            (0..=runs).map(|_| run()).collect::<Result<_, _>>()?;
        let read_first = current[0].0.as_secs_f64() * 1e3;
        let mut warm: Vec<Duration> = current[1..].iter().map(|(t, _)| *t).collect();
        // Private reads: a transaction replacing the latest delta with a proposed one.
        let sessions = store.sessions();
        let (mut private_first, mut private, mut rollback) = (Vec::new(), Vec::new(), Vec::new());
        let (mut private_answers, mut transactions_ok) = (Vec::new(), true);
        for _ in 0..SESSIONS {
            let id = sessions.begin(None)?;
            let update = SparqlUpdateRequest::new(SMALL.replace(&latest, "proposed"));
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
                private_answers.push(out);
            }
            let at = Instant::now();
            transactions_ok &= sessions.rollback(&id, None);
            rollback.push(at.elapsed());
        }
        let (_, after_rollback) = run()?;
        // The answers, after every timed window.
        let snapshot = store.read_snapshot(None)?;
        let (latest_rows, proposed_rows) = (SMALL.rows(&latest), SMALL.rows("proposed"));
        let base_quads = SMALL.quads(&base);
        let views_ok = views
            .iter()
            .all(|view| view.revision() == r0 && SMALL.held_by(view) == base_quads);
        let current_ok = snapshot.revision() == r0 + COMMITS as u64
            && SMALL.held_by(&snapshot) == SMALL.quads(&latest)
            && current.iter().all(|(_, out)| rows(out) == latest_rows)
            && rows(&after_rollback) == latest_rows;
        let private_ok = transactions_ok
            && private_answers.len() == SESSIONS * (PRIVATE_READS + 1)
            && private_answers.iter().all(|out| rows(out) == proposed_rows);
        let ok = views_ok && current_ok && private_ok;
        all_ok &= ok;
        let revision = snapshot.revision();
        drop(snapshot);
        // What the views retain: the live bytes dropping them frees on this thread.
        store.settle();
        let retained = crate::bytes_freed_by(|| drop(views));
        let (a50, a99, amax) = percentiles(&mut acquire);
        let acquire_ns = format!("{:.1}, {:.1}, {:.1}", a50 * 1e6, a99 * 1e6, amax * 1e6);
        let (commit_p50, commit_p99, commit_max) = percentiles(&mut commits);
        let private_p50 = percentiles(&mut private).0;
        eprintln!(
            "held views {held}: acquire p50 {:.0} ns, commit p50 {commit_p50:.2} ms, private read p50 \
             {private_p50:.3} ms, retained {} live bytes; answers ok: {ok}",
            a50 * 1e6,
            bytes(retained)
        );
        levels_json.push(format!(
            "{{\"held\": {held}, \"base_revision\": {r0}, \"revision\": {revision}, \"delta\": {}, \
             \"acquire_ns_p50_p99_max\": [{acquire_ns}], \"commits\": {COMMITS}, \"commit_p50_ms\": {commit_p50:.4}, \
             \"commit_p99_ms\": {commit_p99:.4}, \"commit_max_ms\": {commit_max:.4}, \
             \"read_first_ms\": {read_first:.4}, \"read_warm_ms_p50_p99_max\": [{}], \
             \"private_first_ms_p50_p99_max\": [{}], \"private_read_p50_ms\": {private_p50:.4}, \
             \"private_read_ms_p50_p99_max\": [{}], \"rollback_ms_p50_p99_max\": [{}], \"sessions\": {SESSIONS}, \
             \"retained_live_bytes\": {}, \"views_ok\": {views_ok}, \"current_ok\": {current_ok}, \
             \"private_ok\": {private_ok}, \"answers_ok\": {ok}}}",
            SMALL.rows,
            ms(&mut warm),
            ms(&mut private_first),
            ms(&mut private),
            ms(&mut rollback),
            bytes(retained)
        ));
    }
    let mut sections = Vec::new();
    if let Some(spec) = staggered {
        let (json, ok) = staggered_views(&store, &commit, spec)?;
        all_ok &= ok;
        sections.push(("staggered_views", json));
    }
    sections.extend([
        ("views", levels_json.last().cloned().unwrap_or_default()),
        ("view_sweep", format!("[{}]", levels_json.join(", "))),
        (
            "view_checks",
            format!(
                "{{\"answers_ok\": {all_ok}, \"levels\": {}}}",
                levels_json.len()
            ),
        ),
    ]);
    Ok(sections)
}

/// `VIEWS:ROWS`: a view held before each of VIEWS commits that replace ROWS statements, so
/// the views read VIEWS revisions with different statements; then each view's drop, oldest
/// first, and the live bytes it frees. Its JSON, and whether its answers were right.
fn staggered_views(
    store: &nrese_store::StoreService,
    commit: &Commit<'_>,
    spec: &str,
) -> Result<(String, bool), Box<dyn Error>> {
    let (count, rows) = spec
        .split_once(':')
        .ok_or(format!("--staggered-views VIEWS:ROWS, not {spec}"))?;
    let (count, rows): (usize, usize) = (count.parse()?, rows.parse()?);
    let big = Delta {
        subject: "http://example.org/held/big/s",
        predicate: "http://example.org/held/big/tag",
        rows,
    };
    commit(big.reset("b0"))?;
    store.settle();
    let (mut views, mut commits) = (Vec::with_capacity(count), Vec::with_capacity(count));
    for k in 0..count {
        views.push(store.read_snapshot(None)?);
        commits.push(commit(
            big.replace(&format!("b{k}"), &format!("b{}", k + 1)),
        )?);
    }
    // The answers, after the timed commits: view k holds tag k's statements, the store the last.
    let current = store.read_snapshot(None)?;
    let ok = views.windows(2).all(|w| w[0].revision() < w[1].revision())
        && views
            .iter()
            .enumerate()
            .all(|(k, view)| big.held_by(view) == big.quads(&format!("b{k}")))
        && big.held_by(&current) == big.quads(&format!("b{count}"));
    let revisions: Vec<String> = views.iter().map(|v| v.revision().to_string()).collect();
    drop(current);
    store.settle();
    let freed: Vec<Option<i64>> = views
        .into_iter()
        .map(|view| crate::bytes_freed_by(|| drop(view)))
        .collect();
    let total = freed.iter().copied().sum::<Option<i64>>();
    let commit_ms: Vec<String> = commits
        .iter()
        .map(|t| format!("{:.3}", t.as_secs_f64() * 1e3))
        .collect();
    let (p50, _, max) = percentiles(&mut commits);
    eprintln!(
        "staggered views {count} x {rows} rows: commit p50 {p50:.1} ms, retained {} live bytes; answers ok: {ok}",
        bytes(total)
    );
    Ok((
        format!(
            "{{\"views\": {count}, \"replaced_rows\": {rows}, \"revisions\": [{}], \"commit_ms\": [{}], \
             \"commit_p50_ms\": {p50:.3}, \"commit_max_ms\": {max:.3}, \"freed_live_bytes\": [{}], \
             \"retained_live_bytes\": {}, \"answers_ok\": {ok}}}",
            revisions.join(", "),
            commit_ms.join(", "),
            freed
                .iter()
                .map(|&b| bytes(b))
                .collect::<Vec<_>>()
                .join(", "),
            bytes(total),
        ),
        ok,
    ))
}
