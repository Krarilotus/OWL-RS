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
            "no-reuse-model",
            Options {
                context_core: false,
                exact_lower_bound: false,
                reuse_model: false,
                ..base.clone()
            },
        ),
        (
            "no-detached-probes",
            Options {
                context_core: false,
                exact_lower_bound: false,
                detached_probes: false,
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
    /// Classes the Horn part decided alone (in the tableau-path variant).
    exact_classes: u64,
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
                        detached: true,
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
        if env("NRESE_FUZZ_ONLY").is_some() {
            // Every variant first, for the diagnosis.
            for (name, opts) in variants() {
                let t = classify::classify(&o, &opts);
                eprintln!("variant {name}: {:?}", t.classification);
            }
        }
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
            if name == "no-context-core" {
                tally.exact_classes += t.profile.exact_classes as u64;
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
    assert!(
        tally.exact_classes > 0,
        "no class was ever exact: {tally:?}"
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
            Some(false) => {
                // Every individual has every type; the canonical form is empty.
                let r = classify::realise(&o, &options());
                if r.incomplete.is_empty() {
                    assert!(!r.taxonomy.classification.consistent, "case {case}");
                    assert!(r.types.iter().all(|t| *t == classes), "case {case}");
                    assert_eq!(r.canonical(&|t| format!("t{t}")), "");
                }
                continue;
            }
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

/// Guard (saturation coupling): a terminology the Horn stage refuses as a whole (a
/// functional property, so equality) whose Horn part is exact for every class: the
/// driver decides all 2,000 classes and `owl:Thing` from it and runs no hypertableau
/// test of a class (ore_ont_7127, 65 k classes: 43 s of tests before, none after).
#[test]
fn an_exact_horn_part_needs_no_class_test() {
    let mut o = Ontology::default();
    let n: Term = 2000;
    let (r, s) = (100_000, 100_001);
    let class = |o: &mut Ontology, t: Term| ExprId(o.classes.intern(ClassExpr::Class(t)));
    for i in 0..n {
        let (a, b) = (class(&mut o, i), class(&mut o, i + 1));
        let some = ExprId(
            o.classes
                .intern(ClassExpr::Some(nrese_owl::ObjProp::Named(r), b)),
        );
        o.axioms.push(Axiom::SubClassOf(a, some));
        if i % 2 == 0 {
            o.axioms.push(Axiom::SubClassOf(a, b));
        }
    }
    o.axioms.push(Axiom::ObjectCharacteristic(
        nrese_owl::Characteristic::Functional,
        nrese_owl::ObjProp::Named(s),
    ));
    o.sources = vec![Vec::new(); o.axioms.len()];
    let t = classify::classify(&o, &options());
    assert!(t.complete(), "{:?}", t.incomplete);
    assert_eq!(t.profile.path, "tableau", "{}", t.profile.line());
    assert_eq!(
        t.profile.exact_classes,
        n as usize + 1,
        "{}",
        t.profile.line()
    );
    assert_eq!(
        t.profile.sat_tests + t.profile.candidate_tests,
        0,
        "{}",
        t.profile.line()
    );
    assert!(t.classification.subsumptions.contains(&(0, 1)));
    assert!(!t.classification.subsumptions.contains(&(1, 2)));
}

/// Found by the campaign (seed 55, case 6108): `C1 ≡ ∃r.C3`, `range(r) = C0 ⊔ C2`,
/// `C0 ⊑ C2`, `C2 ⊓ C3 ⊑ ⊥`: `C1` is unsatisfiable. The Horn part (without the range's
/// disjunction) has `C1` satisfiable, and its context for `C1` holds the range's fresh
/// name only as a head about the successor, `Q(f(x))` (no clause of the part reads `Q`,
/// so the successor's context never gets it): the exactness check must see such heads.
#[test]
fn a_disjunctive_range_on_a_successor_is_not_exact() {
    use nrese_owl::ObjProp;
    let mut o = Ontology::default();
    let class = |o: &mut Ontology, t: Term| ExprId(o.classes.intern(ClassExpr::Class(t)));
    let (c0, c1, c2, c3) = (
        class(&mut o, 0),
        class(&mut o, 1),
        class(&mut o, 2),
        class(&mut o, 3),
    );
    let r = ObjProp::Named(4);
    let some = ExprId(o.classes.intern(ClassExpr::Some(r, c3)));
    let or = ExprId(o.classes.intern(ClassExpr::Or(vec![c0, c2])));
    let both = ExprId(o.classes.intern(ClassExpr::And(vec![c2, c3])));
    let nothing = ExprId(o.classes.intern(ClassExpr::Nothing));
    o.axioms = vec![
        Axiom::SubClassOf(c0, c2),
        Axiom::SubClassOf(both, nothing),
        Axiom::EquivalentClasses(vec![c1, some]),
        Axiom::ObjectPropertyRange(r, or),
    ];
    o.sources = vec![Vec::new(); o.axioms.len()];
    for (name, opts) in variants() {
        let t = classify::classify(&o, &opts);
        assert!(t.complete(), "{name}: {:?}", t.incomplete);
        assert_eq!(t.classification.unsatisfiable, vec![1], "{name}");
    }
}

/// Guard (detached probes): with a nominal in the terminology the individuals can
/// matter, but a class whose model never reaches one is answered on the terminology
/// alone: here `A ⊑ ∃r.B` and `B ⊑ C` never reach `a` (only `D ⊑ {a}` does), so only
/// `D`'s test runs again with the 100 assertions (ore_ont_9881: 77.8 -> 64.0 s).
#[test]
fn classes_away_from_the_individuals_need_no_assertions() {
    use nrese_owl::ObjProp;
    let mut o = Ontology::default();
    let class = |o: &mut Ontology, t: Term| ExprId(o.classes.intern(ClassExpr::Class(t)));
    let (a, b, c, d, e) = (
        class(&mut o, 1),
        class(&mut o, 2),
        class(&mut o, 3),
        class(&mut o, 4),
        class(&mut o, 5),
    );
    let ind: Term = 100;
    let r = ObjProp::Named(50);
    let some = ExprId(o.classes.intern(ClassExpr::Some(r, b)));
    let one = ExprId(o.classes.intern(ClassExpr::OneOf(vec![ind])));
    let or = ExprId(o.classes.intern(ClassExpr::Or(vec![c, e])));
    o.axioms = vec![
        Axiom::SubClassOf(a, some),
        Axiom::SubClassOf(b, c),
        Axiom::SubClassOf(d, one),
        Axiom::SubClassOf(e, or),
    ];
    for i in 0..50 {
        o.axioms.push(Axiom::ClassAssertion(e, 200 + i));
        o.axioms
            .push(Axiom::ObjectPropertyAssertion(50, 200 + i, ind));
    }
    o.sources = vec![Vec::new(); o.axioms.len()];
    let opts = Options {
        context_core: false,
        exact_lower_bound: false,
        ..options()
    };
    let t = classify::classify(&o, &opts);
    assert!(t.complete(), "{:?}", t.incomplete);
    // Every test but `D`'s (its model is `a`) stays on the terminology.
    assert_eq!(t.profile.detached, 4, "{}", t.profile.line());
    assert_eq!(t.profile.fallbacks, 1, "{}", t.profile.line());
    assert!(t.classification.subsumptions.contains(&(2, 3)));
}

/// Guard (completion-graph reuse): class tests that reach an individual start from the
/// individuals' model built once: 20 classes `Dᵢ ⊑ ∃r.{n}` with 300 individuals beside
/// `n`, each test adds a node or two instead of rebuilding the 300 (ore_ont_16542:
/// unsolved in 150 s -> 5.8 s; ore_ont_9881 81.3 -> 2.9 s).
#[test]
fn tests_with_individuals_start_from_their_model() {
    use nrese_owl::ObjProp;
    let mut o = Ontology::default();
    let class = |o: &mut Ontology, t: Term| ExprId(o.classes.intern(ClassExpr::Class(t)));
    let n: Term = 1000;
    let r = 999;
    let one = ExprId(o.classes.intern(ClassExpr::OneOf(vec![n])));
    let to_n = ExprId(o.classes.intern(ClassExpr::Some(ObjProp::Named(r), one)));
    let (e, f) = (class(&mut o, 50), class(&mut o, 51));
    let either = ExprId(o.classes.intern(ClassExpr::Or(vec![e, f])));
    for i in 0..20 {
        let d = class(&mut o, i);
        o.axioms.push(Axiom::SubClassOf(d, to_n));
    }
    for i in 0..300 {
        o.axioms.push(Axiom::ClassAssertion(either, 2000 + i));
        o.axioms
            .push(Axiom::ObjectPropertyAssertion(r, 2000 + i, n));
    }
    o.sources = vec![Vec::new(); o.axioms.len()];
    let opts = Options {
        context_core: false,
        exact_lower_bound: false,
        ..options()
    };
    let t = classify::classify(&o, &opts);
    assert!(t.complete(), "{:?}", t.incomplete);
    let p = &t.profile;
    assert!(p.from_model >= 20, "{}", p.line());
    assert_eq!(p.from_deterministic, 0, "{}", p.line());
    // Without the base every one of these tests builds the 301 individuals again.
    assert!(p.nodes_created < 2_000, "{}", p.line());
    let off = classify::classify(
        &o,
        &Options {
            reuse_model: false,
            ..opts
        },
    );
    assert_eq!(off.classification, t.classification);
    assert!(
        off.profile.nodes_created > 20 * 300,
        "{}",
        off.profile.line()
    );
}
