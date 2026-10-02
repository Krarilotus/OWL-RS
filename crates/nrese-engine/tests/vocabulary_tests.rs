//! `store.vocabulary = "fsst"` (`nrese_engine::VocabularyEncoding`): a checkpoint keeps the
//! dictionary's keys FSST-compressed, and the store reads as before: every term looked up
//! and decoded, string scans, prefix and full-text searches, terms interned after the
//! checkpoint, a second checkpoint over the compressed one, and a full verification on
//! open. A process of its own: the encoding is set for the process.

use nrese_engine::{
    DurabilityConfig, Engine, EngineConfig, Placement, QuadPattern, StringTest, TextQuery,
    VocabularyEncoding, set_vocabulary_encoding,
};
use nrese_rdf::vocab::xsd;
use nrese_rdf::{BaseDirection, BlankNode, GraphName, Literal, NamedNode, Quad, Term, Triple};

fn config() -> EngineConfig {
    EngineConfig {
        background_maintenance: false,
        durability: DurabilityConfig {
            verify_on_open: true,
            ..DurabilityConfig::default()
        },
        ..EngineConfig::default()
    }
}

/// Every kind of term the dictionary holds, many of them alike (what FSST compresses).
fn quads(round: u64) -> Vec<Quad> {
    let iri = |s: String| NamedNode::new_unchecked(s);
    (0..3_000u64)
        .map(|n| {
            let n = n + round * 10_000;
            let object: Term = match n % 7 {
                0 => iri(format!("http://www.wikidata.org/entity/Q{n}")).into(),
                1 => Literal::new_simple_literal(format!("a label of item {n}, \u{e9}t\u{e9}")).into(),
                2 => Literal::new_language_tagged_literal(format!("Windm\u{fc}hle {n}"), "de")
                    .unwrap()
                    .into(),
                3 => Literal::new_directional_language_tagged_literal(
                    format!("\u{5e9}\u{5dc}\u{5d5}\u{5dd} {n}"),
                    "he",
                    BaseDirection::Rtl,
                )
                .unwrap()
                .into(),
                4 => Literal::new_typed_literal(format!("{n}.5"), xsd::DOUBLE).into(),
                5 => BlankNode::new_unchecked(format!("b{n}")).into(),
                _ => Triple::new(
                    iri(format!("http://example.com/s{n}")),
                    iri("http://example.com/p".to_owned()),
                    Literal::new_simple_literal(format!("quoted {n}")),
                )
                .into(),
            };
            Quad::new(
                iri(format!("http://example.com/s{n}")),
                iri(format!("http://example.com/p{}", n % 5)),
                object,
                GraphName::DefaultGraph,
            )
        })
        .collect()
}

fn insert(engine: &Engine, quads: &[Quad]) {
    let mut tx = engine.transaction();
    for quad in quads {
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
}

/// Every quad is there, each term found by lookup and decoded back to itself.
fn check(engine: &Engine, quads: &[Quad]) {
    let snapshot = engine.snapshot();
    for quad in quads {
        let encoded = snapshot
            .lookup_quad(quad.as_ref())
            .unwrap_or_else(|| panic!("{quad} is looked up"));
        assert!(snapshot.contains(&encoded), "{quad}");
        assert_eq!(snapshot.decode_quad(encoded).as_ref(), Some(quad));
    }
}

#[test]
fn compressed_vocabularies_read_as_plain_ones() {
    set_vocabulary_encoding(VocabularyEncoding::Fsst);
    let dir = tempfile::tempdir().unwrap();
    let first = quads(0);
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        insert(&engine, &first);
        engine.checkpoint().unwrap();
        // Served from the compressed checkpoint now.
        check(&engine, &first);
    }
    let second = quads(1);
    {
        let engine = Engine::open(dir.path(), config()).unwrap();
        check(&engine, &first);
        let snapshot = engine.snapshot();
        let stats = snapshot.dictionary_bytes();
        assert!(stats > 0);
        // A scan over every key: the German labels, by a substring.
        let test = StringTest {
            needle: "windm\u{fc}hle 1",
            placement: Placement::Start,
            iris: false,
            strings: false,
            lang_strings: true,
            typed: false,
            language: None,
            ascii_case_insensitive: false,
        };
        let by_prefix = snapshot.matching_strings(&test);
        let anywhere = snapshot.matching_strings(&StringTest {
            needle: "m\u{fc}hle 1",
            placement: Placement::Anywhere,
            ..test
        });
        let expected = first
            .iter()
            .filter(|q| {
                matches!(&q.object, Term::Literal(l)
                    if l.language() == Some("de") && l.value().starts_with("Windm\u{fc}hle 1"))
            })
            .count();
        assert!(expected > 0);
        // The prefix search is case-sensitive: none of the capitalised labels.
        assert_eq!(by_prefix.len(), 0);
        assert_eq!(anywhere.len(), expected);
        // Full-text search over the decoded strings.
        let found = snapshot.text_search(&TextQuery {
            text: "windm\u{fc}hle".to_owned(),
            all_words: true,
            prefix: false,
            stem: None,
        });
        assert_eq!(found.len(), first.len() / 7 + usize::from(first.len() % 7 > 2));
        // Views of decoded keys, many under one lock.
        let ids: Vec<_> = first
            .iter()
            .take(50)
            .map(|q| snapshot.lookup(q.object.as_ref()).unwrap())
            .collect();
        let texts = snapshot.with_views(|view| {
            ids.iter()
                .filter_map(|&id| view(id).and_then(|v| v.str().map(str::to_owned)))
                .count()
        });
        assert!(texts > 20);
        // Terms interned after the checkpoint, then a checkpoint over the compressed one.
        insert(&engine, &second);
        check(&engine, &second);
        engine.checkpoint().unwrap();
        check(&engine, &first);
    }
    let engine = Engine::open(dir.path(), config()).unwrap();
    check(&engine, &first);
    check(&engine, &second);
    assert_eq!(
        engine.snapshot().quads_for_pattern(&QuadPattern::all()).count(),
        first.len() + second.len()
    );
    let checkpoint = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "nck"))
        .max()
        .unwrap();
    // The flags' bit 1: the keys are compressed.
    let header = std::fs::read(checkpoint).unwrap();
    assert_eq!(&header[..8], b"NRESECKB");
    assert_eq!(u64::from_le_bytes(header[16..24].try_into().unwrap()) & 2, 2);
}
