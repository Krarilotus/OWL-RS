use std::collections::BTreeSet;

use crate::{Facts, Triple, triple};

#[derive(Clone, Debug, Default)]
pub struct Write {
    pub insert: Vec<Triple>,
    pub delete: Vec<Triple>,
}

impl Write {
    pub fn after(&self, before: &Facts) -> Facts {
        let mut after = before.clone();
        for fact in &self.delete {
            after.remove(fact);
        }
        after.extend(self.insert.iter().cloned());
        after
    }
}

#[derive(Clone)]
pub struct Case {
    pub shape: &'static str,
    pub writes: Vec<Write>,
    pub watch: Triple,
}

pub fn rows(text: &str) -> Vec<Triple> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut terms = line.split_whitespace();
            let fact = std::array::from_fn(|_| terms.next().unwrap());
            assert!(terms.next().is_none(), "{line}");
            triple(fact)
        })
        .collect()
}

pub fn insert(text: &str) -> Write {
    Write {
        insert: rows(text),
        ..Write::default()
    }
}

pub fn delete(text: &str) -> Write {
    Write {
        delete: rows(text),
        ..Write::default()
    }
}

pub fn case(shape: &'static str, watch: [&str; 3], writes: Vec<Write>) -> Case {
    Case {
        shape,
        watch: triple(watch),
        writes,
    }
}

// Xorshift and the bounded coverage extension follow the reasoner's seed harness.
fn rng(seed: u64) -> impl FnMut(usize) -> usize {
    let mut state = (seed ^ 0x9e37_79b9_7f4a_7c15).max(1);
    move |n| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    }
}

pub fn draws_on(drawn: u64, cases: u64, covered: bool) -> bool {
    drawn < cases || (!covered && drawn < cases.saturating_mul(10))
}

pub const RED_SHAPES: &[&str] = &[
    "redundant",
    "cycle",
    "schema-insert",
    "schema-delete",
    "equality-member",
    "long-chain",
    "equality-schema",
    "literal-functional",
];

pub const SHAPES: &[&str] = &[
    "redundant",
    "cycle",
    "schema-insert",
    "schema-delete",
    "equality-member",
    "long-chain",
    "equality-schema",
    "literal-functional",
    "branch",
    "shared-tail",
    "missing-first",
    "nil-head",
    "nil-cell",
    "cell-alias",
    "head-alias",
    "member-alias",
    "first-subproperty",
    "rest-subproperty",
    "literal-value",
    "literal-distinct",
];

const CHAIN: &str = "
    ex:out owl:propertyChainAxiom ex:h
    ex:h rdf:first ex:p
    ex:h rdf:rest ex:t
    ex:t rdf:first ex:q
    ex:t rdf:rest rdf:nil
    ex:a ex:p ex:b
    ex:b ex:q ex:z
";

pub fn long_chain(length: usize) -> Case {
    let mut setup = insert("ex:out owl:propertyChainAxiom ex:c0");
    let mut data = Write::default();
    for index in 0..length {
        let cell = format!("ex:c{index}");
        let next = index + 1;
        let rest = if next == length {
            "rdf:nil".to_owned()
        } else {
            format!("ex:c{next}")
        };
        setup.insert.push(triple([&cell, "rdf:first", "ex:p"]));
        setup.insert.push(triple([&cell, "rdf:rest", &rest]));
        data.insert.push(triple([
            &format!("ex:x{index}"),
            "ex:p",
            &format!("ex:x{next}"),
        ]));
    }
    case(
        "long-chain",
        ["ex:x0", "ex:out", &format!("ex:x{length}")],
        vec![setup, data],
    )
}

pub fn redundant(branches: usize) -> Case {
    let mut setup = insert(
        "
        ex:out owl:propertyChainAxiom ex:h
        ex:h rdf:first ex:p
        ex:h rdf:rest ex:t
        ex:t rdf:first ex:q
        ex:t rdf:rest rdf:nil
        ex:h ex:part ex:extra
        ex:part rdfs:subPropertyOf ex:early
        ex:early rdfs:subPropertyOf ex:middle
        ex:middle rdfs:subPropertyOf rdf:first
        ex:a ex:extra ex:b
        ex:b ex:q ex:z
    ",
    );
    for branch in 0..branches {
        setup
            .insert
            .push(triple(["ex:h", "ex:early", &format!("ex:v{branch}")]));
    }
    let redundant = "ex:part rdfs:subPropertyOf rdf:first";
    case(
        "redundant",
        ["ex:a", "ex:out", "ex:z"],
        vec![setup, insert(redundant), delete(redundant)],
    )
}

pub const INTEGER: &str = "\"1\"^^xsd:integer";
pub const DECIMAL: &str = "\"1.0\"^^xsd:decimal";

pub fn schema_operand(insert_edge: bool) -> Case {
    let mut setup = insert(
        "
        ex:out owl:propertyChainAxiom ex:h
        ex:h rdf:first rdfs:subClassOf
        ex:h rdf:rest ex:t
        ex:t rdf:first ex:q
        ex:t rdf:rest rdf:nil
        ex:b ex:q ex:z
    ",
    );
    let edge = "ex:a rdfs:subClassOf ex:b";
    let (shape, change) = if insert_edge {
        ("schema-insert", insert(edge))
    } else {
        setup.insert.extend(rows(edge));
        ("schema-delete", delete(edge))
    };
    case(shape, ["ex:a", "ex:out", "ex:z"], vec![setup, change])
}

fn literal(shape: &'static str, left: &str, right: &str, flip: bool) -> Case {
    let mut setup = insert(&format!("ex:a ex:p {left}\nex:a ex:p {right}"));
    if shape != "literal-value" {
        setup
            .insert
            .push(triple(["ex:p", "rdf:type", "owl:FunctionalProperty"]));
    }
    let (written, read) = if flip { (right, left) } else { (left, right) };
    let later = format!("ex:b ex:q {written}");
    case(
        shape,
        ["ex:b", "ex:q", read],
        vec![setup, insert(&later), delete(&later)],
    )
}

pub fn generated(seed: u64, plain: bool) -> Case {
    let mut next = rng(seed);
    if plain {
        return plain_case(&mut next);
    }
    let shape = SHAPES[(seed % SHAPES.len() as u64) as usize];
    let make_chain = |writes| case(shape, ["ex:a", "ex:out", "ex:z"], writes);
    let mut candidate = match shape {
        "redundant" => redundant(64 + next(4)),
        "cycle" => make_chain(vec![
            insert(CHAIN),
            insert("ex:t rdf:rest ex:h"),
            Write {
                delete: rows("ex:b ex:q ex:z"),
                insert: rows("ex:u rdf:type ex:Marker"),
            },
        ]),
        "schema-insert" => schema_operand(true),
        "schema-delete" => schema_operand(false),
        "equality-member" => make_chain(vec![
            insert(
                "
                ex:out owl:propertyChainAxiom ex:h
                ex:out owl:propertyDisjointWith ex:q
                ex:h rdf:first owl:sameAs
                ex:h rdf:rest ex:t
                ex:t rdf:first ex:q
                ex:t rdf:rest rdf:nil
            ",
            ),
            insert("ex:a ex:q ex:z"),
        ]),
        "long-chain" => long_chain(251 + next(3)),
        "equality-schema" => case(
            shape,
            ["ex:a", "ex:p", "ex:b"],
            vec![
                insert("ex:a ex:p ex:b"),
                insert("owl:sameAs rdf:type owl:IrreflexiveProperty"),
            ],
        ),
        "literal-functional" | "literal-value" => literal(shape, INTEGER, DECIMAL, next(2) == 0),
        "literal-distinct" => literal(shape, INTEGER, "\"1.25\"^^xsd:decimal", next(2) == 0),
        "branch" => make_chain(vec![
            insert(CHAIN),
            insert("ex:t rdf:first ex:p\nex:h rdf:rest rdf:nil"),
            delete("ex:t rdf:first ex:q"),
        ]),
        "shared-tail" => make_chain(vec![
            insert(CHAIN),
            insert("ex:other owl:propertyChainAxiom ex:k\nex:k rdf:first ex:p\nex:k rdf:rest ex:t"),
            delete("ex:out owl:propertyChainAxiom ex:h"),
        ]),
        "missing-first" => make_chain(vec![
            insert(CHAIN),
            delete("ex:t rdf:first ex:q"),
            insert("ex:t rdf:first ex:q"),
        ]),
        "nil-head" => make_chain(vec![
            insert(&CHAIN.replace("ex:h", "rdf:nil")),
            delete("rdf:nil rdf:first ex:p"),
        ]),
        "nil-cell" => make_chain(vec![
            insert(&CHAIN.replace("ex:t", "rdf:nil")),
            delete("rdf:nil rdf:rest rdf:nil"),
        ]),
        "head-alias" | "cell-alias" | "member-alias" => {
            let (term, line) = match shape {
                "head-alias" => ("ex:h", "ex:out owl:propertyChainAxiom ex:h"),
                "cell-alias" => ("ex:t", "ex:h rdf:rest ex:t"),
                _ => ("ex:q", "ex:b ex:q ex:z"),
            };
            let setup = CHAIN.replace(line, &line.replace(term, "ex:alias"));
            let equality = format!("ex:alias owl:sameAs {term}");
            make_chain(vec![insert(&setup), insert(&equality), delete(&equality)])
        }
        "first-subproperty" | "rest-subproperty" => {
            let (predicate, atom) = if shape == "first-subproperty" {
                ("rdf:first", "ex:h rdf:first ex:p")
            } else {
                ("rdf:rest", "ex:h rdf:rest ex:t")
            };
            let setup = CHAIN.replace(atom, &atom.replace(predicate, "ex:part"));
            let schema = format!("ex:part rdfs:subPropertyOf {predicate}");
            make_chain(vec![insert(&setup), insert(&schema), delete(&schema)])
        }
        _ => unreachable!("{shape}"),
    };

    // Vary encounter order and IRI names while preserving the selected shape.
    let tag = next(997);
    let rename = |fact: &mut Triple| {
        for term in fact {
            if let Some(local) = term.strip_prefix("ex:") {
                *term = format!("ex:g{tag}_{local}");
            }
        }
    };
    rename(&mut candidate.watch);
    for write in &mut candidate.writes {
        for fact in write.insert.iter_mut().chain(&mut write.delete) {
            rename(fact);
        }
        if next(2) == 0 {
            write.insert.reverse();
        }
    }
    candidate
}

fn plain_case(next: &mut impl FnMut(usize) -> usize) -> Case {
    // Independent ordinary axioms and data selected across Tables 4–9. Each list
    // has one well-formed walk. Reserved terms occur only in their schema roles.
    let patterns = [
        "ex:A owl:equivalentClass ex:B\nex:B rdfs:subClassOf ex:C\nex:a rdf:type ex:A",
        "ex:p rdfs:domain ex:A\nex:p rdfs:range ex:B\nex:a ex:p ex:b",
        "ex:p owl:equivalentProperty ex:q\nex:q owl:inverseOf ex:r\nex:a ex:p ex:b",
        "ex:p rdf:type owl:TransitiveProperty\nex:a ex:p ex:b\nex:b ex:p ex:z",
        "ex:p rdf:type owl:SymmetricProperty\nex:a ex:p ex:b",
        "ex:p rdf:type owl:FunctionalProperty\nex:a ex:p ex:b\nex:a ex:p ex:z",
        "ex:p rdf:type owl:InverseFunctionalProperty\nex:a ex:p ex:z\nex:b ex:p ex:z",
        "ex:A owl:disjointWith ex:B\nex:a rdf:type ex:A\nex:b rdf:type ex:B",
        "ex:A owl:complementOf ex:B\nex:a rdf:type ex:A\nex:b rdf:type ex:B",
        "ex:R owl:someValuesFrom ex:B\nex:R owl:onProperty ex:p\nex:b rdf:type ex:B\nex:a ex:p ex:b",
        "ex:R owl:allValuesFrom ex:B\nex:R owl:onProperty ex:p\nex:a rdf:type ex:R\nex:a ex:p ex:b",
        "ex:R owl:hasValue ex:b\nex:R owl:onProperty ex:p\nex:a rdf:type ex:R",
        "ex:R owl:maxCardinality \"1\"^^xsd:nonNegativeInteger\nex:R owl:onProperty ex:p\nex:a rdf:type ex:R\nex:a ex:p ex:b\nex:a ex:p ex:z",
        "ex:A owl:oneOf ex:l\nex:l rdf:first ex:a\nex:l rdf:rest rdf:nil",
        "ex:A owl:unionOf ex:l\nex:l rdf:first ex:B\nex:l rdf:rest rdf:nil\nex:a rdf:type ex:B",
        "ex:A owl:intersectionOf ex:l\nex:l rdf:first ex:B\nex:l rdf:rest rdf:nil\nex:a rdf:type ex:B",
        "ex:A owl:hasKey ex:l\nex:l rdf:first ex:p\nex:l rdf:rest rdf:nil\nex:a rdf:type ex:A\nex:b rdf:type ex:A\nex:a ex:p ex:z\nex:b ex:p ex:z",
        "ex:ax rdf:type owl:AllDifferent\nex:ax owl:distinctMembers ex:l\nex:l rdf:first ex:a\nex:l rdf:rest ex:m\nex:m rdf:first ex:b\nex:m rdf:rest rdf:nil",
        "ex:p rdfs:range xsd:integer\nex:a ex:p \"7\"^^xsd:integer",
        CHAIN,
    ];
    let mut selected = BTreeSet::new();
    let count = 1 + next(3);
    while selected.len() < count {
        selected.insert(next(patterns.len()));
    }
    let mut setup = Write::default();
    for (ordinal, pattern) in selected.into_iter().enumerate() {
        setup.insert.extend(rows(
            &patterns[pattern].replace("ex:", &format!("ex:p{ordinal}_")),
        ));
    }
    // Edits are ordinary data assertions; avoid changing a list's shape on plain seeds.
    let mut writes = vec![setup];
    for index in 1..=next(4) {
        let addition = format!("ex:sample{index} ex:out ex:z");
        writes.push(if index == 3 {
            delete("ex:sample1 ex:out ex:z")
        } else {
            insert(&addition)
        });
    }
    case("plain", ["ex:sample1", "ex:out", "ex:z"], writes)
}
