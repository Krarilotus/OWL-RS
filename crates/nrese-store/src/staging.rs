use std::collections::BTreeSet;

use nrese_core::TripleSource;
use oxigraph::store::Store;

use crate::backup::{DatasetRestoreRequest, restore_dataset};
use crate::error::StoreResult;
use crate::graph_store::{GraphTarget, GraphWriteRequest};
use crate::graph_store_executor::{execute_graph_delete, execute_graph_write};
use crate::snapshot::StoreDatasetSnapshot;
use crate::update::SparqlUpdateRequest;
use crate::update_executor::execute_update;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MutationDeltaPreview {
    pub inserted_triples: Vec<(String, String, String)>,
    pub removed_triples: Vec<(String, String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedMutationPreview {
    pub snapshot: StoreDatasetSnapshot,
    pub delta: MutationDeltaPreview,
}

pub fn snapshot_after_update(
    source: &Store,
    request: &SparqlUpdateRequest,
    revision: u64,
) -> StoreResult<StagedMutationPreview> {
    preview_store_mutation(source, revision, |staged| {
        let _ = execute_update(staged, request)?;
        Ok(())
    })
}

pub fn snapshot_after_graph_write(
    source: &Store,
    request: &GraphWriteRequest,
    revision: u64,
) -> StoreResult<StagedMutationPreview> {
    preview_store_mutation(source, revision, |staged| {
        let _ = execute_graph_write(staged, request)?;
        Ok(())
    })
}

pub fn snapshot_after_graph_delete(
    source: &Store,
    target: &GraphTarget,
    revision: u64,
) -> StoreResult<StagedMutationPreview> {
    preview_store_mutation(source, revision, |staged| {
        let _ = execute_graph_delete(staged, target)?;
        Ok(())
    })
}

pub fn snapshot_after_restore(
    source: &Store,
    request: &DatasetRestoreRequest,
    revision: u64,
) -> StoreResult<StagedMutationPreview> {
    preview_store_mutation(source, revision, |staged| {
        let _ = restore_dataset(staged, request, revision)?;
        Ok(())
    })
}

fn preview_store_mutation<F>(
    source: &Store,
    revision: u64,
    apply: F,
) -> StoreResult<StagedMutationPreview>
where
    F: FnOnce(&Store) -> StoreResult<()>,
{
    let baseline = StoreDatasetSnapshot::capture(source, revision.saturating_sub(1))?;
    let staged = clone_store(source)?;
    apply(&staged)?;
    let snapshot = StoreDatasetSnapshot::capture(&staged, revision)?;
    let delta = diff_snapshots(&baseline, &snapshot);

    Ok(StagedMutationPreview { snapshot, delta })
}

fn clone_store(source: &Store) -> StoreResult<Store> {
    let staged = Store::new()?;

    for quad in source.quads_for_pattern(None, None, None, None) {
        let quad = quad?;
        staged.insert(&quad)?;
    }

    Ok(staged)
}

fn diff_snapshots(
    baseline: &StoreDatasetSnapshot,
    updated: &StoreDatasetSnapshot,
) -> MutationDeltaPreview {
    let baseline_triples = snapshot_triples(baseline);
    let updated_triples = snapshot_triples(updated);

    MutationDeltaPreview {
        inserted_triples: updated_triples
            .difference(&baseline_triples)
            .cloned()
            .collect(),
        removed_triples: baseline_triples
            .difference(&updated_triples)
            .cloned()
            .collect(),
    }
}

fn snapshot_triples(snapshot: &StoreDatasetSnapshot) -> BTreeSet<(String, String, String)> {
    TripleSource::triples(snapshot)
        .map(|triple| {
            (
                triple.subject.as_str().to_owned(),
                triple.predicate.as_str().to_owned(),
                triple.object.as_str().to_owned(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use nrese_core::DatasetSnapshot;

    use crate::update::SparqlUpdateRequest;

    use super::snapshot_after_update;

    #[test]
    fn staged_snapshot_applies_update_without_touching_source_store() {
        let source = oxigraph::store::Store::new().expect("source store");
        source
            .insert(oxigraph::model::QuadRef::new(
                oxigraph::model::NamedNodeRef::new("http://example.com/s").expect("subject"),
                oxigraph::model::NamedNodeRef::new("http://example.com/p").expect("predicate"),
                oxigraph::model::NamedNodeRef::new("http://example.com/o").expect("object"),
                oxigraph::model::GraphNameRef::DefaultGraph,
            ))
            .expect("seed source");

        let preview = snapshot_after_update(
            &source,
            &SparqlUpdateRequest::new(
                "INSERT DATA { <http://example.com/a> <http://example.com/p> <http://example.com/b> }",
            ),
            2,
        )
        .expect("staged snapshot");

        let source_count = source
            .quads_for_pattern(None, None, None, None)
            .collect::<Result<Vec<_>, _>>()
            .expect("source quads")
            .len();
        let snapshot_count = nrese_core::TripleSource::triples(&preview.snapshot).count();

        assert_eq!(source_count, 1);
        assert_eq!(preview.snapshot.revision(), 2);
        assert_eq!(snapshot_count, 2);
        assert_eq!(preview.delta.inserted_triples.len(), 1);
        assert!(preview.delta.removed_triples.is_empty());
    }
}
