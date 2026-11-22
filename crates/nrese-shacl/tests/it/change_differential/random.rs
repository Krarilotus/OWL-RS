use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::case::Case;
use super::gaps::EntryPoint;
use super::observe::{Deviation, compare};
use super::stops::Outcome;
use super::{draw, shrink, witnesses};

/// Draws per invocation; each seed draws different cases.
const DRAWS: u64 = 64;
/// Restriction for the known gap `shapes-change-repeats-shared-result`, removed with it: a
/// draw whose shapes change introduces one result more than once is left out.
const SHAPES_CHANGES_MAY_REPEAT: bool = false;

pub(super) enum Verdict {
    /// With the number of evaluations whose change introduces a result: draws that never
    /// do test little.
    Pass(usize),
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
    let mut introducing = 0;
    for evaluation in &evaluations {
        introducing += usize::from(
            evaluation
                .after
                .results
                .keys()
                .any(|key| !evaluation.before.results.contains_key(key)),
        );
        let at = format!("step {}, {:?}", evaluation.step, evaluation.entry);
        let Outcome::Report(actual) = &evaluation.actual else {
            return Verdict::Fail(format!("{at}: unexpected stop {:?}", evaluation.actual));
        };
        let deviation = compare(&evaluation.before, &evaluation.after, actual);
        if deviation != Deviation::default() {
            return Verdict::Fail(format!("{at}: {deviation:?}"));
        }
    }
    Verdict::Pass(introducing)
}

/// Where a failing case's fixtures go: a path its replay line determines, made safe for
/// every file system (a selector holds `:`).
fn fixture_dir(outer: &str, selector: &str) -> PathBuf {
    let name = Path::new(selector.strip_prefix("saved:").unwrap_or(selector))
        .file_name()
        .map_or_else(|| selector.into(), |name| name.to_string_lossy());
    let safe: String = format!("{outer}-{name}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    std::env::var_os("FLEET_OUT")
        .map_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")), PathBuf::from)
        .join("shacl-change-differential/random_changes")
        .join(safe)
}

/// Writes `case` as fixture `file` in `dir`, with its replay command beside it.
fn save(dir: &Path, file: &str, id: &str, case: &Case, signature: &str, replay: &str) {
    fs::create_dir_all(dir).expect("fixture directory");
    let text = witnesses::fixture(id, case, signature, replay);
    fs::write(dir.join(file), text).expect("write fixture");
    let command = file.replace(".toml", ".replay.txt");
    fs::write(dir.join(command), format!("{replay}\n")).expect("write replay command");
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
    let draws: Vec<(String, Case)> = match selector.as_deref() {
        None => (0..DRAWS)
            .map(|index| (index.to_string(), draw::case(index)))
            .collect(),
        // A matrix row's selector.
        Some(row) if row.starts_with("row:") => return,
        Some(saved) if saved.starts_with("saved:") => {
            vec![(saved.into(), witnesses::load(&saved["saved:".len()..]))]
        }
        Some(index) => {
            let index = index
                .parse::<u64>()
                .ok()
                .filter(|index| *index < DRAWS)
                .unwrap_or_else(|| panic!("no random draw matches {index:?}"));
            vec![(index.to_string(), draw::case(index))]
        }
    };
    let (mut equality, mut shapes_edits, mut inferred_edits, mut introducing) = (0, 0, 0, 0);
    let mut left_out = BTreeMap::<_, usize>::new();
    let mut failures = Vec::new();
    for (selector, case) in &draws {
        equality += usize::from(case.equality);
        for step in &case.steps {
            shapes_edits += usize::from(step.shapes.is_some());
            inferred_edits += usize::from(!step.infer.is_empty() || !step.retract.is_empty());
        }
        match check(case) {
            Verdict::Pass(evaluations) => introducing += evaluations,
            Verdict::LeftOut(restriction) => *left_out.entry(restriction).or_default() += 1,
            Verdict::Fail(signature) => {
                let replay = replay(&outer, selector);
                let dir = fixture_dir(&outer, selector);
                save(&dir, "original.toml", selector, case, &signature, &replay);
                // Only the first failing case is shrunk; later ones are saved as drawn.
                let shrunk = failures.is_empty().then(|| {
                    let shrunk = shrink::shrink(case, &signature);
                    let minimal = dir.join("minimal.toml");
                    let command = self::replay(&outer, &format!("saved:{}", minimal.display()));
                    save(
                        &dir,
                        "minimal.toml",
                        selector,
                        &shrunk.case,
                        &signature,
                        &command,
                    );
                    format!(
                        "\nshrunk in {} attempts{}",
                        shrunk.attempts,
                        if shrunk.ran_out {
                            ", budget ran out"
                        } else {
                            ""
                        }
                    )
                });
                failures.push(format!(
                    "{replay}\n{signature}\nfixtures: {}{}",
                    dir.display(),
                    shrunk.unwrap_or_default()
                ));
            }
        }
    }
    println!(
        "{} draws: {equality} with equality, {shapes_edits} shapes edits, {inferred_edits} \
         inferred edits, {introducing} evaluations introducing results; left out {left_out:?}",
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
