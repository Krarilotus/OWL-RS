use std::collections::BTreeMap;

use super::case::Case;
use super::draw;
use super::gaps::EntryPoint;
use super::observe::{Deviation, compare};
use super::stops::Outcome;

/// Draws per invocation; each seed draws different cases.
const DRAWS: u64 = 64;
/// Restriction for the known gap `shapes-change-repeats-shared-result`, removed with it: a
/// draw whose shapes change introduces one result more than once is left out.
const SHAPES_CHANGES_MAY_REPEAT: bool = false;

pub(super) enum Verdict {
    Pass,
    LeftOut(&'static str),
    /// The first deviation, stop or error: its step, entry point and delta.
    Fail(String),
}

/// Runs every step of `case` through both entry points and compares each with full validation.
pub(super) fn check(case: &Case) -> Verdict {
    let evaluations = match case.run() {
        Ok(evaluations) => evaluations,
        Err(error) => return Verdict::Fail(format!("error: {error}")),
    };
    let repeats = |before: &BTreeMap<_, usize>, after: &BTreeMap<_, usize>| {
        after
            .iter()
            .any(|(key, &count)| count > 1 && !before.contains_key(key))
    };
    if !SHAPES_CHANGES_MAY_REPEAT
        && evaluations.iter().any(|evaluation| {
            evaluation.entry == EntryPoint::Shapes
                && repeats(&evaluation.before.results, &evaluation.after.results)
        })
    {
        return Verdict::LeftOut("shapes-change-repeats-shared-result");
    }
    for evaluation in &evaluations {
        let at = format!("step {}, {:?}", evaluation.step, evaluation.entry);
        let Outcome::Report(actual) = &evaluation.actual else {
            return Verdict::Fail(format!("{at}: unexpected stop {:?}", evaluation.actual));
        };
        let deviation = compare(&evaluation.before, &evaluation.after, actual);
        if deviation != Deviation::default() {
            return Verdict::Fail(format!("{at}: {deviation:?}"));
        }
    }
    Verdict::Pass
}

pub(super) fn replay(outer: &str, case: &str) -> String {
    format!(
        "NRESE_FUZZ_SEED={outer} NRESE_SHACL_CASE={case} cargo test --locked -p nrese-shacl \
         --test it change_differential::random_changes -- --exact"
    )
}

pub(super) fn run() {
    let outer = std::env::var("NRESE_FUZZ_SEED").unwrap_or_else(|_| "0".into());
    let selector = std::env::var("NRESE_SHACL_CASE").ok();
    let draws: Vec<u64> = match selector.as_deref() {
        None => (0..DRAWS).collect(),
        // A matrix row's selector.
        Some(row) if row.starts_with("row:") => return,
        Some(index) => vec![
            index
                .parse()
                .ok()
                .filter(|index| *index < DRAWS)
                .unwrap_or_else(|| panic!("no random draw matches {index:?}")),
        ],
    };
    let (mut equality, mut shapes_edits, mut inferred_edits) = (0, 0, 0);
    let mut left_out = BTreeMap::<_, usize>::new();
    let mut failures = Vec::new();
    for index in &draws {
        let case = draw::case(*index);
        equality += usize::from(case.equality);
        for step in &case.steps {
            shapes_edits += usize::from(step.shapes.is_some());
            inferred_edits += usize::from(!step.infer.is_empty() || !step.retract.is_empty());
        }
        match check(&case) {
            Verdict::Pass => {}
            Verdict::LeftOut(restriction) => *left_out.entry(restriction).or_default() += 1,
            Verdict::Fail(signature) => {
                failures.push(format!(
                    "{}\n{signature}",
                    replay(&outer, &index.to_string())
                ));
            }
        }
    }
    println!(
        "{} draws: {equality} with equality, {shapes_edits} shapes edits, {inferred_edits} \
         inferred edits; left out {left_out:?}",
        draws.len()
    );
    if let Some(first) = failures.first() {
        panic!(
            "{first}\n{} of {} draws deviate",
            failures.len(),
            draws.len()
        );
    }
}
