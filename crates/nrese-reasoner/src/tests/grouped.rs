use crate::ir::{Atom, Head, Rule, Term, Vocabulary};
use crate::vocabulary::LocalVocabulary;
use crate::{RuleProgram, UserRules, batch, delta, explain};
use std::sync::Arc;

fn rules(heads: &str, vocabulary: &mut LocalVocabulary) -> Vec<Rule> {
    let source = format!("Prefices {{}}\nRules {{\nId: local\nx <urn:p> y\n---\n{heads}\n}}\n");
    RuleProgram::new(
        None,
        Some(Arc::new(UserRules::pie("local.pie", source).unwrap())),
    )
    .unwrap()
    .rules(vocabulary)
    .unwrap()
}

#[test]
fn local_heads_keep_original_proofs_and_delete_with_their_premise() {
    let mut v = LocalVocabulary::default();
    let rules = rules("x <urn:q> y [Constraint x != x]\nx <urn:r> y", &mut v);
    assert_eq!(rules.len(), 1);
    assert!(matches!(rules[0].head, Head::Grouped(_)));
    let [s, p, o, q, r] = ["urn:s", "urn:p", "urn:o", "urn:q", "urn:r"].map(|iri| v.iri(iri));
    let premise = [s, p, o];
    let allowed = [s, r, o];
    let blocked = [s, q, o];
    let schema = batch::Schema::owl(&mut v);
    let result = batch::materialise(&[premise], &rules, None, &schema);
    // Independent ordinary-rule oracle: only the unrestricted conclusion is true.
    let oracle = Rule {
        name: "oracle".into(),
        body: rules[0].body.clone(),
        guards: vec![],
        head: Head::Facts(vec![Atom([Term::Var(0), Term::Const(r), Term::Var(1)])]),
    };
    assert_eq!(
        result
            .derived
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>(),
        crate::naive::materialise(&[premise], &[oracle], None).derived
    );
    assert_eq!(result.derived, vec![allowed]);
    let base = delta::MemoryBase::new(&[premise], &result.derived);
    let spec = delta::Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let program = delta::program(&base, spec);
    let proof = explain::explain_with(&base, &program, allowed, 64).unwrap();
    assert_eq!(proof.steps[0].rule.as_deref(), Some("local"));
    assert!(explain::explain_with(&base, &program, blocked, 64).is_none());
    let after = delta::MemoryBase::new(&[], &result.derived);
    assert_eq!(
        delta::update(&after, &[], &[premise], spec, Some(&program)).remove,
        vec![allowed]
    );
}

/// This currently accepted, undocumented option-only syntax is block-wide. Inline
/// head repair must preserve it until the separate vendor newline question is settled.
#[test]
fn constraint_only_lines_keep_block_wide_compatibility() {
    for heads in [
        "[Constraint x != x]\nx <urn:q> y\nx <urn:r> y",
        "x <urn:q> y\n[Constraint x != x]\nx <urn:r> y",
    ] {
        let mut v = LocalVocabulary::default();
        let rules = rules(heads, &mut v);
        let premise = [v.iri("urn:s"), v.iri("urn:p"), v.iri("urn:o")];
        let schema = batch::Schema::owl(&mut v);
        assert!(
            batch::materialise(&[premise], &rules, None, &schema)
                .derived
                .is_empty()
        );
    }
}

#[test]
fn local_schema_only_sources_recover_after_class_growth() {
    for mixed in [false, true] {
        for already_reflexive in [false, true] {
            let mut v = LocalVocabulary::default();
            let mut rs = crate::ir::parse_rules(
                "eq-rep-s: (?s owl:sameAs ?t), (?s ?p ?o) -> (?t ?p ?o)\n\
                 eq-rep-p: (?p owl:sameAs ?q), (?s ?p ?o) -> (?s ?q ?o)\n\
                 eq-rep-o: (?o owl:sameAs ?t), (?s ?p ?o) -> (?s ?p ?t)\n\
                 merge: (?k rdf:fixP ?x), (?k rdf:fixP ?y), ?x != ?y -> (?x owl:sameAs ?y)",
                &mut v,
            )
            .unwrap();
            let extra = if mixed { "<urn:m> <urn:r> <urn:z>" } else { "" };
            let source = format!(
                "Prefices {{}}\nRules {{\nId: local\nx <http://www.w3.org/2000/01/rdf-schema#domain> y\n{extra}\n---\n<urn:m> <urn:q> <urn:z> [Constraint x != y]\n<urn:m> <urn:seen> <urn:z>\n}}\n"
            );
            rs.extend(
                RuleProgram::new(
                    None,
                    Some(Arc::new(UserRules::pie("schema.pie", source).unwrap())),
                )
                .unwrap()
                .rules(&mut v)
                .unwrap(),
            );
            let [a, b, k, m, z, q, r] = [
                "urn:a", "urn:b", "urn:k", "urn:m", "urn:z", "urn:q", "urn:r",
            ]
            .map(|iri| v.iri(iri));
            let domain = v.iri("http://www.w3.org/2000/01/rdf-schema#domain");
            let fixp = v.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#fixP");
            let same = v.iri("http://www.w3.org/2002/07/owl#sameAs");
            let mut input = vec![[a, domain, a], [k, fixp, a], [k, fixp, b], [m, r, z]];
            if already_reflexive {
                input.push([a, same, a]);
            }
            let schema = batch::Schema::owl(&mut v);
            let want = [m, q, z];
            assert!(
                batch::materialise(&input, &rs, None, &schema)
                    .derived
                    .contains(&want)
            );
            let result = crate::representatives::materialise(&input, &rs, None, &schema);
            assert!(
                result.facts.contains(&want),
                "mixed={mixed}, reflexive={already_reflexive}"
            );
            assert_eq!(result.merges, 1);
            assert!(result.rounds <= 4);
        }
    }
}
