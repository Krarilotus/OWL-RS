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
// requests served from the cache, and whether EXPLAIN shows a vector search
// that went first: it probes the local group from each hit, so the group's
// scan runs more than once.
fn compare_twin(
    routes: &Routes,
    case: &Case,
    expected: &Rows,
    failures: &mut Vec<Failure>,
) -> (usize, bool) {
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
    let (mut hits, mut probed) = (0, false);
    for route in STORES {
        let store = routes.store(route);
        let cold = routes::execute(store, &request);
        report(failures, Route::Cold(route), "twin bag", check(&cold));
        let before = store.query_cache_stats().hits;
        let warm = routes::execute(store, &request);
        report(failures, Route::Repeat(route), "twin bag", check(&warm));
        hits += usize::from(store.query_cache_stats().hits > before);

        let explanation = routes::explain(store, &request);
        let scans = explanation.steps.iter();
        probed |= scans
            .filter(|s| s.detail.contains("<urn:embedding>"))
            .count()
            > 1;
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
    (hits, probed)
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
    // EXPLAIN executes the drawing request again: its total is an answer,
    // judged as the other routes' row counts are.
    for route in STORES {
        let explained = routes::explain(routes.store(route), &request).rows;
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
}

// The failures, the repeated requests served from the cache, and whether a
// vector search went first.
pub fn check(case: &Case, repeats: usize) -> (Vec<Failure>, usize, bool) {
    let routes = Routes::open(case);
    let expected = routes.native(&case.query.twin(), true);
    let mut failures = Vec::new();
    let (hits, probed) = compare_twin(&routes, case, &expected, &mut failures);
    compare_draws(&routes, case, &expected, repeats, &mut failures);
    (failures, hits, probed)
}

pub fn assert_case(case: &Case) {
    // Twelve runs, as in the probes: a fault in one run of a few is missed.
    let (failures, _, _) = check(case, 12);
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
fn print_reduced(seed: u64, case: Case, failures: &[Failure]) {
    let witness = &failures[0];
    println!(
        "WITNESS {} seed={seed} labels={:?} route={:?} property={} {}",
        case.shape, case.labels, witness.route, witness.property, witness.detail
    );
    for fault in &failures[1..] {
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

// Known faults of the head's base: a draw site, the property its witnesses
// break there, a label they must carry (if any), whether the red check
// requires one, and the reason. Only answers are known: a twin's mismatch
// and EXPLAIN's disagreement with its own execution never are.
const KNOWN: &[(&str, &str, &str, bool, &str)] = &[
    (
        "exists",
        "P4",
        "",
        true,
        "an EXISTS that draws decides once for all the rows it tests",
    ),
    (
        "lateral",
        "P2",
        "",
        true,
        "left rows with the same correlated values share the right side's draws",
    ),
    (
        "service",
        "P1",
        "batched",
        true,
        "SERVICE evaluates its pattern again for each batch of bound values it sends",
    ),
    (
        "service",
        "P3",
        "batched",
        false,
        "SERVICE evaluates its pattern again for each batch of bound values it sends",
    ),
    (
        "aggregate",
        "AVG",
        "",
        false,
        "streamed AVG can evaluate its argument separately for the sum and the count",
    ),
];

fn known(case: &Case, failure: &Failure) -> bool {
    KNOWN.iter().any(|&(site, property, label, _, _)| {
        (site, property) == (case.shape, failure.property)
            && (label.is_empty() || case.labels.contains(&label))
    })
}

// A sweep prints a SEED line for each seed that fails, with the draw site
// and the properties broken there, so a tree's result for every seed is in
// its output. NRESE_PLANNER_ORACLE_EXPECT_RED=1 requires a witness for each
// required known (site, property); a comma-separated list of site:property
// pairs requires one for each of those.
#[test]
fn seeded_queries() {
    let red = std::env::var(EXPECT_RED).ok();
    let Some(seeds) = seeds() else {
        assert!(red.is_none(), "{EXPECT_RED} requires {SEEDS}");
        return;
    };
    let required: Vec<(&str, &str)> = match red.as_deref() {
        None => Vec::new(),
        Some("1") => KNOWN.iter().filter(|k| k.3).map(|k| (k.0, k.1)).collect(),
        Some(pairs) => pairs
            .split(',')
            .map(|pair| pair.split_once(':').expect("expected site:property"))
            .collect(),
    };
    let start = Instant::now();
    let mut checked = BTreeMap::<&str, usize>::new();
    let mut found = BTreeMap::<(&str, &str), usize>::new();
    let mut unknown = BTreeSet::new();
    let (mut hits, mut skipped, mut disjoint, mut probed) = (0, 0, 0, 0);
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
            *found.entry((case.shape, "panic")).or_default() += 1;
            unknown.insert((case.shape, "panic"));
            continue;
        };
        hits += cached;
        if case.labels.contains(&"disjoint") {
            disjoint += 1;
            probed += usize::from(reached);
        }
        if failures.is_empty() {
            continue;
        }
        let pairs: BTreeSet<_> = failures.iter().map(|f| (case.shape, f.property)).collect();
        let properties: Vec<_> = pairs.iter().map(|p| p.1).collect();
        println!(
            "SEED {seed} {} {} labels={:?}",
            case.shape,
            properties.join(","),
            case.labels
        );
        let first = pairs.iter().any(|pair| !found.contains_key(pair));
        for pair in &pairs {
            *found.entry(*pair).or_default() += 1;
        }
        for failure in failures.iter().filter(|f| !known(&case, f)) {
            unknown.insert((case.shape, failure.property));
        }
        if first && catch_unwind(AssertUnwindSafe(|| print_reduced(seed, case, &failures))).is_err()
        {
            println!("RECHECK seed={seed}: the reduction panicked");
        }
    }
    println!(
        "SWEEP {count} seeds {} ms; {hits} cache hits; {skipped} skipped without a vector index; checked {checked:?}; witnesses {found:?}",
        start.elapsed().as_millis()
    );
    if generator::VECTOR_INDEX {
        let reach = format!(
            "{probed} of {disjoint} seeds whose drawing group shares no variable with the hit"
        );
        match probed {
            0 => println!("NOT COVERED vector: EXPLAIN shows the search first in {reach}"),
            _ => println!("VECTOR the search went first by EXPLAIN in {reach}"),
        }
    }
    for (site, property, _, _, reason) in KNOWN {
        match found.get(&(*site, *property)) {
            Some(n) => println!("KNOWN {site} {property}: {n} seeds ({reason})"),
            None => println!("NOT SEEN {site} {property}"),
        }
    }
    if red.is_some() {
        let missing: Vec<_> = required
            .iter()
            .filter(|pair| !found.contains_key(*pair))
            .collect();
        assert!(
            missing.is_empty(),
            "required witnesses missing: {missing:?}"
        );
    } else {
        assert!(
            unknown.is_empty(),
            "witnesses outside the known (site, property) pairs: {unknown:?}"
        );
    }
}
