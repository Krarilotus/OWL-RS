//! Draft checks over HTTP (`dmw-store-check/1`): the capabilities, an answered check that
//! echoes the request's identity, and the requests the server refuses.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use serde_json::{Value, json};
use tower::util::ServiceExt;

use crate::support::{body_text, test_app};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DATA: &str = "<https://example.test/alice> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <https://example.test/Person> .\n";
const SHAPES: &str = "<https://example.test/S> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/ns/shacl#NodeShape> .\n\
<https://example.test/S> <http://www.w3.org/ns/shacl#targetClass> <https://example.test/Person> .\n\
<https://example.test/S> <http://www.w3.org/ns/shacl#property> _:p .\n\
_:p <http://www.w3.org/ns/shacl#path> <https://example.test/name> .\n\
_:p <http://www.w3.org/ns/shacl#minCount> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n";

async fn call(
    app: &axum::Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    let request = Request::builder()
        .uri(uri)
        .method(method)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))?;
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    Ok((status, body_text(response).await?))
}

async fn capabilities(app: &axum::Router) -> Result<Value, Box<dyn std::error::Error>> {
    let (status, text) = call(app, Method::GET, "/api/v1/draft-check/capabilities", None).await?;
    assert_eq!(status, StatusCode::OK, "{text}");
    Ok(serde_json::from_str(&text)?)
}

fn request(capabilities: &Value) -> Value {
    json!({
        "protocol": "dmw-store-check/1",
        "operation": "shacl",
        "request_sha256": "a".repeat(64),
        "scope": {"project_id": "project", "owner_id": "owner"},
        "build_id": capabilities["build_id"],
        "semantics_id": capabilities["semantics"][0],
        "profile": "owl2-rl",
        "input_hashes": {"validation": "b".repeat(64)},
        "limits": {"max_seconds": 5.0, "max_results": 10, "max_triples": 100},
        "inputs": {"data": DATA, "schema": "", "shapes": SHAPES}
    })
}

async fn check(app: &axum::Router, body: Value) -> Result<Value, Box<dyn std::error::Error>> {
    let (status, text) = call(app, Method::POST, "/api/v1/draft-check", Some(body)).await?;
    assert_eq!(status, StatusCode::OK, "{text}");
    Ok(serde_json::from_str(&text)?)
}

#[tokio::test]
async fn the_capabilities_name_the_protocol_operations_and_build() -> TestResult {
    let app = test_app()?;
    let declared = capabilities(&app).await?;
    assert_eq!(declared["protocol"], "dmw-store-check/1");
    assert_eq!(
        declared["operations"],
        json!(["shacl", "query", "reasoning"])
    );
    assert!(
        declared["profiles"]
            .as_array()
            .is_some_and(|profiles| !profiles.is_empty())
    );
    assert!(
        declared["build_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("nrese-server "))
    );
    assert!(
        declared["max_seconds"]
            .as_f64()
            .is_some_and(|seconds| seconds > 0.0)
    );
    Ok(())
}

#[tokio::test]
async fn an_answered_check_echoes_the_request_identity() -> TestResult {
    let app = test_app()?;
    let declared = capabilities(&app).await?;
    let asked = request(&declared);
    let reply = check(&app, asked.clone()).await?;
    for field in [
        "operation",
        "request_sha256",
        "scope",
        "build_id",
        "semantics_id",
        "profile",
        "input_hashes",
    ] {
        assert_eq!(reply[field], asked[field], "{field}");
    }
    assert_eq!(reply["effective_limits"], asked["limits"]);
    assert_eq!(reply["terminal_status"], "completed", "{reply}");
    assert_eq!(reply["complete"], true);
    assert_eq!(reply["shacl_conforms"], false);
    assert_eq!(reply["shacl_applicable"], true);
    assert_eq!(reply["shacl_meta_validated"], true);
    // Exactly the reply fields the protocol defines: nothing more.
    let fields: std::collections::BTreeSet<&str> = reply
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        fields,
        [
            "build_id",
            "complete",
            "detail",
            "effective_limits",
            "input_hashes",
            "logical_consistent",
            "observed_ask",
            "observed_rows",
            "operation",
            "profile",
            "request_sha256",
            "scope",
            "semantics_id",
            "shacl_applicable",
            "shacl_conforms",
            "shacl_meta_validated",
            "terminal_status",
            "unsatisfiable_classes",
            "unsupported",
        ]
        .into_iter()
        .collect()
    );
    Ok(())
}

#[tokio::test]
async fn requests_the_server_cannot_answer_as_asked_are_refused_in_the_reply() -> TestResult {
    let app = test_app()?;
    let declared = capabilities(&app).await?;
    let mut other_build = request(&declared);
    other_build["build_id"] = json!("another build");
    assert_eq!(check(&app, other_build).await?["terminal_status"], "failed");

    let mut other_profile = request(&declared);
    other_profile["profile"] = json!("owl2-full");
    assert_eq!(
        check(&app, other_profile).await?["terminal_status"],
        "unsupported"
    );

    let mut too_long = request(&declared);
    too_long["limits"]["max_seconds"] = json!(1.0e9);
    let reply = check(&app, too_long).await?;
    assert_eq!(reply["terminal_status"], "unsupported");
    assert_eq!(reply["effective_limits"]["max_seconds"], json!(1.0e9));

    let mut tampered = request(&declared);
    tampered["input_hashes"]["@active_data"] = json!("0".repeat(64));
    let reply = check(&app, tampered).await?;
    assert_eq!(reply["terminal_status"], "failed");
    assert!(
        reply["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("@active_data"))
    );
    Ok(())
}

#[tokio::test]
async fn a_request_of_another_protocol_or_shape_is_a_bad_request() -> TestResult {
    let app = test_app()?;
    let declared = capabilities(&app).await?;
    let mut other_protocol = request(&declared);
    other_protocol["protocol"] = json!("something-else/1");
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/v1/draft-check",
        Some(other_protocol),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut extra = request(&declared);
    extra["unexpected"] = json!(true);
    let (status, _) = call(&app, Method::POST, "/api/v1/draft-check", Some(extra)).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    Ok(())
}

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";
const OWL: &str = "http://www.w3.org/2002/07/owl#";
const EX: &str = "https://example.test/";

/// `subject ⊑ ∃r.filler`.
fn some(subject: &str, filler: &str) -> String {
    format!(
        "<{subject}> <{RDFS}subClassOf> _:r .\n\
         _:r <{RDF}type> <{OWL}Restriction> .\n\
         _:r <{OWL}onProperty> <{EX}r> .\n\
         _:r <{OWL}someValuesFrom> <{filler}> .\n"
    )
}

/// `A ⊑ ∀R.∃S.⊥` with `R ∘ S ⊑ S` and `S ∘ R ⊑ R`: with `a : A` and `R(a, b)` no model,
/// and the universal over the irregular `R` is left out of the DL engines.
fn irregular() -> String {
    let chain = |p: &str, first: &str, second: &str, l: &str| {
        format!(
            "<{EX}{p}> <{OWL}propertyChainAxiom> _:{l}1 .\n\
             _:{l}1 <{RDF}first> <{EX}{first}> .\n_:{l}1 <{RDF}rest> _:{l}2 .\n\
             _:{l}2 <{RDF}first> <{EX}{second}> .\n_:{l}2 <{RDF}rest> <{RDF}nil> .\n"
        )
    };
    format!(
        "<{EX}R> <{RDF}type> <{OWL}ObjectProperty> .\n\
         <{EX}S> <{RDF}type> <{OWL}ObjectProperty> .\n\
         <{EX}A> <{RDF}type> <{OWL}Class> .\n\
         {}{}\
         <{EX}A> <{RDFS}subClassOf> _:u .\n\
         _:u <{RDF}type> <{OWL}Restriction> .\n_:u <{OWL}onProperty> <{EX}R> .\n\
         _:u <{OWL}allValuesFrom> _:e .\n\
         _:e <{RDF}type> <{OWL}Restriction> .\n_:e <{OWL}onProperty> <{EX}S> .\n\
         _:e <{OWL}someValuesFrom> <{OWL}Nothing> .\n",
        chain("S", "R", "S", "s"),
        chain("R", "S", "R", "q"),
    )
}

/// The reasoning check's verdict (`nrese:draft-check/2`, notes N4 design G4-1 to G4-3):
/// `logical_consistent` is `false` where the OWL 2 RL rules or the OWL 2 DL engines found
/// no model, `true` where neither did and both decided, `null` where nothing decided. An
/// inconsistent input lists no unsatisfiable class. Inputs from the witnesses
/// `lane-d-draft-witness-cab7e725` and `lane-d-classification-witness-84baa169`.
#[tokio::test]
async fn the_verdict_is_the_rules_and_dl_engines_or_null() -> TestResult {
    let member = format!("<{EX}a> <{RDF}type> <{EX}A> .\n");
    let thing = format!("{OWL}Thing");
    let nothing = format!("{OWL}Nothing");
    let a = format!("{EX}A");
    // (name, schema, data, logical_consistent, complete)
    let cases = [
        // ⊤ ⊑ ∃r.⊥: no model; the DL engines see it, the RL rules don't.
        (
            "top",
            some(&thing, &nothing),
            String::new(),
            json!(false),
            json!(true),
        ),
        // A ⊑ ∃r.⊥ with a : A.
        (
            "member",
            some(&a, &nothing),
            member.clone(),
            json!(false),
            json!(true),
        ),
        // a : ⊥, which the RL rules see (cls-nothing2).
        (
            "rules",
            String::new(),
            format!("<{EX}a> <{RDF}type> <{OWL}Nothing> .\n"),
            json!(false),
            json!(true),
        ),
        // No model, and nothing decides it.
        (
            "undecided",
            irregular(),
            format!("{member}<{EX}a> <{EX}R> <{EX}b> .\n"),
            Value::Null,
            json!(false),
        ),
        // A ⊑ ∃r.B with a : A: a model.
        (
            "consistent",
            some(&a, &format!("{EX}B")),
            member,
            json!(true),
            json!(true),
        ),
    ];
    let app = test_app()?;
    let declared = capabilities(&app).await?;
    let mut wrong = Vec::new();
    for (name, schema, data, consistent, complete) in cases {
        let mut asked = request(&declared);
        asked["operation"] = json!("reasoning");
        asked["inputs"] = json!({"data": data, "schema": schema});
        let reply = check(&app, asked).await?;
        let got = (
            &reply["terminal_status"],
            &reply["logical_consistent"],
            &reply["complete"],
        );
        if got != (&json!("completed"), &consistent, &complete) {
            wrong.push(format!(
                "{name}: (status, logical_consistent, complete) {got:?}, \
                 expected (completed, {consistent}, {complete}): {reply}"
            ));
        }
        if consistent == json!(false) && reply["unsatisfiable_classes"] != json!([]) {
            wrong.push(format!(
                "{name}: an inconsistent input lists unsatisfiable classes {}",
                reply["unsatisfiable_classes"]
            ));
        }
    }
    // The field's meaning changed, so the semantics identifier did.
    if declared["semantics"] != json!(["nrese:draft-check/2"]) {
        wrong.push(format!("semantics {}", declared["semantics"]));
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}
