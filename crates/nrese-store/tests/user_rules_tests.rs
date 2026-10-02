//! User rules (Notation3) through the store: materialised, maintained on every commit,
//! consistency rules rejecting commits, facts of the rules file, and a change of rules
//! making the recorded closure stale.

mod support;

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, RuleProgram, UserRules};
use nrese_store::{
    MutationCommand, MutationError, MutationPipeline, MutationTicket, SparqlUpdateRequest,
    StoreService,
};
use support::in_memory_store_config;

const EX: &str = "http://example.com/";

const FAMILY: &str = r#"
@prefix : <http://example.com/> .
@prefix log: <http://www.w3.org/2000/10/swap/log#> .

{ ?x :parent ?y . ?y :parent ?z } => { ?x :grandparent ?z } .
{ ?x :sibling ?y } => { ?y :sibling ?x } .
{ ?x :parent ?p . ?y :parent ?p . ?x log:notEqualTo ?y } => { ?x :sibling ?y } .
{ ?x :parent ?x } => false .

:parent :label "parent"@en .
"#;

fn rules(text: &str) -> Arc<UserRules> {
    Arc::new(UserRules::n3("family.n3", text).expect("the rules compile"))
}

fn pipeline(mode: ReasoningMode, text: &str) -> MutationPipeline {
    let store = StoreService::new(in_memory_store_config()).expect("store");
    let config = ReasonerConfig::for_mode(mode)
        .with_rules(Some(rules(text)))
        .expect("a mode for the rules");
    MutationPipeline::new(Arc::new(store), Arc::new(ReasonerService::new(config)))
}

fn update(text: String) -> MutationCommand {
    MutationCommand::Update(SparqlUpdateRequest::new(text))
}

fn insert(triples: &str) -> MutationCommand {
    update(format!("INSERT DATA {{ {triples} }}"))
}

fn delete(triples: &str) -> MutationCommand {
    update(format!("DELETE DATA {{ {triples} }}"))
}

fn contains(pipeline: &MutationPipeline, triple: &str) -> bool {
    let result = pipeline
        .store()
        .execute_query_str(&format!("ASK {{ {triple} }}"))
        .expect("ask");
    String::from_utf8(result.payload)
        .expect("utf8")
        .contains("true")
}

#[test]
fn user_rules_follow_commits() {
    let pipeline = pipeline(ReasoningMode::Custom, FAMILY);
    let ticket = MutationTicket::new;
    pipeline
        .apply(
            insert(&format!(
                "<{EX}anna> <{EX}parent> <{EX}ben> . <{EX}ben> <{EX}parent> <{EX}carl> .
                 <{EX}dora> <{EX}parent> <{EX}ben> ."
            )),
            &nrese_store::Requester::all(),
            &ticket(),
        )
        .expect("facts");
    for inferred in [
        format!("<{EX}anna> <{EX}grandparent> <{EX}carl>"),
        format!("<{EX}anna> <{EX}sibling> <{EX}dora>"),
        format!("<{EX}dora> <{EX}sibling> <{EX}anna>"),
        // A fact of the rules file.
        format!("<{EX}parent> <{EX}label> \"parent\"@en"),
    ] {
        assert!(contains(&pipeline, &inferred), "missing {inferred}");
    }
    // `log:notEqualTo`: nobody is their own sibling.
    assert!(!contains(
        &pipeline,
        &format!("<{EX}anna> <{EX}sibling> <{EX}anna>")
    ));
    // Deleting the support retracts the inference.
    pipeline
        .apply(
            delete(&format!("<{EX}ben> <{EX}parent> <{EX}carl>")),
            &nrese_store::Requester::all(),
            &ticket(),
        )
        .expect("delete");
    assert!(!contains(
        &pipeline,
        &format!("<{EX}anna> <{EX}grandparent> <{EX}carl>")
    ));
    // `=> false` rejects the commit that makes it hold.
    let own_parent = format!("<{EX}eve> <{EX}parent> <{EX}eve>");
    let result = pipeline.apply(
        insert(&own_parent),
        &nrese_store::Requester::all(),
        &ticket(),
    );
    let Err(MutationError::Rejected(reject)) = result else {
        panic!("expected a rejection, got {result:?}");
    };
    assert!(reject.detail.contains("family.n3#4"), "{}", reject.detail);
    assert!(!contains(&pipeline, &own_parent));
}

#[test]
fn user_rules_add_to_a_ruleset() {
    let pipeline = pipeline(ReasoningMode::Owl2Rl, FAMILY);
    pipeline
        .apply(
            insert(&format!(
                "<{EX}Person> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}Agent> .
                 <{EX}anna> a <{EX}Person> .
                 <{EX}anna> <{EX}parent> <{EX}ben> . <{EX}ben> <{EX}parent> <{EX}carl> ."
            )),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("facts");
    assert!(contains(&pipeline, &format!("<{EX}anna> a <{EX}Agent>")));
    assert!(contains(
        &pipeline,
        &format!("<{EX}anna> <{EX}grandparent> <{EX}carl>")
    ));
}

#[test]
fn changed_rules_make_the_recorded_closure_stale() {
    let store = StoreService::new(in_memory_store_config()).expect("store");
    let first = RuleProgram::new(None, Some(rules(FAMILY))).expect("a program");
    store.rematerialise(&first).expect("materialise");
    assert!(store.reasoning_is_current(&first));
    let edited = RuleProgram::new(
        None,
        Some(rules(&FAMILY.replace(":grandparent", ":grandParent"))),
    )
    .expect("a program");
    assert!(!store.reasoning_is_current(&edited));
    // Adding the same rules to a ruleset is another program as well.
    let added = RuleProgram::new(
        Some(nrese_reasoner::v2::rulesets::Ruleset::Rdfs),
        Some(rules(FAMILY)),
    )
    .expect("a program");
    assert!(!store.reasoning_is_current(&added));
}

/// The family rules as a GraphDB ruleset (`.pie`): the same closure, and the consistency
/// check rejecting the same commit.
const FAMILY_PIE: &str = r#"
Prefices
{
  ex : http://example.com/
}

Axioms
{
  <ex:parent> <ex:label> "parent"@en
}

Rules
{
Id: grandparent
  x <ex:parent> y
  y <ex:parent> z
  ---------------
  x <ex:grandparent> z

Id: sibling_symmetric
  x <ex:sibling> y
  ---------------
  y <ex:sibling> x

Id: siblings
  x <ex:parent> p
  y <ex:parent> p     [Constraint x != y]
  ---------------
  x <ex:sibling> y

Consistency: own_parent
  x <ex:parent> x
  ---------------
}
"#;

#[test]
fn graphdb_rulesets_are_read_as_user_rules() {
    let store = StoreService::new(in_memory_store_config()).expect("store");
    let rules = Arc::new(UserRules::pie("family.pie", FAMILY_PIE).expect("the ruleset compiles"));
    let config = ReasonerConfig::for_mode(ReasoningMode::Custom)
        .with_rules(Some(rules))
        .expect("custom rules");
    let pipeline = MutationPipeline::new(Arc::new(store), Arc::new(ReasonerService::new(config)));
    pipeline
        .apply(
            insert(&format!(
                "<{EX}anna> <{EX}parent> <{EX}ben> . <{EX}ben> <{EX}parent> <{EX}carl> .
                 <{EX}dora> <{EX}parent> <{EX}ben> ."
            )),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("facts");
    for inferred in [
        format!("<{EX}anna> <{EX}grandparent> <{EX}carl>"),
        format!("<{EX}anna> <{EX}sibling> <{EX}dora>"),
        format!("<{EX}dora> <{EX}sibling> <{EX}anna>"),
        format!("<{EX}parent> <{EX}label> \"parent\"@en"),
    ] {
        assert!(contains(&pipeline, &inferred), "{inferred}");
    }
    assert!(!contains(
        &pipeline,
        &format!("<{EX}anna> <{EX}sibling> <{EX}anna>")
    ));
    let rejected = pipeline.apply(
        insert(&format!("<{EX}eve> <{EX}parent> <{EX}eve>")),
        &nrese_store::Requester::all(),
        &MutationTicket::new(),
    );
    assert!(
        matches!(rejected, Err(MutationError::Rejected(_))),
        "{rejected:?}"
    );
}
