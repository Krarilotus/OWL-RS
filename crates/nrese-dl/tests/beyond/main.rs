//! Inputs outside OWL 2 DL (`nrese_dl::beyond`, docs/design/owl2-dl.md §16): DL-safe rules
//! decided by grounding their instances, each answer by a certified model or by
//! monotonicity; cases small enough to check by hand.

use nrese_dl::beyond::{Options, Verdict, consistency};
use nrese_dl::scope::{answer_beyond, read_with_rules};

const PREFIXES: &str = "Prefix(:=<http://example.org/scope#>) \
    Prefix(owl:=<http://www.w3.org/2002/07/owl#>) \
    Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) \
    Prefix(swrlb:=<http://www.w3.org/2003/11/swrlb#>) ";

fn decide(axioms: &str) -> nrese_dl::beyond::Decision {
    let (o, _) = read_with_rules(&format!("{PREFIXES}Ontology({axioms})")).expect("reads");
    consistency(&o, &Options::default())
}

fn is_consistent(d: &nrese_dl::beyond::Decision) -> bool {
    match &d.verdict {
        Verdict::Consistent { .. } => true,
        Verdict::Inconsistent { .. } => false,
        Verdict::Undecided(why) => panic!("undecided: {why}\n{:#?}", d.tried),
    }
}

/// A rule whose body the assertions state: its head is added outright (the chase), and
/// contradicts the DL part.
#[test]
fn the_chase_adds_what_the_assertions_entail() {
    let d = decide(
        "ClassAssertion(:A :a) SubClassOf(:B owl:Nothing) \
         DLSafeRule(Body(ClassAtom(:A Variable(:x))) Head(ClassAtom(:B Variable(:x))))",
    );
    assert!(!is_consistent(&d));
    assert_eq!(d.chased, 1);
}

/// A body that holds only by a choice of the model: each model breaks one instance, its
/// axiom goes in, and the next round refutes.
#[test]
fn instances_the_models_break_are_grounded_until_none_is() {
    let d = decide(
        "Declaration(Class(:C)) Declaration(Class(:D)) \
         ClassAssertion(:A :a) SubClassOf(:A ObjectUnionOf(:C :D)) SubClassOf(:B owl:Nothing) \
         DLSafeRule(Body(ClassAtom(:C Variable(:x))) Head(ClassAtom(:B Variable(:x)))) \
         DLSafeRule(Body(ClassAtom(:D Variable(:x))) Head(ClassAtom(:B Variable(:x))))",
    );
    assert!(!is_consistent(&d));
    assert!(d.grounded >= 2, "{d:#?}");
}

/// DL-safety: a rule binds named individuals only. Elements the axioms make anonymous may
/// be in `A` though `A(x) → ⊥` is a rule (under first-order rules the input would be
/// inconsistent).
#[test]
fn rules_bind_named_individuals_only() {
    let d = decide(
        "Declaration(ObjectProperty(:r)) ClassAssertion(owl:Thing :a) \
         SubClassOf(owl:Thing ObjectSomeValuesFrom(:r :A)) \
         DLSafeRule(Body(ClassAtom(:A Variable(:x))) Head())",
    );
    assert!(is_consistent(&d));
    // But the named individual can't be one.
    let d = decide(
        "Declaration(ObjectProperty(:r)) ClassAssertion(owl:Thing :a) \
         SubClassOf(owl:Thing :A) \
         DLSafeRule(Body(ClassAtom(:A Variable(:x))) Head())",
    );
    assert!(!is_consistent(&d));
}

/// `x ≠ x` is false and binds every named individual, a declared one too (the random
/// campaign's case 1:106 wrote it as a vacuous `DifferentIndividuals(a)`).
#[test]
fn an_inequality_of_one_individual_is_false() {
    let d = decide(
        "Declaration(NamedIndividual(:a)) \
         DLSafeRule(Body(SameIndividualAtom(Variable(:x) Variable(:x))) \
         Head(DifferentIndividualsAtom(Variable(:x) Variable(:x))))",
    );
    assert!(!is_consistent(&d));
    // Without a named individual the rule binds nothing.
    let d = decide(
        "Declaration(Class(:C)) \
         DLSafeRule(Body(SameIndividualAtom(Variable(:x) Variable(:x))) \
         Head(DifferentIndividualsAtom(Variable(:x) Variable(:x))))",
    );
    assert!(is_consistent(&d));
}

/// The instance budget limits how many rule instances a round grounds, not which rules
/// are checked: `⊤(x) → x ≠ x` over the named `a` is false in every model, so a round that
/// may ground none can't certify one (A-1701).
#[test]
fn a_zero_instance_budget_certifies_no_model() {
    let (o, _) = read_with_rules(&format!(
        "{PREFIXES}Ontology(Declaration(NamedIndividual(:a)) ClassAssertion(owl:Thing :a) \
         DLSafeRule(Body(ClassAtom(owl:Thing Variable(:x))) \
         Head(DifferentIndividualsAtom(Variable(:x) Variable(:x)))))"
    ))
    .expect("reads");
    let options = Options {
        max_instances: Some(0),
        ..Options::default()
    };
    let d = consistency(&o, &options);
    assert!(
        !matches!(d.verdict, Verdict::Consistent { .. }),
        "certified consistent: {:?}\n{:#?}",
        d.verdict,
        d.tried
    );
}

/// An exact classification settles each class's place, not only its pairs (A-1702):
/// - `A ⊑ {a}` and `A(x) → ⊥`: an element of `A` is `a`, which the rule keeps out of `A`, so
///   `A` is unsatisfiable though it has no pair of classes to test; with no test budget it
///   is listed so;
/// - `⊤ ⊑ {a}`, `⊤ ⊑ A ⊔ B` and `B(x) → ⊥`: every element is `a`, which the rule keeps out of
///   `B`, so `A` is `⊤`;
/// - the same with `a : ∀U.{a}` (`U` the universal property) in place of `⊤ ⊑ {a}`, asserted
///   or concluded by a rule (PR #32 review, R1): it reaches every element, so no model of the
///   TBox alone joins the input's.
///
/// A classification that doesn't list it isn't exact.
#[test]
fn an_exact_classification_settles_each_class() {
    let only_a = "ObjectAllValuesFrom(owl:topObjectProperty ObjectOneOf(:a))";
    let asserted = format!("ClassAssertion({only_a} :a)");
    let concluded = format!(
        "ClassAssertion(owl:Thing :a) \
         DLSafeRule(Body(ClassAtom(owl:Thing :a)) Head(ClassAtom({only_a} :a)))"
    );
    let empty_a = "Declaration(Class(:A)) Declaration(NamedIndividual(:a)) \
                   DLSafeRule(Body(ClassAtom(:A Variable(:x))) Head())";
    let top_a = "Declaration(Class(:A)) Declaration(Class(:B)) Declaration(NamedIndividual(:a)) \
                 SubClassOf(owl:Thing ObjectUnionOf(:A :B)) \
                 DLSafeRule(Body(ClassAtom(:B Variable(:x))) Head())";
    // (case, axioms, whether A is unsatisfiable rather than ⊤)
    let cases = [
        (
            "A ⊑ {a}",
            format!("{empty_a} SubClassOf(:A ObjectOneOf(:a))"),
            true,
        ),
        (
            "⊤ ⊑ {a}, ⊤ ⊑ A ⊔ B",
            format!("{top_a} SubClassOf(owl:Thing ObjectOneOf(:a))"),
            false,
        ),
        ("a : ∀U.{a}", format!("{empty_a} {asserted}"), true),
        (
            "a : ∀U.{a} by a rule",
            format!("{empty_a} {concluded}"),
            true,
        ),
        (
            "a : ∀U.{a}, ⊤ ⊑ A ⊔ B",
            format!("{top_a} {asserted}"),
            false,
        ),
    ];
    let mut wrong = Vec::new();
    for (case, axioms, unsatisfiable) in cases {
        let (o, names) = read_with_rules(&format!("{PREFIXES}Ontology({axioms})")).expect("reads");
        let a = names.classes[0];
        for max_tests in [None, Some(0)] {
            let options = Options {
                max_tests,
                ..Options::default()
            };
            let c = nrese_dl::beyond::classify::classify(
                &o,
                &options,
                &nrese_dl::classify::Options::default(),
            );
            let listed = if unsatisfiable {
                c.classification.unsatisfiable.contains(&a)
            } else {
                c.classification.top.contains(&a)
            };
            if !listed && c.exact {
                wrong.push(format!(
                    "{case}, max_tests {max_tests:?}: A not listed, and exact"
                ));
            }
            if unsatisfiable && max_tests.is_none() && !listed {
                wrong.push(format!("{case}: A not unsatisfiable"));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// SWRL's comparisons (SWRL §8.1): an inclusive one holds for equal values of any type, so
/// `lessThanOrEqual(true, true) → a ≠ a` has no model (review A-1607); a strict one on equal
/// values doesn't hold, so its rule never fires; `lessThan(false, true)` holds, so its rule
/// has no model either, and where the order isn't evaluated nothing is certified.
#[test]
fn comparisons_hold_as_swrl_defines_them() {
    for (body, consistent) in [
        (
            "lessThanOrEqual \"true\"^^xsd:boolean \"true\"^^xsd:boolean",
            false,
        ),
        (
            "greaterThanOrEqual \"true\"^^xsd:boolean \"true\"^^xsd:boolean",
            false,
        ),
        ("lessThan \"true\"^^xsd:boolean \"true\"^^xsd:boolean", true),
        (
            "lessThan \"false\"^^xsd:boolean \"true\"^^xsd:boolean",
            false,
        ),
    ] {
        let (o, _) = read_with_rules(&format!(
            "{PREFIXES}Ontology(Declaration(NamedIndividual(:a)) ClassAssertion(owl:Thing :a) \
             DLSafeRule(Body(BuiltInAtom(swrlb:{body})) \
             Head(DifferentIndividualsAtom(:a :a))))"
        ))
        .expect("reads");
        let verdict = consistency(&o, &Options::default()).verdict;
        assert_eq!(
            matches!(verdict, Verdict::Consistent { .. }),
            consistent,
            "{body}: {verdict:?}"
        );
    }
}

/// A built-in outside SWRL's (SWRLTab's `abox:hasClass` in ORE's 10860) has no meaning
/// NRESE knows: its rule is left out and named; "inconsistent" from the rest still holds,
/// "consistent" isn't claimed.
#[test]
fn a_foreign_built_in_is_left_out_and_named() {
    let rule = "DLSafeRule(Body(ClassAtom(:A Variable(:x)) \
        BuiltInAtom(<http://example.org/hasClass> Variable(:x) Variable(:c))) \
        Head(ClassAtom(:B Variable(:x))))";
    let d = decide(&format!("ClassAssertion(:A :a) {rule}"));
    match &d.verdict {
        Verdict::Undecided(why) => assert!(why.contains("example.org/hasClass"), "{why}"),
        other => panic!("{other:?}"),
    }
    let d = decide(&format!(
        "ClassAssertion(:A :a) SubClassOf(:A owl:Nothing) {rule}"
    ));
    assert!(!is_consistent(&d));
}

/// A rule with a SQWRL built-in is a query: it asserts nothing (ORE's 3726, 7278, 15753),
/// and the answer says it read it so.
#[test]
fn sqwrl_queries_assert_nothing() {
    let d = decide(
        "ClassAssertion(:A :a) SubClassOf(:B owl:Nothing) \
         DLSafeRule(Body(ClassAtom(:A Variable(:x))) Head(ClassAtom(:B Variable(:x)) \
         BuiltInAtom(<http://sqwrl.stanford.edu/ontologies/built-ins/3.4/sqwrl.owl#select> Variable(:x))))",
    );
    assert!(is_consistent(&d));
    assert!(
        d.repairs.iter().any(|r| r.contains("SQWRL")),
        "{:?}",
        d.repairs
    );
}

/// A built-in computes a value along a chain of individuals: `index(y) = index(x) + 1`.
#[test]
fn built_ins_compute_values() {
    let rule = "DLSafeRule(Body(ObjectPropertyAtom(:next Variable(:x) Variable(:y)) \
        DataPropertyAtom(:index Variable(:x) Variable(:w)) \
        BuiltInAtom(swrlb:add Variable(:v) Variable(:w) \"1\"^^xsd:integer)) \
        Head(DataPropertyAtom(:index Variable(:y) Variable(:v))))";
    let base = format!(
        "Declaration(ObjectProperty(:next)) Declaration(DataProperty(:index)) \
         FunctionalDataProperty(:index) ObjectPropertyAssertion(:next :a :b) \
         ObjectPropertyAssertion(:next :b :c) \
         DataPropertyAssertion(:index :a \"1\"^^xsd:integer) {rule}"
    );
    let d = decide(&base);
    assert!(is_consistent(&d));
    // The model has index 3 at c, and the input entails it: 4 contradicts.
    let model = d.model.as_ref().expect("a model");
    let three = nrese_xsd::owl::Value::Real(nrese_xsd::owl::Rational::integer(3).expect("small"));
    assert!(
        model.data.values().flatten().any(|(_, v)| *v == three),
        "{model:?}"
    );
    let d = decide(&format!(
        "{base} DataPropertyAssertion(:index :c \"4\"^^xsd:integer)"
    ));
    assert!(!is_consistent(&d));
}

/// A built-in NRESE doesn't evaluate is no answer, and says which.
#[test]
fn an_unknown_built_in_is_named() {
    let d = decide(
        "Declaration(DataProperty(:d)) DataPropertyAssertion(:d :a \"1\"^^xsd:integer) \
         DLSafeRule(Body(DataPropertyAtom(:d Variable(:x) Variable(:w)) \
         BuiltInAtom(swrlb:tan Variable(:v) Variable(:w))) Head(ClassAtom(:A Variable(:x))))",
    );
    match d.verdict {
        Verdict::Undecided(why) => assert!(why.contains("tan"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// Entailments: `r(x, y) ∧ A(y) → A(x)` with `r(a, b)`, `b : A` entails `a : A` (by
/// monotonicity: `a : ¬A` is refuted); `b : B` isn't entailed (a model shows it).
#[test]
fn entailments_of_the_whole_input() {
    let (o, names) = read_with_rules(&format!(
        "{PREFIXES}Ontology(Declaration(Class(:B)) Declaration(ObjectProperty(:r)) \
         ObjectPropertyAssertion(:r :a :b) ClassAssertion(:A :b) \
         DLSafeRule(Body(ObjectPropertyAtom(:r Variable(:x) Variable(:y)) \
         ClassAtom(:A Variable(:y))) Head(ClassAtom(:A Variable(:x)))))"
    ))
    .expect("reads");
    let a = answer_beyond(&o, &names, &Options::default());
    // A sat, B sat, A ⊑ B, B ⊑ A, ⊤ ⊑ A, ⊤ ⊑ B, a:A, a:B, b:A, b:B.
    assert_eq!(a.code, "c1100001010");
    assert_eq!(a.certified, "mmmmmmmgmgm");
}
