//! Package 2a's guards (`notes/2026-11-08-p2-bounded-vocabulary-design.md` §4 and §9): new-contract
//! tests of an API the base doesn't have, so none is red there. Each states its specified result:
//! ids by position from the overlay's `start`, terms decoded back to the inputs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use nrese_exec::Budget;
use nrese_rdf::vocab::xsd;
use nrese_rdf::{GraphName, Literal, LiteralRef, NamedNode, Quad, Term, TermRef, Triple};

use super::Overlay;
use crate::term::dictionary::Dictionary;
use crate::term::resolve::Vocabulary;
use crate::term::{Placement, StringTest, TermId, TermKind};

fn iri(s: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("http://e/{s}"))
}

fn literal(s: &str) -> Term {
    Literal::new_simple_literal(s).into()
}

fn unlimited() -> Arc<Budget> {
    Arc::new(Budget::unlimited())
}

/// A dictionary of `n` literals "old 0".., and an overlay opened at its length.
fn with_overlay(n: usize, budget: &Arc<Budget>) -> (Arc<Dictionary>, Arc<Overlay>) {
    let dictionary = Arc::new(Dictionary::default());
    for i in 0..n {
        dictionary.intern(LiteralRef::new_simple_literal(&format!("old {i}")).into());
    }
    let start = dictionary.len();
    let overlay = Overlay::over(Arc::clone(&dictionary), start, Arc::clone(budget));
    (dictionary, overlay)
}

fn strings_starting(needle: &str) -> StringTest<'_> {
    StringTest {
        needle,
        placement: Placement::Start,
        iris: false,
        strings: true,
        lang_strings: false,
        typed: false,
        language: None,
        ascii_case_insensitive: false,
    }
}

/// B2. A term the dictionary had at `start` keeps its shared id in the overlay; one published
/// after the overlay opened is the overlay's own, from `start`, while the dictionary gave it
/// another meaning of that number; the view decodes and finds its own, whatever bound it is
/// passed; the private and shared vocabularies don't share ids.
#[test]
fn a_term_published_after_the_overlay_opened_is_its_own() {
    let budget = unlimited();
    let (dictionary, overlay) = with_overlay(10, &budget);
    let start = overlay.start();
    assert_eq!(start, 10);
    let old = overlay.intern(literal("old 3").as_ref()).unwrap();
    assert_eq!((old.kind(), old.payload()), (TermKind::String, 3));
    let published = dictionary.intern(literal("published").as_ref());
    assert_eq!(published.payload(), 10);
    let own = overlay.intern(literal("x").as_ref()).unwrap();
    assert_eq!(own.payload(), 10, "the overlay's first own id is its start");
    let later = overlay.intern(literal("published").as_ref()).unwrap();
    assert_eq!(
        later.payload(),
        11,
        "published after start: the overlay's own"
    );
    assert_eq!(overlay.lookup(literal("x").as_ref()), Some(own));
    let view = overlay.freeze().unwrap();
    assert_eq!((view.start(), view.end(), view.len()), (10, 12, 2));
    let private = Vocabulary::Private(view.clone());
    let shared = Vocabulary::Shared(Arc::clone(&dictionary));
    assert_eq!(private.decode(own), Some(literal("x")));
    assert_eq!(private.decode(later), Some(literal("published")));
    assert_eq!(
        shared.decode(own),
        Some(literal("published")),
        "the same number, another term"
    );
    for bound in [0, 5, u64::MAX] {
        assert_eq!(
            private.lookup(literal("x").as_ref(), bound),
            Some(own),
            "bound {bound}"
        );
    }
    assert_eq!(shared.lookup(literal("x").as_ref(), 11), None);
    assert!(private.is_private() && !shared.is_private());
    assert_eq!(
        (private.shared_bound(99), shared.shared_bound(99)),
        (10, 99)
    );
    assert!(!private.same_as(&shared) && !shared.same_as(&private));
    assert!(shared.same_as(&Vocabulary::Shared(Arc::clone(&dictionary))));
    assert!(private.same_as(&Vocabulary::Private(overlay.freeze().unwrap())));
    let other = Overlay::over(Arc::clone(&dictionary), 10, Arc::clone(&budget));
    assert!(!private.same_as(&Vocabulary::Private(other.freeze().unwrap())));
    assert!(private.dictionary().is_none());
    assert!(Arc::ptr_eq(shared.dictionary().unwrap(), &dictionary));
    assert!(Arc::ptr_eq(view.dictionary(), &dictionary));
}

/// B3. Interns of the same terms from 8 threads at once give each term one id, dense from start.
#[test]
fn concurrent_interns_of_one_term_give_one_id() {
    let budget = unlimited();
    let (_dictionary, overlay) = with_overlay(5, &budget);
    let texts: Arc<Vec<String>> = Arc::new((0..1_000).map(|i| format!("t {i}")).collect());
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let (overlay, texts) = (Arc::clone(&overlay), Arc::clone(&texts));
            std::thread::spawn(move || {
                texts
                    .iter()
                    .map(|t| {
                        overlay
                            .intern(LiteralRef::new_simple_literal(t).into())
                            .unwrap()
                    })
                    .collect::<Vec<TermId>>()
            })
        })
        .collect();
    let ids: Vec<Vec<TermId>> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    for other in &ids[1..] {
        assert_eq!(other, &ids[0], "every thread saw one id per term");
    }
    let mut payloads: Vec<u64> = ids[0].iter().map(|id| id.payload()).collect();
    payloads.sort_unstable();
    assert_eq!(
        payloads,
        (5..1_005).collect::<Vec<u64>>(),
        "dense from start"
    );
}

/// B4. Triple terms resolve their components first: a triple the dictionary had below start keeps
/// its id; one it got only after start is the overlay's own; one with an own component is own; a
/// nested one too. Each one's components precede it and it decodes back.
#[test]
fn triple_terms_resolve_components_first() {
    let budget = unlimited();
    let dictionary = Arc::new(Dictionary::default());
    let shared = Triple::new(iri("s"), iri("p"), literal("o"));
    let shared_id = dictionary.intern(TermRef::Triple(&shared));
    let start = dictionary.len();
    let overlay = Overlay::over(Arc::clone(&dictionary), start, Arc::clone(&budget));
    assert_eq!(overlay.intern(TermRef::Triple(&shared)).unwrap(), shared_id);
    let later = Triple::new(iri("s"), iri("p"), literal("later"));
    let published = dictionary.intern(TermRef::Triple(&later));
    let own = overlay.intern(TermRef::Triple(&later)).unwrap();
    // The overlay's own: at or past start. The dictionary's later copy may have the same
    // number in its own domain (contract §4.1: equal numbers across vocabularies mean nothing).
    assert!(own.payload() >= start);
    assert_eq!(
        dictionary.decode(published),
        Some(Term::Triple(Box::new(later.clone())))
    );
    let mixed = Triple::new(iri("s"), iri("q-new"), literal("o"));
    let mixed_id = overlay.intern(TermRef::Triple(&mixed)).unwrap();
    let nested = Triple::new(iri("s"), iri("p"), Term::Triple(Box::new(mixed.clone())));
    let nested_id = overlay.intern(TermRef::Triple(&nested)).unwrap();
    let private = Vocabulary::Private(overlay.freeze().unwrap());
    for (triple, id) in [(&later, own), (&mixed, mixed_id), (&nested, nested_id)] {
        assert_eq!(id.kind(), TermKind::Triple);
        let components = private
            .components(id, 0)
            .expect("an own triple's components");
        for component in components {
            assert!(
                component.payload() < id.payload(),
                "{component:?} precedes {id:?}"
            );
        }
        assert_eq!(
            private.decode(id),
            Some(Term::Triple(Box::new(triple.clone())))
        );
        assert_eq!(private.triple_term(components, 0), Some(id));
        assert_eq!(private.lookup(TermRef::Triple(triple), 0), Some(id));
    }
    assert!(private.components(mixed_id, 0).unwrap()[1].payload() >= start);
    assert_eq!(
        private.components(shared_id, 0),
        dictionary.components(shared_id)
    );
    assert_eq!(
        private.components(TermId::new(TermKind::String, 0), 0),
        None
    );
}

/// B5. Inline literals are never added; on a dictionary that keeps integers as entries (a store
/// from before inline integers) an integer literal is added like any term.
#[test]
fn inline_literals_are_never_added_and_legacy_integers_are() {
    let budget = unlimited();
    let (_dictionary, overlay) = with_overlay(3, &budget);
    for term in [
        LiteralRef::new_typed_literal("42", xsd::INTEGER),
        LiteralRef::new_typed_literal("true", xsd::BOOLEAN),
        LiteralRef::new_typed_literal("2026-11-08", xsd::DATE),
    ] {
        let id = overlay.intern(term.into()).unwrap();
        assert!(!id.kind().is_dictionary(), "{term}: inline");
        assert_eq!(overlay.lookup(term.into()), Some(id));
    }
    assert_eq!(overlay.freeze().unwrap().len(), 0, "nothing added");
    let legacy = Arc::new(Dictionary::default());
    legacy.set_integers_in_dictionary(true);
    let overlay = Overlay::over(Arc::clone(&legacy), 0, Arc::clone(&budget));
    // The legacy flag governs integers of the derived types (`xsd:int` …), not `xsd:integer`.
    let id = overlay
        .intern(LiteralRef::new_typed_literal("42", xsd::INT).into())
        .unwrap();
    assert!(id.kind().is_dictionary(), "an entry on a legacy store");
    assert_eq!(id.payload(), 0);
    assert_eq!(legacy.len(), 0, "the overlay's, not the dictionary's");
}

/// B6. A frozen view answers exactly what was interned when it was taken.
#[test]
fn a_frozen_view_answers_what_it_was_taken_with() {
    let budget = unlimited();
    let (_dictionary, overlay) = with_overlay(4, &budget);
    let first = overlay.intern(literal("a").as_ref()).unwrap();
    let early = Vocabulary::Private(overlay.freeze().unwrap());
    let second = overlay.intern(literal("b").as_ref()).unwrap();
    let late = Vocabulary::Private(overlay.freeze().unwrap());
    assert_eq!(early.decode(first), Some(literal("a")));
    assert_eq!(early.decode(second), None, "not in the earlier view");
    assert_eq!(early.lookup(literal("b").as_ref(), 0), None);
    assert_eq!(late.decode(second), Some(literal("b")));
    assert_eq!(late.lookup(literal("b").as_ref(), 0), Some(second));
}

/// B9. The bounded and charged reads at the edge: the last own id, the first past it, an id below
/// start, a triple term (whose charged decode is its components', `None`, as the dictionary's).
#[test]
fn bounded_reads_answer_at_the_own_edge() {
    let budget = unlimited();
    let (_dictionary, overlay) = with_overlay(6, &budget);
    let last = overlay.intern(literal("last").as_ref()).unwrap();
    let triple = overlay
        .intern(TermRef::Triple(&Triple::new(
            iri("s"),
            iri("p"),
            literal("last"),
        )))
        .unwrap();
    let view = Vocabulary::Private(overlay.freeze().unwrap());
    let past = TermId::new(TermKind::String, view_end(&view));
    let below = TermId::new(TermKind::String, 2);
    let ok = |_: usize| Ok::<(), ()>(());
    assert_eq!(view.decode_charged(last, 0, ok), Ok(Some(literal("last"))));
    assert_eq!(view.decode_charged(past, 0, ok), Ok(None));
    assert_eq!(
        view.decode_charged(below, 0, ok),
        Ok(Some(literal("old 2")))
    );
    assert_eq!(view.decode_charged(triple, 0, ok), Ok(None));
    assert_eq!(view.decode_bound(last, 0), Some(literal_key_len("last")));
    assert_eq!(view.decode_bound(past, 0), None);
    assert_eq!(view.decode_bound(triple, 0), None);
    assert!(view.decode_bound(below, 0).is_some());
    let text = |v: crate::TermView<'_>| v.str().map(str::to_owned);
    assert_eq!(view.with_view(last, 0, text), Some(Some("last".into())));
    assert_eq!(view.with_view(below, 0, text), Some(Some("old 2".into())));
    assert_eq!(view.with_view(past, 0, |_| ()), None);
    let mut charged = 0;
    let found = view.lookup_charged(literal("last").as_ref(), 0, |bytes| {
        charged += bytes;
        Ok::<(), ()>(())
    });
    assert_eq!(found, Ok(Some(last)));
    assert!(charged > 0, "the lookup's scratch charged first");
}

/// B9's quad form: an encoded quad of shared and own terms, in a named graph and the default.
#[test]
fn quads_of_shared_and_own_terms_round_trip() {
    let budget = unlimited();
    let (dictionary, _) = with_overlay(3, &budget);
    let named = dictionary.intern(iri("g").as_ref().into());
    let overlay = Overlay::over(
        Arc::clone(&dictionary),
        dictionary.len(),
        Arc::clone(&budget),
    );
    for graph in [GraphName::NamedNode(iri("g")), GraphName::DefaultGraph] {
        let quad = Quad::new(iri("s-new"), iri("p-new"), literal("old 1"), graph.clone());
        let encoded = overlay.intern_quad(quad.as_ref()).unwrap();
        assert_eq!(encoded.object.payload(), 1, "a shared object");
        let view = Vocabulary::Private(overlay.freeze().unwrap());
        assert_eq!(view.decode_quad(encoded), Some(quad.clone()));
        assert_eq!(view.lookup_quad(quad.as_ref(), 0), Some(encoded));
        if graph != GraphName::DefaultGraph {
            assert_eq!(encoded.graph, named);
        }
    }
}

/// B10. `with_views` answers shared and own ids in one call.
#[test]
fn with_views_answers_mixed_ids_in_one_call() {
    let budget = unlimited();
    let (_dictionary, overlay) = with_overlay(8, &budget);
    let own = overlay.intern(literal("own").as_ref()).unwrap();
    let view = Vocabulary::Private(overlay.freeze().unwrap());
    let shared = TermId::new(TermKind::String, 5);
    let texts = view.with_views(0, |views| {
        [shared, own].map(|id| views(id).and_then(|v| v.str().map(str::to_owned)))
    });
    assert_eq!(texts, [Some("old 5".to_owned()), Some("own".to_owned())]);
}

/// B11. `matching_strings` over a private view: the shared matches below start, then the own
/// ones, sorted; the scan counts the own keys it tested.
#[test]
fn matching_strings_finds_shared_and_own_sorted() {
    let budget = unlimited();
    let (_dictionary, overlay) = with_overlay(4, &budget);
    let own_b = overlay.intern(literal("old own b").as_ref()).unwrap();
    let own_a = overlay.intern(literal("old own a").as_ref()).unwrap();
    overlay.intern(literal("new").as_ref()).unwrap();
    let view = Vocabulary::Private(overlay.freeze().unwrap());
    let test = strings_starting("old ");
    let mut expected: Vec<TermId> = (0..4).map(|i| TermId::new(TermKind::String, i)).collect();
    expected.extend([own_b, own_a]);
    assert_eq!(view.matching_strings(&test, 0), expected);
    let (found, scan) = view.matching_strings_scan(&test, 0);
    assert_eq!(found, expected);
    assert!(scan.keys_tested >= 2, "{scan}");
}

/// B12. A refused intern adds nothing: same charge, not found, the earlier entry intact.
#[test]
fn a_refused_intern_adds_nothing() {
    let budget = Arc::new(Budget::new(4_096));
    let (_dictionary, overlay) = with_overlay(2, &budget);
    let small = overlay.intern(literal("small").as_ref()).unwrap();
    let used = budget.used();
    let long = "y".repeat(8_192);
    assert!(overlay.intern(literal(&long).as_ref()).is_err());
    assert_eq!(budget.used(), used, "the refused intern's charges released");
    assert_eq!(overlay.lookup(literal(&long).as_ref()), None);
    let view = Vocabulary::Private(overlay.freeze().unwrap());
    assert_eq!(view.decode(small), Some(literal("small")));
}

/// B12 (sol-loop r1 S2: what a refused triple intern leaves). The components interned before
/// the refusal stay, each its own entry, found and charged; the triple term isn't added; a later
/// intern of the same component finds the same id.
#[test]
fn a_refused_triple_intern_keeps_the_components_it_added() {
    let budget = Arc::new(Budget::new(4_096));
    let (_dictionary, overlay) = with_overlay(2, &budget);
    let long = "z".repeat(8_192);
    let triple = Triple::new(iri("new subject"), iri("p"), literal(&long));
    assert!(overlay.intern(TermRef::Triple(&triple)).is_err());
    let subject = overlay
        .lookup(iri("new subject").as_ref().into())
        .expect("the subject interned before the refusal stays");
    assert_eq!(subject.payload(), 2, "the overlay's first own id");
    assert!(budget.used() > 0, "it is charged");
    assert_eq!(
        overlay.lookup(TermRef::Triple(&triple)),
        None,
        "the triple isn't added"
    );
    assert_eq!(
        overlay.intern(iri("new subject").as_ref().into()).unwrap(),
        subject,
        "one id per term"
    );
}

/// B11 (sol-loop r1 S1: the order across kinds). Ids sort by kind before payload, so an own
/// simple literal sorts before a shared language-tagged string: the merged matches are sorted
/// as the dictionary's are, whatever kinds they mix.
#[test]
fn matching_strings_keeps_the_dictionarys_order_across_kinds() {
    let budget = unlimited();
    let dictionary = Arc::new(Dictionary::default());
    let string = dictionary.intern(literal("old a").as_ref());
    let tagged =
        dictionary.intern(LiteralRef::new_language_tagged_literal_unchecked("old b", "de").into());
    let overlay = Overlay::over(
        Arc::clone(&dictionary),
        dictionary.len(),
        Arc::clone(&budget),
    );
    let own = overlay.intern(literal("old own").as_ref()).unwrap();
    let view = Vocabulary::Private(overlay.freeze().unwrap());
    let test = StringTest {
        lang_strings: true,
        ..strings_starting("old ")
    };
    let mut expected = vec![string, tagged, own];
    expected.sort_unstable();
    assert_eq!(view.matching_strings(&test, 0), expected);
    assert!(
        own < tagged,
        "this case mixes kinds: the own string sorts before the shared tagged one"
    );
}

/// B13. What an overlay's entries, its table and its views allocate is charged before it is
/// allocated and released with the last holder: the overlay dropped before its view. Its
/// reservations are made inside the measurement (`measure`'s rule), so the overlay is too; its
/// own `Arc` block is the one allocation held uncharged, deliberately (a transaction's fixed
/// state, as the transaction is: design note §11), and exactly that is allowed. Measured, alone.
#[test]
fn an_overlay_and_its_views_hold_only_what_they_charged() {
    if !nrese_exec::measure::alone() {
        return;
    }
    let budget = unlimited();
    let dictionary = Arc::new(Dictionary::default());
    let texts: Vec<String> = (0..5_000).map(|i| format!("term {i}")).collect();
    let triple = Triple::new(iri("s"), iri("p"), literal("o"));
    let overlay_block = 2 * size_of::<usize>() + size_of::<Overlay>();
    let ((), excess) = nrese_exec::measure::measure_within(&budget, || {
        let overlay = Overlay::over(Arc::clone(&dictionary), 0, Arc::clone(&budget));
        for text in &texts {
            overlay
                .intern(LiteralRef::new_simple_literal(text).into())
                .unwrap();
        }
        overlay.intern(TermRef::Triple(&triple)).unwrap();
        let view = overlay.freeze().unwrap();
        drop(overlay);
        assert!(budget.used() > 0, "the view keeps its entries charged");
        drop(view);
    });
    assert_eq!(
        excess, overlay_block,
        "bytes held beyond the charges: the overlay's own block alone"
    );
    assert_eq!(
        budget.used(),
        0,
        "every charge released with its last holder"
    );
}

/// B16. An overlay's intern reads the dictionary with recursive read guards and takes only its
/// own leaf lock: it returns while a reader holds the dictionary and a writer is queued behind it.
///
/// - Input: a reader holding the dictionary's views; a writer's intern, seen queued behind it
///   (a plain read of the entries then fails: review P2A-F2, 8 Nov, no sleep); then an
///   overlay's intern of a new term.
/// - Expected: the overlay's intern returns before the reader lets go, within [`DEADLINE`],
///   with the overlay's first own id, which its view decodes to that term.
#[test]
fn an_overlay_intern_returns_behind_a_queued_writer() {
    let budget = unlimited();
    let (dictionary, overlay) = with_overlay(4, &budget);
    let (held, done) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let reader = {
        let (dictionary, held, done) = (
            Arc::clone(&dictionary),
            Arc::clone(&held),
            Arc::clone(&done),
        );
        std::thread::spawn(move || {
            dictionary.with_views(u64::MAX, |_| {
                held.store(true, Ordering::Release);
                while !done.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        })
    };
    while !held.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    let writer = {
        let dictionary = Arc::clone(&dictionary);
        std::thread::spawn(move || dictionary.intern(literal("queued").as_ref()))
    };
    let queued = until(DEADLINE, || dictionary.writer_waiting());
    let (sent, answer) = std::sync::mpsc::channel();
    let interning = {
        let overlay = Arc::clone(&overlay);
        std::thread::spawn(move || {
            let id = overlay.intern(literal("behind the writer").as_ref());
            let _ = sent.send(id);
        })
    };
    let id = if queued {
        answer.recv_timeout(DEADLINE)
    } else {
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    };
    done.store(true, Ordering::Release);
    reader.join().unwrap();
    writer.join().unwrap();
    interning.join().unwrap();
    assert!(queued, "the writer queued behind the reader");
    let id = id
        .expect("the overlay's intern returned behind a queued writer")
        .unwrap();
    assert_eq!(id.payload(), 4, "the overlay's first own id");
    assert_eq!(
        overlay.freeze().unwrap().decode(id),
        Some(literal("behind the writer")),
        "the returned id is the term's"
    );
}

/// How long a call that should return may take: far above any run.
const DEADLINE: Duration = Duration::from_secs(10);

/// Waits until `state` holds or `limit` passes; whether it held.
fn until(limit: Duration, state: impl Fn() -> bool) -> bool {
    let start = std::time::Instant::now();
    while !state() {
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

fn view_end(view: &Vocabulary) -> u64 {
    match view {
        Vocabulary::Private(view) => view.end(),
        Vocabulary::Shared(dictionary) => dictionary.len(),
    }
}

/// A simple literal's key length: its kind byte and its text.
fn literal_key_len(text: &str) -> usize {
    1 + text.len()
}

/// Dictionary lifecycle V1, count 2's fix: a lookup through a private vocabulary makes one key
/// for both of its probes (the dictionary's and its own entries'), never a second copy. The
/// counted A/B is `dictlc-pv1-count2c-468e813c` (private lookup 973 to 692 instructions a row,
/// 414,511 to 203,057 allocations); this guards its shape. Measured, alone.
///
/// - Input: an overlay with own entries and its view; an IRI of 100,000 bytes neither the
///   dictionary nor the overlay has, looked up through the overlay and through the view (both
///   probe the dictionary, then the own entries).
/// - Expected: no id, and at most one key held at a time: the lookup's peak below twice the
///   key's length (a second copy of the key for the own probe would hold at least that).
#[test]
fn a_private_lookup_holds_one_key_for_both_probes() {
    if !nrese_exec::measure::alone() {
        return;
    }
    let budget = unlimited();
    let (_dictionary, overlay) = with_overlay(10, &budget);
    for i in 0..10 {
        overlay
            .intern(LiteralRef::new_simple_literal(&format!("own {i}")).into())
            .unwrap();
    }
    let view = overlay.freeze().unwrap();
    let absent = NamedNode::new_unchecked(format!("http://example.com/{}", "x".repeat(100_000)));
    let key = 1 + absent.as_str().len();
    for (name, lookup) in [
        (
            "the overlay",
            &(|term: TermRef<'_>| overlay.lookup(term)) as &dyn Fn(TermRef<'_>) -> Option<TermId>,
        ),
        ("its view", &|term: TermRef<'_>| view.lookup(term)),
    ] {
        let (found, peak) = nrese_exec::measure::measure(|| lookup(absent.as_ref().into()));
        assert_eq!(found, None, "{name}: absent");
        assert!(
            peak < 2 * key,
            "{name}: {peak} bytes held at once for a {key}-byte key: a second copy of it"
        );
    }
}

/// Sol-loop e1 S1: a probe of a key already made allocates nothing on a plain dictionary, so
/// interning a shared term through the overlay charges its key and not the compressed base's
/// probe bound.
///
/// - Input: a plain dictionary holding an IRI whose key is 1,026 bytes, and the two other terms
///   of a quad; an overlay at its length with an 8 KiB budget; the IRI interned through it
///   alone, then as that quad's object.
/// - Expected: its shared id both times, not a refusal. The parent charged 4,104 bytes for the
///   single intern: a lookup's own key and the overlay's copy.
#[test]
fn a_shared_long_term_is_interned_on_a_plain_base_within_its_key() {
    let dictionary = Arc::new(Dictionary::default());
    let long = iri(&"x".repeat(1_016));
    let shared = dictionary.intern(long.as_ref().into());
    let (s, p) = (iri("s"), iri("p"));
    dictionary.intern(s.as_ref().into());
    dictionary.intern(p.as_ref().into());
    let start = dictionary.len();
    let overlay = Overlay::over(Arc::clone(&dictionary), start, Arc::new(Budget::new(8_192)));
    assert_eq!(overlay.intern(long.as_ref().into()), Ok(shared));
    let quad = Quad::new(s, p, long, GraphName::DefaultGraph);
    let encoded = overlay.intern_quad(quad.as_ref());
    assert_eq!(encoded.map(|quad| quad.object), Ok(shared));
}
