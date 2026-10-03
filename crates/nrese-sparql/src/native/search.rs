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
//! | `bds:stem` | a language (`"en"`, `"de"`, ...): words match by their Snowball stem, so `"connect"` finds `connected` and `connection` (an NRESE extension; Blazegraph ignores it) |
//!
//! The subject is the matched literal: a simple or language-tagged string that occurs as
//! the object of a statement in the graph the pattern reads. The matches start the
//! pattern's joins, so the other triple patterns are read for them only.
//!
//! Jena's `text:query` (what Fuseki users send) runs on the same index:
//!
//! ```sparql
//! PREFIX text: <http://jena.apache.org/text#>
//! SELECT ?s ?score ?label WHERE {
//!   (?s ?score ?label) text:query (rdfs:label "tower AND bridge" 10 "lang:en") .
//! }
//! ```
//!
//! The subject is `?s`, or a list of `?s`, the score, the matched literal, its graph and
//! its property (any prefix of them; the graph stays unbound, the property too where the
//! object names it). The object is the query string, or a list of an
//! optional property, the query string, an optional limit on the matched literals, and
//! `"lang:xx"` for the literals' language (other `name:value` options are ignored). The
//! query string is read as Lucene's syntax as far as the index goes: words, phrases in
//! double quotes, `word*` prefixes and fuzzy `word~` (two edits) or `word~1`; `AND` between
//! words makes every word needed (else any); words after `NOT` or `-` are excluded (a phrase
//! after them is dropped); `OR`, `+`, field names and boosts (`^`) are dropped. Without a
//! property, a literal matches as the
//! object of any property (Jena uses the index's default field). The score is the
//! relevance as for `bds:` (the best match has 1), not Lucene's.
//!
//! GraphDB's legacy Lucene predicates likewise: `?x luc:anyIndex "query"` finds the
//! resources with a literal matching the query (Lucene's syntax as above), and
//! `?x luc:score ?s` binds the relevance. The index name is not looked up: there is one
//! index, over every string literal.

use nrese_engine::{GraphSelector, QuadPattern, TermId, TextQuery};
use nrese_rdf::{Literal, Term, Variable, vocab::rdf};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern};

use nrese_exec::IdTable;

use super::{Context, GraphScope, NativeResult, Solutions};

const BDS: &str = "http://www.bigdata.com/rdf/search#";
const JENA_QUERY: &str = "http://jena.apache.org/text#query";
const LUC: &str = "http://www.ontotext.com/owlim/lucene#";

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
    /// Only literals in this language (`text:query`'s `"lang:xx"`).
    language: Option<String>,
}

impl Search {
    fn new(matched: Variable, text: String) -> Self {
        Self {
            matched,
            query: TextQuery {
                text,
                all_words: false,
                prefix: false,
                stem: None,
            },
            relevance: None,
            rank: None,
            min_relevance: None,
            min_rank: None,
            max_rank: None,
            language: None,
        }
    }
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
/// `bds:search` in `triples` searches for, or a `text:query`.
pub(super) fn is_search(triple: &TriplePattern, triples: &[TriplePattern]) -> bool {
    is_jena(triple)
        || luc(triple) == Some("score")
        || (bds(triple).is_some()
            && triples.iter().any(|t| {
                bds(t) == Some("search")
                    && matches!(t.object, TermPattern::Literal(_))
                    && subject_variable(&t.subject).is_some()
                    && subject_variable(&t.subject) == subject_variable(&triple.subject)
            }))
}

/// A `text:query`, or a GraphDB `luc:` search (`?x luc:index "query"`).
fn is_jena(triple: &TriplePattern) -> bool {
    matches!(&triple.predicate, NamedNodePattern::NamedNode(n) if n.as_str() == JENA_QUERY)
        || (luc(triple).is_some_and(|name| !matches!(name, "score" | "snippet"))
            && matches!(triple.object, TermPattern::Literal(_)))
}

/// The local name of a `luc:` predicate.
fn luc(triple: &TriplePattern) -> Option<&str> {
    match &triple.predicate {
        NamedNodePattern::NamedNode(n) => n.as_str().strip_prefix(LUC),
        NamedNodePattern::Variable(_) => None,
    }
}

/// The searches among `triples` and the other triple patterns; `None` if there is no
/// search.
pub(super) fn split(triples: &[TriplePattern]) -> Option<(Vec<Search>, Vec<TriplePattern>)> {
    let (mut searches, triples) = match jena(triples) {
        Some((searches, rest)) => (searches, rest),
        None => (Vec::new(), triples.to_vec()),
    };
    let jena_searches = searches.len();
    for triple in &triples {
        if let (Some("search"), TermPattern::Literal(text), Some(matched)) = (
            bds(triple),
            &triple.object,
            subject_variable(&triple.subject),
        ) {
            searches.push(Search::new(matched, text.value().to_owned()));
        }
    }
    if searches.is_empty() {
        return None;
    }
    let mut rest = Vec::new();
    for triple in &triples {
        let search = subject_variable(&triple.subject).and_then(|v| {
            searches[jena_searches..]
                .iter_mut()
                .find(|s| s.matched == v)
        });
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
            "stem" => search.query.stem = literal.map(str::to_owned),
            // Options this implementation doesn't have are ignored, as Blazegraph ignores
            // what it doesn't know.
            _ => {}
        }
    }
    Some((searches, rest))
}

/// The items of the RDF list `head` as the parser writes `( ... )` in a pattern
/// (`rdf:first` and `rdf:rest` triples on blank nodes), and the indexes of those triples;
/// `None` if `head` isn't such a list.
fn list(head: &TermPattern, triples: &[TriplePattern]) -> Option<(Vec<TermPattern>, Vec<usize>)> {
    let (mut items, mut used) = (Vec::new(), Vec::new());
    let mut node = head.clone();
    loop {
        match &node {
            TermPattern::NamedNode(n) if n.as_str() == rdf::NIL.as_str() => {
                return Some((items, used));
            }
            TermPattern::BlankNode(_) => {}
            _ => return None,
        }
        let link = |property: &str| {
            triples.iter().position(|t| {
                t.subject == node
                    && matches!(&t.predicate, NamedNodePattern::NamedNode(p) if p.as_str() == property)
            })
        };
        let (first, rest) = (link(rdf::FIRST.as_str())?, link(rdf::REST.as_str())?);
        items.push(triples[first].object.clone());
        used.extend([first, rest]);
        node = triples[rest].object.clone();
    }
}

/// Jena's query string as this index's query (module docs), and whether every word is
/// needed.
fn lucene(text: &str) -> (String, bool) {
    let (mut out, mut all_words, mut negate_next) = (Vec::new(), false, false);
    // Phrases stay whole: split outside double quotes only.
    for (i, part) in text.split('"').enumerate() {
        if i % 2 == 1 {
            // A phrase after `-` or `NOT` is dropped: the index excludes words only.
            if !std::mem::take(&mut negate_next) {
                out.push(format!("\"{part}\""));
            }
            continue;
        }
        for token in part.split_whitespace() {
            match token {
                "AND" | "&&" => all_words = true,
                "OR" | "||" => {}
                "NOT" | "!" | "-" => negate_next = true,
                _ => {
                    let negated = std::mem::take(&mut negate_next) || token.starts_with('-');
                    let token = token.trim_start_matches(['+', '-']);
                    // A field name and boosts go; fuzziness stays.
                    let token = token.rsplit_once(':').map_or(token, |(_, word)| word);
                    let token = token.split('^').next().unwrap_or_default();
                    if !token.is_empty() {
                        out.push(match negated {
                            true => format!("-{token}"),
                            false => token.to_owned(),
                        });
                    }
                }
            }
        }
        // A `-` right before a phrase.
        if part.ends_with('-') {
            negate_next = true;
        }
    }
    (out.join(" "), all_words)
}

/// The `text:query` patterns among `triples` as searches, each with the triple pattern
/// that joins its literal to the subject (`?s <property> ?literal`), and the other triple
/// patterns; `None` if there is none.
fn jena(triples: &[TriplePattern]) -> Option<(Vec<Search>, Vec<TriplePattern>)> {
    if !triples.iter().any(is_jena) {
        return None;
    }
    let (mut searches, mut joins, mut consumed) = (Vec::new(), Vec::new(), Vec::new());
    for (index, triple) in triples.iter().enumerate().filter(|(_, t)| is_jena(t)) {
        let n = searches.len();
        let fresh = |what: &str| Variable::new_unchecked(format!("_text_{what}_{n}"));
        let variable = |term: &TermPattern| match term {
            TermPattern::Variable(v) => Some(v.clone()),
            _ => None,
        };
        // The subject: ?s, or (?s ?score ?literal ?graph ?property).
        let (outputs, used) = match list(&triple.subject, triples) {
            Some(found) => found,
            None => (vec![triple.subject.clone()], Vec::new()),
        };
        consumed.extend(used);
        let subject = outputs.first().cloned()?;
        // The object: "query", or (property? "query" limit? "name:value"*).
        let (arguments, used) = match list(&triple.object, triples) {
            Some(found) => found,
            None => (vec![triple.object.clone()], Vec::new()),
        };
        consumed.extend(used);
        let mut arguments = arguments.into_iter().peekable();
        let property = match arguments.peek() {
            Some(TermPattern::NamedNode(p)) => {
                let p = p.clone();
                arguments.next();
                Some(p)
            }
            _ => None,
        };
        let Some(TermPattern::Literal(text)) = arguments.next() else {
            return None;
        };
        let (text, all_words) = lucene(text.value());
        let literal = outputs
            .get(2)
            .and_then(variable)
            .unwrap_or_else(|| fresh("literal"));
        let mut search = Search::new(literal.clone(), text);
        search.query.all_words = all_words;
        search.relevance = outputs.get(1).and_then(variable);
        // GraphDB: the score is a statement on the subject.
        if luc(triple).is_some()
            && let Some(score) = triples.iter().position(|t| {
                luc(t) == Some("score") && t.subject == subject && variable(&t.object).is_some()
            })
        {
            search.relevance = variable(&triples[score].object);
            consumed.push(score);
        }
        for argument in arguments {
            let TermPattern::Literal(value) = argument else {
                continue;
            };
            let value = value.value();
            if let Ok(limit) = value.parse::<usize>() {
                search.max_rank = Some(limit);
            } else if let Some(language) = value.strip_prefix("lang:") {
                search.language = Some(language.to_owned());
            }
        }
        // The property: given, or any (bound to the fifth output if asked for).
        let predicate: NamedNodePattern = match (&property, outputs.get(4).and_then(variable)) {
            (Some(p), _) => NamedNodePattern::NamedNode(p.clone()),
            (None, Some(v)) => NamedNodePattern::Variable(v),
            (None, None) => NamedNodePattern::Variable(fresh("property")),
        };
        joins.push(TriplePattern {
            subject,
            predicate,
            object: TermPattern::Variable(literal),
        });
        consumed.push(index);
        searches.push(search);
    }
    let rest = triples
        .iter()
        .enumerate()
        .filter(|(i, _)| !consumed.contains(i))
        .map(|(_, t)| t.clone())
        .chain(joins)
        .collect();
    Some((searches, rest))
}

impl Context<'_> {
    /// Whether `id` is the object of a statement in the active graph.
    pub(super) fn used_as_object(&self, id: TermId) -> bool {
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
            if let Some(language) = &search.language {
                let in_language = matches!(
                    self.term(found.id),
                    Some(Term::Literal(l)) if l.language().is_some_and(|l| l.eq_ignore_ascii_case(language))
                );
                if !in_language {
                    continue;
                }
            }
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
