//! A rule's name is only its name (D16-773): what the reasoner makes of a user's rules follows
//! from what they say, whatever they are called. The expected answers are written by hand from
//! the PIE dialect's meaning (a rule's premises entail its conclusions; its `Id` is only its
//! name).

use std::sync::Arc;

use crate::support::{in_memory_store_config, inferred_statements};
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, UserRules};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, SparqlUpdateRequest, StoreConfig,
    StoreService,
};

const EX: &str = "http://example.com/";
const TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const PREFIXES: &str = "PREFIX ex: <http://example.com/> \
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
    PREFIX owl: <http://www.w3.org/2002/07/owl#> ";
const PREFICES: &str = "Prefices
{
  ex : http://example.com/
  rdf : http://www.w3.org/1999/02/22-rdf-syntax-ns#
  rdfs : http://www.w3.org/2000/01/rdf-schema#
  owl : http://www.w3.org/2002/07/owl#
}
";

/// Reasoning with the PIE `rules` alone (custom mode, no built-in part).
fn custom(rules: &str) -> ReasonerConfig {
    custom_with("", rules)
}

/// [`custom`] with the PIE `axioms` (one statement a line) as well.
fn custom_with(axioms: &str, rules: &str) -> ReasonerConfig {
    let pie = format!("{PREFICES}\nAxioms\n{{\n{axioms}}}\nRules\n{{\n{rules}}}\n");
    let rules = Arc::new(UserRules::pie("user.pie", pie).expect("the ruleset compiles"));
    ReasonerConfig::for_mode(ReasoningMode::Custom)
        .with_rules(Some(rules))
        .expect("custom rules")
}

/// A store under `config` reasoning by `reasoner`, holding `data` (with the prefixes ex, rdfs
/// and owl).
fn store_with(config: StoreConfig, reasoner: ReasonerConfig, data: &str) -> MutationPipeline {
    let store = StoreService::new(config).expect("store");
    let pipeline = MutationPipeline::new(Arc::new(store), Arc::new(ReasonerService::new(reasoner)));
    let update = format!("{PREFIXES} INSERT DATA {{ {data} }}");
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update)),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("the data commits");
    pipeline
}

/// OWL 2 RL's range rule, in PIE.
const RANGE: &str = "Id: prp-rng\n  p <rdfs:range> c\n  x p y\n  ---\n  y <rdf:type> c\n\n";
/// `ex:x` a member of a declared union class by `ex:p`'s range.
const DATA: &str = "ex:p rdfs:range [ a owl:Class ; owl:unionOf ( ex:A ex:B ) ] . ex:s ex:p ex:x";

/// One check's line, `CASE {case}: right|WRONG: {what}: {got}, must be {expected}`; a wrong
/// one also goes to `wrong`.
fn check(wrong: &mut Vec<String>, case: &str, what: &str, got: bool, expected: bool) {
    let verdict = if got == expected { "right" } else { "WRONG" };
    println!("CASE {case}: {verdict}: {what}: {got}, must be {expected}");
    if got != expected {
        wrong.push(format!("{case}: {what}: {got}"));
    }
}

fn holds(pipeline: &MutationPipeline, pattern: &str) -> bool {
    let result = pipeline
        .store()
        .execute_query_str(&format!("{PREFIXES} ASK {{ {pattern} }}"))
        .expect("ask");
    String::from_utf8(result.payload)
        .expect("utf8")
        .contains("true")
}

/// Under `reasoner.unnamed_classes = "skip"`, an unnamed union class nothing consumes keeps its
/// memberships out of the store, and nothing else changes (`nrese_reasoner::unnamed`): a member
/// is an `owl:Thing` exactly where the rules make it one. Three programs, the first two with no
/// built-in part and the range rule, over `ex:p rdfs:range _:u`, `_:u` a declared union class,
/// and `ex:s ex:p ex:x`:
///
/// - (a) beside a symmetry rule over `ex:knows` named `scm-cls`: nothing makes `ex:x` an
///   `owl:Thing`;
/// - (b) beside OWL 2 RL's scm-cls named `classes`, and cax-sco: `_:u rdfs:subClassOf
///   owl:Thing` and `ex:x a _:u`, so `ex:x a owl:Thing`;
/// - (c) the built-in `owl2-rl` alone: its scm-cls and cax-sco, so `ex:x a owl:Thing`.
#[test]
fn hidden_memberships_become_things_as_the_rules_say_whatever_they_are_called() {
    let symmetry = "Id: scm-cls\n  x <ex:knows> y\n  ---\n  y <ex:knows> x\n\n";
    let classes = "Id: classes
  c <rdf:type> <owl:Class>
  ---
  c <rdfs:subClassOf> c
  c <owl:equivalentClass> c
  c <rdfs:subClassOf> <owl:Thing>
  <owl:Nothing> <rdfs:subClassOf> c

Id: cax-sco
  c1 <rdfs:subClassOf> c2
  x <rdf:type> c1
  ---
  x <rdf:type> c2

";
    let data = format!("{DATA} . ex:s ex:knows ex:t");
    let cases = [
        (
            "(a) a symmetry rule named scm-cls",
            custom(&format!("{RANGE}{symmetry}")),
            false,
            "ex:t ex:knows ex:s",
        ),
        (
            "(b) OWL 2 RL's scm-cls named classes, and cax-sco",
            custom(&format!("{RANGE}{classes}")),
            true,
            "?u rdfs:subClassOf owl:Thing FILTER(isBlank(?u))",
        ),
        (
            "(c) the built-in owl2-rl",
            ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
            true,
            "?u rdfs:subClassOf owl:Thing FILTER(isBlank(?u))",
        ),
    ];
    let mut wrong = Vec::new();
    for (case, reasoner, thing, own) in cases {
        let config = StoreConfig {
            hide_unnamed_classes: true,
            ..in_memory_store_config()
        };
        let pipeline = store_with(config, reasoner, &data);
        let membership = (format!("{EX}x"), TYPE.to_owned(), THING.to_owned());
        let inferred = inferred_statements(pipeline.store()).expect("the inferred statements");
        let checks = [
            (
                "ASK ex:x a owl:Thing",
                holds(&pipeline, "ex:x a owl:Thing"),
                thing,
            ),
            (
                "stored: ex:x a owl:Thing",
                inferred.contains(&membership),
                thing,
            ),
            (own, holds(&pipeline, own), true),
            (
                "ASK ex:x a _:u (left out)",
                holds(&pipeline, "ex:x a ?u FILTER(isBlank(?u))"),
                false,
            ),
        ];
        for (what, got, expected) in checks {
            check(&mut wrong, case, what, got, expected);
        }
    }
    assert!(wrong.is_empty(), "wrong: {wrong:#?}");
}

/// Fail-safe: with unnamed classes skipped, a thing the rules make only from the union's own
/// definition is left out with the union's memberships; nothing is called a thing the rules
/// don't make one. The rules beside the range rule make a union's members things from
/// `c owl:unionOf l`, a fact the question the store asks of a program (does a member of a
/// declared class become an `owl:Thing`) doesn't hold: under `"skip"`, `ex:x` is neither a
/// member of `_:u` nor an `owl:Thing`; under `"keep"`, it is both.
#[test]
fn a_thing_made_only_from_the_unions_definition_is_left_out_with_its_memberships() {
    let things = "Id: union-things
  x <rdf:type> c
  c <owl:unionOf> l
  ---
  x <rdf:type> <owl:Thing>

";
    let mut wrong = Vec::new();
    for (mode, hide) in [("skip", true), ("keep", false)] {
        let config = StoreConfig {
            hide_unnamed_classes: hide,
            ..in_memory_store_config()
        };
        let pipeline = store_with(config, custom(&format!("{RANGE}{things}")), DATA);
        let thing = holds(&pipeline, "ex:x a owl:Thing");
        check(&mut wrong, mode, "ASK ex:x a owl:Thing", thing, !hide);
        let member = holds(&pipeline, "ex:x a ?u FILTER(isBlank(?u))");
        check(&mut wrong, mode, "ASK ex:x a _:u", member, !hide);
    }
    assert!(wrong.is_empty(), "wrong: {wrong:#?}");
}

/// A rule that makes a declared class's members things except one named individual: every
/// member but `ex:nobody`. `ex:x` and `ex:nobody` are both members of `_:u` by the range rule;
/// the store calls `ex:nobody` an `owl:Thing` neither under `"skip"` nor under `"keep"`, and
/// under `"keep"` (nothing left out) `ex:x` is one.
#[test]
fn a_member_the_rules_keep_from_being_a_thing_is_never_called_one() {
    let things = "Id: things-but-nobody
  x <rdf:type> c  [Constraint x != <ex:nobody>]
  c <rdf:type> <owl:Class>
  ---
  x <rdf:type> <owl:Thing>

";
    let data = format!("{DATA} . ex:s ex:p ex:nobody");
    let mut wrong = Vec::new();
    for (mode, hide) in [("skip", true), ("keep", false)] {
        let config = StoreConfig {
            hide_unnamed_classes: hide,
            ..in_memory_store_config()
        };
        let pipeline = store_with(config, custom(&format!("{RANGE}{things}")), &data);
        let nobody = holds(&pipeline, "ex:nobody a owl:Thing");
        check(&mut wrong, mode, "ASK ex:nobody a owl:Thing", nobody, false);
        if !hide {
            let x = holds(&pipeline, "ex:x a owl:Thing");
            check(&mut wrong, mode, "ASK ex:x a owl:Thing", x, true);
        }
    }
    assert!(wrong.is_empty(), "wrong: {wrong:#?}");
}

/// The same, the named individual reaching the rule's inequality through a variable: the
/// rule makes a declared class's members things except the one the axiom
/// `ex:nobody ex:is ex:special` marks (`x != n`, `n` bound by `n ex:is ex:special`; no
/// constant in the inequality). `ex:x` and `ex:nobody` are both members of `_:u` by the range
/// rule; the store calls `ex:nobody` an `owl:Thing` neither under `"skip"` nor under `"keep"`,
/// and under `"keep"` `ex:x` is one.
#[test]
fn a_member_marked_by_an_axiom_against_being_a_thing_is_never_called_one() {
    let axiom = "<ex:nobody> <ex:is> <ex:special>\n";
    let things = "Id: things-but-special
  x <rdf:type> c
  c <rdf:type> <owl:Class>
  n <ex:is> <ex:special>  [Constraint x != n]
  ---
  x <rdf:type> <owl:Thing>

";
    let data = format!("{DATA} . ex:s ex:p ex:nobody");
    let mut wrong = Vec::new();
    for (mode, hide) in [("skip", true), ("keep", false)] {
        let config = StoreConfig {
            hide_unnamed_classes: hide,
            ..in_memory_store_config()
        };
        let rules = custom_with(axiom, &format!("{RANGE}{things}"));
        let pipeline = store_with(config, rules, &data);
        let nobody = holds(&pipeline, "ex:nobody a owl:Thing");
        check(&mut wrong, mode, "ASK ex:nobody a owl:Thing", nobody, false);
        if !hide {
            let x = holds(&pipeline, "ex:x a owl:Thing");
            check(&mut wrong, mode, "ASK ex:x a owl:Thing", x, true);
        }
    }
    assert!(wrong.is_empty(), "wrong: {wrong:#?}");
}
