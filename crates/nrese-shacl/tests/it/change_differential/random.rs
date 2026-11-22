use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::case::Case;
use super::gaps::EntryPoint;
use super::observe::{Deviation, compare};
use super::stops::Outcome;
use super::{draw, matrix, shrink, witnesses};

/// Draws per invocation; each seed draws different cases.
const DRAWS: u64 = 64;
/// Restriction for the known gap `shapes-change-repeats-shared-result`, removed with it: in
/// a shapes change, the new results the later validation repeats are left out of the
/// comparison; everything else of the case is compared.
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
    let (mut introducing, mut left_out) = (0, false);
    for evaluation in &evaluations {
        let new = |key| !evaluation.before.results.contains_key(key);
        introducing += usize::from(evaluation.after.results.keys().any(new));
        let at = format!("step {}, {:?}", evaluation.step, evaluation.entry);
        let Outcome::Report(actual) = &evaluation.actual else {
            return Verdict::Fail(format!("{at}: unexpected stop {:?}", evaluation.actual));
        };
        let mut deviation = compare(&evaluation.before, &evaluation.after, actual);
        if !SHAPES_CHANGES_MAY_REPEAT && evaluation.entry == EntryPoint::Shapes {
            // Only a repeat as often as full validation repeats the key; a missed result or
            // any other count of it stays a deviation.
            let repeated: BTreeMap<_, _> = (evaluation.after.results.iter())
                .filter(|(key, count)| **count > 1 && new(*key))
                .map(|(key, count)| (format!("{key:?}"), *count))
                .collect();
            let before = deviation.results.len();
            (deviation.results).retain(|key, count| repeated.get(key) != Some(&count.actual));
            left_out |= deviation.results.len() < before;
        }
        if deviation != Deviation::default() {
            return Verdict::Fail(format!("{at}: {deviation:?}"));
        }
    }
    if left_out {
        Verdict::LeftOut("shapes-change-repeats-shared-result")
    } else {
        Verdict::Pass(introducing)
    }
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

/// The command that replays `case` of `test`: the first line of its panic.
pub(super) fn replay(test: &str, outer: &str, case: &str) -> String {
    format!(
        "NRESE_FUZZ_SEED={outer} NRESE_SHACL_CASE={case} cargo test --locked -p nrese-shacl \
         --test it change_differential::{test} -- --exact"
    )
}

/// Whether `selector`, not a matrix row's, names a draw or an existing saved case.
pub(super) fn selects_a_draw(selector: &str) -> bool {
    match selector.strip_prefix("saved:") {
        Some(file) => witnesses::saved_path(file).is_file(),
        None => selector.parse::<u64>().is_ok_and(|index| index < DRAWS),
    }
}

pub(super) fn run() {
    let outer = std::env::var("NRESE_FUZZ_SEED").unwrap_or_else(|_| "0".into());
    let selector = std::env::var("NRESE_SHACL_CASE").ok();
    let draws: Vec<(String, Case, Option<String>)> = match selector.as_deref() {
        None => (0..DRAWS)
            .map(|index| (index.to_string(), draw::case(index), None))
            .collect(),
        Some(row) if row.starts_with("row:") => {
            let id = &row["row:".len()..];
            assert!(
                matrix::has_row(id),
                "no case matches NRESE_SHACL_CASE={row}"
            );
            return;
        }
        Some(case) if !selects_a_draw(case) => panic!("no case matches NRESE_SHACL_CASE={case}"),
        Some(saved) if saved.starts_with("saved:") => {
            let (case, recorded) = witnesses::load(&saved["saved:".len()..]);
            vec![(saved.into(), case, recorded)]
        }
        Some(index) => {
            let index = index.parse::<u64>().expect("a draw's index");
            vec![(index.to_string(), draw::case(index), None)]
        }
    };
    let (mut equality, mut shapes_edits, mut inferred_edits, mut introducing) = (0, 0, 0, 0);
    let mut left_out = BTreeMap::<_, usize>::new();
    let mut failures = Vec::new();
    for (selector, case, recorded) in &draws {
        equality += usize::from(case.equality);
        for step in &case.steps {
            shapes_edits += usize::from(step.shapes.is_some());
            inferred_edits += usize::from(!step.infer.is_empty() || !step.retract.is_empty());
        }
        let verdict = check(case);
        // A saved fixture must still show what it recorded, restriction or not.
        if let (Some(recorded), Verdict::Pass(_) | Verdict::LeftOut(_)) = (recorded, &verdict) {
            let replay = replay("random_changes", &outer, selector);
            failures.push(format!(
                "{replay}\nchanged: the saved case recorded {recorded}, now it shows none"
            ));
            continue;
        }
        match verdict {
            Verdict::Pass(evaluations) => introducing += evaluations,
            Verdict::LeftOut(restriction) => *left_out.entry(restriction).or_default() += 1,
            Verdict::Fail(signature) => {
                let replay = replay("random_changes", &outer, selector);
                let changed = (recorded.as_ref())
                    .filter(|recorded| **recorded != signature)
                    .map(|recorded| format!("\nchanged: the saved case recorded {recorded}"))
                    .unwrap_or_default();
                let dir = fixture_dir(&outer, selector);
                save(&dir, "original.toml", selector, case, &signature, &replay);
                // Only the first failing draw is shrunk; later ones and saved cases are kept.
                let draw = !selector.starts_with("saved:");
                let shrunk = (draw && failures.is_empty()).then(|| {
                    let shrunk = shrink::shrink(case, &signature);
                    let minimal = dir.join("minimal.toml");
                    let saved = format!("saved:{}", minimal.display());
                    let command = self::replay("random_changes", &outer, &saved);
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
                    "{replay}\n{signature}{changed}\nfixtures: {}{}",
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
