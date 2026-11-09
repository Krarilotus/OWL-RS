//! A support round's derivations under a query's memory limit (SYN-21, B-703): a restricted
//! reader's `inferred = "supported"` read of a complete transitive relation. Its n² facts
//! have n³ rule instances, so the support evaluation's first round finds n³ derivations,
//! nearly all of them the same few sets per fact again.
//!
//! Spec (docs/STATUS.md, "Inferences and graph access"; `nrese_store::support`): the reader
//! sees exactly the closure of the statements it may read; a query's `memory_limit` bounds
//! what the query holds (`SparqlQueryRequest::memory_limit`), and a read is refused only where
//! what it needs passes it. What the reader needs here is about the sets the facts keep (n²
//! times a few), not one entry per rule instance: the read answers as the oracle, within the
//! limit.

use std::collections::BTreeSet;
use std::sync::Arc;

use nrese_reasoner::rulesets::Ruleset;
use nrese_sparql::GraphAccess;
use nrese_store::{BulkLoadRequest, GraphTarget, ReadScope, SparqlQueryRequest, StoreService};

use crate::support::in_memory_store_config;

const EX: &str = "http://example.com/";

/// Nodes of the relation: n³ = 3.4 M instances of 56-byte derivations, about 190 MB, against
/// the n² = 22,500 facts' few sets each.
const NODES: usize = 150;

/// The query's memory limit, measured, not picked (probes `p2c5-probe-base` and
/// `p2c5-probe-head2`, 7 Nov 2026, this read at 16 MiB to 2 GiB): at 150 nodes, with one entry
/// per instance, the read is refused up to 768 MiB and answers at 1 GiB. With each fact's
/// derivations kept once per distinct set as they come, it is refused at 128 MiB and
/// answers at 192 MiB. So the limit is twice the reduced need and half of where the
/// unreduced one is still refused.
const LIMIT: usize = 384 << 20;

/// The graph of the edge `i → j`: one of three.
fn graph(i: usize, j: usize) -> usize {
    (i + 2 * j) % 3
}

/// Every edge `i → j`, `i ≠ j`, in its graph, and `anc` transitive.
fn quads() -> String {
    let mut lines = format!(
        "<{EX}anc> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> \
         <http://www.w3.org/2002/07/owl#TransitiveProperty> .\n"
    );
    for i in 0..NODES {
        for j in (0..NODES).filter(|&j| j != i) {
            lines.push_str(&format!(
                "<{EX}n{i}> <{EX}anc> <{EX}n{j}> <{EX}g{}> .\n",
                graph(i, j)
            ));
        }
    }
    // One edge in `g2` that the other graphs don't give: no reader of `g0` and `g1` sees it.
    lines.push_str(&format!(
        "<{EX}n{HIDDEN}> <{EX}anc> <{EX}n{}> <{EX}g2> .\n",
        HIDDEN + 1
    ));
    lines
}

/// The first node of the edge only `g2` holds, outside the relation's nodes.
const HIDDEN: usize = 900;

/// The graphs whose asserted `ex:anc` statements `scope` reads, under the query memory limit.
fn graphs_read(store: &StoreService, scope: &ReadScope) -> BTreeSet<String> {
    let mut request = SparqlQueryRequest::new(
        format!("SELECT DISTINCT ?g WHERE {{ GRAPH ?g {{ ?s <{EX}anc> ?o }} }}"),
        scope.clone(),
    );
    request.memory_limit = Some(LIMIT);
    let result = store.execute_query(&request).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&result.payload).unwrap();
    json["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["g"]["value"].as_str().unwrap().to_owned())
        .collect()
}

/// The closure of the edges in `graphs`, by Floyd–Warshall: the pairs it holds.
fn oracle(graphs: &[usize]) -> BTreeSet<(usize, usize)> {
    let mut reach = vec![vec![false; NODES]; NODES];
    for (i, row) in reach.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = i != j && graphs.contains(&graph(i, j));
        }
    }
    for k in 0..NODES {
        let via = reach[k].clone();
        for row in reach.iter_mut().filter(|row| row[k]) {
            for (cell, &step) in row.iter_mut().zip(&via) {
                *cell |= step;
            }
        }
    }
    (0..NODES)
        .flat_map(|i| (0..NODES).map(move |j| (i, j)))
        .filter(|&(i, j)| reach[i][j])
        .collect()
}

/// The pairs `ex:anc` relates for `scope`, under the query memory limit.
fn read(store: &StoreService, scope: &ReadScope) -> Result<BTreeSet<(usize, usize)>, String> {
    read_within(store, scope, LIMIT)
}

/// [`read`] under the query memory limit `limit`.
fn read_within(
    store: &StoreService,
    scope: &ReadScope,
    limit: usize,
) -> Result<BTreeSet<(usize, usize)>, String> {
    let mut request = SparqlQueryRequest::new(
        // The default graph (inferred statements, and those asserted only where the reader
        // can't read, which its view adds) and the graphs it may read (their asserted edges).
        format!(
            "SELECT ?s ?o WHERE {{ {{ ?s <{EX}anc> ?o }} UNION {{ GRAPH ?g {{ ?s <{EX}anc> ?o }} }} }}"
        ),
        scope.clone(),
    );
    request.memory_limit = Some(limit);
    let result = store
        .execute_query(&request)
        .map_err(|error| error.to_string())?;
    let json: serde_json::Value = serde_json::from_slice(&result.payload).unwrap();
    let node = |binding: &serde_json::Value| -> usize {
        binding["value"]
            .as_str()
            .unwrap()
            .strip_prefix(&format!("{EX}n"))
            .unwrap()
            .parse()
            .unwrap()
    };
    Ok(json["results"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (node(&row["s"]), node(&row["o"])))
        .collect())
}

/// A reader of two of the three graphs, shown what they support, sees the closure of their
/// edges across the default graph and the graphs it reads, within the query's memory limit.
/// The closure of two thirds of a complete relation's edges is every pair, loops included.
/// The `g2` edges among them reach the reader only as inferences of what it reads (in the
/// default graph, beside the loops no graph asserts), never as `g2`'s assertions, and the
/// edge only `g2` holds not at all. Before SYN-21 the first support round held one
/// derivation per rule instance before its size was checked, and the read was refused past
/// the limit.
#[test]
fn a_restricted_read_of_a_complete_relation_fits_its_memory_limit() {
    let store = StoreService::new(in_memory_store_config()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("data.nq");
    std::fs::write(&file, quads()).unwrap();
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
    let scope = ReadScope::Graphs(Arc::new(GraphAccess {
        graphs: vec![format!("{EX}g0"), format!("{EX}g1")],
        default_graph: true,
        inferred: true,
        inferred_by_support: true,
        ..GraphAccess::default()
    }));
    let expected = oracle(&[0, 1]);
    assert!(
        expected.len() > NODES * (NODES - 1),
        "the closure adds the loops"
    );
    // Refused first, under a limit the read can't fit (probes: it needs 192 MiB): that
    // refusal keeps no view, and the same reader's next read answers (review r1).
    assert!(
        read_within(&store, &scope, 16 << 20).is_err(),
        "refused at 16 MiB"
    );
    assert_eq!(read(&store, &scope), Ok(expected));
    assert_eq!(
        graphs_read(&store, &scope),
        BTreeSet::from([format!("{EX}g0"), format!("{EX}g1")]),
        "the reader sees asserted statements of its own graphs only"
    );
}
