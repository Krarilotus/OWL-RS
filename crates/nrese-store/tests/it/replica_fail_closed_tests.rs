//! A primary whose served version is stored compact serves no read replica, refused at the
//! store (docs/ops/replication.md): its rows are kept over representatives, and a replica
//! would read them without that reading, as the only identities. A store configured
//! compact under a ruleset that doesn't close equality stores its rows as they are, and is
//! followed as any other; an operator's backup of a compact store is still written.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, SparqlUpdateRequest, StoreConfig,
    StoreService,
};

const PREFIXES: &str = "PREFIX ex: <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// What a refusal says, in part: the sentence a replica reports.
const REFUSED: &str = "serves no replica";

fn compact_primary(mode: ReasoningMode, dir: &std::path::Path) -> MutationPipeline {
    let config = StoreConfig {
        equality_by_representatives: true,
        equality_compact: true,
        ..StoreConfig::on_disk(dir)
    };
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn update(pipeline: &MutationPipeline, text: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{text}"))),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap_or_else(|error| panic!("{text}: {error}"));
}

/// Spec (the replica's fail-closed form): a store whose
/// served version is stored compact refuses `replication_log` with the sentence, whichever
/// revision it is asked after; under `rdfs`, which closes no equality, the same
/// configuration serves the last commit's record. `backup_image`, an operator's backup,
/// writes its image in every case. Each case is printed.
#[test]
fn a_primary_whose_rows_are_stored_compact_serves_no_log() {
    let mut wrong = Vec::new();
    for (mode, stored_compact) in [
        (ReasoningMode::Owl2Rl, true),
        (ReasoningMode::RdfsPlus, true),
        (ReasoningMode::Rdfs, false),
    ] {
        let (dir, backup) = (
            tempfile::tempdir().expect("dir"),
            tempfile::tempdir().expect("dir"),
        );
        let primary = compact_primary(mode, dir.path());
        update(
            &primary,
            "INSERT DATA { ex:Dog rdfs:subClassOf ex:Animal . ex:a a ex:Dog . \
             ex:a owl:sameAs ex:b }",
        );
        update(&primary, "INSERT DATA { ex:c owl:sameAs ex:a }");
        // The last commit wrote one record: the log after the revision before it holds it.
        let after = primary.store().current_revision() - 1;
        let log = primary.store().replication_log(after, 1 << 20);
        let seen = match &log {
            Ok(batch) => format!("{} record(s)", batch.records),
            Err(error) => format!("refused: {error}"),
        };
        let right = match &log {
            Err(error) => stored_compact && error.to_string().contains(REFUSED),
            Ok(batch) => !stored_compact && batch.records == 1,
        };
        let verdict = if right { "right" } else { "WRONG" };
        println!("CASE {mode:?} log after {after}: {verdict} {seen}");
        if !right {
            wrong.push(format!("{mode:?} log: {seen}"));
        }
        if let Err(error) = primary.store().backup_image(&backup.path().join("backup")) {
            println!("CASE {mode:?} backup: WRONG {error}");
            wrong.push(format!("{mode:?} backup: {error}"));
        }
    }
    assert!(wrong.is_empty(), "wrong: {wrong:#?}");
}
