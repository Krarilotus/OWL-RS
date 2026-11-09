//! Guards of package C3 (`notes/2026-11-09-d039-update-sweep.md`, row C3): a charged decode
//! covers every kind of term, as the plain decode does. An inline literal's text and a triple
//! term's components and boxes are charged before they are made; `Ok(None)` only for an id that
//! isn't a term of the snapshot. Through a published snapshot (the dictionary) and a private one
//! (its overlay's own entries). Measured against the charging budget (`nrese_exec::measure`), so
//! this file is a test binary of its own.
//!
//! On the parent (engine-v2 `ab99d53e`) each charged decode of these ids answers `Ok(None)` with
//! no charge: every guard here fails there on its first assertion.

use std::cell::Cell;
use std::sync::Arc;

use nrese_engine::{
    Engine, EngineConfig, Snapshot, TermId, TermKind, VocabularyEncoding, set_vocabulary_encoding,
};
use nrese_exec::{Budget, BudgetExceeded, measure};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term, Triple};

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

fn iri(s: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("urn:c3:{s}"))
}

fn engine(dir: &std::path::Path) -> Engine {
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    Engine::open(dir, config).unwrap()
}

/// One inline literal of each inline kind with a text to make: an integer, a decimal, a boolean,
/// a date and a dateTime (with milliseconds and an offset, its longest form). No string literal
/// is inline: the inline kinds are these, plus derived integers where a store inlines them.
fn inline_literals() -> Vec<Term> {
    [
        ("-1234567890123", xsd::INTEGER),
        ("-12345.678901", xsd::DECIMAL),
        ("false", xsd::BOOLEAN),
        ("2026-11-09+14:00", xsd::DATE),
        ("2026-11-09T10:57:30.123-09:30", xsd::DATE_TIME),
    ]
    .into_iter()
    .map(|(lexical, datatype)| Literal::new_typed_literal(lexical, datatype).into())
    .collect()
}

/// `<< <outer> <p> << <inner> <q> "o"@en >> >>`: a triple term nested once.
fn nested_triple(tag: &str) -> Term {
    let inner = Triple::new(
        iri(&format!("{tag}-inner")),
        iri("q"),
        Literal::new_language_tagged_literal_unchecked(format!("o {tag}"), "en"),
    );
    Triple::new(iri(&format!("{tag}-outer")), iri("p"), Term::from(inner)).into()
}

/// Commits a quad with each of `objects` and gives their ids.
fn committed(engine: &Engine, objects: &[Term]) -> Vec<TermId> {
    let mut tx = engine.transaction();
    let ids = objects.iter().map(|o| tx.intern(o.as_ref())).collect();
    for object in objects {
        let quad = Quad::new(
            iri("s"),
            iri("has"),
            object.clone(),
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    ids
}

/// The charged decode of `id` in `snapshot` gives the plain decode, `expected`, after one or more
/// charges that together cover what the term holds, and holds no byte beyond what it charged.
fn decodes_charged(snapshot: &Snapshot, id: TermId, expected: &Term) {
    assert_eq!(snapshot.decode(id).as_ref(), Some(expected), "plain decode");
    let budget = Budget::unlimited();
    let (charges, charged) = (Cell::new(0usize), Cell::new(0usize));
    let (decoded, excess) = measure::measure_within(&budget, || {
        snapshot.decode_charged(id, |bytes| {
            charges.set(charges.get() + 1);
            charged.set(charged.get() + bytes);
            budget.charge(bytes)
        })
    });
    assert_eq!(
        decoded,
        Ok(Some(expected.clone())),
        "charged decode of {expected}"
    );
    assert!(charges.get() >= 1, "{expected}: no charge");
    assert!(
        charged.get() >= expected.heap_bytes(),
        "{expected}: {} bytes charged, the term holds {}",
        charged.get(),
        expected.heap_bytes()
    );
    assert_eq!(
        excess, 0,
        "{expected}: held {excess} bytes beyond its charge"
    );
}

/// A charged decode of `id` whose first charge is refused gives the refusal, after that one
/// charge, and allocates nothing.
fn refused_charge_decodes_nothing(snapshot: &Snapshot, id: TermId, expected: &Term) {
    let budget = Budget::new(0);
    let charges = Cell::new(0usize);
    let (decoded, excess) = measure::measure_within(&budget, || {
        snapshot.decode_charged(id, |bytes| {
            charges.set(charges.get() + 1);
            budget.charge(bytes)
        })
    });
    assert!(
        matches!(decoded, Err(BudgetExceeded { limit: 0, .. })),
        "refused decode of {expected}: {decoded:?}"
    );
    assert_eq!(charges.get(), 1, "{expected}: charges after the refusal");
    assert_eq!(
        excess, 0,
        "{expected}: allocated {excess} bytes, none charged"
    );
}

/// A published and a private snapshot over the same engine: inline literals and a triple term
/// nested once committed, and in the private one, a triple term of its own nested over a shared
/// one and one nested over its own.
struct Fixture {
    published: Snapshot,
    private: Snapshot,
    terms: Vec<(TermId, Term)>,
    own: Vec<(TermId, Term)>,
}

fn fixture(engine: &Engine) -> Fixture {
    let mut terms = inline_literals();
    terms.push(nested_triple("shared"));
    let ids = committed(engine, &terms);
    let terms: Vec<(TermId, Term)> = ids.into_iter().zip(terms).collect();
    for (id, term) in &terms[..terms.len() - 1] {
        assert!(
            !matches!(
                id.kind(),
                TermKind::Iri
                    | TermKind::BlankNode
                    | TermKind::String
                    | TermKind::LangString
                    | TermKind::TypedLiteral
                    | TermKind::Triple
            ),
            "{term} is inline"
        );
    }
    let published = engine.snapshot();
    let mut private = engine
        .speculative_over(&published, Arc::new(Budget::unlimited()))
        .unwrap();
    let Term::Triple(shared) = &terms[terms.len() - 1].1 else {
        unreachable!()
    };
    let own_over_shared: Term =
        Triple::new(iri("own-outer"), iri("p"), shared.object.clone()).into();
    let own = [own_over_shared, nested_triple("own")]
        .into_iter()
        .map(|term| {
            let id = private.try_intern(term.as_ref()).unwrap();
            let quad = Quad::new(iri("s"), iri("has"), term.clone(), GraphName::DefaultGraph);
            private.try_insert(quad.as_ref()).unwrap();
            (id, term)
        })
        .collect::<Vec<_>>();
    for (id, term) in &own {
        assert_eq!(id.kind(), TermKind::Triple, "{term}");
        assert_eq!(published.decode(*id), None, "{term} is its own");
    }
    let private = private.pending_snapshot().unwrap();
    assert!(private.is_private());
    Fixture {
        published,
        private,
        terms,
        own,
    }
}

/// Every inline literal and the nested triple term, through the published snapshot and the
/// private one; the private one's own triple terms through it.
#[test]
fn every_kind_of_term_decodes_charged_as_it_decodes() {
    if !measure::alone() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let fixture = fixture(&engine);
    for (id, term) in &fixture.terms {
        decodes_charged(&fixture.published, *id, term);
        decodes_charged(&fixture.private, *id, term);
    }
    for (id, term) in &fixture.own {
        decodes_charged(&fixture.private, *id, term);
    }
}

/// The same terms, each charged decode refused at its first charge.
#[test]
fn a_refused_first_charge_decodes_nothing() {
    if !measure::alone() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let fixture = fixture(&engine);
    for (id, term) in &fixture.terms {
        refused_charge_decodes_nothing(&fixture.published, *id, term);
        refused_charge_decodes_nothing(&fixture.private, *id, term);
    }
    for (id, term) in &fixture.own {
        refused_charge_decodes_nothing(&fixture.private, *id, term);
    }
}

/// A triple term nested once committed while the keys are plain, a snapshot taken, a checkpoint
/// then publishing its keys compressed: that snapshot's charged decode of it, and a refused one,
/// hold nothing beyond their charge (reading a compressed triple key's component ids included).
#[test]
fn a_triple_term_stored_compressed_decodes_charged() {
    if !measure::alone() {
        return;
    }
    set_vocabulary_encoding(VocabularyEncoding::Fsst);
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path());
    let triple = nested_triple("compressed");
    let id = committed(&engine, std::slice::from_ref(&triple))[0];
    let snapshot = engine.snapshot();
    engine.checkpoint().unwrap();
    decodes_charged(&snapshot, id, &triple);
    refused_charge_decodes_nothing(&snapshot, id, &triple);
}
