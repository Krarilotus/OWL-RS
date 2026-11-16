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
