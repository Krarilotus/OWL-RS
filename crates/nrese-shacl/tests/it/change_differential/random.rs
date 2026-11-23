use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::case::Case;
use super::gaps::{Allowances, Gap, Kind, Location};
use super::observe::compare;
use super::stops::Outcome;
use super::{draw, matrix, shrink, witnesses};

/// Draws per invocation; each seed draws different cases.
const DRAWS: u64 = 64;

pub(super) enum Verdict {
    /// With the number of evaluations whose change introduces a result: draws that never
    /// do test little.
    Pass(usize),
    /// A drawn case holding the trigger of the known gap a restriction leaves out.
    LeftOut(&'static str),
    /// The first deviation, stop or error: its step, entry point and delta.
    Fail(String),
}

/// Runs every step of `case` through both entry points and compares each exactly with full
/// validation. Only a verified entry of `gaps` at `location`, the case's own selector,
/// accepts a deviation, and an entry there that accepts none is closed. A `drawn` case
/// holding a restricted trigger is left out before any comparison.
pub(super) fn check(case: &Case, location: &str, gaps: &[Gap], drawn: bool) -> Verdict {
    let evaluations = match case.run() {
        Ok(evaluations) => evaluations,
        Err(error) => return Verdict::Fail(format!("error: {error}")),
    };
    if let Some(gap) = draw::left_out(&evaluations).filter(|_| drawn) {
        return Verdict::LeftOut(gap);
    }
    let mut allowance = Allowances::default();
    let mut introducing = 0;
    for evaluation in &evaluations {
        let new = |key| !evaluation.before.results.contains_key(key);
        introducing += usize::from(evaluation.after.results.keys().any(new));
        let at = format!("step {}, {:?}", evaluation.step, evaluation.entry);
        let Outcome::Report(actual) = &evaluation.actual else {
            return Verdict::Fail(format!("{at}: unexpected stop {:?}", evaluation.actual));
        };
        let deviation = compare(&evaluation.before, &evaluation.after, actual);
        let here = Location {
            case: location,
            step: evaluation.step,
            entry: evaluation.entry,
        };
        for kind in [Kind::Result, Kind::Failure] {
            if !kind.select(&deviation).is_empty()
                && allowance.forgive(gaps, &here, kind, &deviation).is_none()
            {
                return Verdict::Fail(format!("{at}: {deviation:?}"));
            }
        }
    }
    let ran = BTreeSet::from([location.to_owned()]);
    if let Some(closed) = allowance.unused(gaps, &ran).first() {
        return Verdict::Fail(format!("{closed} shows nothing: fixed, delete the entry"));
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

/// The command that replays `case` of `test`: the first line of its panic.
pub(super) fn replay(test: &str, outer: &str, case: &str) -> String {
    format!(
        "NRESE_FUZZ_SEED={outer} NRESE_SHACL_CASE={case} cargo test --locked -p nrese-shacl \
         --test it change_differential::{test} -- --exact"
    )
}

/// Runs the draws, or the draw or saved case `NRESE_SHACL_CASE` names. A matrix row's
/// selector runs that row, so every selector runs a case or fails.
pub(super) fn run() {
    let outer = std::env::var("NRESE_FUZZ_SEED").unwrap_or_else(|_| "0".into());
    let selector = std::env::var("NRESE_SHACL_CASE").ok();
    type Draw = (String, Case, Option<String>);
    let draws: Box<dyn Iterator<Item = Draw>> = match selector.as_deref() {
        // Drawn on (up to ten times as many) until a draw introduces a result, so that the
        // batch never compares nothing new whatever the seed (as `incremental_tests` do).
        None => Box::new((0..DRAWS * 10).map(|index| (index.to_string(), draw::case(index), None))),
        Some(row) if row.starts_with("row:") => return matrix::run(),
        Some(saved) if saved.starts_with("saved:") => {
            let (case, recorded) = witnesses::load(&saved["saved:".len()..]);
            Box::new(std::iter::once((saved.into(), case, recorded)))
        }
        Some(index) => {
            let index = (index.parse::<u64>().ok())
                .filter(|index| *index < DRAWS * 10)
                .unwrap_or_else(|| panic!("no case matches NRESE_SHACL_CASE={index}"));
            Box::new(std::iter::once((
                index.to_string(),
                draw::case(index),
                None,
            )))
        }
    };
    // A saved case is compared exactly; only a verified entry at its own selector accepts a
    // deviation. No entry names a draw.
    let saved = selector
        .as_deref()
        .is_some_and(|case| case.starts_with("saved:"));
    let gaps = if saved {
        witnesses::verified_gaps()
    } else {
        Vec::new()
    };
    let (mut equality, mut shapes_edits, mut inferred_edits, mut introducing) = (0, 0, 0, 0);
    let (mut count, mut controls) = (0, 0);
    let mut left_out = BTreeMap::<_, u64>::new();
    let mut failures = Vec::new();
    for draw in draws {
        // Draws left out by a restriction don't count towards the batch.
        let compared = count - left_out.values().sum::<u64>();
        if selector.is_none() && compared >= DRAWS && introducing > 0 {
            break;
        }
        count += 1;
        let (selector, case, recorded) = (&draw.0, &draw.1, &draw.2);
        equality += usize::from(case.equality);
        for step in &case.steps {
            shapes_edits += usize::from(step.shapes.is_some());
            controls += usize::from(step.shapes.is_none() && step.edits_the_shapes_graph());
            inferred_edits += usize::from(!step.infer.is_empty() || !step.retract.is_empty());
        }
        let verdict = check(case, selector, &gaps, !saved);
        // A saved fixture must still show what it recorded.
        if let (Some(recorded), Verdict::Pass(_)) = (recorded, &verdict) {
            let replay = replay("random_changes", &outer, selector);
            failures.push(format!(
                "{replay}\nchanged: the saved case recorded {recorded}, now it shows none"
            ));
            continue;
        }
        match verdict {
            Verdict::Pass(evaluations) => introducing += evaluations,
            Verdict::LeftOut(gap) => *left_out.entry(gap).or_default() += 1,
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
        "{count} draws: {equality} with equality, {shapes_edits} shapes edits, {controls} \
         ordinary edits of the shapes graph, {inferred_edits} inferred edits, {introducing} \
         evaluations introducing results; left out {left_out:?}"
    );
    if let Some(first) = failures.first() {
        panic!("{first}\n{} of {count} draws deviate", failures.len());
    }
    assert!(
        selector.is_some() || introducing > 0,
        "no draw of {count} introduced a result: the batch compared nothing new"
    );
}
