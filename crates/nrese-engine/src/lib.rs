//! NRESE storage engine (layer L1, see `docs/ARCHITECTURE.md` and ADR-0002).
//!
//! Owns the physical representation of an RDF dataset:
//! - [`term`]: RDF terms ⇄ 64-bit [`TermId`]s (dictionary plus inline canonical values)
//! - [`quad`]: encoded quads, patterns, and which permutation answers which pattern
//! - `index`: immutable sorted runs over seven permutations (four for the inferred stack),
//!   merged with sign-sum visibility, and size-tiered compaction
//! - [`engine`]: the public façade: [`Engine`], MVCC [`Snapshot`]s over an asserted and an
//!   inferred stack ([`ReadModel`]), single-writer [`Transaction`]s with exact deltas, and
//!   inline/background compaction
//! - `durability`: redo WAL, checkpoints and crash recovery behind [`Engine::open`]
//!
//! Not owned here: SPARQL (L2 `nrese-sparql`), reasoning and validation (L2), mutation
//! policy (L3 `nrese-store`), transport (L4). Fix storage-layout, visibility and durability
//! bugs here and nowhere else.

mod durability;
pub mod engine;
pub mod error;
mod index;
pub mod quad;
pub mod term;

pub use durability::{DurabilityConfig, SyncPolicy};
pub use engine::{
    BulkLoad, BulkMode, CommitSummary, Engine, EngineConfig, EngineStats, ReadModel,
    Rematerialisation, Snapshot, Transaction,
};
pub use error::{EngineError, EngineResult};
pub use index::CompactionPolicy;
pub use quad::{EncodedQuad, EncodedTriple, GraphSelector, QuadPattern};
pub use term::{Dictionary, DictionaryStats, TermId, TermKind, TermView, TextMatch, TextQuery};
