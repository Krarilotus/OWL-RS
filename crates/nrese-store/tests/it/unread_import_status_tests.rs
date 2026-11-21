//! An ontology that imports another (OWL 2 Structural Specification §3.4: `owl:imports`
//! names an ontology IRI or a version IRI) reasons over the import closure. The store
//! follows no import: where the imported ontology isn't loaded, its axioms take no part in
//! reasoning, and a read that rests on reasoning isn't told `complete`. An import counts as
//! read where the statements the read reasons over hold `<t> a owl:Ontology` or
//! `?o owl:versionIRI <t>`, or where `<t>` is a W3C vocabulary document (`rdf:`, `rdfs:`,
//! `owl:`, `xsd:`), whose meaning is built in. A document address is not an ontology IRI.

use std::sync::Arc;

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationPipeline, MutationTicket, QlRewritingMode, Requester,
    SolutionsResultFormat, SparqlQueryRequest, SparqlUpdateRequest, StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX : <http://example.org/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> \
     PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> ";

/// The docs author's read: the imported ontology's axiom makes `:tom` an animal.
const READ: &str = "SELECT ?x WHERE { ?x a :Animal }";

/// The words the withheld status's reason holds.
const REASON: &str = "no ontology with that IRI is loaded";

/// The importing ontology and its data, importing `target`.
fn importing(target: &str) -> String {
    format!(":onto-a a owl:Ontology ; owl:imports {target} . :tom a :Cat .")
}

/// The importing ontology and its data with no import: what an import that is read leaves.
const NO_IMPORT: &str = ":onto-a a owl:Ontology . :tom a :Cat .";

/// The imported ontology's axiom.
const AXIOM: &str = ":Cat rdfs:subClassOf :Animal .";

const RULESETS: [&str; 3] = ["owl2-dl", "owl2-ql", "owl2-rl"];

fn open(ruleset: &str) -> MutationPipeline {
    let mut config = StoreConfig::in_memory();
    let mode = match ruleset {
        "owl2-dl" => ReasoningMode::Owl2Dl,
        "owl2-ql" => ReasoningMode::Owl2Ql,
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

fn insert(pipeline: &MutationPipeline, data: &str) {
    let update = format!("{PREFIXES}INSERT DATA {{ {data} }}");
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(update)),
            &Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap_or_else(|error| panic!("{data}: {error:?}"));
}

/// The read's status on a store holding `data`, committed in one commit per part: the word
/// and the reasons' texts.
fn told(ruleset: &str, data: &[&str]) -> (String, Vec<String>) {
    let pipeline = open(ruleset);
    for part in data {
        insert(&pipeline, part);
    }
    let mut request = SparqlQueryRequest::all(format!("{PREFIXES}{READ}"));
    request.solutions_format = SolutionsResultFormat::Json;
    let result = pipeline.store().execute_query(&request).expect("read");
    let status = result.completeness.expect("a reasoned read has a status");
    let reasons = status.reasons.iter().map(|r| r.text.clone()).collect();
    (status.as_str().to_owned(), reasons)
}

/// Each ruleset where the read over `data` isn't withheld for an unread import.
fn not_withheld(data: &[&str]) -> Vec<String> {
    RULESETS
        .into_iter()
        .filter_map(|ruleset| {
            let (word, reasons) = told(ruleset, data);
            let withheld = word != "complete" && reasons.iter().any(|r| r.contains(REASON));
            (!withheld).then(|| format!("{ruleset}: {word} {reasons:?}"))
        })
        .collect()
}

/// Each ruleset where the read over `data` isn't told what it is told over `without`, the
/// same data with no import: the import, read, takes nothing away.
fn not_as_without(data: &[&str], without: &[&str]) -> Vec<String> {
    RULESETS
        .into_iter()
        .filter_map(|ruleset| {
            let (got, reasons) = told(ruleset, data);
            let (want, _) = told(ruleset, without);
            let import = reasons.iter().any(|r| r.contains(REASON));
            (got != want || import).then(|| format!("{ruleset}: {got}, not {want}: {reasons:?}"))
        })
        .collect()
}

/// The docs author's case: the importing ontology loaded, the imported one not.
#[test]
fn a_read_beside_an_unloaded_import_is_not_complete() {
    let unmet = not_withheld(&[&importing("<http://example.org/onto-b.ttl>")]);
    assert!(
        unmet.is_empty(),
        "a read beside an unloaded import is told complete, or not for it:\n{}",
        unmet.join("\n")
    );
}

/// The imported ontology loaded with its header naming the IRI imported, or a version IRI
/// that is: the word comes back.
#[test]
fn an_import_loaded_with_its_header_gives_the_word_back() {
    let header = format!(":onto-b a owl:Ontology . {AXIOM}");
    let version = format!(":onto-b-2 a owl:Ontology ; owl:versionIRI :onto-b-v2 . {AXIOM}");
    let mut unmet = not_as_without(&[&importing(":onto-b"), &header], &[NO_IMPORT, &header]);
    unmet.extend(not_as_without(
        &[&importing(":onto-b-v2"), &version],
        &[NO_IMPORT, &version],
    ));
    assert!(
        unmet.is_empty(),
        "an import loaded with its header doesn't give the word back:\n{}",
        unmet.join("\n")
    );
}

/// The imported ontology loaded, its header naming its own IRI, while the import names its
/// document address: no ontology with the imported IRI is loaded.
#[test]
fn an_import_by_document_address_stays_unread() {
    let unmet = not_withheld(&[
        &importing("<http://example.org/onto-b.ttl>"),
        &format!(":onto-b a owl:Ontology . {AXIOM}"),
    ]);
    assert!(
        unmet.is_empty(),
        "an import by document address is taken as read:\n{}",
        unmet.join("\n")
    );
}

/// A imports B, which is loaded and imports C, which isn't: the import closure isn't loaded.
#[test]
fn a_transitive_import_with_one_link_missing_is_not_complete() {
    let unmet = not_withheld(&[
        &importing(":onto-b"),
        &format!(":onto-b a owl:Ontology ; owl:imports :onto-c . {AXIOM}"),
    ]);
    assert!(
        unmet.is_empty(),
        "a transitive import with one link missing is taken as read:\n{}",
        unmet.join("\n")
    );
}

/// An import of the `owl:` namespace document: its meaning is built in, so it counts as read.
#[test]
fn an_import_of_the_owl_vocabulary_counts_as_read() {
    let data = format!(
        ":onto-a a owl:Ontology ; owl:imports <http://www.w3.org/2002/07/owl> . \
         :tom a :Cat . {AXIOM}"
    );
    let without = format!("{NO_IMPORT} {AXIOM}");
    let unmet = not_as_without(&[&data], &[&without]);
    assert!(
        unmet.is_empty(),
        "an import of the owl: vocabulary takes the word away:\n{}",
        unmet.join("\n")
    );
}
