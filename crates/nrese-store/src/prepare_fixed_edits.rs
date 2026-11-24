use super::*;

/// Spec: default and compact equality preserve the same answers across changes
/// to the schema, list membership and equality, including re-derivation and refusal.
#[test]
fn prepared_writes_match_settled_program_edits() {
    fn edit(insert: &str, remove: &str) -> Edit {
        Edit {
            insert: parse(insert),
            remove: parse(remove),
        }
    }
    let setup = edit(
        "ex:x a ex:A . ex:y a ex:B . ex:A rdfs:subClassOf ex:B . \
         ex:Union owl:unionOf ex:list . ex:list rdf:first ex:A ; rdf:rest rdf:nil . \
         ex:C owl:disjointWith ex:B .",
        "",
    );
    let steps = [
        edit(
            "ex:A rdfs:subClassOf ex:D . ex:B rdfs:subClassOf ex:D .",
            "",
        ),
        edit("", "ex:A rdfs:subClassOf ex:D ."),
        edit("ex:x owl:sameAs ex:y .", ""),
        edit("ex:list rdf:first ex:B .", "ex:list rdf:first ex:A ."),
        edit("ex:x a ex:C .", ""),
    ];
    for compact in [false, true] {
        replay(rl(), compact, setup.clone(), steps.clone(), |_| {});
    }
}

/// Spec: when a property is redeclared as an annotation, the OWL 2 QL writers
/// withdraw the domain conclusion the same way a full rematerialisation does.
#[test]
fn prepared_writers_retract_the_domain_of_a_redeclared_annotation() {
    let ql = Arc::new(ReasonerService::new(ReasonerConfig::for_mode(
        ReasoningMode::Owl2Ql,
    )));
    let setup = Edit {
        insert: parse("ex:q rdfs:domain ex:A . ex:t ex:q ex:y ."),
        ..Edit::default()
    };
    let redeclare = Edit {
        insert: parse("ex:q a owl:AnnotationProperty ."),
        ..Edit::default()
    };
    replay(ql, false, setup, [redeclare], |store| {
        let answer = store
            .execute_query_str(&format!("{PREFIX} ASK {{ ex:t a ex:A }}"))
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&answer.payload).unwrap();
        assert_eq!(json["boolean"], false);
    });
}
