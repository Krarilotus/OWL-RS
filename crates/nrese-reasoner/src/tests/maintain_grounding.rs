use super::*;
use nrese_engine::{Engine, EngineConfig};

/// Spec: grounding a certified snapshot and grounding the same base through the
/// cold commit adapter give the same rule instances, premises and list tables.
#[test]
fn snapshot_and_cold_commit_ground_the_same_program() {
    use crate::eval::{GroundProgram, Grounding};
    use crate::ir::{Head, Rule, Term};
    use nrese_rdf_io::{RdfFormat, RdfParser};
    use std::fmt::Write as _;
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let mut input = "@prefix ex: <http://example.org/> .
        @prefix owl: <http://www.w3.org/2002/07/owl#> .
        @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
        @prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
        ex:p owl:propertyChainAxiom (ex:q ex:r) .
        ex:wide owl:propertyChainAxiom ex:c0 .
        ex:ancestor a owl:TransitiveProperty .
        ex:A rdfs:subClassOf ex:B .
        ex:x a ex:A ; ex:q ex:y . ex:y ex:r ex:z .
        ex:B owl:disjointWith ex:C .
        _:u owl:unionOf (ex:A ex:B) .\n"
        .to_owned();
    // Seven binary cells give 128 walks, above the 64-walk enumeration limit.
    for index in 0..7 {
        let next = index + 1;
        let rest = if next == 7 {
            "rdf:nil".to_owned()
        } else {
            format!("ex:c{next}")
        };
        writeln!(
            input,
            "ex:c{index} rdf:first ex:edge, ex:alternate ; rdf:rest {rest} . \
             ex:n{index} ex:edge ex:n{next} ."
        )
        .unwrap();
    }
    for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(input.as_bytes()) {
        tx.insert(quad.unwrap().as_ref());
    }
    let rules = crate::rulesets::Ruleset::Owl2Rl.into();
    let program = Program::compile(&rules, &|term| tx.intern(term))
        .hiding_unnamed_classes(true)
        .unwrap();
    tx.commit().unwrap();
    let remat = engine.rematerialisation();
    let closure = super::super::materialise(&program, remat.base());
    remat.finish(closure.inferred).unwrap();
    let tx = engine.transaction();
    let base = tx.base();
    let adapter = EngineBase::new(&tx, program.axioms());
    let schema = program.schema_for(base);
    let rules = program.rules_for(base);
    let cold = delta::program(
        &adapter,
        delta::Rules {
            rules: &rules,
            schema: &schema,
            ..program.rules()
        },
    );
    let prepared = program.ground_program(base);
    let (prepared_cells, prepared_predicate) = prepared.list_predicates.by_cell();
    let (cold_cells, cold_predicate) = cold.list_predicates.by_cell();
    assert_eq!(prepared_cells, cold_cells);
    // A private relation belongs to the RDF list cell, whichever index it got.
    let rule_at = |rule: &Rule, predicate: &dyn Fn(u64) -> u64| {
        let mut rule = rule.clone();
        let heads = match &mut rule.head {
            Head::Facts(heads) => heads.as_mut_slice(),
            Head::Inconsistent => &mut [],
        };
        for atom in rule.body.iter_mut().chain(heads) {
            if let Term::Const(id) = &mut atom.0[1] {
                *id = predicate(*id);
            }
        }
        format!("{rule:?}")
    };
    let instances = |g: &GroundProgram, predicate: &dyn Fn(u64) -> u64| {
        g.rules
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                let mut sets: Vec<_> = g
                    .unpolled()
                    .premise_alternatives(Grounding::Rule(index))
                    .into_iter()
                    .map(<[_]>::to_vec)
                    .collect();
                sets.sort();
                (rule_at(rule, predicate), sets, g.equality[index])
            })
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        instances(&prepared, &prepared_predicate),
        instances(&cold, &cold_predicate)
    );
    assert_eq!(prepared.bodiless, cold.bodiless);
    assert_eq!(prepared.transitive, cold.transitive);
    for grounding in prepared
        .bodiless
        .iter()
        .copied()
        .map(Grounding::Bodiless)
        .chain(
            prepared
                .transitive
                .iter()
                .copied()
                .map(Grounding::Transitive),
        )
    {
        let alternatives = |g: &GroundProgram| {
            g.unpolled()
                .premise_alternatives(grounding)
                .into_iter()
                .map(<[_]>::to_vec)
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(alternatives(&prepared), alternatives(&cold));
    }
    for quad in base.quads_for_pattern_in(ReadModel::Materialised, &QuadPattern::all()) {
        let produced = |g: &GroundProgram, predicate: &dyn Fn(u64) -> u64| {
            g.unpolled()
                .producers(triple(quad))
                .into_iter()
                .map(|i| rule_at(&g.rules[i], predicate))
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            produced(&prepared, &prepared_predicate),
            produced(&cold, &cold_predicate)
        );
    }
    let consistency = |g: &GroundProgram| {
        g.consistency
            .iter()
            .map(|(rule, bound)| {
                (
                    format!("{rule:?}/{:?}", bound.rule),
                    bound.substitution.clone(),
                )
            })
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(consistency(&prepared), consistency(&cold));
    assert_eq!(prepared.list_diagnostics, cold.list_diagnostics);
    let table = |g: &GroundProgram, predicate: &dyn Fn(u64) -> u64| {
        let mut rows = BTreeSet::new();
        g.table
            .as_ref()
            .unwrap()
            .scan_table(&Poller::never(), |mut fact| {
                fact[1] = predicate(fact[1]);
                rows.insert(
                    fact.map(|id| super::super::decoded(base.decode(TermId::from_raw(id)), id)),
                );
            })
            .unwrap();
        rows
    };
    let rows = table(&prepared, &prepared_predicate);
    assert!(!rows.is_empty());
    assert_eq!(rows, table(&cold, &cold_predicate));
    let templates = |g: &GroundProgram, predicate: &dyn Fn(u64) -> u64| {
        g.operand_templates().map(|templates| {
            templates
                .iter()
                .map(|(rule, premises)| (rule_at(rule, predicate), premises.to_vec()))
                .collect::<BTreeSet<_>>()
        })
    };
    assert_eq!(
        templates(&prepared, &prepared_predicate),
        templates(&cold, &cold_predicate)
    );
    let table_rules = |g: &GroundProgram, predicate: &dyn Fn(u64) -> u64| {
        g.table_rules
            .iter()
            .map(|&index| rule_at(&g.rules[index], predicate))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        table_rules(&prepared, &prepared_predicate),
        table_rules(&cold, &cold_predicate)
    );
    assert_eq!(prepared.family_relations, cold.family_relations);
}
