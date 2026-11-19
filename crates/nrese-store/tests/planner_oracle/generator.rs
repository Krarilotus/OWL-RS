use crate::routes::ENDPOINT;
use crate::twin::{Draw, Query};

#[derive(Clone)]
pub struct Case {
    // A generated case's shape is the place where it draws ("none" if it
    // doesn't); its labels add the places around the draw.
    pub shape: &'static str,
    pub labels: Vec<&'static str>,
    pub data: Vec<String>,
    pub query: Query,
    pub remote: Option<&'static str>,
    pub vector: bool,
}

impl Case {
    pub fn new(shape: &'static str, data: Vec<String>, query: Query) -> Self {
        Self {
            shape,
            labels: vec![shape],
            data,
            query,
            remote: None,
            vector: false,
        }
    }
}

const LABEL: &str = "<http://www.w3.org/2000/01/rdf-schema#label>";
const VALUES: &str = "VALUES ?s { <urn:a> <urn:a> <urn:b> }";
const FEW: &str = "?s <urn:few> ?o";
const VECTOR: &str = "urn:nrese:vector:";
pub const REMOTE: &str = "<urn:a> <urn:remote> 1 .";
// Beside a drawing AVG: a volatile argument whose branches are both 1, so
// its mean is bound and 1 on every run and route, whatever is drawn; and
// each group's size, the arguments the drawing mean needs all to be 1.
pub const BOUND: &str = "(AVG(IF(RAND() < 0.5, 1, 1)) AS ?bound) (COUNT(*) AS ?rows)";

// Where a generated query draws. "none" draws nothing and is compared as a bag.
pub const PLACES: &[&str] = &[
    "none",
    "none",
    "outer",
    "nested",
    "exists",
    "lateral",
    "path",
    "aggregate",
    "service",
    "vector",
];

// Whether the engine under test has a vector index; a tree without one
// skips the seeds that search it. And whether its EXPLAIN reports rewrites;
// without either, the cells that need it say they are unsupported.
pub const VECTOR_INDEX: bool = true;
pub const EXPLAIN_REWRITES: bool = true;

pub fn keys(n: usize) -> String {
    (0..n).map(|i| i.to_string()).collect::<Vec<_>>().join(" ")
}

pub fn value(call: &'static str, keys: &'static [&'static str]) -> Draw {
    Draw::Value {
        call,
        column: "d",
        keys,
    }
}

pub fn filter(keys: &'static [&'static str]) -> Draw {
    Draw::Filter { keys }
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
            filter(&[]),
        ),
        // The group's one OPTIONAL pair is decided by one draw, which the join
        // copies to every key: FILTER(BOUND(?o)) keeps all forty rows or none.
        "optional-condition" => (
            format!("SELECT ?k {{ VALUES ?k {{ {many} }} {{ <urn:a> {LABEL} ?l OPTIONAL {{ <urn:a> {LABEL} ?o FILTER($DRAW) }} }} FILTER(BOUND(?o)) }}"),
            filter(&[]),
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
                filter(&[]),
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
                filter(&[]),
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
            filter(&["i"]),
        ),
        "lateral-draw" => (
            "SELECT ?i ?d { VALUES (?s ?i) { (<urn:a> 1) (<urn:a> 2) } LATERAL { ?s <urn:few> ?o BIND($DRAW AS ?d) } }".into(),
            value("UUID()", &["i"]),
        ),
        "path-draw" => {
            data.extend(["<urn:a> <urn:step> <urn:b> .".into(), "<urn:b> <urn:step> <urn:c> .".into()]);
            (
                "SELECT ?s ?o { VALUES ?s { <urn:a> <urn:a> } { ?s <urn:step>+ ?o FILTER($DRAW) } }".into(),
                filter(&["s", "o"]),
            )
        }
        "vector-draw" => {
            vectors(&mut data, 120);
            (
                format!("SELECT ?s ?d {{ {{ ?s <urn:embedding> ?v . {{ BIND($DRAW AS ?d) }} }} {} }}", vector_search()),
                value("BNODE()", &[]),
            )
        }
        // The group shares no variable with the hit: it is evaluated once,
        // and each of its solutions joins both hits with one blank node.
        "vector-disjoint" => {
            vectors(&mut data, 120);
            (
                format!("SELECT ?x ?w ?v ?d {{ {{ ?x <urn:embedding> ?w BIND($DRAW AS ?d) }} {} }}", vector_search()),
                value("BNODE()", &["x", "w"]),
            )
        }
        "service-batches" => (
            format!("SELECT ?k ?d {{ VALUES ?k {{ {} }} SERVICE <{ENDPOINT}> {{ {{ <urn:a> <urn:remote> ?z BIND($DRAW AS ?d) }} UNION {{ ?k <urn:missing> ?z BIND($OTHER AS ?d) }} }} }}", keys(201)),
            value("UUID()", &[]),
        ),
        "average-draw" => (
            format!("SELECT (AVG($DRAW) AS ?avg) {BOUND} {{ ?s <urn:few> ?o . ?s <urn:q> ?z }}"),
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

pub fn vectors(data: &mut Vec<String>, size: usize) {
    for i in 0..size {
        let vector = nrese_engine::vector::lexical(&[1.0, i as f32 / 100.0]);
        data.push(format!(
            "<urn:vec:{i}> <urn:embedding> \"{vector}\"^^<{VECTOR}vector> ."
        ));
    }
}

pub fn vector_search() -> String {
    let vector = nrese_engine::vector::lexical(&[1.0, 0.0]);
    format!(
        "SERVICE <{VECTOR}search> {{ ?v <{VECTOR}near> \"{vector}\"^^<{VECTOR}vector> ; <{VECTOR}k> 2 ; <{VECTOR}exact> true }}"
    )
}

pub struct Rng(pub u64);

impl Rng {
    pub fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut x = self.0;
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        ((x ^ (x >> 31)) % n as u64) as usize
    }
}
