use std::collections::{BTreeMap, BTreeSet};

use nrese_engine::Snapshot;
use nrese_rdf::{Literal, NamedNode, Term};
use nrese_shacl::{PropertyPath, ValidationReport, ValidationResult};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ResultTerm {
    Rdf(Term),
    FreshBlank,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ResultKey {
    pub focus: ResultTerm,
    pub path: Option<PropertyPath>,
    pub value: Option<ResultTerm>,
    pub shape: ResultTerm,
    pub constraint: Option<ResultTerm>,
    pub component: NamedNode,
    pub severity: NamedNode,
    pub messages: Vec<Literal>,
}

pub(crate) struct Observer<'a> {
    before: &'a Snapshot,
    after: &'a Snapshot,
    fresh_value: bool,
}

impl<'a> Observer<'a> {
    pub(crate) fn new(before: &'a Snapshot, after: &'a Snapshot, fresh_value: bool) -> Self {
        Self {
            before,
            after,
            fresh_value,
        }
    }

    fn term(&self, term: &Term, fresh_allowed: bool) -> Result<ResultTerm, String> {
        if matches!(term, Term::BlankNode(_))
            && self.before.lookup(term.as_ref()).is_none()
            && self.after.lookup(term.as_ref()).is_none()
        {
            return if fresh_allowed {
                Ok(ResultTerm::FreshBlank)
            } else {
                Err(format!("unexpected fresh blank node {term}"))
            };
        }
        Ok(ResultTerm::Rdf(term.clone()))
    }

    fn key(&self, result: &ValidationResult) -> Result<ResultKey, String> {
        let mut messages = result.messages.clone();
        messages.sort();
        Ok(ResultKey {
            focus: self.term(&result.focus_node, false)?,
            path: result.path.clone(),
            value: result
                .value
                .as_ref()
                .map(|term| self.term(term, self.fresh_value))
                .transpose()?,
            shape: self.term(&result.source_shape, false)?,
            constraint: result
                .source_constraint
                .as_ref()
                .map(|term| self.term(term, false))
                .transpose()?,
            component: result.component.clone(),
            severity: result.severity.clone(),
            messages,
        })
    }

    pub(crate) fn observe(&self, report: &ValidationReport) -> Result<Observation, String> {
        let results = report
            .results
            .iter()
            .map(|result| self.key(result))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Observation {
            results: counts(results),
            failures: counts(report.failures.iter().cloned()),
        })
    }
}

fn counts<K: Ord>(values: impl IntoIterator<Item = K>) -> BTreeMap<K, usize> {
    let mut counts = BTreeMap::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    counts
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Observation {
    pub results: BTreeMap<ResultKey, usize>,
    pub failures: BTreeMap<String, usize>,
}

impl Observation {
    pub(crate) fn result_set(&self) -> BTreeSet<ResultKey> {
        self.results.keys().cloned().collect()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Direction {
    Missed,
    Extra,
    Duplicate,
    Mixed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Count {
    pub expected: usize,
    pub actual: usize,
}

impl Count {
    pub(crate) fn direction(self) -> Direction {
        if self.actual < self.expected {
            Direction::Missed
        } else if self.expected == 0 {
            Direction::Extra
        } else {
            Direction::Duplicate
        }
    }
}

pub(crate) type Delta = BTreeMap<String, Count>;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Deviation {
    pub results: Delta,
    pub failures: Delta,
}

fn delta<K: Clone + Ord>(
    expected: &BTreeMap<K, usize>,
    actual: &BTreeMap<K, usize>,
    label: impl Fn(&K) -> String,
) -> Delta {
    let keys: BTreeSet<_> = expected.keys().chain(actual.keys()).collect();
    keys.into_iter()
        .filter_map(|key| {
            let expected = expected.get(key).copied().unwrap_or_default();
            let actual = actual.get(key).copied().unwrap_or_default();
            (expected != actual).then(|| (label(key), Count { expected, actual }))
        })
        .collect()
}

pub(crate) fn compare(
    before: &Observation,
    after: &Observation,
    actual: &Observation,
) -> Deviation {
    let expected = after
        .results
        .keys()
        .filter(|key| !before.results.contains_key(*key))
        .cloned()
        .map(|key| (key, 1))
        .collect();
    Deviation {
        results: delta(&expected, &actual.results, |key| format!("{key:?}")),
        failures: delta(&after.failures, &actual.failures, Clone::clone),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Spec: the SHACL change differential expects one copy of each introduced
    // key and every post-state failure, including repeated failures.
    #[test]
    fn compare_counts_results_and_failures_independently() {
        let node = NamedNode::new_unchecked("urn:focus");
        let term = ResultTerm::Rdf(node.clone().into());
        let key = ResultKey {
            focus: term.clone(),
            path: None,
            value: None,
            shape: term,
            constraint: None,
            component: node.clone(),
            severity: node,
            messages: vec![],
        };
        let before = Observation::default();
        let after = Observation {
            results: BTreeMap::from([(key.clone(), 3)]),
            failures: BTreeMap::from([("validation failed".into(), 2)]),
        };
        let actual = Observation {
            results: BTreeMap::from([(key, 2)]),
            failures: BTreeMap::from([("validation failed".into(), 1)]),
        };
        let deviation = compare(&before, &after, &actual);
        assert_eq!(
            deviation.results.values().copied().collect::<Vec<_>>(),
            [Count {
                expected: 1,
                actual: 2
            }]
        );
        assert_eq!(
            deviation.failures,
            BTreeMap::from([(
                "validation failed".into(),
                Count {
                    expected: 2,
                    actual: 1
                }
            )])
        );
    }
}
