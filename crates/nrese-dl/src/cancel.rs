//! Cancellation shared by every engine: a flag checked at the engines' budget checks, with
//! no polling thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A flag that stops a run at its next budget check.
#[derive(Debug, Clone, Default)]
pub struct Cancel {
    flag: Arc<AtomicBool>,
    parent: Option<Arc<Cancel>>,
}

impl Cancel {
    /// Shares a caller's cancellation flag.
    pub fn from_flag(flag: Arc<AtomicBool>) -> Self {
        Self { flag, parent: None }
    }

    /// A child that may stop its siblings without cancelling the request; the parent's
    /// cancellation reaches it.
    pub(crate) fn child(parent: Option<&Self>) -> Self {
        Self {
            flag: Arc::default(),
            parent: parent.cloned().map(Arc::new),
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| parent.is_cancelled())
    }
}

impl PartialEq for Cancel {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.flag, &other.flag)
    }
}

impl Eq for Cancel {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sibling_stop_does_not_cancel_the_request_but_parent_cancellation_reaches_descendants() {
        let request = Cancel::default();
        let winner = Cancel::child(Some(&request));
        let sibling = winner.clone();
        winner.cancel();
        assert!(sibling.is_cancelled());
        assert!(!request.is_cancelled());
        let child = Cancel::child(Some(&request));
        let nested = Cancel::child(Some(&child));
        request.cancel();
        assert!(nested.is_cancelled());
    }
}
