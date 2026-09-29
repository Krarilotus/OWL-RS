//! What the inferred stack is exact for: the ruleset, the semantics it was computed with
//! and whether the data was consistent under it.
//!
//! [`StoreService::rematerialise`](crate::StoreService::rematerialise) records the state;
//! reasoner-v2 commits keep it (they maintain the inferred stack and reject new
//! violations); writes that bypass reasoning drop it. On-disk stores persist it as
//! `reasoning.state`, so a restart can skip rematerialisation, but only when both the
//! ruleset name **and** its semantic fingerprint match: an upgrade that changes what a
//! ruleset derives forces a rebuild.
//!
//! **Consistency.** Commit-path reasoning checks only the facts a commit adds, so it relies
//! on a consistent baseline. A rematerialisation that finds violations therefore puts the
//! store in *quarantine*: the data stays readable for diagnosis and consistent commits
//! (repairs) are accepted, but the store doesn't report itself ready, and every commit
//! revalidates until no violation is left.

use nrese_reasoner::v2::rulesets::Ruleset;

/// The persisted reasoning state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningState {
    pub ruleset: String,
    /// [`Ruleset::fingerprint`] of the semantics the stack was computed with.
    pub fingerprint: u64,
    /// Consistency violations in the materialised closure.
    pub violations: usize,
}

/// Whether the stored data is consistent under the configured ruleset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsistencyStatus {
    /// No current reasoning state (reasoning off, or not yet materialised).
    Unknown,
    Consistent,
    /// Quarantine: violations in the baseline (see the module docs).
    Inconsistent {
        violations: usize,
    },
}

impl ConsistencyStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Consistent => "consistent",
            Self::Inconsistent { .. } => "inconsistent",
        }
    }
}

impl ReasoningState {
    pub(crate) fn of(ruleset: Ruleset, violations: usize) -> Self {
        Self {
            ruleset: ruleset.name().to_owned(),
            fingerprint: ruleset.fingerprint(),
            violations,
        }
    }

    /// Whether the inferred stack is exactly `ruleset`'s closure of the asserted data.
    pub fn is_current_for(&self, ruleset: Ruleset) -> bool {
        self.ruleset == ruleset.name() && self.fingerprint == ruleset.fingerprint()
    }

    pub fn consistency(&self) -> ConsistencyStatus {
        match self.violations {
            0 => ConsistencyStatus::Consistent,
            violations => ConsistencyStatus::Inconsistent { violations },
        }
    }

    /// The file form: `key value` lines.
    pub(crate) fn to_text(&self) -> String {
        format!(
            "ruleset {}\nfingerprint {:016x}\nviolations {}\n",
            self.ruleset, self.fingerprint, self.violations
        )
    }

    /// Parses [`to_text`](Self::to_text); `None` for anything else, including the older
    /// marker that held only the ruleset name (the store then rematerialises once).
    pub(crate) fn from_text(text: &str) -> Option<Self> {
        let (mut ruleset, mut fingerprint, mut violations) = (None, None, None);
        for line in text.lines() {
            let (key, value) = line.split_once(' ')?;
            match key {
                "ruleset" => ruleset = Some(value.to_owned()),
                "fingerprint" => fingerprint = u64::from_str_radix(value, 16).ok(),
                "violations" => violations = value.parse().ok(),
                _ => {}
            }
        }
        Some(Self {
            ruleset: ruleset?,
            fingerprint: fingerprint?,
            violations: violations?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_and_old_markers_are_not_current() {
        let state = ReasoningState::of(Ruleset::Owl2Rl, 3);
        assert_eq!(
            ReasoningState::from_text(&state.to_text()),
            Some(state.clone())
        );
        assert!(state.is_current_for(Ruleset::Owl2Rl));
        assert!(!state.is_current_for(Ruleset::Rdfs));
        assert_eq!(
            state.consistency(),
            ConsistencyStatus::Inconsistent { violations: 3 }
        );
        // The marker before semantic fingerprints: just the name.
        assert_eq!(ReasoningState::from_text("owl2-rl"), None);
        // Another fingerprint (changed semantics) is not current.
        let stale = ReasoningState {
            fingerprint: state.fingerprint ^ 1,
            ..state
        };
        assert!(!stale.is_current_for(Ruleset::Owl2Rl));
    }
}
