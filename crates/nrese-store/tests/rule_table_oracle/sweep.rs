use std::collections::{BTreeMap, BTreeSet};

use nrese_reasoner::ReasoningMode;
use nrese_store::StoreService;

use crate::{
    Facts, Triple,
    generator::{self, Case},
    judges,
    oracle::{self, Datatypes, Model},
    publication::{self, Publication},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Answer {
    Accept(bool),
    Row(Triple, bool),
    CompleteRead(Triple, bool),
    Redundant,
}

const UNDOCUMENTED: &str = "undocumented";
const CAPPED: &str = "documented limit reasoning-semantics.md:63-65";
const GENERALISED: &str = "documented limit reasoning-semantics.md:74";

#[derive(Clone, Debug)]
pub struct Difference {
    mode: &'static str,
    judge: &'static str,
    write: usize,
    expected: Answer,
    limit: &'static str,
    detail: String,
}

impl Difference {
    fn same_witness(&self, other: &Self) -> bool {
        (self.mode, self.judge, &self.expected, self.limit)
            == (other.mode, other.judge, &other.expected, other.limit)
    }

    fn line(&self) -> String {
        format!(
            "{} {} write {} oracle {:?}; judge {} [{}]",
            self.mode,
            self.judge,
            self.write + 1,
            self.expected,
            self.detail,
            self.limit,
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
            if !model.inconsistent || write.past_gates {
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

fn projection(step: &Step) -> Publication {
    publication::project(&step.asserted, &step.model)
}

struct Comparison<'a> {
    case: &'a Case,
    focus: Option<&'a Difference>,
    differences: Vec<Difference>,
    counts: BTreeMap<&'static str, usize>,
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
        (mode, judge, write): (&'static str, &'static str, usize),
        store: Option<&StoreService>,
        (accepted, wanted): (bool, bool),
        (step, expected): (&Step, &Publication),
    ) -> Option<bool> {
        let mut report = |expected, detail| {
            let limit = match &expected {
                Answer::Accept(false) if !oracle::published_clash(&step.asserted, false) => {
                    "documented limit reasoning-semantics.md:57"
                }
                Answer::Accept(false)
                    if write > 0
                        && matches!(judge, "warm" | "cold")
                        && !oracle::published_clash(&step.asserted, true) =>
                {
                    GENERALISED
                }
                Answer::Row(row, true) if row == &self.case.watch => match self.case.shape {
                    "redundant" | "long-chain" => CAPPED,
                    "cycle" => "documented limit reasoning-semantics.md:68",
                    "literal-functional" if matches!(judge, "warm" | "cold" | "reopened") => {
                        GENERALISED
                    }
                    _ => UNDOCUMENTED,
                },
                _ => UNDOCUMENTED,
            };
            self.differences.push(Difference {
                mode,
                judge,
                write,
                expected,
                limit,
                detail,
            })
        };
        if accepted != wanted {
            report(Answer::Accept(wanted), format!("accepted={accepted}"));
        }
        let store = store?;
        let (rows, select_status) = judges::rows(store);
        for row in expected.required.difference(&rows) {
            report(
                Answer::Row(row.clone(), true),
                format!("row absent (SELECT {select_status})"),
            );
        }
        for row in rows.difference(&step.model.facts) {
            report(
                Answer::Row(row.clone(), false),
                format!("row present (SELECT {select_status})"),
            );
        }

        if step.model.inconsistent {
            return None;
        }
        let (answer, status) = judges::ask(store, &self.case.watch);
        let wanted = step.model.facts.contains(&self.case.watch);
        if status == "complete" && answer != wanted {
            report(
                Answer::CompleteRead(self.case.watch.clone(), wanted),
                format!("ASK={answer} ({status})"),
            );
        }
        accepted.then_some(answer)
    }
}

fn run<'a>(case: &'a Case, steps: &[Step], focus: Option<&'a Difference>) -> Comparison<'a> {
    let mut compare = Comparison {
        case,
        focus,
        differences: Vec::new(),
        counts: BTreeMap::new(),
    };
    let projected: Vec<_> = steps.iter().map(projection).collect();
    let empty = Step {
        model: oracle::evaluate(&Facts::new(), Datatypes::Standard),
        asserted: Facts::new(),
        ordered: Vec::new(),
    };
    let initial = projection(&empty);
    for publication in &projected {
        for (&reason, count) in &publication.counts {
            *compare.counts.entry(reason).or_default() += count;
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
                let state = if accepted || rebuild {
                    (step, expected)
                } else {
                    (&empty, &initial)
                };
                let answer = compare.compare(
                    (mode, judge, index),
                    // Rematerialisation reports consistency without a transactional write.
                    (!rebuild).then_some(store.as_ref()),
                    (accepted, !step.model.inconsistent),
                    state,
                );
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
                        limit: CAPPED,
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
            let mut committed = (&empty, &initial);
            for (index, ((write, step), expected)) in
                case.writes.iter().zip(steps).zip(&projected).enumerate()
            {
                let accepted = match write.past_gates {
                    true => judges::load_past_gates(&store, write),
                    false => judges::apply(&pipeline, write),
                };
                let wanted = !step.model.inconsistent;
                let visible = if accepted || write.past_gates {
                    (step, expected)
                } else {
                    committed
                };
                if compare.enabled(mode, judge) {
                    compare.compare(
                        (mode, judge, index),
                        Some(store.as_ref()),
                        (accepted, wanted),
                        visible,
                    );
                }
                if restart {
                    drop(pipeline);
                    drop(store);
                    store = open();
                    pipeline = judges::pipeline(&store, rl);
                    if reopened {
                        compare.compare(
                            (mode, "reopened", index),
                            Some(store.as_ref()),
                            (accepted, accepted),
                            visible,
                        );
                    }
                }
                if wanted || write.past_gates {
                    committed = (step, expected);
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
            Answer::Accept(_) | Answer::Redundant => true,
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
            ("nil-head" | "nil-cell", Answer::Row(row, true)) => {
                difference.write == 0 && row == &case.watch
            }
            ("literal-functional", Answer::Row(row, true)) => {
                delta && difference.write == 1 && row == &case.watch
            }
            ("equality-member" | "equality-schema", Answer::Accept(false)) => difference.write == 1,
            _ => false,
        }
    })
}

fn persists(case: &Case, witness: &Difference) -> bool {
    if (case.writes.len() == 1) != (witness.write == 0) {
        return false;
    }
    let steps = steps(case);
    let plausible = match &witness.expected {
        Answer::Accept(accepted) => steps
            .iter()
            .any(|step| step.model.inconsistent != *accepted),
        Answer::Row(row, present) | Answer::CompleteRead(row, present) => {
            !present || steps.iter().any(|step| step.model.facts.contains(row))
        }
        Answer::Redundant => steps
            .windows(2)
            .any(|pair| pair[0].model.facts == pair[1].model.facts),
    };
    plausible
        && run(case, &steps, Some(witness))
            .differences
            .iter()
            .any(|difference| {
                difference.write + 1 == steps.len() && difference.same_witness(witness)
            })
}

fn reduce(case: &mut Case, witness: &Difference) {
    case.writes.truncate(witness.write + 1);
    // Try contiguous block removals first. End with single removals to get a
    // 1-minimal reproducer for this answer, judge and equality mode.
    let mut changed = true;
    while changed {
        changed = false;
        for index in (0..case.writes.len() - 1).rev() {
            let mut smaller = case.clone();
            smaller.writes.remove(index);
            if persists(&smaller, witness) {
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

fn print_reduced(
    seed: u64,
    mut case: Case,
    witness: &Difference,
) -> BTreeSet<(&'static str, &'static str)> {
    reduce(&mut case, witness);
    println!(
        "REDUCED seed={seed} shape={} watch={} witness={:?}",
        case.shape,
        case.watch.join(" "),
        witness.expected,
    );
    for (index, write) in case.writes.iter().enumerate() {
        println!("WRITE {}", index + 1);
        for (verb, rows) in [("DELETE", &write.delete), ("INSERT", &write.insert)] {
            for row in rows {
                println!("{verb} {} .", row.join(" "));
            }
        }
    }
    let judges = run(&case, &steps(&case), None)
        .differences
        .into_iter()
        .filter(|difference| {
            difference.write + 1 == case.writes.len()
                && difference.expected == witness.expected
                && difference.limit == witness.limit
        })
        .map(|difference| {
            println!("RED RESULT {}", difference.line());
            (difference.mode, difference.judge)
        })
        .collect();
    println!("END REDUCED seed={seed}");
    judges
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
    let mut witnesses = BTreeMap::new();
    let mut disagreements = 0;
    let mut counts = BTreeMap::new();
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
        for (reason, count) in &result.counts {
            *counts.entry(*reason).or_default() += count;
        }
        if result.differences.is_empty() {
            continue;
        }
        disagreements += 1;
        let known = required_red(&case, &result.differences).is_some();
        if known {
            red.insert(case.shape);
        }
        println!("SEED {seed} shape={} required-red={known}", case.shape);
        result.print("RESULT ");
        if demonstrate_red {
            assert!(
                generator::RED_SHAPES.contains(&case.shape),
                "unexpected red shape {}",
                case.shape
            );
        }
        for difference in result.differences {
            // Verdict and redundancy mismatches have no row key. Keep their
            // originating write so different inconsistency inputs stay distinct.
            let origin = matches!(difference.expected, Answer::Accept(_) | Answer::Redundant)
                .then_some((seed, difference.write));
            let judge = (difference.mode, difference.judge, difference.write + 1);
            witnesses
                .entry((origin, difference.expected.clone(), difference.limit))
                .or_insert_with(BTreeMap::new)
                .entry(judge)
                .or_insert((seed, difference));
        }
    }
    for ((_, answer, limit), mut sightings) in witnesses {
        println!(
            "WITNESS {answer:?} [{limit}] observed={:?}",
            sightings.keys().collect::<Vec<_>>()
        );
        while let Some((_, (seed, witness))) = sightings.pop_first() {
            let matched = print_reduced(seed, generator::generated(seed, plain), &witness);
            sightings.retain(|&(mode, judge, write), _| {
                write != witness.write + 1 || !matched.contains(&(mode, judge))
            });
        }
    }
    for item in publication::ITEMS {
        println!(
            "PUBLICATION {item} => {}",
            counts.get(item).copied().unwrap_or(0)
        );
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
