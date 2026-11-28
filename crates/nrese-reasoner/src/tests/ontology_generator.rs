/// A xorshift generator: `next(n)` is in `0..n`.
pub(crate) fn rng(seed: u64) -> impl FnMut(usize) -> usize {
    // Varied by `NRESE_FUZZ_SEED` (a number) for bug hunts over many seeds.
    let fuzz: u64 = std::env::var("NRESE_FUZZ_SEED")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    let mut state = (seed ^ fuzz.wrapping_mul(0x9e37_79b9_7f4a_7c15)).max(1);
    move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    }
}

/// A random small ontology (one triple per line) over a vocabulary that exercises every
/// rule table: hierarchies, property axioms and characteristics, restrictions,
/// cardinalities, equality, lists and the consistency rules.
pub(crate) fn random_ontology(next: &mut dyn FnMut(usize) -> usize) -> Vec<String> {
    ontology_with(next, |_, _, _| {})
}

/// [`random_ontology`], telling `witness` each choice between constructors. Every such choice
/// goes through `choose`: one drawn with a plain `next` is invisible to the family check.
pub(crate) fn ontology_with(
    next: &mut dyn FnMut(usize) -> usize,
    mut witness: impl FnMut(&'static str, usize, usize),
) -> Vec<String> {
    let mut choose = |next: &mut dyn FnMut(usize) -> usize, name, size| {
        let choice = next(size);
        witness(name, size, choice);
        choice
    };
    const CLASS_AXIOMS: [&str; 5] = [
        "rdfs:subClassOf",
        "owl:equivalentClass",
        "owl:disjointWith",
        "owl:complementOf",
        "rdfs:subClassOf",
    ];
    const PROPERTY_AXIOMS: [&str; 5] = [
        "rdfs:subPropertyOf",
        "owl:equivalentProperty",
        "owl:inverseOf",
        "owl:propertyDisjointWith",
        "rdfs:subPropertyOf",
    ];
    const CHARACTERISTICS: [&str; 7] = [
        "owl:TransitiveProperty",
        "owl:SymmetricProperty",
        "owl:FunctionalProperty",
        "owl:InverseFunctionalProperty",
        "owl:IrreflexiveProperty",
        "owl:AsymmetricProperty",
        "owl:ObjectProperty",
    ];
    const CARDINALITY: [&str; 2] = [
        "\"1\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger>",
        "\"0\"^^<http://www.w3.org/2001/XMLSchema#nonNegativeInteger>",
    ];
    let class = |n: usize| format!("ex:C{n}");
    let property = |n: usize| format!("ex:p{n}");
    let individual = |n: usize| format!("ex:i{n}");
    let mut lines: Vec<String> = Vec::new();
    let list = |lines: &mut Vec<String>, name: &str, members: [String; 2]| {
        let [a, b] = members;
        lines.push(format!("_:{name}a rdf:first {a}"));
        lines.push(format!("_:{name}a rdf:rest _:{name}b"));
        lines.push(format!("_:{name}b rdf:first {b}"));
        lines.push(format!("_:{name}b rdf:rest rdf:nil"));
        format!("_:{name}a")
    };
    for n in 0..(10 + next(30)) {
        let line = match choose(next, "statement", 12) {
            0 | 1 => format!(
                "{} {} {}",
                class(next(5)),
                CLASS_AXIOMS[choose(next, "class axiom", CLASS_AXIOMS.len())],
                class(next(5))
            ),
            2 => format!(
                "{} {} {}",
                property(next(4)),
                PROPERTY_AXIOMS[choose(next, "property axiom", PROPERTY_AXIOMS.len())],
                property(next(4))
            ),
            3 => {
                let axiom = ["rdfs:domain", "rdfs:range"][choose(next, "domain or range", 2)];
                format!("{} {axiom} {}", property(next(4)), class(next(5)))
            }
            4 => format!(
                "{} rdf:type {}",
                property(next(4)),
                CHARACTERISTICS[choose(next, "characteristic", CHARACTERISTICS.len())]
            ),
            5 | 6 => format!("{} rdf:type {}", individual(next(6)), class(next(5))),
            7 => {
                // A restriction: onProperty plus a filler.
                let c = class(next(5));
                lines.push(format!("{c} owl:onProperty {}", property(next(4))));
                match choose(next, "restriction", CARDINALITY.len() + 3) {
                    0 => format!("{c} owl:someValuesFrom {}", class(next(5))),
                    1 => format!("{c} owl:allValuesFrom {}", class(next(5))),
                    2 => format!("{c} owl:hasValue {}", individual(next(6))),
                    n => format!("{c} owl:maxCardinality {}", CARDINALITY[n - 3]),
                }
            }
            8 => {
                let relation = ["owl:sameAs", "owl:differentFrom", "rdf:type"]
                    [choose(next, "equality or bottom", 3)];
                if relation == "rdf:type" {
                    format!("{} rdf:type owl:Nothing", class(next(5)))
                } else {
                    format!("{} {relation} {}", individual(next(6)), individual(next(6)))
                }
            }
            9 => {
                let name = format!("l{n}");
                match choose(next, "list axiom", 4) {
                    0 => {
                        let head = list(&mut lines, &name, [class(next(5)), class(next(5))]);
                        format!("{} owl:intersectionOf {head}", class(next(5)))
                    }
                    1 => {
                        let head = list(&mut lines, &name, [class(next(5)), class(next(5))]);
                        format!("{} owl:unionOf {head}", class(next(5)))
                    }
                    2 => {
                        let members = [property(next(4)), property(next(4))];
                        let head = list(&mut lines, &name, members);
                        format!("{} owl:propertyChainAxiom {head}", property(next(4)))
                    }
                    _ => {
                        let head = list(&mut lines, &name, [property(next(4)), property(next(4))]);
                        format!("{} owl:hasKey {head}", class(next(5)))
                    }
                }
            }
            _ => format!(
                "{} {} {}",
                individual(next(6)),
                property(next(4)),
                individual(next(6))
            ),
        };
        lines.push(line);
    }
    lines
}
