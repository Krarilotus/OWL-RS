//! HTTP acceptance cases for inference scoped to a query's dataset.

use std::error::Error;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_store::StoreConfig;
use serde_json::Value;
use tower::util::ServiceExt;

use crate::access_control_tests::{policy_of, token};
use crate::support::{body_text, test_app_with_store_config};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;
type Rows = Result<Vec<String>, (StatusCode, String)>;

const PREFIXES: &str = "PREFIX ex: <urn:ex:> PREFIX : <urn:graph:>
    PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
    PREFIX onto: <http://www.ontotext.com/> ";
const SCOPED: &str = "?inference-scope=dataset";
const G1: &str = "FROM :g1";
const BOTH: &str = "FROM :g1 FROM :g2";
const READER: &[&str] = &["nrese.read", "one"];
const BOTH_READER: &[&str] = &["nrese.read", "both"];
const ADMIN: &[&str] = &["nrese.admin"];
const POLICY: &str = r#"
default = "deny"
inferred = "supported"

[[role]]
name = "one"
read = ["urn:graph:g1"]
default_graph = "read"

[[role]]
name = "both"
read = ["urn:graph:g1", "urn:graph:g2"]
default_graph = "read"
"#;
const G1_FACT: &str = "GRAPH :g1 { ex:a ex:p ex:b }";
const SPLIT: &str = "GRAPH :g1 { ex:a ex:p ex:b }
    GRAPH :g2 { ex:p rdfs:subPropertyOf ex:q }";
const INSIDE: &str = "GRAPH :g1 { ex:a ex:p ex:b . ex:p rdfs:subPropertyOf ex:q }";
const SUPPRESSED: &str = "GRAPH :g1 { ex:a ex:p ex:b . ex:p rdfs:subPropertyOf ex:q }
    GRAPH :g2 { ex:a ex:q ex:b }";
const CONCLUSION: &str = "{ ?s ex:q ex:b }";
const A: &str = "<urn:ex:a>";
const P: &str = "<urn:ex:p>";
const Q: &str = "<urn:ex:q>";

fn app(store: StoreConfig, restricted: bool) -> TestResult<Router> {
    let policy = if restricted {
        policy_of(POLICY)?
    } else {
        Default::default()
    };
    test_app_with_store_config(
        store,
        policy,
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )
}

async fn send(
    app: &Router,
    roles: &[&str],
    method: Method,
    uri: &str,
    content_type: &str,
    body: String,
) -> TestResult<Response> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCEPT, "text/tab-separated-values");
    if !roles.is_empty() {
        request = request.header(header::AUTHORIZATION, format!("Bearer {}", token(roles)));
    }
    Ok(app.clone().oneshot(request.body(Body::from(body))?).await?)
}

async fn insert(app: &Router, facts: &str) -> TestResult {
    let response = send(
        app,
        ADMIN,
        Method::POST,
        "/dataset/update",
        "application/sparql-update",
        format!("{PREFIXES} INSERT DATA {{ {facts} }}"),
    )
    .await?;
    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "insert {facts}: {}",
        body_text(response).await?
    );
    Ok(())
}

#[derive(Debug)]
struct Reply {
    code: StatusCode,
    completeness: Option<String>,
    body: String,
}

impl Reply {
    async fn read(response: Response) -> TestResult<Self> {
        Ok(Self {
            code: response.status(),
            completeness: response
                .headers()
                .get("nrese-completeness")
                .map(|value| value.to_str().map(str::to_owned))
                .transpose()?,
            body: body_text(response).await?,
        })
    }

    /// Sorted TSV bindings retain duplicates. Refusals are evidence at the assertion.
    fn rows(&self) -> Rows {
        let mut lines = self.body.lines();
        if self.code != StatusCode::OK || !lines.next().is_some_and(|line| line.starts_with('?')) {
            return Err((self.code, self.body.clone()));
        }
        let mut rows: Vec<_> = lines.map(str::to_owned).collect();
        rows.sort_unstable();
        Ok(rows)
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

async fn select(
    app: &Router,
    roles: &[&str],
    options: &str,
    projection: &str,
    dataset: &str,
    pattern: &str,
) -> TestResult<Reply> {
    Reply::read(
        send(
            app,
            roles,
            Method::POST,
            &format!("/dataset/query{options}"),
            "application/sparql-query",
            format!("{PREFIXES} SELECT {projection} {dataset} WHERE {pattern}"),
        )
        .await?,
    )
    .await
}

fn bindings(values: &[&str]) -> Rows {
    Ok(values.iter().map(|value| (*value).to_owned()).collect())
}

#[tokio::test]
async fn derivation_uses_only_dataset_graphs() -> TestResult {
    // Spec: Dataset-scoped inference derives a conclusion only when both premises are in
    // the dataset.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, SPLIT).await?;
    let one = select(&app, &[], SCOPED, "?s", G1, CONCLUSION).await?;
    let both = select(&app, &[], SCOPED, "?s", BOTH, CONCLUSION).await?;
    assert_eq!((one.rows(), both.rows()), (bindings(&[]), bindings(&[A])));
    Ok(())
}

#[tokio::test]
async fn scoped_support_counts_owl_list_premises() -> TestResult {
    // Spec: A property-chain derivation needs the list axiom and its list triples
    // in the dataset, as well as the data premises.
    let app = app(StoreConfig::default(), false)?;
    insert(
        &app,
        "GRAPH :g1 { ex:a ex:p ex:b . ex:b ex:r ex:c }
         GRAPH :g2 { ex:q <http://www.w3.org/2002/07/owl#propertyChainAxiom> (ex:p ex:r) }",
    )
    .await?;
    let pattern = "{ ?s ex:q ex:c }";
    let one = select(&app, &[], SCOPED, "?s", G1, pattern).await?;
    let both = select(&app, &[], SCOPED, "?s", BOTH, pattern).await?;
    assert_eq!((one.rows(), both.rows()), (bindings(&[]), bindings(&[A])));
    Ok(())
}

#[tokio::test]
async fn scoped_reads_refuse_a_named_graph_without_from() -> TestResult {
    // Spec: Under dataset scoping, every FROM NAMED graph must also occur in FROM.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, SPLIT).await?;
    let reply = select(
        &app,
        &[],
        SCOPED,
        "?s",
        "FROM :g1 FROM NAMED :g2",
        CONCLUSION,
    )
    .await?;
    assert_bad_request(reply, "FROM NAMED");
    Ok(())
}

#[tokio::test]
async fn scoped_reads_include_assertions_from_graphs_named_in_both_clauses() -> TestResult {
    // Spec: A graph named in both FROM and FROM NAMED contributes its assertions
    // to the default scan and to GRAPH.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, SUPPRESSED).await?;
    let dataset = "FROM :g1 FROM :g2 FROM NAMED :g2";
    let default = select(&app, &[], SCOPED, "?s", dataset, CONCLUSION).await?;
    let named = select(
        &app,
        &[],
        SCOPED,
        "?s",
        dataset,
        "{ GRAPH :g2 { ?s ex:q ex:b } }",
    )
    .await?;
    assert_eq!(
        (default.rows(), named.rows()),
        (bindings(&[A]), bindings(&[A]))
    );
    Ok(())
}

#[tokio::test]
async fn recovers_a_conclusion_asserted_outside_the_dataset() -> TestResult {
    // Spec: A statement asserted outside the dataset is returned when the selected
    // graph's assertions independently derive it.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, SUPPRESSED).await?;
    let scoped = select(&app, &[], SCOPED, "?s", G1, CONCLUSION).await?;
    let asserted = select(&app, &[], "?infer=false", "?s", G1, CONCLUSION).await?;
    assert_eq!(
        (scoped.rows(), asserted.rows()),
        (bindings(&[A]), bindings(&[])),
        "scoped and asserted-only rows"
    );
    Ok(())
}

#[tokio::test]
async fn physical_default_supplies_neither_rows_nor_premises() -> TestResult {
    // Spec: With dataset-scoped inference, a default-graph assertion and a conclusion
    // requiring a default-graph premise are absent while an in-scope derivation is present.
    let app = app(StoreConfig::default(), false)?;
    insert(
        &app,
        &format!("{INSIDE} ex:a ex:s ex:b . ex:p rdfs:subPropertyOf ex:r ."),
    )
    .await?;
    let answer = select(
        &app,
        &[],
        SCOPED,
        "?p",
        G1,
        "{ ex:a ?p ex:b VALUES ?p { ex:p ex:q ex:r ex:s } }",
    )
    .await?;
    assert_eq!(
        answer.rows(),
        bindings(&[P, Q]),
        "asserted and locally derived predicates"
    );
    Ok(())
}

struct OracleCase {
    name: &'static str,
    dataset: &'static str,
    projection: &'static str,
    pattern: &'static str,
    expected: &'static [&'static str],
}

const SPLIT_CASES: &[OracleCase] = &[
    OracleCase {
        name: "split schema with one graph",
        dataset: G1,
        projection: "?s",
        pattern: CONCLUSION,
        expected: &[],
    },
    OracleCase {
        name: "split schema with both graphs",
        dataset: BOTH,
        projection: "?s",
        pattern: CONCLUSION,
        expected: &[A],
    },
];
const SUPPRESSED_CASES: &[OracleCase] = &[
    OracleCase {
        name: "suppressed assertion with duplicate bindings",
        dataset: G1,
        projection: "?s",
        pattern: "{ ?s ?p ex:b VALUES ?p { ex:p ex:q } }",
        expected: &[A, A],
    },
    OracleCase {
        name: "property path through the recovered inference",
        dataset: G1,
        projection: "?s",
        pattern: "{ ?s ex:q/^ex:p ex:a }",
        expected: &[A],
    },
    OracleCase {
        name: "aggregate over asserted and recovered rows",
        dataset: G1,
        projection: "(COUNT(*) AS ?n)",
        pattern: "{ ex:a ?p ex:b VALUES ?p { ex:p ex:q } }",
        expected: &["2"],
    },
];

#[tokio::test]
async fn matches_an_independently_materialised_repository() -> TestResult {
    // Spec: Scoped solution multisets match a repository materialised from just the
    // selected graphs under the same rules, including for paths and aggregates.
    let config = StoreConfig {
        union_default_graph: true,
        ..StoreConfig::default()
    };
    let mut compared = Vec::new();
    for (facts, cases, one_graph) in [
        (SPLIT, SPLIT_CASES, G1_FACT),
        (SUPPRESSED, SUPPRESSED_CASES, INSIDE),
    ] {
        let full = app(config.clone(), false)?;
        insert(&full, facts).await?;
        let selected = app(config.clone(), false)?;
        insert(&selected, one_graph).await?;
        let both = if facts == SPLIT {
            let oracle = app(config.clone(), false)?;
            insert(&oracle, facts).await?;
            Some(oracle)
        } else {
            None
        };
        for case in cases {
            let oracle = if case.dataset == BOTH {
                both.as_ref().expect("oracle with both graphs")
            } else {
                &selected
            };
            let reference = select(oracle, &[], "", case.projection, "", case.pattern)
                .await?
                .rows();
            let observed = select(
                &full,
                &[],
                SCOPED,
                case.projection,
                case.dataset,
                case.pattern,
            )
            .await?
            .rows();
            compared.push((case.name, reference, observed, bindings(case.expected)));
        }
    }
    assert_oracle(compared);
    Ok(())
}

fn assert_oracle(compared: Vec<(&str, Rows, Rows, Rows)>) {
    assert!(
        compared
            .iter()
            .all(|(_, oracle, scoped, expected)| oracle == expected && scoped == oracle),
        "case, oracle rows, scoped rows, expected rows: {compared:#?}"
    );
}

#[tokio::test]
async fn warm_reads_follow_a_commit() -> TestResult {
    // Spec: Alternating scoped and ordinary reads keep their own answers on a warm cache
    // and both see the facts added by a commit, including their scope's new inferences.
    let app = app(
        StoreConfig {
            query_cache_bytes: 1024 * 1024,
            ..StoreConfig::default()
        },
        false,
    )?;
    insert(&app, INSIDE).await?;
    let pattern = "{ ex:a ?p ?o VALUES ?p { ex:p ex:q } }";
    let mut seen = Vec::new();
    let mut expected = Vec::new();
    for change in [false, true] {
        if change {
            insert(&app, "GRAPH :g1 { ex:a ex:p ex:c }").await?;
        }
        for _ in 0..3 {
            seen.push(
                select(&app, &[], SCOPED, "?p ?o", G1, pattern)
                    .await?
                    .rows(),
            );
            seen.push(select(&app, &[], "", "?p ?o", G1, pattern).await?.rows());
            expected.extend(if change {
                [
                    bindings(&[
                        "<urn:ex:p>\t<urn:ex:b>",
                        "<urn:ex:p>\t<urn:ex:c>",
                        "<urn:ex:q>\t<urn:ex:b>",
                        "<urn:ex:q>\t<urn:ex:c>",
                    ]),
                    bindings(&["<urn:ex:p>\t<urn:ex:b>", "<urn:ex:p>\t<urn:ex:c>"]),
                ]
            } else {
                [
                    bindings(&["<urn:ex:p>\t<urn:ex:b>", "<urn:ex:q>\t<urn:ex:b>"]),
                    bindings(&["<urn:ex:p>\t<urn:ex:b>"]),
                ]
            });
        }
    }
    assert_eq!(
        seen, expected,
        "scoped and plain bindings before and after the commit"
    );
    Ok(())
}

#[tokio::test]
async fn scoped_rows_and_explanations_are_not_certified() -> TestResult {
    // Spec: Dataset-scoped reads and their explanations carry the not-certified
    // completeness header.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, G1_FACT).await?;
    let pattern = "{ ?s ex:p ex:b }";
    let rows = select(&app, &[], SCOPED, "?s", G1, pattern).await?;
    let explain = select(
        &app,
        &[],
        "?inference-scope=dataset&explain=true",
        "?s",
        G1,
        pattern,
    )
    .await?;
    let plan = select(
        &app,
        &[],
        "?inference-scope=dataset&explain=plan",
        "?s",
        G1,
        pattern,
    )
    .await?;
    assert_eq!(rows.rows(), bindings(&[A]));
    for reply in [&rows, &explain, &plan] {
        assert_eq!(reply.code, StatusCode::OK, "{reply:?}");
    }
    assert_eq!(
        [
            rows.completeness.as_deref(),
            explain.completeness.as_deref(),
            plan.completeness.as_deref()
        ],
        [Some("not-certified"); 3],
        "headers on rows, explain and plan; explain status: {}, plan status: {}",
        explain.json()["completeness"]["status"],
        plan.json()["completeness"]["status"]
    );
    Ok(())
}

#[tokio::test]
async fn ordinary_datasets_keep_their_rows_and_completeness() -> TestResult {
    // Spec: Without inference-scope, plain FROM selects stored graphs and the two
    // pseudo-graphs and infer=false keep their existing rows and completeness.
    let app = app(
        StoreConfig {
            union_default_graph: true,
            ..StoreConfig::default()
        },
        false,
    )?;
    insert(&app, SPLIT).await?;
    // The variable predicate adds a reason even though VALUES restricts its bindings.
    let reasoned = concat!(
        r#"sound-only; regime=owl2-rl; reasons="rules: answers over the owl2-rl closure: "#,
        "what its rules derive, complete for the ontologies of their profile, ",
        "which isn't checked per query; not every certain answer under OWL 2 DL ",
        r#"(reasoner.mode = \"owl2-dl\" gives those); rules: the query reads a variable "#,
        "predicate: OWL 2 entails statements of the rdf:, rdfs: and owl: vocabulary ",
        r#"that no closure, rewriting or bound holds in full""#,
    );
    let mut compared = Vec::new();
    for (options, dataset, expected, completeness) in [
        ("", G1, &[P][..], reasoned),
        ("", "FROM onto:explicit", &[P][..], "complete"),
        ("", "FROM onto:implicit", &[Q][..], reasoned),
        (
            "",
            "FROM onto:explicit FROM onto:implicit",
            &[P, Q][..],
            reasoned,
        ),
        ("?infer=false", "", &[P][..], "complete"),
    ] {
        let reply = select(
            &app,
            &[],
            options,
            "?p",
            dataset,
            "{ ex:a ?p ex:b VALUES ?p { ex:p ex:q } }",
        )
        .await?;
        compared.push((
            options,
            dataset,
            reply.rows(),
            reply.completeness,
            bindings(expected),
            completeness,
        ));
    }
    assert!(
        compared
            .iter()
            .all(|(_, _, rows, status, expected, completeness)| {
                rows == expected && status.as_deref() == Some(*completeness)
            }),
        "options, dataset, rows, header, expected rows, expected header: {compared:#?}"
    );
    Ok(())
}

#[tokio::test]
async fn matches_a_supported_reader_of_the_selected_graphs() -> TestResult {
    // Spec: With the physical default empty, scoped solutions match the multiset seen
    // by a supported reader of exactly the selected graphs under a union default.
    let config = StoreConfig {
        union_default_graph: true,
        ..StoreConfig::default()
    };
    let mut compared = Vec::new();
    for (facts, cases) in [(SPLIT, SPLIT_CASES), (SUPPRESSED, SUPPRESSED_CASES)] {
        let app = app(config.clone(), true)?;
        insert(&app, facts).await?;
        for case in cases {
            let reader = if case.dataset == BOTH {
                BOTH_READER
            } else {
                READER
            };
            let reference = select(&app, reader, "", case.projection, "", case.pattern)
                .await?
                .rows();
            let observed = select(
                &app,
                ADMIN,
                SCOPED,
                case.projection,
                case.dataset,
                case.pattern,
            )
            .await?
            .rows();
            compared.push((case.name, reference, observed, bindings(case.expected)));
        }
    }
    assert_oracle(compared);
    Ok(())
}

#[tokio::test]
async fn a_dataset_cannot_widen_a_readers_access() -> TestResult {
    // Spec: Adding an unreadable graph changes neither default rows, COUNT(*) nor the
    // named graph enumeration. Only the readable graph supports default inferences.
    let app = app(
        StoreConfig {
            union_default_graph: true,
            ..StoreConfig::default()
        },
        true,
    )?;
    insert(
        &app,
        &format!("{INSIDE} GRAPH :g2 {{ ex:p rdfs:subPropertyOf ex:r . ex:a ex:s ex:b }}"),
    )
    .await?;
    let mut predicates = Vec::new();
    let mut counts = Vec::new();
    for dataset in [BOTH, G1] {
        predicates.push(
            select(
                &app,
                READER,
                SCOPED,
                "?p",
                dataset,
                "{ ex:a ?p ex:b VALUES ?p { ex:p ex:q ex:r ex:s } }",
            )
            .await?
            .rows(),
        );
        counts.push(
            select(
                &app,
                READER,
                SCOPED,
                "(COUNT(*) AS ?n)",
                dataset,
                "{ ?s ?p ?o }",
            )
            .await?
            .rows(),
        );
    }
    let reference = select(&app, READER, "", "(COUNT(*) AS ?n)", "", "{ ?s ?p ?o }")
        .await?
        .rows();
    let mut named = Vec::new();
    for dataset in [
        "FROM :g1 FROM :g2 FROM NAMED :g1 FROM NAMED :g2",
        "FROM :g1 FROM NAMED :g1",
    ] {
        named.push(
            select(
                &app,
                READER,
                SCOPED,
                "DISTINCT ?g",
                dataset,
                "{ GRAPH ?g {} }",
            )
            .await?
            .rows(),
        );
    }
    assert_eq!(
        (predicates, counts, named),
        (
            vec![bindings(&[P, Q]); 2],
            vec![reference; 2],
            vec![bindings(&["<urn:graph:g1>"]); 2],
        ),
        "rows, counts and graph enumeration with and without naming the unreadable graph"
    );
    Ok(())
}

#[tokio::test]
async fn result_cache_separates_both_read_orders() -> TestResult {
    // Spec: Starting with either request, cached plain and scoped reads of the same
    // query keep their own rows, including for a supported reader of the selected graph.
    let mut compared = Vec::new();
    for (restricted, roles) in [(false, &[][..]), (true, BOTH_READER)] {
        for scoped_first in [false, true] {
            let app = app(
                StoreConfig {
                    query_cache_bytes: 1024 * 1024,
                    ..StoreConfig::default()
                },
                restricted,
            )?;
            insert(&app, &format!("{INSIDE} GRAPH :g2 {{ ex:x ex:y ex:z }}")).await?;
            let mut seen = Vec::new();
            let mut expected = Vec::new();
            for scoped in [scoped_first, !scoped_first, scoped_first] {
                let options = if scoped { SCOPED } else { "" };
                seen.push(
                    select(&app, roles, options, "?s", BOTH, CONCLUSION)
                        .await?
                        .rows(),
                );
                expected.push(bindings(if scoped { &[A] } else { &[] }));
            }
            compared.push((restricted, scoped_first, seen, expected));
        }
    }
    assert!(
        compared
            .iter()
            .all(|(_, _, seen, expected)| seen == expected),
        "restricted reader, scoped first, observed rows, expected rows: {compared:#?}"
    );
    Ok(())
}

#[tokio::test]
async fn compact_equality_names_the_unsupported_setting() -> TestResult {
    // Spec: With compact equality, dataset-scoped inference returns 501 with a
    // problem type that identifies the equality setting.
    let app = app(
        StoreConfig {
            equality_compact: true,
            ..StoreConfig::default()
        },
        false,
    )?;
    insert(&app, G1_FACT).await?;
    let reply = select(&app, &[], SCOPED, "?s", G1, CONCLUSION).await?;
    let problem = reply.json();
    assert_eq!(reply.code, StatusCode::NOT_IMPLEMENTED, "{reply:?}");
    assert!(
        problem["type"]
            .as_str()
            .is_some_and(|kind| kind.contains("equality")),
        "{reply:?}"
    );
    Ok(())
}

#[tokio::test]
async fn scoped_reads_with_reasoning_disabled_use_the_asserted_merge() -> TestResult {
    // Spec: With reasoning off, a scoped read merges the selected assertions and reports
    // complete without a regime.
    let app = test_app_with_store_config(
        StoreConfig::default(),
        Default::default(),
        ReasonerConfig::for_mode(ReasoningMode::Disabled),
    )?;
    insert(&app, &format!("{SPLIT} ex:a ex:q ex:b .")).await?;
    let reply = select(&app, &[], SCOPED, "?p", BOTH, "{ ex:a ?p ex:b }").await?;
    assert_eq!(
        (reply.rows(), reply.completeness.as_deref()),
        (bindings(&[P]), Some("complete"))
    );
    Ok(())
}

const GEO: &str = "GRAPH :g1 {
    ex:a ex:geometry \"POINT(1 1)\"^^<http://www.opengis.net/ont/geosparql#wktLiteral> .
    ex:geometry rdfs:subPropertyOf <http://www.opengis.net/ont/geosparql#asWKT> .
    ex:box <http://www.opengis.net/ont/geosparql#asWKT>
        \"POLYGON((0 0, 2 0, 2 2, 0 2, 0 0))\"^^<http://www.opengis.net/ont/geosparql#wktLiteral>
}";

#[tokio::test]
async fn scoped_reads_refuse_reader_ranked_search() -> TestResult {
    // Spec: A scoped read that uses reader-ranked search returns 501 and identifies
    // the search_scope setting in its problem type.
    let app = app(
        StoreConfig {
            search_scope: nrese_store::SearchScope::Reader,
            ..StoreConfig::default()
        },
        false,
    )?;
    insert(&app, "GRAPH :g1 { ex:a ex:p \"alpha\" }").await?;
    let reply = select(
        &app,
        &[],
        SCOPED,
        "?text",
        G1,
        "{ ?text <http://www.bigdata.com/rdf/search#search> \"alpha\" }",
    )
    .await?;
    assert_eq!(reply.code, StatusCode::NOT_IMPLEMENTED, "{reply:?}");
    assert!(
        reply.json()["type"]
            .as_str()
            .is_some_and(|kind| kind.contains("search_scope")),
        "{reply:?}"
    );
    Ok(())
}

#[tokio::test]
async fn scoped_reads_refuse_global_text_search() -> TestResult {
    // Spec: A scoped text search in global mode returns 501 and names the search setting.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, "GRAPH :g1 { ex:a ex:p \"alpha\" }").await?;
    let reply = select(
        &app,
        &[],
        SCOPED,
        "?text",
        G1,
        "{ ?text <http://www.bigdata.com/rdf/search#search> \"alpha\" }",
    )
    .await?;
    assert_eq!(reply.code, StatusCode::NOT_IMPLEMENTED, "{reply:?}");
    assert!(
        reply.json()["type"]
            .as_str()
            .is_some_and(|kind| kind.contains("search")),
        "{reply:?}"
    );
    Ok(())
}

#[tokio::test]
async fn scoped_reads_refuse_text_search_before_shortcuts() -> TestResult {
    // Spec: A scoped text search returns 501 even if its property is absent or
    // its pattern is evaluated with a binding from an OPTIONAL's left side.
    let app = app(StoreConfig::default(), false)?;
    let facts = (0..64)
        .map(|i| format!("<urn:item:{i}> ex:p \"alpha\" ."))
        .collect::<Vec<_>>()
        .join(" ");
    insert(&app, &format!("GRAPH :g1 {{ {facts} }}")).await?;
    let mut replies = Vec::new();
    for (dataset, pattern) in [
        (
            G1,
            r#"{ ?s <http://jena.apache.org/text#query> (ex:absent "alpha") }"#,
        ),
        (
            "FROM :g1 FROM NAMED :g1",
            r#"{ GRAPH ?g { ?s <http://jena.apache.org/text#query> (ex:absent "alpha") } }"#,
        ),
        (
            G1,
            r#"{ VALUES ?s { <urn:item:0> } OPTIONAL {
                ?s ex:p ?text . ?text <http://www.bigdata.com/rdf/search#search> "alpha"
            } }"#,
        ),
    ] {
        replies.push(select(&app, &[], SCOPED, "?s", dataset, pattern).await?);
    }
    assert_eq!(
        replies.iter().map(|reply| reply.code).collect::<Vec<_>>(),
        vec![StatusCode::NOT_IMPLEMENTED; 3],
        "{replies:?}"
    );
    for reply in replies {
        assert!(
            reply.json()["type"]
                .as_str()
                .is_some_and(|kind| kind.contains("search")),
            "{reply:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn scoped_reads_refuse_global_vector_search() -> TestResult {
    // Spec: A scoped vector search in global mode returns 501 and names the search setting.
    let app = app(StoreConfig::default(), false)?;
    insert(
        &app,
        "GRAPH :g1 { ex:a ex:p \"[1,0]\"^^<urn:nrese:vector:vector> }",
    )
    .await?;
    let reply = select(
        &app,
        &[],
        SCOPED,
        "?v",
        G1,
        "{ SERVICE <urn:nrese:vector:search> {
           ?v <urn:nrese:vector:near> \"[1,0]\"^^<urn:nrese:vector:vector> ;
              <urn:nrese:vector:k> 1
         } }",
    )
    .await?;
    assert_eq!(reply.code, StatusCode::NOT_IMPLEMENTED, "{reply:?}");
    assert!(
        reply.json()["type"]
            .as_str()
            .is_some_and(|kind| kind.contains("search")),
        "{reply:?}"
    );
    Ok(())
}

#[tokio::test]
async fn scoped_reads_refuse_computed_spatial_relations() -> TestResult {
    // Spec: A scoped computed spatial relation returns 501 and identifies the
    // computed spatial relation in its problem type.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, GEO).await?;
    let reply = select(
        &app,
        &[],
        SCOPED,
        "?s",
        G1,
        "{ ?s <http://www.opengis.net/ont/geosparql#sfWithin> ex:box }",
    )
    .await?;
    assert_eq!(reply.code, StatusCode::NOT_IMPLEMENTED, "{reply:?}");
    assert!(
        reply.json()["type"]
            .as_str()
            .is_some_and(|kind| kind.contains("spatial") || kind.contains("sfWithin")),
        "{reply:?}"
    );
    Ok(())
}

#[tokio::test]
async fn scoped_spatial_filter_matches_an_independently_materialised_repository() -> TestResult {
    // Spec: A scoped spatial filter returns the same rows as evaluating the filter
    // with just the selected graphs and their independently materialised closure.
    let full = app(StoreConfig::default(), false)?;
    insert(
        &full,
        &format!(
            "{GEO} ex:outside <http://www.opengis.net/ont/geosparql#asWKT>
        \"POINT(1 1)\"^^<http://www.opengis.net/ont/geosparql#wktLiteral> ."
        ),
    )
    .await?;
    let oracle = app(
        StoreConfig {
            union_default_graph: true,
            ..StoreConfig::default()
        },
        false,
    )?;
    insert(&oracle, GEO).await?;
    let pattern = "{ ?s <http://www.opengis.net/ont/geosparql#asWKT> ?w
        FILTER(?s != ex:box)
        FILTER(<http://www.opengis.net/def/function/geosparql/sfWithin>(?w,
            \"POLYGON((0 0, 2 0, 2 2, 0 2, 0 0))\"^^<http://www.opengis.net/ont/geosparql#wktLiteral>)) }";
    let reference = select(&oracle, &[], "", "?s", "", pattern).await?;
    let observed = select(&full, &[], SCOPED, "?s", G1, pattern).await?;
    assert_eq!(
        reference.rows(),
        bindings(&[A]),
        "oracle over the selected graphs"
    );
    assert_eq!(observed.rows(), reference.rows(), "scoped spatial filter");
    Ok(())
}

fn assert_bad_request(reply: Reply, reason: &str) {
    assert_eq!(reply.code, StatusCode::BAD_REQUEST, "{reply:?}");
    assert!(
        reply.body.contains(reason),
        "expected a reason naming {reason}: {reply:?}"
    );
}

#[tokio::test]
async fn refuses_conflicting_inference_options() -> TestResult {
    // Spec: Dataset-scoped inference together with infer=false returns 400.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, G1_FACT).await?;
    let reply = select(
        &app,
        &[],
        "?inference-scope=dataset&infer=false",
        "?s",
        G1,
        CONCLUSION,
    )
    .await?;
    assert_bad_request(reply, "infer=false");
    Ok(())
}

#[tokio::test]
async fn refuses_a_pseudo_graph_in_the_dataset() -> TestResult {
    // Spec: Naming a pseudo-graph in a scoped dataset returns 400.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, G1_FACT).await?;
    let reply = select(
        &app,
        &[],
        SCOPED,
        "?s",
        "FROM :g1 FROM onto:explicit",
        CONCLUSION,
    )
    .await?;
    assert_bad_request(reply, "pseudo-graph");
    Ok(())
}

#[tokio::test]
async fn refuses_a_dataset_that_names_no_graphs() -> TestResult {
    // Spec: Scoped inference without a graph in either dataset source returns 400.
    let app = app(StoreConfig::default(), false)?;
    let reply = select(&app, &[], SCOPED, "?s", "", CONCLUSION).await?;
    assert_bad_request(reply, "graph");
    Ok(())
}

#[tokio::test]
async fn refuses_a_dataset_scope_inside_a_pending_transaction() -> TestResult {
    // Spec: A transaction QUERY with dataset-scoped inference returns 400 after it
    // stages an UPDATE.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, G1_FACT).await?;
    let begin = send(
        &app,
        &[],
        Method::POST,
        "/repositories/nrese/transactions",
        "application/x-www-form-urlencoded",
        String::new(),
    )
    .await?;
    assert_eq!(begin.status(), StatusCode::CREATED);
    let tx = begin.headers()[header::LOCATION].to_str()?.to_owned();
    let pending = send(
        &app,
        &[],
        Method::PUT,
        &format!("{tx}?action=UPDATE"),
        "application/x-www-form-urlencoded",
        serde_urlencoded::to_string([(
            "update",
            format!("{PREFIXES} INSERT DATA {{ GRAPH :g1 {{ ex:a ex:q ex:b }} }}"),
        )])?,
    )
    .await?;
    assert_eq!(
        pending.status(),
        StatusCode::OK,
        "{}",
        body_text(pending).await?
    );
    let query = serde_urlencoded::to_string([(
        "query",
        format!("{PREFIXES} SELECT ?s {G1} WHERE {CONCLUSION}"),
    )])?;
    let reply = Reply::read(
        send(
            &app,
            &[],
            Method::PUT,
            &format!("{tx}?action=QUERY&inference-scope=dataset&{query}"),
            "application/sparql-query",
            String::new(),
        )
        .await?,
    )
    .await?;
    assert_bad_request(reply, "pending");
    Ok(())
}

#[tokio::test]
async fn scoped_cache_separates_default_graph_alias_when_reasoning_is_off() -> TestResult {
    // Spec: For equal snapshot and FROM graphs, alternating plain and scoped reads
    // must retain their different visibility of the store default compatibility alias.
    for scoped_first in [false, true] {
        let app = test_app_with_store_config(
            StoreConfig {
                query_cache_bytes: 1024 * 1024,
                ..StoreConfig::default()
            },
            Default::default(),
            ReasonerConfig::for_mode(ReasoningMode::Disabled),
        )?;
        insert(&app, &format!("{SPLIT} ex:a ex:q ex:b .")).await?;
        for scoped in [scoped_first, !scoped_first, scoped_first] {
            let options = if scoped { SCOPED } else { "" };
            let reply = select(
                &app,
                &[],
                options,
                "(COUNT(*) AS ?n)",
                BOTH,
                "{ GRAPH <urn:x-arq:DefaultGraph> { ?s ex:q ex:b } }",
            )
            .await?;
            assert_eq!(
                reply.rows(),
                bindings(&[if scoped { "0" } else { "1" }]),
                "scoped first: {scoped_first}, scoped now: {scoped}"
            );
        }
    }
    Ok(())
}
