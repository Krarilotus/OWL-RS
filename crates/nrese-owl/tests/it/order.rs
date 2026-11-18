//! The reader's ontology is that of a set of statements: what it leaves out doesn't depend on
//! the order it meets them in.

use nrese_owl::fuzz::Rng;
use nrese_owl::{Diagnostic, Statement, Term, TermKind, Terms, read};
use nrese_rdf::BlankNode;

use super::{EX, Table};

const OWL: &str = "http://www.w3.org/2002/07/owl#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";

/// The kinds of diagnostic that leave a part of the input out ([`Diagnostic::leaves_out`]).
const KINDS: [&str; 5] = [
    "Reserved",
    "NotOwl",
    "Malformed",
    "BrokenList",
    "Unsupported",
];

fn kind(d: &Diagnostic) -> &'static str {
    match d {
        Diagnostic::Reserved { .. } => "Reserved",
        Diagnostic::NotOwl { .. } => "NotOwl",
        Diagnostic::Malformed { .. } => "Malformed",
        Diagnostic::BrokenList { .. } => "BrokenList",
        Diagnostic::Unsupported { .. } => "Unsupported",
        _ => "other",
    }
}

/// What `statements`, read in their order, leave out of kind `of`, as a set: each diagnostic
/// in words, its terms named.
fn left_out(statements: &[Statement], table: &Table, of: &str) -> Vec<String> {
    let mut found: Vec<String> = (read(statements, table).diagnostics.iter())
        .filter(|d| d.leaves_out() && kind(d) == of)
        .map(|d| d.describe(&|t| table.name(t)))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// `a` and `b` (one set in two orders) with every statement dropped whose absence keeps
/// kind `of` differing between them: a smallest pair of orders that shows it.
fn shrink(
    a: &[Statement],
    b: &[Statement],
    table: &Table,
    of: &str,
) -> (Vec<Statement>, Vec<Statement>) {
    let (mut a, mut b) = (a.to_vec(), b.to_vec());
    let mut i = 0;
    while i < a.len() {
        let without = |order: &[Statement]| -> Vec<Statement> {
            order.iter().copied().filter(|s| *s != a[i]).collect()
        };
        let (fewer_a, fewer_b) = (without(&a), without(&b));
        if left_out(&fewer_a, table, of) != left_out(&fewer_b, table, of) {
            (a, b) = (fewer_a, fewer_b);
        } else {
            i += 1;
        }
    }
    (a, b)
}

fn shown(statements: &[Statement], table: &Table) -> String {
    (statements.iter())
        .map(|s| format!("    {}\n", s.triple.map(|t| table.name(t)).join(" ")))
        .collect()
}

/// A random small graph: one `owl:equivalentClass` between two reserved names, the second
/// named again by a range, and one structure of each other kind left out (OWL vocabulary on a
/// name, a restriction without its property, a list without its rest, `owl:onProperties`);
/// in about half, a structure property given twice, one value a reserved name met nowhere else
/// and one a plain name (`owl:complementOf`, or a restriction's `owl:someValuesFrom`; `true`
/// with them); then up to twelve statements over names, reserved names and blank nodes, with
/// OWL's structure predicates.
fn graph(rng: &mut Rng, table: &mut Table) -> (Vec<Statement>, bool) {
    let mut iri = |ns: &str, local: &str| table.iri_id(&format!("{ns}{local}"));
    let [a, b, c, d, h, p, q] = ["A", "B", "C", "D", "H", "p", "q"].map(|l| iri(EX, l));
    let reserved = [
        iri(RDFS, "seeAlso"),
        iri(OWL, "deprecated"),
        iri(RDFS, "comment"),
        iri(RDF, "nil"),
        iri(OWL, "incompatibleWith"),
        iri(RDFS, "label"),
    ];
    let [ty, sub, equivalent, sub_property, domain, range] = [
        iri(RDF, "type"),
        iri(RDFS, "subClassOf"),
        iri(OWL, "equivalentClass"),
        iri(RDFS, "subPropertyOf"),
        iri(RDFS, "domain"),
        iri(RDFS, "range"),
    ];
    let [
        on_property,
        some,
        union,
        first,
        rest,
        on_properties,
        same,
        complement,
    ] = [
        iri(OWL, "onProperty"),
        iri(OWL, "someValuesFrom"),
        iri(OWL, "unionOf"),
        iri(RDF, "first"),
        iri(RDF, "rest"),
        iri(OWL, "onProperties"),
        iri(OWL, "sameAs"),
        iri(OWL, "complementOf"),
    ];
    let [e, defined_by] = [iri(EX, "E"), iri(RDFS, "isDefinedBy")];
    let types = [
        iri(OWL, "Class"),
        iri(OWL, "Restriction"),
        iri(OWL, "ObjectProperty"),
        iri(OWL, "AnnotationProperty"),
        iri(OWL, "AllDisjointClasses"),
        a,
    ];
    let blank = |table: &mut Table, l: &str| table.id(BlankNode::new_unchecked(l).into());
    let [r, u, l, n, pl, t] = ["r", "u", "l", "n", "pl", "t"].map(|x| blank(table, x));
    let blanks: Vec<u64> = (0..4).map(|i| blank(table, &format!("x{i}"))).collect();
    let mut triples = vec![
        [reserved[0], equivalent, reserved[1]],
        [q, range, reserved[1]],
        [h, on_property, p],
        [a, sub, r],
        [r, ty, types[1]],
        [b, sub, u],
        [u, union, l],
        [l, first, c],
        [d, sub, n],
        [n, ty, types[1]],
        [n, on_properties, pl],
    ];
    let nodes: Vec<u64> = [a, b, c, p, q]
        .into_iter()
        .chain(reserved)
        .chain(blanks)
        .collect();
    let predicates = [
        ty,
        sub,
        equivalent,
        sub_property,
        domain,
        range,
        on_property,
        some,
        union,
        first,
        rest,
        same,
        p,
    ];
    let pick = |rng: &mut Rng, from: &[u64]| from[rng.below(from.len() as u64) as usize];
    let twice = rng.one_in(2);
    if twice {
        let operator = if rng.one_in(2) {
            complement
        } else {
            triples.extend([[t, ty, types[1]], [t, on_property, p]]);
            some
        };
        triples.extend([[e, sub, t], [t, operator, defined_by], [t, operator, c]]);
    }
    for _ in 0..rng.below(13) {
        let predicate = pick(rng, &predicates);
        let object = if predicate == ty {
            pick(rng, &types)
        } else {
            pick(rng, &nodes)
        };
        triples.push([pick(rng, &nodes), predicate, object]);
    }
    let statements = (triples.into_iter())
        .map(|triple| Statement { triple, graph: 0 })
        .collect();
    (statements, twice)
}

/// `statements` in a random order (Fisher-Yates).
fn shuffled(statements: &[Statement], rng: &mut Rng) -> Vec<Statement> {
    let mut order = statements.to_vec();
    for i in (1..order.len()).rev() {
        order.swap(i, rng.below(i as u64 + 1) as usize);
    }
    order
}

/// Spec ([`read`]: the ontology of a set of statements, of any graphs): the diagnostics that
/// leave a part of the input out (a reserved name used as an entity, OWL vocabulary outside
/// the mapping, a malformed structure, a broken list, syntax not read yet) are the same set
/// in every order the statements come in. Random small graphs, each with an
/// `owl:equivalentClass` between two reserved names, the second named by another axiom too,
/// one structure of each other kind, and in about half a structure property given twice (one
/// value a reserved name, one a plain one), are each read in their order and in six seeded
/// others. `NRESE_FUZZ_SEED` (default 0) varies the graphs. A kind or shape the graphs never
/// reach fails the test, which would state nothing of it; a kind whose set differs is shown by the
/// fewest statements that still show it, in both orders.
#[test]
fn what_the_reader_leaves_out_is_the_same_in_every_order() {
    const GRAPHS: usize = 200;
    const ORDERS: usize = 6;
    let seed: u64 = std::env::var("NRESE_FUZZ_SEED")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    let mut rng = Rng::new(seed ^ 0x006f_7264_6572);
    let mut table = Table::default();
    let (mut reached, mut twice) = ([0usize; KINDS.len()], 0);
    let mut moved: Vec<(&str, String)> = Vec::new();
    for case in 0..GRAPHS {
        let (statements, given_twice) = graph(&mut rng, &mut table);
        twice += usize::from(given_twice);
        for (i, of) in KINDS.iter().enumerate() {
            reached[i] += usize::from(!left_out(&statements, &table, of).is_empty());
        }
        for _ in 0..ORDERS {
            let order = shuffled(&statements, &mut rng);
            for of in KINDS {
                if moved.iter().any(|(k, _)| *k == of)
                    || left_out(&statements, &table, of) == left_out(&order, &table, of)
                {
                    continue;
                }
                let (a, b) = shrink(&statements, &order, &table, of);
                moved.push((
                    of,
                    format!(
                        "{of} (graph {case}): {:?} in one order, {:?} in the other; read in\n{}  \
                         and in\n{}",
                        left_out(&a, &table, of),
                        left_out(&b, &table, of),
                        shown(&a, &table),
                        shown(&b, &table)
                    ),
                ));
            }
        }
    }
    eprintln!(
        "NRESE_FUZZ_SEED={seed}: {GRAPHS} graphs; with each kind: {KINDS:?} {reached:?}; with \
         a structure property given twice: {twice}"
    );
    assert!(
        reached.iter().all(|&n| n > 0) && twice > 0,
        "a kind or shape no graph reached: {KINDS:?} {reached:?}, given twice {twice}"
    );
    let moved: Vec<String> = moved.into_iter().map(|(_, m)| m).collect();
    assert!(moved.is_empty(), "{}", moved.join("\n"));
}

/// Names by ids as a source's dictionary may give them, any ids at all: every term an IRI.
struct Sparse(Vec<(Term, &'static str)>);

impl Terms for Sparse {
    fn kind(&self, _: Term) -> TermKind {
        TermKind::Iri
    }

    fn lexical(&self, _: Term) -> Option<String> {
        None
    }

    fn iri(&self, iri: &str) -> Option<Term> {
        self.0.iter().find(|(_, i)| *i == iri).map(|&(t, _)| t)
    }

    fn datatype(&self, _: Term) -> Option<String> {
        None
    }

    fn iri_text(&self, term: Term) -> Option<String> {
        self.0
            .iter()
            .find(|&&(t, _)| t == term)
            .map(|(_, i)| (*i).to_owned())
    }
}

/// Spec ([`Diagnostic::NotOwl`]: each statement of OWL vocabulary outside the mapping's forms
/// is a part left out): `ex:a owl:onProperty ex:c` and `ex:b owl:onProperty ex:d` are two,
/// whatever ids the source gives their terms; here ids whose mix, as one number, is the same
/// for both statements.
#[test]
fn two_statements_outside_the_mapping_are_two_parts_left_out() {
    let b = 4_398_046_511_105; // 2^42 + 1
    let terms = Sparse(vec![
        (1, "http://example.com/a"),
        (b, "http://example.com/b"),
        (2, "http://example.com/c"),
        (3, "http://example.com/d"),
        (5, "http://www.w3.org/2002/07/owl#onProperty"),
    ]);
    let statements = [[1, 5, 2], [b, 5, 3]].map(|triple| Statement { triple, graph: 0 });
    let diagnostics = read(&statements, &terms).diagnostics;
    let not_owl = (diagnostics.iter())
        .filter(|d| matches!(d, Diagnostic::NotOwl { .. }))
        .count();
    assert_eq!(not_owl, 2, "{diagnostics:?}");
}
