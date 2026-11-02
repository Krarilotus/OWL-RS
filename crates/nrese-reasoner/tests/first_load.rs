//! A first load is materialised once (`engine::maintain`), and its verdict and stack are the
//! delta executor's: every equality mode and plan reports the same violations and keeps the
//! recomputed stack, and a violation the axioms had before stays tolerated. Sol's round 1 on
//! DMW-AB-mem (N-6), F1 and F2: both failed with the closure's own violations.

use nrese_engine::{Engine, EngineConfig, QuadPattern, ReadModel};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_reasoner::{
    delta::policy::Plan,
    engine::{Program, maintain, materialise},
    rulesets::Ruleset,
};

const PREFIX: &str = "@prefix ex: <http://example.com/> . @prefix owl: <http://www.w3.org/2002/07/owl#> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> . @prefix xsd: <http://www.w3.org/2001/XMLSchema#> . ";

/// Each fixture under every equality mode and plan: the same violations and diagnostics,
/// and the recomputed stack where it commits.
#[test]
fn a_first_load_reports_what_every_plan_reports() {
    let fixtures = [
        (
            "functional-literals",
            "ex:age a owl:FunctionalProperty . ex:a ex:age 42, 43 .",
        ),
        (
            "datatype-range",
            "ex:age rdfs:range xsd:nonNegativeInteger . ex:a ex:age -1 .",
        ),
        (
            "aliased-disjoint",
            "ex:C owl:disjointWith ex:D . ex:a a ex:C, ex:D ; owl:sameAs ex:b .",
        ),
        (
            "same-different",
            "ex:a owl:sameAs ex:b ; owl:differentFrom ex:b .",
        ),
        (
            "all-different",
            "[] a owl:AllDifferent ; owl:distinctMembers (ex:a ex:b) . ex:a owl:sameAs ex:b .",
        ),
        ("malformed-list", "ex:C owl:intersectionOf (ex:D ex:E) ."),
    ];
    let mut failures = Vec::new();
    for representatives in [false, true] {
        for compact in [false, true] {
            if compact && !representatives {
                continue;
            }
            for (name, data) in fixtures {
                let mut outcomes = Vec::new();
                for plan in [
                    None,
                    Some(Plan::Dred),
                    Some(Plan::Recompute),
                    Some(Plan::Rebuild),
                ] {
                    let engine = Engine::new(EngineConfig {
                        background_maintenance: false,
                        ..EngineConfig::default()
                    })
                    .unwrap();
                    let mut tx = engine.transaction();
                    let program = Program::compile(&Ruleset::Owl2Rl.into(), &|t| tx.intern(t))
                        .by_representatives(representatives)
                        .storing_representatives(compact)
                        .maintaining_by(plan);
                    if compact {
                        engine.set_equality(Some(
                            tx.intern(
                                nrese_rdf::NamedNodeRef::new_unchecked(
                                    "http://www.w3.org/2002/07/owl#sameAs",
                                )
                                .into(),
                            ),
                        ));
                    }
                    let text = format!("{PREFIX}{data}");
                    for quad in
                        RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes())
                    {
                        tx.insert(quad.unwrap().as_ref());
                    }
                    let done = maintain(&program, None, &mut tx, &|| false).unwrap();
                    let mut violations = done
                        .violations
                        .iter()
                        .map(|v| format!("{}:{:?}", v.rule, v.bindings))
                        .collect::<Vec<_>>();
                    violations.sort();
                    let diagnostics = format!("{:?}", done.diagnostics);
                    outcomes.push((violations, diagnostics));
                    if done.violations.is_empty() {
                        tx.commit().unwrap();
                        let snapshot = engine.snapshot();
                        let stored: std::collections::HashSet<_> = snapshot
                            .stored()
                            .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
                            .map(nrese_engine::EncodedTriple::from)
                            .collect();
                        let fresh: std::collections::HashSet<_> = materialise(&program, &snapshot)
                            .inferred
                            .into_iter()
                            .filter(|t| {
                                !snapshot.contains_in(ReadModel::Asserted, &t.in_default_graph())
                            })
                            .collect();
                        assert_eq!(
                            stored, fresh,
                            "stack: {name} reps={representatives} compact={compact} plan={plan:?}"
                        );
                    }
                }
                if outcomes.iter().any(|o| o != &outcomes[0]) {
                    failures.push(format!(
                        "{name} reps={representatives} compact={compact}: {outcomes:?}"
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A user rule's fact makes the empty state inconsistent; an unrelated insert introduces
/// nothing and is reported as the delta reports it.
#[test]
fn a_first_load_tolerates_a_violation_the_axioms_had_before() {
    use nrese_reasoner::{RuleProgram, UserRules};
    let user = std::sync::Arc::new(
        UserRules::n3("axiom.n3", format!("{PREFIX}ex:old a owl:Nothing .")).unwrap(),
    );
    let rules = RuleProgram::new(Some(Ruleset::Owl2Rl), Some(user)).unwrap();
    let mut outcomes = Vec::new();
    for plan in [None, Some(Plan::Dred)] {
        let engine = Engine::new(EngineConfig {
            background_maintenance: false,
            ..EngineConfig::default()
        })
        .unwrap();
        let tx = engine.transaction();
        let program = Program::compile(&rules, &|t| tx.intern(t)).maintaining_by(plan);
        drop(tx);
        let remat = engine.rematerialisation();
        let closure = materialise(&program, remat.base());
        assert!(
            !closure.violations.is_empty(),
            "preexisting axiom violation"
        );
        remat.finish(closure.inferred).unwrap();
        assert_eq!(engine.snapshot().len_in(ReadModel::Asserted), 0);
        let ground = program.ground_program(&engine.snapshot());
        let mut tx = engine.transaction();
        for quad in RdfParser::from_format(RdfFormat::Turtle)
            .for_reader(format!("{PREFIX}ex:new ex:p ex:o .").as_bytes())
        {
            tx.insert(quad.unwrap().as_ref());
        }
        let done = maintain(&program, Some(&ground), &mut tx, &|| false).unwrap();
        outcomes.push(
            done.violations
                .iter()
                .map(|v| format!("{}:{:?}", v.rule, v.bindings))
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(
        outcomes[0], outcomes[1],
        "first load must keep introduced-only verdicts"
    );
}

/// The same for a conclusion no store holds: an axiom's literal outside its range
/// (`-1 a xsd:nonNegativeInteger`) is old, so an unrelated insert doesn't report its
/// `dt-not-type`, cached grounding or not, in both non-compact equality modes (Sol r2 F1).
#[test]
fn a_first_load_tolerates_a_datatype_violation_the_axioms_had_before() {
    use nrese_reasoner::{RuleProgram, UserRules};
    let user = std::sync::Arc::new(
        UserRules::n3(
            "old-datatype.n3",
            format!("{PREFIX}ex:age rdfs:range xsd:nonNegativeInteger . ex:old ex:age -1 ."),
        )
        .unwrap(),
    );
    let rules = RuleProgram::new(Some(Ruleset::Owl2Rl), Some(user)).unwrap();
    for representatives in [false, true] {
        for cached in [false, true] {
            let mut outcomes = Vec::new();
            for plan in [None, Some(Plan::Dred)] {
                let engine = Engine::new(EngineConfig {
                    background_maintenance: false,
                    ..EngineConfig::default()
                })
                .unwrap();
                let tx = engine.transaction();
                let program = Program::compile(&rules, &|t| tx.intern(t))
                    .by_representatives(representatives)
                    .storing_representatives(false)
                    .maintaining_by(plan);
                drop(tx);
                let remat = engine.rematerialisation();
                let closure = materialise(&program, remat.base());
                assert!(closure.violations.iter().any(|v| v.rule == "dt-not-type"));
                remat.finish(closure.inferred).unwrap();
                assert_eq!(engine.snapshot().len_in(ReadModel::Asserted), 0);
                let ground = program.ground_program(&engine.snapshot());
                let mut tx = engine.transaction();
                for quad in RdfParser::from_format(RdfFormat::Turtle)
                    .for_reader(format!("{PREFIX}ex:new ex:p ex:o .").as_bytes())
                {
                    tx.insert(quad.unwrap().as_ref());
                }
                let done =
                    maintain(&program, cached.then_some(&ground), &mut tx, &|| false).unwrap();
                let mut violations = done
                    .violations
                    .iter()
                    .map(|v| format!("{}:{:?}", v.rule, v.bindings))
                    .collect::<Vec<_>>();
                violations.sort();
                outcomes.push(violations);
            }
            assert!(outcomes[1].is_empty(), "DRed control: {outcomes:?}");
            assert_eq!(
                outcomes[0], outcomes[1],
                "introduced-only parity: representatives={representatives} cached={cached}"
            );
        }
    }
}

/// A new assertion that derives again a conclusion no store holds (`ex:new ex:age -1` beside
/// the axiom's `ex:old ex:age -1`): new to the delta, which reports its `dt-not-type`. Such
/// a closure takes the delta executor, and the verdicts agree (Sol r3 F1).
#[test]
fn a_first_load_that_concludes_an_unstorable_fact_takes_the_delta() {
    use nrese_reasoner::{RuleProgram, UserRules};
    let user = std::sync::Arc::new(
        UserRules::n3(
            "old-datatype.n3",
            format!("{PREFIX}ex:age rdfs:range xsd:nonNegativeInteger . ex:old ex:age -1 ."),
        )
        .unwrap(),
    );
    let rules = RuleProgram::new(Some(Ruleset::Owl2Rl), Some(user)).unwrap();
    for representatives in [false, true] {
        for cached in [false, true] {
            let mut outcomes = Vec::new();
            for plan in [None, Some(Plan::Dred)] {
                let engine = Engine::new(EngineConfig {
                    background_maintenance: false,
                    ..EngineConfig::default()
                })
                .unwrap();
                let tx = engine.transaction();
                let program = Program::compile(&rules, &|t| tx.intern(t))
                    .by_representatives(representatives)
                    .storing_representatives(false)
                    .maintaining_by(plan);
                drop(tx);
                let remat = engine.rematerialisation();
                let closure = materialise(&program, remat.base());
                assert!(closure.violations.iter().any(|v| v.rule == "dt-not-type"));
                remat.finish(closure.inferred).unwrap();
                assert_eq!(engine.snapshot().len_in(ReadModel::Asserted), 0);
                let ground = program.ground_program(&engine.snapshot());
                let mut tx = engine.transaction();
                for quad in RdfParser::from_format(RdfFormat::Turtle)
                    .for_reader(format!("{PREFIX}ex:new ex:age -1 .").as_bytes())
                {
                    tx.insert(quad.unwrap().as_ref());
                }
                let done =
                    maintain(&program, cached.then_some(&ground), &mut tx, &|| false).unwrap();
                if plan.is_none() {
                    assert_ne!(
                        done.work.plan,
                        Plan::Rebuild,
                        "a literal's conclusion takes the delta"
                    );
                }
                let mut violations = done
                    .violations
                    .iter()
                    .map(|v| format!("{}:{:?}", v.rule, v.bindings))
                    .collect::<Vec<_>>();
                violations.sort();
                outcomes.push(violations);
            }
            assert!(
                outcomes[1].iter().any(|v| v.starts_with("dt-not-type:")),
                "DRed must report the new derivation: {outcomes:?}"
            );
            assert_eq!(
                outcomes[0], outcomes[1],
                "introduced-only parity: representatives={representatives} cached={cached}"
            );
        }
    }
}
