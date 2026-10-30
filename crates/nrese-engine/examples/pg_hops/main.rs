//! Property-graph storage measurements over LDBC SNB mapped to RDF 1.2
//! (`benches/property-graphs/`): the counts of a load (M0) and the micro-costs of a hop (M1),
//! work counters first, then times. Not an LDBC benchmark result.
//!
//! ```text
//! cargo run --release -p nrese-engine --example pg_hops -- load DATA.nt STORE
//!     [--load-memory-gb N]                                                          > load.json
//! cargo run --release -p nrese-engine --example pg_hops -- m0 STORE [--probe IRI]... > m0.json
//! cargo run --release -p nrese-engine --example pg_hops -- update STORE [--seed S] > update.json
//! cargo run --release -p nrese-engine --example pg_hops -- m1 STORE --runs 1|4
//!     [--seed S] [--reps R] [--bfs-starts N] [--only PHASE]                         > m1.json
//! cargo run --release -p nrese-engine --example pg_hops -- m6 STORE                 > m6.json
//! ```
//!
//! - `m6` (counts only) places every statement under the anchor rule in contiguous ranges, the
//!   anchor rule hashed, and a hash of each term, for 4, 16 and 64 ranges, and counts the
//!   edge-property hops that cross a range and what an in-edge mirror would hold ([`m6`]).
//!
//! - `load` bulk-loads the N-Triples file into a durable store (a checkpoint, one run).
//! - `m0` opens it and counts: quads, runs, index and dictionary bytes, the store on disk,
//!   statements per predicate and per class, reifiers and the triple terms they reify; and,
//!   per `--probe` subject, its statements decoded (the checker compares the extreme ids and
//!   dates with the CSV's).
//! - `update` turns a loaded store (a copy: it changes it) into one under updates: three
//!   commits, each under a quarter of the run before so compaction keeps them apart, left in
//!   the log (a checkpoint would merge them): the deletion of a seeded 1/256 of the
//!   statements, then two re-insertions of parts of them (1/8 and 1/64 of the first).
//! - `m1` opens a store, never writes it, and checks it has `--runs` runs in the asserted
//!   stack (an updated store's log replayed). Then it runs every phase `--reps` times in a
//!   seeded order, each repetition's answer checked against the first's and against the other
//!   access paths'. `--only PHASE` runs that one (or `none`), for `perf stat` against the
//!   `none` baseline.
//!
//! The phases (hops.rs has the operations):
//! - `a.*`: out-neighbours under `knows` and the messages of a person (`?m hasCreator p`),
//!   every person, by scan in random order, by cursor in random and in ascending order;
//! - `b.fwd.*`, `b.bwd.*`: `knows.creationDate` per edge from each person, forward and
//!   backward, as a nested loop of scans, and batched (each step's inputs sorted) on cursors;
//! - `b.step.*`: the edge property hop's steps alone over precomputed inputs: the triple-term
//!   hash probe, the reifier lookup (POSG) and the value lookup (SPOG) by scan and by cursor
//!   (sorted, and in the order found), the date's decoding;
//! - `c.*`: breadth-first search over `knows` (both directions), 1-4 hops from
//!   `--bfs-starts` seeded people, per node by scan (as the path steps read) and per sorted
//!   frontier by cursor.

mod hops;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use hops::{Access, Direction, Reader, Work, pattern};
use nrese_engine::{BulkMode, EncodedQuad, Engine, EngineConfig, Snapshot, TermId};
use nrese_rdf::{NamedNode, Quad, TermRef};
use nrese_rdf_io::{RdfFormat, RdfParser};
use rayon::prelude::*;

const V: &str = "http://ldbcouncil.org/snb/vocab#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
const BATCH: usize = 32 * 1024;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = match args.first().map(String::as_str) {
        Some("load") => load(
            Path::new(&args[1]),
            Path::new(&args[2]),
            option(&args[3..], "--load-memory-gb", 0),
        ),
        Some("m0") => m0(Path::new(&args[1]), &options(&args[2..], "--probe")),
        Some("update") => {
            let engine = Engine::open(&args[1], config()).expect("open the store");
            let mut rng = SplitMix(option(&args[2..], "--seed", 1));
            let work = update(&engine, &mut rng);
            let runs = hops::asserted_runs(
                &engine.snapshot(),
                lookup(&engine.snapshot(), RDF_TYPE).expect("rdf:type"),
            );
            let mut j = Json::new();
            j.work("updates", work).num("runs", runs);
            j.done()
        }
        Some("m1") => m1(&args[1..]),
        Some("m6") => m6(Path::new(&args[1]), &[4, 16, 64]),
        _ => {
            eprintln!(
                "usage: pg_hops load DATA STORE | m0 STORE [--probe IRI]... | update STORE | m1 STORE --runs N ..."
            );
            std::process::exit(2);
        }
    };
    println!("{json}");
}

/// Every value of `flag` in `args`.
fn options(args: &[String], flag: &str) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == flag)
        .map(|w| w[1].clone())
        .collect()
}

fn option<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> T {
    options(args, flag)
        .last()
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("{flag}: not a value: {v}"))
        })
        .unwrap_or(default)
}

fn config() -> EngineConfig {
    // Deterministic: compaction by the policy within the commit, checkpoints only when asked.
    EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    }
}

/// `memory_gb`: the bulk load's memory for quads, past which it sorts in chunks spilled to
/// disk (`DurabilityConfig::bulk_load_memory`); 0, unbounded.
fn load(data: &Path, store: &Path, memory_gb: u64) -> String {
    let mut config = config();
    config.durability.bulk_load_memory = (memory_gb > 0).then_some(memory_gb << 30);
    let engine = Engine::open(store, config).expect("open the store");
    let started = Instant::now();
    let load = engine.bulk_load(BulkMode::Replace);
    let threads = rayon::current_num_threads();
    let chunks = RdfParser::from_format(RdfFormat::NTriples)
        .split_file_for_parallel_parsing(data, threads)
        .expect("split the input");
    let parsed: u64 = chunks
        .into_par_iter()
        .enumerate()
        .map(|(chunk, parser)| {
            let mut batch: Vec<Quad> = Vec::with_capacity(BATCH);
            let (mut parsed, mut number) = (0, 0);
            for quad in parser {
                batch.push(quad.unwrap_or_else(|e| panic!("chunk {chunk}: {e}")));
                if batch.len() == BATCH {
                    load.add_at(chunk as u32, number, &batch);
                    parsed += batch.len() as u64;
                    number += 1;
                    batch.clear();
                }
            }
            load.add_at(chunk as u32, number, &batch);
            parsed + batch.len() as u64
        })
        .sum();
    load.finish().expect("finish the load");
    let seconds = started.elapsed().as_secs_f64();
    let stats = engine.stats();
    let mut j = Json::new();
    j.num("parsed", parsed)
        .num("quads", stats.quads)
        .float("load_s", seconds)
        .num("threads", threads as u64)
        .num("load_memory_gb", memory_gb)
        .num("store_bytes", directory_bytes(store));
    j.done()
}

fn m0(store: &Path, probes: &[String]) -> String {
    let engine = Engine::open(store, config()).expect("open the store");
    let stats = engine.stats();
    let snapshot = engine.snapshot();
    let reifies = lookup(&snapshot, REIFIES);
    let rdf_type = lookup(&snapshot, RDF_TYPE);
    let mut predicates = hashbrown::HashMap::<TermId, u64>::new();
    let mut classes = hashbrown::HashMap::<TermId, u64>::new();
    let mut terms = Vec::new();
    for quad in snapshot.quads_for_pattern_in(hops::MODEL, &pattern(None, None, None)) {
        *predicates.entry(quad.predicate).or_default() += 1;
        if Some(quad.predicate) == rdf_type {
            *classes.entry(quad.object).or_default() += 1;
        }
        if Some(quad.predicate) == reifies {
            terms.push(quad.object);
        }
    }
    let reifiers = terms.len() as u64;
    terms.sort_unstable();
    terms.dedup();
    let name = |id: TermId| {
        snapshot
            .decode(id)
            .map_or_else(|| format!("?{}", id.raw()), |t| t.to_string())
    };
    let mut j = Json::new();
    j.num("quads", stats.quads)
        .num("inferred", stats.inferred)
        .num("runs", stats.runs as u64)
        .num("index_bytes", stats.index_bytes)
        .num("index_mapped_bytes", stats.index_mapped_bytes)
        .num("dictionary_terms", stats.dictionary.terms)
        .num("dictionary_arena_bytes", stats.dictionary.arena_bytes)
        .num("dictionary_index_bytes", stats.dictionary.index_bytes)
        .num("dictionary_mapped_bytes", stats.dictionary.mapped_bytes)
        .num("store_bytes", directory_bytes(store))
        .num("reifier_statements", reifiers)
        .num("reified_triple_terms", terms.len() as u64);
    // The store's files (checkpoint, log), by name: which bytes are the checkpoint's.
    let mut files: Vec<(String, u64)> = std::fs::read_dir(store)
        .expect("read the store's directory")
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| {
            let bytes = e.metadata().map_or(0, |m| m.len());
            (e.file_name().to_string_lossy().into_owned(), bytes)
        })
        .collect();
    files.sort();
    j.map("files", &files);
    let mut sorted: Vec<_> = predicates.iter().map(|(&p, &n)| (name(p), n)).collect();
    sorted.sort();
    j.map("predicates", &sorted);
    let mut sorted: Vec<_> = classes.iter().map(|(&c, &n)| (name(c), n)).collect();
    sorted.sort();
    j.map("classes", &sorted);
    let mut probed = Vec::new();
    for iri in probes {
        let iri = iri.trim_start_matches('<').trim_end_matches('>');
        let statements: Vec<String> = match lookup(&snapshot, iri) {
            None => vec!["absent".to_owned()],
            Some(id) => snapshot
                .quads_for_pattern_in(hops::MODEL, &pattern(Some(id), None, None))
                .map(|q| format!("{} {}", name(q.predicate), name(q.object)))
                .collect(),
        };
        probed.push((format!("<{iri}>"), statements));
    }
    j.lists("probes", &probed);
    j.done()
}

/// The ids the phases need.
struct Ids {
    knows: TermId,
    has_creator: TermId,
    reifies: TermId,
    creation_date: TermId,
    /// Every person, ascending.
    people: Vec<TermId>,
}

type Phase<'p> = (
    &'static str,
    &'static str,
    Box<dyn Fn() -> (Work, u64) + 'p>,
);

fn m1(args: &[String]) -> String {
    let store = PathBuf::from(&args[0]);
    let runs: u64 = option(args, "--runs", 1);
    let seed: u64 = option(args, "--seed", 1);
    let reps: usize = option(args, "--reps", 5);
    let bfs_starts: usize = option(args, "--bfs-starts", 8);
    let only: Option<String> = options(args, "--only").last().cloned();
    let engine = Engine::open(&store, config()).expect("open the store");
    let mut rng = SplitMix(seed);
    let snapshot = engine.snapshot();
    let person = lookup(&snapshot, &format!("{V}Person")).expect("snb:Person");
    let rdf_type = lookup(&snapshot, RDF_TYPE).expect("rdf:type");
    let mut people: Vec<_> = snapshot
        .quads_for_pattern_in(hops::MODEL, &pattern(None, Some(rdf_type), Some(person)))
        .map(|q| q.subject)
        .collect();
    people.sort_unstable();
    let ids = Ids {
        knows: lookup(&snapshot, &format!("{V}knows")).expect("snb:knows"),
        has_creator: lookup(&snapshot, &format!("{V}hasCreator")).expect("snb:hasCreator"),
        reifies: lookup(&snapshot, REIFIES).expect("rdf:reifies"),
        creation_date: lookup(&snapshot, &format!("{V}creationDate")).expect("snb:creationDate"),
        people,
    };
    let measured_runs = hops::asserted_runs(&snapshot, ids.knows);
    assert_eq!(
        measured_runs, runs,
        "the asserted stack has the runs asked for"
    );
    let mut random = ids.people.clone();
    rng.shuffle(&mut random);
    let bfs_from: Vec<TermId> = random.iter().copied().take(bfs_starts).collect();

    // The step phases' inputs, made once.
    let scan = || Reader::new(&snapshot, Access::Scan, runs);
    let mut edges = Vec::new();
    hops::neighbours(&mut scan(), &random, ids.knows, Direction::Out, &mut edges);
    let mut terms = Vec::new();
    hops::triple_terms(
        &snapshot,
        &edges,
        ids.knows,
        &mut Work::default(),
        &mut terms,
    );
    let mut rs = Vec::new();
    hops::reifiers(&mut scan(), &terms, ids.reifies, &mut rs);
    let mut dates = Vec::new();
    hops::values(&mut scan(), &rs, ids.creation_date, &mut dates);
    let mut terms_sorted = terms.clone();
    terms_sorted.sort_unstable();
    terms_sorted.dedup();
    let mut rs_sorted = rs.clone();
    rs_sorted.sort_unstable();
    rs_sorted.dedup();

    let edge_property = |access, nodes: &[TermId], direction, batched| {
        let mut reader = Reader::new(&snapshot, access, runs);
        let mut work = Work::default();
        let out = hops::edge_property(
            &mut reader,
            &snapshot,
            nodes,
            [ids.knows, ids.reifies, ids.creation_date],
            direction,
            batched,
            &mut work,
        );
        work.add(reader.work());
        (work, checksum(&out))
    };
    let neighbours = |access, nodes: &[TermId], predicate, direction| {
        let mut reader = Reader::new(&snapshot, access, runs);
        let mut out = Vec::new();
        hops::neighbours(&mut reader, nodes, predicate, direction, &mut out);
        (reader.work(), out.len() as u64)
    };
    let step = |access, inputs: &[TermId], reifier: bool| {
        let mut reader = Reader::new(&snapshot, access, runs);
        let mut out = Vec::new();
        match reifier {
            true => hops::reifiers(&mut reader, inputs, ids.reifies, &mut out),
            false => hops::values(&mut reader, inputs, ids.creation_date, &mut out),
        }
        (reader.work(), checksum(&out))
    };
    let bfs = |access, sorted| {
        let mut reader = Reader::new(&snapshot, access, runs);
        let mut levels = [0u64; 4];
        for &start in &bfs_from {
            for (l, n) in hops::bfs(&mut reader, start, ids.knows, 4, sorted)
                .into_iter()
                .enumerate()
            {
                levels[l] += n;
            }
        }
        let check = levels
            .iter()
            .fold(0u64, |a, &n| a.wrapping_mul(1_000_003).wrapping_add(n));
        (reader.work(), check)
    };
    let (k, c) = (ids.knows, ids.has_creator);
    let phases: Vec<Phase> = vec![
        (
            "a.out.scan.random",
            "a.out",
            Box::new(|| neighbours(Access::Scan, &random, k, Direction::Out)),
        ),
        (
            "a.out.cursor.random",
            "a.out",
            Box::new(|| neighbours(Access::Cursor, &random, k, Direction::Out)),
        ),
        (
            "a.out.cursor.asc",
            "a.out",
            Box::new(|| neighbours(Access::Cursor, &ids.people, k, Direction::Out)),
        ),
        (
            "a.in.scan.random",
            "a.in",
            Box::new(|| neighbours(Access::Scan, &random, c, Direction::In)),
        ),
        (
            "a.in.cursor.random",
            "a.in",
            Box::new(|| neighbours(Access::Cursor, &random, c, Direction::In)),
        ),
        (
            "a.in.cursor.asc",
            "a.in",
            Box::new(|| neighbours(Access::Cursor, &ids.people, c, Direction::In)),
        ),
        (
            "b.fwd.scan",
            "b.fwd",
            Box::new(|| edge_property(Access::Scan, &random, Direction::Out, false)),
        ),
        (
            "b.fwd.cursor.batched",
            "b.fwd",
            Box::new(|| edge_property(Access::Cursor, &ids.people, Direction::Out, true)),
        ),
        (
            "b.bwd.scan",
            "b.bwd",
            Box::new(|| edge_property(Access::Scan, &random, Direction::In, false)),
        ),
        (
            "b.bwd.cursor.batched",
            "b.bwd",
            Box::new(|| edge_property(Access::Cursor, &ids.people, Direction::In, true)),
        ),
        (
            "b.step.hash",
            "b.step.hash",
            Box::new(|| {
                let mut work = Work::default();
                let mut out = Vec::new();
                hops::triple_terms(&snapshot, &edges, k, &mut work, &mut out);
                (work, checksum(&out))
            }),
        ),
        (
            "b.step.reifier.scan",
            "b.step.reifier",
            Box::new(|| step(Access::Scan, &terms_sorted, true)),
        ),
        (
            "b.step.reifier.cursor.sorted",
            "b.step.reifier",
            Box::new(|| step(Access::Cursor, &terms_sorted, true)),
        ),
        (
            "b.step.reifier.cursor.found",
            "b.step.reifier.found",
            Box::new(|| step(Access::Cursor, &terms, true)),
        ),
        (
            "b.step.value.scan",
            "b.step.value",
            Box::new(|| step(Access::Scan, &rs_sorted, false)),
        ),
        (
            "b.step.value.cursor.sorted",
            "b.step.value",
            Box::new(|| step(Access::Cursor, &rs_sorted, false)),
        ),
        (
            "b.step.value.cursor.found",
            "b.step.value.found",
            Box::new(|| step(Access::Cursor, &rs, false)),
        ),
        (
            "b.step.decode",
            "b.step.decode",
            Box::new(|| {
                let mut work = Work::default();
                let length = hops::decode(&snapshot, &dates, &mut work);
                (work, length)
            }),
        ),
        ("c.bfs.scan", "c", Box::new(|| bfs(Access::Scan, false))),
        (
            "c.bfs.cursor.sorted",
            "c",
            Box::new(|| bfs(Access::Cursor, true)),
        ),
    ];
    let selected: Vec<usize> = (0..phases.len())
        .filter(|&i| only.as_deref().is_none_or(|o| o == phases[i].0))
        .collect();
    if let Some(o) = &only
        && o != "none"
        && selected.is_empty()
    {
        panic!("--only {o}: no such phase");
    }
    // Every repetition of every phase, in a seeded order.
    let mut order: Vec<usize> = selected
        .iter()
        .flat_map(|&i| std::iter::repeat_n(i, reps))
        .collect();
    rng.shuffle(&mut order);
    let mut timings: Vec<Vec<u64>> = vec![Vec::new(); phases.len()];
    let mut results: Vec<Option<(Work, u64)>> = vec![None; phases.len()];
    for &i in &order {
        let started = Instant::now();
        let (work, check) = (phases[i].2)();
        timings[i].push(started.elapsed().as_nanos() as u64);
        match results[i] {
            None => results[i] = Some((work, check)),
            Some(first) => assert_eq!(
                first,
                (work, check),
                "{}: a repetition differs",
                phases[i].0
            ),
        }
    }
    // Answers before times: every access path to one answer gives it.
    let mut answers: Vec<(&str, u64)> = Vec::new();
    for (i, (name, answer, _)) in phases.iter().enumerate() {
        let Some((work, check)) = results[i] else {
            continue;
        };
        let value = if answer.starts_with("a.") {
            work.results
        } else {
            check
        };
        match answers.iter().find(|(a, _)| a == answer) {
            Some(&(_, expected)) => assert_eq!(
                value, expected,
                "{name}: its answer differs from its group's"
            ),
            None => answers.push((answer, value)),
        }
    }
    let mut j = Json::new();
    j.num("runs", measured_runs)
        .num("seed", seed)
        .num("reps", reps as u64)
        .num("people", ids.people.len() as u64)
        .num("bfs_starts", bfs_from.len() as u64)
        .num("knows_edges_from_people", edges.len() as u64)
        .num("reified_knows_edges", terms.len() as u64)
        .num("knows_reifiers", rs.len() as u64)
        .num("knows_dates", dates.len() as u64)
        .num("quads", engine.stats().quads)
        .str("only", only.as_deref().unwrap_or(""));
    let mut rows = Vec::new();
    for (i, (name, answer, _)) in phases.iter().enumerate() {
        let Some((work, check)) = results[i] else {
            continue;
        };
        let mut row = Json::new();
        row.str("phase", name)
            .str("answer", answer)
            .num("check", check)
            .work("work", work)
            .list("ns", &timings[i]);
        rows.push(row.done());
    }
    j.raw("phases", &format!("[{}]", rows.join(",")));
    j.done()
}

/// `update`'s three commits, each under a quarter of the run before (so the compaction
/// policy keeps four runs): the deletion of a seeded 1/256 of the asserted statements, then
/// the re-insertion of an eighth of them, then of a 64th. What they did: `probes` commits,
/// `results` statements deleted.
fn update(engine: &Engine, rng: &mut SplitMix) -> Work {
    let snapshot = engine.snapshot();
    let deleted: Vec<EncodedQuad> = snapshot
        .quads_for_pattern_in(hops::MODEL, &pattern(None, None, None))
        .filter(|_| rng.next().is_multiple_of(256))
        .collect();
    drop(snapshot);
    let mut tx = engine.transaction();
    for &quad in &deleted {
        tx.remove_encoded(quad);
    }
    tx.commit().expect("commit the deletions");
    let eighth = deleted.len() / 8;
    for part in [&deleted[..eighth], &deleted[eighth..eighth + eighth / 8]] {
        let mut tx = engine.transaction();
        for &quad in part {
            tx.insert_encoded(quad);
        }
        tx.commit().expect("commit the re-insertions");
    }
    Work {
        probes: 3,
        results: deleted.len() as u64,
        ..Work::default()
    }
}

/// An edge occurrence: a reifier `r` of the asserted triple `(s p o)` through the triple term `t`.
#[derive(Clone, Copy)]
struct Occurrence {
    r: u64,
    t: u64,
    s: u64,
    p: u64,
    o: u64,
}

/// What M6 counts per placement scheme and range count: the edge-property hops that cross a
/// range, forward (from `s`) and backward (from `o`), the statements an in-edge mirror would
/// hold, and the ranges' balance (largest range's placed keys over the mean).
#[derive(Default, Clone, Copy)]
struct Crossing {
    forward: u64,
    backward: u64,
    mirrored_occurrences: u64,
    mirrored_statements: u64,
}

/// M6, counts only: the placement of every statement under three schemes, for each range
/// count in `ranges`:
/// - `anchor`: the anchor rule (an occurrence and a triple term at their edge's subject, every
///   other term at itself), anchors in contiguous id ranges of equal placed keys;
/// - `anchor-hash`: the anchor rule, anchors placed by a hash;
/// - `term-hash`: every term at a hash of itself (no anchor rule).
///
/// A statement's keys are placed by the leading node component of each permutation of the
/// asserted layout: the subject for SPOG, GSPO and GPSO (3 keys), the object for POSG, OSPG,
/// GPOS and GOSP (4). A forward hop from `s` reads `s`'s out-edge, the triple term, `r rdf:reifies
/// t` and `r`'s properties; it crosses where any of them is placed away from `s`. A backward hop
/// from `o` reads `o`'s in-edge and the same three; it crosses where any is away from `o`, and an
/// in-edge mirror would then hold `r`'s properties at `o`.
fn m6(store: &Path, ranges: &[u64]) -> String {
    let engine = Engine::open(store, config()).expect("open the store");
    let stats = engine.stats();
    let snapshot = engine.snapshot();
    let reifies = lookup(&snapshot, REIFIES).expect("rdf:reifies");
    // The occurrences: reifiers of asserted triples.
    let mut occurrences: Vec<Occurrence> = Vec::new();
    let mut unasserted = 0u64;
    for q in snapshot.quads_for_pattern_in(hops::MODEL, &pattern(None, Some(reifies), None)) {
        let [s, p, o] = snapshot
            .components(q.object)
            .expect("rdf:reifies has a triple term");
        if snapshot.exists_in(hops::MODEL, &pattern(Some(s), Some(p), Some(o))) {
            occurrences.push(Occurrence {
                r: q.subject.raw(),
                t: q.object.raw(),
                s: s.raw(),
                p: p.raw(),
                o: o.raw(),
            });
        } else {
            unasserted += 1;
        }
    }
    occurrences.par_sort_unstable_by_key(|x| x.r);
    let mut terms: Vec<(u64, u64)> = occurrences.iter().map(|x| (x.t, x.s)).collect();
    terms.par_sort_unstable();
    terms.dedup();
    let reifier = |id: u64| occurrences.binary_search_by_key(&id, |x| x.r).ok();
    let anchor = |id: u64| match reifier(id) {
        Some(i) => occurrences[i].s,
        None => terms
            .binary_search_by_key(&id, |x| x.0)
            .map_or(id, |i| terms[i].1),
    };
    let hash = |id: u64, p: u64| {
        let mut z = id.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        (z ^ (z >> 31)) % p
    };
    // Every statement's placed keys at its anchors (and, for the hash per term, at its own
    // subject's and object's hash), and each occurrence's properties.
    let mut subjects = Vec::with_capacity(stats.quads as usize);
    let mut objects = Vec::with_capacity(stats.quads as usize);
    let mut termed: Vec<Vec<u64>> = ranges.iter().map(|&p| vec![0; p as usize]).collect();
    let mut properties = vec![0u32; occurrences.len()];
    for q in snapshot.quads_for_pattern_in(hops::MODEL, &pattern(None, None, None)) {
        let (s, o) = (q.subject.raw(), q.object.raw());
        subjects.push(anchor(s));
        objects.push(anchor(o));
        for (loads, &p) in termed.iter_mut().zip(ranges) {
            loads[hash(s, p) as usize] += 3;
            loads[hash(o, p) as usize] += 4;
        }
        if q.predicate != reifies
            && let Some(i) = reifier(s)
        {
            properties[i] += 1;
        }
    }
    subjects.par_sort_unstable();
    objects.par_sort_unstable();
    // Per distinct anchor, its keys: 3 per statement it leads by subject, 4 by object.
    let mut weights: Vec<(u64, u64)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < subjects.len() || j < objects.len() {
        let next = match (subjects.get(i), objects.get(j)) {
            (Some(&a), Some(&b)) => a.min(b),
            (Some(&a), None) => a,
            (None, Some(&b)) => b,
            (None, None) => unreachable!("the loop ends first"),
        };
        let mut w = 0;
        while subjects.get(i) == Some(&next) {
            w += 3;
            i += 1;
        }
        while objects.get(j) == Some(&next) {
            w += 4;
            j += 1;
        }
        weights.push((next, w));
    }
    drop((subjects, objects));
    let total: u64 = weights.iter().map(|w| w.1).sum();
    // The object's anchor per occurrence (its subject's, the triple term's and the reifier's
    // anchor is `s` by the rule).
    let objects: Vec<u64> = occurrences.par_iter().map(|x| anchor(x.o)).collect();
    let mut rows = Vec::new();
    for (&p, termed) in ranges.iter().zip(&termed) {
        // Contiguous ranges of anchors, each closed once it holds its share of the keys.
        let mut cuts = vec![0u64];
        let mut loads = vec![0u64];
        let mut acc = 0;
        for &(id, w) in &weights {
            if acc >= total * cuts.len() as u64 / p && (cuts.len() as u64) < p {
                cuts.push(id);
                loads.push(0);
            }
            acc += w;
            *loads.last_mut().expect("one range at least") += w;
        }
        let range = |id: u64| (cuts.partition_point(|&c| c <= id) - 1) as u64;
        let mut hashed = vec![0u64; p as usize];
        for &(id, w) in &weights {
            hashed[hash(id, p) as usize] += w;
        }
        let balance =
            |loads: &[u64]| *loads.iter().max().expect("ranges") as f64 / (total as f64 / p as f64);
        let schemes: [(&str, f64); 3] = [
            ("anchor", balance(&loads)),
            ("anchor-hash", balance(&hashed)),
            ("term-hash", balance(termed)),
        ];
        for (k, (name, balanced)) in schemes.into_iter().enumerate() {
            let mut c = Crossing::default();
            let mut by_label: std::collections::BTreeMap<u64, (u64, u64, u64)> = Default::default();
            for ((x, &n), &ao) in occurrences.iter().zip(&properties).zip(&objects) {
                // Where the subject, the object, the triple term and the reifier are placed.
                let [s, o, t, r] = match k {
                    0 => [range(x.s), range(ao), range(x.s), range(x.s)],
                    1 => [hash(x.s, p), hash(ao, p), hash(x.s, p), hash(x.s, p)],
                    _ => [hash(x.s, p), hash(x.o, p), hash(x.t, p), hash(x.r, p)],
                };
                // Forward: s's out-edge at s, then t and r; backward: o's in-edge at o, then t and r.
                let forward = t != s || r != s;
                let backward = t != o || r != o;
                c.forward += u64::from(forward);
                c.backward += u64::from(backward);
                // A mirror is of the anchor rule: under a hash per term the backward hop also
                // reads the triple term at its own hash, which a copy of r's properties at o
                // wouldn't bring there.
                if backward && k < 2 {
                    c.mirrored_occurrences += 1;
                    c.mirrored_statements += u64::from(n);
                }
                let e = by_label.entry(x.p).or_default();
                e.0 += 1;
                e.1 += u64::from(forward);
                e.2 += u64::from(backward);
            }
            let mut row = Json::new();
            row.num("ranges", p)
                .str("scheme", name)
                .num("occurrences", occurrences.len() as u64)
                .num("forward_crossing", c.forward)
                .num("backward_crossing", c.backward)
                .float("largest_range_over_mean", balanced);
            if k < 2 {
                row.num("mirrored_occurrences", c.mirrored_occurrences)
                    .num("mirrored_statements", c.mirrored_statements);
            }
            let labels: Vec<String> = by_label
                .iter()
                .map(|(&label, &(n, f, b))| {
                    let name = snapshot
                        .decode(TermId::from_raw(label))
                        .map_or_else(|| format!("?{label}"), |t| t.to_string());
                    format!("{}:[{n},{f},{b}]", quote(&name))
                })
                .collect();
            row.raw("by_label", &format!("{{{}}}", labels.join(",")));
            rows.push(row.done());
        }
    }
    let mut j = Json::new();
    j.num("quads", stats.quads)
        .num("index_mapped_bytes", stats.index_mapped_bytes)
        .num("store_bytes", directory_bytes(store))
        .num("occurrences", occurrences.len() as u64)
        .num("unasserted_reifiers", unasserted)
        .num(
            "occurrence_properties",
            properties.iter().map(|&n| u64::from(n)).sum(),
        )
        .num("anchors", weights.len() as u64)
        .num("placed_keys", total)
        .raw("placements", &format!("[{}]", rows.join(",")));
    j.done()
}

fn lookup(snapshot: &Snapshot, iri: &str) -> Option<TermId> {
    snapshot.lookup(TermRef::from(&NamedNode::new_unchecked(iri)))
}

/// An order-independent digest of ids and their count.
fn checksum(ids: &[TermId]) -> u64 {
    let sum = ids.iter().fold(0u64, |a, id| {
        a.wrapping_add(id.raw().wrapping_mul(0x9E37_79B9_7F4A_7C15))
    });
    sum ^ (ids.len() as u64).rotate_left(48)
}

fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => directory_bytes(&e.path()),
            _ => e.metadata().map_or(0, |m| m.len()),
        })
        .sum()
}

/// SplitMix64: a seeded, portable sequence (the order and samples are the seed's alone).
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            items.swap(i, (self.next() % (i as u64 + 1)) as usize);
        }
    }
}

/// A JSON object written field by field.
struct Json(String);

impl Json {
    fn new() -> Self {
        Self(String::new())
    }

    fn key(&mut self, key: &str) -> &mut String {
        if !self.0.is_empty() {
            self.0.push(',');
        }
        self.0 += &quote(key);
        self.0.push(':');
        &mut self.0
    }

    fn num(&mut self, key: &str, value: u64) -> &mut Self {
        let _ = write!(self.key(key), "{value}");
        self
    }

    fn float(&mut self, key: &str, value: f64) -> &mut Self {
        let _ = write!(self.key(key), "{value:.3}");
        self
    }

    fn str(&mut self, key: &str, value: &str) -> &mut Self {
        let quoted = quote(value);
        *self.key(key) += &quoted;
        self
    }

    fn raw(&mut self, key: &str, value: &str) -> &mut Self {
        *self.key(key) += value;
        self
    }

    fn list(&mut self, key: &str, values: &[u64]) -> &mut Self {
        let items: Vec<String> = values.iter().map(u64::to_string).collect();
        self.raw(key, &format!("[{}]", items.join(",")))
    }

    fn map(&mut self, key: &str, entries: &[(String, u64)]) -> &mut Self {
        let items: Vec<String> = entries
            .iter()
            .map(|(k, v)| format!("{}:{v}", quote(k)))
            .collect();
        self.raw(key, &format!("{{{}}}", items.join(",")))
    }

    fn lists(&mut self, key: &str, entries: &[(String, Vec<String>)]) -> &mut Self {
        let items: Vec<String> = entries
            .iter()
            .map(|(k, vs)| {
                let vs: Vec<String> = vs.iter().map(|v| quote(v)).collect();
                format!("{}:[{}]", quote(k), vs.join(","))
            })
            .collect();
        self.raw(key, &format!("{{{}}}", items.join(",")))
    }

    fn work(&mut self, key: &str, w: Work) -> &mut Self {
        let mut o = Json::new();
        o.num("probes", w.probes)
            .num("results", w.results)
            .num("seeks", w.seeks)
            .num("root_searches", w.root_searches)
            .num("hash_probes", w.hash_probes)
            .num("hash_misses", w.hash_misses)
            .num("decoded", w.decoded);
        let object = o.done();
        self.raw(key, &object)
    }

    fn done(&self) -> String {
        format!("{{{}}}", self.0)
    }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out += "\\\"",
            '\\' => out += "\\\\",
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
