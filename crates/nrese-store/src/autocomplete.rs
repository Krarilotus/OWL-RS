//! Autocompletion: the resources whose labels' words, or whose local name's words, begin
//! with what was typed (GraphDB's autocomplete index; the console's search box).
//!
//! Every typed word must begin a word (`alb ein` finds *Albert Einstein*). Labels are the
//! objects of [`LABELS`]; local names are split at camel case and punctuation
//! (`hasPart`, `Albert_Einstein`). Both come from the full-text indexes, built at the first
//! use. A resource found both ways keeps its better score and its label. Scores are ranked
//! among what the snapshot holds (`Snapshot::text_search_in`), never what an open
//! transaction or a later version interned.

use std::collections::HashMap;

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId, TextQuery, TextScope};
use nrese_rdf::{NamedNodeRef, Term};

/// The properties whose values are labels.
pub const LABELS: [&str; 7] = [
    "http://www.w3.org/2000/01/rdf-schema#label",
    "http://www.w3.org/2004/02/skos/core#prefLabel",
    "http://www.w3.org/2004/02/skos/core#altLabel",
    "http://xmlns.com/foaf/0.1/name",
    "http://schema.org/name",
    "http://purl.org/dc/terms/title",
    "http://purl.org/dc/elements/1.1/title",
];

/// One suggestion.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// The resource.
    pub iri: String,
    /// The label that matched, if one did.
    pub label: Option<String>,
    /// The relevance in (0, 1]: the best match of its kind has 1.
    pub score: f64,
}

/// At most `limit` resources for `typed`, best first.
pub(crate) fn autocomplete(
    snapshot: &Snapshot,
    model: ReadModel,
    typed: &str,
    limit: usize,
) -> nrese_engine::EngineResult<Vec<Suggestion>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let query = TextQuery::plain_prefix(typed);
    let labels: Vec<TermId> = LABELS
        .iter()
        .filter_map(|iri| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into()))
        .collect();
    // Resource → its score and the label that matched.
    let mut found: HashMap<TermId, (f64, Option<TermId>)> = HashMap::new();
    let mut keep = |resource: TermId, score: f64, label: Option<TermId>| {
        let entry = found.entry(resource).or_insert((score, label));
        if score > entry.0 {
            entry.0 = score;
        }
        if entry.1.is_none() {
            entry.1 = label;
        }
        found.len()
    };
    // Both indexes at once (each built at its first use, on every core); each ranks among
    // what `model` reads in every graph: the string literals that are objects, the IRIs
    // that are subjects.
    let (label_hits, name_hits) = rayon::join(
        || snapshot.text_search_in(model, &TextScope::EVERY, &query, None),
        || snapshot.iri_search_in(model, &TextScope::EVERY, &query, None),
    );
    let (label_hits, name_hits) = (label_hits?, name_hits?);
    // Matches come best first: the first `limit` distinct resources of each kind are
    // its best. Finish each literal's quads before applying the cutoff, as before.
    let mut from_labels = 0;
    for &hit in &label_hits {
        let literal = TermId::from_raw(hit.id);
        for &property in &labels {
            let pattern = QuadPattern {
                subject: None,
                predicate: Some(property),
                object: Some(literal),
                graph: GraphSelector::Any,
            };
            for quad in snapshot.quads_for_pattern_in(model, &pattern) {
                if quad.subject.kind() == nrese_engine::TermKind::Iri {
                    // Name hits are merged later, so this counts label resources only.
                    from_labels = keep(quad.subject, hit.relevance, Some(literal));
                }
            }
        }
        if from_labels >= limit {
            break;
        }
    }
    let mut from_names = 0;
    for &hit in &name_hits {
        let iri = TermId::from_raw(hit.id);
        let described = QuadPattern {
            subject: Some(iri),
            predicate: None,
            object: None,
            graph: GraphSelector::Any,
        };
        if snapshot
            .quads_for_pattern_in(model, &described)
            .next()
            .is_some()
        {
            keep(iri, hit.relevance, None);
            from_names += 1;
            if from_names >= limit {
                break;
            }
        }
    }
    let mut suggestions: Vec<Suggestion> = found
        .into_iter()
        .filter_map(|(resource, (score, label))| {
            let Some(Term::NamedNode(iri)) = snapshot.decode(resource) else {
                return None;
            };
            let label = label.and_then(|label| match snapshot.decode(label) {
                Some(Term::Literal(literal)) => Some(literal.value().to_owned()),
                _ => None,
            });
            Some(Suggestion {
                iri: iri.into_string(),
                label,
                score,
            })
        })
        .collect();
    suggestions.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.label.is_none().cmp(&b.label.is_none()))
            .then_with(|| a.iri.cmp(&b.iri))
    });
    suggestions.truncate(limit);
    Ok(suggestions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nrese_engine::{Engine, EngineConfig};
    use nrese_rdf_io::{RdfFormat, RdfParser};

    fn fixture() -> Engine {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        // Neither resource's local name matches "zeph". Each literal belongs to only
        // one resource, repeated across label properties and graphs, so whichever
        // literal ranks first must not exhaust a two-resource limit by itself.
        let data = "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
            @prefix skos: <http://www.w3.org/2004/02/skos/core#> .
            <urn:resource:a> rdfs:label \"Zephyr alpha\" ; skos:prefLabel \"Zephyr alpha\" .
            <urn:resource:b> rdfs:label \"Zephyr beta\" ; skos:prefLabel \"Zephyr beta\" .
            <urn:graph> {
                <urn:resource:a> rdfs:label \"Zephyr alpha\" .
                <urn:resource:b> rdfs:label \"Zephyr beta\" .
            }";
        let mut tx = engine.transaction();
        for quad in RdfParser::from_format(RdfFormat::TriG).for_reader(data.as_bytes()) {
            tx.insert(quad.unwrap().as_ref());
        }
        tx.commit().unwrap();
        engine
    }

    #[test]
    fn label_quad_duplicates_do_not_exhaust_the_resource_limit() {
        let engine = fixture();
        let snapshot = engine.snapshot();
        let suggestions = autocomplete(&snapshot, ReadModel::Asserted, "zeph", 2).unwrap();
        let mut resources: Vec<_> = suggestions.iter().map(|hit| hit.iri.as_str()).collect();
        resources.sort_unstable();
        assert_eq!(resources, ["urn:resource:a", "urn:resource:b"]);
        for hit in &suggestions {
            let expected = if hit.iri == "urn:resource:a" {
                "Zephyr alpha"
            } else {
                "Zephyr beta"
            };
            assert_eq!(hit.label.as_deref(), Some(expected));
            assert!(hit.score > 0.0 && hit.score <= 1.0);
        }
        assert_eq!(
            autocomplete(&snapshot, ReadModel::Asserted, "zeph", 1)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn zero_limit_returns_no_suggestions() {
        let engine = fixture();
        assert!(
            autocomplete(&engine.snapshot(), ReadModel::Asserted, "zeph", 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn typed_input_is_plain_word_prefixes_including_unicode() {
        let engine = fixture();
        let mut tx = engine.transaction();
        let label = NamedNodeRef::new_unchecked(LABELS[0]);
        for (iri, text) in [
            ("urn:label", "İstanbul"),
            ("urn:İstanbul", "other"),
            ("urn:hasPart", "other"),
        ] {
            tx.insert(nrese_rdf::QuadRef::new(
                NamedNodeRef::new_unchecked(iri),
                label,
                nrese_rdf::LiteralRef::new_simple_literal(text),
                nrese_rdf::GraphNameRef::DefaultGraph,
            ));
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let query = |typed: &str| autocomplete(&snapshot, ReadModel::Asserted, typed, 10).unwrap();
        for typed in [
            "zeph-al",
            "zeph&al",
            "\"zeph al",
            "zeph~ al",
            "-zeph al",
            "zeph* al",
        ] {
            assert_eq!(query(typed), query("zeph al"), "{typed}");
        }
        assert_eq!(query("zeph al").len(), 1);
        for typed in ["has:part", "has-part", "\"has part", "has~ part"] {
            assert_eq!(query(typed), query("has part"), "{typed}");
        }
        assert_eq!(query("has part").len(), 1);
        assert!(query("!~*\"").is_empty());
        assert_eq!(query("İstan").len(), 2);
    }

    fn label(subject: &str, text: &str) -> nrese_rdf::Quad {
        use nrese_rdf::{GraphName, Literal, NamedNode, Quad};
        Quad::new(
            NamedNode::new_unchecked(format!("http://example.com/{subject}")),
            NamedNode::new_unchecked(LABELS[0]),
            Literal::new_simple_literal(text),
            GraphName::DefaultGraph,
        )
    }

    /// Scores rank among what the snapshot holds: an open transaction's labels and local
    /// names, interned in the shared dictionary, change none.
    #[test]
    fn uncommitted_labels_change_no_score() {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        tx.insert(label("s1", "Beta").as_ref());
        tx.insert(label("s2", "Beta gamma delta epsilon").as_ref());
        tx.insert(label("s3", "Other words").as_ref());
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let before = autocomplete(&snapshot, ReadModel::Materialised, "bet", 10).unwrap();
        assert_eq!(before.len(), 2, "{before:?}");
        assert!(before[1].score < 1.0);
        let mut tx = engine.transaction();
        for i in 0..40 {
            let words = format!("beta {}", "filler ".repeat(20));
            tx.insert(label(&format!("p{i}"), &words).as_ref());
        }
        assert_eq!(
            autocomplete(&snapshot, ReadModel::Materialised, "bet", 10).unwrap(),
            before
        );
        drop(tx);
    }
}
