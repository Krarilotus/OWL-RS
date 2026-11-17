//! A user's rules named like OWL 2 RL's equality rules (`eq-rep-s`, `eq-rep-p`, `eq-rep-o`)
//! are the user's rules: a store with a custom program answers what that program entails,
//! whatever its rules are called. The expected answers are written from the PIE dialect's
//! meaning (a rule's premises entail its conclusions; a rule's `Id` is only its name).
//! Default settings throughout (the equality mode included).

use std::{collections::BTreeSet, sync::Arc};

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

/// [`EQUALITY_N3`] without `eq-sym` and `eq-trans`: the subclass rule and the three rules that
/// copy a fact to a `sameAs` partner of its subject, predicate or object, which are not all of
/// the equality rules, so each runs as written. Over `a sameAs b`, `b sameAs c`, `a a Dog` and
/// `d a Dog` they entail `b` and `c` Dogs, `b sameAs b`, `c sameAs c`, `c sameAs b` and
/// `a sameAs c` (the object copy of `a sameAs b` to `c`); not `b sameAs a`, `a sameAs a` or
/// `c sameAs a`, which [`EQUALITY_N3`] entails.
const COPIES_N3: &str = "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
{ ?x rdf:type ?c . ?c rdfs:subClassOf ?d } => { ?x rdf:type ?d } .
{ ?s owl:sameAs ?t . ?s ?p ?o } => { ?t ?p ?o } .
{ ?p owl:sameAs ?q . ?s ?p ?o } => { ?s ?q ?o } .
{ ?o owl:sameAs ?t . ?s ?p ?o } => { ?s ?p ?t } .
";

/// After a store's rules change, its commits answer as the new rules entail: two commits under
/// [`EQUALITY_N3`], then a pipeline with [`COPIES_N3`] takes the store over, and its second
/// commit, maintained from the first, brings `b sameAs c`. Those rules copy `owl:sameAs` facts
/// as they copy any other.
#[test]
fn a_commit_after_the_rules_change_answers_as_the_new_rules_entail() {
    let store = Arc::new(StoreService::new(in_memory_store_config()).expect("store"));
    let pipeline = |n3: &str| {
        let rules = Arc::new(UserRules::n3("rules.n3", n3).expect("the rules parse"));
        let config = ReasonerConfig::for_mode(ReasoningMode::Custom)
            .with_rules(Some(rules))
            .expect("custom rules");
        MutationPipeline::new(store.clone(), Arc::new(ReasonerService::new(config)))
    };
    let same = format!("<{OWL}sameAs>");
    let before = pipeline(EQUALITY_N3);
    apply(
        &before,
        &format!("INSERT DATA {{ <{EX}a> {same} <{EX}b> }}"),
    );
    apply(&before, &format!("INSERT DATA {{ <{EX}a> a <{EX}Dog> }}"));
    // As a repository's new rules take over its store.
    before.retire();
    let after = pipeline(COPIES_N3);
    apply(&after, &format!("INSERT DATA {{ <{EX}d> a <{EX}Dog> }}"));
    apply(&after, &format!("INSERT DATA {{ <{EX}b> {same} <{EX}c> }}"));
    let expected: Vec<(String, bool)> = [
        ("b", "a", "Dog", true),
        ("c", "a", "Dog", true),
        ("b", same.as_str(), "b", true),
        ("c", same.as_str(), "c", true),
        ("c", same.as_str(), "b", true),
        ("a", same.as_str(), "c", true),
        ("b", same.as_str(), "a", false),
        ("a", same.as_str(), "a", false),
        ("c", same.as_str(), "a", false),
    ]
    .iter()
    .map(|&(s, p, o, holds)| (format!("<{EX}{s}> {p} <{EX}{o}>"), holds))
    .collect();
    let wrong = ask_all("after the rules change", &after, &expected);
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

/// A user's subclass rule `C ⊑ D` named `eq-rep-p`, beside `D ⊑ C` and `B ⊑ C`: `C` and `D` derive
/// each other, and only `B` supports them. Deleting the one asserted `a a B` leaves neither: the
/// program entails nothing from an empty store.
const CLASSES_NAMED_EQ_REP_P: &str = r#"
Prefices
{
  ex : http://example.com/
  rdf : http://www.w3.org/1999/02/22-rdf-syntax-ns#
}

Rules
{
Id: from-b
  x <rdf:type> <ex:B>
  ---------------
  x <rdf:type> <ex:C>

Id: eq-rep-p
  x <rdf:type> <ex:C>
  ---------------
  x <rdf:type> <ex:D>

Id: back
  x <rdf:type> <ex:D>
  ---------------
  x <rdf:type> <ex:C>
}
"#;

/// A deletion maintained over a cycle of class rules, one of them named `eq-rep-p`, takes out
/// what the cycle derived from the deleted statement: `back` is on a cycle with the rule named
/// `eq-rep-p`, so `a a C` is not kept by its one step from `a a D`.
#[test]
fn a_deletion_takes_out_a_cycle_through_a_class_rule_named_like_an_equality_rule() {
    let store = store_with(CLASSES_NAMED_EQ_REP_P, &format!("<{EX}a> a <{EX}B> ."));
    let (c, d) = (format!("<{EX}a> a <{EX}C>"), format!("<{EX}a> a <{EX}D>"));
    let mut wrong = ask_all(
        "before the deletion",
        &store,
        &[(c.clone(), true), (d.clone(), true)],
    );
    apply(&store, &format!("DELETE DATA {{ <{EX}a> a <{EX}B> }}"));
    wrong.extend(ask_all(
        "after the deletion",
        &store,
        &[(c, false), (d, false)],
    ));
    assert!(wrong.is_empty(), "wrong cells:\n{}", wrong.join("\n"));
}

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

/// OWL 2 RL's five equality rules over `owl:sameAs` under other names, beside two of the user's
/// rules named like equality rules that say something else: an inverse rule named `eq-sym` and
/// a copy of `p` to `q` named `eq-rep-p`. The five engage the equality module; the two are
/// ordinary rules. As ordinary rules, over `a sameAs b`, `anna hasChild ben` and `a p c`, they
/// entail `ben hasParent anna`, `a q c`, and the copies `b p c` and `b q c`.
const FIVE_BESIDE_NAMESAKES: &str = r#"
Prefices
{
  ex : http://example.com/
  owl : http://www.w3.org/2002/07/owl#
}

Rules
{
Id: same-sym
  x <owl:sameAs> y
  ---------------
  y <owl:sameAs> x

Id: same-trans
  x <owl:sameAs> y
  y <owl:sameAs> z
  ---------------
  x <owl:sameAs> z

Id: same-subject
  s <owl:sameAs> t
  s p o
  ---------------
  t p o

Id: same-predicate
  p <owl:sameAs> q
  s p o
  ---------------
  s q o

Id: same-object
  o <owl:sameAs> t
  s p o
  ---------------
  s p t

Id: eq-sym
  x <ex:hasChild> y
  ---------------
  y <ex:hasParent> x

Id: eq-rep-p
  x <ex:p> y
  ---------------
  x <ex:q> y
}
"#;

/// Where the five equality rules engage the module, a user's rules named like equality rules
/// still entail what they say, under each `reasoner.equality`: at the first commit, after an
/// insertion and after deletions maintained over them (of an equality, then of a statement).
#[test]
fn rules_named_like_the_equality_rules_beside_the_five_entail_what_they_say() {
    let rules = Arc::new(UserRules::pie("user.pie", FIVE_BESIDE_NAMESAKES).expect("it compiles"));
    let reasoner = ReasonerConfig::for_mode(ReasoningMode::Custom)
        .with_rules(Some(rules))
        .expect("custom rules");
    let same = format!("<{OWL}sameAs>");
    let fact = |s: &str, p: &str, o: &str| format!("<{EX}{s}> <{EX}{p}> <{EX}{o}>");
    let cell = |s: &str, p: &str, o: &str, holds: bool| (fact(s, p, o), holds);
    let mut wrong = Vec::new();
    for (equality, representatives, compact) in [
        ("representatives", true, false),
        ("compact", true, true),
        ("replicate", false, false),
    ] {
        let config = nrese_store::StoreConfig {
            equality_by_representatives: representatives,
            equality_compact: compact,
            ..in_memory_store_config()
        };
        let store = MutationPipeline::new(
            Arc::new(StoreService::new(config).expect("store")),
            Arc::new(ReasonerService::new(reasoner.clone())),
        );
        let first = format!(
            "INSERT DATA {{ <{EX}a> {same} <{EX}b> . {} . {} }}",
            fact("anna", "hasChild", "ben"),
            fact("a", "p", "c")
        );
        apply(&store, &first);
        let case = format!("{equality}, the first commit");
        wrong.extend(ask_all(
            &case,
            &store,
            &[
                cell("ben", "hasParent", "anna", true),
                cell("anna", "hasParent", "ben", false),
                cell("a", "q", "c", true),
                cell("b", "p", "c", true),
                cell("b", "q", "c", true),
                cell("c", "q", "a", false),
            ],
        ));
        apply(
            &store,
            &format!("INSERT DATA {{ {} }}", fact("d", "p", "e")),
        );
        let case = format!("{equality}, after an insertion");
        wrong.extend(ask_all(&case, &store, &[cell("d", "q", "e", true)]));
        apply(&store, &format!("DELETE DATA {{ <{EX}a> {same} <{EX}b> }}"));
        let case = format!("{equality}, after the equality's deletion");
        wrong.extend(ask_all(
            &case,
            &store,
            &[
                cell("a", "q", "c", true),
                cell("b", "p", "c", false),
                cell("b", "q", "c", false),
                cell("ben", "hasParent", "anna", true),
            ],
        ));
        apply(
            &store,
            &format!("DELETE DATA {{ {} }}", fact("a", "p", "c")),
        );
        let case = format!("{equality}, after a statement's deletion");
        wrong.extend(ask_all(
            &case,
            &store,
            &[cell("a", "q", "c", false), cell("d", "q", "e", true)],
        ));
    }
    assert!(wrong.is_empty(), "wrong cells:\n{}", wrong.join("\n"));
}

/// Case A under each `reasoner.equality`: a symmetry rule named `eq-rep-s` engages no equality
/// mode, so `ex:knows` is never read as `owl:sameAs`.
#[test]
fn a_symmetry_rule_named_eq_rep_s_entails_what_it_says_under_each_equality_mode() {
    let rules = Arc::new(UserRules::pie("user.pie", SYMMETRY_NAMED_EQ_REP_S).expect("it compiles"));
    let reasoner = ReasonerConfig::for_mode(ReasoningMode::Custom)
        .with_rules(Some(rules))
        .expect("custom rules");
    let mut wrong = Vec::new();
    for (equality, representatives, compact) in [
        ("representatives", true, false),
        ("compact", true, true),
        ("replicate", false, false),
    ] {
        let config = nrese_store::StoreConfig {
            equality_by_representatives: representatives,
            equality_compact: compact,
            ..in_memory_store_config()
        };
        let store = MutationPipeline::new(
            Arc::new(StoreService::new(config).expect("store")),
            Arc::new(ReasonerService::new(reasoner.clone())),
        );
        apply(
            &store,
            &format!("INSERT DATA {{ <{EX}a> <{EX}knows> <{EX}b> . <{EX}a> a <{EX}Dog> }}"),
        );
        wrong.extend(ask_all(
            &format!("A under {equality}"),
            &store,
            &[
                (format!("<{EX}b> <{EX}knows> <{EX}a>"), true),
                (format!("<{EX}b> a <{EX}Dog>"), false),
                (format!("<{EX}a> <{EX}knows> <{EX}a>"), false),
            ],
        ));
    }
    assert!(wrong.is_empty(), "wrong cells:\n{}", wrong.join("\n"));
}

/// OWL 2 RL's equality rules in PIE. With `padded`, `eq-rep-s` comes first a second time,
/// with a second conclusion that repeats its premise `s p o`: that conclusion adds nothing,
/// so both programs entail the same statements.
fn five_equality_rules(padded: bool) -> String {
    let rule = |id: &str, body: &str, head: &str| {
        format!("Id: {id}\n{body}\n  ---------------\n{head}\n\n")
    };
    let padding = rule(
        "padded-subject",
        "  s <owl:sameAs> t\n  s p o",
        "  t p o\n  s p o",
    );
    format!(
        "Prefices\n{{\n  owl : http://www.w3.org/2002/07/owl#\n}}\n\nRules\n{{\n{}{}{}{}{}{}}}\n",
        if padded { padding.as_str() } else { "" },
        rule("same-sym", "  x <owl:sameAs> y", "  y <owl:sameAs> x"),
        rule(
            "same-trans",
            "  x <owl:sameAs> y\n  y <owl:sameAs> z",
            "  x <owl:sameAs> z"
        ),
        rule("same-subject", "  s <owl:sameAs> t\n  s p o", "  t p o"),
        rule("same-predicate", "  p <owl:sameAs> q\n  s p o", "  s q o"),
        rule("same-object", "  o <owl:sameAs> t\n  s p o", "  s p t"),
    )
}

/// Every statement the store answers, `s p o` as strings.
fn statements(pipeline: &MutationPipeline) -> BTreeSet<String> {
    let query = r#"SELECT (CONCAT(STR(?s), " ", STR(?p), " ", STR(?o)) AS ?t) { ?s ?p ?o }"#;
    let result = pipeline.store().execute_query_str(query).expect("select");
    let json = String::from_utf8(result.payload).expect("utf8");
    json.split(r#""value":""#)
        .skip(1)
        .map(|value| value.split('"').next().unwrap_or_default().to_owned())
        .collect()
}

/// A program whose `eq-rep-s` also concludes its own premise stores, statement for
/// statement, what the program without that conclusion stores, under each
/// `reasoner.equality`, after each commit: insertions, a merge of classes and deletions
/// maintained over them.
#[test]
fn an_equality_rule_concluding_its_premise_stores_what_the_rule_without_it_stores() {
    let same = format!("<{OWL}sameAs>");
    let fact = |s: &str, o: &str| format!("<{EX}{s}> <{EX}p> <{EX}{o}>");
    let commits = [
        format!(
            "INSERT DATA {{ <{EX}a> {same} <{EX}b> . {} }}",
            fact("a", "c")
        ),
        format!("INSERT DATA {{ {} }}", fact("a", "d")),
        format!("INSERT DATA {{ <{EX}b> {same} <{EX}e> }}"),
        format!("DELETE DATA {{ <{EX}a> {same} <{EX}b> }}"),
        format!("DELETE DATA {{ {} }}", fact("a", "c")),
    ];
    let mut different = Vec::new();
    for (equality, representatives, compact) in [
        ("representatives", true, false),
        ("compact", true, true),
        ("replicate", false, false),
    ] {
        let open = |padded: bool| {
            let rules =
                UserRules::pie("user.pie", five_equality_rules(padded)).expect("it compiles");
            let reasoner = ReasonerConfig::for_mode(ReasoningMode::Custom)
                .with_rules(Some(Arc::new(rules)))
                .expect("custom rules");
            let config = nrese_store::StoreConfig {
                equality_by_representatives: representatives,
                equality_compact: compact,
                ..in_memory_store_config()
            };
            MutationPipeline::new(
                Arc::new(StoreService::new(config).expect("store")),
                Arc::new(ReasonerService::new(reasoner)),
            )
        };
        let (padded, plain) = (open(true), open(false));
        let (mut verdict, mut sizes) = (None, Vec::new());
        for (k, update) in commits.iter().enumerate() {
            apply(&padded, update);
            apply(&plain, update);
            let (mine, theirs) = (statements(&padded), statements(&plain));
            sizes.push(format!("{}/{}", mine.len(), theirs.len()));
            // Not two closures equal for deriving nothing: the plain program copies `a p d`
            // to `b`, the subject's partner.
            if k == 1 {
                let copy = format!("{EX}b {EX}p {EX}d");
                assert!(
                    theirs.contains(&copy),
                    "{equality}: the plain program stores no `{copy}` after commit 2"
                );
            }
            if mine != theirs && verdict.is_none() {
                verdict = Some(format!(
                    "DIFFERENT after commit {}: padded only {:?}; plain only {:?}",
                    k + 1,
                    mine.difference(&theirs).collect::<Vec<_>>(),
                    theirs.difference(&mine).collect::<Vec<_>>()
                ));
            }
        }
        let verdict = verdict.unwrap_or_else(|| "equal".to_owned());
        println!(
            "CASE {equality}: {verdict} (statements, padded/plain, after each commit: {})",
            sizes.join(" ")
        );
        if verdict != "equal" {
            different.push(format!("{equality}: {verdict}"));
        }
    }
    assert!(
        different.is_empty(),
        "different closures:\n{}",
        different.join("\n")
    );
}
