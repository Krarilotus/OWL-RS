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
