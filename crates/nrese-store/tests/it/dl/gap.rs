//! The gap of Skolem constants (`skolem-only-gap`) against U1 itself, on random
//! ontologies with existentials: debug builds check every query that takes the path
//! (U1's answers without a Skolem constant are L's), so this test is many queries that do,
//! among many that mustn't.

use super::queries::query;
use super::{insert, pipeline};

/// xorshift64*: the same cases on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

const CLASSES: [&str; 4] = [":A", ":B", ":C", ":D"];
const PROPERTIES: [&str; 3] = [":p", ":q", ":r"];
const INDIVIDUALS: [&str; 4] = [":a", ":b", ":c", ":d"];
const VARIABLES: [&str; 3] = ["?x", "?y", "?z"];

/// Axioms with existentials (and what consumes their Skolem constants: inverses,
/// subproperties, `∃p.C ⊑ D`, ranges, a union that gives named facts), and data.
fn ontology(rng: &mut Rng) -> String {
    let mut out = String::new();
    for _ in 0..3 + rng.below(4) {
        let (c1, c2, c3) = (rng.pick(&CLASSES), rng.pick(&CLASSES), rng.pick(&CLASSES));
        let (p1, p2) = (rng.pick(&PROPERTIES), rng.pick(&PROPERTIES));
        out += &match rng.below(7) {
            0 | 1 => format!(
                "{c1} rdfs:subClassOf [ a owl:Restriction ; owl:onProperty {p1} ; \
                 owl:someValuesFrom {c2} ] . "
            ),
            2 => format!(
                "[ a owl:Restriction ; owl:onProperty {p1} ; owl:someValuesFrom {c1} ] \
                 rdfs:subClassOf {c2} . "
            ),
            3 if p1 != p2 => format!("{p1} rdfs:subPropertyOf {p2} . "),
            3 => format!("{c1} rdfs:subClassOf {c2} . "),
            4 if p1 != p2 => format!("{p1} owl:inverseOf {p2} . "),
            4 => format!("{p1} rdfs:range {c1} . "),
            5 => format!("{c1} rdfs:subClassOf [ owl:unionOf ( {c2} {c3} ) ] . "),
            _ => format!("{p1} rdfs:domain {c1} . "),
        };
    }
    for _ in 0..3 + rng.below(4) {
        let i = rng.pick(&INDIVIDUALS);
        out += &match rng.below(2) {
            0 => format!("{i} a {} . ", rng.pick(&CLASSES)),
            _ => format!(
                "{i} {} {} . ",
                rng.pick(&PROPERTIES),
                rng.pick(&INDIVIDUALS)
            ),
        };
    }
    out
}

/// One to three atoms over variables and individuals, some variables projected.
fn random_query(rng: &mut Rng) -> String {
    let term = |rng: &mut Rng| match rng.below(3) {
        0 => rng.pick(&INDIVIDUALS),
        _ => rng.pick(&VARIABLES),
    };
    let mut atoms = Vec::new();
    for _ in 0..1 + rng.below(3) {
        let s = term(rng);
        atoms.push(match rng.below(2) {
            0 => format!("{s} a {}", rng.pick(&CLASSES)),
            _ => format!("{s} {} {}", rng.pick(&PROPERTIES), term(rng)),
        });
    }
    let body = atoms.join(" . ");
    let used: Vec<&str> = VARIABLES
        .iter()
        .copied()
        .filter(|v| body.contains(v))
        .collect();
    let projected: Vec<&str> = used.iter().copied().filter(|_| rng.below(3) > 0).collect();
    match projected.is_empty() {
        true => format!("ASK {{ {body} }}"),
        false => format!("SELECT DISTINCT {} {{ {body} }}", projected.join(" ")),
    }
}

#[test]
fn the_skolem_gap_path_gives_u1s_answers_on_random_ontologies() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let (mut taken, mut queries) = (0, 0);
    for _ in 0..40 {
        let dl = pipeline();
        insert(&dl, &ontology(&mut rng)).expect("consistent: no disjointness");
        for _ in 0..25 {
            let q = random_query(&mut rng);
            let (_, status) = query(&dl, &q);
            assert!(status.shared.sound, "{q}");
            taken += usize::from(status.paths.contains(&"skolem-only-gap"));
            queries += 1;
        }
    }
    // Debug builds checked each of them against U1.
    assert!(
        taken >= queries / 20,
        "the path was taken {taken} times in {queries} queries"
    );
}
