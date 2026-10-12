use super::*;

/// What a query reads and whether it is monotone.
#[derive(Debug, Default)]
pub(crate) struct Analysis {
    /// `(predicate, class)` per triple pattern and path step: `None` for a variable (or a
    /// negated property set, which reads any predicate); the class for `rdf:type` with a
    /// constant object.
    reads: Vec<(Option<String>, Option<String>)>,
    /// The first operator that isn't monotone (or isn't under entailment).
    pub(super) not_monotone: Option<&'static str>,
    /// The single basic graph pattern with filters the exact services take.
    pub(super) shape: Option<Shape>,
    /// Each triple pattern and path step with its ends.
    atoms: Vec<Atom>,
    /// The answer variables: a `SELECT`'s projection, none for `ASK`; `None` for the
    /// other forms.
    answers: Option<HashSet<String>>,
    /// Variables an expression computes from (`BIND`, `SELECT (… AS ?v)`): a value
    /// derived from one may be no term of its own.
    computed: HashSet<String>,
}

/// A triple pattern or path step: what it reads and what stands at its ends.
#[derive(Debug, Clone)]
struct Atom {
    predicate: Option<String>,
    class: Option<String>,
    ends: [End; 2],
}

/// One end of an [`Atom`], as the gap's Skolem constants see it.
#[derive(Debug, Clone)]
enum End {
    /// A term the query names: never a Skolem constant.
    Term,
    Var(String),
    /// A blank node, a path's inner node, a quoted triple, or one of U1's own terms.
    Hidden,
}

fn end_of(term: &TermPattern) -> End {
    match term {
        TermPattern::NamedNode(n) if n.as_str().starts_with(U1) => End::Hidden,
        TermPattern::NamedNode(_) | TermPattern::Literal(_) => End::Term,
        TermPattern::Variable(v) => End::Var(v.as_str().to_owned()),
        _ => End::Hidden,
    }
}

fn atom_of(t: &TriplePattern) -> Atom {
    let (predicate, class) = pattern_reads(t);
    Atom {
        predicate,
        class,
        ends: [end_of(&t.subject), end_of(&t.object)],
    }
}

/// The answer variables of a `SELECT` (none for `ASK`).
fn answer_variables(query: &Query) -> Option<HashSet<String>> {
    let pattern = match query {
        Query::Select { pattern, .. } => pattern,
        Query::Ask { .. } => return Some(HashSet::new()),
        _ => return None,
    };
    let mut p = pattern;
    loop {
        match p {
            GraphPattern::Distinct { inner } | GraphPattern::Reduced { inner } => p = inner,
            GraphPattern::Project { variables, .. } => {
                return Some(variables.iter().map(|v| v.as_str().to_owned()).collect());
            }
            _ => return None,
        }
    }
}

/// A query that is one basic graph pattern under filters, with the answer variables.
#[derive(Debug, Clone)]
pub(super) struct Shape {
    pub(super) patterns: Vec<TriplePattern>,
    /// Variables the filters read.
    pub(super) filtered: HashSet<String>,
    /// The answer variables (all of the pattern's for `SELECT *`; none for `ASK`).
    pub(super) projected: Vec<Variable>,
}

fn path_reads(path: &PropertyPathExpression, out: &mut Vec<(Option<String>, Option<String>)>) {
    match path {
        PropertyPathExpression::NamedNode(n) => out.push((Some(n.as_str().to_owned()), None)),
        PropertyPathExpression::Reverse(p)
        | PropertyPathExpression::ZeroOrMore(p)
        | PropertyPathExpression::OneOrMore(p)
        | PropertyPathExpression::ZeroOrOne(p) => path_reads(p, out),
        PropertyPathExpression::Sequence(a, b) | PropertyPathExpression::Alternative(a, b) => {
            path_reads(a, out);
            path_reads(b, out);
        }
        PropertyPathExpression::NegatedPropertySet(_) => out.push((None, None)),
    }
}

fn pattern_reads(t: &TriplePattern) -> (Option<String>, Option<String>) {
    let predicate = match &t.predicate {
        NamedNodePattern::NamedNode(n) => Some(n.as_str().to_owned()),
        NamedNodePattern::Variable(_) => None,
    };
    let class = match (&predicate, &t.object) {
        (Some(p), TermPattern::NamedNode(c)) if p == RDF_TYPE => Some(c.as_str().to_owned()),
        _ => None,
    };
    (predicate, class)
}

fn variables_of(e: &nrese_sparql_syntax::algebra::Expression, out: &mut HashSet<String>) {
    e.find(&mut |node| {
        if let Node::Expression(nrese_sparql_syntax::algebra::Expression::Variable(v)) = node {
            out.insert(v.as_str().to_owned());
        }
        // Variables of EXISTS patterns count too: a filter with one isn't over the row.
        if let Node::Pattern(GraphPattern::Bgp { patterns }) = node {
            for t in patterns {
                for term in [&t.subject, &t.object] {
                    if let TermPattern::Variable(v) = term {
                        out.insert(v.as_str().to_owned());
                    }
                }
            }
        }
        false
    });
}

/// The basic graph pattern under filters of `pattern`, if that is all it is.
fn bgp_under_filters(pattern: &GraphPattern) -> Option<(Vec<TriplePattern>, HashSet<String>)> {
    match pattern {
        GraphPattern::Bgp { patterns } => Some((patterns.clone(), HashSet::new())),
        GraphPattern::Filter { expr, inner } => {
            let (patterns, mut filtered) = bgp_under_filters(inner)?;
            variables_of(expr, &mut filtered);
            Some((patterns, filtered))
        }
        _ => None,
    }
}

fn shape_of(query: &Query) -> Option<Shape> {
    let (pattern, ask) = match query {
        Query::Select { pattern, .. } => (pattern, false),
        Query::Ask { pattern, .. } => (pattern, true),
        _ => return None,
    };
    let mut p = pattern;
    let mut projected = None;
    loop {
        match p {
            GraphPattern::Distinct { inner } | GraphPattern::Reduced { inner } => p = inner,
            GraphPattern::Project { inner, variables } if projected.is_none() => {
                projected = Some(variables.clone());
                p = inner;
            }
            _ => break,
        }
    }
    let (patterns, filtered) = bgp_under_filters(p)?;
    let projected = match (ask, projected) {
        // An ASK's variables are all existential.
        (true, _) => Vec::new(),
        (false, Some(v)) => v,
        (false, None) => return None,
    };
    Some(Shape {
        patterns,
        filtered,
        projected,
    })
}

/// What `query` reads, whether it is monotone, and its shape for the exact services.
pub(crate) fn analyse(query: &Query) -> Analysis {
    let pattern = match query {
        Query::Select { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. }
        | Query::Ask { pattern, .. } => pattern,
    };
    let mut analysis = Analysis {
        shape: shape_of(query),
        answers: answer_variables(query),
        ..Analysis::default()
    };
    if matches!(query, Query::Describe { .. }) {
        analysis.not_monotone = Some("DESCRIBE");
    }
    pattern.find(&mut |node| {
        match node {
            Node::Pattern(p) => {
                let op = match p {
                    GraphPattern::Bgp { patterns } => {
                        analysis.reads.extend(patterns.iter().map(pattern_reads));
                        analysis.atoms.extend(patterns.iter().map(atom_of));
                        None
                    }
                    GraphPattern::Path { path, .. } => {
                        let from = analysis.reads.len();
                        path_reads(path, &mut analysis.reads);
                        // A path's steps meet at nodes the query doesn't see.
                        let steps = analysis.reads[from..]
                            .iter()
                            .map(|(predicate, class)| Atom {
                                predicate: predicate.clone(),
                                class: class.clone(),
                                ends: [End::Hidden, End::Hidden],
                            });
                        let steps: Vec<Atom> = steps.collect();
                        analysis.atoms.extend(steps);
                        None
                    }
                    GraphPattern::Extend { expression, .. } => {
                        variables_of(expression, &mut analysis.computed);
                        None
                    }
                    GraphPattern::LeftJoin { .. } => Some("OPTIONAL"),
                    GraphPattern::Minus { .. } => Some("MINUS"),
                    GraphPattern::Lateral { .. } => Some("LATERAL"),
                    GraphPattern::Group { .. } => Some("an aggregate"),
                    GraphPattern::Slice { .. } => Some("LIMIT or OFFSET"),
                    GraphPattern::Graph { .. } => Some("GRAPH"),
                    GraphPattern::Service { .. } => Some("SERVICE"),
                    _ => None,
                };
                if analysis.not_monotone.is_none() {
                    analysis.not_monotone = op;
                }
            }
            Node::Expression(nrese_sparql_syntax::algebra::Expression::Exists(_)) => {
                if analysis.not_monotone.is_none() {
                    analysis.not_monotone = Some("EXISTS");
                }
            }
            Node::Expression(_) => {}
        }
        false
    });
    analysis
}

/// The vocabulary the bounds don't cover: U1 bounds facts about individuals (class
/// memberships, property values, equality); entailed schema statements (subclass,
/// equivalence, subproperty, disjointness axioms as triples) are classification's. A
/// variable predicate reads them too.
pub(super) fn unbounded(analysis: &Analysis) -> Option<String> {
    const RESERVED: [&str; 3] = [
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "http://www.w3.org/2000/01/rdf-schema#",
        "http://www.w3.org/2002/07/owl#",
    ];
    analysis
        .reads
        .iter()
        .find_map(|(predicate, _)| match predicate {
            None => Some("a variable predicate".to_owned()),
            Some(p) if p == RDF_TYPE || p == OWL_SAME_AS => None,
            Some(p) if RESERVED.iter().any(|ns| p.starts_with(ns)) => Some(format!("<{p}>")),
            Some(_) => None,
        })
}

/// Whether every predicate and class `analysis` reads has no fact in U1 beyond L.
pub(super) fn closed(analysis: &Analysis, view: &View, snapshot: &Snapshot) -> bool {
    // The RL route: the RL closure is complete for an OWL 2 RL ontology.
    if view.rules {
        return true;
    }
    // U1 that doesn't cover every axiom bounds nothing: its gap may miss predicates.
    if view.upper.is_none() || view.unavailable.is_some() {
        return false;
    }
    let id = |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    let any = view.gap_classes.is_empty() && view.gap_predicates.is_empty();
    analysis.reads.iter().all(|(predicate, class)| {
        let Some(predicate) = predicate else {
            return any;
        };
        if predicate == RDF_TYPE {
            return match class {
                Some(c) => id(c).is_none_or(|c| !view.gap_classes.contains(&c.raw())),
                None => view.gap_classes.is_empty(),
            };
        }
        id(predicate).is_none_or(|p| !view.gap_predicates.contains(&p.raw()))
    })
}

/// Whether U1's facts beyond L on what a monotone query reads all put a Skolem constant
/// where the query has a term or an answer variable. Then an answer over U1 that L lacks
/// has a Skolem constant in it, which is never an answer: L's answers are the certain
/// ones (U1 contains them all where the data is consistent). The predicates are open,
/// but only through individuals no answer names (LUBM's existentials).
pub(super) fn closed_for_answers(analysis: &Analysis, view: &View, snapshot: &Snapshot) -> bool {
    if view.upper.is_none() || view.unavailable.is_some() || analysis.not_monotone.is_some() {
        return false;
    }
    let Some(answers) = &analysis.answers else {
        return false;
    };
    let safe = |end: &End| match end {
        End::Term => true,
        End::Var(v) => answers.contains(v) && !analysis.computed.contains(v),
        End::Hidden => false,
    };
    let id = |iri: &str| snapshot.lookup(NamedNodeRef::new_unchecked(iri).into());
    analysis.atoms.iter().all(|atom| {
        let Some(predicate) = &atom.predicate else {
            return false;
        };
        let (gap, named) = if predicate == RDF_TYPE {
            match &atom.class {
                Some(c) => id(c).map_or((false, false), |c| {
                    (
                        view.gap_classes.contains(&c.raw()),
                        view.named_gap_classes.contains(&c.raw()),
                    )
                }),
                // Any class: U1's own among them.
                None => (true, !view.named_gap_classes.is_empty()),
            }
        } else {
            id(predicate).map_or((false, false), |p| {
                (
                    view.gap_predicates.contains(&p.raw()),
                    view.named_gap_predicates.contains(&p.raw()),
                )
            })
        };
        !gap || (!named && atom.ends.iter().all(safe))
    })
}

/// In debug builds, what [`closed_for_answers`] claims: U1's answers without a Skolem
/// constant are L's.
#[cfg(debug_assertions)]
pub(super) fn check_closed_for_answers(
    view: &View,
    prepared: &PreparedQuery,
    settings: &crate::query_executor::StoreSettings,
    cancellation: &CancellationToken,
) -> StoreResult<Answers> {
    let upper = view.upper.as_ref().expect("closed_for_answers needs U1");
    use crate::query_executor::evaluate_bound;
    let lower = evaluate_bound(&view.lower, prepared, settings, cancellation, 0)?;
    let retained = match &lower {
        Answers::Solutions(rows) => rows.reserved_bytes(),
        _ => 0,
    };
    let upper = evaluate_bound(upper, prepared, settings, cancellation, retained)?;
    match (&lower, &upper) {
        (Answers::Boolean(l), Answers::Boolean(u)) => {
            assert_eq!(l, u, "skolem-only-gap: {}", prepared.text());
        }
        (Answers::Solutions(l), Answers::Solutions(u)) => {
            let named = |row: &Vec<Option<Term>>| {
                !row.iter()
                    .any(|t| matches!(t, Some(Term::NamedNode(n)) if n.as_str().starts_with(U1)))
            };
            let l: HashSet<Vec<Option<Term>>> = (0..l.len()).map(|i| l.row(i)).collect();
            let u: HashSet<Vec<Option<Term>>> =
                (0..u.len()).map(|i| u.row(i)).filter(named).collect();
            assert_eq!(l, u, "skolem-only-gap: {}", prepared.text());
        }
        _ => {}
    }
    Ok(lower)
}
