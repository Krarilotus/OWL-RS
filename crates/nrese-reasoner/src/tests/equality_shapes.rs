//! Which rules the equality module stands in for: the five equality rules of OWL 2 RL
//! (`eq-sym`, `eq-trans`, `eq-rep-s`, `eq-rep-p`, `eq-rep-o`) over `owl:sameAs`, all five, known
//! by what they say. A rule's name is only its name.

use super::super::ir::{Vocabulary, parse_rules};
use super::super::program::RuleProgram;
use super::super::representatives;
use super::super::rulesets;
use super::super::vocabulary::LocalVocabulary;

const SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";

/// Every built-in ruleset engages equality exactly where its name says it closes equality:
/// `rdfs-plus`, `owl-horst` and `owl2-rl` hold the five rules; `rdfs`, `rdfs-full` and
/// `owl2-ql` none of them.
#[test]
fn every_built_in_ruleset_engages_equality_as_its_name_says() {
    for ruleset in rulesets::ALL {
        let mut vocabulary = LocalVocabulary::default();
        let rules = ruleset
            .rules(&mut vocabulary)
            .expect("a built-in ruleset parses");
        assert_eq!(
            representatives::same_as(&rules, vocabulary.iri(SAME_AS)).is_some(),
            RuleProgram::closes_equality(ruleset.name()),
            "{}",
            ruleset.name()
        );
    }
}

/// The equality module engages for the five rules over `owl:sameAs` whatever they are called,
/// their variables named and their premises ordered otherwise; and not for a rule that is
/// named like one of them but says something else, nor for a set that lacks one of them.
#[test]
fn the_equality_rules_are_known_by_what_they_say() {
    let five = |predicate: &str, names: [&str; 5]| {
        [
            format!("{}: (?a {predicate} ?b) -> (?b {predicate} ?a)", names[0]),
            format!(
                "{}: (?b {predicate} ?c), (?a {predicate} ?b) -> (?a {predicate} ?c)",
                names[1]
            ),
            format!(
                "{}: (?x ?y ?z), (?x {predicate} ?w) -> (?w ?y ?z)",
                names[2]
            ),
            format!(
                "{}: (?x ?y ?z), (?y {predicate} ?w) -> (?x ?w ?z)",
                names[3]
            ),
            format!(
                "{}: (?x ?y ?z), (?z {predicate} ?w) -> (?x ?y ?w)",
                names[4]
            ),
        ]
        .join("\n")
    };
    let owl_names = ["eq-sym", "eq-trans", "eq-rep-s", "eq-rep-p", "eq-rep-o"];
    let cases = [
        (
            "the five over owl:sameAs, other names, other variables, premises swapped",
            five(
                "owl:sameAs",
                ["sym", "trans", "subject", "predicate", "object"],
            ),
            true,
        ),
        (
            "a symmetry rule over rdfs:seeAlso named eq-rep-s",
            "eq-rep-s: (?x rdfs:seeAlso ?y) -> (?y rdfs:seeAlso ?x)".to_owned(),
            false,
        ),
        (
            "OWL 2 RL's eq-rep-s alone",
            "eq-rep-s: (?s owl:sameAs ?t), (?s ?p ?o) -> (?t ?p ?o)".to_owned(),
            false,
        ),
        (
            "the five over rdfs:seeAlso, named as OWL 2 RL names them",
            five("rdfs:seeAlso", owl_names),
            false,
        ),
    ];
    let mut wrong = Vec::new();
    for (case, text, engages) in cases {
        let mut vocabulary = LocalVocabulary::default();
        let rules = parse_rules(&text, &mut vocabulary).expect("the rules parse");
        let same_as = vocabulary.iri(SAME_AS);
        let got = representatives::same_as(&rules, same_as);
        let right = got == engages.then_some(same_as);
        println!(
            "CASE {case}: {} (equality over {got:?}, owl:sameAs is {same_as}, engages: {engages})",
            if right { "right" } else { "WRONG" }
        );
        if !right {
            wrong.push(case);
        }
    }
    assert!(wrong.is_empty(), "wrong: {wrong:?}");
}

/// A deletion that splits an `owl:sameAs` class puts back what a rule derives from what
/// remains, whatever the rule is called: one named `eq-sym` that says something else is no
/// equality rule, under either maintenance plan. With the five under other names and
/// `eq-sym: (?x rdfs:seeAlso ?y) -> (?y rdfs:isDefinedBy ?x)`, over `anna owl:sameAs ann2`
/// and `anna rdfs:seeAlso ben`, deleting the equality leaves one inferred statement:
/// `ben rdfs:isDefinedBy anna`.
#[test]
fn a_split_puts_back_what_a_rule_named_like_an_equality_rule_derives() {
    use super::super::batch::{self, Schema};
    use super::super::delta::policy::{Plan, Policy};
    use super::super::delta::{MemoryBase, Rules, update_with};
    let text = "sym: (?a owl:sameAs ?b) -> (?b owl:sameAs ?a)
trans: (?b owl:sameAs ?c), (?a owl:sameAs ?b) -> (?a owl:sameAs ?c)
subject: (?x ?y ?z), (?x owl:sameAs ?w) -> (?w ?y ?z)
predicate: (?x ?y ?z), (?y owl:sameAs ?w) -> (?x ?w ?z)
object: (?x ?y ?z), (?z owl:sameAs ?w) -> (?x ?y ?w)
eq-sym: (?x rdfs:seeAlso ?y) -> (?y rdfs:isDefinedBy ?x)";
    let mut vocabulary = LocalVocabulary::default();
    let rules = parse_rules(text, &mut vocabulary).expect("the rules parse");
    let mut asserted = super::load(
        &mut vocabulary,
        "ex:anna owl:sameAs ex:ann2\nex:anna rdfs:seeAlso ex:ben",
    );
    asserted.sort_unstable();
    let equality = super::load(&mut vocabulary, "ex:anna owl:sameAs ex:ann2")[0];
    let expected = super::load(&mut vocabulary, "ex:ben rdfs:isDefinedBy ex:anna");
    let schema = Schema::owl(&mut vocabulary);
    let compiled = Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
        equality: crate::representatives::same_as(&rules, schema.same_as()),
    };
    let closure = |asserted: &[_]| batch::materialise(asserted, &rules, None, &schema).derived;
    let after: Vec<_> = asserted
        .iter()
        .copied()
        .filter(|&t| t != equality)
        .collect();
    let stack: Vec<_> = (closure(&asserted).into_iter())
        .filter(|t| after.binary_search(t).is_err())
        .collect();
    let mut wrong = Vec::new();
    let materialised = closure(&after);
    println!(
        "CASE materialised: {} inferred, must be 1",
        materialised.len()
    );
    if materialised != expected {
        wrong.push("materialised".to_owned());
    }
    for plan in [Plan::Dred, Plan::Recompute] {
        let base = MemoryBase::new(&after, &stack);
        let deleted = std::slice::from_ref(&equality);
        let policy = Policy::forcing(plan);
        let result = update_with(
            &base,
            &[],
            deleted,
            compiled,
            None,
            crate::eval::NEVER,
            policy,
        )
        .expect("never stopped");
        let removal: std::collections::HashSet<_> = result.remove.iter().copied().collect();
        let kept = stack.iter().copied().filter(|t| !removal.contains(t));
        let mut maintained: Vec<_> = kept.chain(result.insert).collect();
        maintained.sort_unstable();
        maintained.dedup();
        let right = maintained == expected && result.policy.plan == plan;
        println!(
            "CASE {plan:?}: {}: {} inferred, must be 1 (ben isDefinedBy anna); plan {:?}",
            if right { "right" } else { "WRONG" },
            maintained.len(),
            result.policy.plan
        );
        if !right {
            wrong.push(format!("{plan:?}"));
        }
    }
    assert!(wrong.is_empty(), "wrong: {wrong:?}");
}
