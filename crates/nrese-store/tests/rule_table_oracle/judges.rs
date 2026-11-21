use std::{path::Path, sync::Arc};

use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    MutationCommand, MutationError, MutationPipeline, MutationTicket, QlRewritingMode, RejectKind,
    Requester, SolutionsResultFormat, SparqlQueryRequest, SparqlUpdateRequest, StoreConfig,
    StoreService,
};
use serde_json::Value;

use crate::{Facts, Triple, generator::Write};

const NAMESPACES: &[(&str, &str)] = &[
    ("ex", "http://example.org/"),
    ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
    ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
    ("owl", "http://www.w3.org/2002/07/owl#"),
    ("xsd", "http://www.w3.org/2001/XMLSchema#"),
];

pub const MODES: &[&str] = &["replication", "representatives", "compact"];

pub fn open(mode: &str, path: Option<&Path>) -> Arc<StoreService> {
    let mut config = path.map_or_else(StoreConfig::in_memory, StoreConfig::on_disk);
    config.equality_by_representatives = mode != "replication";
    config.equality_compact = mode == "compact";
    config.hide_unnamed_classes = true;
    config.ql_rewriting = QlRewritingMode::On;
    Arc::new(StoreService::new(config).expect("open judge store"))
}

pub fn pipeline(store: &Arc<StoreService>, mode: ReasoningMode) -> MutationPipeline {
    MutationPipeline::new(
        Arc::clone(store),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(mode))),
    )
}

fn sparql(body: &str) -> String {
    let prefixes: String = NAMESPACES
        .iter()
        .map(|(p, iri)| format!("PREFIX {p}: <{iri}>\n"))
        .collect();
    format!("{prefixes}{body}")
}

pub fn apply(pipeline: &MutationPipeline, write: &Write) -> bool {
    let operation = |verb: &str, rows: &[Triple]| {
        let triples: String = rows
            .iter()
            .map(|row| format!(" {} .", row.join(" ")))
            .collect();
        format!("{verb} DATA {{ {triples} }}")
    };
    let body = match (write.delete.is_empty(), write.insert.is_empty()) {
        (true, _) => operation("INSERT", &write.insert),
        (_, true) => operation("DELETE", &write.delete),
        (false, false) => format!(
            "{} ; {}",
            operation("DELETE", &write.delete),
            operation("INSERT", &write.insert)
        ),
    };
    apply_update(pipeline, &body)
}

pub fn apply_update(pipeline: &MutationPipeline, body: &str) -> bool {
    match pipeline.apply(
        MutationCommand::Update(SparqlUpdateRequest::new(sparql(body))),
        &Requester::all(),
        &MutationTicket::new(),
    ) {
        Ok(_) => true,
        Err(MutationError::Rejected(refusal)) if refusal.kind == RejectKind::Inconsistent => false,
        Err(error) => panic!("update failed before reasoning judged it: {error:?}\n{body}"),
    }
}

pub fn first_and_rebuild(
    mode: &str,
    asserted: &[Triple],
    rebuild: bool,
) -> (Arc<StoreService>, bool) {
    let store = open(mode, None);
    let write = Write {
        insert: asserted.to_vec(),
        ..Write::default()
    };
    let rl = ReasoningMode::Owl2Rl;
    let accepted = if rebuild {
        let raw = pipeline(&store, ReasoningMode::Disabled);
        assert!(apply(&raw, &write), "the unreasoned input must load");
        let _reader = pipeline(&store, rl);
        let program = ReasonerConfig::for_mode(rl)
            .materialised_program()
            .expect("RL program");
        store
            .rematerialise(program)
            .expect("rebuild judge")
            .violations
            == 0
    } else {
        apply(&pipeline(&store, rl), &write)
    };
    (store, accepted)
}

fn compact_iri(iri: &str) -> String {
    NAMESPACES
        .iter()
        .find_map(|(prefix, base)| {
            iri.strip_prefix(base)
                .map(|name| format!("{prefix}:{name}"))
        })
        .unwrap_or_else(|| format!("<{iri}>"))
}

fn term(binding: &Value) -> String {
    let value = binding["value"].as_str().expect("bound RDF term");
    match binding["type"].as_str().unwrap() {
        "uri" => compact_iri(value),
        "bnode" => format!("_:{value}"),
        "literal" | "typed-literal" => {
            let lexical = serde_json::to_string(value).unwrap();
            if let Some(datatype) = binding["datatype"].as_str() {
                format!("{lexical}^^{}", compact_iri(datatype))
            } else {
                lexical
            }
        }
        other => panic!("unexpected binding type {other}"),
    }
}

pub fn rows(store: &StoreService) -> (Facts, String) {
    let (answer, status) = query(store, "SELECT DISTINCT ?s ?p ?o WHERE { ?s ?p ?o }");
    let rows = answer["results"]["bindings"]
        .as_array()
        .expect("SELECT results");
    let mut facts: Facts = rows
        .iter()
        .map(|row| ["s", "p", "o"].map(|name| term(&row[name])))
        .collect();
    // The unnamed-union fixtures contain one blank node. INSERT DATA assigns
    // a fresh ID, while the oracle calls that node _:union.
    let mut blanks = facts.iter().flatten().filter(|term| term.starts_with("_:"));
    if let Some(first) = blanks.next() {
        assert!(blanks.all(|term| term == first), "one fixture blank node");
        facts = facts
            .into_iter()
            .map(|row| {
                row.map(|term| {
                    if term.starts_with("_:") {
                        "_:union".to_owned()
                    } else {
                        term
                    }
                })
            })
            .collect();
    }
    (facts, status)
}

pub fn ask(store: &StoreService, fact: &Triple) -> (bool, String) {
    let (answer, status) = query(store, &format!("ASK {{ {} }}", fact.join(" ")));
    (answer["boolean"].as_bool().expect("ASK result"), status)
}

fn query(store: &StoreService, body: &str) -> (Value, String) {
    let mut request = SparqlQueryRequest::all(sparql(body));
    request.solutions_format = SolutionsResultFormat::Json;
    let result = store.execute_query(&request).expect("read judge");
    (
        serde_json::from_slice(&result.payload).expect("SPARQL JSON result"),
        result
            .completeness
            .as_ref()
            .map_or("none", |status| status.as_str())
            .to_owned(),
    )
}
