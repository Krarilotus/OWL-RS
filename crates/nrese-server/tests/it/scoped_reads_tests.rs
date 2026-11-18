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
#[ignore = "a dataset cannot yet select the premises of a derivation"]
async fn derivation_uses_only_dataset_graphs() -> TestResult {
    // Spec: Dataset-scoped inference derives a conclusion only when both premises are in
    // the dataset. FROM NAMED can supply a premise while keeping its assertion in GRAPH.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, SPLIT).await?;
    let mut seen = Vec::new();
    for dataset in [G1, BOTH, "FROM :g1 FROM NAMED :g2"] {
        seen.push(
            select(&app, &[], SCOPED, "?s", dataset, CONCLUSION)
                .await?
                .rows(),
        );
    }
    let named = select(
        &app,
        &[],
        SCOPED,
        "?g",
        "FROM :g1 FROM NAMED :g2",
        "{ GRAPH ?g { ex:p rdfs:subPropertyOf ex:q } }",
    )
    .await?;
    let default = select(
        &app,
        &[],
        SCOPED,
        "?p",
        "FROM :g1 FROM NAMED :g2",
        "{ ?p rdfs:subPropertyOf ex:q FILTER(?p = ex:p) }",
    )
    .await?;
    assert_eq!(
        (seen, named.rows(), default.rows()),
        (
            vec![bindings(&[]), bindings(&[A]), bindings(&[A])],
            bindings(&["<urn:graph:g2>"]),
            bindings(&[])
        ),
        "conclusion with one, both or a named premise, then the named assertion in GRAPH and in the default graph"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "an assertion in an unselected graph can still suppress a scoped inference"]
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
#[ignore = "the default graph scan does not yet include the dataset's own inferences"]
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
#[ignore = "a scoped read does not yet match materialisation over its dataset"]
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
#[ignore = "a warm scoped read does not yet return its locally derived rows"]
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
#[ignore = "rows and explanations do not yet carry dataset-scoped certification status"]
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
#[ignore = "dataset scoping does not yet match the supported-reader view"]
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
#[ignore = "a reader of selected graphs does not yet see its supported inferences through FROM"]
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
    for dataset in ["FROM NAMED :g1 FROM NAMED :g2", "FROM NAMED :g1"] {
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
#[ignore = "ordinary and scoped reads do not yet produce distinct answers to cache"]
async fn result_cache_separates_both_read_orders() -> TestResult {
    // Spec: Starting with either request, cached plain and scoped reads of the same
    // query keep their own rows, including for a supported reader of the selected graph.
    let mut compared = Vec::new();
    for (restricted, roles) in [(false, &[][..]), (true, READER)] {
        for scoped_first in [false, true] {
            let app = app(
                StoreConfig {
                    query_cache_bytes: 1024 * 1024,
                    ..StoreConfig::default()
                },
                restricted,
            )?;
            insert(&app, INSIDE).await?;
            let mut seen = Vec::new();
            let mut expected = Vec::new();
            for scoped in [scoped_first, !scoped_first, scoped_first] {
                let options = if scoped { SCOPED } else { "" };
                seen.push(
                    select(&app, roles, options, "?s", G1, CONCLUSION)
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
#[ignore = "compact equality does not yet refuse dataset-scoped inference"]
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

fn assert_bad_request(reply: Reply, reason: &str) {
    assert_eq!(reply.code, StatusCode::BAD_REQUEST, "{reply:?}");
    assert!(
        reply.body.contains(reason),
        "expected a reason naming {reason}: {reply:?}"
    );
}

#[tokio::test]
#[ignore = "a dataset-scoped read does not yet reject infer=false"]
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
#[ignore = "a dataset-scoped read does not yet reject a pseudo-graph"]
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
#[ignore = "a dataset-scoped read does not yet require a named input graph"]
async fn refuses_a_dataset_that_names_no_graphs() -> TestResult {
    // Spec: Scoped inference without a graph in either dataset source returns 400.
    let app = app(StoreConfig::default(), false)?;
    let reply = select(&app, &[], SCOPED, "?s", "", CONCLUSION).await?;
    assert_bad_request(reply, "graph");
    Ok(())
}

#[tokio::test]
#[ignore = "a transaction with pending operations does not yet reject a dataset-scoped read"]
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
