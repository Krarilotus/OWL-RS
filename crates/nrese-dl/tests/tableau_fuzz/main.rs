//! The hypertableau against the semantics, on many fuzzed ontologies (`nrese_owl::fuzz`,
//! without data): the gate of soundness first (docs/design/owl2-dl.md §11).
//!
//! - **Consistent** answers come with the model the graph stands for, folded at the
//!   blocks; it is closed under the role inclusions and checked against every axiom. A
//!   folded model can fail where number restrictions meet pairwise blocking (the
//!   calculus's model is then the unravelling); such answers are confirmed by a search
//!   for a small model instead, and the rest are counted as unconfirmed.
//! - **Inconsistent** answers are checked by a search for a model with up to three
//!   elements (exhaustive where small, sampled otherwise): finding one is a wrong answer.
//! - **Metamorphic:** the answer is the same with each optimisation off, under renaming
//!   and with the axioms shuffled.
//!
//! `NRESE_FUZZ_CASES` and `NRESE_FUZZ_SEED` widen or move a campaign.

mod ni_gen;
mod semantics;

use std::collections::HashMap;

use nrese_dl::tableau::{Answer, Config, Model, consistency};
use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
use nrese_owl::{Concept, Ontology, Term};
use semantics::{Interp, confirms, find_model};

fn env(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// The model as an interpretation of the original signature.
fn interp(m: &Model) -> Option<Interp> {
    if m.size > 128 || m.size == 0 {
        return None;
    }
    let mut i = Interp {
        n: m.size as u32,
        ..Interp::default()
    };
    for &(c, e) in &m.concepts {
        if let Concept::Named(t) = c {
            *i.concepts.entry(t).or_default() |= 1 << e;
        }
    }
    for &(r, a, b) in &m.roles {
        i.add_edge(r, a as u32, b as u32);
    }
    for &(t, e) in &m.individuals {
        i.individuals.insert(t, e as u32);
    }
    Some(i)
}

fn configs() -> Vec<(&'static str, Config)> {
    // Every blocking pass is checked against a recomputation from scratch.
    let base = Config {
        max_nodes: 5_000,
        max_branch_points: Some(5_000),
        timeout: Some(std::time::Duration::from_secs(300)),
        check_blocking: true,
        ..Config::default()
    };
    vec![
        ("default", base.clone()),
        (
            "full-blocking-passes",
            Config {
                incremental_blocking: false,
                ..base.clone()
            },
        ),
        (
            "no-semantic-branching",
            Config {
                semantic_branching: false,
                ..base.clone()
            },
        ),
        (
            "no-backjumping",
            Config {
                backjumping: false,
                ..base.clone()
            },
        ),
        (
            "ancestor-blocking",
            Config {
                anywhere_blocking: false,
                ..base.clone()
            },
        ),
        (
            "pairwise-always",
            Config {
                single_blocking: false,
                ..base.clone()
            },
        ),
        (
            "no-disjunct-learning",
            Config {
                disjunct_learning: false,
                ..base.clone()
            },
        ),
        (
            "dynamic-backtracking",
            Config {
                dynamic_backtracking: true,
                check_retraction: true,
                ..base.clone()
            },
        ),
        (
            "at-most-atoms",
            Config {
                expand_at_most_up_to: 0,
                ..base
            },
        ),
    ]
}

#[derive(Default, Debug)]
struct Tally {
    consistent: u64,
    confirmed_by_model: u64,
    confirmed_by_search: u64,
    unconfirmed: u64,
    inconsistent: u64,
    unsupported: u64,
    gave_up: u64,
    /// Applications of the NI rule in the default runs.
    ni_firings: u64,
}

fn render(o: &Ontology) -> String {
    let name = |t: Term| format!("t{t}");
    o.axioms
        .iter()
        .map(|a| o.functional(a, &name))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Delta debugging: drops axioms while the default run still gives up (a blow-up's
/// minimal witness).
fn shrink(o: &Ontology) {
    let config = Config {
        max_nodes: 3000,
        max_branch_points: Some(5_000),
        timeout: Some(std::time::Duration::from_secs(300)),
        ..Config::default()
    };
    let stuck = |o: &Ontology| matches!(consistency(o, &config).answer, Answer::GaveUp(_));
    let mut cur = o.clone();
    let mut i = 0;
    while i < cur.axioms.len() {
        let mut next = cur.clone();
        next.axioms.remove(i);
        next.sources.remove(i);
        if stuck(&next) {
            cur = next;
        } else {
            i += 1;
        }
    }
    eprintln!(
        "minimal:
{}",
        render(&cur)
    );
    let out = consistency(&cur, &config);
    eprintln!("{:?} {}", out.answer, out.telemetry);
}

/// Every configuration's answer on `o` against the semantics and against each other;
/// renaming and shuffling (`shuffled`) keep the answer.
/// Whether `o` counts: a number restriction, or a functional or inverse-functional
/// property (an at-most-one). A folded model may break those (two predecessors merged by
/// blocking), and the small-model search is then the only confirmation.
fn counts(o: &Ontology) -> bool {
    use nrese_owl::{Axiom, Characteristic, ClassExpr};
    o.axioms.iter().any(|a| {
        matches!(
            a,
            Axiom::ObjectCharacteristic(Characteristic::Functional, _)
                | Axiom::ObjectCharacteristic(Characteristic::InverseFunctional, _)
        )
    }) || (0..o.classes.len()).any(|i| {
        matches!(
            o.classes.get(i as u32),
            ClassExpr::Max(..) | ClassExpr::Exact(..)
        ) || matches!(o.classes.get(i as u32), ClassExpr::Min(n, ..) if *n > 1)
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the campaign's state, threaded through"
)]
fn check_case(
    case: u64,
    o: &Ontology,
    shuffled: Ontology,
    sig: &Signature,
    numbers: bool,
    tally: &mut Tally,
    check: &mut Rng,
    unconfirmed: &mut Vec<String>,
) {
    let all = configs();
    let mut first: Option<Answer> = None;
    // The search for a small model, once per ontology (the configurations agree).
    let mut searched: Option<Option<Interp>> = None;
    // A case past the default's budget: the others run on a small one, checked where they
    // decide all the same (the default seed's case 14 spent 100 of the campaign's 130 s
    // giving up nine times).
    let mut beyond = false;
    for (name, config) in &all {
        let mut config = Config {
            keep_model: true,
            ..config.clone()
        };
        if beyond {
            config.max_nodes = config.max_nodes.min(1_000);
            config.max_branch_points = Some(1_000);
        }
        let out = consistency(o, &config);
        if *name == "default" {
            beyond = matches!(out.answer, Answer::GaveUp(_));
        }
        if *name == "default" {
            tally.ni_firings += out.telemetry.ni_applications;
        }
        if env("NRESE_FUZZ_ONLY").is_some() {
            eprintln!("{name}: {:?} {}", out.answer, out.telemetry);
        }
        if let Some(f) = &first {
            // Gave-up runs may differ by budget; answers may not.
            let decided = |a: &Answer| matches!(a, Answer::Consistent | Answer::Inconsistent);
            if decided(f) && decided(&out.answer) {
                assert_eq!(
                    f,
                    &out.answer,
                    "case {case}: {name} changes the answer\n{}",
                    render(o)
                );
            }
        } else {
            first = Some(out.answer.clone());
        }
        // Every configuration's answer is checked against the semantics.
        match &out.answer {
            Answer::Consistent => {
                // No verdict where the model doesn't fit the reference's 128 elements.
                let verdict = out
                    .model
                    .as_ref()
                    .and_then(interp)
                    .and_then(|i| confirms(o, i));
                let ok = verdict == Some(true);
                if *name == "default" {
                    tally.consistent += 1;
                }
                if ok {
                    if *name == "default" {
                        tally.confirmed_by_model += 1;
                    }
                    continue;
                }
                let found = searched
                    .get_or_insert_with(|| {
                        find_model(
                            o,
                            &sig.classes,
                            &sig.object_properties,
                            &sig.individuals,
                            3,
                            5_000,
                            check,
                        )
                    })
                    .clone();
                if *name == "default" {
                    if found.is_some() {
                        tally.confirmed_by_search += 1;
                    } else {
                        tally.unconfirmed += 1;
                        unconfirmed.push(format!("case {case}:\n{}", render(o)));
                    }
                }
                assert!(
                    found.is_some() || numbers || verdict.is_none(),
                    "case {case} ({name}): consistent, but the folded model fails and no small \
                     model exists, without number restrictions\n{}\nmodel: {:?}",
                    render(o),
                    out.model
                );
            }
            Answer::Inconsistent => {
                if *name == "default" {
                    tally.inconsistent += 1;
                }
                let found = searched
                    .get_or_insert_with(|| {
                        find_model(
                            o,
                            &sig.classes,
                            &sig.object_properties,
                            &sig.individuals,
                            3,
                            5_000,
                            check,
                        )
                    })
                    .clone();
                assert!(
                    found.is_none(),
                    "case {case} ({name}): inconsistent, but this is a model: {found:?}\n{}",
                    render(o)
                );
            }
            Answer::Unsupported(why) => {
                if *name == "default" {
                    tally.unsupported += 1;
                    eprintln!("case {case}: unsupported: {why}");
                }
            }
            Answer::GaveUp(why) => {
                if *name == "default" {
                    tally.gave_up += 1;
                    eprintln!("case {case}: gave up: {why}");
                }
            }
        }
    }
    if beyond {
        // The metamorphic runs compare decided answers only.
        return;
    }
    // Renaming and shuffling leave the answer as it is.
    let base = Config {
        max_nodes: 5_000,
        max_branch_points: Some(5_000),
        timeout: Some(std::time::Duration::from_secs(300)),
        ..Config::default()
    };
    let renamed = fuzz::rename(o, &|t| t + 1000);
    // Complements as OWL Lite writes them, read with the rewriting (`nrese_owl`'s
    // `complements.rs`) and without.
    let mut next = 1_000_000;
    let encoded = fuzz::encode_complements(o, 3, &mut || {
        next += 1;
        next
    });
    let plain = Config {
        complements: false,
        ..base.clone()
    };
    for (what, other, config) in [
        ("renamed", renamed, &base),
        ("shuffled", shuffled, &base),
        ("complements encoded", encoded.clone(), &base),
        ("complements encoded, not rewritten", encoded, &plain),
    ] {
        let answer = consistency(&other, config).answer;
        if let Some(f) = &first
            && matches!(f, Answer::Consistent | Answer::Inconsistent)
            && matches!(answer, Answer::Consistent | Answer::Inconsistent)
        {
            assert_eq!(
                f,
                &answer,
                "case {case}: {what} changes the answer\n{}",
                render(o)
            );
        }
    }
}

#[test]
fn answers_agree_with_the_semantics() {
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(150);
    let seed = env("NRESE_FUZZ_SEED").unwrap_or(0x0020_2610_0333);
    let tally = campaign(seed, cases, env("NRESE_FUZZ_ONLY"));
    assert!(tally.inconsistent > 0 && tally.consistent > 0, "{tally:?}");
}

/// Guard (the oracle): seed 94543, case 143 is consistent (a 3-cycle along a property
/// disjoint with its inverse), and its folded model closes a loop on the blocker; lifted
/// into three copies it is a model (`semantics::lift`). It failed the push gate's
/// new seeds as "consistent, but no model", a confirmation the oracle couldn't make.
#[test]
fn a_loop_folded_onto_its_blocker_is_untied() {
    let tally = campaign(94543, 144, Some(143));
    assert_eq!(tally.consistent, 1, "{tally:?}");
}

/// Guard (dynamic backtracking): seed 131, case 138 is inconsistent (a self-loop on a
/// nominal whose successors need three successors with exactly two each). Retracting level
/// 1 re-decided its disjunction on top; a later backtrack that couldn't retract (a merge
/// since its culprit, level 2) restored level 2's checkpoint, older than the retraction:
/// level 1's first choice stayed retracted, its new one was cut away, and the search found
/// a model without the disjunction. Such a backtrack now starts the search again from
/// before the first retraction. It failed the office campaign as "dynamic-backtracking
/// changes the answer".
#[test]
fn a_backtrack_never_restores_a_point_older_than_a_retraction() {
    let tally = campaign(131, 139, Some(138));
    assert_eq!(tally.inconsistent, 1, "{tally:?}");
}

/// The random cases of `seed` (`only`: that one alone), each checked against the semantics.
fn campaign(seed: u64, cases: u64, only: Option<u64>) -> Tally {
    let mut rng = Rng::new(seed);
    let mut check = Rng::new(seed ^ 0xA5A5);
    let mut tally = Tally::default();
    let mut unconfirmed = Vec::new();
    for case in 0..cases {
        let mut names: HashMap<String, Term> = HashMap::new();
        let mut intern = |n: &Name| {
            let key = format!("{n:?}");
            let len = names.len() as Term;
            *names.entry(key).or_insert(len)
        };
        let sizes = Sizes {
            classes: 3,
            object_properties: 2,
            simple: 1,
            data_properties: 0,
            individuals: 2,
            literals: 0,
        };
        let sig = Signature::new(sizes, &mut intern);
        let profile = Profile {
            axioms: 4 + (case % 5) as usize,
            depth: 2,
            el: false,
            data: false,
            nominals: case.is_multiple_of(3),
            numbers: case.is_multiple_of(2),
            chains: case % 4 == 1,
            abox: true,
        };
        let o = fuzz::ontology(&mut rng, &sig, profile);
        let shuffled = fuzz::shuffle(&o, &mut rng);
        if only.is_some_and(|only| only != case) {
            continue;
        }
        if only.is_some() {
            eprintln!("{}", render(&o));
            if env("NRESE_FUZZ_SHRINK").is_some() {
                shrink(&o);
                return tally;
            }
        }
        check_case(
            case,
            &o,
            shuffled,
            &sig,
            profile.numbers || counts(&o),
            &mut tally,
            &mut check,
            &mut unconfirmed,
        );
    }
    eprintln!("{tally:?}");
    for u in unconfirmed.iter().take(5) {
        eprintln!("unconfirmed {u}");
    }
    tally
}

/// The NI rule's pattern (`ni_gen`): a nominal, a role into it, an at-most restriction on
/// the inverse at it, chains of blockable nodes. A fixed set here; the long runs and the
/// differential runs on HermiT are the `tableau_fuzz` example's `--profile ni --switches`.
#[test]
fn ni_pattern_answers_agree_with_the_semantics() {
    let cases = env("NRESE_FUZZ_NI_CASES").unwrap_or(60);
    let seed = env("NRESE_FUZZ_SEED").unwrap_or(0x0020_2610_0444);
    let mut rng = Rng::new(seed);
    let mut check = Rng::new(seed ^ 0x5A5A);
    let mut tally = Tally::default();
    let mut unconfirmed = Vec::new();
    for case in 0..cases {
        let mut names: HashMap<String, Term> = HashMap::new();
        let mut intern = |n: &Name| {
            let key = format!("{n:?}");
            let len = names.len() as Term;
            *names.entry(key).or_insert(len)
        };
        let sig = Signature::new(ni_gen::sizes(), &mut intern);
        let o = ni_gen::ontology(&mut rng, &sig);
        let shuffled = fuzz::shuffle(&o, &mut rng);
        if env("NRESE_FUZZ_ONLY").is_some_and(|only| only != case) {
            continue;
        }
        check_case(
            case,
            &o,
            shuffled,
            &sig,
            true,
            &mut tally,
            &mut check,
            &mut unconfirmed,
        );
    }
    eprintln!("{tally:?}");
    assert!(tally.ni_firings > 0, "{tally:?}");
    assert!(tally.inconsistent > 0 && tally.consistent > 0, "{tally:?}");
}
