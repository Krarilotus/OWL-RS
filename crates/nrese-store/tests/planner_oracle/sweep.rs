use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::time::Instant;

use nrese_store::SparqlQueryRequest;

use crate::generator::{self, Case, PROBE_SHAPES, RED_SHAPES};
use crate::properties;
use crate::routes::{self, Routes, Rows, STORES, StoreRoute};
use crate::twin::Draw;

const SEEDS: &str = "NRESE_PLANNER_ORACLE_SEEDS";
const EXPECT_RED: &str = "NRESE_PLANNER_ORACLE_EXPECT_RED";
const REPLAY: &str = "NRESE_PLANNER_ORACLE_REPLAY";

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
    let (failures, _) = check(case, 2);
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
        "WITNESS {} seed={seed:?} route={:?} property={} {}",
        case.shape, witness.route, witness.property, witness.detail
    );
    for fault in failures.iter().skip(1) {
        println!(
            "ALSO {} {:?} {} {}",
            case.shape, fault.route, fault.property, fault.detail
        );
    }
    // Query shapes are built as small templates. Reduce the dataset for
    // each new shape, keeping the same query and thus the same draw site.
    let reduced = reduce(case, witness);
    println!("QUERY {} {}", reduced.shape, reduced.query.real());
    let fixture = (reduced.shape, &reduced.data, &reduced.query.text);
    println!("FIXTURE {}", serde_json::to_string(&fixture).unwrap());
    if let Some(remote) = reduced.remote {
        println!("REMOTE {} {remote}", reduced.shape);
    }
}

#[test]
fn seeded_queries() {
    let expect_red = std::env::var(EXPECT_RED).as_deref() == Ok("1");
    let Some(seeds) = seeds() else {
        assert!(!expect_red, "{EXPECT_RED}=1 requires {SEEDS}");
        return;
    };
    let start = Instant::now();
    let mut checked = BTreeMap::new();
    let mut witnesses = BTreeSet::new();
    let mut hits = 0;
    let count = seeds.end - seeds.start;
    for seed in seeds {
        let case = if expect_red {
            generator::drawing(RED_SHAPES[(seed % RED_SHAPES.len() as u64) as usize])
        } else {
            generator::generated(seed)
        };
        *checked.entry(case.shape).or_insert(0) += 1;
        let (failures, cached) = check(&case, if expect_red { 12 } else { 1 });
        hits += cached;
        let found_draw = failures
            .iter()
            .any(|f| matches!(f.property, "P1" | "P2" | "P3" | "P4" | "P5"));
        if (!expect_red && !failures.is_empty() || expect_red && found_draw)
            && witnesses.insert(case.shape)
        {
            print_reduced(Some(seed), case, &failures);
        }
    }
    println!(
        "SWEEP {count} seeds {} ms; {hits} cache hits; shapes {checked:?}; witnesses {witnesses:?}",
        start.elapsed().as_millis()
    );
    if expect_red {
        let missing: Vec<_> = RED_SHAPES
            .iter()
            .filter(|shape| !witnesses.contains(**shape))
            .collect();
        assert!(
            missing.is_empty(),
            "required drawing witnesses missing: {missing:?}"
        );
    } else {
        assert!(
            witnesses.is_empty(),
            "query route or property differences: {witnesses:?}"
        );
    }
}

#[test]
#[ignore = "revision comparison; run explicitly"]
fn drawing_revision_probes() {
    for &shape in PROBE_SHAPES {
        let case = generator::drawing(shape);
        let (failures, _) = check(&case, 12);
        if failures.is_empty() {
            println!("NOT SEEN {shape}");
        } else {
            print_reduced(None, case, &failures);
        }
    }
}

#[test]
#[ignore = "revision comparison; run explicitly with a sweep log"]
fn replay_drawing_witnesses() {
    let log = std::fs::read_to_string(std::env::var(REPLAY).expect("replay log path")).unwrap();
    let mut replays = 0;
    for fixture in log.lines().filter_map(|line| line.strip_prefix("FIXTURE ")) {
        let (shape, data, text): (&str, Vec<String>, String) =
            serde_json::from_str(fixture).unwrap();
        let shape = *RED_SHAPES
            .iter()
            .chain(PROBE_SHAPES)
            .find(|s| **s == shape)
            .expect("replay a named drawing witness");
        let mut case = generator::drawing(shape);
        case.data = data;
        case.query.text = text;
        let (failures, _) = check(&case, 24);
        println!("REPLAY {shape} {failures:?}");
        replays += 1;
    }
    assert!(replays > 0, "no named drawing witness to replay");
}
