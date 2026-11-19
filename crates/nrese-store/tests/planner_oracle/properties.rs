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
    // Only rows that passed the drawing site carry its value; rows from a
    // UNION arm without the site carry none and are not the site's.
    let mut groups = BTreeMap::<_, BTreeSet<_>>::new();
    for row in rows {
        if let Some(value) = row.get(column) {
            groups
                .entry(key(row, keys))
                .or_default()
                .insert(value.as_str());
        }
    }
    groups
}

// The rows that descend from no evaluation of the site (no key column bound,
// as in another UNION arm), and the others by key.
fn split(rows: &Rows, keys: &[&str]) -> (Bag, BTreeMap<Key, Bag>) {
    let (mut outside, mut inside) = (Bag::new(), BTreeMap::<Key, Bag>::new());
    for row in rows {
        let bag = if !keys.is_empty() && keys.iter().all(|k| !row.contains_key(*k)) {
            &mut outside
        } else {
            inside.entry(key(row, keys)).or_default()
        };
        *bag.entry(row.clone()).or_default() += 1;
    }
    (outside, inside)
}

// Properties whose witness is an outcome of small chance under SPARQL: a
// second run must give it too.
pub const RARE: &[&str] = &["P4", "AVG unbound"];

fn number(value: Option<&String>) -> Option<f64> {
    value?.split('"').nth(1)?.parse().ok()
}

// Beside a drawing AVG, a mean whose volatile argument is 1 on both branches:
// SPARQL binds it to 1 in every group.
pub fn bound_one(rows: &Rows) -> Result<(), String> {
    match rows.iter().find(|r| number(r.get("bound")) != Some(1.0)) {
        Some(row) => Err(format!("?bound is not 1 in {row:?}")),
        None => Ok(()),
    }
}

// What SPARQL allows at a drawing site, taken from the twin.
pub struct Model {
    draw: Draw,
    twin: Rows,
    scope: Option<Counts>,
    outside: Bag,
    // Per key of a drawing filter: its evaluations, the rows one evaluation
    // leaves in the result, and the body solutions that may each pass a draw.
    evaluations: BTreeMap<Key, (usize, Bag, usize)>,
}

impl Model {
    pub fn new(draw: Draw, twin: &Rows, scope: Option<&Rows>, body: Option<&Rows>) -> Self {
        let keys = match draw {
            Draw::Value { keys, .. } | Draw::Filter { keys } => keys,
            Draw::Average => &[],
        };
        let scope = scope.map(|rows| counts(rows, keys));
        let body = body.map(|rows| counts(rows, keys));
        let (outside, inside) = match draw {
            Draw::Filter { .. } => split(twin, keys),
            _ => Default::default(),
        };
        let evaluations = inside
            .into_iter()
            .map(|(key, rows)| {
                let m = scope
                    .as_ref()
                    .map_or(1, |c| c.get(&key).copied().unwrap_or(0));
                let n = body
                    .as_ref()
                    .map_or(m, |c| c.get(&key).copied().unwrap_or(0));
                assert!(
                    m > 0
                        && n >= m
                        && n.is_multiple_of(m)
                        && rows.values().all(|c| c.is_multiple_of(m)),
                    "uneven copies in the fixture for {key:?}"
                );
                let one = rows.into_iter().map(|(row, c)| (row, c / m)).collect();
                (key, (m, one, n / m))
            })
            .collect();
        Self {
            draw,
            twin: twin.clone(),
            scope,
            outside,
            evaluations,
        }
    }

    // Per key: its evaluations, how many of them kept their rows, and the
    // body solutions per evaluation; or why the rows are not whole copies.
    fn kept(&self, found: &Rows, keys: &[&str]) -> Result<Vec<(usize, usize, usize)>, String> {
        let (outside, inside) = split(found, keys);
        if outside != self.outside {
            return Err("the rows outside the drawing site are not the twin's".into());
        }
        if let Some(key) = inside.keys().find(|k| !self.evaluations.contains_key(*k)) {
            return Err(format!("key {key:?} is not the twin's"));
        }
        let mut kept = Vec::new();
        for (key, (m, one, n)) in &self.evaluations {
            let rows = inside.get(key).cloned().unwrap_or_default();
            let size: usize = one.values().sum();
            let j = rows.values().sum::<usize>() / size;
            let copies: Bag = one.iter().map(|(row, c)| (row.clone(), c * j)).collect();
            if j > *m || rows != copies.into_iter().filter(|(_, c)| *c > 0).collect() {
                return Err(format!(
                    "key {key:?}: the kept rows are not whole copies of one evaluation's {size}"
                ));
            }
            kept.push((*m, j, *n));
        }
        Ok(kept)
    }

    pub fn check(&self, found: &Rows) -> Result<(), (&'static str, String)> {
        let twin = &self.twin;
        match self.draw {
            Draw::Value { column, keys, .. } => {
                same(&without(twin, column), &without(found, column), false)
                    .map_err(|e| ("P5", e))?;
                let drawn = |rows: &Rows| rows.iter().filter(|r| r.contains_key(column)).count();
                if drawn(found) != drawn(twin) {
                    return Err((
                        "P1",
                        format!(
                            "{} rows carry a drawn ?{column}, the twin's {}",
                            drawn(found),
                            drawn(twin)
                        ),
                    ));
                }
                // Each call gives a fresh value: exactly one per evaluation.
                // Distinct stand-ins include the site, so the two UNION arms
                // may each draw for the same projected key.
                let found = values(found, column, keys);
                let mut owner = BTreeMap::new();
                for (key, sites) in values(twin, column, keys) {
                    let m = self
                        .scope
                        .as_ref()
                        .and_then(|counts| counts.get(&key).copied())
                        .unwrap_or(sites.len());
                    let drawn = found.get(&key).cloned().unwrap_or_default();
                    if drawn.len() != m {
                        return Err((
                            "P1",
                            format!(
                                "key {key:?}: {} distinct draws, SPARQL gives {m}",
                                drawn.len()
                            ),
                        ));
                    }
                    for value in drawn {
                        if let Some(first) = owner.insert(value, key.clone()) {
                            return Err((
                                "P2",
                                format!("one draw shared by {first:?} and {key:?}"),
                            ));
                        }
                    }
                }
                Ok(())
            }
            Draw::Filter { keys } => {
                let kept = self.kept(found, keys).map_err(|e| ("P3", e))?;
                // The chance under SPARQL that every evaluation made the
                // choice it did, if all made the same.
                let all = |f: fn(&(usize, usize, usize)) -> bool| kept.iter().all(f);
                let log2 = if kept.is_empty() {
                    0.0
                } else if all(|&(m, j, _)| j == m) {
                    let pass = |n: usize| (1.0 - 0.5f64.powi(n.min(1000) as i32)).log2();
                    kept.iter().map(|&(m, _, n)| m as f64 * pass(n)).sum()
                } else if all(|&(_, j, _)| j == 0) {
                    -kept.iter().map(|&(m, _, n)| (m * n) as f64).sum::<f64>()
                } else {
                    0.0
                };
                if log2 < -20.0 {
                    let evaluations: usize = kept.iter().map(|k| k.0).sum();
                    return Err((
                        "P4",
                        format!(
                            "all {evaluations} evaluations made the same choice: 2^{log2:.1} under SPARQL"
                        ),
                    ));
                }
                Ok(())
            }
            Draw::Average => {
                bound_one(found).map_err(|e| ("AVG bound", e))?;
                same(&without(twin, "avg"), &without(found, "avg"), false)
                    .map_err(|e| ("P5", e))?;
                for value in found.iter().filter_map(|r| r.get("avg")) {
                    // The only numeric argument is 1. AVG may be unbound after
                    // an error, or 0 for empty input, but never another mean.
                    if !matches!(number(Some(value)), Some(0.0 | 1.0)) {
                        return Err(("AVG", format!("unexpected mean {value}")));
                    }
                }
                // A group's mean is bound when all its n arguments are 1, a
                // chance of 2^-n each; every mean unbound has the chance of
                // the product of 1 - 2^-n over the groups (n from ?rows).
                if found.iter().all(|r| !r.contains_key("avg")) {
                    let unbound = |r: &Row| {
                        let n = number(r.get("rows")).unwrap_or(0.0) as i32;
                        (1.0 - 0.5f64.powi(n.min(1000))).log2()
                    };
                    let log2: f64 = found.iter().map(unbound).sum();
                    if log2 < -20.0 {
                        let groups = found.len();
                        let detail =
                            format!("all {groups} means unbound: 2^{log2:.1} under SPARQL");
                        return Err(("AVG unbound", detail));
                    }
                }
                Ok(())
            }
        }
    }

    // EXPLAIN runs the query again and reports only its total: one the
    // site's evaluations can give, each keeping or dropping all its rows.
    pub fn explains(&self, explained: usize) -> bool {
        if !matches!(self.draw, Draw::Filter { .. }) {
            return explained == self.twin.len();
        }
        let mut possible = vec![false; self.twin.len() + 1];
        possible[self.outside.values().sum::<usize>()] = true;
        for (m, one, _) in self.evaluations.values() {
            let size: usize = one.values().sum();
            for _ in 0..*m {
                for total in (size..possible.len()).rev() {
                    possible[total] |= possible[total - size];
                }
            }
        }
        possible.get(explained).copied().unwrap_or(false)
    }

    pub fn property(&self) -> &'static str {
        match self.draw {
            Draw::Filter { .. } => "P3",
            _ => "P5",
        }
    }
}
