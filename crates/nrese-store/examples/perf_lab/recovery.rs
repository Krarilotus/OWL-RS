//! Restore and catch-up over one loaded store (the case `recovery-catchup-cost`).
//!
//! - `--recovery-prepare COMMITS:QUADS`, after `--store DIR --load FILE` (the case's setup):
//!   an image backup of the loaded store into `DIR-image` (the server's `backup`), then
//!   COMMITS commits of QUADS new statements each through the mutation pipeline. With
//!   `NRESE_WAL_ARCHIVE=true` their log is retained as the server retains it.
//! - `--recovery COMMITS:QUADS` with `--store DIR --load FILE` (FILE: the source's fixture,
//!   read, not loaded), a fresh process per repetition: the
//!   restore of that image and the retained log up to the middle commit
//!   (`nrese_store::restore_until`, what `nrese-server restore` calls), the restored store's
//!   open and first correct query, an image of the latest store, and a follower's catch-up
//!   from the image through the feed and the apply a replica uses, timed apart. The restore
//!   runs first, before the source opens, so a cold run's restore reads from disk.
//!
//! Checked after the timed windows, against contents made without any store: the fixture as
//! parsed and the commits as generated. The source holds them all, the restored store those up
//! to the chosen revision (and is at it), the follower all of them.
//!
//! What runs: the stores with the store's defaults (background maintenance on, durable sync per
//! commit; a follower publishes each record as one such commit); `restore_until` itself runs
//! without background maintenance. The catch-up is in process, fed in batches of up to 8 MiB,
//! with no network: no replication-lag claim; maintenance the apply leaves running isn't waited
//! for (no settle in the timed window).

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::Instant;

use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_store::{
    CancellationToken, MutationCommand, MutationPipeline, MutationTicket, PreparedQuery, Requester,
    SparqlUpdateRequest, StoreConfig, StoreService,
};

use crate::modes::{BATCH_BYTES, catch_up, contents, digest};

const BASE: &str = "http://example.org/recovery/";

fn spec(text: &str) -> Result<(u64, u64), Box<dyn Error>> {
    let (commits, quads) = text
        .split_once(':')
        .ok_or(format!("COMMITS:QUADS, not {text}"))?;
    Ok((commits.parse()?, quads.parse()?))
}

/// Where the setup's image goes: beside the store, `DIR-image`.
fn image_dir(store: &Path) -> PathBuf {
    let mut name = store.as_os_str().to_owned();
    name.push("-image");
    name.into()
}

/// Statement `i` of commit `c`.
fn quad(c: u64, i: u64) -> Quad {
    Quad::new(
        NamedNode::new_unchecked(format!("{BASE}c{c}s{i}")),
        NamedNode::new_unchecked(format!("{BASE}commit")),
        Literal::new_simple_literal(format!("c{c}")),
        GraphName::DefaultGraph,
    )
}

/// The query for commit `c`'s statements, and its TSV rows, sorted.
fn commit_query(c: u64, quads: u64) -> (String, Vec<String>) {
    let mut rows: Vec<String> = (0..quads).map(|i| format!("<{BASE}c{c}s{i}>")).collect();
    rows.sort_unstable();
    (
        format!("SELECT ?s WHERE {{ ?s <{BASE}commit> \"c{c}\" }}"),
        rows,
    )
}

/// The setup: the image, then the commits.
pub fn prepare(
    pipeline: &MutationPipeline,
    store_dir: &Path,
    text: &str,
) -> Result<String, Box<dyn Error>> {
    let (commits, quads) = spec(text)?;
    let manifest = pipeline.store().backup_image(&image_dir(store_dir))?;
    for c in 0..commits {
        let body: String = (0..quads).map(|i| format!("{} . ", quad(c, i))).collect();
        pipeline
            .apply(
                MutationCommand::Update(SparqlUpdateRequest::new(format!(
                    "INSERT DATA {{ {body} }}"
                ))),
                &Requester::all(),
                &MutationTicket::new(),
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(format!(
        "{{\"image_revision\": {}, \"revision\": {}}}",
        manifest.revision,
        pipeline.store().current_revision()
    ))
}

fn bytes_in(dirs: &[PathBuf]) -> u64 {
    dirs.iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(|entry| entry.ok()?.metadata().ok())
        .map(|meta| meta.len())
        .sum()
}

fn query(store: &StoreService, text: &str) -> Result<(f64, Vec<String>), Box<dyn Error>> {
    let prepared = PreparedQuery::parse(&crate::tsv_request(text))?;
    let mut out = Vec::new();
    let at = Instant::now();
    store.run_query(&prepared, &CancellationToken::new(), &mut out)?;
    let ms = at.elapsed().as_secs_f64() * 1e3;
    let mut rows: Vec<String> = String::from_utf8_lossy(&out)
        .lines()
        .skip(1)
        .map(str::to_owned)
        .collect();
    rows.sort_unstable();
    Ok((ms, rows))
}

/// The digest of the contents after commits `0..upto`: the fixture's statements as parsed
/// and the commits' as generated.
fn expected(fixture: &[String], upto: u64, quads: u64) -> String {
    let mut lines = fixture.to_vec();
    lines.extend((0..upto).flat_map(|c| (0..quads).map(move |i| quad(c, i).to_string())));
    lines.sort_unstable();
    lines.dedup();
    digest(lines)
}

/// One repetition's measurement (`config`: the prepared source's; `fixture`: what it loaded).
pub fn measure(
    config: &StoreConfig,
    fixture: &[PathBuf],
    text: &str,
) -> Result<String, Box<dyn Error>> {
    let (commits, quads) = spec(text)?;
    if fixture.is_empty() {
        return Err("--recovery needs --load FILE, the source's fixture".into());
    }
    let source = config.data_dir.clone();
    let image = image_dir(&source);
    let image_revision = nrese_store::read_manifest(&image)?.revision;
    let cut = image_revision + commits / 2;
    let root = tempfile::tempdir_in(source.parent().unwrap_or(Path::new(".")))?;
    let logs: Vec<PathBuf> = ["wal-archive", "wal"]
        .map(|name| source.join(name))
        .into_iter()
        .filter(|dir| dir.is_dir())
        .collect();
    let wal_bytes = bytes_in(&logs);
    let on = |dir: PathBuf| StoreConfig {
        data_dir: dir,
        ..config.clone()
    };

    // Restore up to the middle commit, the source stopped (a fresh process).
    let restored_dir = root.path().join("restored");
    let phase = crate::Phase::start();
    let at = Instant::now();
    let (_, restored_revision) =
        nrese_store::restore_until(&image, &restored_dir, &logs, Some(cut), None)?;
    let restore_ms = at.elapsed().as_secs_f64() * 1e3;
    let restore_memory = phase.json();
    let at = Instant::now();
    let restored = StoreService::new(on(restored_dir))?;
    let open_ms = at.elapsed().as_secs_f64() * 1e3;
    // The last commit restored, and the first one excluded.
    let (included, included_rows) = commit_query(commits / 2 - 1, quads);
    let (first_query_ms, first_rows) = query(&restored, &included)?;
    let (excluded, _) = commit_query(commits / 2, quads);
    let (_, excluded_rows) = query(&restored, &excluded)?;

    // The source: an image of its latest state, then a follower's catch-up from the
    // setup's image, as a replica starts and follows.
    let at = Instant::now();
    let primary = StoreService::new(config.clone())?;
    let primary_open_ms = at.elapsed().as_secs_f64() * 1e3;
    let at = Instant::now();
    let made = primary.backup_image(&root.path().join("image"))?;
    let image_ms = at.elapsed().as_secs_f64() * 1e3;
    let follower_dir = root.path().join("follower");
    nrese_store::restore_image(&image, &follower_dir)?;
    let follower = StoreService::new(StoreConfig {
        wal_archive: false,
        ..on(follower_dir)
    })?;
    let latest = primary.current_revision();
    let phase = crate::Phase::start();
    let caught = catch_up(
        follower.current_revision(),
        latest,
        |at| primary.replication_log(at, BATCH_BYTES),
        |batch, at| {
            follower
                .apply_replication_log(
                    &batch.frames,
                    &batch.expectation(at),
                    &nrese_exec::stop::StopSource::none(),
                )
                .map(|applied| applied.revision)
        },
    )?;
    let catch_up_memory = phase.json();

    // The answers, after every timed window, against the fixture and the generated commits.
    let mut parsed = Vec::new();
    for file in fixture {
        for quad in RdfParser::from_format(RdfFormat::NTriples).for_slice(&std::fs::read(file)?) {
            parsed.push(quad?.to_string());
        }
    }
    let (all, at_cut) = (
        expected(&parsed, commits, quads),
        expected(&parsed, commits / 2, quads),
    );
    drop(parsed);
    let source = contents(&primary.read_snapshot(None)?)?;
    let statements = source.len();
    let primary_ok = latest == image_revision + commits && digest(source) == all;
    let restored_ok = restored_revision == cut
        && restored.current_revision() == cut
        && digest(contents(&restored.read_snapshot(None)?)?) == at_cut
        && first_rows == included_rows
        && excluded_rows.is_empty();
    let follower_ok = follower.current_revision() == latest
        && digest(contents(&follower.read_snapshot(None)?)?) == all;
    let ok = primary_ok && restored_ok && follower_ok;
    let (feed_ms, apply_ms) = (
        caught.feed.as_secs_f64() * 1e3,
        caught.apply.as_secs_f64() * 1e3,
    );
    eprintln!(
        "recovery {text}: image {image_ms:.0} ms ({} MiB), restore to {cut} {restore_ms:.0} ms, open {open_ms:.0} ms, \
         first query {first_query_ms:.2} ms; catch-up feed {feed_ms:.1} ms, apply {apply_ms:.1} ms; answers ok: {ok}",
        made.bytes / 1048576
    );
    Ok(format!(
        "{{\"commits\": {commits}, \"quads_per_commit\": {quads}, \"statements\": {}, \"image_revision\": {image_revision}, \
         \"cut\": {cut}, \"latest\": {latest}, \"setup_image_bytes\": {}, \"wal_bytes\": {wal_bytes}, \
         \"restore_ms\": {restore_ms:.3}, \"restore_memory\": {restore_memory}, \"open_ms\": {open_ms:.3}, \
         \"first_query_ms\": {first_query_ms:.3}, \"primary_open_ms\": {primary_open_ms:.3}, \
         \"image_ms\": {image_ms:.3}, \"image_bytes\": {}, \"feed_ms\": {feed_ms:.3}, \"apply_ms\": {apply_ms:.3}, \
         \"records\": {}, \"frames_bytes\": {}, \"catch_up_memory\": {catch_up_memory}, \
         \"catch_up_conditions\": \"in process, no network; batches up to {BATCH_BYTES} bytes; store defaults:          durable sync per commit, background maintenance on, not settled in the timed window\",          \"primary_ok\": {primary_ok}, \"restored_ok\": {restored_ok}, \"follower_ok\": {follower_ok}, \"answers_ok\": {ok}}}",
        statements,
        bytes_in(std::slice::from_ref(&image)),
        made.bytes,
        caught.records,
        caught.bytes,
    ))
}
