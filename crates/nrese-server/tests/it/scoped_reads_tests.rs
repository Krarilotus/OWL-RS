//! HTTP acceptance cases for inference scoped to a query's dataset.

use std::error::Error;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use axum::response::Response;
use nrese_reasoner::{ReasonerConfig, ReasoningMode};
use nrese_server::build_app;
use nrese_store::StoreConfig;
use serde_json::Value;
use tower::util::ServiceExt;

use crate::access_control_tests::{policy_of, token};
use crate::support::{body_text, test_app_with_store_config, test_state_with_store_config};

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
async fn review_unscoped_search_and_scan_keep_separate_cached_answers() -> TestResult {
    // Spec: A cached ordinary list scan cannot answer a later text search
    // that uses the same projected variable and a blank argument list.
    let app = app(
        StoreConfig {
            query_cache_bytes: 1024 * 1024,
            ..StoreConfig::default()
        },
        false,
    )?;
    insert(
        &app,
        r#"ex:a <http://jena.apache.org/text#query> ex:args .
           ex:args <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> "alpha" ;
                   <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest>
                   <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> .
           ex:b ex:q "alpha" ."#,
    )
    .await?;
    for (argument, expected) in [
        ("?l", bindings(&[A])),
        ("_:l", bindings(&["<urn:ex:args>", "<urn:ex:b>"])),
    ] {
        let pattern = format!(
            r#"{{ ?s <http://jena.apache.org/text#query> {argument} .
                {argument} <http://www.w3.org/1999/02/22-rdf-syntax-ns#first> "alpha" ;
                           <http://www.w3.org/1999/02/22-rdf-syntax-ns#rest>
                           <http://www.w3.org/1999/02/22-rdf-syntax-ns#nil> }}"#
        );
        let answer = select(&app, &[], "", "?s", "", &pattern).await?;
        assert_eq!(answer.rows(), expected, "list argument {argument}");
    }
    Ok(())
}

#[tokio::test]
async fn review_unscoped_path_keeps_the_mixed_text_group_a_scan() -> TestResult {
    // Spec: Ordering a property path beside a mixed text group preserves
    // the ordinary scan's result. A literal in another property gives no match.
    use std::fmt::Write as _;

    let app = app(StoreConfig::default(), false)?;
    let mut data = r#"ex:a <http://jena.apache.org/text#query> "alpha" ;
                           ex:p ex:target .
                      ex:z ex:q "alpha" ; ex:p ex:target . "#
        .to_owned();
    // Increase matches of the unconstrained text predicate at one subject.
    for value in 0..128 {
        write!(data, "ex:z <http://jena.apache.org/text#query> {value} . ")?;
    }
    insert(&app, &data).await?;
    let pattern = r#"{ ?s <http://jena.apache.org/text#query> [] .
                       ?s <http://jena.apache.org/text#query> "alpha" .
                       ?s ex:p+ ?o }"#;
    let answer = select(&app, &[], "", "?s", "", pattern).await?;
    assert_eq!(answer.rows(), bindings(&[A]), "path join");
    Ok(())
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
async fn scoped_default_omits_assertions_in_unselected_graphs() -> TestResult {
    // Spec: An unrestricted reader's scoped default graph contains assertions from
    // the selected graphs only.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, &format!("{SPLIT} GRAPH :g3 {{ ex:z ex:p ex:b }}")).await?;
    let reply = select(&app, &[], SCOPED, "?s", G1, "{ ?s ex:p ex:b }").await?;
    assert_eq!(reply.rows(), bindings(&[A]));
    Ok(())
}

#[tokio::test]
async fn protocol_dataset_overrides_from_for_scoped_inference() -> TestResult {
    // Spec: Under dataset scoping, protocol graph parameters replace FROM both
    // for premises and for the default scan.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, SPLIT).await?;
    let from = select(&app, &[], SCOPED, "?s", BOTH, CONCLUSION).await?;
    let protocol = serde_urlencoded::to_string([
        ("inference-scope", "dataset"),
        ("default-graph-uri", "urn:graph:g1"),
    ])?;
    let overridden = select(&app, &[], &format!("?{protocol}"), "?s", BOTH, CONCLUSION).await?;
    assert_eq!(
        (from.rows(), overridden.rows()),
        (bindings(&[A]), bindings(&[]))
    );
    Ok(())
}

#[tokio::test]
async fn rdf4j_get_applies_dataset_scoped_inference() -> TestResult {
    // Spec: RDF4J repository GET takes the option, returns the scoped conclusion
    // and reports it as not-certified.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, SPLIT).await?;
    let query = format!("{PREFIXES} SELECT ?s {BOTH} WHERE {CONCLUSION}");
    let parameters =
        serde_urlencoded::to_string([("query", query.as_str()), ("inference-scope", "dataset")])?;
    let reply = Reply::read(
        send(
            &app,
            &[],
            Method::GET,
            &format!("/repositories/nrese?{parameters}"),
            "",
            String::new(),
        )
        .await?,
    )
    .await?;
    assert_eq!(
        (reply.rows(), reply.completeness.as_deref()),
        (bindings(&[A]), Some("not-certified"))
    );
    Ok(())
}

#[tokio::test]
async fn scoped_support_past_the_cap_matches_the_independent_repository() -> TestResult {
    // Spec: When the support-set cap omits the selected derivation, the scoped
    // result still matches a repository holding only the selected graphs.
    let config = StoreConfig {
        support_sets: 1,
        union_default_graph: true,
        ..StoreConfig::default()
    };
    let state = test_state_with_store_config(
        config.clone(),
        Default::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;
    let store = state.store();
    let full = build_app(state);
    let oracle = app(config, false)?;
    // The smaller graph name comes first when the cap keeps just one of the two
    // equally sized supports. Both derivations use the axiom in :g2.
    insert(&full, &format!("GRAPH :g0 {{ ex:a ex:p ex:b }} {SPLIT}")).await?;
    insert(&oracle, SPLIT).await?;
    let before = store.support_statistics().closures;
    let scoped = select(&full, &[], SCOPED, "?s", BOTH, CONCLUSION).await?;
    let reference = select(&oracle, &[], "", "?s", "", CONCLUSION).await?;
    assert_eq!(
        (scoped.rows(), reference.rows()),
        (bindings(&[A]), bindings(&[A]))
    );
    assert!(
        store.support_statistics().closures > before,
        "the capped fixture must exercise the closure fallback"
    );
    Ok(())
}

#[tokio::test]
async fn scoped_support_counts_same_as_premises() -> TestResult {
    // Spec: Expanded equality can substitute a term when its sameAs premise is
    // in the effective dataset.
    let app = app(StoreConfig::default(), false)?;
    insert(
        &app,
        &format!("{G1_FACT} GRAPH :g2 {{ ex:a <http://www.w3.org/2002/07/owl#sameAs> ex:z }}"),
    )
    .await?;
    let pattern = "{ ?s ex:p ex:b }";
    let one = select(&app, &[], SCOPED, "?s", G1, pattern).await?;
    let both = select(&app, &[], SCOPED, "?s", BOTH, pattern).await?;
    assert_eq!(
        (one.rows(), both.rows()),
        (bindings(&[A]), bindings(&[A, "<urn:ex:z>"]))
    );
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
    // Spec: docs/spec/reasoning-semantics.md:86 and :141. A scoped view carries no
    // whole-store regime, bounds, or reasons across any representation.
    for reply in [&explain, &plan] {
        let body = reply.json();
        let claim = &body["completeness"];
        assert_eq!(claim["status"], "not-certified");
        assert_eq!(claim["complete"], false);
        assert_eq!(claim["sound"], false);
        assert_eq!(claim["reasons"], serde_json::json!([]));
        assert!(claim["regime"].is_null() && claim["bounds"].is_null() && body["ql"].is_null());
    }
    Ok(())
}

#[tokio::test]
async fn a_scoped_read_of_a_withheld_view_with_no_inferred_statement_is_not_certified() -> TestResult
{
    // Spec: Dataset-scoped reads and their explanations carry the not-certified completeness
    // header, also where the reader's view holds no inferred statement: `:g1` asserts the
    // owl2-rl closure of `ex:a ex:p ex:b` (taken from a store holding that fact alone), and
    // an unrelated chain axiom on a list with no `rdf:rest` in `:g2`, which the reader by
    // support of `:g1` can't read, makes the support grounding leave a list out.
    let source = app(StoreConfig::default(), false)?;
    insert(&source, G1_FACT).await?;
    let closure = select(&source, &[], "", "?s ?p ?o", "", "{ ?s ?p ?o }")
        .await?
        .rows()
        .map_err(|refused| format!("{refused:?}"))?;
    let closure: String = closure
        .iter()
        .map(|row| format!("{} . ", row.replace('\t', " ")))
        .collect();
    let app = app(StoreConfig::default(), true)?;
    let first = "<http://www.w3.org/1999/02/22-rdf-syntax-ns#first>";
    let chain = "<http://www.w3.org/2002/07/owl#propertyChainAxiom>";
    insert(
        &app,
        &format!(
            "GRAPH :g1 {{ ex:a ex:p ex:b . {closure} }} GRAPH :g2 {{ ex:q {chain} _:l . _:l {first} ex:r }}"
        ),
    )
    .await?;
    let pattern = "{ ?s ex:p ex:b }";
    let mut told = Vec::new();
    for options in ["", "&explain=true", "&explain=plan"] {
        let options = format!("{SCOPED}{options}");
        let reply = select(&app, READER, &options, "?s", G1, pattern).await?;
        assert_eq!(reply.code, StatusCode::OK, "{reply:?}");
        told.push(format!("{options}: {:?}", reply.completeness));
    }
    let want = ["", "&explain=true", "&explain=plan"]
        .map(|options| format!("{SCOPED}{options}: Some(\"not-certified\")"));
    assert_eq!(told, want, "the closure asserted in :g1: {closure}");
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
    let omitted = concat!(
        r#"sound-only; regime=owl2-rl; reasons="rules: this read's dataset leaves out the "#,
        "store's default graph from its default graph, where inferred statements are stored; ",
        "default-graph patterns see only the selected graphs' asserted statements; rules: the ",
        "query reads a variable predicate: OWL 2 entails statements of the rdf:, rdfs: and ",
        r#"owl: vocabulary that no closure, rewriting or bound holds in full""#,
    );
    let mut compared = Vec::new();
    for (options, dataset, expected, completeness) in [
        ("", G1, &[P][..], omitted),
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
async fn from_dataset_omitting_default_graph_explains_omitted_inferences() -> TestResult {
    // Spec: A reasoned read whose dataset (FROM, FROM NAMED, or protocol default-graph-uri)
    // omits the store's default graph keeps sound-only status and explains that inferred
    // statements live in the omitted default graph, rather than repeating the default
    // closure reason. Including the default graph keeps the closure reason.
    let app = app(StoreConfig::default(), false)?;
    insert(
        &app,
        "GRAPH :g1 { ex:Cat rdfs:subClassOf ex:Animal }
         GRAPH :g2 { ex:tom a ex:Cat . ex:kit a ex:Cat }",
    )
    .await?;

    let expected_materialised = "this read's dataset leaves out the store's default graph from \
                                 its default graph, where inferred statements are stored; \
                                 default-graph patterns see only the selected graphs' asserted \
                                 statements";
    let expected_inferred = "this read's dataset leaves out the store's default graph from its \
                             default graph, where inferred statements are stored; default-graph \
                             patterns read no inferred statements";
    let pattern = "{ ?x a ex:Animal }";

    // No FROM: reads the materialised default graph (2 inferred rows, closure reason).
    let default_reply = select(&app, &[], "", "?x", "", pattern).await?;
    assert_eq!(default_reply.code, StatusCode::OK);
    assert_eq!(default_reply.rows().unwrap().len(), 2);
    let default_header = default_reply.completeness.as_deref().unwrap_or("");
    assert!(
        default_header.starts_with("sound-only; regime=owl2-rl;")
            && default_header.contains("answers over the owl2-rl closure"),
        "default read header: {default_header}"
    );

    // FROM :g2: omits default graph (0 rows, dataset-omission reason, not closure reason).
    let from_g2 = select(&app, &[], "", "?x", "FROM :g2", pattern).await?;
    assert_eq!(from_g2.code, StatusCode::OK);
    assert!(from_g2.rows().unwrap().is_empty());
    let from_g2_header = from_g2.completeness.as_deref().unwrap_or("");
    assert!(
        from_g2_header.starts_with("sound-only; regime=owl2-rl;")
            && from_g2_header.contains(expected_materialised)
            && !from_g2_header.contains("answers over the owl2-rl closure"),
        "FROM :g2 header: {from_g2_header}"
    );

    // FROM :g2 with explicit GRAPH <urn:x-arq:DefaultGraph>: still returns inferred rows via
    // the compatibility alias, and the header scopes its asserted-only statement to
    // default-graph patterns rather than claiming the whole answer is asserted-only.
    let alias_graph = select(
        &app,
        &[],
        "",
        "?x",
        "FROM :g2",
        "{ GRAPH <urn:x-arq:DefaultGraph> { ?x a ex:Animal } }",
    )
    .await?;
    assert_eq!(alias_graph.code, StatusCode::OK);
    assert_eq!(alias_graph.rows().unwrap().len(), 2);
    let alias_header = alias_graph.completeness.as_deref().unwrap_or("");
    assert!(
        alias_header.starts_with("sound-only; regime=owl2-rl;")
            && alias_header.contains(expected_materialised)
            && !alias_header.contains("answers over its asserted statements"),
        "FROM :g2 + GRAPH default alias header: {alias_header}"
    );

    // FROM NAMED :g2 (empty default graph): omits default graph too.
    let from_named = select(&app, &[], "", "?x", "FROM NAMED :g2", pattern).await?;
    assert_eq!(from_named.code, StatusCode::OK);
    assert!(from_named.rows().unwrap().is_empty());
    let from_named_header = from_named.completeness.as_deref().unwrap_or("");
    assert!(
        from_named_header.starts_with("sound-only; regime=owl2-rl;")
            && from_named_header.contains(expected_materialised)
            && !from_named_header.contains("answers over the owl2-rl closure"),
        "FROM NAMED :g2 header: {from_named_header}"
    );

    // Protocol default-graph-uri=urn:graph:g2: omits default graph too.
    let proto_from = select(
        &app,
        &[],
        "?default-graph-uri=urn:graph:g2",
        "?x",
        "",
        pattern,
    )
    .await?;
    assert_eq!(proto_from.code, StatusCode::OK);
    assert!(proto_from.rows().unwrap().is_empty());
    let proto_header = proto_from.completeness.as_deref().unwrap_or("");
    assert!(
        proto_header.starts_with("sound-only; regime=owl2-rl;")
            && proto_header.contains(expected_materialised)
            && !proto_header.contains("answers over the owl2-rl closure"),
        "protocol default-graph-uri header: {proto_header}"
    );

    // Explicitly including the default graph keeps the closure reason and inferred rows.
    let with_default = select(
        &app,
        &[],
        "",
        "?x",
        "FROM :g2 FROM <urn:x-arq:DefaultGraph>",
        pattern,
    )
    .await?;
    assert_eq!(with_default.code, StatusCode::OK);
    assert_eq!(with_default.rows().unwrap().len(), 2);
    let with_default_header = with_default.completeness.as_deref().unwrap_or("");
    assert!(
        with_default_header.starts_with("sound-only; regime=owl2-rl;")
            && with_default_header.contains("answers over the owl2-rl closure")
            && !with_default_header.contains(expected_materialised),
        "FROM :g2 + default graph header: {with_default_header}"
    );

    // Under owl2-dl: both ordinary materialised FROM :g2 (DL bounds route) and inferred-only
    // FROM :g2 FROM onto:implicit (status_without_dl route) explain the omitted default graph
    // accurately for their read model; including the default graph keeps the generic dataset
    // reason on the DL route.
    let dl_app = test_app_with_store_config(
        StoreConfig::default(),
        Default::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Dl),
    )?;
    insert(
        &dl_app,
        "GRAPH :g1 { ex:Cat rdfs:subClassOf ex:Animal }
         GRAPH :g2 { ex:tom a ex:Cat }",
    )
    .await?;
    let dl_materialised_from_g2 = select(&dl_app, &[], "", "?x", "FROM :g2", pattern).await?;
    assert_eq!(dl_materialised_from_g2.code, StatusCode::OK);
    assert!(dl_materialised_from_g2.rows().unwrap().is_empty());
    let dl_mat_header = dl_materialised_from_g2
        .completeness
        .as_deref()
        .unwrap_or("");
    assert!(
        dl_mat_header.starts_with("sound-only; regime=owl2-dl;")
            && dl_mat_header.contains(expected_materialised)
            && !dl_mat_header.contains("the query names its dataset")
            && !dl_mat_header.contains("answers over the RL closure"),
        "owl2-dl materialised FROM :g2 header: {dl_mat_header}"
    );

    let dl_with_default = select(
        &dl_app,
        &[],
        "",
        "?x",
        "FROM :g2 FROM <urn:x-arq:DefaultGraph>",
        pattern,
    )
    .await?;
    assert_eq!(dl_with_default.code, StatusCode::OK);
    assert_eq!(dl_with_default.rows().unwrap().len(), 1);
    let dl_default_header = dl_with_default.completeness.as_deref().unwrap_or("");
    assert!(
        dl_default_header.starts_with("sound-only; regime=owl2-dl;")
            && dl_default_header.contains("the query names its dataset")
            && !dl_default_header.contains(expected_materialised),
        "owl2-dl FROM :g2 + default graph header: {dl_default_header}"
    );

    let dl_inferred_from_g2 = select(
        &dl_app,
        &[],
        "",
        "?x",
        "FROM :g2 FROM onto:implicit",
        "{ ?x a ex:Cat }",
    )
    .await?;
    assert_eq!(dl_inferred_from_g2.code, StatusCode::OK);
    assert!(dl_inferred_from_g2.rows().unwrap().is_empty());
    let dl_inf_header = dl_inferred_from_g2.completeness.as_deref().unwrap_or("");
    assert!(
        dl_inf_header.starts_with("sound-only; regime=owl2-dl;")
            && dl_inf_header.contains(expected_inferred)
            && !dl_inf_header.contains("asserted statements")
            && !dl_inf_header.contains("a read of the rules' closure alone"),
        "owl2-dl inferred FROM :g2 header: {dl_inf_header}"
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
async fn scoped_view_identity_keeps_cached_reads_separate_in_both_orders() -> TestResult {
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
async fn hidden_reader_gets_scoped_assertions_under_compact_equality() -> TestResult {
    // Spec: A reader whose policy hides inferences gets the scoped asserted merge
    // even when compact equality would prevent a scoped inference view.
    let app = test_app_with_store_config(
        StoreConfig {
            equality_compact: true,
            ..StoreConfig::default()
        },
        policy_of(&POLICY.replace("inferred = \"supported\"", "inferred = \"hidden\""))?,
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;
    insert(&app, INSIDE).await?;
    let reply = select(
        &app,
        READER,
        SCOPED,
        "?p",
        G1,
        "{ ex:a ?p ex:b VALUES ?p { ex:p ex:q } }",
    )
    .await?;
    assert_eq!(reply.rows(), bindings(&[P]));
    // Spec: docs/spec/reasoning-semantics.md:86 and :141. The authorised asserted-only
    // merge carries an unqualified complete header even when asked for scoped inference.
    assert_eq!(reply.completeness.as_deref(), Some("complete"), "{reply:?}");
    Ok(())
}

#[tokio::test]
async fn unnamed_class_skipping_refuses_dataset_scoped_inference() -> TestResult {
    // Spec: With reasoner.unnamed_classes set to "skip", a scoped read returns
    // 501 with a problem type naming that setting.
    let app = app(
        StoreConfig {
            hide_unnamed_classes: true,
            ..StoreConfig::default()
        },
        false,
    )?;
    insert(&app, INSIDE).await?;
    let reply = select(&app, &[], SCOPED, "?s", G1, CONCLUSION).await?;
    assert_eq!(reply.code, StatusCode::NOT_IMPLEMENTED, "{reply:?}");
    assert!(
        reply.json()["type"]
            .as_str()
            .is_some_and(|kind| kind.ends_with("/reasoner.unnamed_classes")),
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
async fn scoped_reads_refuse_text_search_in_either_ranking_mode() -> TestResult {
    // Spec: A scoped text search returns 501 and names the search setting in both
    // ranking modes.
    for search_scope in [
        nrese_store::SearchScope::Global,
        nrese_store::SearchScope::Reader,
    ] {
        let app = app(
            StoreConfig {
                search_scope,
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
        assert_eq!(
            reply.code,
            StatusCode::NOT_IMPLEMENTED,
            "{search_scope:?}: {reply:?}"
        );
        assert!(
            reply.json()["type"]
                .as_str()
                .is_some_and(|kind| kind.contains("search_scope")),
            "{search_scope:?}: {reply:?}"
        );
    }
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
async fn scoped_preparation_refuses_search_in_an_empty_optional() -> TestResult {
    // Spec: An empty OPTIONAL seed does not exempt its local text search from 501.
    scoped_preparation_refusal(
        SCOPED,
        r#"{ VALUES ?s {} OPTIONAL {
            ?s <http://www.bigdata.com/rdf/search#search> "alpha"
        } }"#,
        "store.search_scope",
    )
    .await
}

#[tokio::test]
async fn scoped_preparation_refuses_search_in_an_unselected_graph() -> TestResult {
    // Spec: A local vector search returns 501 even when its GRAPH IRI is
    // absent from FROM NAMED.
    scoped_preparation_refusal(
        SCOPED,
        r#"{ GRAPH :g2 { SERVICE <urn:nrese:vector:search> {
             ?s <urn:nrese:vector:near> "[1,0]"^^<urn:nrese:vector:vector>
         } } }"#,
        "store.search_scope",
    )
    .await
}

#[tokio::test]
async fn scoped_preparation_refuses_computed_spatial_over_no_named_graphs() -> TestResult {
    // Spec: A computed spatial relation returns 501 even when GRAPH ?g has
    // no named graphs to visit.
    scoped_preparation_refusal(
        SCOPED,
        "{ GRAPH ?g { ?s <http://www.opengis.net/ont/geosparql#sfWithin> ex:box } }",
        "computed-spatial-relation",
    )
    .await
}

#[tokio::test]
async fn scoped_preparation_refuses_search_in_an_empty_minus_operand() -> TestResult {
    // Spec: MINUS with empty operands still refuses a local text search in its
    // right operand, even though BIND has no rows on which to run it.
    scoped_preparation_refusal(
        SCOPED,
        r#"{ VALUES ?s {} MINUS { VALUES ?s {} BIND(EXISTS {
            ?s <http://www.bigdata.com/rdf/search#search> "alpha"
        } AS ?found) } }"#,
        "store.search_scope",
    )
    .await
}

#[tokio::test]
async fn scoped_preparation_refuses_search_in_exists_over_empty_input() -> TestResult {
    // Spec: A correlated EXISTS filter with no incoming rows still refuses its
    // local text search.
    scoped_preparation_refusal(
        SCOPED,
        r#"{ VALUES ?outer {} FILTER EXISTS {
            ?s <http://www.bigdata.com/rdf/search#search> "alpha"
            FILTER (?s = ?outer)
        } }"#,
        "store.search_scope",
    )
    .await
}

#[tokio::test]
async fn scoped_preparation_refuses_search_in_a_subquery_joined_to_empty_input() -> TestResult {
    // Spec: A subquery's local text search returns 501 even when the other join
    // operand has no rows.
    scoped_preparation_refusal(
        SCOPED,
        r#"{ VALUES ?s {} { SELECT ?s WHERE {
            ?s <http://www.bigdata.com/rdf/search#search> "alpha"
        } } }"#,
        "store.search_scope",
    )
    .await
}

#[tokio::test]
async fn scoped_preparation_refuses_search_in_a_plan_request() -> TestResult {
    // Spec: A plan request refuses a local search before evaluating it.
    scoped_preparation_refusal(
        "?inference-scope=dataset&explain=plan",
        r#"{ ?s <http://www.bigdata.com/rdf/search#search> "alpha" }"#,
        "store.search_scope",
    )
    .await
}

#[tokio::test]
async fn scoped_preparation_preserves_an_ordinary_text_predicate() -> TestResult {
    // Spec: A Jena-shaped group with a bare blank argument uses ordinary
    // triple scans, including when another triple has a valid query argument.
    let app = app(StoreConfig::default(), false)?;
    insert(
        &app,
        r#"GRAPH :g1 { ex:a <http://jena.apache.org/text#query> "alpha" }"#,
    )
    .await?;
    for pattern in [
        "{ ?s <http://jena.apache.org/text#query> [] }",
        r#"{ ?s <http://jena.apache.org/text#query> [] .
             ?s <http://jena.apache.org/text#query> "alpha" }"#,
    ] {
        let reply = select(&app, &[], SCOPED, "?s", G1, pattern).await?;
        assert_eq!(reply.rows(), bindings(&[A]), "{pattern}");
    }
    Ok(())
}

#[tokio::test]
async fn review_scoped_joined_text_groups_keep_the_scan_rows() -> TestResult {
    // Spec: When the parser joins two Jena-shaped groups into an ordinary
    // BGP, a scoped request returns the same scan rows as an unscoped read.
    // Kept apart by a one-row VALUES, the first group is a search as written,
    // which a scoped request refuses as it refuses every text search form.
    let app = app(StoreConfig::default(), false)?;
    insert(
        &app,
        r#"GRAPH :g1 { ex:a <http://jena.apache.org/text#query> "alpha" }"#,
    )
    .await?;
    let pattern = |between: &str| {
        format!(
            r#"{{ {{ ?s <http://jena.apache.org/text#query> "alpha" }}
                   {between}
                   {{ ?t <http://jena.apache.org/text#query> [] }} }}"#
        )
    };
    let joined = pattern("");
    let plain = select(&app, &[], "", "?s", G1, &joined).await?.rows();
    assert_eq!(plain, bindings(&[A]), "unscoped scan {joined}");
    let scoped = select(&app, &[], SCOPED, "?s", G1, &joined).await?;
    assert_eq!(scoped.rows(), plain, "scoped scan {joined}");
    let apart = pattern("VALUES ?unused { 1 }");
    let plain = select(&app, &[], "", "?s", G1, &apart).await?.rows();
    assert_eq!(plain, bindings(&[A]), "unscoped search {apart}");
    let scoped = select(&app, &[], SCOPED, "?s", G1, &apart).await?;
    assert_eq!(scoped.code, StatusCode::NOT_IMPLEMENTED, "{scoped:?}");
    assert_eq!(
        scoped.json()["type"],
        "https://nrese.dev/problems/store.search_scope",
        "{scoped:?}"
    );
    Ok(())
}

#[tokio::test]
async fn review_scoped_graphdb_text_operator_is_refused() -> TestResult {
    // Spec: Dataset-scoped GraphDB text search returns 501 and names the
    // same problem type as other text search forms.
    scoped_preparation_refusal(
        SCOPED,
        r#"{ ?s <http://www.ontotext.com/owlim/lucene#anyIndex> "alpha" }"#,
        "store.search_scope",
    )
    .await
}

async fn scoped_preparation_refusal(options: &str, pattern: &str, problem: &str) -> TestResult {
    let app = app(StoreConfig::default(), false)?;
    insert(&app, SPLIT).await?;
    let reply = select(&app, &[], options, "?s", G1, pattern).await?;
    assert_eq!(reply.code, StatusCode::NOT_IMPLEMENTED, "{reply:?}");
    assert_eq!(
        reply.json()["type"],
        format!("https://nrese.dev/problems/{problem}"),
        "{reply:?}"
    );
    Ok(())
}

#[tokio::test]
async fn scoped_preparation_leaves_remote_service_bodies_to_the_endpoint() -> TestResult {
    // Spec: A scoped plan leaves the remote SERVICE body to its endpoint, even
    // when the body contains search or spatial algebra.
    let app = app(StoreConfig::default(), false)?;
    insert(&app, G1_FACT).await?;
    let reply = select(
        &app,
        &[],
        "?inference-scope=dataset&explain=plan",
        "?s",
        G1,
        r#"{ SERVICE <urn:ex:remote> {
            ?s <http://www.bigdata.com/rdf/search#search> "alpha" .
            ?s <http://www.opengis.net/ont/geosparql#sfWithin> ex:box .
            SERVICE <urn:nrese:vector:search> {
                ?v <urn:nrese:vector:near> "[1,0]"^^<urn:nrese:vector:vector>
            }
        } }"#,
    )
    .await?;
    assert_eq!(reply.code, StatusCode::OK, "{reply:?}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn scoped_preparation_leaves_self_service_to_the_http_endpoint() -> TestResult {
    // Spec: An allowed SERVICE to this server's own HTTP endpoint delegates
    // search in its body. The outbound query has its endpoint's dataset.
    use nrese_server::federation::HttpServiceClient;
    use nrese_store::FederationConfig;
    use std::sync::Arc;

    let state = test_state_with_store_config(
        StoreConfig::default(),
        Default::default(),
        ReasonerConfig::for_mode(ReasoningMode::Owl2Rl),
    )?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/dataset/query", listener.local_addr()?);
    state
        .store()
        .set_service_client(Arc::new(HttpServiceClient::new(
            FederationConfig {
                allow: vec![endpoint.clone()],
                ..FederationConfig::default()
            },
            tokio::runtime::Handle::current(),
        )?));
    let app = build_app(state);
    insert(&app, &format!("{G1_FACT} ex:c ex:p \"alpha\" .")).await?;
    let server = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let pattern = format!(
        "{{ SERVICE <{endpoint}> {{ ?text <http://www.bigdata.com/rdf/search#search> \"alpha\" }} }}"
    );
    let result = select(&app, &[], SCOPED, "?text", G1, &pattern).await;
    server.abort();
    let reply = result?;
    assert_eq!(reply.rows(), bindings(&["\"alpha\""]));
    Ok(())
}

#[tokio::test]
async fn scoped_preparation_refuses_a_spatial_relation_after_lateral_binding() -> TestResult {
    // Spec: A spatial relation specialised by LATERAL still returns the same 501.
    scoped_preparation_refusal(
        SCOPED,
        "{ VALUES ?p { <http://www.opengis.net/ont/geosparql#sfWithin> }
           LATERAL { ?s ?p ex:box } }",
        "computed-spatial-relation",
    )
    .await
}

#[tokio::test]
async fn scoped_preparation_refuses_search_after_exists_binding() -> TestResult {
    // Spec: Search specialised by correlated EXISTS still returns the same 501.
    scoped_preparation_refusal(
        SCOPED,
        r#"{ VALUES ?p { <http://www.bigdata.com/rdf/search#search> }
            FILTER EXISTS { SELECT ?p ?s WHERE { ?s ?p "alpha" } LIMIT 1 } }"#,
        "store.search_scope",
    )
    .await
}

#[tokio::test]
async fn scoped_preparation_refuses_search_before_explain_analyze() -> TestResult {
    // Spec: EXPLAIN ANALYZE refuses local search even behind an empty OPTIONAL.
    scoped_preparation_refusal(
        "?inference-scope=dataset&explain=true",
        r#"{ VALUES ?s {} OPTIONAL {
            ?s <http://www.bigdata.com/rdf/search#search> "alpha"
        } }"#,
        "store.search_scope",
    )
    .await
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
