//! Whether a query's answers are complete: one concept for every path that can leave
//! certain answers out (OWL 2 DL's bounds in the store's `owl2-dl` mode; OWL 2 QL's
//! rewriting, whose bounds and limits it reports the same way), carried with every answer
//! and in EXPLAIN (docs/design/owl2-dl.md §8, "Every answer carries its status").
//!
//! Answers are always sound: what is returned is entailed. `complete` says that nothing
//! entailed is missing; otherwise [`Completeness::reasons`] say why it may be, and for
//! answers through bounds, [`Bounds`] give the counts.

/// The bounds an answer was computed from: the lower bound's answers (certain), the upper
/// bound's (every certain one is among them), and those of the gap no exact service
/// decided.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Bounds {
    /// Answers in the lower bound.
    pub lower: u64,
    /// Answers in the upper bound (`None`: not evaluated, or not available).
    pub upper: Option<u64>,
    /// Answers of the gap an exact service proved certain (returned).
    pub proved: u64,
    /// Answers of the gap an exact service refuted (not returned).
    pub refuted: u64,
    /// Answers of the gap neither proved nor refuted (not returned).
    pub unresolved: u64,
}

/// Whether the answers are complete, and why not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Completeness {
    reasons: Vec<String>,
    /// The bounds, for answers through them.
    pub bounds: Option<Bounds>,
    /// The services that decided candidates (`exact-ground-entailment`,
    /// `exact-internalisable-cq`), or what made the answer exact without them
    /// (`closed-predicates`, `bounds-equal`).
    pub paths: Vec<&'static str>,
}

impl Completeness {
    /// Complete answers.
    pub fn complete() -> Self {
        Self::default()
    }

    /// Sound answers that may miss some, for `reason`.
    pub fn sound_only(reason: impl Into<String>) -> Self {
        let mut c = Self::default();
        c.add(reason.into());
        c
    }

    pub fn is_complete(&self) -> bool {
        self.reasons.is_empty()
    }

    /// `complete` or `sound-only`.
    pub fn as_str(&self) -> &'static str {
        match self.is_complete() {
            true => "complete",
            false => "sound-only",
        }
    }

    /// Why answers may be missing (empty: complete).
    pub fn reasons(&self) -> &[String] {
        &self.reasons
    }

    /// Adds a reason (once).
    pub fn add(&mut self, reason: String) {
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }

    /// Notes a path that decided answers (once).
    pub fn path(&mut self, path: &'static str) {
        if !self.paths.contains(&path) {
            self.paths.push(path);
        }
    }

    /// Both: complete only if both are.
    pub fn merge(&mut self, other: Completeness) {
        for reason in other.reasons {
            self.add(reason);
        }
        for path in other.paths {
            self.path(path);
        }
        if self.bounds.is_none() {
            self.bounds = other.bounds;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_make_answers_sound_only_once_each() {
        let mut c = Completeness::complete();
        assert!(c.is_complete());
        assert_eq!(c.as_str(), "complete");
        c.add("gap".to_owned());
        c.add("gap".to_owned());
        assert_eq!(c.as_str(), "sound-only");
        assert_eq!(c.reasons(), ["gap".to_owned()]);
        let mut other = Completeness::sound_only("limit");
        other.path("exact-ground-entailment");
        c.merge(other);
        assert_eq!(c.reasons().len(), 2);
        assert_eq!(c.paths, ["exact-ground-entailment"]);
    }
}
