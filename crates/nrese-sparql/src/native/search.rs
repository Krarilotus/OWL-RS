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
//! double quotes, `word*` prefixes and fuzzy `word~` (two edits), `word~0` (exact),
//! `word~1` or `word~2`. Flat AND or OR groups are supported; unconnected positive
//! terms mean OR. NOT and `-` exclude plain words. Mixed Boolean modes, required
//! clauses, fields, boosts, grouping and negated phrases/fuzzy/prefix terms are argument
//! errors. Quoted punctuation is ordinary phrase text. The property, active graph and
//! read model restrict eligible literals before rank limits; the join retains matching
//! subjects. Without a property, any predicate can qualify a literal. Under `GRAPH ?g` a
//! search runs in each named graph the query may read, its rows binding `?g`, its ranks
//! that graph's. The score is the relevance as for `bds:` (the best match has 1), not
//! Lucene's.
//!
//! GraphDB's legacy Lucene predicates likewise: `?x luc:anyIndex "query"` finds the
//! resources with a literal matching the query (Lucene's syntax as above), and
//! `?x luc:score ?s` binds the relevance. The index name is not looked up: there is one
//! index, over every string literal.

use std::collections::HashSet;

use nrese_engine::{GraphSelector, QuadPattern, TermId, TextQuery};
use nrese_rdf::{Literal, NamedNode, Term, Variable, vocab::rdf};
use nrese_sparql_syntax::term::{NamedNodePattern, TermPattern, TriplePattern};

use nrese_exec::IdTable;

use super::{Context, GraphScope, NativeResult, Solutions};
use crate::QueryEvaluationError;

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
    /// Only literals that are objects of this property (`text:query`'s property).
    property: Option<NamedNode>,
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
            property: None,
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

type SearchSplit = (Vec<Search>, Vec<TriplePattern>);

/// The searches and remaining patterns; `None` only when there is no search. Invalid
/// searches stay recognised by planner guards and report their error at evaluation.
pub(super) fn split(triples: &[TriplePattern]) -> Option<NativeResult<SearchSplit>> {
    let (mut searches, triples) = match jena(triples) {
        Ok(Some(found)) => found,
        Ok(None) => (Vec::new(), triples.to_vec()),
        Err(error) => return Some(Err(error)),
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
    Some(Ok((searches, rest)))
}

/// The items of the RDF list `head` as the parser writes `( ... )` in a pattern
/// (`rdf:first` and `rdf:rest` triples on blank nodes), and the indexes of those triples;
/// `None` if `head` isn't such a list; malformed or cyclic lists are query errors.
fn list(
    head: &TermPattern,
    triples: &[TriplePattern],
) -> NativeResult<Option<(Vec<TermPattern>, Vec<usize>)>> {
    let (mut items, mut used) = (Vec::new(), Vec::new());
    let mut seen = HashSet::new();
    let mut node = head;
    let invalid = || QueryEvaluationError::Argument("malformed text:query RDF list".into());
    loop {
        match node {
            TermPattern::NamedNode(n) if n.as_str() == rdf::NIL.as_str() => {
                return Ok(Some((items, used)));
            }
            TermPattern::BlankNode(b) if seen.insert(b) => {}
            _ if items.is_empty() => return Ok(None),
            _ => return Err(invalid().into()),
        }
        let mut link = |property: &str| -> NativeResult<Option<&TermPattern>> {
            let mut value = None;
            for (index, triple) in triples.iter().enumerate().filter(|(_, t)| {
                &t.subject == node
                    && matches!(&t.predicate, NamedNodePattern::NamedNode(p) if p.as_str() == property)
            }) {
                if value.is_some_and(|value| value != &triple.object) {
                    return Err(invalid().into());
                }
                value = Some(&triple.object);
                // Identical repeated links describe the same list, but all must be consumed.
                used.push(index);
            }
            Ok(value)
        };
        let (first, rest) = (link(rdf::FIRST.as_str())?, link(rdf::REST.as_str())?);
        let (first, rest) = match (first, rest) {
            (Some(first), Some(rest)) => (first, rest),
            (None, None) if items.is_empty() => return Ok(None),
            _ => return Err(invalid().into()),
        };
        items.push(first.clone());
        node = rest;
    }
}

/// Jena's query string as this index's query (module docs), and whether every word is
/// needed.
fn lucene(text: &str) -> NativeResult<(String, bool)> {
    let invalid = || QueryEvaluationError::Argument("unsupported or malformed Lucene query".into());
    let (mut out, mut mode, mut operator) = (Vec::new(), None, None);
    let (mut negate_next, mut positive) = (false, false);
    let mut rest = text.trim();
    while !rest.is_empty() {
        // Quoted punctuation is literal text, never a field, boost or Boolean operator.
        let phrase = rest.starts_with('"');
        let end = if phrase {
            rest[1..].find('"').ok_or_else(invalid)? + 2
        } else {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        };
        let (token, tail) = rest.split_at(end);
        if !tail.is_empty() && !tail.starts_with(char::is_whitespace) {
            return Err(invalid().into());
        }
        rest = tail.trim_start();
        match token {
            "AND" | "&&" | "OR" | "||" => {
                if out.is_empty() || operator.is_some() || negate_next {
                    return Err(invalid().into());
                }
                operator = Some(matches!(token, "AND" | "&&"));
                continue;
            }
            "NOT" | "!" | "-" => {
                if negate_next {
                    return Err(invalid().into());
                }
                negate_next = true;
                continue;
            }
            _ => {}
        }
        let negated = negate_next || token.starts_with('-');
        let operand = token
            .strip_prefix('-')
            .unwrap_or(token)
            .split('~')
            .next()
            .unwrap_or_default();
        if (negate_next && token.starts_with('-'))
            || token.starts_with("--")
            || (negated && (phrase || operator == Some(false)))
            || (!phrase && matches!(operand, "AND" | "OR" | "NOT"))
            || (!phrase
                && token.contains([
                    ':', '^', '(', ')', '[', ']', '{', '}', '+', '!', '&', '|', '/', '\\', '"',
                ]))
        {
            return Err(invalid().into());
        }
        // Implicit OR between positive clauses cannot be mixed with explicit AND.
        let link = operator
            .take()
            .or_else(|| (positive && (!negated || mode != Some(true))).then_some(false));
        if let Some(link) = link {
            if mode.is_some_and(|mode| mode != link) {
                return Err(invalid().into());
            }
            mode = Some(link);
        }
        positive |= !negated;
        out.push(if std::mem::take(&mut negate_next) {
            format!("-{token}")
        } else {
            token.to_owned()
        });
    }
    if operator.is_some() || negate_next {
        return Err(invalid().into());
    }
    Ok((out.join(" "), mode.unwrap_or(false)))
}

/// The `text:query` patterns among `triples` as searches, each with the triple pattern
/// that joins its literal to the subject (`?s <property> ?literal`), and the other triple
/// patterns; `None` if there is none.
fn jena(triples: &[TriplePattern]) -> NativeResult<Option<SearchSplit>> {
    if !triples.iter().any(is_jena) {
        return Ok(None);
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
        let (outputs, used) = match list(&triple.subject, triples)? {
            Some(found) => found,
            None => (vec![triple.subject.clone()], Vec::new()),
        };
        consumed.extend(used);
        let subject = outputs
            .first()
            .cloned()
            .ok_or_else(|| QueryEvaluationError::Argument("text:query needs a subject".into()))?;
        // The object: "query", or (property? "query" limit? "name:value"*).
        let (arguments, used) = match list(&triple.object, triples)? {
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
            return Err(
                QueryEvaluationError::Argument("text:query needs a query literal".into()).into(),
            );
        };
        let (text, all_words) = lucene(text.value())?;
        let literal = outputs
            .get(2)
            .and_then(variable)
            .unwrap_or_else(|| fresh("literal"));
        let mut search = Search::new(literal.clone(), text);
        search.query.all_words = all_words;
        search.relevance = outputs.get(1).and_then(variable);
        search.property = property.clone();
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
    Ok(Some((searches, rest)))
}

impl Context<'_> {
    /// Whether `id` is the object of a statement in the active graph.
    pub(super) fn used_as_object(&self, id: TermId) -> bool {
        self.used_as_object_for_property(id, None)
    }

    /// Whether `id` is the object of a statement in the active graph, of `property` if
    /// given.
    fn used_as_object_for_property(&self, id: TermId, property: Option<TermId>) -> bool {
        let graph = match &*self.graph.borrow() {
            GraphScope::Default => GraphSelector::Exact(TermId::DEFAULT_GRAPH),
            GraphScope::Named(g) => GraphSelector::Exact(*g),
            GraphScope::Variable(_) => GraphSelector::AnyNamed,
            GraphScope::Union => GraphSelector::Any,
            GraphScope::Missing => return false,
        };
        let pattern = QuadPattern {
            subject: None,
            predicate: property,
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
        // GRAPH ?g: one search per named graph the query may read (`search_each_graph`).
        let each = match &*self.graph.borrow() {
            GraphScope::Variable(variable) => Some(variable.clone()),
            _ => None,
        };
        if let Some(variable) = each {
            return self.search_each_graph(search, &variable);
        }
        let eligible = self.search_eligible(search);
        let mut vars = vec![search.matched.clone()];
        vars.extend(search.relevance.iter().cloned());
        vars.extend(search.rank.iter().cloned());
        let mut table = IdTable::new(vars.len());
        let mut rank = 0usize;
        let matches = self
            .snapshot
            .text_search(&search.query)
            .map_err(QueryEvaluationError::from)?;
        for found in matches {
            self.check()?;
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
            if !eligible(id) {
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

    /// Which literals `search` may rank, decided before its rank limits: objects of a
    /// statement in the active graph, of its property if it names one (a property the
    /// store doesn't know: none).
    fn search_eligible(&self, search: &Search) -> impl Fn(TermId) -> bool + '_ {
        let property = search
            .property
            .as_ref()
            .map(|p| self.snapshot.lookup(p.as_ref().into()));
        move |id| match property {
            None => self.used_as_object(id),
            Some(None) => false,
            Some(Some(property)) => self.used_as_object_for_property(id, Some(property)),
        }
    }

    /// `search` under `GRAPH ?variable`: in each named graph the query may read (its
    /// dataset's, else the store's), each graph's rows binding `?variable` before filters
    /// and joins run, ranks and rank limits that graph's. A graph, literal or score
    /// variable reused must agree with both bindings.
    fn search_each_graph(&self, search: &Search, variable: &Variable) -> NativeResult<Solutions> {
        let mut graphs = self
            .named
            .clone()
            .unwrap_or_else(|| self.snapshot.named_graphs().collect());
        graphs.sort_unstable();
        graphs.dedup();
        let mut vars = vec![search.matched.clone()];
        vars.extend(search.relevance.iter().cloned());
        vars.extend(search.rank.iter().cloned());
        if !vars.contains(variable) {
            vars.push(variable.clone());
        }
        let mut all = Solutions {
            table: IdTable::new(vars.len()),
            vars,
            ordered: false,
        };
        for graph in graphs {
            let outer = self.graph.replace(GraphScope::Named(graph));
            let found = self.search(search);
            self.graph.replace(outer);
            let found = found?;
            let binding = self.produced(Solutions {
                vars: vec![variable.clone()],
                table: IdTable::from_columns(vec![vec![graph.raw()]]),
                ordered: false,
            })?;
            let found = self.join(found, binding)?;
            all = self.union(all, found)?;
        }
        Ok(all)
    }
}
