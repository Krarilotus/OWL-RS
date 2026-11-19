use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Instant;

use nrese_store::SparqlQueryRequest;

use crate::generator::{self, Case};
use crate::properties::{self, Model};
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

// The twin on every route, and EXPLAIN of the same request on each store,
// which must report the rows its execution gives. Returns the repeated
// requests served from the cache.
fn compare_twin(
    routes: &Routes,
    case: &Case,
    expected: &Rows,
    failures: &mut Vec<Failure>,
) -> usize {
    let request = SparqlQueryRequest::all(case.query.twin());
    let reduced = case.query.reduced;
    let check = |rows: &Rows| properties::same(expected, rows, reduced);
    let planned = routes.native(&request.query, false);
    report(failures, Route::Native(false), "twin bag", check(&planned));
    if matches!(case.query.draw, Some(Draw::Average)) {
        // An argument that never errs gives a bound mean, streamed too.
        let streamed = routes.native_with(&request.query, false, Some(2));
        report(failures, Route::Streamed, "twin bag", check(&streamed));
    }
    let mut hits = 0;
    for route in STORES {
        let store = routes.store(route);
        let cold = routes::execute(store, &request);
        report(failures, Route::Cold(route), "twin bag", check(&cold));
        let before = store.query_cache_stats().hits;
        let warm = routes::execute(store, &request);
        report(failures, Route::Repeat(route), "twin bag", check(&warm));
        hits += usize::from(store.query_cache_stats().hits > before);

        let explanation = routes::explain(store, &request);
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

// Judges the drawing request on every answer route. Returns the store
// routes on which its own EXPLAIN shows a vector search that went first: it
// probes the local group from each hit, so the group's scan runs more than
// once.
fn compare_draws(
    routes: &Routes,
    case: &Case,
    expected: &Rows,
    repeats: usize,
    failures: &mut Vec<Failure>,
) -> Vec<StoreRoute> {
    let Some(draw) = case.query.draw else {
        return Vec::new();
    };
    let request = SparqlQueryRequest::all(case.query.real());
    let scope = |body: &str| {
        let text = case.query.scope.as_ref()?;
        Some(routes.native(&text.replace("$BODY", body), true))
    };
    let body = case.query.body.as_ref().and_then(|body| scope(body));
    let model = Model::new(draw, expected, scope("").as_ref(), body.as_ref());
    let mut judge = |route, run: &mut dyn FnMut() -> Rows| {
        for _ in 0..repeats {
            let Err((property, detail)) = model.check(&run()) else {
                continue;
            };
            // An outcome of chance at most 2^-20 under SPARQL is a witness
            // only if an independent second run gives one too.
            if property == "P4" && !matches!(model.check(&run()), Err(("P4", _))) {
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
    // EXPLAIN executes the drawing request again: its total is checked
    // against the totals SPARQL allows. It never stands for an answer route.
    let mut reached = Vec::new();
    for route in STORES {
        let explanation = routes::explain(routes.store(route), &request);
        let scans = explanation.steps.iter();
        if scans
            .filter(|s| s.detail.contains("<urn:embedding>"))
            .count()
            > 1
        {
            reached.push(route);
        }
        let explained = explanation.rows;
        if !model.explains(explained as usize) {
            let detail = format!("explained {explained} rows, a total SPARQL cannot give");
            report(
                failures,
                Route::Explain(route),
                model.property(),
                Err(detail),
            );
        }
    }
    reached
}

// The failures, the repeated requests served from the cache, and the store
// routes on which the drawing request's vector search went first.
pub fn check(case: &Case, repeats: usize) -> (Vec<Failure>, usize, Vec<StoreRoute>) {
    let routes = Routes::open(case);
    let expected = routes.native(&case.query.twin(), true);
    let mut failures = Vec::new();
    let hits = compare_twin(&routes, case, &expected, &mut failures);
    let reached = compare_draws(&routes, case, &expected, repeats, &mut failures);
    (failures, hits, reached)
}

pub fn assert_case(case: &Case) {
    // Twelve runs, as in the probes: a fault in one run of a few is missed.
    let (failures, _, _) = check(case, 12);
    // One line per failure, for the red driver's record of each cell.
    for f in &failures {
        println!("CELL {} {:?} {}", case.shape, f.route, f.property);
    }
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
// failure on the same route.
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
            let (failures, _, _) = check(&trial, 4);
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

// The witness, reduced to a case that can be typed from this output alone:
// its query, data and remote data, checked again as a cell would.
fn print_reduced(seed: u64, case: Case, failures: &[Failure], witness: &Failure) {
    println!(
        "WITNESS {} seed={seed} labels={:?} route={:?} property={} {}",
        case.shape, case.labels, witness.route, witness.property, witness.detail
    );
    for fault in failures.iter().filter(|f| !std::ptr::eq(*f, witness)) {
        println!("ALSO {:?} {} {}", fault.route, fault.property, fault.detail);
    }
    let reduced = reduce(case, witness);
    println!("QUERY {}", reduced.query.real());
    for statement in &reduced.data {
        println!("DATA {statement}");
    }
    if let Some(remote) = reduced.remote {
        println!("REMOTE {remote}");
    }
    let (again, _, _) = check(&reduced, 12);
    let again = again.iter().any(|f| witness.same_kind(f));
    println!(
        "RECHECK seed={seed} {} {:?} {}: {}",
        reduced.shape,
        witness.route,
        witness.property,
        if again { "fails again" } else { "passed" }
    );
}

// A sweep prints a SEED line for each seed that fails, with the draw site
// and the properties broken there, so a tree's result for every seed is in
// its output. It counts witnesses by (site, property, route) and excuses
// none. NRESE_PLANNER_ORACLE_EXPECT_RED, a comma-separated list of
// site:property pairs, requires a witness for each on an answer route (not
// EXPLAIN alone) instead of failing on witnesses.
#[test]
fn seeded_queries() {
    let red = std::env::var(EXPECT_RED).ok();
    let Some(seeds) = seeds() else {
        assert!(red.is_none(), "{EXPECT_RED} requires {SEEDS}");
        return;
    };
    let required: Vec<(&str, &str)> = red
        .iter()
        .flat_map(|pairs| pairs.split(','))
        .map(|pair| pair.split_once(':').expect("expected site:property"))
        .collect();
    let start = Instant::now();
    let mut checked = BTreeMap::<&str, usize>::new();
    let mut found = BTreeMap::<(&str, &str, String), usize>::new();
    let mut answered = BTreeSet::<(&str, &str)>::new();
    let mut reach = BTreeMap::<String, usize>::new();
    let (mut hits, mut skipped, mut disjoint) = (0, 0, 0);
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
        let Ok((failures, cached, reached)) = catch_unwind(AssertUnwindSafe(|| check(&case, 1)))
        else {
            println!("SEED {seed} {} panic labels={:?}", case.shape, case.labels);
            *found.entry((case.shape, "panic", "-".into())).or_default() += 1;
            continue;
        };
        hits += cached;
        if case.labels.contains(&"disjoint") {
            disjoint += 1;
            for route in reached {
                *reach.entry(format!("{route:?}")).or_default() += 1;
            }
        }
        if failures.is_empty() {
            continue;
        }
        let properties: BTreeSet<_> = failures.iter().map(|f| f.property).collect();
        let properties: Vec<_> = properties.into_iter().collect();
        println!(
            "SEED {seed} {} {} labels={:?}",
            case.shape,
            properties.join(","),
            case.labels
        );
        let mut new = None;
        for failure in &failures {
            let key = (case.shape, failure.property, format!("{:?}", failure.route));
            if !found.contains_key(&key) && new.is_none() {
                new = Some(failure);
            }
            if !matches!(failure.route, Route::Explain(_)) {
                answered.insert((case.shape, failure.property));
            }
        }
        let keys: BTreeSet<_> = failures
            .iter()
            .map(|f| (case.shape, f.property, format!("{:?}", f.route)))
            .collect();
        for key in keys {
            *found.entry(key).or_default() += 1;
        }
        // Reduce and recheck the failure that brought a new triple.
        let Some(witness) = new else { continue };
        let reduced = || print_reduced(seed, case.clone(), &failures, witness);
        if catch_unwind(AssertUnwindSafe(reduced)).is_err() {
            println!("RECHECK seed={seed}: the reduction panicked");
        }
    }
    println!(
        "SWEEP {count} seeds {} ms; {hits} cache hits; {skipped} skipped without a vector index; checked {checked:?}",
        start.elapsed().as_millis()
    );
    for ((site, property, route), n) in &found {
        println!("WITNESSES {site} {property} {route}: {n} seeds");
    }
    if generator::VECTOR_INDEX {
        let of = format!("of {disjoint} seeds whose drawing group shares no variable with the hit");
        if reach.is_empty() {
            println!(
                "NOT COVERED vector: no drawing request's EXPLAIN shows the search first, {of}"
            );
        } else {
            println!(
                "VECTOR the drawing request's own EXPLAIN shows the search first: {reach:?} {of}"
            );
        }
    }
    if red.is_some() {
        let missing: Vec<_> = required
            .iter()
            .filter(|pair| !answered.contains(*pair))
            .collect();
        assert!(
            missing.is_empty(),
            "required witnesses missing on an answer route: {missing:?}"
        );
    } else {
        assert!(found.is_empty(), "witnesses: {:?}", found.keys());
    }
}
