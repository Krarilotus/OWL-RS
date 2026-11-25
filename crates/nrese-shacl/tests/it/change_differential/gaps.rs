use std::collections::BTreeSet;

use serde::Deserialize;

use super::observe::{Delta, Deviation, Direction};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
pub(crate) enum EntryPoint {
    #[serde(rename = "C1")]
    Data,
    #[serde(rename = "C2")]
    Shapes,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Result,
    Failure,
}

impl Kind {
    pub(super) fn select(self, deviation: &Deviation) -> &Delta {
        match self {
            Self::Result => &deviation.results,
            Self::Failure => &deviation.failures,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Location<'a> {
    pub case: &'a str,
    pub step: usize,
    pub entry: EntryPoint,
}

#[derive(Deserialize)]
pub(crate) struct Gap {
    pub id: String,
    pub case: String,
    pub step: usize,
    pub entry: EntryPoint,
    pub kind: Kind,
    pub direction: Direction,
    pub delta: Delta,
}

impl Gap {
    pub(crate) fn verify_witness(&self, raw: &Deviation) -> Result<(), &'static str> {
        if raw.results.is_empty() && raw.failures.is_empty() {
            return Err("fixed, delete the entry");
        }
        let unexpected_kind = match self.kind {
            Kind::Result => !raw.failures.is_empty(),
            Kind::Failure => !raw.results.is_empty(),
        };
        if unexpected_kind || !self.matches_delta(self.kind, self.kind.select(raw)) {
            return Err("changed");
        }
        Ok(())
    }

    fn matches_delta(&self, kind: Kind, delta: &Delta) -> bool {
        self.kind == kind && self.delta == *delta && direction(delta) == Some(self.direction)
    }
}

fn direction(delta: &Delta) -> Option<Direction> {
    let mut counts = delta.values();
    let first = counts.next()?.direction();
    Some(if counts.all(|count| count.direction() == first) {
        first
    } else {
        Direction::Mixed
    })
}

/// Use a fresh one for each padding run. Witnesses are checked before matching.
#[derive(Default)]
pub(crate) struct Allowances<'a> {
    used: BTreeSet<&'a str>,
}

impl<'a> Allowances<'a> {
    pub(crate) fn forgive(
        &mut self,
        gaps: &'a [Gap],
        location: &Location<'_>,
        kind: Kind,
        deviation: &Deviation,
    ) -> Option<&'a str> {
        let gap = gaps.iter().find(|gap| {
            gap.case == location.case
                && gap.step == location.step
                && gap.entry == location.entry
                && gap.matches_delta(kind, kind.select(deviation))
        })?;
        self.used.insert(&gap.id).then_some(&gap.id)
    }

    /// The entries whose case ran in this run but forgave nothing: their gap is closed.
    pub(crate) fn unused(&self, gaps: &'a [Gap], ran: &BTreeSet<String>) -> Vec<&'a str> {
        gaps.iter()
            .filter(|gap| ran.contains(&gap.case) && !self.used.contains(gap.id.as_str()))
            .map(|gap| gap.id.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change_differential::observe::Count;

    fn missed() -> Delta {
        Delta::from([(
            "key".into(),
            Count {
                expected: 1,
                actual: 0,
            },
        )])
    }

    fn gap() -> Gap {
        Gap {
            id: "missing-result".into(),
            case: "row:missing".into(),
            step: 1,
            entry: EntryPoint::Data,
            kind: Kind::Result,
            direction: Direction::Missed,
            delta: missed(),
        }
    }

    fn location() -> Location<'static> {
        Location {
            case: "row:missing",
            step: 1,
            entry: EntryPoint::Data,
        }
    }

    fn result(results: Delta) -> Deviation {
        Deviation {
            results,
            ..Deviation::default()
        }
    }

    // Spec: a gap covers the exact observed counts, including duplicate multiplicity.
    #[test]
    fn allowance_checks_exact_counts() {
        let extra = |actual| {
            Delta::from([(
                "key".into(),
                Count {
                    expected: 0,
                    actual,
                },
            )])
        };
        let gaps = [Gap {
            direction: Direction::Extra,
            delta: extra(1),
            ..gap()
        }];
        assert_eq!(
            Allowances::default().forgive(&gaps, &location(), Kind::Result, &result(extra(2))),
            None
        );
    }

    // Spec: a matching deviation from another step, case or entry point needs its own gap.
    #[test]
    fn allowance_checks_the_location() {
        let gaps = [gap()];
        for location in [
            Location {
                step: 2,
                ..location()
            },
            Location {
                case: "row:other",
                ..location()
            },
            Location {
                entry: EntryPoint::Shapes,
                ..location()
            },
        ] {
            assert_eq!(
                Allowances::default().forgive(&gaps, &location, Kind::Result, &result(missed())),
                None,
                "{location:?}"
            );
        }
    }

    // Spec: a gap may cover one deviation per padding run.
    #[test]
    fn allowance_is_used_once_per_run() {
        let gaps = [gap()];
        let mut run = Allowances::default();
        assert_eq!(
            run.forgive(&gaps, &location(), Kind::Result, &result(missed())),
            Some("missing-result")
        );
        assert_eq!(
            run.forgive(&gaps, &location(), Kind::Result, &result(missed())),
            None
        );
    }

    // Spec: even identical signed counts for a failure need an entry of the failure kind.
    #[test]
    fn a_result_allowance_does_not_forgive_a_failure() {
        let gaps = [gap()];
        let deviation = Deviation {
            failures: missed(),
            ..Deviation::default()
        };
        assert_eq!(
            Allowances::default().forgive(&gaps, &location(), Kind::Failure, &deviation),
            None
        );
    }

    // Spec: an entry whose row ran without deviating is closed, and the run fails on it;
    // one whose row didn't run is not judged.
    #[test]
    fn a_known_gap_row_that_passes_is_closed() {
        let gaps = [gap()];
        let ran = BTreeSet::from(["row:missing".to_owned()]);
        let mut run = Allowances::default();
        assert_eq!(run.unused(&gaps, &ran), ["missing-result"]);
        assert_eq!(run.unused(&gaps, &BTreeSet::new()), Vec::<&str>::new());
        run.forgive(&gaps, &location(), Kind::Result, &result(missed()));
        assert_eq!(run.unused(&gaps, &ran), Vec::<&str>::new());
    }

    // Spec: an entry is stale once its witness stops deviating.
    #[test]
    fn witness_rejects_a_fixed_gap() {
        assert_eq!(
            gap().verify_witness(&Deviation::default()),
            Err("fixed, delete the entry")
        );
    }

    // Spec: the witness's whole raw deviation must match the entry before it can be used.
    #[test]
    fn witness_rejects_a_changed_gap() {
        let mut raw = result(missed());
        raw.failures = missed();
        assert_eq!(gap().verify_witness(&raw), Err("changed"));
        raw.results.extend([(
            "another key".into(),
            Count {
                expected: 1,
                actual: 0,
            },
        )]);
        raw.failures.clear();
        assert_eq!(gap().verify_witness(&raw), Err("changed"));
    }
}
