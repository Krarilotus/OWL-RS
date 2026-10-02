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

use nrese_reasoner::RuleProgram;

/// The persisted reasoning state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReasoningState {
    pub ruleset: String,
    /// [`RuleProgram::fingerprint`] of the semantics the stack was computed with.
    pub fingerprint: u64,
    /// Consistency violations in the materialised closure.
    pub violations: usize,
    /// The inferred stack holds the closure over representatives of the `owl:sameAs`
    /// classes, which reads expand (`reasoner.equality = "compact"`).
    pub compact_equality: bool,
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

/// The fingerprint of `program` as the store runs it: leaving out unnamed classes'
/// memberships is another closure.
pub(crate) fn fingerprint(
    program: &RuleProgram,
    hide_unnamed_classes: bool,
    compact_equality: bool,
) -> u64 {
    const HIDDEN_UNNAMED_CLASSES: u64 = 0x5717_c1a5_5e5f_0007;
    // Another form of the stack, not another closure; still not interchangeable.
    const COMPACT_EQUALITY: u64 = 0x0e9a_11c0_3a5c_0b02;
    program.fingerprint()
        ^ if hide_unnamed_classes {
            HIDDEN_UNNAMED_CLASSES
        } else {
            0
        }
        ^ if compact_equality {
            COMPACT_EQUALITY
        } else {
            0
        }
}

impl ReasoningState {
    #[cfg(test)]
    pub(crate) fn of(program: &RuleProgram, violations: usize) -> Self {
        Self::of_with(program, false, false, violations)
    }

    pub(crate) fn of_with(
        program: &RuleProgram,
        hide_unnamed_classes: bool,
        compact_equality: bool,
        violations: usize,
    ) -> Self {
        Self {
            ruleset: program.name(),
            fingerprint: fingerprint(program, hide_unnamed_classes, compact_equality),
            violations,
            compact_equality,
        }
    }

    /// Whether the inferred stack is exactly `program`'s closure of the asserted data.
    pub fn is_current_for(&self, program: impl Into<RuleProgram>) -> bool {
        self.is_current_with(&program.into(), false, false)
    }

    /// [`Self::is_current_for`] with unnamed classes' memberships left out or not, and
    /// equality stored compactly or not.
    pub fn is_current_with(
        &self,
        program: &RuleProgram,
        hide_unnamed_classes: bool,
        compact_equality: bool,
    ) -> bool {
        self.ruleset == program.name()
            && self.fingerprint == fingerprint(program, hide_unnamed_classes, compact_equality)
            && self.compact_equality == compact_equality
    }

    pub fn consistency(&self) -> ConsistencyStatus {
        match self.violations {
            0 => ConsistencyStatus::Consistent,
            violations => ConsistencyStatus::Inconsistent { violations },
        }
    }

    /// The file form: `key value` lines.
    pub(crate) fn to_text(&self) -> String {
        let mut text = format!(
            "ruleset {}\nfingerprint {:016x}\nviolations {}\n",
            self.ruleset, self.fingerprint, self.violations
        );
        if self.compact_equality {
            text.push_str("equality compact\n");
        }
        text
    }

    /// Parses [`to_text`](Self::to_text); `None` for anything else, including the older
    /// marker that held only the ruleset name (the store then rematerialises once).
    pub(crate) fn from_text(text: &str) -> Option<Self> {
        let (mut ruleset, mut fingerprint, mut violations) = (None, None, None);
        let mut compact_equality = false;
        for line in text.lines() {
            let (key, value) = line.split_once(' ')?;
            match key {
                "ruleset" => ruleset = Some(value.to_owned()),
                "fingerprint" => fingerprint = u64::from_str_radix(value, 16).ok(),
                "violations" => violations = value.parse().ok(),
                "equality" => compact_equality = value == "compact",
                _ => {}
            }
        }
        Some(Self {
            ruleset: ruleset?,
            fingerprint: fingerprint?,
            violations: violations?,
            compact_equality,
        })
    }
}

#[cfg(test)]
mod tests {
    use nrese_reasoner::v2::rulesets::Ruleset;

    use super::*;

    #[test]
    fn state_round_trips_and_old_markers_are_not_current() {
        let state = ReasoningState::of(&RuleProgram::builtin(Ruleset::Owl2Rl), 3);
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
