use std::collections::BTreeMap;
use std::fmt::Write as _;

use nrese_shacl::Component;

use super::{Row, core_key, predicate};
use crate::change_differential::case::{Case, Step, ex};
use crate::change_differential::gaps::EntryPoint;
use crate::change_differential::stops::Stop;

const MIN: &str = "ex:T sh:targetNode ex:n0 ; sh:property ex:Q .
    ex:Q sh:path ex:p0 ; sh:minCount 1 .";
const DATATYPE: &str = "ex:T sh:targetNode ex:n0 ; sh:property ex:Q .
    ex:Q sh:path ex:p0 ; sh:datatype xsd:integer .";
const SUPPORT: &str = "ex:n0 ex:p0 ex:v1 .";
const FAILURE: &str =
    "a SPARQL constraint at focus node <http://example.com/n5>: the query reported ?failure";

pub(super) fn rows() -> impl Iterator<Item = Row> {
    stops().into_iter().chain(failures())
}

// Spec: a stop while evaluating either revision is the whole outcome.
fn stops() -> Vec<Row> {
    let mut rows = Vec::new();
    for (entry, label) in [(EntryPoint::Data, "data"), (EntryPoint::Shapes, "shapes")] {
        for (side, prefix) in [(Stop::Before, "before"), (Stop::After, "after")] {
            let mut case = Case::new(MIN, SUPPORT, Step::delete(SUPPORT));
            if entry == EntryPoint::Shapes {
                case.steps[0].shapes = Some(MIN.to_owned());
            }
            case.stop = Some((entry, side));
            let mut row = Row::new(
                format!("cancel-{prefix}-{label}"),
                "Cancellation on the named revision returns a stop.",
                case,
                [],
                [core_key(
                    "n0",
                    predicate("p0"),
                    None,
                    "Q",
                    Component::MinCount,
                )],
            );
            row.entry = entry;
            rows.push(row);
        }

        let mut add = String::new();
        let values = (0..256).map(|i| format!("v{i}")).collect::<Vec<_>>();
        for value in &values {
            writeln!(add, "ex:n0 ex:p0 ex:{value} .").unwrap();
        }
        let mut step = Step::insert(&add);
        let initial = if entry == EntryPoint::Shapes {
            step.shapes = Some(DATATYPE.to_owned());
            ""
        } else {
            DATATYPE
        };
        let mut case = Case::new(initial, "", step);
        case.stop = Some((entry, Stop::Envelope(16 * 1024)));
        let mut row = Row::new(
            format!("envelope-{label}"),
            "The report's retained results exceed the request envelope.",
            case,
            [],
            values.iter().map(|value| {
                core_key(
                    "n0",
                    predicate("p0"),
                    Some(ex(value).into()),
                    "Q",
                    Component::Datatype,
                )
            }),
        );
        row.entry = entry;
        rows.push(row);
    }
    rows
}

// Spec: both change APIs report every failure of the later validation.
fn failures() -> [Row; 3] {
    let shapes = "ex:T sh:targetNode ex:n5 ; sh:sparql ex:Check .
        ex:Check sh:select '''SELECT $this ?failure WHERE {
            $this <http://example.com/p0> ?v .
            BIND(true AS ?failure)
        }''' .";
    let trigger = "ex:n5 ex:p0 ex:v1 .";
    let mut step = Step::insert(trigger);
    step.shapes = Some(format!("{shapes} ex:T sh:name \"conditional check\" ."));
    let row = |id, step| {
        let mut row = Row::new(
            id,
            "The selected edit triggers exactly one new SPARQL failure.",
            Case::new(shapes, "", step),
            [],
            [],
        );
        row.after_failures = BTreeMap::from([(FAILURE.into(), 1)]);
        row
    };
    let unconditional = "ex:T sh:targetNode ex:n5 ; sh:sparql ex:Check .
        ex:Check sh:select '''SELECT $this ?failure WHERE { BIND(true AS ?failure) }''' .";
    let mut persistent = Row::new(
        "failure-excluded-edit",
        "An edit only in the excluded shapes graph keeps the failure of both validations.",
        Case::new(
            unconditional,
            "",
            Step::insert("ex:shapes { ex:n5 ex:p0 ex:v1 . }"),
        ),
        [],
        [],
    );
    persistent.before_failures = BTreeMap::from([(FAILURE.into(), 1)]);
    persistent.after_failures = persistent.before_failures.clone();
    [
        row("failure-trigger-data", Step::insert(trigger)),
        row("failure-trigger-shapes", step).for_shapes(),
        persistent,
    ]
}
