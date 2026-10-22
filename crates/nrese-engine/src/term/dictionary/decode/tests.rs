use nrese_exec::measure;
use nrese_rdf::vocab::xsd;
use nrese_rdf::{BaseDirection, BlankNode, Literal, NamedNode};

use super::*;
use crate::term::TermKind;

const EXISTING: usize = 37;

/// Success accounts for every allocation event, then keeps exactly the returned heap.
/// Refusal has no allocation when the caller's existing reservation exhausts its budget.
fn check(dictionary: &Dictionary, id: TermId, expected: &Term) {
    assert_eq!(dictionary.decode(id).as_ref(), Some(expected));
    let budget = Budget::unlimited();
    let ((term, prior, held), excess) = measure::measure_within(&budget, || {
        let mut held = Reservation::new(&budget);
        held.charge(EXISTING).unwrap();
        let prior = vec![0_u8; EXISTING];
        let term = dictionary.decode_within(id, &mut held).unwrap().unwrap();
        (term, prior, held)
    });
    assert_eq!(excess, 0, "admission before every allocation: {id:?}");
    assert_eq!(&term, expected);
    assert_eq!(held.bytes(), EXISTING + term.heap_bytes());
    drop(term);
    drop(prior);
    drop(held);
    assert_eq!(budget.used(), 0);

    let budget = Budget::new(EXISTING);
    let ((result, held), allocated) = measure::measure(|| {
        let mut held = Reservation::new(&budget);
        held.charge(EXISTING).unwrap();
        (dictionary.decode_within(id, &mut held), held)
    });
    assert!(result.is_err(), "known nonempty term must refuse: {id:?}");
    assert_eq!(allocated, 0, "refuse before the first allocation");
    assert_eq!(held.bytes(), EXISTING);
    assert_eq!(budget.used(), EXISTING);
    drop(held);
    assert_eq!(budget.used(), 0);
}

#[test]
fn complete_decode_values_and_inline_families() {
    if !measure::alone() {
        return;
    }
    let dictionary = Dictionary::default();
    let cases = [
        ("-576460752303423488", xsd::INTEGER, TermKind::Integer),
        ("576460752303423487", xsd::INTEGER, TermKind::Integer),
        ("true", xsd::BOOLEAN, TermKind::Boolean),
        ("false", xsd::BOOLEAN, TermKind::Boolean),
        ("-36028797018963967.0", xsd::DECIMAL, TermKind::Decimal),
        ("0.000000000000001", xsd::DECIMAL, TermKind::Decimal),
        ("0000-01-01-14:00", xsd::DATE, TermKind::Date),
        ("9999-12-31+14:00", xsd::DATE, TermKind::Date),
        (
            "9999-12-31T23:59:59.999+14:00",
            xsd::DATE_TIME,
            TermKind::DateTime,
        ),
    ];
    for (lexical, datatype, kind) in cases {
        let term: Term = Literal::new_typed_literal(lexical, datatype).into();
        let id = dictionary.intern(term.as_ref());
        assert_eq!(id.kind(), kind, "fixture really is inline");
        check(&dictionary, id, &term);
    }
    for datatype in [
        xsd::NON_POSITIVE_INTEGER,
        xsd::NEGATIVE_INTEGER,
        xsd::LONG,
        xsd::INT,
        xsd::SHORT,
        xsd::BYTE,
        xsd::NON_NEGATIVE_INTEGER,
        xsd::UNSIGNED_LONG,
        xsd::UNSIGNED_INT,
        xsd::UNSIGNED_SHORT,
        xsd::UNSIGNED_BYTE,
        xsd::POSITIVE_INTEGER,
    ] {
        let lexical = if datatype == xsd::NEGATIVE_INTEGER || datatype == xsd::NON_POSITIVE_INTEGER
        {
            "-1"
        } else {
            "1"
        };
        let term: Term = Literal::new_typed_literal(lexical, datatype).into();
        let id = dictionary.intern(term.as_ref());
        assert_eq!(id.kind(), TermKind::DerivedInteger);
        check(&dictionary, id, &term);
    }
    let atoms: [Term; 6] = [
        NamedNode::new_unchecked("urn:atomic").into(),
        BlankNode::new_unchecked("b").into(),
        Literal::new_simple_literal("wide".repeat(4096)).into(),
        Literal::new_language_tagged_literal_unchecked("word", "en").into(),
        Literal::new_directional_language_tagged_literal_unchecked(
            "word",
            "en",
            BaseDirection::Rtl,
        )
        .into(),
        Literal::new_typed_literal("01", xsd::INTEGER).into(),
    ];
    for term in atoms {
        check(&dictionary, dictionary.intern(term.as_ref()), &term);
    }
    let mut term: Term = Literal::new_simple_literal("nested".repeat(2048)).into();
    for depth in 0..8 {
        term = Triple::new(
            NamedNode::new_unchecked(format!("urn:s{depth}")),
            NamedNode::new_unchecked("urn:p"),
            term,
        )
        .into();
    }
    check(&dictionary, dictionary.intern(term.as_ref()), &term);
}

#[test]
fn complete_decode_retained_legacy_and_parser_scratch() {
    if !measure::alone() {
        return;
    }
    // Restore the actual retained R encoding, rather than interning it as modern Q IDs.
    for width in [1, 4096] {
        let mut term: Term = Literal::new_language_tagged_literal_unchecked(
            format!("{}\n\"\\", "x".repeat(width)),
            "en",
        )
        .into();
        for depth in 0..16 {
            term = Triple::new(
                NamedNode::new_unchecked(format!("urn:é{depth}")),
                NamedNode::new_unchecked("urn:p"),
                term,
            )
            .into();
        }
        let text = term.to_string();
        let key = [vec![super::super::TAG_TRIPLE], text.as_bytes().to_vec()].concat();
        let dictionary = Dictionary::default();
        dictionary.restore_keys(&[&key]).unwrap();
        check(&dictionary, TermId::new(TermKind::Triple, 0), &term);
    }
    // Existing legacy fallback is also part of decode's value domain.
    let dictionary = Dictionary::default();
    dictionary.restore_keys(&[b"Rnot a triple"]).unwrap();
    check(
        &dictionary,
        TermId::new(TermKind::Triple, 0),
        &Literal::new_simple_literal("not a triple").into(),
    );
}

#[test]
fn complete_decode_partial_refusal_absence_and_checked_bounds_release_only_added_bytes() {
    if !measure::alone() {
        return;
    }
    let dictionary = Dictionary::default();
    let term: Term = Triple::new(
        NamedNode::new_unchecked("urn:s"),
        NamedNode::new_unchecked("urn:p"),
        Literal::new_simple_literal("x".repeat(8192)),
    )
    .into();
    let id = dictionary.intern(term.as_ref());
    for room in [0, 32, 64, 1024] {
        let budget = Budget::new(EXISTING + room);
        let ((result, prior, held), excess) = measure::measure_within(&budget, || {
            let mut held = Reservation::new(&budget);
            held.charge(EXISTING).unwrap();
            let prior = vec![0_u8; EXISTING];
            (dictionary.decode_within(id, &mut held), prior, held)
        });
        assert!(result.is_err());
        assert_eq!(excess, 0, "partial payload freed before credit");
        assert_eq!(held.bytes(), EXISTING);
        assert_eq!(budget.used(), EXISTING);
        drop(prior);
        drop(held);
        assert_eq!(budget.used(), 0);
    }
    let mut broken = Vec::new();
    super::super::triple_key(
        [
            TermId::new(TermKind::Iri, 999),
            TermId::new(TermKind::Iri, 0),
            TermId::new(TermKind::String, 999),
        ],
        &mut broken,
    );
    let dictionary = Dictionary::default();
    dictionary.restore_keys(&[b"Iurn:p", &broken]).unwrap();
    for id in [
        TermId::new(TermKind::Triple, 1),
        TermId::new(TermKind::Iri, 999),
        TermId::DEFAULT_GRAPH,
        TermId::new(TermKind::DerivedInteger, 15),
    ] {
        assert_eq!(dictionary.decode(id), None);
        let budget = Budget::unlimited();
        let ((result, prior, held), excess) = measure::measure_within(&budget, || {
            let mut held = Reservation::new(&budget);
            held.charge(EXISTING).unwrap();
            let prior = vec![0_u8; EXISTING];
            (dictionary.decode_within(id, &mut held), prior, held)
        });
        assert_eq!(result, Ok(None));
        assert_eq!(excess, 0);
        assert_eq!(held.bytes(), EXISTING);
        drop(prior);
        drop(held);
        assert_eq!(budget.used(), 0);
    }
    assert_eq!(legacy_peak(usize::MAX), None);
}

#[test]
fn complete_decode_atomic_allocation_cost_matches_existing_decoder() {
    if !measure::alone() {
        return;
    }
    let dictionary = Dictionary::default();
    let term: Term = Literal::new_simple_literal("x".repeat(16384)).into();
    let id = dictionary.intern(term.as_ref());
    let (old, old_peak) = measure::measure(|| dictionary.decode(id).unwrap());
    let budget = Budget::unlimited();
    let ((new, held), new_peak) = measure::measure(|| {
        let mut held = Reservation::new(&budget);
        let term = dictionary.decode_within(id, &mut held).unwrap().unwrap();
        (term, held)
    });
    assert_eq!(new, old);
    assert_eq!(new_peak, old_peak, "no extra atomic allocation/copy");
    assert_eq!(held.bytes(), new.heap_bytes());
    println!(
        "atomic_heap_peak old={old_peak} new={new_peak}; added budget operations: one charge, one trim; no timing verdict"
    );
    drop((new, held));
    assert_eq!(budget.used(), 0);
}
