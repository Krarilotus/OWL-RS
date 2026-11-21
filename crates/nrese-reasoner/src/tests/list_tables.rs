use super::{ListVocabulary, LocalVocabulary, Ruleset, Schema, SliceFacts, batch, load};
use crate::delta::{self, MemoryBase, Rules};
use crate::lists::Facts;

#[test]
fn dense_conjunctive_list_search_stays_within_counted_work() {
    use std::cell::Cell;

    struct Counted<'a> {
        facts: SliceFacts<'a>,
        work: Cell<usize>,
        limit: usize,
    }
    impl Facts for Counted<'_> {
        fn objects(&self, subject: u64, predicate: u64) -> Vec<u64> {
            self.facts.objects(subject, predicate)
        }
        fn pairs(&self, predicate: u64) -> Vec<(u64, u64)> {
            self.facts.pairs(predicate)
        }
        fn stopped(&self) -> bool {
            self.work.set(self.work.get() + 1);
            self.work.get() > self.limit
        }
    }
    for axiom in ["owl:intersectionOf", "owl:hasKey"] {
        let mut text = format!(
            "ex:C {axiom} ex:h\nex:h rdf:first ex:A\nex:h rdf:rest ex:n0\nex:h rdf:rest rdf:nil"
        );
        for i in 0..16 {
            text.push_str(&format!("\nex:n{i} rdf:first ex:A\nex:n{i} rdf:rest ex:h"));
            for j in (0..16).filter(|&j| j != i) {
                text.push_str(&format!("\nex:n{i} rdf:rest ex:n{j}"));
            }
        }
        let mut vocabulary = LocalVocabulary::default();
        let input = load(&mut vocabulary, &text);
        let voc = ListVocabulary::new(&mut vocabulary);
        // Table 3 has a one-cell terminating walk. Finding one shouldn't
        // exhaust the search allowance on failed prefixes.
        let facts = Counted {
            facts: SliceFacts(&input),
            work: Cell::new(0),
            limit: 16 * input.len(),
        };
        let (_, diagnostics) = crate::lists::instantiate(&voc, &facts);
        assert!(diagnostics.is_empty(), "{axiom}: {diagnostics:?}");
        assert!(
            facts.work.get() <= facts.limit,
            "{axiom} exhausted the search allowance"
        );
    }
}

/// How many rules of the property chain `ex:out` read or conclude a private relation (the
/// per-cell form). Its list starts at `head`; `cells` cells `ex:c0`… with two firsts each
/// end it, after the cells of `before`.
fn per_cell_rules(head: &str, before: &str, cells: usize) -> usize {
    use crate::ir::{Atom, Head, Term};
    let mut text = format!("ex:out owl:propertyChainAxiom {head}\n{before}");
    for i in 0..cells {
        let rest = if i + 1 == cells {
            "rdf:nil".to_owned()
        } else {
            format!("ex:c{}", i + 1)
        };
        text.push_str(&format!(
            "\nex:c{i} rdf:first ex:p{i}\nex:c{i} rdf:first ex:q{i}\nex:c{i} rdf:rest {rest}"
        ));
    }
    let mut vocabulary = LocalVocabulary::default();
    let input = load(&mut vocabulary, &text);
    let voc = ListVocabulary::new(&mut vocabulary);
    let (rules, diagnostics) = crate::lists::instantiate(&voc, &SliceFacts(&input));
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let private =
        |Atom(terms): &Atom| matches!(terms[1], Term::Const(p) if crate::lists::internal(p));
    rules
        .iter()
        .filter(|rule| {
            rule.body.iter().any(private)
                || matches!(&rule.head, Head::Facts(heads) if heads.iter().any(private))
        })
        .count()
}

#[test]
fn a_chain_of_up_to_sixty_four_walks_is_enumerated() {
    // Spec: reasoning-semantics.md, list limits: a property chain uses the
    // ordinary rule form for up to 64 alternatives. Five and six cells with
    // two firsts each give 32 and 64 walks.
    for cells in [5, 6] {
        assert_eq!(per_cell_rules("ex:c0", "", cells), 0, "{cells} cells");
    }
    // A head with one first before the six cells, and a second rest to a
    // one-cell list, gives 65 walks: past the bound, individual cells.
    let before = "ex:h rdf:first ex:r
        ex:h rdf:rest ex:c0
        ex:h rdf:rest ex:y
        ex:y rdf:first ex:s
        ex:y rdf:rest rdf:nil";
    assert!(per_cell_rules("ex:h", before, 6) > 0);
}

#[test]
fn explaining_an_ordinary_property_refusal_does_no_list_grounding() {
    use crate::engine::{Program, explain_violation, maintain};
    use crate::witness::{Site, get};
    use crate::{ReasonerConfig, ReasoningMode};
    use nrese_engine::{EncodedQuad, Engine, EngineConfig};
    use nrese_rdf::NamedNodeRef;

    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rules = ReasonerConfig::for_mode(ReasoningMode::Owl2Rl)
        .materialised_program()
        .unwrap();
    let program = Program::compile(&rules, &|term| tx.intern(term));
    let [a, b, p, q, r, sub, disjoint] = [
        "http://example.org/a",
        "http://example.org/b",
        "http://example.org/p",
        "http://example.org/q",
        "http://example.org/r",
        "http://www.w3.org/2000/01/rdf-schema#subPropertyOf",
        "http://www.w3.org/2002/07/owl#propertyDisjointWith",
    ]
    .map(|name| tx.intern(NamedNodeRef::new_unchecked(name).into()));
    for [s, p, o] in [[p, sub, q], [q, disjoint, r], [a, p, b], [a, r, b]] {
        tx.insert_encoded(EncodedQuad::new(
            s,
            p,
            o,
            nrese_engine::TermId::DEFAULT_GRAPH,
        ));
    }
    // Table 5 refuses the prp-spo1 consequence q(a,b) by prp-pdw.
    let update = maintain(&program, None, &mut tx, crate::eval::NEVER).unwrap();
    let violation = update
        .violations
        .first()
        .expect("disjoint properties clash");
    assert_eq!(violation.rule, "prp-pdw");
    let before = get(Site::ListGrounding);
    let explanation = explain_violation(&program, &violation, &tx, None);
    assert!(
        explanation
            .evidence
            .iter()
            .any(|fact| fact.origin == "inferred")
    );
    assert_eq!(get(Site::ListGrounding) - before, 0, "grounding passes");
}

#[test]
fn short_cyclic_memberships_and_keys_need_no_private_facts() {
    // Spec: OWL 2 RL Tables 3, 5 and 6 give U, I and sameAs from the
    // three simple walks. Each turn around a loop repeats a premise.
    let mut vocabulary = LocalVocabulary::default();
    let input = load(
        &mut vocabulary,
        "
        ex:U owl:unionOf ex:u
        ex:u rdf:first ex:A
        ex:u rdf:rest ex:u
        ex:u rdf:rest rdf:nil
        ex:I owl:intersectionOf ex:i
        ex:i rdf:first ex:A
        ex:i rdf:rest ex:i
        ex:i rdf:rest rdf:nil
        ex:K owl:hasKey ex:k
        ex:k rdf:first ex:p
        ex:k rdf:rest ex:k
        ex:k rdf:rest rdf:nil
        ex:a rdf:type ex:A
        ex:a rdf:type ex:K
        ex:b rdf:type ex:K
        ex:a ex:p ex:v
        ex:b ex:p ex:v
        ",
    );
    let answers = load(
        &mut vocabulary,
        "ex:a rdf:type ex:U\nex:a rdf:type ex:I\nex:a owl:sameAs ex:b",
    );
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let closure = batch::materialise(&input, &rules, Some(&lists), &schema);
    assert!(answers.iter().all(|row| closure.derived.contains(row)));
    let base = MemoryBase::new(&input, &closure.derived);
    let program = delta::program(
        &base,
        Rules {
            rules: &rules,
            lists: Some(&lists),
            schema: &schema,
            equality: crate::representatives::same_as(&rules, schema.same_as()),
        },
    );
    assert!(program.table.is_none());
}
