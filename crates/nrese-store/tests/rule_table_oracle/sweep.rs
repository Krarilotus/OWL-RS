use std::collections::BTreeSet;

use nrese_reasoner::ReasoningMode;
use nrese_store::StoreService;

use crate::{
    Facts, Triple,
    generator::{self, Case},
    judges,
    oracle::{self, Datatypes, Model},
    publication::{self, Publication},
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Answer {
    Accept(bool),
    Row(Triple, bool),
    CompleteRead(Triple, bool),
    Rollback(Triple, bool),
    Redundant,
}

#[derive(Clone, Debug)]
pub struct Difference {
    mode: &'static str,
    judge: &'static str,
    write: usize,
    expected: Answer,
    detail: String,
}

impl Difference {
    fn same_witness(&self, other: &Self) -> bool {
        (self.mode, self.judge, &self.expected) == (other.mode, other.judge, &other.expected)
    }

    fn line(&self) -> String {
        let source = if matches!(self.expected, Answer::Rollback(..)) {
            "before refusal"
        } else {
            "oracle"
        };
        format!(
            "{} {} write {} {source} {:?}; judge {}",
            self.mode,
            self.judge,
            self.write + 1,
            self.expected,
            self.detail
        )
    }
}

pub struct Step {
    pub model: Model,
    asserted: Facts,
    ordered: Vec<Triple>,
}

pub fn steps(case: &Case) -> Vec<Step> {
    let mut committed = Facts::new();
    let mut ordered = Vec::new();
    case.writes
        .iter()
        .map(|write| {
            let asserted = write.after(&committed);
            let mut candidate = ordered.clone();
            candidate.retain(|fact| !write.delete.contains(fact));
            let mut seen: Facts = candidate.iter().cloned().collect();
            candidate.extend(
                write
                    .insert
                    .iter()
                    .filter(|fact| seen.insert((*fact).clone()))
                    .cloned(),
            );
            let model = oracle::evaluate(&asserted, Datatypes::Standard);
            if !model.inconsistent {
                committed = asserted.clone();
                ordered = candidate.clone();
            }
            Step {
                model,
                asserted,
                ordered: candidate,
            }
        })
        .collect()
}

struct Comparison<'a> {
    case: &'a Case,
    focus: Option<&'a Difference>,
    differences: Vec<Difference>,
    counts: [usize; publication::ITEMS.len()],
}

impl Comparison<'_> {
    fn print(&self, prefix: &str) {
        for difference in &self.differences {
            println!("{prefix}{}", difference.line());
        }
    }

    fn enabled(&self, mode: &str, judge: &str) -> bool {
        self.focus
            .is_none_or(|focus| focus.mode == mode && focus.judge == judge)
    }

    fn compare(
        &mut self,
        mode: &'static str,
        judge: &'static str,
        write: usize,
        store: &StoreService,
        accepted: bool,
        step: &Step,
        expected: &Publication,
    ) -> Option<bool> {
        let mut report = |expected, detail| {
            self.differences.push(Difference {
                mode,
                judge,
                write,
                expected,
                detail,
            })
        };
        if accepted == step.model.inconsistent {
            report(
                Answer::Accept(!step.model.inconsistent),
                format!("accepted={accepted}"),
            );
            return None;
        }
        if !accepted {
            return None;
        }
        let (rows, select_status) = judges::rows(store);
        let preferred = match self.focus.map(|focus| &focus.expected) {
            Some(Answer::Row(row, _)) => row,
            _ => &self.case.watch,
        };
        let missing = |row: &Triple| expected.required.contains(row) && !rows.contains(row);
        let extra = |row: &Triple| !step.model.facts.contains(row);
        if let Some(row) = Some(preferred)
            .filter(|row| missing(row))
            .or_else(|| expected.required.difference(&rows).next())
        {
            report(
                Answer::Row(row.clone(), true),
                format!("row absent (SELECT {select_status})"),
            );
        }
        if let Some(row) = rows
            .get(preferred)
            .filter(|row| extra(row))
            .or_else(|| rows.iter().find(|row| extra(row)))
        {
            report(
                Answer::Row(row.clone(), false),
                format!("row present (SELECT {select_status})"),
            );
        }

        let (answer, status) = judges::ask(store, &self.case.watch);
        let wanted = step.model.facts.contains(&self.case.watch);
        if status == "complete" && answer != wanted {
            report(
                Answer::CompleteRead(self.case.watch.clone(), wanted),
                format!("ASK={answer} ({status})"),
            );
        }
        Some(answer)
    }

    fn rollback(
        &mut self,
        mode: &'static str,
        judge: &'static str,
        write: usize,
        store: &StoreService,
        before: &Facts,
    ) {
        let (after, _) = judges::rows(store);
        if let Some(row) = before.symmetric_difference(&after).next() {
            self.differences.push(Difference {
                mode,
                judge,
                write,
                expected: Answer::Rollback(row.clone(), before.contains(row)),
                detail: format!("row present after refusal={}", after.contains(row)),
            });
        }
    }
}

fn run<'a>(case: &'a Case, steps: &[Step], focus: Option<&'a Difference>) -> Comparison<'a> {
    let mut compare = Comparison {
        case,
        focus,
        differences: Vec::new(),
        counts: [0; publication::ITEMS.len()],
    };
    let projected: Vec<_> = steps
        .iter()
        .map(|step| publication::project(&step.asserted, &step.model))
        .collect();
    for publication in &projected {
        for (sum, count) in compare.counts.iter_mut().zip(publication.counts) {
            *sum += count;
        }
    }

    for &mode in judges::MODES {
        for (judge, rebuild) in [("first", false), ("rebuild", true)] {
            if !compare.enabled(mode, judge) {
                continue;
            }
            let mut previous = None;
            for (index, (step, expected)) in steps.iter().zip(&projected).enumerate() {
                let (store, accepted) = judges::first_and_rebuild(mode, &step.ordered, rebuild);
                let answer = compare.compare(mode, judge, index, &store, accepted, step, expected);
                if case.shape == "redundant"
                    && !rebuild
                    && index > 0
                    && let (Some(before), Some(after)) = (previous, answer)
                    && before != after
                    && steps[index - 1].model.facts == step.model.facts
                    && case.writes[index].insert.len() == 1
                    && case.writes[index].delete.is_empty()
                {
                    compare.differences.push(Difference {
                        mode,
                        judge,
                        write: index,
                        expected: Answer::Redundant,
                        detail: format!("watch {:?} changed from {before} to {after}", case.watch),
                    });
                }
                previous = answer;
            }
        }

        for (judge, restart) in [("warm", false), ("cold", true)] {
            let reopened = restart && compare.enabled(mode, "reopened");
            if !compare.enabled(mode, judge) && !reopened {
                continue;
            }
            let temp = tempfile::tempdir().expect("judge's private store directory");
            let open = || judges::open(mode, Some(temp.path()));
            let rl = ReasoningMode::Owl2Rl;
            let mut store = open();
            let mut pipeline = judges::pipeline(&store, rl);
            for (index, ((write, step), expected)) in
                case.writes.iter().zip(steps).zip(&projected).enumerate()
            {
                let before = step.model.inconsistent.then(|| judges::rows(&store).0);
                let accepted = judges::apply(&pipeline, write);
                let compare_store = |compare: &mut Comparison<'_>, judge, store: &StoreService| {
                    if !accepted && let Some(before) = &before {
                        compare.rollback(mode, judge, index, store, before);
                    } else {
                        compare.compare(mode, judge, index, store, accepted, step, expected);
                    }
                };
                if compare.enabled(mode, judge) {
                    compare_store(&mut compare, judge, &store);
                }
                if restart {
                    drop(pipeline);
                    drop(store);
                    store = open();
                    pipeline = judges::pipeline(&store, rl);
                    if reopened && accepted != step.model.inconsistent {
                        compare_store(&mut compare, "reopened", &store);
                    }
                }
            }
        }
    }
    compare
}

pub fn check_cell(case: &Case, steps: &[Step]) -> bool {
    let mut result = run(case, steps, None);
    result
        .differences
        .retain(|difference| match &difference.expected {
            Answer::Row(row, _) | Answer::CompleteRead(row, _) => row == &case.watch,
            Answer::Accept(_) | Answer::Redundant | Answer::Rollback(..) => true,
        });
    result.print("");
    if let Some(witness) = required_red(case, &result.differences) {
        println!("CELL RED {} {}", case.shape, witness.line());
    }
    result.differences.is_empty()
}

fn required_red<'a>(case: &Case, differences: &'a [Difference]) -> Option<&'a Difference> {
    differences.iter().find(|difference| {
        let delta = matches!(difference.judge, "warm" | "cold");
        match (case.shape, &difference.expected) {
            ("redundant", Answer::Redundant) => true,
            ("cycle" | "long-chain", Answer::Row(row, true)) => {
                difference.write == 1 && row == &case.watch
            }
            ("schema-insert" | "literal-functional", Answer::Row(row, true)) => {
                delta && difference.write == 1 && row == &case.watch
            }
            ("schema-delete", Answer::Row(row, false)) => {
                delta && difference.write == 1 && row == &case.watch
            }
            ("equality-member" | "equality-schema", Answer::Accept(false)) => difference.write == 1,
            _ => false,
        }
    })
}

fn report_red_seed(seed: u64, case: &Case, result: &Comparison<'_>) -> bool {
    let required = required_red(case, &result.differences);
    let known = required.is_some();
    println!("SEED {seed} shape={} required-red={known}", case.shape);
    result.print("RESULT ");
    let witness = required.unwrap_or(&result.differences[0]);
    print_reduced(seed, case.clone(), witness);
    known
}

fn persists(case: &Case, witness: &Difference) -> bool {
    let steps = steps(case);
    let plausible = match &witness.expected {
        Answer::Accept(accepted) => steps
            .iter()
            .any(|step| step.model.inconsistent != *accepted),
        Answer::Row(row, present) | Answer::CompleteRead(row, present) => steps
            .iter()
            .any(|step| !step.model.inconsistent && step.model.facts.contains(row) == *present),
        Answer::Redundant => steps
            .windows(2)
            .any(|pair| pair[0].model.facts == pair[1].model.facts),
        Answer::Rollback(..) => steps.iter().any(|step| step.model.inconsistent),
    };
    plausible
        && run(case, &steps, Some(witness))
            .differences
            .iter()
            .any(|difference| difference.same_witness(witness))
}

fn reduce(case: &mut Case, witness: &Difference) {
    // Try contiguous block removals first. End with single removals to get a
    // 1-minimal reproducer for this answer, judge and equality mode.
    let mut changed = true;
    while changed {
        changed = false;
        for index in (0..case.writes.len()).rev() {
            let mut smaller = case.clone();
            smaller.writes.remove(index);
            if !smaller.writes.is_empty() && persists(&smaller, witness) {
                *case = smaller;
                changed = true;
            }
        }
        for index in 0..case.writes.len() {
            for insert in [true, false] {
                let facts = |case: &Case| {
                    if insert {
                        case.writes[index].insert.len()
                    } else {
                        case.writes[index].delete.len()
                    }
                };
                let mut width = facts(case);
                while width > 0 {
                    let mut offset = 0;
                    while offset < facts(case) {
                        let mut smaller = case.clone();
                        let write = &mut smaller.writes[index];
                        let rows = if insert {
                            &mut write.insert
                        } else {
                            &mut write.delete
                        };
                        rows.drain(offset..(offset + width).min(rows.len()));
                        if persists(&smaller, witness) {
                            *case = smaller;
                            changed = true;
                        } else {
                            offset += width;
                        }
                    }
                    width /= 2;
                }
            }
        }
    }
}

fn print_reduced(seed: u64, mut case: Case, witness: &Difference) {
    reduce(&mut case, witness);
    println!(
        "REDUCED seed={seed} shape={} watch={}",
        case.shape,
        case.watch.join(" ")
    );
    for (index, write) in case.writes.iter().enumerate() {
        println!("WRITE {}", index + 1);
        for (verb, rows) in [("DELETE", &write.delete), ("INSERT", &write.insert)] {
            for row in rows {
                println!("{verb} {} .", row.join(" "));
            }
        }
    }
    run(&case, &steps(&case), None).print("RED RESULT ");
    println!("END REDUCED seed={seed}");
}

#[test]
fn seeds() {
    let Ok(range) = std::env::var("NRESE_ORACLE_SEEDS") else {
        return;
    };
    let (first, end) = range.split_once("..").expect("seed range start..end");
    let (first, end): (u64, u64) = (first.parse().unwrap(), end.parse().unwrap());
    let cases = end
        .checked_sub(first)
        .filter(|n| *n > 0)
        .expect("nonempty seed range");
    let plain = std::env::var_os("NRESE_ORACLE_PLAIN").is_some();
    let demonstrate_red = std::env::var_os("NRESE_ORACLE_EXPECT_RED").is_some();
    assert!(!plain || !demonstrate_red);
    let mut drawn = 0;
    let mut covered = BTreeSet::new();
    let mut red = BTreeSet::new();
    let mut disagreements = 0;
    let mut counts = [0; publication::ITEMS.len()];
    while generator::draws_on(
        drawn,
        cases,
        plain || covered.len() == generator::SHAPES.len(),
    ) {
        let seed = first.checked_add(drawn).expect("seed overflow");
        drawn += 1;
        let case = generator::generated(seed, plain);
        covered.insert(case.shape);
        let steps = steps(&case);
        let result = run(&case, &steps, None);
        for (sum, count) in counts.iter_mut().zip(result.counts) {
            *sum += count;
        }
        if result.differences.is_empty() {
            continue;
        }
        disagreements += 1;
        if report_red_seed(seed, &case, &result) {
            red.insert(case.shape);
        }
    }
    for (item, count) in publication::ITEMS.iter().zip(counts) {
        println!("PUBLICATION {item} => {count}");
    }
    println!(
        "SWEEP plain={plain} seeds={drawn} red={disagreements} shapes={covered:?} required-red={red:?}"
    );
    assert!(
        plain || covered.len() == generator::SHAPES.len(),
        "generator shape coverage"
    );
    if demonstrate_red {
        for shape in generator::RED_SHAPES {
            assert!(red.contains(shape), "missing required red shape {shape}");
        }
    } else {
        assert_eq!(disagreements, 0, "seeds differ from the oracle");
    }
}
