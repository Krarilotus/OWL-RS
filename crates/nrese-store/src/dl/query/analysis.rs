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
    reflexive_same_as: bool,
    /// What the query reads of the statements OWL 2 entails for every term ([`universal`]).
    pub(super) universal: Option<&'static str>,
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
        universal: universal(query),
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

/// What `x rdf:type owl:Thing` reads are named as by [`universal`].
pub(super) const THING: &str = "rdf:type owl:Thing";

/// The first predicate `query` reads where OWL 2 entails statements for every term that
/// neither a ruleset's closure nor the QL rewriting holds: `x owl:sameAs x` (eq-ref is
/// left out), `x rdf:type owl:Thing`, the top properties' pairs, a class's or property's
/// equivalence with itself, its subsumption by itself and the top and of the bottom (only
/// declared classes get theirs), and its disjointness with the bottom. `DESCRIBE` reads
/// every predicate of what it names. One walk that allocates nothing.
pub(super) fn universal(query: &Query) -> Option<&'static str> {
    if matches!(query, Query::Describe { .. }) {
        return Some("every predicate of what DESCRIBE names");
    }
    let mut found = None;
    pattern_of(query).find(&mut |node| {
        found = match node {
            Node::Pattern(GraphPattern::Bgp { patterns }) => {
                patterns.iter().find_map(|t| match &t.predicate {
                    NamedNodePattern::NamedNode(p) => universal_read(p.as_str(), Some(t)),
                    NamedNodePattern::Variable(_) => Some("a variable predicate"),
                })
            }
            Node::Pattern(GraphPattern::Path { path, .. }) => universal_path(path),
            _ => None,
        };
        found.is_some()
    });
    found
}

fn universal_path(path: &PropertyPathExpression) -> Option<&'static str> {
    use PropertyPathExpression as P;
    match path {
        P::NamedNode(n) => universal_read(n.as_str(), None),
        P::Reverse(p) | P::ZeroOrMore(p) | P::OneOrMore(p) | P::ZeroOrOne(p) => universal_path(p),
        P::Sequence(paths) | P::Alternative(paths) => paths.iter().find_map(universal_path),
        P::NegatedPropertySet(_) => Some("a negated property set"),
    }
}

/// Whether a read of `predicate` (`t` its pattern; `None` for a path step, whose ends the
/// query doesn't fix) can match a statement [`universal`] names: its name, if so.
fn universal_read(predicate: &str, t: Option<&TriplePattern>) -> Option<&'static str> {
    const OWL: &str = "http://www.w3.org/2002/07/owl#";
    const TOP: [&str; 2] = ["topObjectProperty", "topDataProperty"];
    const BOTTOM: [&str; 2] = ["bottomObjectProperty", "bottomDataProperty"];
    // Whether the subject, and the object, can be one of the OWL terms `names`: a
    // variable or blank node can, as can a path step's ends.
    let ends = |names: &[&str]| {
        let can_be = |term: &TermPattern| match term {
            TermPattern::NamedNode(n) => n
                .as_str()
                .strip_prefix(OWL)
                .is_some_and(|n| names.contains(&n)),
            TermPattern::Literal(_) => false,
            _ => true,
        };
        t.map_or([true; 2], |t| [can_be(&t.subject), can_be(&t.object)])
    };
    let reflexive = t.is_none_or(can_be_reflexive);
    // x ⊑ x, x ⊑ the top, the bottom ⊑ x.
    let subsumed = |top: &[&str], bottom: &[&str]| reflexive || ends(top)[1] || ends(bottom)[0];
    let (reads, name) = match predicate {
        RDF_TYPE => (ends(&["Thing"])[1], THING),
        "http://www.w3.org/2002/07/owl#topObjectProperty" => (true, "owl:topObjectProperty"),
        "http://www.w3.org/2002/07/owl#topDataProperty" => (true, "owl:topDataProperty"),
        OWL_SAME_AS => (reflexive, "owl:sameAs"),
        "http://www.w3.org/2002/07/owl#equivalentClass" => (reflexive, "owl:equivalentClass"),
        "http://www.w3.org/2002/07/owl#equivalentProperty" => (reflexive, "owl:equivalentProperty"),
        "http://www.w3.org/2000/01/rdf-schema#subClassOf" => {
            (subsumed(&["Thing"], &["Nothing"]), "rdfs:subClassOf")
        }
        "http://www.w3.org/2000/01/rdf-schema#subPropertyOf" => {
            (subsumed(&TOP, &BOTTOM), "rdfs:subPropertyOf")
        }
        "http://www.w3.org/2002/07/owl#disjointWith" => {
            (ends(&["Nothing"]).contains(&true), "owl:disjointWith")
        }
        "http://www.w3.org/2002/07/owl#propertyDisjointWith" => {
            (ends(&BOTTOM).contains(&true), "owl:propertyDisjointWith")
        }
        _ => return None,
    };
    reads.then_some(name)
}

/// Whether U1 can hold `x rdf:type owl:Thing` for every `x` that `query` reads it of: U1
/// types the data's individuals, so a variable is covered, a named node only if it is a
/// term of `snapshot`, and a blank node or literal never. A dictionary lookup: a term of
/// the data U1 doesn't type (a class or property name alone) passes it.
pub(super) fn thing_covered(query: &Query, snapshot: &Snapshot) -> bool {
    let covered = |term: &TermPattern| match term {
        TermPattern::Variable(_) => true,
        TermPattern::NamedNode(n) => snapshot
            .lookup(NamedNodeRef::new_unchecked(n.as_str()).into())
            .is_some(),
        _ => false,
    };
    let mut uncovered = false;
    pattern_of(query).find(&mut |node| {
        uncovered = match node {
            Node::Pattern(GraphPattern::Bgp { patterns }) => patterns.iter().any(|t| {
                matches!(&t.predicate, NamedNodePattern::NamedNode(p)
                    if universal_read(p.as_str(), Some(t)) == Some(THING))
                    && !covered(&t.subject)
            }),
            Node::Pattern(GraphPattern::Path {
                subject,
                path,
                object,
            }) => universal_path(path) == Some(THING) && !(covered(subject) && covered(object)),
            _ => false,
        };
        uncovered
    });
    !uncovered
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

/// Why the bounds can't decide `analysis`'s `owl:sameAs` reads: eq-ref (OWL 2 Profiles
/// table 4: every term is `owl:sameAs` itself) is in neither bound: the RL rules leave it
/// out by design, and so does U1. Holds on every route, so it is asked before [`closed`].
/// The answers over L then miss certain ones; under an operator that isn't monotone
/// (`not_monotone`: NOT EXISTS, MINUS, OPTIONAL, …) a missing pair can also keep a row the
/// certain answers lack, so they aren't sound either: `(why, sound)`.
pub(super) fn reflexive_same_as(analysis: &Analysis) -> Option<(String, bool)> {
    if !analysis.reflexive_same_as {
        return None;
    }
    const WHY: &str = "the query reads owl:sameAs where both ends can be one term: reflexive \
                       owl:sameAs isn't materialised in either bound";
    Some(match analysis.not_monotone {
        None => (WHY.to_owned(), true),
        Some(op) => (
            format!("{WHY}, and under {op}, which isn't monotone, a row it would remove is kept"),
            false,
        ),
    })
}

/// Whether every predicate and class `analysis` reads has no fact in U1 beyond L.
pub(super) fn closed(analysis: &Analysis, view: &View, snapshot: &Snapshot) -> bool {
    // The RL route: the RL closure is complete for an OWL 2 RL ontology, but for the
    // reflexive owl:sameAs it leaves out ([`reflexive_same_as`], asked first).
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
