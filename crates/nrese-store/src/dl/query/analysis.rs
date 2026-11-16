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
    /// An `owl:sameAs` read whose two ends can be one term: a triple pattern with a
    /// variable or blank node at an end or one constant at both, or any path step. A
    /// variable predicate and a negated property set count as possible `owl:sameAs` reads.
    /// eq-ref (OWL 2 Profiles table 4: every term is `owl:sameAs` itself) is in neither
    /// bound: the RL rules leave it out by design, and so does U1. So the bounds can't
    /// decide such a read on any route: it is answered over L, and its status, like every
    /// `owl:sameAs` read's, is [`denied`]'s.
    pub(super) reflexive_same_as: bool,
}

/// Whether the `owl:sameAs` pattern `t` can match a reflexive pair: only two distinct
/// constants can't.
fn can_be_reflexive(t: &TriplePattern) -> bool {
    let constant =
        |term: &TermPattern| matches!(term, TermPattern::NamedNode(_) | TermPattern::Literal(_));
    !(constant(&t.subject) && constant(&t.object) && t.subject != t.object)
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
        TermPattern::NamedNode(n) if nrese_dl::internal::is_made(n.as_str()) => End::Hidden,
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
        PropertyPathExpression::Sequence(paths) | PropertyPathExpression::Alternative(paths) => {
            for p in paths {
                path_reads(p, out);
            }
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

fn pattern_of(query: &Query) -> &GraphPattern {
    match query {
        Query::Select { pattern, .. }
        | Query::Construct { pattern, .. }
        | Query::Describe { pattern, .. }
        | Query::Ask { pattern, .. } => pattern,
    }
}

/// What `query` reads, whether it is monotone, and its shape for the exact services.
pub(crate) fn analyse(query: &Query) -> Analysis {
    let pattern = pattern_of(query);
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
                        // A variable predicate can be owl:sameAs.
                        analysis.reflexive_same_as |= patterns.iter().any(|t| {
                            let same_as = match &t.predicate {
                                NamedNodePattern::NamedNode(p) => p.as_str() == OWL_SAME_AS,
                                NamedNodePattern::Variable(_) => true,
                            };
                            same_as && can_be_reflexive(t)
                        });
                        analysis.reads.extend(patterns.iter().map(pattern_reads));
                        analysis.atoms.extend(patterns.iter().map(atom_of));
                        None
                    }
                    GraphPattern::Path { path, .. } => {
                        let from = analysis.reads.len();
                        path_reads(path, &mut analysis.reads);
                        // A step's ends are nodes the query doesn't fix: any can be one.
                        // A negated property set (no predicate) can read owl:sameAs.
                        analysis.reflexive_same_as |= analysis.reads[from..]
                            .iter()
                            .any(|(p, _)| p.as_deref().is_none_or(|p| p == OWL_SAME_AS));
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
                    GraphPattern::Extend { bindings, .. } => {
                        for (_, expression) in bindings {
                            variables_of(expression, &mut analysis.computed);
                        }
                        None
                    }
                    _ => not_monotone(p),
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

/// The operator `p` is, where it isn't monotone (or isn't under entailment).
fn not_monotone(p: &GraphPattern) -> Option<&'static str> {
    match p {
        GraphPattern::LeftJoin { .. } => Some("OPTIONAL"),
        GraphPattern::Minus { .. } => Some("MINUS"),
        GraphPattern::Lateral { .. } => Some("LATERAL"),
        GraphPattern::Group { .. } => Some("an aggregate"),
        GraphPattern::Slice { .. } => Some("LIMIT or OFFSET"),
        GraphPattern::Graph { .. } => Some("GRAPH"),
        GraphPattern::Service { .. } => Some("SERVICE"),
        _ => None,
    }
}

/// The first operator of `query` under which a missing row can make a returned one
/// wrong: `EXISTS` and those [`not_monotone`] names but `GRAPH`, `SERVICE` and a slice
/// of an unordered answer, which only drop rows or read no inference (nor does
/// `DESCRIBE`). One walk that allocates nothing: every answer over a closure asks it.
pub(super) fn negation(query: &Query) -> Option<&'static str> {
    let mut found = None;
    pattern_of(query).find(&mut |node| {
        found = match node {
            Node::Pattern(GraphPattern::Slice { inner, .. }) if !ordered(inner) => None,
            Node::Pattern(GraphPattern::Graph { .. } | GraphPattern::Service { .. }) => None,
            Node::Pattern(p) => not_monotone(p),
            Node::Expression(nrese_sparql_syntax::algebra::Expression::Exists(_)) => Some("EXISTS"),
            Node::Expression(_) => None,
        };
        found.is_some()
    });
    found
}

/// The first value `query` matches against the data by its spelling, where it may be a
/// typed literal: a literal that is no string (simple or language-tagged) at a pattern's or
/// a path's end; or a variable at such an end, anywhere in the query, that `VALUES` binds to
/// such a literal or a `BIND` (a projection's `(… AS ?v)` too) binds to anything but an IRI
/// or a string constant, a computed value or a copy included. A number, a date or any
/// other typed value has more than one spelling (`"2"`, `"+2"`, `"02"`). The store keeps
/// each as written and matches by spelling, while the regime's answers go by value (SPARQL
/// 1.1 Entailment §7.2). One walk; it allocates for the variables it meets, once per query
/// ([`Reads`]).
pub(super) fn typed_constant(query: &Query) -> Option<String> {
    use nrese_sparql_syntax::{algebra::Expression, term::GroundTerm};
    let typed = |l: &nrese_rdf::Literal| (!l.is_plain()).then(|| l.to_string());
    let (mut found, mut bound, mut ends) = (None, Vec::new(), HashSet::new());
    pattern_of(query).find(&mut |node| {
        let mut end = |t: &TermPattern| match t {
            TermPattern::Literal(l) => typed(l),
            TermPattern::Variable(v) => {
                ends.insert(v.as_str().to_owned());
                None
            }
            _ => None,
        };
        let mut bind = |v: &Variable, what: Option<String>| {
            bound.extend(what.map(|c| (v.as_str().to_owned(), c)));
        };
        found = match node {
            Node::Pattern(GraphPattern::Bgp { patterns }) => patterns
                .iter()
                .find_map(|t| end(&t.subject).or_else(|| end(&t.object))),
            Node::Pattern(GraphPattern::Path {
                subject, object, ..
            }) => end(subject).or_else(|| end(object)),
            Node::Pattern(GraphPattern::Values {
                variables,
                bindings,
            }) => {
                for row in bindings {
                    for (v, value) in variables.iter().zip(row) {
                        if let Some(GroundTerm::Literal(l)) = value {
                            bind(v, typed(l));
                        }
                    }
                }
                None
            }
            Node::Pattern(GraphPattern::Extend { bindings, .. }) => {
                for (v, value) in bindings {
                    bind(
                        v,
                        match value {
                            Expression::Literal(l) => typed(l),
                            Expression::NamedNode(_) => None,
                            _ => Some(format!("the value it computes for ?{}", v.as_str())),
                        },
                    );
                }
                None
            }
            _ => None,
        };
        found.is_some()
    });
    found.or_else(|| {
        bound
            .into_iter()
            .find(|(v, _)| ends.contains(v))
            .map(|(_, c)| c)
    })
}

/// The data values `query` compares by term: each variable (or blank node, which a path's
/// sequence makes) at the object end of two or more patterns or paths (or at the start of a
/// path that can match zero steps), or of one and bound by `VALUES` or `BIND` (a join), or
/// at one that a `DISTINCT` or `REDUCED` returns by its own projection, `GROUP BY` groups or
/// a `DISTINCT` aggregate reads (or an alias `(?n AS ?m)` of one under `DISTINCT` or `GROUP
/// BY`, which the walk meets first); the nodes inside and at the start of a path with a
/// reversed step; and each string constant at an object end (a stored `"Rex"^^xsd:token` is
/// `"Rex"` to OWL 2). What it is, and the predicates it is read through: a pattern's, or a
/// path's own (`None`, any: a variable predicate, a negated set, or a path that can match
/// zero steps, whose ends are every term of the graph). The store compares terms, the regime
/// values; whether a value read there has another spelling is the data's ([`certify`] asks
/// the snapshot read, so a variable's IRIs-only ends, `rdf:type`'s too, mark nothing). One
/// walk, once per query ([`Reads`]). Every operator and term form is named in its matches,
/// so a new one doesn't compile until it is said whether it compares terms.
pub(super) fn compared_values(query: &Query) -> Vec<(String, Vec<Option<String>>)> {
    use nrese_sparql_syntax::algebra::{AggregateExpression::*, Expression};
    let (mut ends, mut grouped, mut bound) =
        (HashMap::<_, Vec<_>>::new(), HashSet::new(), HashSet::new());
    let (mut distinct, mut all, mut marks) = (false, false, Vec::new());
    let mut deduped = HashSet::new();
    pattern_of(query).find(&mut |node| {
        let mut end = |t: &TermPattern, p: Vec<Option<String>>| match t {
            TermPattern::Variable(v) => ends.entry(v.as_str().to_owned()).or_default().push(p),
            TermPattern::BlankNode(b) => ends.entry(b.to_string()).or_default().push(p),
            TermPattern::Literal(l) if l.is_plain() => marks.push((format!("the string {l}"), p)),
            // A name, a typed constant ([`typed_constant`]'s) or a quoted triple: no value
            // compared here.
            TermPattern::NamedNode(_) | TermPattern::Literal(_) | TermPattern::Triple(_) => {}
        };
        match node {
            Node::Pattern(GraphPattern::Bgp { patterns }) => {
                patterns
                    .iter()
                    .for_each(|t| end(&t.object, vec![pattern_reads(t).0]));
            }
            Node::Pattern(GraphPattern::Path {
                subject,
                path,
                object,
            }) => {
                let mut reads = Vec::new();
                path_reads(path, &mut reads);
                let mut through: Vec<_> = reads.into_iter().map(|(p, _)| p).collect();
                if zero(path) {
                    through = vec![None];
                    end(subject, through.clone());
                }
                end(object, through.clone());
                if reversed(path) {
                    let what = "a node inside or at the start of a path with a reversed step";
                    marks.push((what.to_owned(), through));
                }
            }
            Node::Pattern(GraphPattern::Values { variables, .. }) => {
                bound.extend(variables.iter().map(|v| v.as_str().to_owned()));
            }
            Node::Pattern(GraphPattern::Extend { bindings, .. }) => {
                for (v, e) in bindings.iter().map(|(v, e)| (v.as_str(), e)) {
                    let alias = distinct || deduped.contains(v) || grouped.contains(v);
                    if alias && let Expression::Variable(n) = e {
                        grouped.insert(n.as_str().to_owned());
                    }
                    bound.insert(v.to_owned());
                }
            }
            Node::Pattern(GraphPattern::Distinct { inner } | GraphPattern::Reduced { inner }) => {
                // What it returns: its projection; every end where no projection is under it.
                if let GraphPattern::Project { variables, .. } = &**inner {
                    deduped.extend(variables.iter().map(|v| v.as_str().to_owned()));
                } else {
                    distinct = true;
                }
            }
            Node::Pattern(GraphPattern::Group {
                variables,
                aggregates,
                ..
            }) => {
                grouped.extend(variables.iter().map(|v| v.as_str().to_owned()));
                for (_, aggregate) in aggregates {
                    match aggregate {
                        CountSolutions { distinct } => all |= *distinct,
                        FunctionCall { expr, distinct, .. } if *distinct => {
                            variables_of(expr, &mut grouped)
                        }
                        // Over every value as it comes: no two compared.
                        FunctionCall { .. } => {}
                    }
                }
            }
            // These compare terms only at the ends recorded above (a join, LATERAL, OPTIONAL,
            // MINUS: on shared variables), by value (FILTER, `=`, IN, a left join's condition,
            // ORDER BY; `sameTerm` by term, as it means to), or not at all (UNION, GRAPH, a
            // projection, which DISTINCT reads above, a slice; SERVICE's are the remote's).
            Node::Pattern(
                GraphPattern::Join { .. }
                | GraphPattern::LeftJoin { .. }
                | GraphPattern::Lateral { .. }
                | GraphPattern::Filter { .. }
                | GraphPattern::Union { .. }
                | GraphPattern::Graph { .. }
                | GraphPattern::Minus { .. }
                | GraphPattern::OrderBy { .. }
                | GraphPattern::Project { .. }
                | GraphPattern::Slice { .. }
                | GraphPattern::Service { .. },
            )
            | Node::Expression(_) => {}
        }
        false
    });
    let mut compared: Vec<_> = ends
        .into_iter()
        .filter_map(|(v, through)| {
            let what = if through.len() > 1 || bound.contains(&v) {
                "a join on"
            } else if distinct || deduped.contains(&v) {
                "DISTINCT over"
            } else if all || grouped.contains(&v) {
                "GROUP BY or DISTINCT over"
            } else {
                return None;
            };
            Some((format!("{what} ?{v}"), through.concat()))
        })
        .collect();
    compared.extend(marks);
    compared.sort();
    compared
}

/// Whether `path` can match zero steps (`*`, `?`, a sequence of such, an alternative with
/// one): it then pairs every term of the graph with itself, data values included, whatever
/// its predicates hold.
fn zero(path: &PropertyPathExpression) -> bool {
    use PropertyPathExpression as P;
    match path {
        P::ZeroOrMore(_) | P::ZeroOrOne(_) => true,
        P::NamedNode(_) | P::NegatedPropertySet(_) => false,
        P::Reverse(p) | P::OneOrMore(p) => zero(p),
        P::Sequence(paths) => paths.iter().all(zero),
        P::Alternative(paths) => paths.iter().any(zero),
    }
}

/// Whether `path` has a reversed step (`^p`, `!(^p)`) anywhere, under `*`, `+`, `?`, `|` or
/// `/`: a node inside it or its start can then be a data value, as its end can (a node is
/// one only before a reversed step: a forward step starts at a subject).
fn reversed(path: &PropertyPathExpression) -> bool {
    use PropertyPathExpression as P;
    match path {
        P::Reverse(_) => true,
        P::NamedNode(_) | P::NegatedPropertySet(_) => false,
        P::Sequence(paths) | P::Alternative(paths) => paths.iter().any(reversed),
        P::ZeroOrMore(p) | P::OneOrMore(p) | P::ZeroOrOne(p) => reversed(p),
    }
}

/// Whether `iri` is in a reserved namespace, rdf:, rdfs: or owl:, whose entailed statements
/// no closure, rewriting or bound holds in full.
fn reserved(iri: &str) -> bool {
    [
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
        "http://www.w3.org/2000/01/rdf-schema#",
        "http://www.w3.org/2002/07/owl#",
    ]
    .iter()
    .any(|ns| iri.starts_with(ns))
}

/// The first read of `query` that may miss entailed rows wherever it is answered (default
/// deny): any but a predicate outside the [`reserved`] namespaces or `rdf:type` with a
/// class constant outside them. A variable predicate, a negated property set and DESCRIBE
/// read every predicate. On the bounds' path (`bounds`), one more read may be complete:
/// see [`denied_read`]. One walk that allocates only for what it finds.
pub(super) fn denied(query: &Query, bounds: bool) -> Option<String> {
    if matches!(query, Query::Describe { .. }) {
        return Some("every predicate of what DESCRIBE names".to_owned());
    }
    let mut found = None;
    pattern_of(query).find(&mut |node| {
        found = match node {
            Node::Pattern(GraphPattern::Bgp { patterns }) => {
                patterns.iter().find_map(|t| match &t.predicate {
                    NamedNodePattern::NamedNode(p) => {
                        denied_read(p.as_str(), Some((&t.subject, &t.object)), bounds)
                    }
                    NamedNodePattern::Variable(_) => Some("a variable predicate".to_owned()),
                })
            }
            Node::Pattern(GraphPattern::Path { path, .. }) => denied_path(path),
            _ => None,
        };
        found.is_some()
    });
    found
}

fn denied_path(path: &PropertyPathExpression) -> Option<String> {
    use PropertyPathExpression as P;
    match path {
        P::NamedNode(n) => denied_read(n.as_str(), None, false),
        P::Reverse(p) | P::ZeroOrMore(p) | P::OneOrMore(p) | P::ZeroOrOne(p) => denied_path(p),
        P::Sequence(paths) | P::Alternative(paths) => paths.iter().find_map(denied_path),
        P::NegatedPropertySet(_) => Some("a negated property set".to_owned()),
    }
}

/// [`denied`] for a read of `predicate` between `ends` (`None`: a path step's, which the
/// query doesn't name). What may be complete, each by its argument:
/// - a predicate outside the [`reserved`] namespaces, and `rdf:type` with a class constant
///   outside them: their entailed statements are facts about individuals, which the bounds
///   bound and a closure holds for its profile;
/// - on the bounds' path only, `owl:sameAs` between two different constant IRIs: U1
///   bounds equality between the individuals it reasons about as it bounds their other
///   facts, so the bounds decide it; the pair neither bound holds, eq-ref's reflexive one,
///   needs one term at both ends. Only where U1 types both as individuals
///   ([`untyped_same_as`]).
fn denied_read(
    predicate: &str,
    ends: Option<(&TermPattern, &TermPattern)>,
    bounds: bool,
) -> Option<String> {
    match (predicate, ends) {
        (p, _) if !reserved(p) => None,
        (RDF_TYPE, Some((_, TermPattern::NamedNode(c)))) if !reserved(c.as_str()) => None,
        (RDF_TYPE, Some((_, TermPattern::NamedNode(c)))) => {
            Some(format!("rdf:type <{}>", c.as_str()))
        }
        (RDF_TYPE, _) => Some("rdf:type of a class it doesn't name".to_owned()),
        (OWL_SAME_AS, Some((TermPattern::NamedNode(a), TermPattern::NamedNode(b))))
            if bounds && a != b =>
        {
            None
        }
        (p, _) => Some(format!("<{p}>")),
    }
}

/// The first `owl:sameAs` read [`denied_read`] allows on the bounds' path with a constant
/// that `view`'s upper bound doesn't type as `owl:Thing`: U1 types the individuals it
/// reasons about so, and a term it doesn't (outside the data, or only a class or property
/// name) may be entailed equal to another where the ontology allows one individual only
/// (`owl:Thing ⊑ {:a}`), though neither bound says so. On the RL route no upper bound is
/// built: denied there (that an OWL 2 RL ontology can't force one individual isn't argued).
pub(super) fn untyped_same_as(query: &Query, view: &View) -> Option<String> {
    let upper = view.upper.as_ref().filter(|_| !view.rules);
    let untyped = |n: &str| {
        !upper.is_some_and(|u| {
            let id = |iri: &str| u.lookup(NamedNodeRef::new_unchecked(iri).into());
            let (Some(s), Some(p), Some(o)) = (id(n), id(RDF_TYPE), id(OWL_THING)) else {
                return false;
            };
            let typed = nrese_engine::QuadPattern {
                subject: Some(s),
                predicate: Some(p),
                object: Some(o),
                graph: nrese_engine::GraphSelector::Any,
            };
            u.exists_in(ReadModel::Materialised, &typed)
        })
    };
    let mut found = None;
    pattern_of(query).find(&mut |node| {
        if let Node::Pattern(GraphPattern::Bgp { patterns }) = node {
            found = patterns
                .iter()
                .find_map(|t| match (&t.subject, &t.predicate, &t.object) {
                    (
                        TermPattern::NamedNode(a),
                        NamedNodePattern::NamedNode(p),
                        TermPattern::NamedNode(b),
                    ) if p.as_str() == OWL_SAME_AS => [a.as_str(), b.as_str()]
                        .into_iter()
                        .find(|n| untyped(n))
                        .map(|n| format!("<{OWL_SAME_AS}> of <{n}>, which no upper bound types")),
                    _ => None,
                });
        }
        found.is_some()
    });
    found
}

/// Whether `pattern`, under its projection, is ordered: then a slice of it keeps the
/// first rows, which a missing row can change.
fn ordered(mut pattern: &GraphPattern) -> bool {
    loop {
        match pattern {
            GraphPattern::Project { inner, .. }
            | GraphPattern::Distinct { inner }
            | GraphPattern::Reduced { inner } => pattern = inner,
            GraphPattern::OrderBy { .. } => return true,
            _ => return false,
        }
    }
}

/// The vocabulary the bounds don't cover: U1 bounds facts about individuals (class
/// memberships, property values, equality); entailed schema statements (subclass,
/// equivalence, subproperty, disjointness axioms as triples) are classification's. A
/// variable predicate reads them too.
pub(super) fn unbounded(analysis: &Analysis) -> Option<String> {
    analysis
        .reads
        .iter()
        .find_map(|(predicate, _)| match predicate {
            None => Some("a variable predicate".to_owned()),
            Some(p) if p == RDF_TYPE || p == OWL_SAME_AS => None,
            Some(p) if reserved(p) => Some(format!("<{p}>")),
            Some(_) => None,
        })
}

/// Whether every predicate and class `analysis` reads has no fact in U1 beyond L.
pub(super) fn closed(analysis: &Analysis, view: &View, snapshot: &Snapshot) -> bool {
    // The RL route: the RL closure is complete for an OWL 2 RL ontology, but for the
    // reflexive owl:sameAs it leaves out (`Analysis::reflexive_same_as`, asked first).
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
    memory: &nrese_exec::Envelope,
) -> StoreResult<Answers> {
    let upper = view.upper.as_ref().expect("closed_for_answers needs U1");
    use crate::query_executor::evaluate_bound;
    let lower = evaluate_bound(&view.lower, prepared, settings, cancellation, memory)?;
    let upper = evaluate_bound(upper, prepared, settings, cancellation, memory)?;
    match (&lower, &upper) {
        (Answers::Boolean(l), Answers::Boolean(u)) => {
            assert_eq!(l, u, "skolem-only-gap: {}", prepared.text());
        }
        (Answers::Solutions(l), Answers::Solutions(u)) => {
            let named = |row: &Vec<Option<Term>>| {
                !row.iter()
                    .any(|t| matches!(t, Some(Term::NamedNode(n)) if nrese_dl::internal::is_made(n.as_str())))
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
