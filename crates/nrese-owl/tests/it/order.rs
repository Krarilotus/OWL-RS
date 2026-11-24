//! The reader's ontology is that of a set of statements: what it leaves out doesn't depend on
//! the order it meets them in.

use nrese_owl::fuzz::Rng;
use nrese_owl::{Diagnostic, Statement, Term, TermKind, Terms, read};
use nrese_rdf::{BlankNode, Literal, NamedNode};

use super::{EX, Table, XSD, load, rendered};

const OWL: &str = "http://www.w3.org/2002/07/owl#";
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";

/// The kinds of diagnostic that leave a part of the input out ([`Diagnostic::leaves_out`]),
/// and those that break the rules' premise: a name of two kinds and a structure no axiom uses
/// that the rules may read more of ([`Diagnostic::reads_otherwise`] by their treatment).
const KINDS: [&str; 7] = [
    "Reserved",
    "NotOwl",
    "Malformed",
    "BrokenList",
    "Unsupported",
    "TwoKinds",
    "Unused",
];

fn kind(d: &Diagnostic) -> &'static str {
    match d {
        Diagnostic::Reserved { .. } => "Reserved",
        Diagnostic::NotOwl { .. } => "NotOwl",
        Diagnostic::Malformed { .. } => "Malformed",
        Diagnostic::BrokenList { .. } => "BrokenList",
        Diagnostic::Unsupported { .. } => "Unsupported",
        Diagnostic::TwoKinds { .. } => "TwoKinds",
        Diagnostic::Unused {
            reads_more: true, ..
        } => "Unused",
        Diagnostic::Unused {
            reads_more: false, ..
        } => "Unused, saying nothing",
        Diagnostic::SharedBlankNode { .. } => "SharedBlankNode",
        Diagnostic::UndeclaredProperty { .. } => "UndeclaredProperty",
        Diagnostic::NonSimpleProperty { .. } => "NonSimpleProperty",
        Diagnostic::IrregularPropertyHierarchy { .. } => "IrregularPropertyHierarchy",
        Diagnostic::AnonymousIndividual { .. } => "AnonymousIndividual",
        Diagnostic::Syntax { .. } => "Syntax",
        Diagnostic::RepeatedOperand => "RepeatedOperand",
        Diagnostic::Rule { .. } => "Rule",
    }
}

/// `d` in words, its terms named; a name of two kinds with whether an axiom reads it.
fn words(d: &Diagnostic, table: &Table) -> String {
    let described = d.describe(&|t| table.name(t));
    match d {
        Diagnostic::TwoKinds { by_axiom, .. } => format!("{described} (by axiom: {by_axiom})"),
        _ => described,
    }
}

/// What `statements`, read in their order, report of kind `of`, as a set: each diagnostic
/// in words, its terms named.
fn left_out(statements: &[Statement], table: &Table, of: &str) -> Vec<String> {
    let mut found: Vec<String> = (read(statements, table).diagnostics.iter())
        .filter(|d| kind(d) == of)
        .map(|d| words(d, table))
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
/// with them); in about a quarter each, a datatype restriction with a facet reached only through
/// an anonymous individual's class assertion, its datatype complement given twice (1), valid
/// OWL 2 DL (2), or its facet node's one statement a class assertion, not a facet (3); a name
/// of two kinds a domain reads, and a structure no axiom uses with two forms; then up
/// to twelve statements over names, reserved names and blank nodes, with OWL's structure
/// predicates.
fn graph(rng: &mut Rng, table: &mut Table) -> (Vec<Statement>, bool, u64) {
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
    let [tk, intersection, all] = [
        iri(EX, "tk"),
        iri(OWL, "intersectionOf"),
        iri(OWL, "allValuesFrom"),
    ];
    let data_restriction = [
        iri(EX, "dp"),
        iri(OWL, "DatatypeProperty"),
        iri(OWL, "allValuesFrom"),
        iri(OWL, "datatypeComplementOf"),
        iri(OWL, "onDatatype"),
        iri(OWL, "withRestrictions"),
        iri(XSD, "string"),
        iri(XSD, "integer"),
        iri(XSD, "minInclusive"),
        iri(RDFS, "Datatype"),
    ];
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
    // 1: its datatype complement given twice; 2: valid OWL 2 DL; 3: no facet on the facet node.
    let facets = rng.below(4);
    if facets > 0 {
        let [
            dp,
            data_property,
            all,
            complement,
            on_datatype,
            with,
            string,
            integer,
            min,
            datatype,
        ] = data_restriction;
        let zero =
            Literal::new_typed_literal("0", NamedNode::new_unchecked(format!("{XSD}integer")));
        let zero = table.id(zero.into());
        let [i, cx, rg, fl, f] = ["i", "cx", "rg", "fl", "f"].map(|x| blank(table, x));
        let facet = [[f, min, zero], [f, ty, a]][usize::from(facets == 3)];
        triples.extend([
            [dp, ty, data_property],
            [i, ty, cx],
            [cx, on_property, dp],
            [cx, all, rg],
            [rg, on_datatype, integer],
            [rg, with, fl],
            [fl, first, f],
            [fl, rest, reserved[3]],
            facet,
        ]);
        triples.extend(match facets {
            1 => [[rg, complement, string], [rg, complement, integer]],
            _ => [[cx, ty, types[1]], [rg, ty, datatype]],
        });
    }
    // A name of two kinds an axiom reads: `:tk`'s domain makes it an object property, its
    // literal value a data property. A structure no axiom uses with two forms, an
    // intersection and a restriction, which the rules may read more of.
    let x = table.id(Literal::new_simple_literal("x").into());
    let [w, wl, wl2] = ["w", "wl", "wl2"].map(|l| blank(table, l));
    triples.extend([
        [tk, domain, c],
        [a, tk, x],
        [b, tk, c],
        [w, intersection, wl],
        [wl, first, a],
        [wl, rest, wl2],
        [wl2, first, b],
        [wl2, rest, reserved[3]],
        [w, on_property, p],
        [w, all, d],
    ]);
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
    (statements, twice, facets)
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
/// the mapping, a malformed structure, a broken list, syntax not read yet) and those that break
/// the rules' premise (a name of two kinds, whether an axiom reads it; a structure no axiom
/// uses, whether the rules may read more of it) are the same set
/// in every order the statements come in. Random small graphs, each with an
/// `owl:equivalentClass` between two reserved names, the second named by another axiom too,
/// one structure of each other kind, a name of two kinds a domain reads, a structure no axiom
/// uses with two forms, in about half a structure property given twice (one
/// value a reserved name, one a plain one), and in about a quarter each a datatype restriction
/// with a facet reached only through an anonymous individual, its datatype complement given
/// twice, valid OWL 2 DL, or its facet node's statement not a facet, are each read in their
/// order and in six seeded others.
/// `NRESE_FUZZ_SEED` (default 0) varies the graphs. A kind or shape the graphs never reach
/// fails the test, which would state nothing of it; a kind whose set differs is shown by the
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
    let (mut reached, mut twice, mut facets) = ([0usize; KINDS.len()], 0, [0usize; 4]);
    let mut moved: Vec<(&str, String)> = Vec::new();
    for case in 0..GRAPHS {
        let (statements, given_twice, with_facets) = graph(&mut rng, &mut table);
        twice += usize::from(given_twice);
        facets[with_facets as usize] += 1;
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
         a structure property given twice: {twice}; with a facet through an anonymous \
         individual, [none, its complement given twice, valid, not a facet]: {facets:?}"
    );
    assert!(
        reached.iter().all(|&n| n > 0) && twice > 0 && facets.iter().all(|&n| n > 0),
        "a kind or shape no graph reached: {KINDS:?} {reached:?}, given twice {twice}, facets \
         {facets:?}"
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

/// What `statements`, read in their order, leave out (in words) and the axioms they keep.
fn reading(statements: &[Statement], table: &Table) -> (Vec<String>, Vec<String>) {
    let ontology = read(statements, table);
    let mut left: Vec<String> = (ontology.diagnostics.iter())
        .filter(|d| d.leaves_out())
        .map(|d| d.describe(&|t| table.name(t)))
        .collect();
    left.sort();
    (left, rendered(&ontology, table))
}

/// Spec ([`read`]): a datatype restriction reached only through an anonymous individual's class
/// assertion, its datatype complement given twice, is read the same in either order of its
/// statements, the facet's statement first or last: the same parts left out, the same axioms
/// kept; with a reserved name as the facet and with a plain property.
#[test]
fn a_datatype_restriction_beside_a_doubled_complement_reads_the_same_in_either_order() {
    for facet in ["xsd:minInclusive", "ex:q"] {
        let mut table = Table::default();
        let statements = load(
            &mut table,
            &format!(
                "ex:p a owl:DatatypeProperty .
                _:i a _:c .
                _:c owl:onProperty ex:p ; owl:allValuesFrom _:r .
                _:r owl:datatypeComplementOf xsd:string, xsd:integer ;
                    owl:onDatatype xsd:integer ; owl:withRestrictions ( _:f ) .
                _:f {facet} 0 ."
            ),
        );
        let reversed: Vec<Statement> = statements.iter().rev().copied().collect();
        assert_eq!(
            reading(&statements, &table),
            reading(&reversed, &table),
            "{facet}: as written, then reversed"
        );
    }
}

/// Spec ([`read`]): valid OWL 2 DL, `ClassAssertion(DataAllValuesFrom(ex:p
/// DatatypeRestriction(xsd:integer xsd:minInclusive 0)) _:i)`, whose datatype restriction is
/// reached only through the anonymous individual's class assertion, is read the same in either
/// order of its statements, the facet's statement first or last: nothing left out, the same
/// axioms kept. Each order's reading is printed.
#[test]
fn a_datatype_restriction_reached_by_an_anonymous_individual_reads_the_same_in_either_order() {
    let mut table = Table::default();
    let statements = load(
        &mut table,
        "ex:p a owl:DatatypeProperty .
        _:i a _:c .
        _:c a owl:Restriction ; owl:onProperty ex:p ; owl:allValuesFrom _:r .
        _:r a rdfs:Datatype ; owl:onDatatype xsd:integer ; owl:withRestrictions ( _:f ) .
        _:f xsd:minInclusive 0 .",
    );
    let reversed: Vec<Statement> = statements.iter().rev().copied().collect();
    let [written, backwards] = [&statements, &reversed].map(|order| reading(order, &table));
    eprintln!(
        "VALID as written: left out {:?}; kept {:?}",
        written.0, written.1
    );
    eprintln!(
        "VALID reversed: left out {:?}; kept {:?}",
        backwards.0, backwards.1
    );
    assert_eq!(written, backwards, "as written, then reversed");
    assert!(
        written.0.is_empty(),
        "valid input, nothing left out: {:?}",
        written.0
    );
}

/// Spec (a facet restriction is a facet and its value, the facets those of the OWL 2 datatype
/// map, *Structural Specification* §4): in the valid shape above, a facet node whose one
/// statement isn't a facet
/// is a malformed datatype restriction, reported in either order of the statements, and the
/// statement is read as itself: `_:f a ex:A` a class assertion, `_:f ex:q 0` a data property
/// assertion.
#[test]
fn a_facet_node_without_a_facet_reads_its_statement_as_itself_in_either_order() {
    for (statement, kept) in [
        ("a ex:A", "ClassAssertion(ex:A _:"),
        ("ex:q 0", "DataPropertyAssertion(ex:q _:"),
    ] {
        let mut table = Table::default();
        let statements = load(
            &mut table,
            &format!(
                "ex:p a owl:DatatypeProperty . ex:q a owl:DatatypeProperty . ex:A a owl:Class .
                _:i a _:c .
                _:c a owl:Restriction ; owl:onProperty ex:p ; owl:allValuesFrom _:r .
                _:r a rdfs:Datatype ; owl:onDatatype xsd:integer ; owl:withRestrictions ( _:f ) .
                _:f {statement} ."
            ),
        );
        let reversed: Vec<Statement> = statements.iter().rev().copied().collect();
        let [written, backwards] = [&statements, &reversed].map(|order| reading(order, &table));
        assert_eq!(written, backwards, "_:f {statement}: either order");
        let (left, axioms) = written;
        assert!(
            (left.iter()).any(|d| d.starts_with("a facet restriction without a facet: ")),
            "_:f {statement}: left out {left:?}"
        );
        assert!(
            axioms.iter().any(|a| a.starts_with(kept)),
            "_:f {statement}: kept {axioms:?}"
        );
    }
}

/// Spec (*Mapping to RDF Graphs* Table 12: a facet restriction is a blank node): a datatype
/// restriction whose facet node is named (`ex:f xsd:minInclusive 0`, alone in its list or
/// before a facet node that isn't a facet) is malformed, reported in either order of the
/// statements, the same parts left out and the same axioms kept. Each order's reading printed.
#[test]
fn a_facet_restriction_on_a_named_node_reads_the_same_in_either_order() {
    for list in ["ex:f", "ex:f _:bad"] {
        let mut table = Table::default();
        let statements = load(
            &mut table,
            &format!(
                "ex:p a owl:DatatypeProperty . ex:A a owl:Class .
                ex:p rdfs:range [ owl:onDatatype xsd:integer ; owl:withRestrictions ( {list} ) ] .
                ex:f xsd:minInclusive 0 .
                _:bad a ex:A ."
            ),
        );
        let reversed: Vec<Statement> = statements.iter().rev().copied().collect();
        let [written, backwards] = [&statements, &reversed].map(|order| reading(order, &table));
        eprintln!("NAMED ( {list} ) as written: {written:?}");
        eprintln!("NAMED ( {list} ) reversed: {backwards:?}");
        assert_eq!(written, backwards, "( {list} ): either order");
        assert!(
            (written.0.iter()).any(|d| d.starts_with("a facet restriction not on a blank node: ")),
            "( {list} ): left out {:?}",
            written.0
        );
    }
}
