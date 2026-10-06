//! The `owl2-dl` mode (docs/design/owl2-dl.md §8): OWL 2 DL reasoning through the store's
//! normal interfaces, every answer saying how complete it is.
//!
//! - [`config`]: the mode's settings.
//! - [`source`]: the ontology of the asserted statements, read for the engines.
//! - [`consistency`]: consistency by the engine the ontology allows; [`gate`]: the check
//!   on commit.
//! - [`classification`]: classification and realisation, kept per revision.
//! - [`entailment`]: axiom entailment, reduced to consistency.
//! - [`lower`]: L's memberships beyond the RL closure, from the TBox's taxonomy.
//! - [`upper`]: the upper bound U1 in a stack of its own; [`bounds`]: U1 per revision,
//!   maintained per commit, and the read view queries take.
//! - [`query`]: the query path: closed predicates, the bounds, the exact services, the
//!   status of every answer.
//!
//! The mode is the reasoner's (`ReasoningMode::Owl2Dl`): its OWL 2 RL closure is the
//! inferred stack, the lower bound L. What the store adds is switched on by the mutation
//! pipeline that runs the mode ([`crate::StoreService::use_dl`]).

pub(crate) mod bounds;
pub(crate) mod classification;
pub mod config;
pub mod consistency;
pub mod entailment;
pub(crate) mod gate;
pub(crate) mod lower;
pub(crate) mod query;
pub(crate) mod source;
pub mod upper;

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub use bounds::BoundsReport;
pub use config::{DlAnswers, DlConfig, DlConsistency};
pub use consistency::{Checked, Verdict};

/// The DL status of a revision: whether its data is consistent under OWL 2 DL, by which
/// engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlStatus {
    pub revision: u64,
    pub consistency: Checked,
}

/// The store's DL state: whether the mode is on, and the latest revision's status.
#[derive(Debug, Default)]
pub struct Dl {
    active: AtomicBool,
    status: Mutex<Option<DlStatus>>,
    pub(crate) classification: classification::Cache,
    pub(crate) bounds: bounds::Bounds,
    pub(crate) query_ontology: query::OntologyCache,
}

impl Dl {
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
    }

    /// The status recorded last (by a commit, or a check on request); for the current
    /// revision only if its `revision` is the store's.
    pub fn status(&self) -> Option<DlStatus> {
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(crate) fn record(&self, status: DlStatus) {
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = Some(status);
    }
}
