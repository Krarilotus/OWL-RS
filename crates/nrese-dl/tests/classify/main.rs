//! The classification and realisation driver (`nrese_dl::classify`, package 3.4) against
//! brute force on fuzzed ontologies (`nrese_owl::fuzz`):
//!
//! - **Brute force** asks the hypertableau's plain consistency test once per class and per
//!   pair of classes, `O ∪ {C(x), ¬D(x)}` for a fresh individual `x`, and once per
//!   individual and class, `O ∪ {¬D(a)}`: no probe, no labels, no pruning, so it shares
//!   only the engine with the driver.
//! - **Switches:** every optimisation off, and the parallel run, give the same taxonomy.
//! - **Metamorphic:** renaming the terms and shuffling the axioms give the same taxonomy.
//!
//! Cases where brute force or the driver can't decide (a budget, data approximated) are
//! counted and skipped; the campaign must decide most. `NRESE_FUZZ_CASES` and
//! `NRESE_FUZZ_SEED` widen or move it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use nrese_dl::classify::{self, Classification, Options};
use nrese_dl::tableau::{self, Answer, Config};
use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
use nrese_owl::{Axiom, ClassExpr, EntityKind, ExprId, Ontology, Term};

fn env(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

/// A fresh individual for the brute-force tests (no fuzzed term is this large).
const X: Term = 900_000;

fn config() -> Config {
    Config {
        max_nodes: 20_000,
        timeout: Some(Duration::from_secs(2)),
        max_memory: 256 << 20,
        ..Config::default()
    }
}

fn options() -> Options {
    Options {
        tableau: config(),
        timeout: Some(Duration::from_secs(20)),
        ..Options::default()
    }
}

/// `o` with `C(a)` (`positive`) and `¬D(a)` (`negative`).
fn with(o: &Ontology, a: Term, positive: Option<Term>, negative: Option<Term>) -> Ontology {
    let mut o = o.clone();
    o.axioms
        .push(Axiom::Declaration(EntityKind::NamedIndividual, a));
    if let Some(c) = positive {
        let e = ExprId(o.classes.intern(ClassExpr::Class(c)));
        o.axioms.push(Axiom::ClassAssertion(e, a));
    }
    if let Some(d) = negative {
        let e = ExprId(o.classes.intern(ClassExpr::Class(d)));
        let not = ExprId(o.classes.intern(ClassExpr::Not(e)));
        o.axioms.push(Axiom::ClassAssertion(not, a));
    }
    o.sources = vec![Vec::new(); o.axioms.len()];
    o
}

/// Whether `o` is consistent; `None` if the tableau can't say.
fn consistent(o: &Ontology) -> Option<bool> {
    match tableau::consistency(o, &config()).answer {
        Answer::Consistent => Some(true),
        Answer::Inconsistent => Some(false),
        _ => None,
    }
}

/// The classification by one consistency test per class and pair.
fn brute_classification(o: &Ontology, classes: &[Term]) -> Option<Classification> {
    let mut c = Classification {
        classes: classes.to_vec(),
        consistent: consistent(o)?,
        ..Classification::default()
    };
    if !c.consistent {
        c.unsatisfiable = classes.to_vec();
        return Some(c);
    }
    for &a in classes {
        if !consistent(&with(o, X, Some(a), None))? {
            c.unsatisfiable.push(a);
            continue;
        }
        for &b in classes {
            if a != b && !consistent(&with(o, X, Some(a), Some(b)))? {
                c.subsumptions.push((a, b));
            }
        }
    }
    for &b in classes {
        if !consistent(&with(o, X, None, Some(b)))? {
            c.top.push(b);
        }
    }
    c.subsumptions.sort_unstable();
    c.unsatisfiable.sort_unstable();
    c.top.sort_unstable();
    Some(c)
}

/// Each individual's types by one consistency test per individual and class.
fn brute_types(o: &Ontology, classes: &[Term], individuals: &[Term]) -> Option<Vec<Vec<Term>>> {
    let mut out = Vec::new();
    for &a in individuals {
        let mut types = Vec::new();
        for &d in classes {
            if !consistent(&with(o, a, None, Some(d)))? {
                types.push(d);
            }
        }
        out.push(types);
    }
    Some(out)
}

fn render(o: &Ontology) -> String {
    let name = |t: Term| format!("t{t}");
    o.axioms
        .iter()
        .map(|a| o.functional(a, &name))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The driver's switches, each off once, and the parallel run.
fn variants() -> Vec<(&'static str, Options)> {
    let base = options();
    vec![
        ("default", base.clone()),
        (
            "no-context-core",
            Options {
                context_core: false,
                ..base.clone()
            },
        ),
        (
            "no-inline",
            Options {
                inline: false,
                ..base.clone()
            },
        ),
        (
            "no-inline-tableau",
            Options {
                inline: false,
                context_core: false,
                exact_lower_bound: false,
                ..base.clone()
            },
        ),
        (
            "no-exact-shortcut",
            Options {
                context_core: false,
                exact_lower_bound: false,
                ..base.clone()
            },
        ),
        (
            "no-lower-bound",
            Options {
                context_core: false,
                horn_lower_bound: false,
                ..base.clone()
            },
        ),
        (
            "no-model-pruning",
            Options {
                context_core: false,
                exact_lower_bound: false,
                model_pruning: false,
                ..base.clone()
            },
        ),
        (
            "no-skip-seen",
            Options {
                context_core: false,
                exact_lower_bound: false,
                skip_seen: false,
                ..base.clone()
            },
        ),
        (
            "no-tbox-only",
            Options {
                context_core: false,
                exact_lower_bound: false,
                tbox_only: false,
                ..base.clone()
            },
        ),
        (
            "parallel",
            Options {
                context_core: false,
                exact_lower_bound: false,
                threads: 3,
                ..base
            },
        ),
    ]
}

#[derive(Debug, Default)]
struct Tally {
    cases: u64,
    compared: u64,
    brute_undecided: u64,
    driver_incomplete: u64,
    inconsistent: u64,
    with_subsumptions: u64,
    with_unsat: u64,
    candidate_tests: u64,
    realised: u64,
    with_types: u64,
}

fn signature(case: u64, rng: &mut Rng) -> (Ontology, Signature) {
    let mut names: HashMap<String, Term> = HashMap::new();
    let mut intern = |n: &Name| {
        let key = format!("{n:?}");
        let len = names.len() as Term;
        *names.entry(key).or_insert(len)
    };
    let sizes = Sizes {
        classes: 4,
        object_properties: 2,
        simple: 1,
        data_properties: u32::from(case % 4 == 3),
        individuals: 2,
        literals: 2,
    };
    let sig = Signature::new(sizes, &mut intern);
    let profile = Profile {
        axioms: 4 + (case % 5) as usize,
        depth: 2,
        el: false,
        data: case % 4 == 3,
        nominals: case.is_multiple_of(3),
        numbers: case.is_multiple_of(2),
        chains: case % 4 == 1,
        abox: !case.is_multiple_of(5),
    };
    (fuzz::ontology(rng, &sig, profile), sig)
}

#[test]
fn taxonomies_equal_brute_force() {
    let started = Instant::now();
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(120);
    let seed = env("NRESE_FUZZ_SEED").unwrap_or(0x0020_2610_0534);
    let mut rng = Rng::new(seed);
    let mut tally = Tally::default();
    for case in 0..cases {
        // The test's own budget: a campaign never runs on unbounded.
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "over the budget at case {case}: {tally:?}"
        );
        let (o, _) = signature(case, &mut rng);
        let shuffled = fuzz::shuffle(&o, &mut rng);
        if env("NRESE_FUZZ_ONLY").is_some_and(|only| only != case) {
            continue;
        }
        tally.cases += 1;
        let classes = nrese_dl::context::signature(&o);
        let Some(expected) = brute_classification(&o, &classes) else {
            tally.brute_undecided += 1;
            continue;
        };
        if env("NRESE_FUZZ_ONLY").is_some() {
            let p = tableau::prepared(&o);
            let n = nrese_owl::normalise(&p);
            let prep = tableau::Prepared::new(&p, &n, &classes, &[]);
            for i in 0..classes.len() as u32 {
                let out = prep.probe(
                    &tableau::Probe {
                        at: tableau::At::Fresh,
                        positive: &[i],
                        negative: &[],
                    },
                    &config(),
                    tableau::Want {
                        elements: true,
                        individuals: false,
                    },
                );
                eprintln!(
                    "probe {i}: {:?} {:?} {}",
                    out.answer, out.labels, out.telemetry
                );
            }
            for cl in &n.clauses {
                eprintln!("clause {:?} -> {:?}", cl.body, cl.head);
            }
        }
        let mut decided = false;
        for (name, opts) in variants() {
            let t = classify::classify(&o, &opts);
            if env("NRESE_FUZZ_ONLY").is_some() {
                eprintln!(
                    "{name}: {:?} {:?}
  {}",
                    t.classification,
                    t.incomplete,
                    t.profile.line()
                );
            }
            if !t.complete() {
                tally.driver_incomplete += 1;
                continue;
            }
            decided = true;
            if name == "default" {
                tally.candidate_tests += t.profile.candidate_tests;
            }
            assert_eq!(
                t.classification,
                expected,
                "case {case}, {name}: the taxonomy differs from brute force ({:?})\n{}",
                t.incomplete,
                render(&o)
            );
        }
        if !decided {
            continue;
        }
        tally.compared += 1;
        tally.inconsistent += u64::from(!expected.consistent);
        tally.with_subsumptions += u64::from(!expected.subsumptions.is_empty());
        tally.with_unsat += u64::from(expected.consistent && !expected.unsatisfiable.is_empty());
        // Metamorphic: renamed and shuffled.
        let renamed = fuzz::rename(&o, &|t| t + 1000);
        let back = |c: &Classification| {
            let f = |t: &Term| t - 1000;
            let mut c = Classification {
                classes: c.classes.iter().map(f).collect(),
                subsumptions: c.subsumptions.iter().map(|(a, b)| (f(a), f(b))).collect(),
                unsatisfiable: c.unsatisfiable.iter().map(f).collect(),
                top: c.top.iter().map(f).collect(),
                consistent: c.consistent,
            };
            c.subsumptions.sort_unstable();
            c
        };
        let opts = Options {
            context_core: false,
            ..options()
        };
        let r = classify::classify(&renamed, &opts);
        if r.complete() {
            assert_eq!(
                back(&r.classification),
                expected,
                "case {case}: renaming\n{}",
                render(&o)
            );
        }
        let s = classify::classify(&shuffled, &opts);
        if s.complete() {
            assert_eq!(
                s.classification,
                expected,
                "case {case}: shuffling\n{}",
                render(&o)
            );
        }
    }
    eprintln!("{tally:?} in {:?}", started.elapsed());
    assert!(
        tally.compared * 4 >= tally.cases * 3,
        "too few cases decided: {tally:?}"
    );
    assert!(
        tally.with_subsumptions > 0 && tally.inconsistent > 0,
        "{tally:?}"
    );
    assert!(
        tally.candidate_tests > 0,
        "no candidate was ever tested: {tally:?}"
    );
}

#[test]
fn types_equal_brute_force() {
    let started = Instant::now();
    let cases = env("NRESE_FUZZ_CASES").unwrap_or(120);
    let seed = env("NRESE_FUZZ_SEED").unwrap_or(0x0020_2610_0535);
    let mut rng = Rng::new(seed);
    let mut tally = Tally::default();
    for case in 0..cases {
        assert!(
            started.elapsed() < Duration::from_secs(600),
            "over the budget at case {case}: {tally:?}"
        );
        let (o, _) = signature(case, &mut rng);
        if env("NRESE_FUZZ_ONLY").is_some_and(|only| only != case) {
            continue;
        }
        tally.cases += 1;
        let classes = nrese_dl::context::signature(&o);
        let individuals = classify::realise::individuals(&o);
        if individuals.is_empty() {
            continue;
        }
        match consistent(&o) {
            Some(true) => {}
            Some(false) => continue,
            None => {
                tally.brute_undecided += 1;
                continue;
            }
        }
        let Some(expected) = brute_types(&o, &classes, &individuals) else {
            tally.brute_undecided += 1;
            continue;
        };
        for threads in [1, 3] {
            let r = classify::realise(
                &o,
                &Options {
                    threads,
                    ..options()
                },
            );
            if !r.incomplete.is_empty() {
                tally.driver_incomplete += 1;
                continue;
            }
            assert_eq!(r.individuals, individuals);
            assert_eq!(
                r.types,
                expected,
                "case {case}, {threads} threads: the types differ from brute force\n{}",
                render(&o)
            );
            tally.realised += 1;
        }
        tally.with_types += u64::from(expected.iter().any(|t| !t.is_empty()));
    }
    eprintln!("{tally:?} in {:?}", started.elapsed());
    assert!(tally.realised > 0 && tally.with_types > 0, "{tally:?}");
}

/// A taxonomy only a test of a candidate shows: `A ⊑ B ⊔ C`, `B ⊑ D`, `C ⊑ D` gives
/// `A ⊑ D`, which no deterministic label holds; and `E ≡ ¬F ⊓ F` is unsatisfiable.
#[test]
fn a_subsumption_through_a_disjunction() {
    let mut o = Ontology::default();
    let [a, b, c, d, e, f] = [1, 2, 3, 4, 5, 6];
    let class = |o: &mut Ontology, t: Term| ExprId(o.classes.intern(ClassExpr::Class(t)));
    let (ea, eb, ec, ed, ee, ef) = (
        class(&mut o, a),
        class(&mut o, b),
        class(&mut o, c),
        class(&mut o, d),
        class(&mut o, e),
        class(&mut o, f),
    );
    let or = ExprId(o.classes.intern(ClassExpr::Or(vec![eb, ec])));
    let nf = ExprId(o.classes.intern(ClassExpr::Not(ef)));
    let both = ExprId(o.classes.intern(ClassExpr::And(vec![ef, nf])));
    o.axioms = vec![
        Axiom::SubClassOf(ea, or),
        Axiom::SubClassOf(eb, ed),
        Axiom::SubClassOf(ec, ed),
        Axiom::EquivalentClasses(vec![ee, both]),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    let t = classify::classify(&o, &options());
    assert!(t.complete(), "{:?}", t.incomplete);
    let c = &t.classification;
    assert!(c.subsumptions.contains(&(a, d)), "{c:?}");
    assert!(!c.subsumptions.contains(&(a, b)), "{c:?}");
    assert_eq!(c.unsatisfiable, vec![e]);
    assert_eq!(t.profile.path, "tableau");
    assert!(t.profile.positive >= 1, "{}", t.profile.line());
    let name = |t: Term| format!("http://e/{t}");
    let text = c.canonical(&name);
    assert!(text.contains("< http://e/1 http://e/4\n"), "{text}");
    assert!(!text.contains("< http://e/1 http://www.w3.org"), "{text}");
}
