//! Public cached-update regressions for RSH-21 R1-F1/F2. The review's initial probes
//! were unrun; these controls assert the decisive source/grounding/policy phases.
use nrese_reasoner as r;
use r::ir::{Head, Rule, Triple, Vocabulary};
use r::vocabulary::LocalVocabulary;
use std::collections::HashSet;
use std::sync::Arc;

fn basis(v: &mut LocalVocabulary) -> Vec<Rule> {
    let rules = r::ir::parse_rules(
        "eq-sym: (?x owl:sameAs ?y) -> (?y owl:sameAs ?x)\n\
         eq-trans: (?x owl:sameAs ?y), (?y owl:sameAs ?z) -> (?x owl:sameAs ?z)\n\
         eq-rep-s: (?s owl:sameAs ?t), (?s ?p ?o) -> (?t ?p ?o)\n\
         eq-rep-p: (?p owl:sameAs ?q), (?s ?p ?o) -> (?s ?q ?o)\n\
         eq-rep-o: (?o owl:sameAs ?t), (?s ?p ?o) -> (?s ?p ?t)",
        v,
    )
    .unwrap();
    assert_eq!(rules.len(), 5);
    assert!(rules.iter().all(|r| matches!(r.head, Head::Facts(_))));
    rules
}

fn custom(text: &str, v: &mut LocalVocabulary) -> Vec<Rule> {
    let rules = r::RuleProgram::new(
        None,
        Some(Arc::new(r::UserRules::pie("review.pie", text).unwrap())),
    )
    .unwrap()
    .rules(v)
    .unwrap();
    assert_eq!(rules.len(), 1);
    assert!(matches!(rules[0].head, Head::Grouped(_)));
    rules
}

fn set(facts: &[Triple]) -> HashSet<Triple> {
    facts.iter().copied().collect()
}
fn apply(before: &[Triple], update: &r::delta::Update) -> HashSet<Triple> {
    let mut after = set(before);
    for f in &update.remove {
        after.remove(f);
    }
    after.extend(update.insert.iter().copied());
    after
}
fn layout() {
    println!(
        "LAYOUT ground_program={} rule={} head={} key={}",
        size_of::<r::eval::GroundProgram>(),
        size_of::<Rule>(),
        size_of::<Head>(),
        size_of::<r::eval::RuleKey>()
    );
}

#[test]
fn rsh21_review_f1_schema_collapse_keeps_reserved_name_output() {
    layout();
    let mut v = LocalVocabulary::default();
    let mut rules = basis(&mut v);
    let extra = custom(
        r#"Prefices {}
Rules {
Id: eq-rep-p
<urn:decl> <http://www.w3.org/2000/01/rdf-schema#domain> <urn:class>
x <urn:input> y
---
x <urn:never> y [Constraint x != x]
x <urn:out> y
}
"#,
        &mut v,
    );
    assert_eq!(extra[0].name, "eq-rep-p");
    rules.extend(extra);
    let [decl, class, s, o, input, output, never] = [
        "urn:decl",
        "urn:class",
        "urn:s",
        "urn:o",
        "urn:input",
        "urn:out",
        "urn:never",
    ]
    .map(|x| v.iri(x));
    let domain = v.iri("http://www.w3.org/2000/01/rdf-schema#domain");
    let schema_fact = [decl, domain, class];
    let added = [s, input, o];
    let want = [s, output, o];
    let schema = r::batch::Schema::owl(&mut v);
    let spec = r::delta::Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let cached = r::delta::program(&r::delta::MemoryBase::new(&[schema_fact], &[]), spec);
    let residual=cached.rules.iter().find(|rule| rule.name=="eq-rep-p" &&
        matches!(&rule.head,Head::Facts(h) if h.iter().any(|h|h.0[1]==r::ir::Term::Const(output)))).expect("named normalized Facts residual");
    assert_eq!(residual.body.len(), 1);
    assert_eq!(residual.body[0].0[1], r::ir::Term::Const(input));
    assert!(
        !residual
            .head
            .atoms()
            .any(|h| h.0[1] == r::ir::Term::Const(never))
    );
    let asserted = [schema_fact, added];
    let cold = r::batch::materialise(&asserted, &rules, None, &schema);
    assert_eq!(set(&cold.derived), set(&[want]));
    let after = r::delta::MemoryBase::new(&asserted, &[]);
    let cache = cached.clone();
    let update = r::delta::update(&after, &[added], &[], spec, Some(&cache));
    assert!(
        update.program.is_none(),
        "data-only insertion must use cached program"
    );
    println!(
        "F1 source=Grouped residual=Facts name={} cold={} cached_insert={} cache_reused={}",
        residual.name,
        cold.derived.len(),
        update.insert.len(),
        update.program.is_none()
    );
    assert_eq!(
        apply(&[], &update),
        set(&cold.derived),
        "R1-F1: normalized grouped source was replaced by name"
    );
    let now = r::delta::MemoryBase::new(&asserted, &update.insert);
    let proof =
        r::explain::explain_with(&now, &cache, want, 64).expect("survivor keeps original proof");
    assert_eq!(proof.steps[0].rule.as_deref(), Some("eq-rep-p"));
    assert!(matches!(
        r::explain::explain_with(&now, &cache, [s, never, o], 64),
        Err(r::explain::Unexplained::NoDerivation)
    ));
}

fn region_control(survives: bool) {
    layout();
    let mut v = LocalVocabulary::default();
    let mut rules = basis(&mut v);
    let subject = if survives { "urn:a" } else { "urn:m" };
    let source = format!(
        "Prefices {{}}\nRules {{\nId: eq-sym\nx <http://www.w3.org/2002/07/owl#sameAs> y\n---\nx <urn:never> y [Constraint x != x]\n<{subject}> <urn:out> <urn:z>\n}}\n"
    );
    let extra = custom(&source, &mut v);
    assert_eq!(extra[0].name, "eq-sym");
    rules.extend(extra);
    let [a, b, m, out, z, never] =
        ["urn:a", "urn:b", subject, "urn:out", "urn:z", "urn:never"].map(|x| v.iri(x));
    let same = v.iri("http://www.w3.org/2002/07/owl#sameAs");
    let deleted = [a, same, b];
    let want = [m, out, z];
    let reflexive = [a, same, a];
    assert_eq!(
        want.iter().any(|t| [a, b].contains(t)),
        survives,
        "fixed conclusion locality is deliberate"
    );
    let schema = r::batch::Schema::owl(&mut v);
    let spec = r::delta::Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let mut asserted_before = vec![deleted];
    let mut asserted_after = Vec::new();
    if survives {
        asserted_before.push(reflexive);
        asserted_after.push(reflexive);
    }
    let old = r::batch::materialise(&asserted_before, &rules, None, &schema);
    assert!(old.derived.contains(&want));
    assert!(!old.derived.iter().any(|f| f[1] == never));
    let cached = r::delta::program(
        &r::delta::MemoryBase::new(&asserted_before, &old.derived),
        spec,
    );
    let cold = r::batch::materialise(&asserted_after, &rules, None, &schema);
    assert_eq!(cold.derived.contains(&want), survives);
    let after = r::delta::MemoryBase::new(&asserted_after, &old.derived);
    for plan in [
        r::delta::policy::Plan::Dred,
        r::delta::policy::Plan::Recompute,
    ] {
        let update = r::delta::update_with(
            &after,
            &[],
            &[deleted],
            spec,
            Some(&cached),
            r::eval::NEVER,
            r::delta::policy::Policy::forcing(plan),
        )
        .unwrap();
        assert_eq!(update.policy.plan, plan);
        assert_eq!(
            update.policy.region_terms,
            if plan == r::delta::policy::Plan::Recompute {
                2
            } else {
                0
            }
        );
        assert!(
            update.program.is_none(),
            "no schema rebuild hides the cached classification"
        );
        println!(
            "F2 survives={survives} plan={plan:?} region={} overdeleted={} removed_marker={} expected_marker={survives}",
            update.policy.region_terms,
            update.policy.overdeleted,
            update.remove.contains(&want)
        );
        assert_eq!(
            apply(&old.derived, &update),
            set(&cold.derived),
            "R1-F2: {plan:?} misclassified grouped producer; survives={survives}"
        );
        assert_eq!(!update.remove.contains(&want), survives);
    }
}

#[test]
fn rsh21_review_f2_region_propagates_nonregion_group_output() {
    region_control(false);
}

/// Also exercise the region's generic proof mask: a fixed region output with an
/// independent surviving reflexive premise must be put back, not just overdeleted.
#[test]
fn rsh21_review_f2_region_rederives_surviving_group_output() {
    region_control(true);
}
