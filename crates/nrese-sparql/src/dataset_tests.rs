//! Whether the dataset a request reads by leaves out the store's default graph
//! ([`super::omits_store_default_graph`]): the protocol's dataset over the query's own, and the
//! default graph named by its own name or by another store's.

use nrese_engine::{Engine, EngineConfig, TermId};
use nrese_rdf::{GraphName, NamedNode, Quad};
use nrese_sparql_syntax::algebra::QueryDataset;

use super::omits_store_default_graph as omits;
use super::{DefaultGraph, GraphSet, ResolvedDataset};
use crate::compat::JENA_DEFAULT_GRAPH;
use crate::results::QueryDatasetSpecification;

const G2: &str = "urn:ex:g2";

fn iri(name: &str) -> NamedNode {
    NamedNode::new_unchecked(name)
}

/// A query's own dataset: `FROM` each of `default`, `FROM NAMED` each of `named`.
fn from(default: &[&str], named: Option<&[&str]>) -> QueryDataset {
    QueryDataset {
        default: default.iter().map(|name| iri(name)).collect(),
        named: named.map(|named| named.iter().map(|name| iri(name)).collect()),
    }
}

/// A protocol dataset whose default graph is the merge of `default`.
fn protocol(default: Vec<GraphName>) -> QueryDatasetSpecification {
    let mut specification = QueryDatasetSpecification::new();
    specification.set_default_graph(default);
    specification
}

/// Spec: a request that names no dataset reads the store's, its default graph included.
#[test]
fn no_dataset_keeps_the_default_graph() {
    assert!(!omits(None, None));
}

/// Spec: without a protocol dataset the query's own decides: `FROM` named graphs alone, or
/// `FROM NAMED` alone (an empty default graph), leave the store's default graph out; naming it
/// by another store's name for it keeps it.
#[test]
fn the_querys_dataset_decides_without_a_protocol_one() {
    assert!(omits(None, Some(&from(&[G2], None))));
    assert!(omits(None, Some(&from(&[], Some(&[G2])))));
    assert!(!omits(None, Some(&from(&[G2, JENA_DEFAULT_GRAPH], None))));
}

/// Spec: the protocol's dataset takes precedence over the query's own, either way.
#[test]
fn the_protocol_dataset_takes_precedence_over_the_querys() {
    let only_g2 = protocol(vec![GraphName::NamedNode(iri(G2))]);
    assert!(omits(
        Some(&only_g2),
        Some(&from(&[JENA_DEFAULT_GRAPH], None))
    ));
    let with_default = protocol(vec![GraphName::NamedNode(iri(G2)), GraphName::DefaultGraph]);
    assert!(!omits(Some(&with_default), Some(&from(&[G2], None))));
}

/// Spec: a default graph merged from every graph but some leaves the store's default graph
/// out only where it is one of those.
#[test]
fn a_default_graph_of_every_graph_but_some_leaves_out_only_those() {
    let mut excluding = QueryDatasetSpecification::new();
    excluding.set_default_graph_excluding(vec![GraphName::DefaultGraph]);
    assert!(omits(Some(&excluding), None));
    excluding.set_default_graph_excluding(vec![GraphName::NamedNode(iri(G2))]);
    assert!(!omits(Some(&excluding), None));
}

/// Spec: a dataset with the scoped inferred layer has inferred statements of its own: it
/// leaves none out. The layer comes with a list of graphs only: a default graph of every graph
/// but the store's default graph leaves it out, scoped or not.
#[test]
fn a_dataset_of_scoped_inference_leaves_out_no_inferred_statements() {
    let mut scoped = protocol(vec![GraphName::NamedNode(iri(G2))]);
    scoped.include_scoped_inference();
    assert!(!omits(Some(&scoped), None));
    let mut excluding = QueryDatasetSpecification::new();
    excluding.include_scoped_inference();
    excluding.set_default_graph_excluding(vec![GraphName::DefaultGraph]);
    assert!(omits(Some(&excluding), None));
}

/// Whether the default graph `resolve` gives reads the store's inferred statements: the
/// store's default graph, a merge holding it, or the scoped inferred layer.
fn reads_inferred(resolved: &ResolvedDataset) -> bool {
    match &resolved.default {
        DefaultGraph::Store | DefaultGraph::Merge(None | Some(GraphSet::Scoped(_))) => true,
        DefaultGraph::Graph(_) | DefaultGraph::Empty => false,
        DefaultGraph::Merge(Some(GraphSet::Only(ids))) => ids.contains(&TermId::DEFAULT_GRAPH),
        DefaultGraph::Merge(Some(GraphSet::AllBut(ids))) => !ids.contains(&TermId::DEFAULT_GRAPH),
    }
}

/// Spec: for each dataset, the store's default graph is left out exactly where the default
/// graph [`ResolvedDataset::resolve`] builds for it reads no inferred statement.
#[test]
fn it_says_what_resolve_reads() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let g2 = GraphName::NamedNode(iri(G2));
    tx.insert(
        Quad::new(
            iri("urn:ex:a"),
            iri("urn:ex:p"),
            iri("urn:ex:b"),
            g2.clone(),
        )
        .as_ref(),
    );
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let excluding = |graphs: Vec<GraphName>, scoped: bool| {
        let mut specification = QueryDatasetSpecification::new();
        if scoped {
            specification.include_scoped_inference();
        }
        specification.set_default_graph_excluding(graphs);
        specification
    };
    let scoped = |graphs: Vec<GraphName>| {
        let mut specification = protocol(graphs);
        specification.include_scoped_inference();
        specification
    };
    let cases = [
        ("none", None, None),
        ("FROM g2", None, Some(from(&[G2], None))),
        ("FROM NAMED g2", None, Some(from(&[], Some(&[G2])))),
        (
            "FROM g2 and the alias",
            None,
            Some(from(&[G2, JENA_DEFAULT_GRAPH], None)),
        ),
        (
            "protocol g2 over FROM the alias",
            Some(protocol(vec![g2.clone()])),
            Some(from(&[JENA_DEFAULT_GRAPH], None)),
        ),
        (
            "protocol g2 and default over FROM g2",
            Some(protocol(vec![g2.clone(), GraphName::DefaultGraph])),
            Some(from(&[G2], None)),
        ),
        (
            "all but the default graph",
            Some(excluding(vec![GraphName::DefaultGraph], false)),
            None,
        ),
        ("all but g2", Some(excluding(vec![g2.clone()], false)), None),
        ("scoped g2", Some(scoped(vec![g2.clone()])), None),
        (
            "scoped, all but the default graph",
            Some(excluding(vec![GraphName::DefaultGraph], true)),
            None,
        ),
    ];
    for (name, protocol, own) in &cases {
        let resolved = ResolvedDataset::resolve(&snapshot, false, protocol.as_ref(), own.as_ref());
        assert_eq!(
            omits(protocol.as_ref(), own.as_ref()),
            !reads_inferred(&resolved),
            "{name}: {resolved:?}"
        );
    }
}
