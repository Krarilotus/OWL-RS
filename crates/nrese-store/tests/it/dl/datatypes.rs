//! Datatypes outside the OWL 2 datatype map. The map (OWL 2 Structural Specification §4) has
//! `xsd:dateTime` and `xsd:dateTimeStamp` (§4.7) but not `xsd:date`, `xsd:time`, the `g*` types
//! or the durations, nor any other name of the XML Schema namespace it doesn't list. An ontology
//! using one of these as a datatype isn't OWL 2 DL (§5.2), and nothing here gives its values a
//! meaning. So where its values or lexical forms could decide a clash, the store can't back
//! `consistent`: it isn't told so, and no read over it is told complete, on any route; the RL
//! rules' route is complete only for OWL 2 RL, whose datatypes are the map's (OWL 2 Profiles
//! §4.2.1). Told as before: a value nothing reads, `xsd:dateTime`, and a datatype of the user's
//! own (§5.2 allows it, and nothing fixes its values, so no clash is missed).

use nrese_store::{MutationError, MutationPipeline};

use super::queries::query;
use super::{insert, pipeline};

const DATA: &str = ":born a owl:DatatypeProperty .";
const RL: &str = ":A a owl:Class . :p a owl:ObjectProperty . :a a :A . :a :p :b .";
/// A disjunction the upper bound can't prove consistent by itself.
const UNION: &str = ":A a owl:Class . :B a owl:Class . :C a owl:Class . \
     :A rdfs:subClassOf [ owl:unionOf ( :B :C ) ] . :B owl:disjointWith :C . :a a :A .";
/// Horn, not OWL 2 RL: an existential in a superclass.
const HORN: &str = ":A a owl:Class . :B a owl:Class . :p a owl:ObjectProperty . \
     :A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty :p ; owl:someValuesFrom :B ] . \
     :a a :A .";
const TWO_DATES: &str = r#":a :born "2020-01-01"^^xsd:date , "2021-06-30"^^xsd:date ."#;
const MAX_ONE: &str = ":A a owl:Class . :A rdfs:subClassOf [ a owl:Restriction ; \
     owl:onProperty :born ; owl:maxCardinality \"1\"^^xsd:nonNegativeInteger ] . :a a :A .";

/// The route and the verdict the store is told after its last commit, and whether a read of
/// `:born` is told complete; its reason as a `REASON` line.
fn told(dl: &MutationPipeline, name: &str) -> String {
    let status = dl.store().dl().status().expect("a status after a commit");
    let consistency = &status.consistency;
    let reason = consistency.verdict.reason().unwrap_or("none");
    println!("REASON {name}: {reason}");
    let (_, read) = query(dl, "SELECT ?s ?v { ?s :born ?v }");
    let read = if read.is_complete() {
        "complete"
    } else {
        "not complete"
    };
    format!(
        "{} {}; read {read}",
        consistency.engine,
        consistency.verdict.as_str()
    )
}

/// What one commit of `parts` into a fresh store is told; `refused` for a refused commit.
fn told_after(name: &str, parts: &[&str]) -> String {
    let dl = pipeline();
    match insert(&dl, &parts.join(" ")) {
        Ok(_) => told(&dl, name),
        Err(MutationError::Rejected(_)) => "refused".to_owned(),
        Err(other) => panic!("{name}: {other:?}"),
    }
}

/// Each cell's line as a `CASE` line, and every cell whose line isn't one of its own.
fn judge(cells: Vec<(&str, String, &[&str])>) {
    let mut wrong = Vec::new();
    for (name, got, want) in cells {
        println!("CASE {name}: {got}");
        if !want.contains(&got.as_str()) {
            wrong.push(format!("{name}: {got}, not {want:?}"));
        }
    }
    assert!(wrong.is_empty(), "wrong: {wrong:#?}");
}

/// The full check decides (the upper bound proves nothing where data values are tested), and
/// its datatype check has no values for a datatype outside the map, so a model it finds is no
/// answer: `unknown`, and every read says why it isn't complete.
const NOT_TOLD: &[&str] = &["hypertableau unknown; read not complete"];
const RULES: &[&str] = &["rules consistent; read complete"];
const REFUSED: &[&str] = &["refused"];
const DECIDED: &[&str] = &["hypertableau consistent; read complete"];

#[test]
fn a_datatype_outside_the_map_where_its_values_decide_a_clash_is_not_told_consistent() {
    let range =
        |d: &'static str| -> String { format!(":born a owl:DatatypeProperty ; rdfs:range {d} .") };
    let (date, integer) = (range("xsd:date"), range("xsd:integer"));
    let cells: Vec<(&str, Vec<String>, &[&str])> = vec![
        (
            "an ill-typed date under a date range",
            vec![date.clone(), r#":a :born "not-a-date"^^xsd:date ."#.into()],
            NOT_TOLD,
        ),
        (
            "an integer under a date range",
            vec![date.clone(), ":a :born 5 .".into()],
            NOT_TOLD,
        ),
        (
            "two dates under a functional property",
            vec![
                format!("{DATA} :born a owl:FunctionalProperty ."),
                TWO_DATES.into(),
            ],
            NOT_TOLD,
        ),
        (
            "two dates under a functional property with a date range",
            vec![
                date.clone(),
                ":born a owl:FunctionalProperty .".into(),
                TWO_DATES.into(),
            ],
            NOT_TOLD,
        ),
        (
            "two dates under max 1",
            vec![DATA.into(), MAX_ONE.into(), TWO_DATES.into()],
            NOT_TOLD,
        ),
        (
            "a date under a date range and an integer range",
            vec![
                date.clone(),
                integer.clone(),
                r#":a :born "2020-01-01"^^xsd:date ."#.into(),
            ],
            NOT_TOLD,
        ),
        (
            "an integer under a date range and an integer range",
            vec![date.clone(), integer.clone(), ":a :born 5 .".into()],
            NOT_TOLD,
        ),
        (
            "a date under an integer range",
            vec![
                integer.clone(),
                r#":a :born "2020-01-01"^^xsd:date ."#.into(),
            ],
            NOT_TOLD,
        ),
        (
            "an integer under a gYear range",
            vec![range("xsd:gYear"), ":a :born 5 .".into()],
            NOT_TOLD,
        ),
        (
            "an ill-typed duration under a duration range",
            vec![
                range("xsd:duration"),
                r#":a :born "not-a-duration"^^xsd:duration ."#.into(),
            ],
            NOT_TOLD,
        ),
        (
            "a dateTime under a misspelt xsd:datetime range",
            vec![
                range("xsd:datetime"),
                r#":a :born "2020-01-01T00:00:00Z"^^xsd:dateTime ."#.into(),
            ],
            NOT_TOLD,
        ),
    ];
    judge(
        cells
            .into_iter()
            .map(|(name, parts, want)| {
                let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
                (name, told_after(name, &parts), want)
            })
            .collect(),
    );
}

#[test]
fn the_maps_datatypes_a_value_nothing_reads_and_a_users_datatype_are_told_as_before() {
    const DATE_TIME: &str = r#":a :born "2020-01-01T00:00:00Z"^^xsd:dateTime ."#;
    const DATE_TIME_RANGE: &str = ":born a owl:DatatypeProperty ; rdfs:range xsd:dateTime .";
    let cells: Vec<(&str, Vec<&str>, &[&str])> = vec![
        ("the RL control", vec![RL], RULES),
        ("a dateTime nothing reads", vec![RL, DATE_TIME], RULES),
        (
            "a dateTime under a dateTime range",
            vec![RL, DATE_TIME_RANGE, DATE_TIME],
            RULES,
        ),
        (
            "an ill-typed dateTime under a dateTime range",
            vec![
                RL,
                DATE_TIME_RANGE,
                r#":a :born "not-a-dateTime"^^xsd:dateTime ."#,
            ],
            REFUSED,
        ),
        (
            "a dateTime beside a disjunction",
            vec![UNION, DATE_TIME],
            DECIDED,
        ),
        (
            "a dateTime under a dateTime range in a Horn ontology",
            vec![HORN, DATE_TIME_RANGE, DATE_TIME],
            DECIDED,
        ),
        // Nothing reads the value, so it decides no clash: the rules' route or the upper
        // bound's proof, consistent either way.
        (
            "a date nothing reads",
            vec![RL, r#":a :born "2020-01-01"^^xsd:date ."#],
            NOTHING_READS,
        ),
        (
            "a datatype of the user's own under its range",
            vec![
                ":myType a rdfs:Datatype . :born a owl:DatatypeProperty ; rdfs:range :myType .",
                r#":a :born "x"^^:myType ."#,
            ],
            RULES,
        ),
    ];
    judge(
        cells
            .into_iter()
            .map(|(name, parts, want)| (name, told_after(name, &parts), want))
            .collect(),
    );
}

/// A commit that adds only a value on a data property the RL route already knows keeps the
/// route without reading the ontology again; a value outside the map must still leave it.
/// One cell per way a literal is held: an inline `xsd:date`, and a literal kept in the
/// dictionary (`xsd:gYear`).
#[test]
fn a_later_value_outside_the_map_takes_the_store_off_the_rules_route() {
    let mut cells = Vec::new();
    for (name, value) in [
        (
            "a later date under an integer range",
            r#":b :born "2020-01-01"^^xsd:date ."#,
        ),
        (
            "a later gYear under an integer range",
            r#":b :born "2020"^^xsd:gYear ."#,
        ),
    ] {
        let dl = pipeline();
        insert(
            &dl,
            ":born a owl:DatatypeProperty ; rdfs:range xsd:integer . :a :born 5 .",
        )
        .expect("an OWL 2 RL store");
        let first = format!("{name}, before");
        cells.push((first.clone(), told(&dl, &first), RULES));
        let got = match insert(&dl, value) {
            Ok(_) => told(&dl, name),
            Err(MutationError::Rejected(_)) => "refused".to_owned(),
            Err(other) => panic!("{name}: {other:?}"),
        };
        cells.push((name.to_owned(), got, NOT_TOLD));
    }
    judge(
        cells
            .iter()
            .map(|(name, got, want)| (name.as_str(), got.clone(), *want))
            .collect(),
    );
}

/// The rules' route, or the upper bound's proof: consistent either way.
const NOTHING_READS: &[&str] = &[
    "rules consistent; read complete",
    "upper-bound consistent; read complete",
];

/// An ill-typed literal of a datatype of the map ("abc" isn't in `xsd:integer`'s lexical
/// space) makes the input not OWL 2 DL (Structural Specification §5.7). OWL 2 RL's rules make
/// it a clash only where a rule types it (dt-not-type, OWL 2 Profiles §4.3, table 8): alone it
/// is told `consistent`, under a range the commit is refused. Where a value is read
/// (`hasValue`), the store leaves the rules' route, and the full check, which has no value for
/// the literal, tells `unknown`.
#[test]
fn an_ill_typed_literal_of_the_maps_datatypes_is_told_as_the_rules_tell_it() {
    const ILL: &str = r#":a :born "abc"^^xsd:integer ."#;
    const HAS_VALUE: &str = ":C a owl:Class ; owl:equivalentClass [ a owl:Restriction ; \
         owl:onProperty :born ; owl:hasValue 5 ] .";
    let range = ":born rdfs:range xsd:integer .";
    let cells: Vec<(&str, Vec<&str>, &[&str])> = vec![
        ("an ill-typed integer nothing reads", vec![DATA, ILL], RULES),
        (
            "an ill-typed integer under an integer range",
            vec![DATA, range, ILL],
            REFUSED,
        ),
        (
            "an ill-typed integer beside a hasValue",
            vec![DATA, HAS_VALUE, ILL],
            NOT_TOLD,
        ),
    ];
    judge(
        cells
            .into_iter()
            .map(|(name, parts, want)| (name, told_after(name, &parts), want))
            .collect(),
    );
}
