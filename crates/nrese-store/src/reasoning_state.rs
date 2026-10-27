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
    /// The closure reads lists through `rdf:nil`'s equality class, as the replacement rules
    /// do (`nrese_reasoner::batch::Materialisation::nil_merged`): every state written since
    /// 27 Oct 2026 except a compact stack's, which keeps the old reading. A state without it
    /// is current only for data that doesn't equate `rdf:nil` with another term, so only a
    /// store whose data does rebuilds, once.
    pub nil_lists: bool,
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

/// Whether `snapshot` equates `rdf:nil` with another term, asserted or inferred
/// ([`ReasoningState::nil_lists`]).
pub(crate) fn nil_equated(snapshot: &nrese_engine::Snapshot) -> bool {
    use nrese_engine::{GraphSelector, QuadPattern, ReadModel};
    use nrese_rdf::vocab::{owl, rdf};
    let (Some(nil), Some(same_as)) = (
        snapshot.lookup(rdf::NIL.into()),
        snapshot.lookup(owl::SAME_AS.into()),
    ) else {
        return false;
    };
    let pattern = |subject, object| QuadPattern {
        subject,
        predicate: Some(same_as),
        object,
        graph: GraphSelector::Any,
    };
    [ReadModel::Asserted, ReadModel::Inferred]
        .into_iter()
        .any(|model| {
            (snapshot.quads_for_pattern_in(model, &pattern(Some(nil), None)))
                .any(|quad| quad.object != nil)
                || (snapshot.quads_for_pattern_in(model, &pattern(None, Some(nil))))
                    .any(|quad| quad.subject != nil)
        })
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
            nil_lists: !compact_equality,
        }
    }

    /// Whether the inferred stack is exactly `program`'s closure of the asserted data. Not
    /// knowing the data, a state without [`Self::nil_lists`] is taken as stale.
    pub fn is_current_for(&self, program: impl Into<RuleProgram>) -> bool {
        self.is_current_with(&program.into(), false, false, || true)
    }

    /// [`Self::is_current_for`] with unnamed classes' memberships left out or not and
    /// equality stored compactly or not; `nil_equated` tells whether the data equates
    /// `rdf:nil` with another term, asked only of a state without [`Self::nil_lists`].
    pub fn is_current_with(
        &self,
        program: &RuleProgram,
        hide_unnamed_classes: bool,
        compact_equality: bool,
        nil_equated: impl FnOnce() -> bool,
    ) -> bool {
        self.ruleset == program.name()
            && self.fingerprint == fingerprint(program, hide_unnamed_classes, compact_equality)
            && self.compact_equality == compact_equality
            && (compact_equality || self.nil_lists || !nil_equated())
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
        if self.nil_lists {
            text.push_str("lists nil-class\n");
        }
        text
    }

    /// Parses [`to_text`](Self::to_text); `None` for anything else, including the older
    /// marker that held only the ruleset name (the store then rematerialises once).
    pub(crate) fn from_text(text: &str) -> Option<Self> {
        let (mut ruleset, mut fingerprint, mut violations) = (None, None, None);
        let (mut compact_equality, mut nil_lists) = (false, false);
        for line in text.lines() {
            let (key, value) = line.split_once(' ')?;
            match key {
                "ruleset" => ruleset = Some(value.to_owned()),
                "fingerprint" => fingerprint = u64::from_str_radix(value, 16).ok(),
                "violations" => violations = value.parse().ok(),
                "equality" => compact_equality = value == "compact",
                "lists" => nil_lists = value == "nil-class",
                _ => {}
            }
        }
        Some(Self {
            ruleset: ruleset?,
            fingerprint: fingerprint?,
            violations: violations?,
            compact_equality,
            nil_lists,
        })
    }
}

#[cfg(test)]
mod tests {
    use nrese_reasoner::rulesets::Ruleset;

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

    /// A state written before [`ReasoningState::nil_lists`] stays current for a store whose
    /// data doesn't equate `rdf:nil` with another term and is stale for one whose data does:
    /// only that store rebuilds. A state written since stays current when a commit equates
    /// it, so the next write doesn't rebuild.
    #[test]
    fn only_data_equating_nil_makes_an_older_state_stale() {
        use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
        use std::sync::Arc;
        let config = || ReasonerConfig::for_mode(ReasoningMode::Owl2Rl);
        let program = config().materialised_program().unwrap();
        for (other, older_is_current) in [
            ("urn:m", true),
            ("http://www.w3.org/1999/02/22-rdf-syntax-ns#nil", false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let open = || {
                let store = crate::StoreService::new(crate::StoreConfig::on_disk(dir.path()));
                crate::MutationPipeline::new(
                    Arc::new(store.unwrap()),
                    Arc::new(ReasonerService::new(config())),
                )
            };
            let pipeline = open();
            let update = format!(
                "INSERT DATA {{ <urn:l> <http://www.w3.org/2002/07/owl#sameAs> <{other}> }}"
            );
            pipeline
                .apply(
                    crate::MutationCommand::Update(crate::SparqlUpdateRequest::new(update)),
                    &crate::Requester::all(),
                    &crate::MutationTicket::new(),
                )
                .unwrap();
            let current = |pipeline: &crate::MutationPipeline| {
                pipeline.store().reasoning_is_current(&program)
            };
            assert!(current(&pipeline), "{other}: a state written since");
            drop(pipeline);
            // As the store wrote it before.
            let older = format!(
                "ruleset {}\nfingerprint {:016x}\nviolations 0\n",
                program.name(),
                program.fingerprint()
            );
            std::fs::write(dir.path().join("reasoning.state"), older).unwrap();
            assert_eq!(
                current(&open()),
                older_is_current,
                "{other}: a state written before"
            );
        }
    }
}
