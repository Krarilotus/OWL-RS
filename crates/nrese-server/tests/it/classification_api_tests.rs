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

/// A restriction with no property: the reader leaves the axiom out (a fatal diagnostic),
/// so the rest's model says nothing of the whole.
const MALFORMED: &str = r#"
@prefix ex: <http://example.com/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:A a owl:Class ; rdfs:subClassOf [ a owl:Restriction ] .
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
        (
            "malformed",
            MALFORMED,
            serde_json::Value::Null,
            "unknown",
            "sound-only",
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

/// An EL ontology with an irregular role hierarchy (`r1 ∘ r2 ⊑ r3`, `r3 ⊑ r1`): outside
/// OWL 2 DL, so the DL engines leave the chain out; the EL classifier answers by default.
const IRREGULAR: &str = r#"
@prefix ex: <http://example.com/> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:r1 a owl:ObjectProperty . ex:r2 a owl:ObjectProperty . ex:r3 a owl:ObjectProperty .
ex:r3 owl:propertyChainAxiom ( ex:r1 ex:r2 ) ; rdfs:subPropertyOf ex:r1 .
ex:A a owl:Class . ex:B a owl:Class . ex:C a owl:Class . ex:D a owl:Class .
ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:r1 ; owl:someValuesFrom ex:B ] .
ex:B rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:r2 ; owl:someValuesFrom ex:C ] .
[ a owl:Restriction ; owl:onProperty ex:r3 ; owl:someValuesFrom ex:C ] rdfs:subClassOf ex:D .
"#;

/// The EL classifier's answer on an input outside OWL 2 DL says so in every form: the
/// header names what is outside and the procedure, the JSON carries the record with its
/// two texts, and the N-Triples form starts with a comment line saying both. Without a
/// range, the answer is complete. Every form is checked; the failures are reported together.
#[tokio::test]
async fn an_answer_outside_owl2_dl_is_marked_in_every_form()
-> Result<(), Box<dyn std::error::Error>> {
    const HEADER: &str = "complete; regime=owl2-dl; outside=\"an irregular role hierarchy\"; by=el";
    const GUARANTEED: &str = "Every subsumption listed holds under the OWL 2 Direct Semantics.";
    const NOT_GUARANTEED: &str = "That other OWL 2 DL reasoners give this answer:";
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("irregular.ttl");
    std::fs::write(&path, IRREGULAR)?;
    let app = test_app_with_store_config(
        StoreConfig::in_memory().with_ontology(path),
        PolicyConfig::default(),
        ReasonerConfig::default(),
    )?;
    let header = |response: &axum::response::Response| {
        response
            .headers()
            .get("nrese-completeness")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let mut wrong = Vec::new();
    for route in [ROUTES[0], ROUTES[2]] {
        let response = app
            .clone()
            .oneshot(get_at(route, "application/json")?)
            .await?;
        if header(&response).as_deref() != Some(HEADER) {
            wrong.push(format!("{route} json: header {:?}", header(&response)));
        }
        let json: serde_json::Value = serde_json::from_str(&body_text(response).await?)?;
        let outside = &json["outside_profile"];
        let texts = outside["guaranteed"]
            .as_str()
            .is_some_and(|t| t.starts_with(GUARANTEED))
            && outside["not_guaranteed"]
                .as_str()
                .is_some_and(|t| t.starts_with(NOT_GUARANTEED));
        if (&json["engine"], &json["complete"]) != (&"el".into(), &true.into())
            || outside["what"] != serde_json::json!(["an irregular role hierarchy"])
            || outside["by"] != "el"
            || !texts
        {
            wrong.push(format!("{route} json: {json}"));
        }
        let response = app
            .clone()
            .oneshot(get_at(route, "application/n-triples")?)
            .await?;
        if header(&response).as_deref() != Some(HEADER) {
            wrong.push(format!("{route} triples: header {:?}", header(&response)));
        }
        let triples = body_text(response).await?;
        let first = triples.lines().next().unwrap_or_default();
        let marked = first
            .starts_with("# Outside OWL 2 DL (an irregular role hierarchy); answered by el.")
            && first.contains(&format!("Guaranteed: {GUARANTEED}"))
            && first.contains(&format!("Not guaranteed: {NOT_GUARANTEED}"));
        let answered = triples.contains(
            "<http://example.com/A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://example.com/D> .",
        );
        if !(marked && answered) {
            wrong.push(format!("{route} triples: {triples:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}
