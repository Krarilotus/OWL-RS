use std::collections::{BTreeMap, BTreeSet};

use nrese_rdf::{NamedNode, Term};
use nrese_shacl::{Component, PropertyPath, SH};

use super::case::{Case, Step, ex, quads};
use super::gaps::{Allowances, EntryPoint, Kind, Location};
use super::observe::{Deviation, Observation, ResultKey, ResultTerm, compare};
use super::stops::{Outcome, Stop};

mod data;
mod identity;
mod shapes;
mod terminal;

struct Row {
    id: String,
    description: &'static str,
    case: Case,
    entry: EntryPoint,
    before: BTreeSet<ResultKey>,
    after: BTreeSet<ResultKey>,
    before_failures: BTreeMap<String, usize>,
    after_failures: BTreeMap<String, usize>,
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
            entry: EntryPoint::Data,
            before: before.into_iter().collect(),
            after: after.into_iter().collect(),
            before_failures: BTreeMap::new(),
            after_failures: BTreeMap::new(),
        }
    }

    fn for_shapes(mut self) -> Self {
        self.entry = EntryPoint::Shapes;
        self
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
fn rows() -> impl Iterator<Item = Row> {
    recursion()
        .into_iter()
        .chain(cycle())
        .chain(shared_target())
        .chain(data::rows())
        .chain(shapes::rows())
        .chain(identity::rows())
        .chain(terminal::rows())
}

/// Fails with `location`, whose first line is the replay command, unlike `assert_eq!`.
fn pin<A: PartialEq<B> + std::fmt::Debug, B: std::fmt::Debug>(
    location: &str,
    what: &str,
    actual: A,
    expected: B,
) {
    if actual != expected {
        panic!("{location}: {what}: expected {expected:?}, got {actual:?}");
    }
}

/// Runs every row, or the row `NRESE_SHACL_CASE` names. A draw's or saved case's selector
/// runs that case, so every selector runs a case or fails.
pub(super) fn run() {
    let outer = std::env::var("NRESE_FUZZ_SEED").unwrap_or_else(|_| "0".into());
    let selector = std::env::var("NRESE_SHACL_CASE").ok();
    if selector
        .as_ref()
        .is_some_and(|case| !case.starts_with("row:"))
    {
        return super::random::run();
    }
    let gaps = super::witnesses::verified_gaps();
    let mut allowances: [Allowances<'_>; 3] = Default::default();
    let mut checked = 0;
    let mut ran = BTreeSet::new();
    for row in rows() {
        let case_id = format!("row:{}", row.id);
        if selector.as_ref().is_some_and(|wanted| wanted != &case_id) {
            continue;
        }
        ran.insert(case_id.clone());
        let mut known = BTreeSet::new();
        let replay = super::random::replay("matrix", &outer, &case_id);
        for (padding, allowance) in allowances.iter_mut().enumerate() {
            // Every message's first line is the row's replay command.
            let context = format!(
                "{replay}\n{case_id}, padding {padding}: {}",
                row.description
            );
            let mut case = row.case.clone();
            for i in 0..padding {
                case.asserted
                    .extend(quads(&format!("ex:m{i} ex:pad ex:spare .")));
            }
            let mut pinned = false;
            for outcome in case
                .run()
                .unwrap_or_else(|error| panic!("{context}: {error}"))
            {
                let location = format!("{context}, step {}, {:?}", outcome.step, outcome.entry);
                let mut failures_forgiven = false;
                let stop = case.stop.filter(|(entry, _)| *entry == outcome.entry);
                let actual = match (&outcome.actual, stop.map(|(_, stop)| stop)) {
                    (Outcome::Report(actual), None) => {
                        let deviation = compare(&outcome.before, &outcome.after, actual);
                        if deviation != Deviation::default() {
                            let at = Location {
                                case: &case_id,
                                step: outcome.step,
                                entry: outcome.entry,
                            };
                            for kind in [Kind::Result, Kind::Failure] {
                                if !kind.select(&deviation).is_empty() {
                                    let id = allowance
                                        .forgive(&gaps, &at, kind, &deviation)
                                        .unwrap_or_else(|| panic!("{location}: {deviation:?}"));
                                    known.insert(id);
                                    failures_forgiven |= kind == Kind::Failure;
                                }
                            }
                        }
                        Some(actual)
                    }
                    (
                        Outcome::Cancelled {
                            before: true,
                            after: false,
                        },
                        Some(Stop::Before),
                    )
                    | (
                        Outcome::Cancelled {
                            before: false,
                            after: true,
                        },
                        Some(Stop::After),
                    )
                    | (Outcome::MemoryLimit, Some(Stop::Envelope(_))) => None,
                    (actual, stop) => panic!("{location}: expected {stop:?}, got {actual:?}"),
                };
                if outcome.entry != row.entry {
                    continue;
                }
                pinned = true;
                let (before, after) = (&outcome.before, &outcome.after);
                pin(&location, "full before", &before.result_set(), &row.before);
                pin(&location, "full after", &after.result_set(), &row.after);
                pin(
                    &location,
                    "full before failures",
                    &before.failures,
                    &row.before_failures,
                );
                pin(
                    &location,
                    "full after failures",
                    &after.failures,
                    &row.after_failures,
                );
                // A forgiven failure delta was matched count by count against its entry.
                if let Some(actual) = actual.filter(|_| !failures_forgiven) {
                    pin(
                        &location,
                        "actual failures",
                        &actual.failures,
                        &row.after_failures,
                    );
                }
                if case.fresh_value {
                    let values = |o: &Observation| o.results.values().sum::<usize>();
                    pin(&location, "one before value", values(before), 1_usize);
                    pin(&location, "one after value", values(after), 1_usize);
                }
            }
            assert!(pinned, "{context}: missing {:?} observation", row.entry);
        }
        if known.is_empty() {
            println!("{case_id} pass");
        } else {
            for id in known {
                println!("{case_id} known-gap {id}");
            }
        }
        checked += 1;
    }
    assert!(checked != 0, "no matrix row matches {selector:?}");
    for (padding, allowance) in allowances.iter().enumerate() {
        let closed = allowance.unused(&gaps, &ran);
        assert!(
            closed.is_empty(),
            "padding {padding}: the rows of {closed:?} pass: fixed, delete the entries"
        );
    }
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
            let support = format!("ex:n{supported} ex:p0 ex:support{supported} . ");
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
            write!(data, "ex:n{supported} ex:p0 ex:spare{supported} . ").unwrap();
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
        ex:n0 ex:p1 ex:n1 ; ex:p0 ex:v0 .
        ex:n1 ex:p1 ex:n2 ; ex:p0 ex:v1 .
        ex:n2 ex:p1 ex:n3 ; ex:p0 ex:v2 .
        ex:n3 ex:p0 ex:v3 .
    ";
    // The old walk back from the changed object must not reach n0 through p0.
    let step = Step::delete("ex:n3 ex:p0 ex:v3 .");
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
            Case::new(shapes, &format!("{data} ex:n3 ex:p0 ex:v4 ."), step),
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
