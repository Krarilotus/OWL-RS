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

/// A store under `config` reasoning with `rules` alone (custom mode, no built-in part),
/// holding `data` (with the prefixes ex, rdfs and owl).
fn store_with(config: StoreConfig, rules: &str, data: &str) -> MutationPipeline {
    let store = StoreService::new(config).expect("store");
    let pie = format!("{PREFICES}\nRules\n{{\n{rules}}}\n");
    let rules = Arc::new(UserRules::pie("user.pie", pie).expect("the ruleset compiles"));
    let reasoner = ReasonerConfig::for_mode(ReasoningMode::Custom)
        .with_rules(Some(rules))
        .expect("custom rules");
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
/// is an `owl:Thing` exactly where the rules make it one. Two programs with no built-in part,
/// each with the range rule, over `ex:p rdfs:range _:u`, `_:u` a declared union class, and
/// `ex:s ex:p ex:x`:
///
/// - (a) beside a symmetry rule over `ex:knows` named `scm-cls`: nothing makes `ex:x` an
///   `owl:Thing`;
/// - (b) beside OWL 2 RL's scm-cls named `classes`, and cax-sco: `_:u rdfs:subClassOf
///   owl:Thing` and `ex:x a _:u`, so `ex:x a owl:Thing`.
#[test]
fn hidden_memberships_become_things_as_the_rules_say_whatever_they_are_called() {
    let range = "Id: prp-rng\n  p <rdfs:range> c\n  x p y\n  ---\n  y <rdf:type> c\n\n";
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
    let data = "ex:p rdfs:range [ a owl:Class ; owl:unionOf ( ex:A ex:B ) ] .
        ex:s ex:p ex:x . ex:s ex:knows ex:t";
    let cases = [
        (
            "(a) a symmetry rule named scm-cls",
            format!("{range}{symmetry}"),
            false,
            "ex:t ex:knows ex:s",
        ),
        (
            "(b) OWL 2 RL's scm-cls named classes, and cax-sco",
            format!("{range}{classes}"),
            true,
            "?u rdfs:subClassOf owl:Thing FILTER(isBlank(?u))",
        ),
    ];
    let mut wrong = Vec::new();
    for (case, rules, thing, own) in cases {
        let config = StoreConfig {
            hide_unnamed_classes: true,
            ..in_memory_store_config()
        };
        let pipeline = store_with(config, &rules, data);
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
            let verdict = if got == expected { "right" } else { "WRONG" };
            println!("CASE {case}: {verdict}: {what}: {got}, must be {expected}");
            if got != expected {
                wrong.push(format!("{case}: {what}: {got}"));
            }
        }
    }
    assert!(wrong.is_empty(), "wrong: {wrong:#?}");
}
