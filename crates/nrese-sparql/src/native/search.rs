//! Full-text search in basic graph patterns, with Blazegraph's vocabulary (what
//! ResearchSpace and other Blazegraph clients send):
//!
//! ```sparql
//! PREFIX bds: <http://www.bigdata.com/rdf/search#>
//! SELECT ?s ?o ?score WHERE {
//!   ?o bds:search "tower bridge" ; bds:relevance ?score ; bds:matchAllTerms "true" .
//!   ?s rdfs:label ?o .
//! }
//! ```
//!
//! | Predicate | Object |
//! |---|---|
//! | `bds:search` | the words to find (a literal); a word ending in `*` matches as a prefix |
//! | `bds:relevance` | a variable: the relevance, an `xsd:double` in (0, 1] |
//! | `bds:rank` | a variable: the rank by relevance, from 1 |
//! | `bds:matchAllTerms` | `"true"`: every word must occur (else any) |
//! | `bds:prefixMatch` | `"true"`: every word matches as a prefix |
//! | `bds:minRelevance` | the least relevance to keep |
//! | `bds:minRank`, `bds:maxRank` | the ranks to keep |
//!
//! The subject is the matched literal: a simple or language-tagged string that occurs as
//! the object of a statement in the graph the pattern reads. The matches start the
//! pattern's joins, so the other triple patterns are read for them only.

use nrese_engine::{GraphSelector, QuadPattern, TermId, TextQuery};
use nrese_rdf::{Literal, Term, Variable};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern};

use nrese_exec::IdTable;

use super::{Context, GraphScope, NativeResult, Solutions};

const BDS: &str = "http://www.bigdata.com/rdf/search#";

/// A search: the variable it binds and its options.
#[derive(Debug, Clone)]
pub(super) struct Search {
    matched: Variable,
    query: TextQuery,
    relevance: Option<Variable>,
    rank: Option<Variable>,
    min_relevance: Option<f64>,
    min_rank: Option<usize>,
    max_rank: Option<usize>,
}

/// The local name of a `bds:` predicate.
fn bds(triple: &TriplePattern) -> Option<&str> {
    match &triple.predicate {
        NamedNodePattern::NamedNode(n) => n.as_str().strip_prefix(BDS),
        NamedNodePattern::Variable(_) => None,
    }
}

/// The variable a pattern's subject is (a blank node is one too).
fn subject_variable(term: &TermPattern) -> Option<Variable> {
    match term {
        TermPattern::Variable(v) => Some(v.clone()),
        TermPattern::BlankNode(b) => {
            Some(Variable::new_unchecked(format!("_bnode_{}", b.as_str())))
        }
        _ => None,
    }
}

/// Whether `triple` is part of a search: a `bds:` predicate whose subject some
/// `bds:search` in `triples` searches for.
pub(super) fn is_search(triple: &TriplePattern, triples: &[TriplePattern]) -> bool {
    bds(triple).is_some()
        && triples.iter().any(|t| {
            bds(t) == Some("search")
                && matches!(t.object, TermPattern::Literal(_))
                && subject_variable(&t.subject).is_some()
                && subject_variable(&t.subject) == subject_variable(&triple.subject)
        })
}

/// The searches among `triples` and the other triple patterns; `None` if there is no
/// search.
pub(super) fn split(triples: &[TriplePattern]) -> Option<(Vec<Search>, Vec<TriplePattern>)> {
    let mut searches: Vec<Search> = Vec::new();
    for triple in triples {
        if let (Some("search"), TermPattern::Literal(text), Some(matched)) = (
            bds(triple),
            &triple.object,
            subject_variable(&triple.subject),
        ) {
            searches.push(Search {
                matched,
                query: TextQuery {
                    text: text.value().to_owned(),
                    all_words: false,
                    prefix: false,
                },
                relevance: None,
                rank: None,
                min_relevance: None,
                min_rank: None,
                max_rank: None,
            });
        }
    }
    if searches.is_empty() {
        return None;
    }
    let mut rest = Vec::new();
    for triple in triples {
        let search = subject_variable(&triple.subject)
            .and_then(|v| searches.iter_mut().find(|s| s.matched == v));
        let (Some(name), Some(search)) = (bds(triple), search) else {
            rest.push(triple.clone());
            continue;
        };
        let literal = match &triple.object {
            TermPattern::Literal(l) => Some(l.value()),
            _ => None,
        };
        let variable = match &triple.object {
            TermPattern::Variable(v) => Some(v.clone()),
            _ => None,
        };
        let yes = literal.is_some_and(|l| l.eq_ignore_ascii_case("true"));
        match name {
            "search" => {}
            "relevance" => search.relevance = variable,
            "rank" => search.rank = variable,
            "matchAllTerms" => search.query.all_words = yes,
            "prefixMatch" => search.query.prefix = yes,
            "minRelevance" => search.min_relevance = literal.and_then(|l| l.parse().ok()),
            "minRank" => search.min_rank = literal.and_then(|l| l.parse().ok()),
            "maxRank" => search.max_rank = literal.and_then(|l| l.parse().ok()),
            // Options this implementation doesn't have are ignored, as Blazegraph ignores
            // what it doesn't know.
            _ => {}
        }
    }
    Some((searches, rest))
}

impl Context<'_> {
    /// Whether `id` is the object of a statement in the active graph.
    fn used_as_object(&self, id: TermId) -> bool {
        let graph = match &*self.graph.borrow() {
            GraphScope::Default => GraphSelector::Exact(TermId::DEFAULT_GRAPH),
            GraphScope::Named(g) => GraphSelector::Exact(*g),
            GraphScope::Variable(_) => GraphSelector::AnyNamed,
            GraphScope::Union => GraphSelector::Any,
            GraphScope::Missing => return false,
        };
        let pattern = QuadPattern {
            subject: None,
            predicate: None,
            object: Some(id),
            graph,
        };
        let merge_set = matches!(*self.graph.borrow(), GraphScope::Union)
            .then_some(self.merge_set.as_ref())
            .flatten();
        self.snapshot
            .quads_for_pattern_in(self.model, &pattern)
            .any(|quad| merge_set.is_none_or(|graphs| graphs.contains(&quad.graph)))
    }

    /// The literals `search` matches, with their relevance and rank.
    pub(super) fn search(&self, search: &Search) -> NativeResult<Solutions> {
        self.check()?;
        let mut vars = vec![search.matched.clone()];
        vars.extend(search.relevance.iter().cloned());
        vars.extend(search.rank.iter().cloned());
        let mut table = IdTable::new(vars.len());
        let mut rank = 0usize;
        for found in self.snapshot.text_search(&search.query) {
            if search
                .min_relevance
                .is_some_and(|min| found.relevance < min)
            {
                break;
            }
            let id = TermId::from_raw(found.id);
            if !self.used_as_object(id) {
                continue;
            }
            rank += 1;
            if search.max_rank.is_some_and(|max| rank > max) {
                break;
            }
            if search.min_rank.is_some_and(|min| rank < min) {
                continue;
            }
            let mut row = vec![found.id];
            if search.relevance.is_some() {
                row.push(self.id(&Term::from(Literal::from(found.relevance))));
            }
            if search.rank.is_some() {
                row.push(self.id(&Term::from(Literal::from(rank as i64))));
            }
            table.push_row(&row);
        }
        self.produced(Solutions {
            vars,
            table,
            ordered: false,
        })
    }
}
