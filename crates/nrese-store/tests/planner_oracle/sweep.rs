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
const CELLS_RED: &str = "NRESE_PLANNER_ORACLE_CELLS_RED";
const RECORD: &str = "NRESE_PLANNER_ORACLE_RECORD";

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Route {
    Native(bool),
    Streamed,
    Cold(StoreRoute),
    Repeat(StoreRoute),
}

// The query a failure is about: the drawing one, or its deterministic twin.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Request {
    Real,
    Twin,
}

// What failed: the rows a route answered, or EXPLAIN's report of its run
// (on a store, from a cleared cache).
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Kind {
    Answer,
    Explain,
}

#[derive(Debug)]
pub struct Failure {
    request: Request,
    kind: Kind,
    route: Route,
    property: &'static str,
    detail: String,
}

impl Failure {
    fn same_kind(&self, other: &Self) -> bool {
        self.label() == other.label() && self.property == other.property
    }

    fn label(&self) -> String {
        format!("{:?} {:?} {:?}", self.request, self.kind, self.route)
    }

    // Whether it can meet a red requirement: the real query's answer on
    // native planned, or on a store route cold or repeated. As written and
    // streamed never do.
    fn red(&self) -> bool {
        self.request == Request::Real
            && self.kind == Kind::Answer
            && matches!(
                self.route,
                Route::Native(false) | Route::Cold(_) | Route::Repeat(_)
            )
    }
}

struct Report<'a>(&'a mut Vec<Failure>, Request);

impl Report<'_> {
    fn answer(&mut self, route: Route, property: &'static str, result: Result<(), String>) {
        if let Err(detail) = result {
            let (request, kind) = (self.1, Kind::Answer);
            self.0.push(Failure {
                request,
                kind,
                route,
                property,
                detail,
            });
        }
    }

    // EXPLAIN runs on a store from a cleared cache.
    fn explain(&mut self, store: StoreRoute, property: &'static str, detail: String) {
        let (request, kind, route) = (self.1, Kind::Explain, Route::Cold(store));
        self.0.push(Failure {
            request,
            kind,
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
    let mut report = Report(failures, Request::Twin);
    let request = SparqlQueryRequest::all(case.query.twin());
    let reduced = case.query.reduced;
    let check = |rows: &Rows| properties::same(expected, rows, reduced);
    let planned = routes.native(&request.query, false);
    report.answer(Route::Native(false), "twin bag", check(&planned));
    if matches!(case.query.draw, Some(Draw::Average)) {
        // The mean beside the draw never errs: bound to 1, as written too.
        let bound = properties::bound_one(expected);
        report.answer(Route::Native(true), "AVG bound", bound);
        let streamed = routes.native_with(&request.query, false, Some(2));
        report.answer(Route::Streamed, "twin bag", check(&streamed));
    }
    let mut hits = 0;
    for route in STORES {
        let store = routes.store(route);
        let cold = routes::execute(store, &request);
        report.answer(Route::Cold(route), "twin bag", check(&cold));
        let before = store.query_cache_stats().hits;
        let warm = routes::execute(store, &request);
        report.answer(Route::Repeat(route), "twin bag", check(&warm));
        hits += usize::from(store.query_cache_stats().hits > before);

        let explanation = routes::explain(store, &request);
        if !reduced && explanation.rows != cold.len() as u64 {
            let root = explanation
                .steps
                .first()
                .map(|s| (&s.operator, s.rows, &s.detail));
            let (explained, executed) = (explanation.rows, cold.len());
            let detail = format!("explained {explained}, executed {executed} (root {root:?})");
            report.explain(route, "EXPLAIN rows", detail);
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
    let mut report = Report(failures, Request::Real);
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
            if !properties::RARE.contains(&property) {
                report.answer(route, property, Err(detail));
                break;
            }
            // An outcome of chance at most 2^-20 under SPARQL is a witness
            // when an independent second run gives it too. Whatever else
            // the second run breaks is a failure of its own.
            match model.check(&run()) {
                Ok(()) => continue,
                Err((again, more)) if again == property => {
                    let detail = format!("{detail}; again: {more}");
                    report.answer(route, property, Err(detail));
                }
                Err((other, more)) => {
                    let detail =
                        format!("{more} (the run after an unconfirmed {property}: {detail})");
                    report.answer(route, other, Err(detail));
                }
            }
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
    // against the totals SPARQL allows, as an EXPLAIN failure.
    let mut reached = Vec::new();
    for route in STORES {
        let explanation = routes::explain(routes.store(route), &request);
        let steps = explanation.steps.iter();
        let scans = steps.filter(|s| s.detail.contains("<urn:embedding>"));
        reached.extend((scans.count() > 1).then_some(route));
        let explained = explanation.rows;
        if !model.explains(explained as usize) {
            let detail = format!("explained {explained} rows, a total SPARQL cannot give");
            report.explain(route, model.property(), detail);
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

fn print_cell(shape: &str, failures: &[Failure]) {
    for f in failures {
        println!("CELL {shape} {} {}", f.label(), f.property);
    }
}

pub fn assert_case(case: &Case) {
    // Twelve runs, as in the probes: a fault in one run of a few is missed.
    let (failures, _, _) = check(case, 12);
    print_cell(case.shape, &failures);
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

// A red requirement: comma-separated place:property pairs.
fn pairs(text: &str) -> Vec<(&str, &str)> {
    text.split(',')
        .map(|pair| pair.split_once(':').expect("expected place:property"))
        .collect()
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
fn print_reduced(seed: u64, case: Case, witness: &Failure) {
    println!(
        "WITNESS {} seed={seed} labels={:?} {} property={} {}",
        case.shape,
        case.labels,
        witness.label(),
        witness.property,
        witness.detail
    );
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
        "RECHECK seed={seed} {} {} {}: {}",
        reduced.shape,
        witness.label(),
        witness.property,
        if again { "fails again" } else { "passed" }
    );
}

// NRESE_PLANNER_ORACLE_CELLS_RED, comma-separated shape:property pairs of
// named cells: each must break that property in the real query's answer on
// a red route. Without it, the test does nothing.
#[test]
fn named_cells_red() {
    let Ok(text) = std::env::var(CELLS_RED) else {
        return;
    };
    let mut missing = Vec::new();
    for (shape, property) in pairs(&text) {
        let shape: &'static str = Box::leak(shape.to_owned().into_boxed_str());
        let (failures, _, _) = check(&generator::drawing(shape), 12);
        print_cell(shape, &failures);
        if !failures.iter().any(|f| f.red() && f.property == property) {
            missing.push((shape, property));
        }
    }
    assert!(
        missing.is_empty(),
        "named cells without their red on a red route: {missing:?}"
    );
}

// A sweep prints a SEED line for each seed that fails, with the draw site
// and the properties broken there, so a tree's result for every seed is in
// its output. It counts witnesses by site, property, query, kind and route,
// and fails on any. NRESE_PLANNER_ORACLE_EXPECT_RED (site:property pairs)
// instead requires each pair from a red witness. NRESE_PLANNER_ORACLE_RECORD
// only records: for typing on another tree.
#[test]
fn seeded_queries() {
    let red = std::env::var(EXPECT_RED).ok();
    let record = std::env::var(RECORD).is_ok();
    let Some(seeds) = seeds() else {
        assert!(red.is_none(), "{EXPECT_RED} requires {SEEDS}");
        return;
    };
    let required = red.as_deref().map(pairs).unwrap_or_default();
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
        // Each triple this seed brings first gets its own reduced witness.
        let mut new = Vec::new();
        for failure in &failures {
            let key = (case.shape, failure.property, failure.label());
            if failure.red() {
                answered.insert((case.shape, failure.property));
            }
            let count = found.entry(key).or_default();
            if *count == 0 {
                new.push(failure);
            }
            *count += 1;
        }
        for witness in new {
            let reduced = || print_reduced(seed, case.clone(), witness);
            if catch_unwind(AssertUnwindSafe(reduced)).is_err() {
                println!("RECHECK seed={seed}: the reduction panicked");
            }
        }
    }
    println!(
        "SWEEP {count} seeds {} ms; {hits} cache hits; {skipped} skipped without a vector index; checked {checked:?}",
        start.elapsed().as_millis()
    );
    for ((site, property, label), n) in &found {
        println!("WITNESSES {site} {property} {label}: {n}");
    }
    if generator::VECTOR_INDEX {
        // Seeds whose drawing request's own EXPLAIN shows the search first,
        // per store route, of those whose group shares no variable with the hit.
        let head = ["VECTOR", "NOT COVERED vector:"][usize::from(reach.is_empty())];
        println!("{head} search first by EXPLAIN in {reach:?} of {disjoint} seeds");
    }
    if red.is_some() {
        let missing: Vec<_> = required
            .iter()
            .filter(|pair| !answered.contains(*pair))
            .collect();
        assert!(
            missing.is_empty(),
            "required witnesses missing on a red route: {missing:?}"
        );
    } else if !record {
        assert!(found.is_empty(), "witnesses: {:?}", found.keys());
    }
}
