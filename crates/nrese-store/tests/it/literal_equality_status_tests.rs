//! One value in two spellings on a functional property (OWL 2 Profiles §4.3, Table 5
//! prp-fp; Table 6 cls-maxc2 for an at-most-one restriction): the rules equate the two
//! literals, and that `owl:sameAs` has a literal subject, which no store holds. A value
//! written in a later commit then gets no copy in the other spelling (Table 4 eq-rep-o),
//! where the same data in one commit do; under compact equality not even in one commit.
//! While the rules have derived such an equality, a read of a property that holds typed
//! values is not told `complete` (docs/limits.md, "two spellings across commits"). The
//! answers stay as they are: only the word drops.

use std::{path::Path, sync::Arc};

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, QlRewritingMode, Requester,
    SolutionsResultFormat, SparqlQueryRequest, SparqlUpdateRequest, StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX : <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
     PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ";

const INTEGER: &str = "\"1\"^^xsd:integer";
const DECIMAL: &str = "\"1.0\"^^xsd:decimal";

/// The sources of the equality: a functional property, and an at-most-one restriction.
const SOURCES: [&str; 2] = [
    ":p a owl:FunctionalProperty .",
    ":s a :C . :C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; \
     owl:maxCardinality \"1\"^^xsd:nonNegativeInteger ] .",
];

/// The reads of the later value: its values, and their number.
const READS: [&str; 2] = [
    "SELECT ?v WHERE { :t :q ?v }",
    "SELECT (COUNT(*) AS ?n) WHERE { :t :q ?v }",
];

/// The words the withheld status's reason holds.
const REASON: &str = "an owl:sameAs between literals";

const RULESETS: [&str; 2] = ["owl2-dl", "owl2-rl"];
const MODES: [&str; 3] = ["replication", "representatives", "compact"];

fn config(mode: &str, dir: Option<&Path>) -> StoreConfig {
    let mut config = dir.map_or_else(StoreConfig::in_memory, StoreConfig::on_disk);
    config.equality_by_representatives = mode != "replication";
    config.equality_compact = mode == "compact";
    config
}

fn open(ruleset: &str, mut config: StoreConfig) -> MutationPipeline {
    let mode = match ruleset {
        "owl2-dl" => ReasoningMode::Owl2Dl,
        "owl2-rl" => {
            config.ql_rewriting = QlRewritingMode::On;
            ReasoningMode::Owl2Rl
        }
        other => panic!("no ruleset {other}"),
    };
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn update(pipeline: &MutationPipeline, body: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{body}"))),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap_or_else(|error| panic!("{body}: {error:?}"));
}

fn insert(pipeline: &MutationPipeline, data: &str) {
    update(pipeline, &format!("INSERT DATA {{ {data} }}"));
}

/// `query`'s answer (its JSON) and its status: the word and the reasons' texts.
fn read(store: &StoreService, query: &str) -> (String, String, Vec<String>) {
    let mut request = SparqlQueryRequest::all(format!("{PREFIXES}{query}"));
    request.solutions_format = SolutionsResultFormat::Json;
    let result = store.execute_query(&request).expect("read");
    let status = result.completeness.expect("a reasoned read has a status");
    (
        String::from_utf8(result.payload).expect("utf8"),
        status.as_str().to_owned(),
        status.reasons.iter().map(|r| r.text.clone()).collect(),
    )
}

/// Each read of [`READS`] on `store` that is told `complete`, or not for the literal
/// equality, as `case: read: word reasons`.
fn told_otherwise(store: &StoreService, case: &str, unmet: &mut Vec<String>) {
    for query in READS {
        let (_, word, reasons) = read(store, query);
        if word == "complete" || !reasons.iter().any(|r| r.contains(REASON)) {
            unmet.push(format!("{case}: {query}: {word} {reasons:?}"));
        }
    }
}

/// The value written later and the other spelling in the pair, both ways round.
fn spellings() -> [(&'static str, &'static str); 2] {
    [(INTEGER, DECIMAL), (DECIMAL, INTEGER)]
}

#[test]
fn a_value_read_after_two_spellings_were_equated_is_not_complete() {
    let mut unmet = Vec::new();
    let mut controls = Vec::new();
    for ruleset in RULESETS {
        for mode in MODES {
            for (source, axiom) in SOURCES.iter().enumerate() {
                for (written, other) in spellings() {
                    let pair = format!("{axiom} :s :p {written} , {other} .");
                    let later = format!(":t :q {written} .");
                    let case = format!("{ruleset} {mode} source {source} {written}");
                    let two = open(ruleset, config(mode, None));
                    insert(&two, &pair);
                    insert(&two, &later);
                    told_otherwise(two.store(), &format!("{case} two commits"), &mut unmet);
                    let one = open(ruleset, config(mode, None));
                    insert(&one, &format!("{pair} {later}"));
                    told_otherwise(one.store(), &format!("{case} one commit"), &mut unmet);
                    // The same values with no equality between them: the word stays.
                    let plain = open(ruleset, config(mode, None));
                    insert(&plain, &format!(":s :p {written} , {other} . {later}"));
                    let (_, word, reasons) = read(plain.store(), READS[0]);
                    if word != "complete" {
                        controls.push(format!("{case}: {word} {reasons:?}"));
                    }
                }
            }
        }
    }
    assert!(
        unmet.is_empty(),
        "a read after a literal equality is told complete, or not for it:\n{}",
        unmet.join("\n")
    );
    assert!(
        controls.is_empty(),
        "with no equality between the spellings the read is complete:\n{}",
        controls.join("\n")
    );
}

/// A library store reopened from disk is not rematerialised: the condition holds across the
/// reopen all the same, and a value written after it is withheld.
#[test]
fn a_reopened_store_withholds_the_word_after_a_literal_equality() {
    let mut unmet = Vec::new();
    for ruleset in RULESETS {
        for mode in MODES {
            let dir = tempfile::tempdir().expect("dir");
            {
                let before = open(ruleset, config(mode, Some(dir.path())));
                insert(
                    &before,
                    &format!("{} :s :p {INTEGER} , {DECIMAL} .", SOURCES[0]),
                );
            }
            let after = open(ruleset, config(mode, Some(dir.path())));
            insert(&after, &format!(":t :q {INTEGER} ."));
            assert!(
                after.store().last_materialisation().is_none(),
                "{ruleset} {mode}: the reopened store was rematerialised"
            );
            told_otherwise(
                after.store(),
                &format!("{ruleset} {mode} reopened"),
                &mut unmet,
            );
        }
    }
    assert!(
        unmet.is_empty(),
        "after a reopen, a read after a literal equality is told complete:\n{}",
        unmet.join("\n")
    );
}

/// The condition stays while the store holds the pair's derivation, a delete of its premise
/// included (the commit can't tell which later value lacks a copy); a rebuild that finds no
/// literal equality clears it, and one that finds it again keeps it.
#[test]
fn only_a_rebuild_that_finds_no_literal_equality_gives_the_word_back() {
    for mode in MODES {
        let pipeline = open("owl2-rl", config(mode, None));
        let store = pipeline.store();
        insert(
            &pipeline,
            &format!("{} :s :p {INTEGER} , {DECIMAL} .", SOURCES[0]),
        );
        insert(&pipeline, &format!(":t :q {INTEGER} ."));
        let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl)
            .materialised_program()
            .expect("owl2-rl rules");
        store.rematerialise(rules.clone()).expect("rebuild");
        let mut unmet = Vec::new();
        told_otherwise(store, &format!("{mode} rebuilt with the pair"), &mut unmet);
        update(&pipeline, &format!("DELETE DATA {{ :s :p {DECIMAL} }}"));
        told_otherwise(store, &format!("{mode} the pair deleted"), &mut unmet);
        assert!(
            unmet.is_empty(),
            "withheld until a clean rebuild:\n{}",
            unmet.join("\n")
        );
        store.rematerialise(rules).expect("rebuild");
        let (_, word, reasons) = read(store, READS[0]);
        assert_eq!(
            word, "complete",
            "{mode}: a rebuild with no literal equality gives the word back: {reasons:?}"
        );
    }
}
