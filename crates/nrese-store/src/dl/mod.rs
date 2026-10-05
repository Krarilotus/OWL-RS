//! The `owl2-dl` mode (docs/design/owl2-dl.md §8): OWL 2 DL reasoning through the store's
//! normal interfaces, every answer saying how complete it is.
//!
//! - [`config`]: the mode's settings.
//!
//! The mode is the reasoner's (`ReasoningMode::Owl2Dl`): its OWL 2 RL closure is the
//! inferred stack, the lower bound L. What the store adds is switched on by the mutation
//! pipeline that runs the mode ([`crate::StoreService::use_dl`]).

pub mod config;

use std::sync::atomic::{AtomicBool, Ordering};

pub use config::{DlAnswers, DlConfig, DlConsistency};

/// The store's DL state: whether the mode is on.
#[derive(Debug, Default)]
pub struct Dl {
    active: AtomicBool,
}

impl Dl {
    pub fn active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
    }
}
