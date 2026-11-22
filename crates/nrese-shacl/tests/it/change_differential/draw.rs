use std::fmt::Write as _;

use nrese_rdf::Quad;

use super::case::{Case, Step, quads};
use crate::incremental_tests::{Rng, fuzz};

const SAME_AS: &str = "http://www.w3.org/2002/07/owl#sameAs";
/// Restriction for the known gaps `sameas-insert-unchecked` and `sameas-removal-unchecked`,
/// removed with them: a step changes no `owl:sameAs`; the start state may hold some.
const STEPS_CHANGE_SAME_AS: bool = false;
const OBJECTS: [&str; 5] = ["\"a\"", "\"a\"@en", "\"a\"@de", "1", "\"1\"^^xsd:decimal"];
const TEMPLATES: [&str; 2] = [
    "SELECT $this ?value WHERE { $this <http://example.com/p0> ?value FILTER(isLiteral(?value)) }",
    "SELECT $this ?value ('unexpected'@en AS ?message) WHERE { $this <http://example.com/p1> ?value }",
];

fn seed(index: u64) -> u64 {
    let mut value = (fuzz(0x312d_7368_6163_6c00) ^ index).wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

pub(super) fn case(index: u64) -> Case {
    let rng = &mut Rng(seed(index));
    let mut asserted = initial(rng, 14, false);
    let mut inferred = initial(rng, 8, true);
    let mut case = Case::new(shapes(rng), "", Step::default());
    case.asserted = asserted.clone();
    case.inferred = inferred.clone();
    case.steps.clear();
    for _ in 0..=rng.below(3) {
        let old_asserted = asserted.clone();
        let old_inferred = inferred.clone();
        for _ in 0..=rng.below(3) {
            match rng.below(4) {
                0 => add(&mut asserted, quad(rng, false, false)),
                1 => remove(&mut asserted, rng),
                2 => add(&mut inferred, quad(rng, true, STEPS_CHANGE_SAME_AS)),
                _ => remove(&mut inferred, rng),
            }
        }
        case.steps.push(Step {
            add: difference(&asserted, &old_asserted),
            remove: difference(&old_asserted, &asserted),
            infer: difference(&inferred, &old_inferred),
            retract: difference(&old_inferred, &inferred),
            shapes: (rng.below(3) == 0).then(|| shapes(rng)),
        });
    }
    case.equality = case
        .inferred
        .iter()
        .chain(case.steps.iter().flat_map(|step| &step.infer))
        .any(|quad| quad.predicate.as_str() == SAME_AS);
    case
}

fn initial(rng: &mut Rng, max: u64, inferred: bool) -> Vec<Quad> {
    let mut quads = Vec::new();
    for _ in 0..rng.below(max) {
        add(&mut quads, quad(rng, inferred, inferred));
    }
    quads
}

fn difference(left: &[Quad], right: &[Quad]) -> Vec<Quad> {
    left.iter()
        .filter(|quad| !right.contains(quad))
        .cloned()
        .collect()
}

fn add(current: &mut Vec<Quad>, quad: Quad) {
    if !current.contains(&quad) {
        current.push(quad);
    }
}

fn remove(current: &mut Vec<Quad>, rng: &mut Rng) {
    let removable = (0..current.len())
        .filter(|&i| STEPS_CHANGE_SAME_AS || current[i].predicate.as_str() != SAME_AS)
        .collect::<Vec<_>>();
    if !removable.is_empty() {
        current.swap_remove(removable[rng.below(removable.len() as u64) as usize]);
    }
}

fn quad(rng: &mut Rng, inferred: bool, alias: bool) -> Quad {
    let subject = rng.below(12);
    let triple = match rng.below(12) {
        0 if alias => format!("ex:n{subject} owl:sameAs ex:n{} .", rng.below(12)),
        1..=3 => format!("ex:n{subject} a ex:C{} .", rng.below(3)),
        4 => format!("ex:m{} ex:pad ex:spare .", rng.below(6)),
        _ => {
            let predicate = rng.below(4);
            let object = if rng.below(3) == 0 {
                OBJECTS[rng.below(OBJECTS.len() as u64) as usize].to_owned()
            } else {
                format!("ex:n{}", rng.below(12))
            };
            format!("ex:n{subject} ex:p{predicate} {object} .")
        }
    };
    let text = if inferred {
        triple
    } else {
        match rng.below(4) {
            0 => triple,
            1 => format!("ex:g1 {{ {triple} }}"),
            2 => format!("ex:g2 {{ {triple} }}"),
            _ => format!("ex:shapes {{ {triple} }}"),
        }
    };
    quads(&text).pop().expect("one generated quad")
}

fn path(rng: &mut Rng, depth: u8) -> String {
    let kind = if depth == 0 { 0 } else { rng.below(7) };
    let step = |rng: &mut Rng| path(rng, depth - 1);
    match kind {
        0 => format!("ex:p{}", rng.below(4)),
        1 => format!("( {} {} )", step(rng), step(rng)),
        2 => format!("[ sh:alternativePath ( {} {} ) ]", step(rng), step(rng)),
        _ => {
            let parameter = [
                "inversePath",
                "zeroOrMorePath",
                "oneOrMorePath",
                "zeroOrOnePath",
            ];
            format!("[ sh:{} {} ]", parameter[kind as usize - 3], step(rng))
        }
    }
}

fn shapes(rng: &mut Rng) -> String {
    let names = (0..=rng.below(4))
        .map(|i| {
            if rng.below(2) == 0 {
                format!("ex:S{i}")
            } else {
                format!("_:S{i}")
            }
        })
        .collect::<Vec<_>>();
    let mut turtle = String::new();
    for (index, name) in names.iter().enumerate() {
        let target = match rng.below(5) {
            0 => "sh:targetNode ex:absent".to_owned(),
            1 => format!("sh:targetNode ex:n{}", rng.below(12)),
            2 => format!("sh:targetClass ex:C{}", rng.below(3)),
            3 => format!("sh:targetSubjectsOf ex:p{}", rng.below(4)),
            _ => format!("sh:targetObjectsOf ex:p{}", rng.below(4)),
        };
        write!(turtle, "{name} {target} ; sh:path {}", path(rng, 2)).unwrap();
        let mut options = (0..14).collect::<Vec<_>>();
        for _ in 0..=rng.below(3) {
            let kind = options.swap_remove(rng.below(options.len() as u64) as usize);
            write!(turtle, " ; {}", constraint(rng, &names, kind)).unwrap();
        }
        if rng.below(4) == 0 {
            turtle.push_str(" ; sh:deactivated true");
        }
        if rng.below(3) == 0 {
            turtle.push_str(" ; sh:severity sh:Warning");
        }
        if rng.below(3) == 0 {
            let message = OBJECTS[1 + rng.below(2) as usize];
            write!(turtle, " ; sh:message {message}").unwrap();
        }
        if rng.below(5) == 0 {
            let query = TEMPLATES[rng.below(TEMPLATES.len() as u64) as usize];
            writeln!(turtle, " ; sh:sparql ex:Check{index} .").unwrap();
            writeln!(turtle, "ex:Check{index} sh:select '''{query}''' .").unwrap();
        } else {
            turtle.push_str(" .\n");
        }
    }
    turtle
}

fn constraint(rng: &mut Rng, shapes: &[String], kind: usize) -> String {
    let child = |rng: &mut Rng| &shapes[rng.below(shapes.len() as u64) as usize];
    match kind {
        0 => "sh:minCount 1".into(),
        1 => "sh:maxCount 2".into(),
        2 => format!("sh:class ex:C{}", rng.below(3)),
        3 => "sh:nodeKind sh:IRI".into(),
        4 => "sh:datatype xsd:integer".into(),
        5 => "sh:hasValue \"a\"@en".into(),
        6 => "sh:uniqueLang true".into(),
        7 => format!("sh:node {}", child(rng)),
        8 => format!("sh:not {}", child(rng)),
        9..=11 => format!(
            "sh:{} ( {} {} )",
            ["and", "or", "xone"][kind - 9],
            child(rng),
            child(rng)
        ),
        12 => format!(
            "sh:qualifiedValueShape {} ; sh:qualifiedMinCount 1",
            child(rng)
        ),
        _ => format!("sh:property {}", child(rng)),
    }
}
