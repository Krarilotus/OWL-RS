use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::time::Instant;

use nrese_store::SparqlQueryRequest;

use crate::generator::{self, Case};
use crate::properties;
use crate::routes::{self, Routes, Rows, STORES, StoreRoute};
use crate::tree;
use crate::twin::Draw;

const SEEDS: &str = "NRESE_PLANNER_ORACLE_SEEDS";
const EXPECT_RED: &str = "NRESE_PLANNER_ORACLE_EXPECT_RED";

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Route {
    Native(bool),
    Streamed,
    Cold(StoreRoute),
    Repeat(StoreRoute),
    Explain(StoreRoute),
}

#[derive(Debug)]
pub struct Failure {
    route: Route,
    property: &'static str,
    detail: String,
}

impl Failure {
    fn same_kind(&self, other: &Self) -> bool {
        (self.route, self.property) == (other.route, other.property)
    }
}

fn report(
    failures: &mut Vec<Failure>,
    route: Route,
    property: &'static str,
    result: Result<(), String>,
) {
    if let Err(detail) = result {
        failures.push(Failure {
            route,
            property,
            detail,
        });
    }
}

fn compare_twin(
    routes: &Routes,
    request: &SparqlQueryRequest,
    expected: &Rows,
    reduced: bool,
    failures: &mut Vec<Failure>,
) -> usize {
    let check = |rows: &Rows| properties::same(expected, rows, reduced);
    let planned = routes.native(&request.query, false);
    report(failures, Route::Native(false), "twin bag", check(&planned));
    let mut hits = 0;
    for route in STORES {
        let store = routes.store(route);
        let cold = routes::execute(store, request);
        report(failures, Route::Cold(route), "twin bag", check(&cold));
        let before = store.query_cache_stats().hits;
        let warm = routes::execute(store, request);
        report(failures, Route::Repeat(route), "twin bag", check(&warm));
        hits += usize::from(store.query_cache_stats().hits > before);

        let explanation = routes::explain(store, request);
        if !reduced && explanation.rows != cold.len() as u64 {
            let root = explanation
                .steps
                .first()
                .map(|step| (&step.operator, step.rows, &step.detail));
            report(
                failures,
                Route::Explain(route),
                "EXPLAIN rows",
                Err(format!(
                    "explained {}, executed {} (root {root:?})",
                    explanation.rows,
                    cold.len()
                )),
            );
        }
    }
    hits
}

fn compare_draws(
    routes: &Routes,
    case: &Case,
    expected: &Rows,
    repeats: usize,
    failures: &mut Vec<Failure>,
) {
    let Some(draw) = case.query.draw else { return };
    let request = SparqlQueryRequest::all(case.query.real());
    let scope = case
        .query
        .scope
        .as_ref()
        .map(|text| routes.native(text, true));
    let mut judge = |route, run: &mut dyn FnMut() -> Rows| {
        for _ in 0..repeats {
            let Err((property, detail)) = properties::check(draw, expected, &run(), scope.as_ref())
            else {
                continue;
            };
            // A second independent run makes an accidental all-or-none P4
            // witness at most 2^-78 for forty keys.
            if property == "P4"
                && !matches!(
                    properties::check(draw, expected, &run(), scope.as_ref()),
                    Err(("P4", _))
                )
            {
                continue;
            }
            report(failures, route, property, Err(detail));
            break;
        }
    };
    for written in [true, false] {
        judge(Route::Native(written), &mut || {
            routes.native(&request.query, written)
        });
    }
    if matches!(draw, Draw::Average) {
        judge(Route::Streamed, &mut || {
            routes.native_with(&request.query, false, Some(2))
        });
    }
    for route in STORES {
        let store = routes.store(route);
        judge(Route::Cold(route), &mut || {
            store.clear_query_cache();
            routes::execute(store, &request)
        });
        judge(Route::Repeat(route), &mut || {
            routes::execute(store, &request)
        });
    }
    for route in STORES {
        let explanation = routes::explain(routes.store(route), &request);
        if !properties::explain_count(
            Some(draw),
            expected,
            scope.as_ref(),
            explanation.rows as usize,
        ) {
            report(
                failures,
                Route::Explain(route),
                "EXPLAIN rows",
                Err(format!(
                    "explained {} rows, impossible under the twin",
                    explanation.rows
                )),
            );
        }
    }
}

pub fn check(case: &Case, repeats: usize) -> (Vec<Failure>, usize) {
    let routes = Routes::open(case);
    let twin = SparqlQueryRequest::all(case.query.twin());
    let expected = routes.native(&twin.query, true);
    let mut failures = Vec::new();
    let hits = compare_twin(&routes, &twin, &expected, case.query.reduced, &mut failures);
    compare_draws(&routes, case, &expected, repeats, &mut failures);
    (failures, hits)
}

pub fn assert_case(case: &Case) {
    // Twelve runs, as in the probes: a fault in one run of a few is missed.
    let (failures, _) = check(case, 12);
    assert!(
        failures.is_empty(),
        "{}: {}\n{failures:#?}",
        case.shape,
        case.query.real()
    );
}

fn seeds() -> Option<Range<u64>> {
    let text = std::env::var(SEEDS).ok()?;
    let (start, end) = text.split_once("..").expect("expected SEEDS=start..end");
    let start = start.parse().expect("invalid start seed");
    let end = end.parse().expect("invalid end seed");
    assert!(start < end, "the seed range must be non-empty");
    Some(start..end)
}

// Remove independently parseable data chunks while preserving the same
// failure on the same route. Inspect the exact reduced case again at the end.
fn reduce(mut case: Case, witness: &Failure) -> Case {
    let mut granularity = 2;
    let mut attempts = 0;
    while !case.data.is_empty() && attempts < 24 {
        let chunk = case.data.len().div_ceil(granularity);
        let mut reduced = false;
        for start in (0..case.data.len()).step_by(chunk) {
            attempts += 1;
            let mut trial = case.clone();
            let end = (start + chunk).min(trial.data.len());
            trial.data.drain(start..end);
            let (failures, _) = check(&trial, 4);
            if failures.iter().any(|f| witness.same_kind(f)) {
                case = trial;
                granularity = granularity.saturating_sub(1).max(2);
                reduced = true;
                break;
            }
            if attempts >= 24 {
                break;
            }
        }
        if !reduced {
            if granularity >= case.data.len() {
                break;
            }
            granularity = (2 * granularity).min(case.data.len());
        }
    }
    case
}

fn print_reduced(seed: Option<u64>, case: Case, failures: &[Failure]) {
    let Some(witness) = failures.first() else {
        return;
    };
    println!(
        "WITNESS {} seed={seed:?} labels={:?} route={:?} property={} {}",
        case.shape, case.labels, witness.route, witness.property, witness.detail
    );
    for fault in failures.iter().skip(1) {
        println!(
            "ALSO {} {:?} {} {}",
            case.shape, fault.route, fault.property, fault.detail
        );
    }
    // Reduce the dataset, keeping the same query and thus the same draw site.
    let reduced = reduce(case, witness);
    println!(
        "QUERY {} ({} statements) {}",
        reduced.shape,
        reduced.data.len(),
        reduced.query.real()
    );
}

// Places where the base is known to draw against SPARQL's count of
// evaluations, each with whether the red check requires a witness there, the
// properties it breaks, and the reason. A witness there that breaks only those
// (and EXPLAIN's count, which runs the same plan) is reported and counted,
// but does not fail an ordinary sweep.
const KNOWN: &[(&str, bool, &[&str], &str)] = &[
    (
        "exists",
        true,
        &["P4"],
        "an EXISTS that draws decides once for all the rows it tests",
    ),
    (
        "lateral",
        true,
        &["P2"],
        "left rows with the same correlated values share the right side's draws",
    ),
    (
        "service",
        true,
        &["P1"],
        "SERVICE evaluates its pattern again for each batch of bound values it sends",
    ),
    (
        "aggregate",
        false,
        &["AVG"],
        "streamed AVG can evaluate its argument separately for the sum and the count",
    ),
];

fn known(case: &Case, failures: &[Failure]) -> bool {
    KNOWN.iter().any(|(place, _, properties, _)| {
        *place == case.shape
            && failures
                .iter()
                .all(|f| properties.contains(&f.property) || f.property == "EXPLAIN rows")
    })
}

// NRESE_PLANNER_ORACLE_EXPECT_RED=1 requires a witness at each required known
// place; a comma-separated list of labels requires one at each of those.
#[test]
fn seeded_queries() {
    let red = std::env::var(EXPECT_RED).ok();
    let Some(seeds) = seeds() else {
        assert!(red.is_none(), "{EXPECT_RED} requires {SEEDS}");
        return;
    };
    let required: Vec<&str> = match red.as_deref() {
        None => Vec::new(),
        Some("1") => KNOWN.iter().filter(|k| k.1).map(|k| k.0).collect(),
        Some(labels) => labels.split(',').collect(),
    };
    let start = Instant::now();
    let mut checked = BTreeMap::<&str, usize>::new();
    let mut found = BTreeMap::<&str, usize>::new();
    let mut unknown = BTreeSet::new();
    let (mut hits, mut skipped) = (0, 0);
    let count = seeds.end - seeds.start;
    for seed in seeds {
        let case = tree::generated(seed);
        if case.vector && !generator::VECTOR_INDEX {
            skipped += 1;
            continue;
        }
        for label in &case.labels {
            *checked.entry(label).or_default() += 1;
        }
        let (failures, cached) = check(&case, 1);
        hits += cached;
        if failures.is_empty() {
            continue;
        }
        let first = case.labels.iter().any(|l| !found.contains_key(l));
        for label in &case.labels {
            *found.entry(label).or_default() += 1;
        }
        if !known(&case, &failures) {
            unknown.insert(case.shape);
        }
        if first {
            print_reduced(Some(seed), case, &failures);
        }
    }
    println!(
        "SWEEP {count} seeds {} ms; {hits} cache hits; {skipped} skipped without a vector index; checked {checked:?}; witnesses {found:?}",
        start.elapsed().as_millis()
    );
    for (place, _, _, reason) in KNOWN {
        match found.get(place) {
            Some(n) => println!("KNOWN {place}: {n} witnesses ({reason})"),
            None => println!("NOT SEEN {place}"),
        }
    }
    if red.is_some() {
        let missing: Vec<_> = required
            .iter()
            .filter(|l| !found.contains_key(*l))
            .collect();
        assert!(
            missing.is_empty(),
            "required witnesses missing: {missing:?}"
        );
    } else {
        assert!(
            unknown.is_empty(),
            "witnesses outside the known places: {unknown:?}"
        );
    }
}
