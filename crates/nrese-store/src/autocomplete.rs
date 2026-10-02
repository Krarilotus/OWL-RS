//! Autocompletion: the resources whose labels' words, or whose local name's words, begin
//! with what was typed (GraphDB's autocomplete index; the console's search box).
//!
//! Every typed word must begin a word (`alb ein` finds *Albert Einstein*). Labels are the
//! objects of [`LABELS`]; local names are split at camel case and punctuation
//! (`hasPart`, `Albert_Einstein`). Both come from the full-text indexes, built at the first
//! use. A resource found both ways keeps its better score and its label.

use std::collections::HashMap;

use nrese_engine::{GraphSelector, QuadPattern, ReadModel, Snapshot, TermId, TextQuery};
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
) -> Vec<Suggestion> {
    let query = TextQuery {
        text: typed.to_owned(),
        all_words: true,
        prefix: true,
        stem: None,
    };
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
    };
    // Matches come best first: the first `limit` resources of each kind are its best.
    let mut from_labels = 0;
    for hit in snapshot.text_search(&query) {
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
                    keep(quad.subject, hit.relevance, Some(literal));
                    from_labels += 1;
                }
            }
        }
        if from_labels >= limit {
            break;
        }
    }
    let mut from_names = 0;
    for hit in snapshot.iri_search(&query) {
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
    suggestions
}
