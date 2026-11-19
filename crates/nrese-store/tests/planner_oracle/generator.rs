use crate::routes::ENDPOINT;
use crate::twin::{Draw, Query};

#[derive(Clone)]
pub struct Case {
    pub shape: &'static str,
    pub data: Vec<String>,
    pub query: Query,
    pub remote: Option<&'static str>,
}

impl Case {
    pub fn new(shape: &'static str, data: Vec<String>, query: Query) -> Self {
        Self {
            shape,
            data,
            query,
            remote: None,
        }
    }
}

const LABEL: &str = "<http://www.w3.org/2000/01/rdf-schema#label>";
const VALUES: &str = "VALUES ?s { <urn:a> <urn:a> <urn:b> }";
const FEW: &str = "?s <urn:few> ?o";
const VECTOR: &str = "urn:nrese:vector:";
const REMOTE: &str = "<urn:a> <urn:remote> 1 .";

pub const RED_SHAPES: &[&str] = &[
    "site-1",
    "site-2",
    "optional-condition",
    "duplicate-keys",
    "nested-sibling",
    "unrelated-group",
    "undef",
    "graph-duplicates",
    "union-arms",
    "group-values",
];
pub const PROBE_SHAPES: &[&str] = &[
    "exists-independent",
    "lateral-draw",
    "path-draw",
    "vector-draw",
    "service-batches",
    "average-draw",
];

pub const PLAIN_SHAPES: &[&str] = &[
    "bgp",
    "join",
    "optional",
    "union",
    "values",
    "filter",
    "exists",
    "not-exists",
    "bind",
    "graph",
    "minus",
    "lateral",
    "path-plus",
    "path-star",
    "path-sequence",
    "path-inverse",
    "subquery",
    "aggregate",
    "vector",
    "service",
    "reduced",
];

const GENERATED_KEYS: &[&str] = &["s", "o", "t", "tag", "g", "v", "w", "x", "layer0", "layer1"];

fn keys(n: usize) -> String {
    (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(" ")
}

fn value(call: &'static str, keys: &'static [&'static str]) -> Draw {
    Draw::Value {
        call,
        column: "d",
        keys,
    }
}

fn filter(keys: &'static [&'static str], independent: bool) -> Draw {
    Draw::Filter { keys, independent }
}

pub fn drawing(shape: &'static str) -> Case {
    let mut data = vec![
        format!("<urn:a> {LABEL} \"alpha one\" ."),
        "<urn:a> <urn:few> 1 .".into(),
        "<urn:b> <urn:few> 2 .".into(),
        "<urn:a> <urn:q> \"z\" .".into(),
        "<urn:b> <urn:q> \"z\" .".into(),
        "<urn:g> { <urn:a> <urn:p> 1 . }".into(),
    ];
    let many = keys(40);
    let (text, draw) = match shape {
        "site-1" => (
            format!("SELECT DISTINCT ?d {{ VALUES ?k {{ 1 2 }} {{ BIND($DRAW AS ?d) }} FILTER EXISTS {{ ?s {LABEL} ?o }} }}"),
            value("BNODE()", &[]),
        ),
        "site-2" => (
            format!("SELECT ?k {{ VALUES ?k {{ {many} }} {{ ?x {LABEL} ?l FILTER($DRAW) }} }}"),
            filter(&[], false),
        ),
        "optional-condition" => (
            format!("SELECT ?s ?z {{ VALUES (?s ?x) {{ (<urn:a> 1) (<urn:b> 2) }} {{ {FEW} OPTIONAL {{ ?s <urn:q> ?z FILTER($DRAW && !BOUND(?x)) }} }} }}"),
            Draw::Condition,
        ),
        "duplicate-keys" => (
            format!("SELECT ?s ?o ?d {{ {VALUES} {{ {FEW} BIND($DRAW AS ?d) }} }}"),
            value("STRUUID()", &["s", "o"]),
        ),
        "nested-sibling" => {
            let subjects = (0..40).map(|i| {
                data.push(format!("<urn:s:{i}> <urn:sibling> {i} ."));
                format!("<urn:s:{i}>")
            }).collect::<Vec<_>>().join(" ");
            (
                format!("SELECT ?s {{ VALUES ?s {{ {subjects} }} {{ {{ ?x {LABEL} ?l FILTER($DRAW) }} ?s <urn:sibling> ?o BIND(1 AS ?one) }} }}"),
                filter(&[], false),
            )
        }
        "unrelated-group" => {
            for i in 0..512 {
                data.push(format!("<urn:decoy> <urn:wide> {i} ."));
            }
            for i in 0..254 {
                data.push(format!("<urn:decoy> <urn:many> {i} ."));
            }
            for s in ["urn:a", "urn:b"] {
                data.push(format!("<{s}> <urn:many> 0 . <{s}> <urn:wide> 0 ."));
            }
            (
                format!("SELECT ?s {{ {{ ?s <urn:many> ?i . ?s <urn:wide> ?j }} VALUES ?s {{ <urn:a> <urn:b> }} {{ ?t {LABEL} ?tag FILTER($DRAW) }} }}"),
                filter(&[], false),
            )
        }
        "undef" => (
            format!("SELECT ?s ?o ?d {{ VALUES ?s {{ <urn:a> UNDEF }} {{ {FEW} BIND($DRAW AS ?d) }} }}"),
            value("BNODE()", &["s", "o"]),
        ),
        "graph-duplicates" => (
            "SELECT ?s ?o ?d { GRAPH <urn:g> { VALUES ?s { <urn:a> <urn:a> } { ?s <urn:p> ?o BIND($DRAW AS ?d) } } }".into(),
            value("BNODE()", &["s", "o"]),
        ),
        "union-arms" => {
            for i in 0..100 {
                data.push(format!("<urn:s:{i}> <urn:p> {i} ."));
            }
            data.push("<urn:a> <urn:p> 1 .".into());
            (
                format!("SELECT ?s ?o ?d {{ VALUES ?s {{ <urn:a> <urn:a> }} {{ {{ ?s <urn:p> ?o BIND($DRAW AS ?d) }} UNION {{ {FEW} BIND($OTHER AS ?d) }} }} }}"),
                value("STRUUID()", &["s", "o"]),
            )
        }
        "group-values" => (
            format!("SELECT ?s ?o ?t ?d {{ VALUES ?s {{ <urn:a> <urn:a> }} {{ VALUES ?t {{ 1 2 }} {FEW} BIND($DRAW AS ?d) }} }}"),
            value("STRUUID()", &["s", "o", "t"]),
        ),
        "exists-independent" => (
            format!("SELECT ?i {{ VALUES ?i {{ {many} }} <urn:a> <urn:few> ?k FILTER EXISTS {{ ?s <urn:q> ?z FILTER($DRAW && ?s = <urn:a> && ?k = 1) }} }}"),
            filter(&["i"], true),
        ),
        "lateral-draw" => (
            "SELECT ?i ?d { VALUES (?s ?i) { (<urn:a> 1) (<urn:a> 2) } LATERAL { ?s <urn:few> ?o BIND($DRAW AS ?d) } }".into(),
            value("UUID()", &["i"]),
        ),
        "path-draw" => {
            data.extend(["<urn:a> <urn:step> <urn:b> .".into(), "<urn:b> <urn:step> <urn:c> .".into()]);
            (
                "SELECT ?s ?o { VALUES ?s { <urn:a> <urn:a> } { ?s <urn:step>+ ?o FILTER($DRAW) } }".into(),
                filter(&["s", "o"], false),
            )
        }
        "vector-draw" => {
            vectors(&mut data, 120);
            (
                format!("SELECT ?s ?d {{ {{ ?s <urn:embedding> ?v . {{ BIND($DRAW AS ?d) }} }} {} }}", vector_search()),
                value("BNODE()", &[]),
            )
        }
        "service-batches" => (
            format!("SELECT ?k ?d {{ VALUES ?k {{ {} }} SERVICE <{ENDPOINT}> {{ {{ <urn:a> <urn:remote> ?z BIND($DRAW AS ?d) }} UNION {{ ?k <urn:missing> ?z BIND($OTHER AS ?d) }} }} }}", keys(201)),
            value("UUID()", &[]),
        ),
        "average-draw" => (
            "SELECT (AVG($DRAW) AS ?avg) { ?s <urn:few> ?o . ?s <urn:q> ?z }".into(),
            Draw::Average,
        ),
        _ => panic!("unknown drawing shape {shape}"),
    };
    let mut case = Case::new(shape, data, Query::drawing(text, draw));
    if shape == "service-batches" {
        case.remote = Some(REMOTE);
    }
    case
}

fn vectors(data: &mut Vec<String>, size: usize) {
    for i in 0..size {
        let vector = nrese_engine::vector::lexical(&[1.0, i as f32 / 100.0]);
        data.push(format!(
            "<urn:vec:{i}> <urn:embedding> \"{vector}\"^^<{VECTOR}vector> ."
        ));
    }
}

fn vector_search() -> String {
    let vector = nrese_engine::vector::lexical(&[1.0, 0.0]);
    format!(
        "SERVICE <{VECTOR}search> {{ ?v <{VECTOR}near> \"{vector}\"^^<{VECTOR}vector> ; <{VECTOR}k> 2 ; <{VECTOR}exact> true }}"
    )
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((x ^ (x >> 31)) % n as u64) as usize
    }
}

pub fn generated(seed: u64) -> Case {
    let mut rng = Rng(seed);
    let shape = PLAIN_SHAPES[rng.below(PLAIN_SHAPES.len())];
    let subjects = 2 + rng.below(39);
    let predicates = 2 + rng.below(5);
    let mut data = Vec::new();
    for s in 0..subjects {
        let next = (s + 1) % subjects;
        data.push(format!("<urn:s:{s}> <urn:p:0> <urn:s:{next}> ."));
        for p in 1..predicates {
            if p == 1 || rng.below(2) == 0 {
                data.push(format!("<urn:s:{s}> <urn:p:{p}> {} .", rng.below(8)));
            }
        }
        data.push(format!("<urn:g> {{ <urn:s:{s}> <urn:p:1> \"s{s}\" . }}"));
    }
    for p in 2..=3 {
        for o in 0..100 {
            data.push(format!("<urn:wide> <urn:wide:{p}> {o} ."));
        }
    }
    data.push("<urn:s:0> <urn:few> 1 .".into());
    let n = 1 + rng.below(40);
    let values = (0..n)
        .map(|_| {
            let s = if rng.below(5) == 0 {
                "UNDEF".into()
            } else {
                format!("<urn:s:{}>", rng.below(subjects))
            };
            if seed.is_multiple_of(2) {
                s
            } else {
                format!("({s} {})", rng.below(5))
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let columns = if seed.is_multiple_of(2) {
        "?s"
    } else {
        "(?s ?tag)"
    };
    let values = format!("VALUES {columns} {{ {values} }}");
    let p = rng.below(predicates - 1) + 1;
    let bgp = format!("?s <urn:p:{p}> ?o");
    let mut case = Case::new(shape, data, Query::plain(String::new()));
    let mut modifier = "";
    let mut draw = None;
    let mut scope = None;
    let mut tail = String::new();
    let mut body = match shape {
        "bgp" => {
            let extra = [
                "",
                " . ?s <urn:p:0> ?t",
                " . ?s <urn:p:0> ?t . ?t <urn:p:1> ?v",
                " . ?s <urn:p:0> ?t . ?t <urn:p:1> ?v . ?t <urn:p:0> ?w",
            ];
            format!("{bgp}{}", extra[rng.below(extra.len())])
        }
        "join" => format!("{values} {{ {bgp} }} {{ ?s <urn:p:0> ?t }}"),
        "optional" => format!(
            "{values} {bgp} OPTIONAL {{ ?s <urn:few> ?x FILTER(?o >= {}) }}",
            rng.below(4)
        ),
        "union" => {
            let other = if rng.below(2) == 0 { "?o" } else { "?t" };
            format!("{values} {{ {{ {bgp} }} UNION {{ ?s <urn:p:0> {other} }} }}")
        }
        "values" => values,
        "filter" => format!("{bgp} FILTER(?o >= {})", rng.below(7)),
        "exists" => format!("{bgp} FILTER EXISTS {{ ?s <urn:few> ?x }}"),
        "not-exists" => format!("{bgp} FILTER NOT EXISTS {{ ?s <urn:few> ?x }}"),
        "bind" => format!("{bgp} BIND(?o + 1 AS ?x)"),
        "graph" => format!(
            "GRAPH {} {{ ?s <urn:p:1> ?o }}",
            if rng.below(2) == 0 { "?g" } else { "<urn:g>" }
        ),
        "minus" => format!("{values} {bgp} MINUS {{ ?s <urn:few> ?x }}"),
        "lateral" => format!("{values} LATERAL {{ {bgp} }}"),
        "path-plus" => "?s <urn:p:0>+ <urn:s:0>".into(),
        "path-star" => "<urn:s:0> <urn:p:0>* ?s".into(),
        "path-sequence" => "?s <urn:p:0>/<urn:p:1> ?o".into(),
        "path-inverse" => "?s ^<urn:p:0> ?o".into(),
        "subquery" => format!(
            "{{ SELECT ?s ?o {{ {bgp} }} ORDER BY ?s ?o LIMIT {} }} {values}",
            1 + rng.below(5)
        ),
        "vector" => {
            vectors(&mut case.data, 6);
            format!("{{ ?s <urn:embedding> ?v }} {}", vector_search())
        }
        "service" => {
            case.remote = Some(REMOTE);
            format!("{bgp} SERVICE <{ENDPOINT}> {{ ?x <urn:remote> ?z }}")
        }
        "aggregate" => {
            let aggregate = ["COUNT", "SUM", "MIN", "MAX", "AVG"][rng.below(5)];
            let distinct = if rng.below(2) == 0 { "DISTINCT " } else { "" };
            case.query.text = format!(
                "SELECT ?s ({aggregate}({distinct}?o) AS ?n) {{ {values} {bgp} }} GROUP BY ?s"
            );
            return case;
        }
        "reduced" => {
            modifier = "REDUCED ";
            format!("{values} {{ {bgp} }}")
        }
        _ => unreachable!(),
    };
    // Wrap the chosen algebra with up to two more operators. Use an independent
    // variable so that the added copies are intentional and bounded.
    for depth in 0..rng.below(3) {
        body = match rng.below(3) {
            0 => format!("{{ {body} }} VALUES ?layer{depth} {{ 0 1 }}"),
            1 => format!("{{ {body} }} OPTIONAL {{ VALUES ?layer{depth} {{ 0 }} }}"),
            _ => format!("{{ {body} }} FILTER(true)"),
        };
    }
    if shape != "reduced" && !matches!(shape, "vector" | "service") && rng.below(4) == 0 {
        let call = ["BNODE()", "UUID()", "STRUUID()"][rng.below(3)];
        // Site keys are the whole local input, including duplicate rows and UNDEF.
        // A unique row id is immaterial to the ordinary joins these groups test.
        scope = Some(format!("SELECT * {{ {body} }}"));
        draw = Some(if rng.below(2) == 0 {
            value(call, GENERATED_KEYS)
        } else {
            filter(GENERATED_KEYS, false)
        });
        body = if matches!(draw, Some(Draw::Value { .. })) {
            format!("{{ {body} BIND($DRAW AS ?d) }} VALUES ?copy {{ 0 1 }}")
        } else {
            format!("{{ {body} FILTER($DRAW) }} VALUES ?copy {{ 0 1 }}")
        };
    } else if shape != "reduced" && rng.below(5) == 0 {
        // DISTINCT makes every projected tuple unique, so sorting its columns
        // makes the LIMIT deterministic.
        tail = " ORDER BY ?s ?o ?t ?tag ?g ?v ?w ?x ?z ?layer0 ?layer1 LIMIT 3".into();
        modifier = "DISTINCT ";
    }
    case.query = Query {
        text: format!("SELECT {modifier}* {{ {body} }}{tail}"),
        draw,
        scope,
        reduced: shape == "reduced",
    };
    case
}
