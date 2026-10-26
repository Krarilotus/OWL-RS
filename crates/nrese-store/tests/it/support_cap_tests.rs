//! The cap on support graph sets (`reasoner.support_sets`) under strain (C-310): a restricted
//! reader's answers with `inferred = "supported"` against an oracle that computes them by
//! brute force from the statements the reader may read (`support_family`), on the inputs of the
//! bench suite `support-stress` as benches/datasets.toml freezes them.
//!
//! Spec (docs/STATUS.md, "Inferences and graph access"; `nrese_store::support`): a reader sees
//! an inferred statement when one of its derivations uses graphs it may read alone, that is,
//! exactly what the closure of the statements it may read holds. Exact here means a cap past
//! every statement's sets (`usize::MAX`); the cap's own contract (config.rs,
//! `StoreConfig::support_sets`) is that it bounds memory and work, not what readers see: a
//! reader none of a statement's kept sets fits, where the cap left some out, is decided by the
//! closure of its graphs. So every cap, the default's included, answers as the oracle.

use std::sync::Arc;

use nrese_reasoner::rulesets::Ruleset;
use nrese_sparql::GraphAccess;
use nrese_store::{
    BulkLoadRequest, GraphTarget, ReadScope, SparqlQueryRequest, StoreConfig, StoreService,
};

use crate::support::in_memory_store_config;
use crate::support_family::{EX, Family, Quad, Shape, ask, count, members_of_d};

/// Every set kept.
const EXACT: usize = usize::MAX;

/// The bench suite's inputs (benches/datasets.toml: each `made_by`, the generate command) with
/// at most 256 sets per target (8, 16, 17, 64 and 256; the default cap is 16), one subject
/// each: every subject's target has the same sets, and the bench times the full sizes.
fn catalogued() -> Vec<Family> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../benches/datasets.toml");
    let mut files = std::collections::BTreeSet::new();
    let mut families = Vec::new();
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let Some(command) = line
            .strip_prefix("made_by = \"perf_lab generate support-stress ")
            .and_then(|command| command.strip_suffix('"'))
        else {
            continue;
        };
        let mut words = command.split_whitespace();
        if !files.insert(words.next().unwrap().to_owned()) {
            continue;
        }
        let mut family = Family::from_params(words.filter_map(|w| w.split_once('='))).unwrap();
        family.subjects = 1;
        if family.shape.sets() <= 256 {
            families.push(family);
        }
    }
    assert!(
        families.iter().any(|family| family.shape.sets() > 16),
        "the suite's inputs past the default cap"
    );
    families
}

/// A store of `quads` with `config`, bulk-loaded, its OWL 2 RL closure computed.
fn store(quads: &[Quad], config: StoreConfig) -> StoreService {
    let store = StoreService::new(config).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.nq");
    let lines: String = quads.iter().map(|q| q.nquad() + "\n").collect();
    std::fs::write(&file, lines).unwrap();
    store
        .bulk_load(&BulkLoadRequest {
            files: vec![file],
            replace: false,
            graph: GraphTarget::DefaultGraph,
            invalid: None,
        })
        .unwrap();
    store.rematerialise(Ruleset::Owl2Rl).unwrap();
    store.use_reasoning_rules(Some(Ruleset::Owl2Rl.into()));
    store
}

fn capped(cap: usize) -> StoreConfig {
    StoreConfig {
        support_sets: cap,
        ..in_memory_store_config()
    }
}

/// A reader of the default graph and `graphs`, shown the inferred statements they support.
fn reader(graphs: &[String]) -> ReadScope {
    ReadScope::Graphs(Arc::new(GraphAccess {
        graphs: graphs.to_vec(),
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }))
}

/// What `scope` answers: whether `probe` is in `ex:D`, and how many subjects are.
fn answers(store: &StoreService, scope: &ReadScope, probe: &str) -> (bool, u64) {
    let run = |query: String| -> serde_json::Value {
        let result = store
            .execute_query(&SparqlQueryRequest::new(query, scope.clone()))
            .unwrap();
        serde_json::from_slice(&result.payload).unwrap()
    };
    let asked = run(ask(probe))["boolean"].as_bool().unwrap();
    let counted = run(count())["results"]["bindings"][0]["n"]["value"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (asked, counted)
}

/// What the oracle answers for a reader of the default graph and `graphs`.
fn oracle(quads: &[Quad], graphs: &[String], probe: &str) -> (bool, u64) {
    let members = members_of_d(quads, graphs);
    (members.contains(probe), members.len() as u64)
}

/// The readers checked: the graphs of up to 64 of a target's sets, spread over them; each of
/// those but its first graph (no set of its own left); the bench's reader; every graph.
fn readers(family: &Family) -> Vec<Vec<String>> {
    let sets = family.shape.sets();
    let picked = sets.min(64);
    let mut readers = Vec::new();
    for k in 0..picked {
        let support = family.support(k * sets / picked);
        readers.push(support[1..].to_vec());
        readers.push(support);
    }
    readers.push(family.reader());
    readers.push(family.graphs.clone());
    readers
}

/// Where a store of `family` with `config` answers otherwise than the oracle, one line each;
/// first, the oracle against the store's unrestricted answers (every graph read).
fn mismatches(family: &Family, config: StoreConfig) -> Vec<String> {
    let quads = family.quads();
    let store = store(&quads, config);
    let probe = format!("{EX}x0");
    let mut out = Vec::new();
    let unrestricted = answers(&store, &ReadScope::All, &probe);
    if unrestricted != oracle(&quads, &family.graphs, &probe) {
        out.push(format!(
            "the oracle misses the closure: store {unrestricted:?}"
        ));
    }
    for graphs in readers(family) {
        let (got, want) = (
            answers(&store, &reader(&graphs), &probe),
            oracle(&quads, &graphs, &probe),
        );
        if got != want {
            out.push(format!("{graphs:?}: store {got:?}, oracle {want:?}"));
        }
    }
    out
}

#[test]
fn exact_reads_equal_the_oracle() {
    for family in catalogued() {
        let wrong = mismatches(&family, capped(EXACT));
        assert!(wrong.is_empty(), "{:?}: {wrong:#?}", family.shape);
    }
}

/// The default configuration (cap 16) answers as the oracle on the suite's inputs: a reader
/// whose graphs hold only a set the cap left out (the 17 sets of `copies 17`, and more) is
/// decided by the closure of its graphs.
#[test]
fn default_reads_equal_the_oracle() {
    let wrong: Vec<String> = catalogued()
        .iter()
        .flat_map(|family| {
            mismatches(family, in_memory_store_config())
                .into_iter()
                .map(move |line| format!("{:?} {line}", family.shape))
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Spec: config.rs (`StoreConfig::support_sets`): the cap bounds memory and work, not what
/// readers see; a reader none of a statement's kept sets fits, where the cap left some out,
/// is decided by the closure of its graphs. A target derivable from each of k graphs alone
/// has k sets: at caps 1, 4, 16 and off (every set kept), with k at the cap and one past it,
/// every reader answers as the oracle, the k readers of one graph each among them.
#[test]
fn readers_equal_the_oracle_at_every_cap() {
    for (cap, k) in [
        (1, 1),
        (1, 2),
        (4, 4),
        (4, 5),
        (16, 16),
        (16, 17),
        (EXACT, 17),
    ] {
        let wrong = mismatches(&Family::new(Shape::Copies(k), 1, 7), capped(cap));
        assert!(wrong.is_empty(), "cap {cap}, {k} sets: {wrong:#?}");
    }
}

/// C-310's witness: `ex:x a ex:D` follows from two graphs the reader may read (`ex:x a ex:C`
/// in one, the schema in the other); 16 graphs it may not read each hold `ex:yi owl:sameAs
/// ex:x` and `ex:yi a ex:D`, a support of one graph, smaller than the reader's. The reader's
/// answers without the hidden graphs, then with them.
fn hidden_supports(config: impl Fn() -> StoreConfig) -> [(bool, u64); 2] {
    let readable = [format!("{EX}public-1"), format!("{EX}public-2")];
    let public = vec![
        Quad::typed("x", "C", Some(readable[0].as_str())),
        Quad::sub_class("C", "D", Some(readable[1].as_str())),
    ];
    let mut with_hidden = public.clone();
    for i in 0..16 {
        let (y, graph) = (format!("y{i}"), format!("{EX}secret-{i:02}"));
        with_hidden.push(Quad::same_as(&y, "x", Some(graph.as_str())));
        with_hidden.push(Quad::typed(&y, "D", Some(graph.as_str())));
    }
    let probe = format!("{EX}x");
    // The hidden graphs change nothing the reader may read.
    assert_eq!(
        oracle(&public, &readable, &probe),
        oracle(&with_hidden, &readable, &probe)
    );
    assert_eq!(oracle(&public, &readable, &probe), (true, 1));
    [public, with_hidden].map(|quads| answers(&store(&quads, config()), &reader(&readable), &probe))
}

#[test]
fn exact_reads_ignore_hidden_supports() {
    assert_eq!(hidden_supports(|| capped(EXACT)), [(true, 1); 2]);
}

/// The default configuration: the hidden supports push the reader's set out of the 16 kept,
/// and its answers stay what they are without them.
#[test]
fn default_reads_ignore_hidden_supports() {
    assert_eq!(hidden_supports(in_memory_store_config), [(true, 1); 2]);
}
