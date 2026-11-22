use std::collections::BTreeSet;

use nrese_rdf::{NamedNode, Term};
use nrese_shacl::{Component, PropertyPath, SH};

use super::case::{Case, Step, ex, quads};
use super::observe::{Deviation, ResultKey, ResultTerm, compare};

mod data;

struct Row {
    id: String,
    description: &'static str,
    case: Case,
    before: BTreeSet<ResultKey>,
    after: BTreeSet<ResultKey>,
}

impl Row {
    fn new(
        id: impl Into<String>,
        description: &'static str,
        case: Case,
        before: impl IntoIterator<Item = ResultKey>,
        after: impl IntoIterator<Item = ResultKey>,
    ) -> Self {
        Self {
            id: id.into(),
            description,
            case,
            before: before.into_iter().collect(),
            after: after.into_iter().collect(),
        }
    }
}

fn predicate(name: &str) -> PropertyPath {
    PropertyPath::Predicate(ex(name))
}

fn core_key(
    focus: &str,
    path: PropertyPath,
    value: Option<Term>,
    shape: &str,
    component: Component,
) -> ResultKey {
    ResultKey {
        focus: ResultTerm::Rdf(ex(focus).into()),
        path: Some(path),
        value: value.map(ResultTerm::Rdf),
        shape: ResultTerm::Rdf(ex(shape).into()),
        constraint: None,
        component: NamedNode::new_unchecked(component.iri()),
        severity: NamedNode::new_unchecked(format!("{SH}Violation")),
        messages: vec![],
    }
}

// Spec: SHACL Core nested constraints report what a change newly violates,
// including reversals of conformance through sh:not and shared target routes.
pub(super) fn run() {
    let rows = recursion()
        .into_iter()
        .chain(cycle())
        .chain(shared_target())
        .chain(data::rows());
    let selector = std::env::var("NRESE_SHACL_CASE").ok();
    let mut checked = 0;
    for row in rows {
        let case_id = format!("row:{}", row.id);
        if selector.as_ref().is_some_and(|wanted| wanted != &case_id) {
            continue;
        }
        for padding in 0..3 {
            let context = format!("{case_id}, padding {padding}: {}", row.description);
            let mut case = row.case.clone();
            for i in 0..padding {
                case.asserted
                    .extend(quads(&format!("ex:m{i} ex:pad ex:spare .")));
            }
            for outcome in case
                .run()
                .unwrap_or_else(|error| panic!("{context}: {error}"))
            {
                let location = format!("{context}, step {}, {:?}", outcome.step, outcome.entry);
                let deviation = compare(&outcome.before, &outcome.after, &outcome.actual);
                assert_eq!(deviation, Deviation::default(), "{location}");
                assert_eq!(
                    outcome.before.result_set(),
                    row.before,
                    "{location}: full before"
                );
                assert_eq!(
                    outcome.after.result_set(),
                    row.after,
                    "{location}: full after"
                );
                assert!(
                    outcome.before.failures.is_empty(),
                    "{location}: full before failures"
                );
                assert!(
                    outcome.after.failures.is_empty(),
                    "{location}: full after failures"
                );
                assert!(
                    outcome.actual.failures.is_empty(),
                    "{location}: actual failures"
                );
            }
        }
        checked += 1;
    }
    assert!(checked != 0, "no matrix row matches {selector:?}");
}

fn recursion() -> Vec<Row> {
    let mut rows = Vec::new();
    for (wrapper, parameter, component) in [
        ("node", "sh:node {child}", Component::Node),
        ("not", "sh:not {child}", Component::Not),
        ("and", "sh:and ( {child} ex:Any )", Component::And),
        ("or", "sh:or ( {child} ex:Never )", Component::Or),
        ("xone", "sh:xone ( {child} ex:Never )", Component::Xone),
        (
            "qualified",
            "sh:qualifiedValueShape {child} ; sh:qualifiedMinCount 1",
            Component::QualifiedMinCount,
        ),
        ("property", "", Component::MinCount),
    ] {
        for depth in 1..=3 {
            let property = wrapper == "property";
            let odd_not = wrapper == "not" && depth % 2 == 1;
            let mut data = String::new();
            let mut shapes = String::from(
                "ex:T sh:targetNode ex:n0 . ex:Any sh:nodeKind sh:IRI . ex:Never sh:class ex:C2 .",
            );
            use std::fmt::Write as _;
            for level in 0..depth {
                let child = level + 1;
                write!(data, "ex:n{level} ex:p1 ex:n{child} . ").unwrap();
                if property {
                    if level == 0 {
                        shapes.push_str("ex:T sh:property ex:R1 . ");
                    } else {
                        write!(
                            shapes,
                            "ex:R{level} sh:path ex:p1 ; sh:property ex:R{child} . "
                        )
                        .unwrap();
                    }
                } else {
                    let parent = if level == 0 {
                        "ex:T".into()
                    } else {
                        format!("ex:L{level}")
                    };
                    let constraint = parameter.replace("{child}", &format!("ex:L{child}"));
                    write!(shapes, "{parent} sh:property ex:R{level} . ex:R{level} sh:path ex:p1 ; {constraint} . ").unwrap();
                }
            }
            let (focus, path, value, shape, supported) = if property {
                write!(shapes, "ex:R{depth} sh:path ex:p0 ; sh:minCount 1 . ").unwrap();
                let index = depth - 1;
                (format!("n{index}"), "p0", None, format!("R{depth}"), index)
            } else {
                write!(
                    shapes,
                    "ex:L{depth} sh:property ex:Z . ex:Z sh:path ex:p0 ; sh:minCount 1 . "
                )
                .unwrap();
                (
                    "n0".into(),
                    "p1",
                    (wrapper != "qualified").then(|| ex("n1").into()),
                    "R0".into(),
                    depth,
                )
            };
            let key = core_key(&focus, predicate(path), value, &shape, component);
            let support = format!("ex:n{supported} ex:p0 ex:v1 . ");
            if !odd_not {
                data.push_str(&support);
            }
            let step = if odd_not {
                Step::insert(&support)
            } else {
                Step::delete(&support)
            };
            rows.push(Row::new(
                format!("rec-{wrapper}-{depth}-pos"),
                "Changing the deepest support makes the target violate.",
                Case::new(shapes.clone(), &data, step.clone()),
                [],
                [key.clone()],
            ));
            write!(data, "ex:n{supported} ex:p0 ex:v2 . ").unwrap();
            let persistent = odd_not.then_some(key).into_iter().collect::<Vec<_>>();
            rows.push(Row::new(
                format!("rec-{wrapper}-{depth}-twin"),
                "A second support preserves the previous result set.",
                Case::new(shapes, &data, step),
                persistent.clone(),
                persistent,
            ));
        }
    }
    rows
}

fn cycle() -> [Row; 2] {
    let shapes = "
        ex:T sh:targetNode ex:n0 ; sh:property ex:M , ex:R .
        ex:M sh:path ex:p0 ; sh:minCount 1 .
        ex:R sh:path ex:p1 ; sh:node ex:T .
    ";
    let data = "
        ex:n0 ex:p1 ex:n1 ; ex:p0 ex:v1 .
        ex:n1 ex:p1 ex:n2 ; ex:p0 ex:v1 .
        ex:n2 ex:p1 ex:n3 ; ex:p0 ex:v1 .
        ex:n3 ex:p0 ex:v1 .
    ";
    let step = Step::delete("ex:n3 ex:p0 ex:v1 .");
    let key = core_key(
        "n0",
        predicate("p1"),
        Some(ex("n1").into()),
        "R",
        Component::Node,
    );
    [
        Row::new(
            "cycle-pos",
            "A reachable shape cycle carries the lost support back to the target.",
            Case::new(shapes, data, step.clone()),
            [],
            [key],
        ),
        Row::new(
            "cycle-twin",
            "A second support at the distant node keeps the cycle conformant.",
            Case::new(shapes, &format!("{data} ex:n3 ex:p0 ex:v2 ."), step),
            [],
            [],
        ),
    ]
}

fn shared_target() -> [Row; 2] {
    let shapes = "
        ex:A sh:targetClass ex:C ; sh:property ex:Q .
        ex:B sh:targetClass ex:D ; sh:property ex:R .
        ex:R sh:path ex:likes ; sh:property ex:Q .
        ex:Q sh:path ex:age ; sh:datatype xsd:integer .
    ";
    let data = "ex:v ex:age \"x\" . ex:y1 ex:likes ex:v .";
    let step = Step::insert("ex:v a ex:C .");
    let key = core_key(
        "v",
        predicate("age"),
        Some(nrese_rdf::Literal::from("x").into()),
        "Q",
        Component::Datatype,
    );
    [
        Row::new(
            "shared-target-pos",
            "The first route introduces the result.",
            Case::new(shapes, data, step.clone()),
            [],
            [key.clone()],
        ),
        Row::new(
            "shared-target-persistent",
            "A new route to a result already reached through an unchanged target adds nothing.",
            Case::new(shapes, &format!("{data} ex:y1 a ex:D ."), step),
            [key.clone()],
            [key],
        ),
    ]
}
