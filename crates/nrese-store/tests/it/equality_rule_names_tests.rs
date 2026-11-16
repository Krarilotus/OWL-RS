//! A user's rules named like OWL 2 RL's equality rules (`eq-rep-s`, `eq-rep-p`, `eq-rep-o`)
//! are the user's rules: a store with a custom program answers what that program entails,
//! whatever its rules are called. The expected answers are written from the PIE dialect's
//! meaning (a rule's premises entail its conclusions; a rule's `Id` is only its name).
//! Default settings throughout (the equality mode included).

use std::sync::Arc;

use crate::support::in_memory_store_config;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode, UserRules};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, SparqlUpdateRequest, StoreService,
};

const EX: &str = "http://example.com/";

/// A store reasoning with `pie` alone (custom mode), holding `data`.
fn store_with(pie: &str, data: &str) -> MutationPipeline {
    let store = StoreService::new(in_memory_store_config()).expect("store");
    let rules = Arc::new(UserRules::pie("user.pie", pie).expect("the ruleset compiles"));
    let config = ReasonerConfig::for_mode(ReasoningMode::Custom)
        .with_rules(Some(rules))
        .expect("custom rules");
    let pipeline = MutationPipeline::new(Arc::new(store), Arc::new(ReasonerService::new(config)));
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!(
                "INSERT DATA {{ {data} }}"
            ))),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .expect("the data commits");
    pipeline
}

fn holds(pipeline: &MutationPipeline, triple: &str) -> bool {
    let result = pipeline
        .store()
        .execute_query_str(&format!("ASK {{ {triple} }}"))
        .expect("ask");
    String::from_utf8(result.payload)
        .expect("utf8")
        .contains("true")
}

/// Each `(statement, entailed)` asked; one line per case, `right` or `WRONG`; the wrong ones.
fn ask_all(case: &str, pipeline: &MutationPipeline, expected: &[(String, bool)]) -> Vec<String> {
    let mut wrong = Vec::new();
    for (triple, entailed) in expected {
        let answer = holds(pipeline, triple);
        let verdict = if answer == *entailed {
            "right"
        } else {
            "WRONG"
        };
        println!("{case}: {verdict}: ASK {{ {triple} }} answered {answer}, entailed {entailed}");
        if answer != *entailed {
            wrong.push(format!(
                "{case}: ASK {{ {triple} }} answered {answer}, entailed {entailed}"
            ));
        }
    }
    wrong
}

/// Case A: a symmetry rule over `ex:knows` named `eq-rep-s`. It entails `b knows a` and
/// nothing else: `ex:knows` is not `owl:sameAs`, so `a` and `b` are not the same individual
/// and `b` is not a dog.
const SYMMETRY_NAMED_EQ_REP_S: &str = r#"
Prefices
{
  ex : http://example.com/
}

Rules
{
Id: eq-rep-s
  x <ex:knows> y
  ---------------
  y <ex:knows> x
}
"#;

/// Case B: OWL 2 RL's `eq-rep-s` as it is, beside a user's inverse rule named `eq-rep-p`
/// (its first premise's predicate is a variable). With no `owl:sameAs` in the data,
/// `eq-rep-s` concludes nothing; the inverse rule entails `ben hasParent anna`.
const INVERSE_NAMED_EQ_REP_P: &str = r#"
Prefices
{
  ex : http://example.com/
  owl : http://www.w3.org/2002/07/owl#
}

Rules
{
Id: eq-rep-s
  s <owl:sameAs> s2
  s p o
  ---------------
  s2 p o

Id: eq-rep-p
  x p y
  p <ex:inverse> q
  ---------------
  y q x
}
"#;

#[test]
fn a_users_rules_named_like_the_equality_rules_entail_what_they_say() {
    let mut wrong = Vec::new();

    let a = store_with(
        SYMMETRY_NAMED_EQ_REP_S,
        &format!("<{EX}a> <{EX}knows> <{EX}b> . <{EX}a> a <{EX}Dog> ."),
    );
    wrong.extend(ask_all(
        "A (symmetry named eq-rep-s)",
        &a,
        &[
            (format!("<{EX}b> <{EX}knows> <{EX}a>"), true),
            (format!("<{EX}b> a <{EX}Dog>"), false),
            (format!("<{EX}a> <{EX}knows> <{EX}a>"), false),
            (format!("<{EX}b> <{EX}knows> <{EX}b>"), false),
        ],
    ));

    let b = store_with(
        INVERSE_NAMED_EQ_REP_P,
        &format!(
            "<{EX}anna> <{EX}hasChild> <{EX}ben> . <{EX}hasChild> <{EX}inverse> <{EX}hasParent> ."
        ),
    );
    wrong.extend(ask_all(
        "B (inverse named eq-rep-p)",
        &b,
        &[
            (format!("<{EX}ben> <{EX}hasParent> <{EX}anna>"), true),
            (format!("<{EX}anna> <{EX}hasParent> <{EX}ben>"), false),
        ],
    ));

    assert!(wrong.is_empty(), "wrong cells:\n{}", wrong.join("\n"));
}
