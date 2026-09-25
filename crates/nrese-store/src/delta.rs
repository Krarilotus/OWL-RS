//! The pending delta of a mutation, in the shape the v1 reasoner gate and reject attribution
//! read: IRI-only triples as strings, flattened across graphs.
//!
//! Cost is O(delta): it decodes only the transaction's pending changes. (Engine v1 cloned the
//! store and diffed two full snapshots instead.)

use std::collections::BTreeSet;

use nrese_engine::{EncodedQuad, Transaction};
use oxrdf::{NamedOrBlankNode, Quad, Term};

use crate::error::StoreResult;
use crate::snapshot::StoreDatasetSnapshot;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MutationDeltaPreview {
    pub inserted_triples: Vec<(String, String, String)>,
    pub removed_triples: Vec<(String, String, String)>,
}

/// What a mutation would do, as seen by the validation gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedMutationPreview {
    pub snapshot: StoreDatasetSnapshot,
    pub delta: MutationDeltaPreview,
}

impl MutationDeltaPreview {
    pub(crate) fn of(tx: &Transaction<'_>) -> Self {
        Self {
            inserted_triples: iri_triples(tx, tx.inserted()),
            removed_triples: iri_triples(tx, tx.deleted()),
        }
    }
}

fn iri_triples(
    tx: &Transaction<'_>,
    quads: impl Iterator<Item = EncodedQuad>,
) -> Vec<(String, String, String)> {
    let triples: BTreeSet<_> = quads
        .filter_map(|quad| tx.decode_quad(quad))
        .filter_map(iri_triple)
        .collect();
    triples.into_iter().collect()
}

fn iri_triple(quad: Quad) -> Option<(String, String, String)> {
    let NamedOrBlankNode::NamedNode(subject) = quad.subject else {
        return None;
    };
    let Term::NamedNode(object) = quad.object else {
        return None;
    };
    Some((
        subject.into_string(),
        quad.predicate.into_string(),
        object.into_string(),
    ))
}

/// The post-mutation dataset for the v1 reasoner: O(dataset) when reasoning is enabled,
/// O(1) otherwise.
pub(crate) fn gate_snapshot(
    tx: &Transaction<'_>,
    revision: u64,
    reasoning_reads_triples: bool,
) -> StoreResult<StoreDatasetSnapshot> {
    if !reasoning_reads_triples {
        return Ok(StoreDatasetSnapshot::summary(revision, tx.len()));
    }
    StoreDatasetSnapshot::capture(
        crate::view::decoded_quads(tx, &nrese_engine::QuadPattern::all()),
        revision,
    )
}
