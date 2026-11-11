//! Package 3.4's route C (`notes/2026-11-08-dict3-4-equality-design.md` §5, 1C): a product
//! query over a client's pending changes, with data closed under `owl:sameAs`, reaches the
//! query side's equality representatives; it and the committed state's queries around it
//! answer as specified. What the query side keeps is guarded in `nrese-sparql`'s
//! `native::equality_tests`; a pending read's snapshot has no shared state (`nrese-engine`'s
//! `shared_state_tests`).

use std::sync::Arc;

use crate::support::in_memory_store_config;
use nrese_reasoner::{ReasonerConfig, ReasonerService, ReasoningMode};
use nrese_store::{
    CancellationToken, MutationCommand, MutationPipeline, MutationTicket, ReadScope,
    SolutionsResultFormat, SparqlQueryRequest, SparqlUpdateRequest, StatementOp, StatementsRequest,
    StoreConfig, StoreService,
};

const PREFIXES: &str = "PREFIX ex: <http://example.com/> \
     PREFIX owl: <http://www.w3.org/2002/07/owl#> ";

/// Per key, the distinct values its partners have: a grouped query whose OPTIONAL feeds
/// `COUNT(DISTINCT …)` alone, which joins on one identity per class.
const QUERY: &str = "SELECT ?k (COUNT(DISTINCT ?x) AS ?n) WHERE { \
     ?k ex:p0 ?y . OPTIONAL { ?y ex:p1 ?x } } GROUP BY ?k ORDER BY ?k";

fn pipeline(compact: bool) -> MutationPipeline {
    let config = StoreConfig {
        equality_by_representatives: compact,
        equality_compact: compact,
        ..in_memory_store_config()
    };
    MutationPipeline::new(
        Arc::new(StoreService::new(config).expect("store")),
        Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
            ReasoningMode::Owl2Rl,
        ))),
    )
}

fn update(pipeline: &MutationPipeline, text: &str) {
    pipeline
        .apply(
            MutationCommand::Update(SparqlUpdateRequest::new(format!("{PREFIXES}{text}"))),
            &nrese_store::Requester::all(),
            &MutationTicket::new(),
        )
        .unwrap_or_else(|error| panic!("{text}: {error}"));
}

/// [`QUERY`]'s answer, `key count` per line: on the committed state, or with `pending` (an
/// update) pending in a client transaction.
fn answer(pipeline: &MutationPipeline, pending: Option<&str>) -> Vec<String> {
    let mut request = SparqlQueryRequest::new(format!("{PREFIXES}{QUERY}"), ReadScope::All);
    request.solutions_format = SolutionsResultFormat::Tsv;
    let prepared = pipeline.store().prepare_query(&request).expect("prepared");
    let mut out = Vec::new();
    match pending {
        Some(text) => {
            let ops = vec![StatementOp::Update(SparqlUpdateRequest::new(format!(
                "{PREFIXES}{text}"
            )))];
            pipeline
                .store()
                .run_query_pending(
                    &StatementsRequest::new(ops),
                    &prepared,
                    &CancellationToken::new(),
                    &mut out,
                )
                .expect("pending query");
        }
        None => {
            pipeline
                .store()
                .run_query(&prepared, &CancellationToken::new(), &mut out)
                .expect("query");
        }
    }
    let text = String::from_utf8(out).expect("utf-8");
    text.lines()
        .skip(1)
        .map(|line| {
            let (key, count) = line.split_once('\t').expect("two columns");
            let digits: String = count
                .trim_start_matches('"')
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            format!("{key} {digits}")
        })
        .collect()
}

/// Review §5 item 1, route C: a client's pending change over closed data, between queries of
/// the committed state.
///
/// - Input: `a` and `b` one identity (`a sameAs b`), `c` alone; `k1 p0 a`, `k2 p0 b`,
///   `k3 p0 c`, `a p1 "1"`, `b p1 "2"`, `c p1 "3"`; reasoned by OWL 2 RL, its equality
///   replicated and kept compact. Pending: `c p1 "3"` deleted, `k3 p0 a` inserted.
/// - Expected: committed, `k1` 2, `k2` 2, `k3` 1 (the class's two values, `c`'s one); pending,
///   every key 2; committed again, as before.
#[test]
fn a_pending_query_over_equality_answers_between_committed_ones() {
    let committed = [
        "<http://example.com/k1> 2",
        "<http://example.com/k2> 2",
        "<http://example.com/k3> 1",
    ];
    let changed = [
        "<http://example.com/k1> 2",
        "<http://example.com/k2> 2",
        "<http://example.com/k3> 2",
    ];
    for compact in [false, true] {
        let pipeline = pipeline(compact);
        update(
            &pipeline,
            "INSERT DATA { ex:a owl:sameAs ex:b . ex:k1 ex:p0 ex:a . ex:k2 ex:p0 ex:b . \
             ex:k3 ex:p0 ex:c . ex:a ex:p1 \"1\" . ex:b ex:p1 \"2\" . ex:c ex:p1 \"3\" }",
        );
        assert_eq!(answer(&pipeline, None), committed, "compact {compact}");
        let pending = "DELETE DATA { ex:c ex:p1 \"3\" } ; INSERT DATA { ex:k3 ex:p0 ex:a }";
        for round in 0..2 {
            assert_eq!(
                answer(&pipeline, Some(pending)),
                changed,
                "compact {compact}, pending, round {round}"
            );
            assert_eq!(
                answer(&pipeline, None),
                committed,
                "compact {compact}, committed, round {round}"
            );
        }
    }
}
