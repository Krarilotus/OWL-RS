//! Support graph sets timed (`--support-reader FILE`; the fast suite's `support-*` cases, data
//! from `perf_lab generate support-stress`): a reader of the default graph and the named
//! graphs in FILE, shown the inferred statements they support (`inferred = "supported"`),
//! under the cap `--support-sets` (`off`: every set kept).
//!
//! - **The first restricted read:** the sets computed for the revision and the reader's view
//!   built, timed whole and by part (the store's support statistics), with the phase's peak.
//! - **Commits** (`--support-commits FILE`, one SPARQL update per line) through the mutation
//!   pipeline, each followed by a restricted read: the commit's time, then the read's (the
//!   sets updated, or computed afresh, and the view rebuilt from the reader's last one).
//! - **The reader's updates** (`--support-updates FILE`, one SPARQL update request per line,
//!   `u1`, `u2`, …; [`updates`]), after the commits: each first pending in a client
//!   transaction while the reader tallies `ex:D` and its markers (`pending_ms`: its operations
//!   replayed, their `WHERE` clauses on the reader's view, and the tally on the state they
//!   leave), then applied by the reader, who writes its named graphs, through the pipeline
//!   (`update_ms`), then a restricted read and the tally again. Both tallies are reported
//!   (`pending`, `applied`), so the suite checks what the updates did.
//!
//! The queries then run as that reader, so the suite checks its answers.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use nrese_engine::ReadModel;
use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_sparql::GraphAccess;
use nrese_store::support::SupportStatistics;
use nrese_store::{
    CancellationToken, MutationCommand, MutationPipeline, MutationTicket, PreparedQuery, ReadScope,
    Requester, SolutionsResultFormat, SparqlQueryRequest, SparqlUpdateRequest, StatementOp,
    StatementsRequest, StoreResult, StoreService, WriteScope,
};

use crate::Phase;

/// The reader of the default graph and the named graphs listed in `file`, one per line.
pub fn reader(file: &Path) -> Result<Arc<GraphAccess>, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("--support-reader: {e}"))?;
    Ok(Arc::new(GraphAccess {
        graphs: text
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }))
}

fn ms(us: u64) -> f64 {
    us as f64 / 1e3
}

/// One restricted read of the latest revision: its time, the support work it did (the
/// statistics' growth), and the inferred statements it shows.
struct Read {
    read_ms: f64,
    statistics: [u64; 5],
    visible: u64,
}

impl Read {
    fn of(store: &StoreService, access: &Arc<GraphAccess>) -> StoreResult<Self> {
        let before = store.support_statistics();
        let started = Instant::now();
        let visible = store
            .read_snapshot(Some(access))?
            .len_in(ReadModel::Inferred);
        let read_ms = started.elapsed().as_secs_f64() * 1e3;
        let after = store.support_statistics();
        let grew = |f: fn(&SupportStatistics) -> u64| f(&after) - f(&before);
        Ok(Self {
            read_ms,
            statistics: [
                grew(|s| s.computed),
                grew(|s| s.updated),
                grew(|s| s.computing_us),
                grew(|s| s.updating_us),
                grew(|s| s.viewing_us),
            ],
            visible,
        })
    }

    fn json(&self) -> String {
        let [computed, updated, computing, updating, viewing] = self.statistics;
        format!(
            "\"read_ms\": {:.3}, \"computed\": {computed}, \"updated\": {updated}, \
             \"computing_ms\": {:.3}, \"updating_ms\": {:.3}, \"viewing_ms\": {:.3}, \"visible\": {}",
            self.read_ms,
            ms(computing),
            ms(updating),
            ms(viewing),
            self.visible
        )
    }
}

/// Each commit's phase (the commit and the read after it), by its line.
const COMMIT_PHASES: [&str; 4] = [
    "support_commit_1",
    "support_commit_2",
    "support_commit_3",
    "support_commit_4",
];

/// Each of the reader's updates' phase (pending, applied, and the read after it), by its line.
const UPDATE_PHASES: [&str; 4] = [
    "support_update_1",
    "support_update_2",
    "support_update_3",
    "support_update_4",
];

/// The reader's own updates, one request a line, into `own`, the first of its graphs (`perf_lab
/// generate support-stress` writes them to `OUT.updates.ru`): u1 marks every `ex:D` it reads
/// (`marked`); u2 inserts a probe, then marks every `ex:D` its second `WHERE` reads with the
/// probe in place (`seen`), so it reads the state its first operation left. Each `WHERE` reads
/// `ex:D` as the count query does ([`crate::support_family::count`]): each update leaves one
/// marker per member, the count's answer, and none where it reads nothing or out of order.
pub fn updates(own: &str) -> String {
    use crate::support_family::EX;
    let d = format!("{{ ?x a <{EX}D> }} UNION {{ GRAPH ?g {{ ?x a <{EX}D> }} }}");
    format!(
        "INSERT {{ GRAPH <{own}> {{ ?x <{EX}marked> true }} }} WHERE {{ {d} }}\n\
         INSERT DATA {{ GRAPH <{own}> {{ <{EX}probe> <{EX}step> 1 }} }} ; \
         INSERT {{ GRAPH <{own}> {{ ?x <{EX}seen> true }} }} \
         WHERE {{ GRAPH <{own}> {{ <{EX}probe> <{EX}step> 1 }} {d} }}\n"
    )
}

/// One row: the subjects in `ex:D` (`d`, read as the count query reads) and those the
/// reader's markers of each kind name, in any graph it reads ([`updates`]).
fn tally() -> String {
    use crate::support_family::EX;
    format!(
        "SELECT (COUNT(DISTINCT ?s) AS ?d) (COUNT(DISTINCT ?m) AS ?marked) \
         (COUNT(DISTINCT ?n) AS ?seen) WHERE {{ {{ ?s a <{EX}D> }} UNION {{ GRAPH ?g {{ ?s a <{EX}D> }} }} \
         UNION {{ GRAPH ?g {{ ?m <{EX}marked> true }} }} UNION {{ GRAPH ?g {{ ?n <{EX}seen> true }} }} }}"
    )
}

/// The tally's answer (a TSV row of three counts) as the report's object.
fn tallied(answer: &[u8]) -> Result<String, String> {
    let text = String::from_utf8_lossy(answer);
    let counts: Vec<u64> = text
        .lines()
        .nth(1)
        .ok_or_else(|| format!("the tally answered no row: {text:?}"))?
        .split('\t')
        .map(|cell| {
            // `"12"^^<…#integer>` or a plain term: the lexical form.
            let lexical = cell
                .strip_prefix('"')
                .and_then(|c| c.split('"').next())
                .unwrap_or(cell);
            lexical
                .parse()
                .map_err(|_| format!("the tally's {cell:?} isn't a count"))
        })
        .collect::<Result<_, _>>()?;
    match counts[..] {
        [d, marked, seen] => Ok(format!(
            "{{\"d\": {d}, \"marked\": {marked}, \"seen\": {seen}}}"
        )),
        _ => Err(format!("the tally's row isn't three counts: {text:?}")),
    }
}

/// The non-empty lines of `file`, at most `most` (`what` names the option).
fn lines(file: Option<&Path>, most: usize, what: &str) -> Result<Vec<String>, String> {
    let text = match file {
        Some(file) => std::fs::read_to_string(file).map_err(|e| format!("{what}: {e}"))?,
        None => String::new(),
    };
    let lines: Vec<String> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect();
    match lines.len() > most {
        true => Err(format!("{what}: at most {most} lines")),
        false => Ok(lines),
    }
}

/// The report's sections: `support` (every read, commit and update) and `phase_samples` (the
/// phases' times in ms, one series each: the fast suite's samples). Each phase's memory goes
/// to `phases`.
pub fn measure(
    store: &Arc<StoreService>,
    reasoner: &ReasonerConfig,
    access: &Arc<GraphAccess>,
    commits: Option<&Path>,
    updates: Option<&Path>,
    phases: &mut Vec<(&'static str, String)>,
) -> Result<Vec<(&'static str, String)>, Box<dyn std::error::Error>> {
    // The pipeline registers its rules with the store: the sets are computed under them.
    let pipeline = MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(reasoner.clone())),
    );
    let inferred = store.read_snapshot(None)?.len_in(ReadModel::Inferred);
    let phase = Phase::start();
    let first = Read::of(store, access)?;
    phases.push(("support_first_read", phase.json()));
    eprintln!("support: first restricted read {}", first.json());
    let [_, _, computing, _, viewing] = first.statistics;
    let mut samples = vec![
        format!("\"sets\": [{:.3}]", ms(computing)),
        format!("\"view\": [{:.3}]", ms(viewing)),
        format!("\"first_read\": [{:.3}]", first.read_ms),
    ];
    let mut done = Vec::new();
    let commits = lines(commits, COMMIT_PHASES.len(), "--support-commits")?;
    for (k, update) in commits.into_iter().enumerate() {
        let phase = Phase::start();
        let started = Instant::now();
        pipeline.apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update)),
            &Requester::all(),
            &MutationTicket::new(),
        )?;
        let commit_ms = started.elapsed().as_secs_f64() * 1e3;
        let after = Read::of(store, access)?;
        phases.push((COMMIT_PHASES[k], phase.json()));
        eprintln!("support: commit {commit_ms:.1} ms, then {}", after.json());
        samples.push(format!("\"commit_{}\": [{commit_ms:.3}]", k + 1));
        samples.push(format!("\"read_{}\": [{:.3}]", k + 1, after.read_ms));
        done.push(format!(
            "{{\"commit_ms\": {commit_ms:.3}, {}}}",
            after.json()
        ));
    }
    // The reader writes its own named graphs; it tallies `ex:D` as the suite's count query
    // counts it, and its markers.
    let scope = ReadScope::Graphs(Arc::clone(access));
    let writer = Requester::new(
        scope.clone(),
        WriteScope::Graphs(Arc::new(GraphAccess {
            graphs: access.graphs.clone(),
            ..GraphAccess::default()
        })),
    );
    let mut request = SparqlQueryRequest::new(tally(), scope);
    request.solutions_format = SolutionsResultFormat::Tsv;
    let tally = PreparedQuery::parse(&request)?;
    let mut applied = Vec::new();
    let updates = lines(updates, UPDATE_PHASES.len(), "--support-updates")?;
    for (k, update) in updates.into_iter().enumerate() {
        let phase = Phase::start();
        let pending =
            StatementsRequest::new(vec![StatementOp::Update(SparqlUpdateRequest::new(&update))]);
        let mut answer = Vec::new();
        let started = Instant::now();
        store.run_query_pending(&pending, &tally, &CancellationToken::new(), &mut answer)?;
        let pending_ms = started.elapsed().as_secs_f64() * 1e3;
        let on_pending = tallied(&answer)?;
        let started = Instant::now();
        pipeline.apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update)),
            &writer,
            &MutationTicket::new(),
        )?;
        let update_ms = started.elapsed().as_secs_f64() * 1e3;
        let after = Read::of(store, access)?;
        let mut answer = Vec::new();
        store.run_query(&tally, &CancellationToken::new(), &mut answer)?;
        let on_applied = tallied(&answer)?;
        phases.push((UPDATE_PHASES[k], phase.json()));
        eprintln!(
            "support: update u{} pending {pending_ms:.1} ms {on_pending}, applied \
             {update_ms:.1} ms {on_applied}, then {}",
            k + 1,
            after.json()
        );
        samples.push(format!("\"pending_{}\": [{pending_ms:.3}]", k + 1));
        samples.push(format!("\"update_{}\": [{update_ms:.3}]", k + 1));
        applied.push(format!(
            "\"u{}\": {{\"pending_ms\": {pending_ms:.3}, \"update_ms\": {update_ms:.3}, \
             \"pending\": {on_pending}, \"applied\": {on_applied}, {}}}",
            k + 1,
            after.json()
        ));
    }
    let cap = match store.config().support_sets {
        usize::MAX => "\"off\"".to_owned(),
        cap => cap.to_string(),
    };
    let support = format!(
        "{{\"cap\": {cap}, \"reader_graphs\": {}, \"inferred\": {inferred}, \"first\": {{{}}}, \
         \"commits\": [{}], \"updates\": {{{}}}}}",
        access.graphs.len(),
        first.json(),
        done.join(", "),
        applied.join(", ")
    );
    Ok(vec![
        ("support", support),
        ("phase_samples", format!("{{{}}}", samples.join(", "))),
    ])
}
