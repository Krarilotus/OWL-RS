//! `GET /dataset/classification` and `/dataset/realisation`: the OWL 2 DL hierarchy and
//! types of the loaded ontology, as JSON and as N-Triples.

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use nrese_reasoner::ReasonerConfig;
use nrese_server::policy::PolicyConfig;
use nrese_store::StoreConfig;
use tower::util::ServiceExt;

use crate::support::{body_text, test_app_with_store_config};

const ONTOLOGY: &str = r#"
@prefix ex: <http://example.com/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Dog rdfs:subClassOf ex:Mammal , [ a owl:Restriction ; owl:onProperty ex:has ; owl:someValuesFrom ex:Tail ] .
ex:Mammal rdfs:subClassOf ex:Animal .
ex:TailedThing owl:equivalentClass [ a owl:Restriction ; owl:onProperty ex:has ; owl:someValuesFrom ex:Tail ] .
ex:Rock owl:disjointWith ex:Animal .
ex:PetRock rdfs:subClassOf ex:Rock , ex:Dog .
ex:Odd rdfs:subClassOf [ owl:unionOf ( ex:Dog ex:Mammal ) ] .
ex:rex a ex:Odd .
"#;

fn get(accept: &str) -> Result<Request<Body>, axum::http::Error> {
    get_at("/dataset/classification", accept)
}

fn get_at(uri: &str, accept: &str) -> Result<Request<Body>, axum::http::Error> {
    Request::builder()
        .uri(uri)
        .method(Method::GET)
        .header("accept", accept)
        .body(Body::empty())
}

#[tokio::test]
async fn the_hierarchy_as_json_and_as_triples() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("pets.ttl");
    std::fs::write(&path, ONTOLOGY)?;
    let app = test_app_with_store_config(
        StoreConfig::in_memory().with_ontology(path),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )?;
    let response = app.clone().oneshot(get("application/json")?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    let pairs: Vec<(String, String)> = serde_json::from_value(json["subsumptions"].clone())?;
    let has = |a: &str, b: &str| {
        pairs.contains(&(
            format!("http://example.com/{a}"),
            format!("http://example.com/{b}"),
        ))
    };
    assert!(has("Dog", "Animal") && has("Dog", "TailedThing") && has("Mammal", "Animal"));
    assert!(!has("Animal", "Dog"));
    assert_eq!(
        json["unsatisfiable"],
        serde_json::json!(["http://example.com/PetRock"])
    );
    // The union (Odd ⊑ Dog ⊔ Mammal) is classified, not skipped: Odd ⊑ Mammal.
    assert!(has("Odd", "Mammal") && has("Odd", "Animal") && !has("Odd", "Dog"));
    assert_eq!(json["profile"], "OWL 2 DL");
    assert_eq!(json["engine"], "tableau");
    assert_eq!(json["complete"], true);
    assert_eq!(json["consistent"], true);

    let response = app.clone().oneshot(get("application/n-triples")?).await?;
    assert_eq!(response.status(), StatusCode::OK);
    let text = body_text(response).await?;
    assert!(text.contains(
        "<http://example.com/Dog> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://example.com/TailedThing> ."
    ));
    assert!(text.contains("<http://example.com/PetRock> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://www.w3.org/2002/07/owl#Nothing> ."));

    // Realisation: rex is an Odd, so a Mammal and an Animal.
    let response = app
        .clone()
        .oneshot(get_at("/dataset/realisation", "application/json")?)
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
    assert_eq!(json["complete"], true);
    let types: Vec<String> =
        serde_json::from_value(json["types"]["http://example.com/rex"].clone())?;
    for class in ["Odd", "Mammal", "Animal"] {
        assert!(
            types.contains(&format!("http://example.com/{class}")),
            "{types:?}"
        );
    }
    assert!(!types.contains(&"http://example.com/Dog".to_owned()));
    let response = app
        .oneshot(get_at("/dataset/realisation", "application/n-triples")?)
        .await?;
    let text = body_text(response).await?;
    assert!(text.contains(
        "<http://example.com/rex> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://example.com/Animal> ."
    ));
    Ok(())
}

/// No model: `a : A`, `R(a, b)`, `A ⊑ ∀R.∃S.⊥` with `R ∘ S ⊑ S` and `S ∘ R ⊑ R`. The RL
/// rules see no clash, and the universal over the irregular `R` is left out of the DL
/// engines, so nothing decides it (witness `lane-d-classification-witness-84baa169`).
const UNDECIDED: &str = r#"
@prefix ex: <http://example.com/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:R a owl:ObjectProperty .
ex:S a owl:ObjectProperty .
ex:A a owl:Class .
ex:S owl:propertyChainAxiom ( ex:R ex:S ) .
ex:R owl:propertyChainAxiom ( ex:S ex:R ) .
ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:R ;
    owl:allValuesFrom [ a owl:Restriction ; owl:onProperty ex:S ; owl:someValuesFrom owl:Nothing ] ] .
ex:a a ex:A ; ex:R ex:b .
"#;

/// No model, decided: `⊤ ⊑ ∃r.⊥` (witness `lane-d-draft-witness-cab7e725`), with a class
/// below `owl:Thing` and a member.
const INCONSISTENT: &str = r#"
@prefix ex: <http://example.com/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:A rdfs:subClassOf ex:B .
owl:Thing rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:r ; owl:someValuesFrom owl:Nothing ] .
ex:a a ex:A .
"#;

/// Declarations only: a model, nothing to list.
const DECLARED: &str = r#"
@prefix ex: <http://example.com/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
ex:A a owl:Class .
"#;

const ROUTES: [&str; 4] = [
    "/dataset/classification",
    "/dataset/realisation",
    "/api/v1/repositories/nrese/classification",
    "/api/v1/repositories/nrese/realisation",
];

/// Every form of every route states the verdict or its absence (notes N4 design, G4-1 and
/// G4-2): JSON `consistent` is `true`, `false` or `null` with `consistency`; an
/// inconsistency lists no class and is the one triple `owl:Thing ⊑ owl:Nothing`; the
/// N-Triples form carries `NRESE-Completeness`, so an undecided answer is never an
/// unmarked list.
#[tokio::test]
async fn every_form_states_the_verdict_or_its_absence() -> Result<(), Box<dyn std::error::Error>> {
    const INCONSISTENCY: &str = "<http://www.w3.org/2002/07/owl#Thing> \
        <http://www.w3.org/2000/01/rdf-schema#subClassOf> \
        <http://www.w3.org/2002/07/owl#Nothing> .\n";
    // (name, ontology, consistent, consistency, header's status)
    let cases = [
        (
            "undecided",
            UNDECIDED,
            serde_json::Value::Null,
            "unknown",
            "sound-only",
        ),
        (
            "inconsistent",
            INCONSISTENT,
            serde_json::json!(false),
            "inconsistent",
            "complete",
        ),
        (
            "declared",
            DECLARED,
            serde_json::json!(true),
            "consistent",
            "complete",
        ),
    ];
    let mut wrong = Vec::new();
    for (name, ontology, consistent, consistency, status) in cases {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("ontology.ttl");
        std::fs::write(&path, ontology)?;
        let app = test_app_with_store_config(
            StoreConfig::in_memory().with_ontology(path),
            PolicyConfig::default(),
            ReasonerConfig::default(),
        )?;
        for route in ROUTES {
            let response = app
                .clone()
                .oneshot(get_at(route, "application/json")?)
                .await?;
            let json: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
            if (&json["consistent"], &json["consistency"])
                != (&consistent, &serde_json::json!(consistency))
            {
                wrong.push(format!(
                    "{name} {route} json: consistent {}, consistency {}; expected {consistent}, \
                     {consistency}",
                    json["consistent"], json["consistency"]
                ));
            }
            let listed = ["subsumptions", "unsatisfiable", "equivalent_to_thing"]
                .iter()
                .any(|f| json[f].as_array().is_some_and(|l| !l.is_empty()))
                || json["types"].as_object().is_some_and(|t| !t.is_empty());
            if consistency == "inconsistent" && listed {
                wrong.push(format!(
                    "{name} {route} json: an inconsistency with a list: {json}"
                ));
            }
            let response = app
                .clone()
                .oneshot(get_at(route, "application/n-triples")?)
                .await?;
            let header = response
                .headers()
                .get("nrese-completeness")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let triples = body_text(response).await?;
            let body_ok = match consistency {
                "inconsistent" => triples == INCONSISTENCY,
                _ => !triples.contains("owl#Nothing"),
            };
            if !body_ok {
                wrong.push(format!("{name} {route} triples: {triples:?}"));
            }
            let marked = header.as_deref().is_some_and(|h| {
                h.split(';').next() == Some(status)
                    && (status == "complete" || h.contains("consistency not decided"))
            });
            if !marked {
                wrong.push(format!(
                    "{name} {route} triples: NRESE-Completeness {header:?}, expected {status}"
                ));
            }
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}
