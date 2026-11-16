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

fn apply(pipeline: &MutationPipeline, update: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update.to_owned())),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap_or_else(|error| panic!("{update}: {error}"));
}

const OWL: &str = "http://www.w3.org/2002/07/owl#";

/// OWL 2 RL's five equality rules and a subclass rule in Notation3, whose rules carry no
/// equality rule's name. As ordinary rules they entail, over `a sameAs b`, `a a Dog`, `c a Dog`
/// and `Dog subClassOf Animal`: `b` an Animal, `b sameAs a`, `a sameAs a` and `b sameAs b`; and
/// nothing reflexive for `c` or `Dog`, which are the same as nothing (no `eq-ref`).
const EQUALITY_N3: &str = "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
{ ?x rdf:type ?c . ?c rdfs:subClassOf ?d } => { ?x rdf:type ?d } .
{ ?x owl:sameAs ?y } => { ?y owl:sameAs ?x } .
{ ?x owl:sameAs ?y . ?y owl:sameAs ?z } => { ?x owl:sameAs ?z } .
{ ?s owl:sameAs ?t . ?s ?p ?o } => { ?t ?p ?o } .
{ ?p owl:sameAs ?q . ?s ?p ?o } => { ?s ?q ?o } .
{ ?o owl:sameAs ?t . ?s ?p ?o } => { ?s ?p ?t } .
";

/// A program that holds the equality rules under other names answers as those rules entail as
/// ordinary rules, under each `reasoner.equality` (representatives, compact, replicate), right
/// after its commits and after a restart.
#[test]
fn equality_rules_under_other_names_answer_as_ordinary_rules_through_a_restart() {
    let rules = Arc::new(UserRules::n3("equality.n3", EQUALITY_N3).expect("the rules parse"));
    let reasoner = ReasonerConfig::for_mode(ReasoningMode::Custom)
        .with_rules(Some(rules))
        .expect("custom rules");
    let same = format!("<{OWL}sameAs>");
    let expected: Vec<(String, bool)> = [
        ("a", "a", "Dog", true),
        ("b", "a", "Animal", true),
        ("c", "a", "Animal", true),
        ("b", same.as_str(), "a", true),
        ("a", same.as_str(), "a", true),
        ("b", same.as_str(), "b", true),
        ("c", same.as_str(), "c", false),
        ("Dog", same.as_str(), "Dog", false),
        ("c", same.as_str(), "a", false),
    ]
    .iter()
    .map(|&(s, p, o, holds)| (format!("<{EX}{s}> {p} <{EX}{o}>"), holds))
    .collect();
    let mut wrong = Vec::new();
    for (equality, representatives, compact) in [
        ("representatives", true, false),
        ("compact", true, true),
        ("replicate", false, false),
    ] {
        let dir = tempfile::tempdir().expect("dir");
        let open = || {
            let config = nrese_store::StoreConfig {
                equality_by_representatives: representatives,
                equality_compact: compact,
                ..nrese_store::StoreConfig::on_disk(dir.path().join("store"))
            };
            MutationPipeline::new(
                Arc::new(StoreService::new(config).expect("store")),
                Arc::new(ReasonerService::new(reasoner.clone())),
            )
        };
        {
            let written = open();
            apply(
                &written,
                &format!("INSERT DATA {{ <{EX}a> {same} <{EX}b> }}"),
            );
            apply(
                &written,
                &format!(
                    "INSERT DATA {{ <{EX}a> a <{EX}Dog> . <{EX}c> a <{EX}Dog> . \
                     <{EX}Dog> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <{EX}Animal> }}"
                ),
            );
            let case = format!("{equality}, after the commits");
            wrong.extend(ask_all(&case, &written, &expected));
        }
        let case = format!("{equality}, after a restart");
        wrong.extend(ask_all(&case, &open(), &expected));
    }
    assert!(wrong.is_empty(), "wrong cells:\n{}", wrong.join("\n"));
}

/// A user's rule named `eq-rep-p` that copies `p` to `q`, beside a rule that copies `q` back
/// to `p`: the two derive each other. Deleting the only asserted statement leaves neither: the
/// program entails nothing from an empty store.
const COPIES_NAMED_EQ_REP_P: &str = r#"
Prefices
{
  ex : http://example.com/
}

Rules
{
Id: eq-rep-p
  x <ex:p> y
  ---------------
  x <ex:q> y

Id: back
  x <ex:q> y
  ---------------
  x <ex:p> y
}
"#;

/// A deletion maintained over a user's rule named `eq-rep-p` takes out what only that rule and
/// its partner derived from the deleted statement.
#[test]
fn a_deletion_takes_out_what_a_rule_named_like_an_equality_rule_derived() {
    let store = store_with(COPIES_NAMED_EQ_REP_P, &format!("<{EX}a> <{EX}p> <{EX}b> ."));
    let (p, q) = (
        format!("<{EX}a> <{EX}p> <{EX}b>"),
        format!("<{EX}a> <{EX}q> <{EX}b>"),
    );
    let mut wrong = ask_all(
        "before the deletion",
        &store,
        &[(p.clone(), true), (q.clone(), true)],
    );
    apply(&store, &format!("DELETE DATA {{ {p} }}"));
    wrong.extend(ask_all(
        "after the deletion",
        &store,
        &[(p, false), (q, false)],
    ));
    assert!(wrong.is_empty(), "wrong cells:\n{}", wrong.join("\n"));
}
