use std::collections::{BTreeMap, BTreeSet};

use crate::routes::{Row, Rows};
use crate::twin::Draw;

type Bag = BTreeMap<Row, usize>;
type Key = Vec<Option<String>>;
type Counts = BTreeMap<Key, usize>;

fn bag(rows: &Rows, reduced: bool) -> Bag {
    let mut bag = Bag::new();
    for row in rows {
        let count = bag.entry(row.clone()).or_default();
        *count = if reduced { 1 } else { *count + 1 };
    }
    bag
}

fn subbag(part: &Bag, full: &Bag) -> bool {
    part.iter()
        .all(|(row, n)| full.get(row).is_some_and(|m| n <= m))
}

pub fn same(expected: &Rows, found: &Rows, reduced: bool) -> Result<(), String> {
    let expected = bag(expected, reduced);
    let found = bag(found, reduced);
    if expected == found {
        return Ok(());
    }
    let first_difference = expected
        .keys()
        .chain(found.keys())
        .find(|row| expected.get(*row) != found.get(*row))
        .unwrap();
    Err(format!(
        "bag at {first_difference:?}: expected {}, found {}",
        expected.get(first_difference).copied().unwrap_or_default(),
        found.get(first_difference).copied().unwrap_or_default()
    ))
}

fn key(row: &Row, columns: &[&str]) -> Key {
    columns.iter().map(|c| row.get(*c).cloned()).collect()
}

fn counts(rows: &Rows, columns: &[&str]) -> Counts {
    let mut counts = Counts::new();
    for row in rows {
        *counts.entry(key(row, columns)).or_default() += 1;
    }
    counts
}

fn without(rows: &Rows, column: &str) -> Rows {
    rows.iter()
        .map(|row| {
            let mut row = row.clone();
            row.remove(column);
            row
        })
        .collect()
}

fn values<'a>(rows: &'a Rows, column: &str, keys: &[&str]) -> BTreeMap<Key, BTreeSet<&'a str>> {
    let mut groups = BTreeMap::<_, BTreeSet<_>>::new();
    for row in rows {
        let group = groups.entry(key(row, keys)).or_default();
        if let Some(value) = row.get(column) {
            group.insert(value.as_str());
        }
    }
    groups
}

pub fn check(
    draw: Draw,
    twin: &Rows,
    found: &Rows,
    scope: Option<&Rows>,
) -> Result<(), (&'static str, String)> {
    match draw {
        Draw::Value { column, keys, .. } => {
            same(&without(twin, column), &without(found, column), false).map_err(|e| ("P5", e))?;
            // Distinct stand-ins include the site, so the two UNION arms may
            // independently draw for the same projected key.
            let sites = values(twin, column, keys);
            let multiplicity = scope.map(|rows| counts(rows, keys));
            let mut owner = BTreeMap::new();
            if found.iter().any(|r| !r.contains_key(column)) {
                return Err(("P1", format!("missing drawn value in ?{column}")));
            }
            for (key, values) in values(found, column, keys) {
                let m = multiplicity
                    .as_ref()
                    .and_then(|counts| counts.get(&key).copied())
                    .unwrap_or_else(|| sites[&key].len());
                if values.is_empty() || values.len() > m {
                    return Err((
                        "P1",
                        format!(
                            "key {key:?}: {} distinct draws, allowed 1..={m}",
                            values.len()
                        ),
                    ));
                }
                for value in values {
                    if let Some(first) = owner.insert(value, key.clone()) {
                        return Err(("P2", format!("one draw shared by {first:?} and {key:?}")));
                    }
                }
            }
            Ok(())
        }
        Draw::Filter { keys, independent } => {
            let wanted = counts(twin, keys);
            let kept = counts(found, keys);
            if !subbag(&bag(found, false), &bag(twin, false)) {
                return Err(("P3", "the kept rows are not a sub-bag of the twin".into()));
            }
            let multiplicity = scope.map(|rows| counts(rows, keys));
            for (key, n) in &wanted {
                let m = multiplicity
                    .as_ref()
                    .and_then(|counts| counts.get(key).copied())
                    .unwrap_or(1);
                let actual = kept.get(key).copied().unwrap_or_default();
                assert!(
                    m > 0 && n.is_multiple_of(m),
                    "uneven copies in the fixture for {key:?}"
                );
                let copies = n / m;
                if !actual.is_multiple_of(copies) {
                    return Err((
                        "P3",
                        format!(
                            "key {key:?}: {actual} of {n} rows kept; copies come in groups of {copies}"
                        ),
                    ));
                }
            }
            if independent && wanted.len() >= 40 && (kept.is_empty() || kept == wanted) {
                return Err((
                    "P4",
                    format!("all {} independent keys made the same choice", wanted.len()),
                ));
            }
            Ok(())
        }
        Draw::Average => {
            same(&without(twin, "avg"), &without(found, "avg"), false).map_err(|e| ("P5", e))?;
            for value in found.iter().filter_map(|r| r.get("avg")) {
                // The only numeric argument is 1. AVG may be unbound after an
                // error, or 0 for empty input, but never a different mean.
                let numeric = value.split('"').nth(1).and_then(|s| s.parse::<f64>().ok());
                if !matches!(numeric, Some(0.0 | 1.0)) {
                    return Err(("AVG", format!("unexpected mean {value}")));
                }
            }
            Ok(())
        }
    }
}

pub fn explain_count(
    draw: Option<Draw>,
    twin: &Rows,
    scope: Option<&Rows>,
    explained: usize,
) -> bool {
    let Some(Draw::Filter { keys, .. }) = draw else {
        return explained == twin.len();
    };
    // EXPLAIN runs the query again and reports only the total. Enumerate the
    // possible totals of whole evaluations, without comparing separate draws.
    let mut possible = vec![false; twin.len() + 1];
    possible[0] = true;
    let multiplicity = scope.map(|rows| counts(rows, keys));
    for (key, n) in counts(twin, keys) {
        let m = multiplicity
            .as_ref()
            .and_then(|counts| counts.get(&key).copied())
            .unwrap_or(1);
        let copies = n / m;
        for _ in 0..m {
            for total in (copies..possible.len()).rev() {
                possible[total] |= possible[total - copies];
            }
        }
    }
    possible.get(explained).copied().unwrap_or(false)
}
