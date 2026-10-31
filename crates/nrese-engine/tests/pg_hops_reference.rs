//! The property-graph hop operations (`examples/pg_hops/hops.rs`) against a slow reference walk
//! over every quad: the same answers by every access path, and the work counters they report
//! are the work done. Over one run and over four runs with deletions (the store under updates).

#[allow(dead_code)]
#[path = "../examples/pg_hops/hops.rs"]
mod hops;

use std::collections::{BTreeMap, BTreeSet};

use hops::{Access, Direction, Reader, Work, pattern};
use nrese_engine::{Engine, EngineConfig, Snapshot, TermId};
use nrese_rdf::{NamedNode, TermRef};
use nrese_rdf_io::{RdfFormat, RdfParser};

const NS: &str = "http://example.org/";
const REIFIES: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
const PEOPLE: u64 = 60;
/// The hub: knows everyone, the largest degree in the graph.
const HUB: u64 = 0;

fn person(n: u64) -> String {
    format!("<{NS}person/{n}>")
}

/// `knows` edges: the hub to everyone, a ring, and chords; every edge but those from a
/// multiple of 7 has a reifier with a date, and `(1, 2)` has two (a multi-edge).
fn graph() -> String {
    let mut edges = BTreeSet::new();
    for n in 1..PEOPLE {
        edges.insert((HUB, n));
        edges.insert((n, n % (PEOPLE - 1) + 1));
        edges.insert((n, (n * 7) % PEOPLE));
    }
    edges.retain(|&(s, o)| s != o);
    let knows = format!("<{NS}knows>");
    let mut text = String::new();
    let mut reifier = 0;
    for (s, o) in edges {
        text += &format!("{} {knows} {} .\n", person(s), person(o));
        let copies = match (s, o) {
            (1, 2) => 2,
            _ if s % 7 == 0 && s != HUB => 0,
            _ => 1,
        };
        for _ in 0..copies {
            reifier += 1;
            let r = format!("<{NS}e/{reifier}>");
            text += &format!(
                "{r} <{REIFIES}> <<( {} {knows} {} )>> .\n",
                person(s),
                person(o)
            );
            text += &format!("{r} <{NS}since> \"{}\" .\n", 2000 + reifier);
        }
    }
    text
}

fn insert(engine: &Engine, text: &str) {
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::NTriples).for_slice(text.as_bytes()) {
        tx.insert(quad.expect("valid N-Triples").as_ref());
    }
    tx.commit().expect("commit");
}

fn id(snapshot: &Snapshot, iri: &str) -> TermId {
    snapshot
        .lookup(TermRef::from(&NamedNode::new_unchecked(iri)))
        .expect("a known term")
}

struct Ids {
    knows: TermId,
    reifies: TermId,
    since: TermId,
    people: Vec<TermId>,
}

fn ids(snapshot: &Snapshot) -> Ids {
    Ids {
        knows: id(snapshot, &format!("{NS}knows")),
        reifies: id(snapshot, REIFIES),
        since: id(snapshot, &format!("{NS}since")),
        people: (0..PEOPLE)
            .map(|n| id(snapshot, &format!("{NS}person/{n}")))
            .collect(),
    }
}

/// The reference: every quad read once, edges and their dates found by decoding each
/// reifier's triple term into its components (not by the hash probe the hop uses).
struct Reference {
    out: BTreeMap<TermId, Vec<TermId>>,
    into: BTreeMap<TermId, Vec<TermId>>,
    /// Per edge `(s, o)`, the dates of its reifiers.
    dates: BTreeMap<(TermId, TermId), Vec<TermId>>,
}

fn reference(snapshot: &Snapshot, ids: &Ids) -> Reference {
    let all: Vec<_> = snapshot
        .quads_for_pattern_in(hops::MODEL, &pattern(None, None, None))
        .collect();
    let mut r = Reference {
        out: BTreeMap::new(),
        into: BTreeMap::new(),
        dates: BTreeMap::new(),
    };
    for q in all.iter().filter(|q| q.predicate == ids.knows) {
        r.out.entry(q.subject).or_default().push(q.object);
        r.into.entry(q.object).or_default().push(q.subject);
    }
    for q in all.iter().filter(|q| q.predicate == ids.reifies) {
        let [s, p, o] = snapshot.components(q.object).expect("a triple term");
        let asserted = r.out.get(&s).is_some_and(|out| out.contains(&o));
        if p != ids.knows || !asserted {
            continue;
        }
        for d in all
            .iter()
            .filter(|d| d.subject == q.subject && d.predicate == ids.since)
        {
            r.dates.entry((s, o)).or_default().push(d.object);
        }
    }
    r
}

fn sorted<T: Ord>(mut v: Vec<T>) -> Vec<T> {
    v.sort_unstable();
    v
}

fn reference_bfs(r: &Reference, start: TermId, hops: usize) -> Vec<u64> {
    let mut seen = BTreeSet::from([start]);
    let mut frontier = vec![start];
    let mut levels = Vec::new();
    for _ in 0..hops {
        let mut next = Vec::new();
        for node in frontier {
            let around = r
                .out
                .get(&node)
                .into_iter()
                .chain(r.into.get(&node))
                .flatten();
            for &other in around {
                if seen.insert(other) {
                    next.push(other);
                }
            }
        }
        levels.push(next.len() as u64);
        frontier = next;
    }
    levels
}

/// Every operation by every access path gives the reference's answer.
fn answers_match(snapshot: &Snapshot, ids: &Ids, r: &Reference, runs: u64) {
    let mut ascending = ids.people.clone();
    ascending.sort_unstable();
    let mut shuffled = ascending.clone();
    shuffled.reverse();
    for access in [Access::Scan, Access::Cursor] {
        for nodes in [&ascending, &shuffled] {
            for (direction, expected) in [(Direction::Out, &r.out), (Direction::In, &r.into)] {
                let mut reader = Reader::new(snapshot, access, runs);
                let mut edges = Vec::new();
                hops::neighbours(&mut reader, nodes, ids.knows, direction, &mut edges);
                let want: Vec<_> = expected
                    .iter()
                    .flat_map(|(&a, bs)| bs.iter().map(move |&b| (a, b)))
                    .map(|(a, b)| {
                        if direction == Direction::Out {
                            (a, b)
                        } else {
                            (b, a)
                        }
                    })
                    .collect();
                assert_eq!(sorted(edges), sorted(want), "{access:?} {direction:?}");
            }
            for direction in [Direction::Out, Direction::In] {
                for batched in [false, true] {
                    let mut reader = Reader::new(snapshot, access, runs);
                    let mut work = Work::default();
                    let dates = hops::edge_property(
                        &mut reader,
                        snapshot,
                        nodes,
                        [ids.knows, ids.reifies, ids.since],
                        direction,
                        batched,
                        &mut work,
                    );
                    let want: Vec<_> = r.dates.values().flatten().copied().collect();
                    assert_eq!(
                        sorted(dates),
                        sorted(want),
                        "{access:?} {direction:?} {batched}"
                    );
                    let edges: usize = r.out.values().map(Vec::len).sum();
                    let reified = r.dates.len() as u64;
                    assert_eq!(work.hash_probes, edges as u64);
                    assert_eq!(work.hash_misses, edges as u64 - reified);
                }
            }
        }
        for sorted_frontier in [false, true] {
            let mut reader = Reader::new(snapshot, access, runs);
            let start = ids.people[5];
            assert_eq!(
                hops::bfs(&mut reader, start, ids.knows, 4, sorted_frontier),
                reference_bfs(r, start, 4),
            );
        }
    }
}

fn engine() -> Engine {
    Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })
    .expect("engine")
}

#[test]
fn one_run_answers_and_counters_match_the_reference_walk() {
    let engine = engine();
    insert(&engine, &graph());
    let snapshot = engine.snapshot();
    let ids = ids(&snapshot);
    assert_eq!(hops::asserted_runs(&snapshot, ids.knows), 1);
    let r = reference(&snapshot, &ids);
    assert_eq!(
        r.dates[&(ids.people[1], ids.people[2])].len(),
        2,
        "the multi-edge keeps both"
    );
    answers_match(&snapshot, &ids, &r, 1);

    let mut ascending = ids.people.clone();
    ascending.sort_unstable();
    let probes = ascending.len() as u64;
    let degree: u64 = r.out.values().map(|v| v.len() as u64).sum();
    let mut scan = Reader::new(&snapshot, Access::Scan, 1);
    hops::neighbours(
        &mut scan,
        &ascending,
        ids.knows,
        Direction::Out,
        &mut Vec::new(),
    );
    assert_eq!((scan.work().probes, scan.work().results), (probes, degree));
    // Ascending probes on one cursor: one search from the root, then fingers.
    let mut cursor = Reader::new(&snapshot, Access::Cursor, 1);
    hops::neighbours(
        &mut cursor,
        &ascending,
        ids.knows,
        Direction::Out,
        &mut Vec::new(),
    );
    assert_eq!(
        (
            cursor.work().seeks,
            cursor.work().root_searches,
            cursor.work().results
        ),
        (probes, 1, degree)
    );
    // Descending probes search from the root each time.
    let mut descending = ascending.clone();
    descending.reverse();
    let mut cursor = Reader::new(&snapshot, Access::Cursor, 1);
    hops::neighbours(
        &mut cursor,
        &descending,
        ids.knows,
        Direction::Out,
        &mut Vec::new(),
    );
    assert_eq!(cursor.work().root_searches, probes);
    // The hub has the largest degree: everyone but itself.
    assert_eq!(r.out[&ids.people[HUB as usize]].len() as u64, PEOPLE - 1);
}

#[test]
fn four_runs_with_deletions_answer_as_the_reference_walk() {
    let engine = engine();
    let base = graph();
    insert(&engine, &base);
    // Three commits, each under a quarter of the run before (so no compaction merges them:
    // 26, 6 and 1 entries over a base of about 470), each adding new edges and deleting the
    // base triple of a reified one; the last deletes the multi-edge's.
    let knows = format!("<{NS}knows>");
    type Edge = (u64, u64);
    let first: Vec<Edge> = (1..=25).map(|n| (n, n + 31)).collect();
    let second: Vec<Edge> = (1..=5).map(|n| (n, n + 32)).collect();
    let commits: [(&[Edge], Edge); 3] = [(&first, (2, 3)), (&second, (4, 5)), (&[], (1, 2))];
    for (adds, (ds, dobj)) in commits {
        let mut tx = engine.transaction();
        for &(s, o) in adds {
            let line = format!("{} {knows} {} .\n", person(s), person(o));
            for quad in RdfParser::from_format(RdfFormat::NTriples).for_slice(line.as_bytes()) {
                tx.insert(quad.expect("valid").as_ref());
            }
        }
        let line = format!("{} {knows} {} .\n", person(ds), person(dobj));
        for quad in RdfParser::from_format(RdfFormat::NTriples).for_slice(line.as_bytes()) {
            assert!(
                tx.remove(quad.expect("valid").as_ref()),
                "the edge to delete exists"
            );
        }
        tx.commit().expect("commit");
    }
    let snapshot = engine.snapshot();
    let ids = ids(&snapshot);
    assert_eq!(hops::asserted_runs(&snapshot, ids.knows), 4);
    let r = reference(&snapshot, &ids);
    assert!(
        !r.dates.contains_key(&(ids.people[1], ids.people[2])),
        "its base triple is gone"
    );
    answers_match(&snapshot, &ids, &r, 4);
}

/// M6's placement weighs a statement by the permutations the store holds: a default-graph
/// store keeps SPOG, POSG, OSPG and PSOG (two placed at the subject, two at the object), one
/// with a named graph seven (SPOG, GSPO, GPSO at the subject; POSG, OSPG, GPOS, GOSP at the
/// object). The keys the tool places are the store's keys.
#[test]
fn placement_weighs_the_permutations_the_store_holds() {
    let engine = engine();
    insert(&engine, &graph());
    let snapshot = engine.snapshot();
    let quads = engine.stats().quads;
    let orders = snapshot.asserted_key_orders();
    assert_eq!(orders.len(), 4, "N-Triples keep the default-graph layout");
    let (subject, object) = hops::anchor_keys(&orders);
    assert_eq!((subject, object), (2, 2));
    assert_eq!(
        quads * (subject + object),
        quads * 4,
        "one key per statement and permutation"
    );

    let named = format!("{} <{NS}knows> {} <{NS}graph> .\n", person(1), person(9));
    let mut tx = engine.transaction();
    for quad in RdfParser::from_format(RdfFormat::NQuads).for_slice(named.as_bytes()) {
        tx.insert(quad.expect("valid N-Quads").as_ref());
    }
    tx.commit().expect("commit");
    let orders = engine.snapshot().asserted_key_orders();
    assert_eq!(
        orders.len(),
        7,
        "a named graph turns the stack into the quad layout"
    );
    assert_eq!(hops::anchor_keys(&orders), (3, 4));
}
