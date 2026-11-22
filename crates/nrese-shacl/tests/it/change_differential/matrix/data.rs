use nrese_rdf::{Literal, NamedNode};
use nrese_shacl::{Component, PropertyPath, SH};

use super::{Row, core_key, predicate};
use crate::change_differential::case::{Case, Step, ex, quads};
use crate::change_differential::observe::{ResultKey, ResultTerm};

const MIN: &str = "ex:T sh:targetNode ex:n0 ; sh:property ex:Q .
    ex:Q sh:path ex:p0 ; sh:minCount 1 .";
const DATATYPE: &str = "ex:T sh:targetNode ex:n0 ; sh:property ex:Q .
    ex:Q sh:path ex:p0 ; sh:datatype xsd:integer .";
const SUPPORT: &str = "ex:n0 ex:p0 ex:v1 .";
const NAMED_SUPPORT: &str = "ex:g1 { ex:n0 ex:p0 ex:v1 . }";

pub(super) fn rows() -> impl Iterator<Item = Row> {
    life_cycle()
        .into_iter()
        .chain(paths())
        .chain(targets())
        .chain(inferred())
        .chain(graphs())
        .chain(equality())
}

// Spec: expansion of the sameAs class in each committed snapshot changes
// which values the property shape reads at its explicit target.
fn equality() -> [Row; 4] {
    const SUPPORT: &str = "ex:n1 ex:p0 ex:v1 .";
    const ALIAS: &str = "ex:n1 owl:sameAs ex:n0 .";
    const UNRELATED: &str = "ex:n2 owl:sameAs ex:n3 .";
    const MAX: &str = "ex:T sh:targetNode ex:n0 ; sh:property ex:Q .
        ex:Q sh:path ex:p0 ; sh:maxCount 0 .";
    let make = |shapes, data, before, infer, retract| {
        let mut case = Case::new(
            shapes,
            data,
            Step {
                infer: quads(infer),
                retract: quads(retract),
                ..Step::default()
            },
        );
        case.inferred = quads(before);
        case.equality = true;
        case
    };
    [
        Row::new(
            "equality-add-pos",
            "An inferred alias makes the target read a forbidden value.",
            make(MAX, SUPPORT, "", ALIAS, ""),
            [],
            [core_key(
                "n0",
                predicate("p0"),
                None,
                "Q",
                Component::MaxCount,
            )],
        ),
        Row::new(
            "equality-add-twin",
            "Equality between unrelated nodes leaves the target conformant.",
            make(MAX, SUPPORT, "", UNRELATED, ""),
            [],
            [],
        ),
        Row::new(
            "equality-remove-pos",
            "Removing an inferred alias takes away the target's only support.",
            make(MIN, SUPPORT, ALIAS, "", ALIAS),
            [],
            [min_key("n0")],
        ),
        Row::new(
            "equality-remove-twin",
            "A direct support remains when the alias is removed.",
            make(
                MIN,
                "ex:n1 ex:p0 ex:v1 . ex:n0 ex:p0 ex:v1 .",
                ALIAS,
                "",
                ALIAS,
            ),
            [],
            [],
        ),
    ]
}

fn min_key(focus: &str) -> ResultKey {
    core_key(focus, predicate("p0"), None, "Q", Component::MinCount)
}

fn life_cycle() -> [Row; 4] {
    let key = min_key("n0");
    let target = "ex:T sh:targetSubjectsOf ex:p1 ; sh:property ex:Q .
        ex:Q sh:path ex:p0 ; sh:minCount 1 .";
    let absent = "ex:T sh:targetNode ex:absent ; sh:property ex:Q .
        ex:Q sh:path ex:p0 ; sh:maxCount 0 .";
    let trigger = "ex:n0 ex:p1 ex:v1 .";
    [
        Row::new(
            "repair",
            "Repairing a result introduces none.",
            Case::new(MIN, "", Step::insert(SUPPORT)),
            [key.clone()],
            [],
        ),
        Row::new(
            "new-target",
            "A new target with no support introduces a result.",
            Case::new(target, "", Step::insert(trigger)),
            [],
            [key.clone()],
        ),
        Row::new(
            "persistent",
            "An unrelated edit at the focus leaves its existing result.",
            Case::new(MIN, "", Step::insert(trigger)),
            [key.clone()],
            [key],
        ),
        Row::new(
            "absent-target",
            "The first data about an explicit target can violate its shape.",
            Case::new(absent, "", Step::insert("ex:absent ex:p0 ex:v1 .")),
            [],
            [core_key(
                "absent",
                predicate("p0"),
                None,
                "Q",
                Component::MaxCount,
            )],
        ),
    ]
}

fn paths() -> Vec<Row> {
    use PropertyPath as Path;
    let p0 = || predicate("p0");
    let literal = || Literal::from("a").into();
    let mut rows = Vec::new();
    for (id, syntax, path, data, on, off, value, component) in [
        (
            "predicate",
            "ex:p0",
            p0(),
            "",
            "ex:n0 ex:p0 \"a\" .",
            "ex:n1 ex:p0 \"a\" .",
            literal(),
            Component::Datatype,
        ),
        (
            "inverse",
            "[ sh:inversePath ex:p0 ]",
            Path::Inverse(Box::new(p0())),
            "",
            "ex:n1 ex:p0 ex:n0 .",
            "ex:n1 ex:p0 ex:n2 .",
            ex("n1").into(),
            Component::Datatype,
        ),
        (
            "sequence",
            "( ex:p1 [ sh:alternativePath ( ex:p0 ex:p2 ) ] )",
            Path::Sequence(vec![
                predicate("p1"),
                Path::Alternative(vec![p0(), predicate("p2")]),
            ]),
            "ex:n0 ex:p1 ex:n1 .",
            "ex:n1 ex:p0 \"a\" .",
            "ex:n2 ex:p0 \"a\" .",
            literal(),
            Component::Datatype,
        ),
        (
            "alternative",
            "[ sh:alternativePath ( ex:p0 ( ex:p1 ex:p2 ) ) ]",
            Path::Alternative(vec![
                p0(),
                Path::Sequence(vec![predicate("p1"), predicate("p2")]),
            ]),
            "ex:n0 ex:p1 ex:n1 .",
            "ex:n1 ex:p2 \"a\" .",
            "ex:n2 ex:p2 \"a\" .",
            literal(),
            Component::Datatype,
        ),
        (
            "zero-or-more",
            "[ sh:zeroOrMorePath ex:p0 ]",
            Path::ZeroOrMore(Box::new(p0())),
            "",
            "ex:n0 ex:p0 \"a\" .",
            "ex:n1 ex:p0 \"a\" .",
            literal(),
            Component::NodeKind,
        ),
        (
            "one-or-more",
            "[ sh:oneOrMorePath ex:p0 ]",
            Path::OneOrMore(Box::new(p0())),
            "",
            "ex:n0 ex:p0 \"a\" .",
            "ex:n1 ex:p0 \"a\" .",
            literal(),
            Component::Datatype,
        ),
        (
            "zero-or-one",
            "[ sh:zeroOrOnePath ex:p0 ]",
            Path::ZeroOrOne(Box::new(p0())),
            "",
            "ex:n0 ex:p0 \"a\" .",
            "ex:n1 ex:p0 \"a\" .",
            literal(),
            Component::NodeKind,
        ),
    ] {
        let constraint = if component == Component::Datatype {
            "sh:datatype xsd:integer"
        } else {
            // The zero-hop value is the IRI focus itself.
            "sh:nodeKind sh:IRI"
        };
        let shapes = format!(
            "ex:T sh:targetNode ex:n0 ; sh:property ex:Q . ex:Q sh:path {syntax} ; {constraint} ."
        );
        rows.push(Row::new(
            format!("path-{id}-pos"),
            "A value reached by the path violates the constraint.",
            Case::new(shapes.clone(), data, Step::insert(on)),
            [],
            [core_key("n0", path, Some(value), "Q", component)],
        ));
        rows.push(Row::new(
            format!("path-{id}-twin"),
            "An edge one step off the path leaves the shape conformant.",
            Case::new(shapes, data, Step::insert(off)),
            [],
            [],
        ));
    }
    rows
}

fn targets() -> Vec<Row> {
    let mut rows = Vec::new();
    for (id, target, trigger) in [
        ("class", "sh:targetClass ex:C0", "ex:n0 a ex:C0 ."),
        (
            "subject",
            "sh:targetSubjectsOf ex:p1",
            "ex:n0 ex:p1 ex:v1 .",
        ),
        ("object", "sh:targetObjectsOf ex:p1", "ex:v1 ex:p1 ex:n0 ."),
    ] {
        let shapes =
            format!("ex:T {target} ; sh:property ex:Q . ex:Q sh:path ex:p0 ; sh:minCount 1 .");
        rows.push(Row::new(
            format!("target-{id}-add"),
            "A new target member violates the shape.",
            Case::new(shapes.clone(), "", Step::insert(trigger)),
            [],
            [min_key("n0")],
        ));
        rows.push(Row::new(
            format!("target-{id}-remove"),
            "A removed target member ceases to report a result.",
            Case::new(shapes, trigger, Step::delete(trigger)),
            [min_key("n0")],
            [],
        ));
    }
    rows
}

fn inferred() -> [Row; 6] {
    let mut retract = Case::new(
        MIN,
        "",
        Step {
            retract: quads(SUPPORT),
            ..Step::default()
        },
    );
    retract.inferred = quads(SUPPORT);
    let mut retract_with_copy = retract.clone();
    retract_with_copy.asserted = quads(NAMED_SUPPORT);
    let delete = Case::new(MIN, NAMED_SUPPORT, Step::delete(NAMED_SUPPORT));
    let mut delete_with_copy = delete.clone();
    delete_with_copy.inferred = quads(SUPPORT);
    let infer = |statement| Step {
        infer: quads(statement),
        ..Step::default()
    };
    [
        Row::new(
            "inferred-insert-pos",
            "An inferred value of the wrong datatype is a new result.",
            Case::new(DATATYPE, "", infer("ex:n0 ex:p0 \"a\" .")),
            [],
            [core_key(
                "n0",
                predicate("p0"),
                Some(Literal::from("a").into()),
                "Q",
                Component::Datatype,
            )],
        ),
        Row::new(
            "inferred-insert-twin",
            "An inferred integer conforms.",
            Case::new(DATATYPE, "", infer("ex:n0 ex:p0 1 .")),
            [],
            [],
        ),
        Row::new(
            "inferred-delete-pos",
            "Retracting the last inferred value violates minCount.",
            retract,
            [],
            [min_key("n0")],
        ),
        Row::new(
            "inferred-delete-twin",
            "An asserted copy in a selected named graph still supports minCount.",
            retract_with_copy,
            [],
            [],
        ),
        Row::new(
            "asserted-delete-pos",
            "Removing the last asserted value violates minCount.",
            delete,
            [],
            [min_key("n0")],
        ),
        Row::new(
            "asserted-delete-twin",
            "The inferred copy still supports minCount.",
            delete_with_copy,
            [],
            [],
        ),
    ]
}

fn graphs() -> [Row; 2] {
    let shapes = "
        ex:T sh:targetSubjectsOf ex:p1 ; sh:property ex:Q ; sh:sparql ex:Check .
        ex:Q sh:path ex:p0 ; sh:minCount 1 .
        ex:Check sh:select '''
            SELECT $this WHERE { FILTER NOT EXISTS { $this <http://example.com/p0> ?support } }
        ''' .
    ";
    let step = Step::insert("ex:g1 { ex:n0 ex:p1 ex:v1 . }");
    let node = ResultTerm::Rdf(ex("n0").into());
    let sparql_key = ResultKey {
        focus: node.clone(),
        path: None,
        value: Some(node),
        shape: ResultTerm::Rdf(ex("T").into()),
        constraint: Some(ResultTerm::Rdf(ex("Check").into())),
        component: NamedNode::new_unchecked(Component::Sparql.iri()),
        severity: NamedNode::new_unchecked(format!("{SH}Violation")),
        messages: vec![],
    };
    [
        Row::new(
            "graph-selected-support",
            "The selected target edit sees support in the selected graph in Core and SPARQL.",
            Case::new(shapes, NAMED_SUPPORT, step.clone()),
            [],
            [],
        ),
        Row::new(
            "graph-excluded-support",
            "The selected target edit cannot read support from the excluded shapes graph.",
            Case::new(shapes, "ex:shapes { ex:n0 ex:p0 ex:v1 . }", step),
            [],
            [min_key("n0"), sparql_key],
        ),
    ]
}
