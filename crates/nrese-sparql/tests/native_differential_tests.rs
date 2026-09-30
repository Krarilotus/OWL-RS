//! XC3 gate: the native executor gives the same results as the reference evaluator
//! (`nrese-sparql-reference`, the specification's algebra evaluated without optimisations)
//! on the same snapshot.
//!
//! Random datasets mix every term kind the operators distinguish: IRIs, inline and
//! dictionary numbers, strings with and without language tags, dates, booleans. Random
//! queries combine BGPs (shared, repeated and unbound variables, unknown constants), FILTERs,
//! OPTIONAL (with filters), UNION, MINUS, (NOT) EXISTS, GROUP BY with every supported
//! aggregate, DISTINCT, ORDER BY and LIMIT. Every query must run natively
//! ([`runs_natively`]); results are compared as multisets, and as sequences where ORDER BY
//! covers every projected variable (so ties are identical rows). A third of the
//! default-graph statements are inferred, and every query runs under a random read model
//! (asserted, inferred or both).
//!
//! With `NRESE_ORACLE_DUMP=<dir>`, the random, pushed-filter and computed-value tests also
//! write their datasets and compared queries with the native rows, for a second oracle to
//! answer ([`Dump`]; benches/oracle).

use std::collections::HashSet;
use std::path::PathBuf;

use nrese_engine::{EncodedTriple, Engine, EngineConfig, ReadModel};
use nrese_sparql::{QueryOptions, QueryResults, evaluate_query, explain_query, runs_natively};
use oxrdf::vocab::xsd;
use oxrdf::{GraphName, Literal, NamedNode, Quad, Term};
use spargebra::SparqlParser;

const EX: &str = "http://example.com/";

/// The reference evaluator's answer on what `snapshot` holds in `options.read_model`.
fn reference(
    snapshot: &nrese_engine::Snapshot,
    query: &spargebra::Query,
    options: &QueryOptions,
) -> Result<QueryResults<'static>, nrese_sparql::QueryEvaluationError> {
    nrese_sparql_reference::evaluate_query(snapshot, query, options)
}

/// Applies `update` to `engine`: natively, or (`reference`) on the reference evaluator's
/// copy of the engine's statements, written back as the engine's asserted statements.
fn update_engine(
    engine: &Engine,
    update: &spargebra::Update,
    options: &nrese_sparql::UpdateOptions,
    reference: bool,
) {
    if !reference {
        let mut tx = engine.transaction();
        nrese_sparql::apply_update(&mut tx, update, options).unwrap();
        tx.commit().unwrap();
        return;
    }
    // The WHERE reads what a query reads, inferred statements included; the changes go
    // to the engine as the removals and insertions a native update makes.
    let before =
        nrese_sparql_reference::Dataset::from_snapshot(&engine.snapshot(), ReadModel::Materialised);
    let mut dataset = before.clone();
    let query_options = QueryOptions {
        dataset: options.using.clone(),
        union_default_graph: options.union_default_graph,
        ..QueryOptions::default()
    };
    dataset.update(update, &query_options).unwrap();
    let old: HashSet<&Quad> = before.quads().collect();
    let new: HashSet<&Quad> = dataset.quads().collect();
    let mut tx = engine.transaction();
    for quad in old.difference(&new) {
        tx.remove(quad.as_ref());
    }
    for quad in new.difference(&old) {
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
}

/// SplitMix64: deterministic test-case generation.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

/// Cases for a second oracle (benches/oracle/README.md): with `NRESE_ORACLE_DUMP=<dir>`, a
/// test writes `<dir>/<test>/d<N>/data.nq` per dataset and `q<M>.rq` with `q<M>.nrese` (a
/// line of the variables, then the native rows, one per line, terms separated by tabs;
/// `q<M>.ordered` if their order counts) per compared query.
struct Dump(Option<PathBuf>);

impl Dump {
    fn new(test: &str) -> Self {
        Self(std::env::var_os("NRESE_ORACLE_DUMP").map(|dir| PathBuf::from(dir).join(test)))
    }

    fn dir(&self, dataset: usize) -> Option<PathBuf> {
        let dir = self.0.as_ref()?.join(format!("d{dataset}"));
        std::fs::create_dir_all(&dir).unwrap();
        Some(dir)
    }

    /// Writes the dataset as NRESE loads it (`data.nq`) and with numbers in their canonical
    /// lexical forms (`data.canonical.nq`, for the oracle): NRESE's `STR` of `"07"^^xsd:integer`
    /// is `"7"`, a known deviation (benches/oracle/README.md) that would otherwise hide the
    /// unknown ones.
    fn dataset(&self, dataset: usize, quads: &[Quad]) {
        if let Some(dir) = self.dir(dataset) {
            let text: String = quads.iter().map(|q| format!("{q} .\n")).collect();
            std::fs::write(dir.join("data.nq"), text).unwrap();
            let canonical: String = quads
                .iter()
                .map(|q| {
                    let object = match &q.object {
                        Term::Literal(l) => Term::Literal(canonical_number(l)),
                        other => other.clone(),
                    };
                    let quad = Quad::new(
                        q.subject.clone(),
                        q.predicate.clone(),
                        object,
                        q.graph_name.clone(),
                    );
                    format!("{quad} .\n")
                })
                .collect();
            std::fs::write(dir.join("data.canonical.nq"), canonical).unwrap();
        }
    }

    /// `variables` gives the result's variables (evaluated only when dumping).
    fn query(
        &self,
        dataset: usize,
        query: usize,
        text: &str,
        ordered: bool,
        rows: &[String],
        variables: impl FnOnce() -> Vec<String>,
    ) {
        if let Some(dir) = self.dir(dataset) {
            std::fs::write(dir.join(format!("q{query}.rq")), text).unwrap();
            let header = variables().join("\t");
            let lines: String = std::iter::once(&header)
                .chain(rows)
                .map(|r| format!("{r}\n"))
                .collect();
            std::fs::write(dir.join(format!("q{query}.nrese")), lines).unwrap();
            if ordered {
                std::fs::write(dir.join(format!("q{query}.ordered")), "").unwrap();
            }
        }
    }
}

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn random_object(rng: &mut Rng) -> Term {
    match rng.below(12) {
        // Range pruning edge cases: dates with timezones next to the FILTER bounds, a
        // non-canonical integer and an xsd:int (both dictionary typed literals).
        9 => Literal::new_typed_literal(
            *rng.pick(&[
                "2001-01-01Z",
                "2000-12-31+14:00",
                "2001-01-02-14:00",
                "2000-01-01+05:30",
            ]),
            xsd::DATE,
        )
        .into(),
        10 => Literal::new_typed_literal(format!("0{}", rng.below(8)), xsd::INTEGER).into(),
        11 => Literal::new_typed_literal(rng.below(8).to_string(), xsd::INT).into(),
        0..=2 => ex(&format!("e{}", rng.below(6))).into(),
        3 => Literal::new_typed_literal(rng.below(8).to_string(), xsd::INTEGER).into(),
        4 => Literal::new_typed_literal(format!("{}.5", rng.below(5)), xsd::DECIMAL).into(),
        5 => Literal::new_typed_literal(format!("{}.25E0", rng.below(8)), xsd::DOUBLE).into(),
        6 => Literal::new_language_tagged_literal_unchecked(
            format!("s{}", rng.below(4)),
            *rng.pick(&["en", "de"]),
        )
        .into(),
        7 => Literal::new_simple_literal(format!("s{}", rng.below(4))).into(),
        _ => Literal::new_typed_literal(
            format!("200{}-01-0{}", rng.below(3), 1 + rng.below(3)),
            xsd::DATE,
        )
        .into(),
    }
}

fn random_dataset(rng: &mut Rng) -> Vec<Quad> {
    (0..10 + rng.below(50))
        .map(|_| {
            Quad::new(
                ex(&format!("e{}", rng.below(6))),
                ex(&format!("p{}", rng.below(4))),
                random_object(rng),
                GraphName::DefaultGraph,
            )
        })
        .collect()
}

const VARS: [&str; 4] = ["?a", "?b", "?c", "?d"];

fn term_or_var(rng: &mut Rng, object: bool) -> String {
    match rng.below(if object { 5 } else { 4 }) {
        0 | 1 => rng.pick(&VARS).to_string(),
        2 => format!("<{EX}e{}>", rng.below(7)), // e6 never occurs: unknown constant
        3 if object => rng.pick(&["3", "\"s1\"@en", "\"s2\"", "2.5"]).to_string(),
        _ => rng.pick(&VARS).to_string(),
    }
}

fn triple(rng: &mut Rng) -> String {
    let predicate = if rng.below(5) == 0 {
        rng.pick(&VARS).to_string()
    } else {
        format!("<{EX}p{}>", rng.below(4))
    };
    format!(
        "{} {} {} .",
        term_or_var(rng, false),
        predicate,
        term_or_var(rng, true)
    )
}

fn bgp(rng: &mut Rng) -> String {
    // Half of the patterns are connected and start from `?a <p> ?b`: each further triple
    // continues from a variable that is already bound, so the join has something to match
    // and the operators above it get rows to work on. The other half is unconstrained
    // (repeated variables, unknown constants, variable predicates) and mostly empty.
    if rng.below(2) == 0 {
        let mut triples = vec![format!("?a <{EX}p{}> ?b .", rng.below(4))];
        for _ in 0..rng.below(3) {
            let subject = *rng.pick(&["?a", "?b"]);
            let object = match rng.below(4) {
                0 => format!("<{EX}e{}>", rng.below(6)),
                _ => rng.pick(&["?c", "?d"]).to_string(),
            };
            triples.push(format!("{subject} <{EX}p{}> {object} .", rng.below(4)));
        }
        return triples.join(" ");
    }
    (0..1 + rng.below(3))
        .map(|_| triple(rng))
        .collect::<Vec<_>>()
        .join(" ")
}

fn filter(rng: &mut Rng) -> String {
    let v = rng.pick(&VARS);
    let w = rng.pick(&VARS);
    match rng.below(25) {
        19 => format!("FILTER(YEAR({v}) = 2001 || MONTH({w}) = 1)"),
        20 => format!("FILTER(ABS({v} - 3) < 2 || ROUND({w}) = 3)"),
        21 => format!("FILTER({v} * 2 > 5 && {v} / 2 != 1)"),
        22 => format!(
            "FILTER(CONCAT(STR({v}), \"x\") = \"s1x\" || STRAFTER(STR({w}), \"e\") = \"2\")"
        ),
        23 => format!("FILTER(-{v} <= -2 && CEIL({v}) >= FLOOR({v}))"),
        24 => format!(
            "FILTER(STRBEFORE({v}, \"1\") = \"s\" || STRLANG(STR({w}), \"en\") = \"s1\"@en)"
        ),
        17 => format!(
            "FILTER({v} >= \"2000-01-02\"^^<http://www.w3.org/2001/XMLSchema#date> && {v} < \"2001-01-02\"^^<http://www.w3.org/2001/XMLSchema#date>)"
        ),
        18 => format!("FILTER({v} > 1 && 5 >= {v} && {w} != 2)"),
        // The compiled (id-level) shapes, alone and combined:
        10 => format!("FILTER(CONTAINS({v}, \"1\") || STRSTARTS(STR({w}), \"http\"))"),
        11 => format!("FILTER(LANGMATCHES(LANG({v}), \"EN\") && !isBlank({w}))"),
        12 => format!("FILTER({v} = <{EX}e1> || {w} != <{EX}e9>)"),
        13 => format!("FILTER(3 < {v} && {v} <= 6)"),
        14 => format!("FILTER(STRENDS({v}, \"2\") || \"de\" = LANG({v}))"),
        15 => format!("FILTER(REGEX({v}, \"S\", \"i\"))"),
        16 => format!("FILTER({v} = 2 || sameTerm({w}, <{EX}e3>))"),
        0 => format!("FILTER({v} > 3)"),
        1 => format!("FILTER({v} <= 2.5 || {w} = <{EX}e1>)"),
        2 => format!("FILTER(isIRI({v}))"),
        3 => format!("FILTER(LANG({v}) = \"en\")"),
        4 => format!("FILTER(CONTAINS(STR({v}), \"1\"))"),
        5 => format!("FILTER({v} != {w})"),
        6 => format!("FILTER(!BOUND({v}) || isLiteral({v}))"),
        7 => format!("FILTER({v} >= \"2001-01-01\"^^<http://www.w3.org/2001/XMLSchema#date>)"),
        8 => format!("FILTER(REGEX(STR({v}), \"^s[12]\", \"i\"))"),
        _ => format!("FILTER({v} IN (1, \"s1\"@en, <{EX}e2>))"),
    }
}

fn group_pattern(rng: &mut Rng, depth: u32) -> String {
    let mut parts = vec![bgp(rng)];
    for _ in 0..rng.below(3) {
        let part = match rng.below(if depth > 1 { 3 } else { 10 }) {
            8 | 9 => {
                // A property path, ends variables or constants (e6 is unknown to the store).
                let path = rng.pick(&[
                    "<P0>*",
                    "<P1>+",
                    "(<P0>|<P1>)+",
                    "^<P2>/<P0>",
                    "!(<P0>|<P1>)",
                    "<P3>?",
                    "(<P0>/<P1>)*",
                    "<P2>/<P2>",
                    "(^<P1>)*",
                    "<P0>|<P3>",
                ]);
                let end = |rng: &mut Rng| match rng.below(3) {
                    0 => format!("<{EX}e{}>", rng.below(7)),
                    _ => rng.pick(&VARS).to_string(),
                };
                // A path that matches with length zero starts from a constant or from
                // `?z`, which no filter names. From a variable that a literal is bound to,
                // spareval's optimiser takes the start for a node and decides `?v = 2`
                // from that; SPARQL lets a zero-length path start at any term of the
                // graph. `paths_from_bound_values_equal_the_reference` covers bound starts.
                let subject = if path.ends_with('*') || path.ends_with('?') {
                    match rng.below(3) {
                        0 => format!("<{EX}e{}>", rng.below(7)),
                        _ => "?z".to_owned(),
                    }
                } else {
                    end(rng)
                };
                let path = path.replace("<P", &format!("<{EX}p"));
                format!("{subject} {path} {} .", end(rng))
            }
            7 => {
                let v = rng.pick(&VARS);
                let expression = rng.pick(&[
                    "VAR + 1",
                    "VAR * 1.5",
                    "VAR / 4",
                    "YEAR(VAR)",
                    "CONCAT(STR(VAR), \"-\")",
                    "STRDT(STR(VAR), <http://www.w3.org/2001/XMLSchema#integer>)",
                    "FLOOR(VAR)",
                ]);
                format!(
                    "BIND({} AS ?e{})",
                    expression.replace("VAR", v),
                    rng.below(1_000_000)
                )
            }
            0 | 1 => filter(rng),
            2 => bgp(rng),
            3 => format!("OPTIONAL {{ {} }}", group_pattern(rng, depth + 1)),
            4 => format!(
                "{{ {} }} UNION {{ {} }}",
                group_pattern(rng, depth + 1),
                group_pattern(rng, depth + 1)
            ),
            5 => format!("MINUS {{ {} }}", bgp(rng)),
            _ => format!(
                "FILTER {}EXISTS {{ {} }}",
                if rng.below(2) == 0 { "NOT " } else { "" },
                bgp(rng)
            ),
        };
        parts.push(part);
    }
    parts.join(" ")
}

fn random_query(rng: &mut Rng) -> (String, bool) {
    let pattern = group_pattern(rng, 0);
    if rng.below(8) == 0 {
        // LIMIT over one filtered pattern (the streamed scan); see `limited` below.
        return (
            format!(
                "SELECT * WHERE {{ {} {} }} LIMIT {}",
                triple(rng),
                filter(rng),
                1 + rng.below(4)
            ),
            false,
        );
    }
    match rng.below(4) {
        0 => {
            let key = rng.pick(&VARS);
            let arg = rng.pick(&VARS);
            let aggregate = rng.pick(&[
                "COUNT(*)",
                "COUNT(DISTINCT *)",
                "COUNT(ARG)",
                "SUM(ARG)",
                "AVG(ARG)",
                "MIN(ARG)",
                "MAX(ARG)",
                "COUNT(DISTINCT ARG)",
            ]);
            let aggregate = aggregate.replace("ARG", arg);
            (
                format!("SELECT {key} (({aggregate}) AS ?x) WHERE {{ {pattern} }} GROUP BY {key}"),
                false,
            )
        }
        1 => {
            // ORDER BY every projected variable: a deterministic sequence even with LIMIT.
            let limit = 1 + rng.below(5);
            (
                format!("SELECT ?a ?b WHERE {{ {pattern} }} ORDER BY DESC(?a) ?b LIMIT {limit}"),
                true,
            )
        }
        2 if rng.below(2) == 0 => (
            format!("SELECT DISTINCT ?a ?c WHERE {{ {pattern} }}"),
            false,
        ),
        // DISTINCT with ORDER BY over every projected variable: a deterministic sequence.
        2 => (
            format!("SELECT DISTINCT ?a ?c WHERE {{ {pattern} }} ORDER BY ?c DESC(?a)"),
            true,
        ),
        _ => (format!("SELECT * WHERE {{ {pattern} }}"), false),
    }
}

/// Integer literals by value: spareval's ORDER BY + LIMIT path outputs `"07"` and
/// `"7"^^xsd:int` as `"7"^^xsd:integer`, while the native executor returns the stored
/// terms (RDF term identity). Both are the same values; the test compares values there.
fn by_value(term: &Term) -> Term {
    const INTEGERS: [&str; 3] = [
        "http://www.w3.org/2001/XMLSchema#integer",
        "http://www.w3.org/2001/XMLSchema#int",
        "http://www.w3.org/2001/XMLSchema#long",
    ];
    match term {
        Term::Literal(l) if INTEGERS.contains(&l.datatype().as_str()) => {
            match l.value().parse::<i64>() {
                Ok(v) => Literal::new_typed_literal(v.to_string(), xsd::INTEGER).into(),
                Err(_) => term.clone(),
            }
        }
        _ => term.clone(),
    }
}

/// A numeric literal in the lexical form NRESE's `STR` gives it; other literals unchanged.
fn canonical_number(literal: &Literal) -> Literal {
    use oxsdatatypes::{Decimal, Double, Float, Integer};
    use std::str::FromStr;
    let value = literal.value();
    let text = match literal
        .datatype()
        .as_str()
        .strip_prefix("http://www.w3.org/2001/XMLSchema#")
    {
        Some("integer" | "int" | "long" | "short" | "byte") => {
            Integer::from_str(value).ok().map(|v| v.to_string())
        }
        Some("decimal") => Decimal::from_str(value).ok().map(|v| v.to_string()),
        Some("double") => Double::from_str(value).ok().map(|v| v.to_string()),
        Some("float") => Float::from_str(value).ok().map(|v| v.to_string()),
        _ => None,
    };
    match text {
        Some(text) => Literal::new_typed_literal(text, literal.datatype().into_owned()),
        None => literal.clone(),
    }
}

/// The variables of a query's result (`ASK` for an ASK), as a dump's header.
fn variables(snapshot: &nrese_engine::Snapshot, query: &spargebra::Query) -> Vec<String> {
    match evaluate_query(snapshot, query, &QueryOptions::default()).unwrap() {
        QueryResults::Solutions(solutions) => solutions
            .variables()
            .iter()
            .map(|v| v.as_str().to_owned())
            .collect(),
        _ => vec!["ASK".to_owned()],
    }
}

/// The rows of a SELECT, as strings; an ASK is one row, `true` or `false`.
fn rows(results: QueryResults<'_>, ordered: bool) -> Vec<String> {
    let solutions = match results {
        QueryResults::Solutions(solutions) => solutions,
        QueryResults::Boolean(answer) => return vec![answer.to_string()],
        QueryResults::Graph(_) => panic!("SELECT and ASK give no graph"),
    };
    let variables: Vec<_> = solutions.variables().to_vec();
    let mut out: Vec<String> = solutions
        .map(|solution| {
            let solution = solution.expect("no evaluation error");
            variables
                .iter()
                .map(|v| {
                    solution
                        .get(v)
                        .map_or("UNDEF".to_owned(), |t| by_value(t).to_string())
                })
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect();
    if !ordered {
        out.sort();
    }
    out
}

/// The LIMIT of a generated `SELECT * … LIMIT n` query without ORDER BY.
fn limited(text: &str) -> Option<usize> {
    (!text.contains("ORDER BY"))
        .then(|| text.rsplit_once(" LIMIT ")?.1.parse().ok())
        .flatten()
}

#[test]
fn native_results_equal_the_reference_on_random_queries() {
    let mut rng = Rng(20_260_927);
    let (mut checked, mut fallbacks) = (0, Vec::new());
    let mut with_solutions = 0;
    let dump = Dump::new("random");
    for dataset_case in 0..150 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let quads = random_dataset(&mut rng);
        dump.dataset(dataset_case, &quads);
        for quad in quads {
            if quad.graph_name.is_default_graph() && rng.below(3) == 0 {
                let triple = EncodedTriple::new(
                    tx.intern(quad.subject.as_ref().into()),
                    tx.intern(quad.predicate.as_ref().into()),
                    tx.intern(quad.object.as_ref()),
                );
                tx.insert_inferred(triple);
            } else {
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for query_case in 0..50 {
            let model = *rng.pick(&[
                ReadModel::Materialised,
                ReadModel::Asserted,
                ReadModel::Inferred,
            ]);
            let native_options = QueryOptions {
                read_model: model,
                ..QueryOptions::default()
            };
            let oracle = QueryOptions {
                read_model: model,
                ..QueryOptions::default()
            };
            let (text, ordered) = random_query(&mut rng);
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            if !runs_natively(&query) {
                fallbacks.push(text);
                continue;
            }
            let native = rows(
                evaluate_query(&snapshot, &query, &native_options).unwrap(),
                ordered,
            );
            if let Some(limit) = limited(&text) {
                // LIMIT without ORDER BY may return any `limit` solutions: the native rows
                // must be that many, and all of them solutions of the unlimited query.
                let unlimited = text[..text.rfind(" LIMIT ").unwrap()].to_owned();
                let query = SparqlParser::new().parse_query(&unlimited).unwrap();
                let mut all = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
                assert_eq!(native.len(), all.len().min(limit), "{text}");
                for row in &native {
                    let position = all.iter().position(|r| r == row);
                    assert!(position.is_some(), "{row} is not a solution: {text}");
                    all.remove(position.unwrap());
                }
                checked += 1;
                continue;
            }
            let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), ordered);
            assert_eq!(
                native, expected,
                "dataset {dataset_case}, query {query_case}, {model:?}: {text}"
            );
            if model == ReadModel::Materialised {
                dump.query(dataset_case, query_case, &text, ordered, &native, || {
                    variables(&snapshot, &query)
                });
            }
            checked += 1;
            with_solutions += usize::from(!expected.is_empty());
        }
    }
    // Agreement on empty results proves little: a fair share of the queries must have
    // solutions. It is a fifth (a tenth before the generator got its connected patterns);
    // the tests with their own generators below reach a quarter to three quarters.
    assert!(
        with_solutions * 6 > checked,
        "only {with_solutions} of {checked} queries have solutions"
    );
    // Queries outside native coverage run on spareval, which is correct by construction;
    // the generator's shapes must still be almost all native.
    assert!(
        fallbacks.len() * 20 < checked,
        "{} of {} generated queries not native, e.g. {:#?}",
        fallbacks.len(),
        checked + fallbacks.len(),
        &fallbacks[..fallbacks.len().min(3)]
    );
}

/// A group for the filter pushdown test: `?a <p0> ?b`, then parts that bind `?c` and `?d`
/// in some solutions, in all, or not at all (OPTIONAL, a path, UNION, MINUS, BIND,
/// subqueries), then filters over the four variables. Unlike [`group_pattern`], the parts
/// share variables in ways that match, so most queries have solutions to filter. `?f` is
/// what a BIND binds.
fn filtered_group(rng: &mut Rng) -> String {
    let p = |rng: &mut Rng| format!("<{EX}p{}>", rng.below(4));
    let mut parts = vec![format!("?a <{EX}p0> ?b .")];
    let mut bind = false;
    for _ in 0..1 + rng.below(3) {
        let (from, to) = (*rng.pick(&["?a", "?b"]), *rng.pick(&["?c", "?d"]));
        let (p1, p2) = (p(rng), p(rng));
        // BIND needs a variable the group hasn't used: `?f`, once.
        let choice = match rng.below(12) {
            7 | 8 if bind => 0,
            choice => choice,
        };
        bind |= matches!(choice, 7 | 8);
        parts.push(match choice {
            0 => format!("OPTIONAL {{ {from} {p1} {to} }}"),
            1 => format!("OPTIONAL {{ {from} {p1} {to} FILTER(isIRI({to})) }}"),
            // The zero-length path starts at `?a`, an IRI. From a variable bound to a
            // literal, spareval's optimiser takes the path's start for a node and compares
            // it as a term (`"04"^^xsd:integer != "4"^^xsd:int`), where SPARQL compares
            // values.
            2 => format!("OPTIONAL {{ {from} {p1} {to} . ?a {p2}* ?e FILTER({to} != ?b) }}"),
            3 => format!("{from} {p1}+ {to} ."),
            4 => format!("{from} ({p1}|{p2}) {to} ."),
            5 => format!("{{ {from} {p1} ?c }} UNION {{ {from} {p2} ?d }}"),
            6 => format!("MINUS {{ {from} {p1} {to} }}"),
            7 => format!("BIND(STR({from}) AS ?f)"),
            // Fails, and leaves its variable unbound, for everything but numbers.
            8 => match rng.below(3) {
                0 => "BIND(?b + 1 AS ?f)".to_owned(),
                // An IRI the store knows or one it doesn't, or a tagged string.
                1 => format!(
                    "BIND(IRI(CONCAT(STR({from}), \"{}\")) AS ?f)",
                    rng.pick(&["", "x"])
                ),
                _ => format!("BIND(STRLANG(STR({from}), \"en\") AS ?f)"),
            },
            9 => format!("{{ SELECT ?a {to} WHERE {{ ?a {p1} {to} . ?a {p2} ?b }} }}"),
            10 => format!(
                "{{ SELECT DISTINCT ?a {to} WHERE {{ ?a {p1} {to} }} ORDER BY DESC(?a) {to} LIMIT {} }}",
                1 + rng.below(8)
            ),
            _ => format!("{{ SELECT ?a (COUNT(*) AS {to}) WHERE {{ ?a {p1} ?b }} GROUP BY ?a }}"),
        });
    }
    for _ in 0..1 + rng.below(2) {
        let variables = ["?a", "?b", "?c", "?d", "?f"];
        let (v, w) = (*rng.pick(&variables), *rng.pick(&variables));
        parts.push(match rng.below(18) {
            // The compiled (id-level) shapes, which must leave computed values (`?f`) to
            // the general evaluator:
            10 => format!("FILTER(CONTAINS(STR({v}), \"e1\") || isBlank({w}))"),
            11 => format!("FILTER(STRSTARTS({v}, \"http\") || STRENDS(STR({w}), \"2\"))"),
            12 => format!("FILTER(REGEX(STR({v}), \"E[0-3]\", \"i\"))"),
            13 => format!("FILTER(LANG({v}) = \"en\" || LANGMATCHES(LANG({w}), \"DE\"))"),
            14 => format!("FILTER({v} = <{EX}e2> || {w} = <{EX}e2x>)"),
            15 => format!("FILTER({v} >= 2 && {v} < 6)"),
            16 => format!("FILTER(isLiteral({v}) && !isIRI({w}))"),
            17 => format!("FILTER(LANG({v}) = \"\")"),
            0 => format!("FILTER(isIRI({v}))"),
            1 => format!("FILTER(!BOUND({v}))"),
            2 => format!("FILTER(BOUND({v}) && {v} != {w})"),
            3 => format!("FILTER({v} != <{EX}e1>)"),
            4 => format!("FILTER(!BOUND({v}) || isLiteral({v}))"),
            5 => format!("FILTER(STR({v}) > \"http://example.com/e1\" && isIRI({w}))"),
            6 => format!("FILTER({v} > 2)"),
            7 => format!("FILTER(COALESCE({v}, 0) != 1)"),
            8 => format!("FILTER({v} = {w} || isLiteral({w}))"),
            _ => format!("FILTER(sameTerm({v}, {w}) || {v} != {w})"),
        });
    }
    parts.join(" ")
}

/// Filter pushdown (`native/pushdown.rs`): filters written at the end of a group give
/// spareval's results wherever the executor moves them: below joins, OPTIONALs, UNIONs,
/// MINUS and BINDs, and into subqueries. Each rule of the pushdown was mutation-checked
/// against this test.
#[test]
fn pushed_filters_equal_the_reference() {
    let mut rng = Rng(20_260_930);
    let (mut checked, mut with_solutions, mut fallbacks) = (0, 0, 0);
    let oracle = QueryOptions {
        ..QueryOptions::default()
    };
    let dump = Dump::new("pushed-filters");
    for dataset_case in 0..60 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let quads = random_dataset(&mut rng);
        dump.dataset(dataset_case, &quads);
        for quad in quads {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for query_case in 0..60 {
            let text = format!("SELECT * WHERE {{ {} }}", filtered_group(&mut rng));
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            if !runs_natively(&query) {
                fallbacks += 1;
                continue;
            }
            let native = rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false,
            );
            let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
            if native != expected {
                let only = |rows: &[String], other: &[String]| -> Vec<String> {
                    let mut other = other.to_vec();
                    rows.iter()
                        .filter(|row| match other.iter().position(|r| r == *row) {
                            Some(at) => {
                                other.swap_remove(at);
                                false
                            }
                            None => true,
                        })
                        .take(5)
                        .cloned()
                        .collect()
                };
                panic!(
                    "dataset {dataset_case}, query {query_case}: {text}
{} native and {} reference rows
only native: {:#?}
only reference: {:#?}",
                    native.len(),
                    expected.len(),
                    only(&native, &expected),
                    only(&expected, &native)
                );
            }
            dump.query(dataset_case, query_case, &text, false, &native, || {
                variables(&snapshot, &query)
            });
            checked += 1;
            with_solutions += usize::from(!expected.is_empty());
        }
    }
    assert!(
        fallbacks * 5 < checked,
        "{fallbacks} of {} queries not native",
        checked + fallbacks
    );
    // A filter that drops everything proves little: enough queries must keep solutions.
    assert!(
        with_solutions * 5 > checked,
        "only {with_solutions} of {checked} queries have solutions"
    );
}

/// The rows of two evaluations, or a panic that shows the rows only one of them has.
fn assert_same_rows(native: &[String], expected: &[String], context: &str) {
    if native == expected {
        return;
    }
    let only = |rows: &[String], other: &[String]| -> Vec<String> {
        let mut other = other.to_vec();
        rows.iter()
            .filter(|row| match other.iter().position(|r| r == *row) {
                Some(at) => {
                    other.swap_remove(at);
                    false
                }
                None => true,
            })
            .take(5)
            .cloned()
            .collect()
    };
    panic!(
        "{context}
{} native and {} reference rows
only native: {:#?}
only reference: {:#?}",
        native.len(),
        expected.len(),
        only(native, expected),
        only(expected, native)
    );
}

/// A property path joined to a pattern that binds one of its ends is followed from those
/// values only (`Context::path_from`): the same rows, with their multiplicities, as the
/// open path joined afterwards. Every path form, from either end, both ends, the same
/// variable at both ends, under OPTIONAL, with a filter of its own, from values the store
/// doesn't know, and over the merge of all graphs. Mutation-checked.
#[test]
fn paths_from_bound_values_equal_the_reference() {
    const PATHS: [&str; 14] = [
        "<P0>*",
        "<P1>+",
        "(<P0>|<P1>)+",
        "^<P2>/<P0>",
        "!(<P0>|<P1>)",
        "<P3>?",
        "(<P0>/<P1>)*",
        "<P2>/<P2>",
        "(^<P1>)*",
        "<P0>|<P3>",
        "(<P0>|^<P0>)*",
        "^<P1>",
        "<P0>/<P1>/^<P0>",
        "(<P2>|<P2>)/<P0>?",
    ];
    let mut rng = Rng(20_261_001);
    let (mut checked, mut with_solutions) = (0, 0);
    for dataset_case in 0..50 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let quads = random_dataset(&mut rng);
        // Half of the cases spread the statements over graphs and read their merge.
        let merged = dataset_case % 2 == 1;
        for (i, quad) in quads.iter().enumerate() {
            let graph: GraphName = match (merged, i % 3) {
                (true, 1) => ex("g1").into(),
                (true, 2) => ex("g2").into(),
                _ => GraphName::DefaultGraph,
            };
            let quad = Quad::new(
                quad.subject.clone(),
                quad.predicate.clone(),
                quad.object.clone(),
                graph,
            );
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        let native_options = QueryOptions {
            union_default_graph: merged,
            ..QueryOptions::default()
        };
        let oracle = QueryOptions {
            union_default_graph: merged,
            ..QueryOptions::default()
        };
        for query_case in 0..60 {
            let path = |rng: &mut Rng| rng.pick(&PATHS).replace("<P", &format!("<{EX}p"));
            let (p, q) = (path(&mut rng), path(&mut rng));
            // A closure: each pair once.
            let closure = rng
                .pick(&[PATHS[0], PATHS[1], PATHS[2], PATHS[6], PATHS[8], PATHS[10]])
                .replace("<P", &format!("<{EX}p"));
            let first = format!("?a <{EX}p{}> ?b .", rng.below(4));
            let group = match rng.below(12) {
                0 => format!("{first} ?b {p} ?c"),
                1 => format!("{first} ?c {p} ?b"),
                // Both ends bound. Only closures: where a path has a pair twice (two
                // statements behind `!(…)`, two ways through a sequence), spareval tests
                // for the path per row and returns the row once; SPARQL counts both.
                2 => format!("{first} ?a {closure} ?b"),
                3 => format!("{first} ?b {p} ?b"),
                4 => format!("{first} OPTIONAL {{ ?b {p} ?c }}"),
                5 => format!("{first} OPTIONAL {{ ?c {p} ?a FILTER(isIRI(?c) && ?c != ?a) }}"),
                6 => format!("?c {p} ?b . {first}"),
                7 => format!("{first} ?b {p} ?c FILTER(isIRI(?c))"),
                8 => format!("{first} ?b {p} ?c . ?c {q} ?d"),
                // Values the store may not know (e6 never occurs), and a literal.
                9 => format!("VALUES ?b {{ <{EX}e1> <{EX}e6> 3 \"s1\" }} ?b {p} ?c"),
                10 => format!("{first} BIND(IRI(CONCAT(STR(?a), \"\")) AS ?f) ?f {p} ?c"),
                _ => format!("{first} {{ ?b {p} ?c }} UNION {{ ?c {q} ?a }}"),
            };
            let text = format!("SELECT * WHERE {{ {group} }}");
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            assert!(runs_natively(&query), "{text}");
            let native = rows(
                evaluate_query(&snapshot, &query, &native_options).unwrap(),
                false,
            );
            let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
            assert_same_rows(
                &native,
                &expected,
                &format!("dataset {dataset_case} (merged: {merged}), query {query_case}: {text}"),
            );
            checked += 1;
            with_solutions += usize::from(!expected.is_empty());
        }
    }
    assert!(
        with_solutions * 2 > checked,
        "only {with_solutions} of {checked} queries have solutions"
    );
}

/// With many bound values a closure walks an adjacency built from one scan of its step,
/// from those values, forwards or backwards.
#[test]
fn closures_from_many_bound_values_equal_the_reference() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    // 6,000 members of 1,500 chains of four, some of them closed into a cycle.
    for i in 0..6_000u32 {
        let node = ex(&format!("n{i}"));
        let member = Quad::new(node.clone(), ex("in"), ex("set"), GraphName::DefaultGraph);
        tx.insert(member.as_ref());
        let next = match (i % 4, i % 28) {
            (3, 27) => Some(i - 3),
            (3, _) => None,
            _ => Some(i + 1),
        };
        if let Some(next) = next {
            let quad = Quad::new(
                node,
                ex("next"),
                ex(&format!("n{next}")),
                GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let oracle = QueryOptions {
        ..QueryOptions::default()
    };
    for group in [
        format!("?a <{EX}in> ?s . ?a <{EX}next>+ ?b"),
        format!("?a <{EX}in> ?s . ?b <{EX}next>+ ?a"),
        format!("?a <{EX}in> ?s . ?a <{EX}next>* ?b"),
        format!("?a <{EX}in> ?s . ?b <{EX}next>* ?a"),
        format!("?a <{EX}in> ?s . ?a <{EX}next>+ ?a"),
        // Literals and the class are bound too: not nodes of any `next` edge.
        format!("?x ?p ?a . ?a <{EX}next>* ?b"),
        // The predicates are bound as well, and they are no nodes: a zero-length path
        // doesn't start from them.
        format!("{{ ?a <{EX}in> ?s }} UNION {{ ?x ?a ?y }} ?a <{EX}next>* ?b"),
        format!("{{ ?a <{EX}in> ?s }} UNION {{ ?x ?a ?y }} ?b <{EX}next>* ?a"),
    ] {
        let text = format!("SELECT ?a ?b WHERE {{ {group} }}");
        let query = SparqlParser::new().parse_query(&text).unwrap();
        assert!(runs_natively(&query), "{text}");
        let native = rows(
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
            false,
        );
        let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
        assert!(expected.len() > 100, "{text}");
        assert_same_rows(&native, &expected, &text);
    }
}

/// A query over values the query computes itself: grouping, aggregating, ordering,
/// deduplicating, joining and filtering on the results of `BIND` and `SELECT` expressions
/// and of aggregates in subqueries. The patterns are single statements and joins on `?a`,
/// so nearly every query has solutions. Returns the query and whether its rows are ordered.
///
/// Left out, because spareval differs from the specification there: `DATATYPE` of a
/// literal of a derived integer type (spareval answers `xsd:integer` for an `xsd:int`).
fn computed_query(rng: &mut Rng) -> (String, bool) {
    let p = |rng: &mut Rng| format!("<{EX}p{}>", rng.below(4));
    let base = match rng.below(3) {
        0 => format!("?a {} ?b", p(rng)),
        1 => format!("?a {} ?b . ?a {} ?c", p(rng), p(rng)),
        _ => format!("?a {} ?b OPTIONAL {{ ?a {} ?c }}", p(rng), p(rng)),
    };
    let expression = rng
        .pick(&[
            "STR(?b)",
            "isIRI(?b)",
            "?b + 1",
            "?b * 1.5",
            "LANG(?b)",
            "YEAR(?b)",
            "STRLEN(STR(?b))",
            "UCASE(STR(?b))",
            "IF(isLiteral(?b), \"lit\", \"iri\")",
            "COALESCE(?b + 0, STRLEN(STR(?b)))",
            "CONCAT(STR(?a), \"/\", STR(?b))",
            "IRI(STR(?b))",
            "SUBSTR(STR(?b), 2, 3)",
            "?b = 3 || isIRI(?b)",
            "ABS(?b - 4)",
            "STRDT(STR(?b), <http://www.w3.org/2001/XMLSchema#integer>)",
        ])
        .to_string();
    let bound = format!("{base} BIND({expression} AS ?k)");
    match rng.below(14) {
        0 => {
            let aggregate = rng
                .pick(&[
                    "COUNT(?b)",
                    "SUM(?b)",
                    "AVG(?b)",
                    "MIN(?b)",
                    "MAX(?b)",
                    "COUNT(DISTINCT ?b)",
                    "COUNT(*)",
                ])
                .to_string();
            (
                format!("SELECT ?a (({aggregate}) AS ?x) WHERE {{ {base} }} GROUP BY ?a"),
                false,
            )
        }
        // Grouping by a computed key, and aggregates of computed values.
        1 => (
            format!(
                "SELECT ?k (COUNT(*) AS ?n) (COUNT(DISTINCT ?a) AS ?d) WHERE {{ {bound} }} GROUP BY ?k"
            ),
            false,
        ),
        2 => (
            format!(
                "SELECT ?a (SUM(?k) AS ?s) (MIN(?k) AS ?lo) (MAX(?k) AS ?hi) (COUNT(?k) AS ?n) (COUNT(DISTINCT ?k) AS ?d) (AVG(?k) AS ?m) WHERE {{ {bound} }} GROUP BY ?a"
            ),
            false,
        ),
        3 => (
            format!(
                "SELECT ?a (COUNT(?b) AS ?n) WHERE {{ {base} }} GROUP BY ?a HAVING (COUNT(?b) > 1 && MAX(?b) != MIN(?b))"
            ),
            false,
        ),
        // A total order over computed values: every variable is a sort key.
        4 => (
            format!("SELECT ?k ?a ?b WHERE {{ {bound} }} ORDER BY ?k ?a ?b"),
            true,
        ),
        5 => (
            format!(
                "SELECT ?k ?a ?b WHERE {{ {bound} }} ORDER BY DESC(?k) DESC(?b) ?a LIMIT {} OFFSET {}",
                1 + rng.below(9),
                rng.below(3)
            ),
            true,
        ),
        6 => (format!("SELECT DISTINCT ?k WHERE {{ {bound} }}"), false),
        // Joining on a computed value, which may or may not be a term of the store.
        7 => (
            format!("SELECT * WHERE {{ {{ {bound} }} ?k {} ?d }}", p(rng)),
            false,
        ),
        8 => (
            format!("SELECT * WHERE {{ {{ {bound} }} {{ ?e {} ?k }} }}", p(rng)),
            false,
        ),
        // Filters over computed values, beyond the compiled shapes.
        9 => {
            let filter = rng
                .pick(&[
                    "?k IN (1, 2, \"s1\", \"lit\", true)",
                    "IF(BOUND(?k), ?k != ?b, true)",
                    "COALESCE(?k, 0) != 1",
                    "?k = ?b || !BOUND(?k)",
                    "STR(?k) < \"m\"",
                    "?k > 2 || isIRI(?k)",
                    "sameTerm(?k, ?b)",
                    "BOUND(?k) && DATATYPE(?k) = <http://www.w3.org/2001/XMLSchema#integer>",
                ])
                .to_string();
            (
                format!("SELECT * WHERE {{ {bound} FILTER({filter}) }}"),
                false,
            )
        }
        // An aggregate of a subquery, joined and filtered outside.
        10 => (
            format!(
                "SELECT * WHERE {{ {{ SELECT ?a (COUNT(*) AS ?n) (MAX(?b) AS ?top) WHERE {{ {base} }} GROUP BY ?a }} ?a {} ?d FILTER(?n >= {}) }}",
                p(rng),
                1 + rng.below(3)
            ),
            false,
        ),
        11 => (
            format!(
                "SELECT ?a ?b (STRLEN(STR(?b)) AS ?l) (IF(isLiteral(?b), ?b, ?a) AS ?f) (?l + 1 AS ?m) WHERE {{ {base} }}"
            ),
            false,
        ),
        12 => (
            format!(
                "SELECT ?a ?k WHERE {{ {bound} VALUES ?a {{ <{EX}e0> <{EX}e1> <{EX}e2> <{EX}e6> }} MINUS {{ ?a {} ?k }} }}",
                p(rng)
            ),
            false,
        ),
        _ => (
            format!(
                "SELECT ?k (COUNT(*) AS ?n) WHERE {{ {{ {base} BIND(\"left\" AS ?k) }} UNION {{ ?a {} ?b BIND(STR(?b) AS ?k) }} }} GROUP BY ?k",
                p(rng)
            ),
            false,
        ),
    }
}

/// Queries over computed values give the reference evaluator's results: see [`computed_query`].
#[test]
fn computed_values_equal_the_reference() {
    let mut rng = Rng(20_261_002);
    let (mut checked, mut with_solutions, mut fallbacks) = (0, 0, 0);
    let oracle = QueryOptions {
        ..QueryOptions::default()
    };
    let dump = Dump::new("computed-values");
    for dataset_case in 0..60 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let quads = random_dataset(&mut rng);
        dump.dataset(dataset_case, &quads);
        for quad in quads {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for query_case in 0..60 {
            let (text, ordered) = computed_query(&mut rng);
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            if !runs_natively(&query) {
                fallbacks += 1;
                continue;
            }
            let native = rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                ordered,
            );
            let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), ordered);
            assert_same_rows(
                &native,
                &expected,
                &format!("dataset {dataset_case}, query {query_case}: {text}"),
            );
            dump.query(dataset_case, query_case, &text, ordered, &native, || {
                variables(&snapshot, &query)
            });
            checked += 1;
            with_solutions += usize::from(!expected.is_empty());
        }
    }
    assert!(
        fallbacks * 5 < checked,
        "{fallbacks} of {} queries not native",
        checked + fallbacks
    );
    assert!(
        with_solutions * 4 > checked * 3,
        "only {with_solutions} of {checked} queries have solutions"
    );
}

/// A query whose consumer ignores duplicate rows (`SELECT DISTINCT`, or a group of
/// `DISTINCT`, `MIN`, `MAX` and `SAMPLE` aggregates) over `?a <p> ?b` and OPTIONALs that
/// are independent of each other, depend on each other, carry conditions, paths and
/// BINDs. Some queries mix in an aggregate that does count duplicates, which switches the
/// set evaluation off. Returns the query and whether its results depend on the order of
/// the rows (`GROUP_CONCAT`, `SAMPLE`).
fn set_query(rng: &mut Rng) -> (String, bool) {
    let p = |rng: &mut Rng| format!("<{EX}p{}>", rng.below(4));
    let mut pattern = format!("?a {} ?b .", p(rng));
    for _ in 0..1 + rng.below(3) {
        let (p1, p2) = (p(rng), p(rng));
        pattern.push_str(&match rng.below(9) {
            0 | 1 => format!(" OPTIONAL {{ ?a {p1} ?c }}"),
            2 => format!(" OPTIONAL {{ ?a {p1} ?d }}"),
            3 => format!(" OPTIONAL {{ ?a {p1} ?d FILTER(?d != ?b) }}"),
            4 => format!(" OPTIONAL {{ ?b {p1} ?e }}"),
            // Depends on an earlier OPTIONAL's variable, if there is one.
            5 => format!(" OPTIONAL {{ ?a {p1} ?c . ?a {p2} ?g }}"),
            6 => format!(" OPTIONAL {{ ?a ({p1}|{p2})+ ?e }}"),
            7 => format!(" OPTIONAL {{ ?a {p1} ?d BIND(STR(?d) AS ?h) }}"),
            _ => format!(" ?a {p1} ?g ."),
        });
    }
    // A filter that must stay above the OPTIONALs, on variables nothing else may need.
    match rng.below(6) {
        0 => pattern.push_str(" FILTER(!BOUND(?c) || ?c != ?b)"),
        1 => pattern.push_str(" FILTER(!BOUND(?d) || isLiteral(?d))"),
        _ => {}
    }
    if rng.below(4) == 0 {
        pattern.push_str(" BIND(STRLEN(STR(?b)) AS ?len)");
    }
    if rng.below(4) == 0 {
        return (
            format!(
                "SELECT DISTINCT {} WHERE {{ {pattern} }}",
                rng.pick(&["?a ?c", "?a ?d ?e", "?b", "?a ?len", "?c ?d", "?a ?h ?g"])
            ),
            false,
        );
    }
    let keys = *rng.pick(&["?a", "?a ?b", "?b", "?a ?c", "?len ?a"]);
    let mut ordered = false;
    let aggregates: Vec<String> = (0..1 + rng.below(3))
        .map(|i| {
            let aggregate = match rng.below(16) {
                0 | 1 => "COUNT(DISTINCT ?c)",
                2 | 3 => "COUNT(DISTINCT ?d)",
                4 => "COUNT(DISTINCT ?e)",
                5 => "MIN(?c)",
                6 => "MAX(?d)",
                7 => "MAX(STR(?e))",
                8 => "COUNT(DISTINCT ?g)",
                // Compared with the as-written evaluation only: SPARQL's DISTINCT is over
                // terms ("03" and "3" are two), spareval's SUM(DISTINCT) over values.
                9 => {
                    ordered = true;
                    "SUM(DISTINCT ?d)"
                }
                // Over two OPTIONALs at once. COALESCE keeps the argument free of errors:
                // for a COUNT(DISTINCT ...) whose argument fails in every row, spareval
                // answers unbound, where SPARQL drops the errors and counts 0.
                10 => {
                    "COUNT(DISTINCT CONCAT(COALESCE(STR(?c), \"-\"), \"/\", COALESCE(STR(?d), \"-\")))"
                }
                11 => "MIN(?h)",
                12 => {
                    ordered = true;
                    "GROUP_CONCAT(DISTINCT STR(?c); separator=\"|\")"
                }
                13 => {
                    ordered = true;
                    "SAMPLE(?d)"
                }
                // These count duplicates: the whole group is evaluated as written.
                14 => "COUNT(?c)",
                _ => "COUNT(*)",
            };
            format!("({aggregate} AS ?x{i})")
        })
        .collect();
    let having = match rng.below(5) {
        0 => " HAVING (COUNT(DISTINCT ?c) > 1)",
        1 => " HAVING (MIN(?c) != MAX(?c) || COUNT(DISTINCT ?d) = 0)",
        _ => "",
    };
    (
        format!(
            "SELECT {keys} {} WHERE {{ {pattern} }} GROUP BY {keys}{having}",
            aggregates.join(" ")
        ),
        ordered,
    )
}

/// Set evaluation (`native/sets.rs`): queries whose consumers ignore duplicates give the
/// results of the same queries evaluated as written, and the reference evaluator's. Order-dependent
/// results (`GROUP_CONCAT`, `SAMPLE`) are compared with the as-written evaluation only,
/// where they must be identical: the set evaluation keeps first occurrences in order.
/// Mutation-checked.
#[test]
fn duplicate_insensitive_queries_equal_both_evaluations() {
    let mut rng = Rng(20_261_003);
    let (mut checked, mut with_solutions) = (0, 0);
    let as_written = QueryOptions {
        as_written: true,
        ..QueryOptions::default()
    };
    let oracle = QueryOptions {
        ..QueryOptions::default()
    };
    for dataset_case in 0..60 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for query_case in 0..60 {
            let (text, order_dependent) = set_query(&mut rng);
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            assert!(runs_natively(&query), "{text}");
            let context = format!("dataset {dataset_case}, query {query_case}: {text}");
            let native = rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false,
            );
            let plain = rows(
                evaluate_query(&snapshot, &query, &as_written).unwrap(),
                false,
            );
            assert_same_rows(&native, &plain, &format!("against as written: {context}"));
            if !order_dependent {
                let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
                assert_same_rows(&native, &expected, &context);
            }
            checked += 1;
            with_solutions += usize::from(!native.is_empty());
        }
    }
    assert!(
        with_solutions * 4 > checked * 3,
        "only {with_solutions} of {checked} queries have solutions"
    );
}

/// Joins on variables that may be unbound (SPARQL compatibility: an unbound variable is
/// compatible with any value and takes it): inner joins, OPTIONAL, MINUS, (NOT) EXISTS and
/// joins that keep the order of a sorted subquery, all after an OPTIONAL that binds the
/// shared variable in some rows only. They run natively (they used to go to the general
/// evaluator while running) and give the reference evaluator's results. Wiring mutation-checked; the
/// kernels have their own test against the definitions.
#[test]
fn joins_on_unbound_variables_equal_the_reference() {
    let mut rng = Rng(20_261_004);
    let (mut checked, mut with_solutions) = (0, 0);
    let oracle = QueryOptions {
        ..QueryOptions::default()
    };
    for dataset_case in 0..60 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for query_case in 0..50 {
            let p = |rng: &mut Rng| format!("<{EX}p{}>", rng.below(4));
            // ?c is bound where the OPTIONAL matches.
            let base = format!("?a {} ?b OPTIONAL {{ ?a {} ?c }}", p(&mut rng), p(&mut rng));
            let (p1, p2) = (p(&mut rng), p(&mut rng));
            let group = match rng.below(10) {
                0 => format!("{base} OPTIONAL {{ ?c {p1} ?d }}"),
                1 => format!("{base} OPTIONAL {{ ?a {p1} ?c }}"),
                2 => format!("{base} OPTIONAL {{ ?b {p1} ?c FILTER(?c != ?a) }}"),
                3 => format!("{base} ?c {p1} ?d ."),
                4 => format!("{base} MINUS {{ ?c {p1} ?e }}"),
                5 => format!("{base} MINUS {{ ?a {p1} ?c }}"),
                6 => format!("{base} FILTER EXISTS {{ ?c {p1} ?e }}"),
                7 => format!("{base} FILTER NOT EXISTS {{ ?b {p1} ?c }}"),
                // The sorted subquery's order is kept through the join on ?c.
                8 => format!("{{ SELECT ?a ?c WHERE {{ {base} }} ORDER BY ?a ?c }} ?c {p1} ?d"),
                _ => format!("{base} OPTIONAL {{ ?c {p1} ?d }} OPTIONAL {{ ?d {p2} ?e }}"),
            };
            let text = format!("SELECT * WHERE {{ {group} }}");
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let context = format!("dataset {dataset_case}, query {query_case}: {text}");
            assert!(runs_natively(&query), "{context}");
            let explained = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
            assert_eq!(explained.executor, "native", "{context}");
            let native = rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false,
            );
            let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
            assert_same_rows(&native, &expected, &context);
            checked += 1;
            with_solutions += usize::from(!expected.is_empty());
        }
    }
    assert!(
        with_solutions * 4 > checked * 3,
        "only {with_solutions} of {checked} queries have solutions"
    );
}

/// A filter on a variable that a subquery binds but doesn't project sees it unbound: the
/// filter is an error and the row is dropped. Checked against the answer the specification
/// gives, not against spareval, whose optimiser moves such a filter into the subquery
/// (where the variable is bound) and returns the rows.
#[test]
fn a_filter_sees_a_variable_a_subquery_hides_as_unbound() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for (s, p, o) in [("e0", "p0", "e1"), ("e0", "p1", "e2"), ("e3", "p1", "e2")] {
        let quad = Quad::new(ex(s), ex(p), ex(o), GraphName::DefaultGraph);
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let answers = |group: &str| {
        let text = format!(
            "SELECT * WHERE {{ {} }}",
            group.replace('<', &format!("<{EX}"))
        );
        let query = SparqlParser::new().parse_query(&text).unwrap();
        assert!(runs_natively(&query), "{text}");
        rows(
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
            false,
        )
        .len()
    };
    let subquery = "{ SELECT ?a WHERE { ?a <p0> ?c . ?a <p1> ?d } }";
    // The subquery alone has the solution ?a = e0.
    assert_eq!(answers(subquery), 1);
    // ?c and ?d are not visible outside it.
    assert_eq!(answers(&format!("{subquery} FILTER(isIRI(?c))")), 0);
    assert_eq!(answers(&format!("{subquery} FILTER(?c != ?a)")), 0);
    assert_eq!(answers(&format!("{subquery} FILTER(!BOUND(?c))")), 1);
    // In a UNION: the branch without the variable has no solutions, the other keeps its own.
    assert_eq!(
        answers(&format!(
            "{{ {subquery} UNION {{ ?a <p1> ?d }} FILTER(isIRI(?d)) }}"
        )),
        2
    );
    // A variable it projects is visible, and the filter may move inside.
    assert_eq!(answers(&format!("{subquery} FILTER(isIRI(?a))")), 1);
    assert_eq!(answers(&format!("{subquery} FILTER(?a = <e3>)")), 0);
}

#[test]
fn count_star_of_one_pattern_reads_the_index() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..1000 {
        tx.insert(
            Quad::new(
                ex(&format!("s{i}")),
                ex("p"),
                Literal::new_simple_literal("o"),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    for (text, expected) in [
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
            "\"1000\"^^<http://www.w3.org/2001/XMLSchema#integer>",
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://example.com/p> ?o }",
            "\"1000\"^^<http://www.w3.org/2001/XMLSchema#integer>",
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <http://example.com/missing> ?o }",
            "\"0\"^^<http://www.w3.org/2001/XMLSchema#integer>",
        ),
    ] {
        let query = SparqlParser::new().parse_query(text).unwrap();
        assert!(runs_natively(&query));
        assert_eq!(
            rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false
            ),
            vec![expected]
        );
    }
}

/// Cyclic BGPs (triangles, 4-cycles, with types, constants, repeated variables and a
/// variable predicate) over dense graphs: the worst-case-optimal join equals the reference evaluator.
#[test]
fn cyclic_bgps_equal_the_reference() {
    let mut rng = Rng(20_260_929);
    let oracle = QueryOptions {
        ..QueryOptions::default()
    };
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let mut checked = 0;
    for _ in 0..40 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for _ in 0..40 + rng.below(120) {
            let quad = Quad::new(
                ex(&format!("e{}", rng.below(8))),
                ex(&format!("p{}", rng.below(3))),
                ex(&format!("e{}", rng.below(8))),
                GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
        for e in 0..8 {
            if rng.below(2) == 0 {
                let quad = Quad::new(
                    ex(&format!("e{e}")),
                    NamedNode::new_unchecked(rdf_type),
                    ex(&format!("T{}", rng.below(2))),
                    GraphName::DefaultGraph,
                );
                tx.insert(quad.as_ref());
            }
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..25 {
            let p = |rng: &mut Rng| format!("<{EX}p{}>", rng.below(3));
            let edge = |rng: &mut Rng, a: &str, b: &str| {
                let predicate = p(rng);
                if rng.below(2) == 0 {
                    format!("{a} {predicate} {b} .")
                } else {
                    format!("{b} {predicate} {a} .")
                }
            };
            let mut body = match rng.below(6) {
                0 | 1 => format!(
                    "{} {} {}",
                    edge(&mut rng, "?a", "?b"),
                    edge(&mut rng, "?b", "?c"),
                    edge(&mut rng, "?c", "?a")
                ),
                2 => format!(
                    "{} {} {} {}",
                    edge(&mut rng, "?a", "?b"),
                    edge(&mut rng, "?b", "?c"),
                    edge(&mut rng, "?c", "?d"),
                    edge(&mut rng, "?d", "?a")
                ),
                3 => format!(
                    "?a ?q ?b . {} {}",
                    edge(&mut rng, "?b", "?c"),
                    edge(&mut rng, "?c", "?a")
                ),
                4 => format!(
                    "?a <{EX}p0> ?a . {} {}",
                    edge(&mut rng, "?a", "?b"),
                    edge(&mut rng, "?b", "?a")
                ),
                _ => {
                    let (x, y) = (
                        format!("<{EX}e{}>", rng.below(8)),
                        format!("<{EX}e{}>", rng.below(8)),
                    );
                    format!(
                        "{} {} {}",
                        edge(&mut rng, "?a", "?b"),
                        edge(&mut rng, "?b", &x),
                        edge(&mut rng, &y, "?a")
                    )
                }
            };
            if rng.below(2) == 0 {
                body.push_str(&format!(" ?a a <{EX}T{}> .", rng.below(2)));
            }
            let text = format!("SELECT * WHERE {{ {body} }}");
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            assert!(runs_natively(&query), "{text}");
            let native = rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false,
            );
            let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
            assert_eq!(native, expected, "{text}");
            checked += 1;
        }
    }
    assert_eq!(checked, 1000);
}

/// A selective pattern joined with a much larger one runs as a parallel index nested-loop
/// join (enough rows for several chunks); it equals the reference evaluator.
#[test]
fn parallel_probe_join_equals_the_reference() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    for i in 0..12_000 {
        let quad = Quad::new(
            ex(&format!("t{i}")),
            rdf_type.clone(),
            ex("T"),
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
        for j in 0..(i % 4) {
            let quad = Quad::new(
                ex(&format!("t{i}")),
                ex("p"),
                ex(&format!("o{}", (i * 7 + j) % 5000)),
                GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
    }
    for i in 0..400_000 {
        let quad = Quad::new(
            ex(&format!("u{}", i / 4)),
            ex("p"),
            ex(&format!("o{}", i % 5000)),
            GraphName::DefaultGraph,
        );
        tx.insert(quad.as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let oracle = QueryOptions {
        ..QueryOptions::default()
    };
    for text in [
        format!("SELECT ?x ?y WHERE {{ ?x a <{EX}T> . ?x <{EX}p> ?y }}"),
        format!("SELECT ?x ?y WHERE {{ ?x a <{EX}T> OPTIONAL {{ ?x <{EX}p> ?y }} }}"),
    ] {
        let query = SparqlParser::new().parse_query(&text).unwrap();
        assert!(runs_natively(&query), "{text}");
        let native = rows(
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
            false,
        );
        let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
        assert!(native.len() >= 12_000, "{text}");
        assert_eq!(native, expected, "{text}");
    }
}

/// EXPLAIN runs the query and reports every operator: the BGP's join order with estimated
/// and actual rows, and the operators above it.
#[test]
fn explain_reports_the_plan() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    for i in 0..500 {
        let x = ex(&format!("x{i}"));
        let class = if i % 10 == 0 { "Grad" } else { "Student" };
        let quads = [
            Quad::new(
                x.clone(),
                rdf_type.clone(),
                ex(class),
                GraphName::DefaultGraph,
            ),
            Quad::new(
                x.clone(),
                ex("memberOf"),
                ex(&format!("d{}", i % 25)),
                GraphName::DefaultGraph,
            ),
            Quad::new(
                x,
                ex("takes"),
                ex(&format!("c{}", i % 40)),
                GraphName::DefaultGraph,
            ),
        ];
        for quad in &quads {
            tx.insert(quad.as_ref());
        }
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let text = format!(
        "SELECT ?x ?c WHERE {{ ?x a <{EX}Grad> . ?x <{EX}memberOf> <{EX}d0> . ?x <{EX}takes> ?c FILTER(?c != <{EX}c1>) }}"
    );
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let explanation = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
    let solutions = rows(
        evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
        false,
    );
    assert_eq!(explanation.executor, "native");
    assert_eq!(explanation.rows, solutions.len() as u64);
    let operators: Vec<(usize, &str)> = explanation
        .steps
        .iter()
        .map(|s| (s.depth, s.operator.as_str()))
        .collect();
    assert_eq!(operators[0], (0, "project"));
    assert_eq!(operators[1], (1, "filter"));
    let below: Vec<_> = explanation.steps.iter().filter(|s| s.depth == 2).collect();
    let bgp: Vec<_> = below.iter().filter(|s| s.operator != "filter").collect();
    assert_eq!(bgp.len(), 3, "{:#?}", explanation.steps);
    // memberOf d0 (20 rows) is the most selective start; every join step has an estimate.
    assert!(bgp[0].detail.contains("memberOf"), "{:#?}", bgp);
    assert!(bgp.iter().all(|s| s.estimated_rows.is_some()), "{:#?}", bgp);
    assert_eq!(bgp[0].rows, 20);
    // The filter runs right after the pattern that binds ?c.
    let takes = below.iter().position(|s| s.detail.contains("takes"));
    assert_eq!(
        below[takes.unwrap() + 1].operator,
        "filter",
        "{:#?}",
        explanation.steps
    );

    // A filter on a variable of the first pattern runs before the joins, so they read
    // fewer rows; a conjunct on a later variable waits for it.
    let text = format!(
        "SELECT ?x ?c ?d WHERE {{ ?x a <{EX}Student> . ?x <{EX}memberOf> ?d . ?x <{EX}takes> ?c
           FILTER(STRENDS(STR(?x), \"7\") && ?c != <{EX}c7>) }}"
    );
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let explanation = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
    let oracle = QueryOptions {
        ..QueryOptions::default()
    };
    assert_eq!(
        rows(
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
            false
        ),
        rows(reference(&snapshot, &query, &oracle).unwrap(), false)
    );
    let below: Vec<_> = explanation.steps.iter().filter(|s| s.depth == 2).collect();
    assert_eq!(
        (below[0].operator.as_str(), below[1].operator.as_str()),
        ("scan", "filter"),
        "{:#?}",
        explanation.steps
    );
    assert!(below[1].detail.contains("STRENDS") && below[1].rows < below[0].rows / 5);
    // Every step after it works on the filtered rows.
    assert!(
        below[2..].iter().all(|s| s.rows <= below[1].rows),
        "{:#?}",
        explanation.steps
    );
    assert_eq!(below.iter().filter(|s| s.operator == "filter").count(), 2);

    let construct = SparqlParser::new()
        .parse_query("CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }")
        .unwrap();
    let native = explain_query(&snapshot, &construct, &QueryOptions::default()).unwrap();
    assert_eq!((native.executor, native.rows), ("native", 1500));
    let describe = SparqlParser::new()
        .parse_query(&format!("DESCRIBE <{EX}x1>"))
        .unwrap();
    let native = explain_query(&snapshot, &describe, &QueryOptions::default()).unwrap();
    assert_eq!((native.executor, native.rows), ("native", 3));
    // BNODE with a label: the native executor too (there is no other).
    let labelled = SparqlParser::new()
        .parse_query("SELECT ?b WHERE { BIND(BNODE(\"x\") AS ?b) }")
        .unwrap();
    let native = explain_query(&snapshot, &labelled, &QueryOptions::default()).unwrap();
    assert_eq!((native.executor, native.rows), ("native", 1));
}

/// Filters and aggregates over tables large enough to run in parallel chunks (with terms
/// computed by BIND, regular expressions, strings, decimals) equal the reference evaluator.
#[test]
fn parallel_filters_and_aggregates_equal_the_reference() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..40_000u32 {
        let s = ex(&format!("s{i}"));
        let values: [Term; 3] = [
            Literal::new_typed_literal((i % 997).to_string(), xsd::INTEGER).into(),
            Literal::new_typed_literal(format!("{}.{}", i % 89, i % 7), xsd::DECIMAL).into(),
            if i % 3 == 0 {
                Literal::new_language_tagged_literal_unchecked(format!("name {}", i % 311), "en")
                    .into()
            } else {
                Literal::new_simple_literal(format!("label-{}-{}", i % 53, i % 5)).into()
            },
        ];
        for (p, value) in ["n", "d", "l"].iter().zip(values) {
            let quad = Quad::new(s.clone(), ex(p), value, GraphName::DefaultGraph);
            tx.insert(quad.as_ref());
        }
        let group = Quad::new(
            s,
            ex("g"),
            ex(&format!("g{}", i % 1500)),
            GraphName::DefaultGraph,
        );
        tx.insert(group.as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let oracle = QueryOptions {
        ..QueryOptions::default()
    };
    let queries = [
        format!("SELECT ?s ?l WHERE {{ ?s <{EX}l> ?l FILTER(REGEX(STR(?l), \"-1[0-9]-\")) }}"),
        format!("SELECT ?s WHERE {{ ?s <{EX}l> ?l FILTER(CONTAINS(UCASE(STR(?l)), \"NAME 3\")) }}"),
        format!(
            "SELECT ?s ?x WHERE {{ ?s <{EX}n> ?n . ?s <{EX}d> ?d BIND(CONCAT(STR(?n), \"/\", STR(?d)) AS ?x) FILTER(STRLEN(?x) > 7 && ?d > 40) }}"
        ),
        format!(
            "SELECT ?g (COUNT(*) AS ?c) (SUM(?n) AS ?sn) (AVG(?d) AS ?ad) (MIN(?l) AS ?ml) (MAX(?d) AS ?xd) (COUNT(DISTINCT ?l) AS ?cl) WHERE {{ ?s <{EX}g> ?g . ?s <{EX}n> ?n . ?s <{EX}d> ?d . ?s <{EX}l> ?l }} GROUP BY ?g"
        ),
        format!(
            "SELECT ?g (SUM(?n * 2) AS ?s2) (MAX(STRLEN(STR(?l))) AS ?ml) (SAMPLE(?n) AS ?one) WHERE {{ ?s <{EX}g> ?g . ?s <{EX}n> ?n . ?s <{EX}l> ?l }} GROUP BY ?g"
        ),
        format!(
            "SELECT ?k (COUNT(*) AS ?c) WHERE {{ ?s <{EX}n> ?n BIND(CONCAT(\"k\", STR(?n / 100)) AS ?k) }} GROUP BY ?k"
        ),
    ];
    for text in queries {
        let query = SparqlParser::new()
            .parse_query(&text)
            .unwrap_or_else(|e| panic!("{e}: {text}"));
        assert!(runs_natively(&query), "{text}");
        let native = rows(
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
            false,
        );
        let mut expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
        assert!(!expected.is_empty(), "{text}");
        let mut native = native;
        if text.contains("SAMPLE(?n) AS ?one") {
            // SAMPLE may pick any value of its group: the other columns must be equal,
            // and each sample must be an ?n of its group.
            let members = rows(
                reference(
                    &snapshot,
                    &SparqlParser::new()
                        .parse_query(&format!(
                            "SELECT ?g ?n WHERE {{ ?s <{EX}g> ?g . ?s <{EX}n> ?n }}"
                        ))
                        .unwrap(),
                    &oracle,
                )
                .unwrap(),
                false,
            );
            let members: HashSet<&str> = members.iter().map(String::as_str).collect();
            let split = |row: &str| -> (String, String) {
                let (rest, sample) = row.rsplit_once('\t').unwrap();
                (rest.to_owned(), sample.to_owned())
            };
            for row in &native {
                let (rest, sample) = split(row);
                let group = rest.split('\t').next().unwrap();
                assert!(
                    members.contains(format!("{group}\t{sample}").as_str()),
                    "{row}: {text}"
                );
            }
            native = native.iter().map(|r| split(r).0).collect();
            expected = expected.iter().map(|r| split(r).0).collect();
            native.sort();
            expected.sort();
        }
        if native != expected {
            let (a, b): (HashSet<_>, HashSet<_>) =
                (native.iter().collect(), expected.iter().collect());
            let only_native: Vec<_> = a.difference(&b).take(5).collect();
            let only_reference: Vec<_> = b.difference(&a).take(5).collect();
            panic!(
                "{text}
{} native vs {} reference rows
only native: {only_native:?}
only reference: {only_reference:?}",
                native.len(),
                expected.len()
            );
        }
    }
}

/// A graph result as sorted triple strings, every blank node written as `_:b` (they are
/// fresh per solution, so only their positions can be compared).
fn graph(results: QueryResults<'_>) -> Vec<String> {
    let QueryResults::Graph(triples) = results else {
        panic!("not a graph result");
    };
    let blank = |term: String| {
        if term.starts_with("_:") {
            "_:b".to_owned()
        } else {
            term
        }
    };
    let mut out: Vec<String> = triples
        .map(|t| {
            let t = t.unwrap();
            format!(
                "{} {} {}",
                blank(t.subject.to_string()),
                t.predicate,
                blank(t.object.to_string())
            )
        })
        .collect();
    out.sort();
    out
}

/// CONSTRUCT over the random generator's patterns (with a template of variables, bound or
/// not, constants, a blank node and a literal in subject position) equals the reference evaluator.
#[test]
fn construct_equals_the_reference() {
    use spargebra::term::{BlankNode, NamedNodePattern, TermPattern, TriplePattern};
    use spargebra::{Query, algebra::GraphPattern};

    let mut rng = Rng(20_260_929);
    let var = |name: &str| TermPattern::Variable(oxrdf::Variable::new_unchecked(name));
    let template = vec![
        TriplePattern {
            subject: var("a"),
            predicate: NamedNodePattern::NamedNode(ex("q")),
            object: var("b"),
        },
        TriplePattern {
            subject: TermPattern::BlankNode(BlankNode::new_unchecked("x")),
            predicate: NamedNodePattern::NamedNode(ex("r")),
            object: var("a"),
        },
        TriplePattern {
            subject: var("b"),
            predicate: NamedNodePattern::Variable(oxrdf::Variable::new_unchecked("c")),
            object: TermPattern::Literal(Literal::new_simple_literal("k")),
        },
        TriplePattern {
            subject: var("d"),
            predicate: NamedNodePattern::NamedNode(ex("s")),
            object: TermPattern::BlankNode(BlankNode::new_unchecked("x")),
        },
    ];
    let mut checked = 0;
    for _ in 0..60 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..30 {
            let (text, _) = random_query(&mut rng);
            // Which solutions a LIMIT without ORDER BY keeps is open, and the template
            // doesn't show them all: no single right graph to compare.
            if limited(&text).is_some() {
                continue;
            }
            let Query::Select { pattern, .. } = SparqlParser::new().parse_query(&text).unwrap()
            else {
                unreachable!()
            };
            let pattern: GraphPattern = pattern;
            let query = Query::Construct {
                template: template.clone(),
                dataset: None,
                pattern,
                base_iri: None,
            };
            if !runs_natively(&query) {
                continue;
            }
            let native =
                graph(evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap());
            let oracle = QueryOptions {
                ..QueryOptions::default()
            };
            let expected = graph(reference(&snapshot, &query, &oracle).unwrap());
            assert_eq!(native, expected, "{query}");
            checked += 1;
        }
    }
    assert!(checked > 1000, "{checked}");
}

/// GRAPH patterns (a graph variable or constant, mixed with default-graph patterns, the
/// graph variable shared across GRAPH blocks and grouped on) equal the reference evaluator.
#[test]
fn graph_patterns_equal_the_reference() {
    let mut rng = Rng(20_260_930);
    let (mut checked, mut fallbacks) = (0, 0);
    for _ in 0..80 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            // e1 is also a graph name, so ?g can join with entities.
            let graph: GraphName = match rng.below(5) {
                0 | 1 => GraphName::DefaultGraph,
                2 => ex("g0").into(),
                3 => ex("g1").into(),
                _ => ex("e1").into(),
            };
            let quad = Quad::new(quad.subject, quad.predicate, quad.object, graph);
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..40 {
            let inner = group_pattern(&mut rng, 0);
            let other = triple(&mut rng);
            let graph = rng
                .pick(&[
                    "?g",
                    "<http://example.com/g0>",
                    "<http://example.com/e1>",
                    "<http://example.com/g9>",
                ])
                .to_string();
            let text = match rng.below(5) {
                0 => format!("SELECT * WHERE {{ GRAPH {graph} {{ {inner} }} }}"),
                1 => format!("SELECT * WHERE {{ {other} GRAPH {graph} {{ {inner} }} }}"),
                2 => format!("SELECT * WHERE {{ GRAPH ?g {{ {inner} }} GRAPH ?g {{ {other} }} }}"),
                3 => format!(
                    "SELECT ?g (COUNT(*) AS ?n) WHERE {{ GRAPH ?g {{ {inner} }} }} GROUP BY ?g"
                ),
                _ => format!(
                    "SELECT * WHERE {{ GRAPH {graph} {{ {inner} OPTIONAL {{ {other} }} }} }}"
                ),
            };
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            if !runs_natively(&query) {
                fallbacks += 1;
                continue;
            }
            let native = rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false,
            );
            let oracle = QueryOptions {
                ..QueryOptions::default()
            };
            let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
            assert_eq!(native, expected, "{text}");
            checked += 1;
        }
    }
    assert!(
        checked > 1500 && fallbacks * 4 < checked,
        "{checked} checked, {fallbacks} fallbacks"
    );
}

/// SUBSTR, REPLACE, ENCODE_FOR_URI, IRI, the XSD casts and GROUP_CONCAT over random data
/// (dates with timezones, non-canonical integers, language strings) equal the reference evaluator.
/// GROUP_CONCAT joins in row order, which SPARQL leaves open: its parts are compared sorted.
#[test]
fn string_functions_casts_and_group_concat_equal_the_reference() {
    let expressions = [
        "SUBSTR(STR(?b), 2)",
        "SUBSTR(STR(?b), 2, 3)",
        "SUBSTR(?b, 1, 2)",
        "SUBSTR(STR(?b), 0, 2)",
        "REPLACE(STR(?b), \"[0-9]\", \"#\")",
        "REPLACE(?b, \"(s)([0-9])\", \"$2$1\")",
        "REPLACE(STR(?b), \"S\", \"x\", \"i\")",
        "REPLACE(STR(?b), \"(\", \"x\")",
        "ENCODE_FOR_URI(STR(?a))",
        "ENCODE_FOR_URI(?b)",
        "IRI(CONCAT(\"http://example.com/n/\", ENCODE_FOR_URI(STR(?b))))",
        "IRI(\"relative\")",
        "IRI(?a)",
        "<http://www.w3.org/2001/XMLSchema#integer>(?b)",
        "<http://www.w3.org/2001/XMLSchema#decimal>(?b)",
        "<http://www.w3.org/2001/XMLSchema#double>(?b)",
        "<http://www.w3.org/2001/XMLSchema#float>(?b)",
        "<http://www.w3.org/2001/XMLSchema#boolean>(?b)",
        "<http://www.w3.org/2001/XMLSchema#string>(?b)",
        "<http://www.w3.org/2001/XMLSchema#string>(?a)",
        "<http://www.w3.org/2001/XMLSchema#date>(?b)",
        "<http://www.w3.org/2001/XMLSchema#dateTime>(?b)",
        "<http://www.w3.org/2001/XMLSchema#dateTime>(\"2020-01-01T10:00:00Z\")",
        "<http://www.w3.org/2001/XMLSchema#integer>(STR(?b))",
    ];
    let mut rng = Rng(20_260_931);
    let oracle = QueryOptions {
        ..QueryOptions::default()
    };
    // GROUP_CONCAT values in a canonical order: the parts of each literal sorted, and no
    // language tag (the result is a simple literal, SPARQL 1.1 §18.5.1.7; spareval keeps a
    // tag all values share).
    let sorted_parts = |row: &str| -> String {
        row.split('\t')
            .map(
                |term| match term.strip_prefix('"').and_then(|t| t.split_once('"')) {
                    Some((value, rest)) => {
                        let mut parts: Vec<&str> = value.split(',').collect();
                        parts.sort_unstable();
                        let rest = if rest.starts_with('@') { "" } else { rest };
                        format!("\"{}\"{rest}", parts.join(","))
                    }
                    None => term.to_owned(),
                },
            )
            .collect::<Vec<_>>()
            .join("\t")
    };
    let mut checked = 0;
    for _ in 0..40 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for expression in expressions {
            for text in [
                format!("SELECT ?a ?b ({expression} AS ?x) WHERE {{ ?a ?p ?b }}"),
                format!(
                    "SELECT ?a ?b (COALESCE({expression}, \"none\") AS ?x) WHERE {{ ?a ?p ?b FILTER(isLiteral({expression}) || isIRI({expression}) || ?p = <{EX}p0>) }}"
                ),
            ] {
                let query = SparqlParser::new()
                    .parse_query(&text)
                    .unwrap_or_else(|e| panic!("{e}: {text}"));
                assert!(runs_natively(&query), "{text}");
                let native = rows(
                    evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                    false,
                );
                let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
                assert_eq!(native, expected, "{text}");
                checked += 1;
            }
        }
        for concat in [
            "GROUP_CONCAT(?b; separator=\",\")",
            "GROUP_CONCAT(DISTINCT ?b; separator=\",\")",
            "GROUP_CONCAT(STR(?b); separator=\",\")",
            "GROUP_CONCAT(DISTINCT STR(?p); separator=\",\")",
        ] {
            let text = format!("SELECT ?a ({concat} AS ?x) WHERE {{ ?a ?p ?b }} GROUP BY ?a");
            let query = SparqlParser::new().parse_query(&text).unwrap();
            assert!(runs_natively(&query), "{text}");
            let normalise = |rows: Vec<String>| {
                let mut rows: Vec<String> = rows.iter().map(|r| sorted_parts(r)).collect();
                rows.sort();
                rows
            };
            let native = normalise(rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false,
            ));
            let expected = normalise(rows(reference(&snapshot, &query, &oracle).unwrap(), false));
            assert_eq!(native, expected, "{text}");
            checked += 1;
        }
    }
    assert_eq!(checked, 40 * (24 * 2 + 4));
}

/// Every quad of `engine`, blank nodes written as `_:b`, sorted.
fn contents(engine: &Engine) -> Vec<String> {
    let snapshot = engine.snapshot();
    let blank = |t: String| {
        if t.starts_with("_:") {
            "_:b".to_owned()
        } else {
            t
        }
    };
    let mut out: Vec<String> = snapshot
        .quads_for_pattern(&nrese_engine::QuadPattern::all())
        .map(|q| {
            let q = snapshot.decode_quad(q).unwrap();
            format!(
                "{} {} {} {}",
                blank(q.subject.to_string()),
                q.predicate,
                blank(q.object.to_string()),
                blank(q.graph_name.to_string())
            )
        })
        .collect();
    out.sort();
    out
}

/// DELETE/INSERT … WHERE with the WHERE evaluated natively (over random patterns, with
/// blank nodes, graph variables and named graphs in the templates) changes the store
/// exactly as with the WHERE on the reference evaluator.
#[test]
fn native_updates_equal_the_reference() {
    use nrese_sparql::UpdateOptions;
    let mut rng = Rng(20_260_932);
    let templates = [
        (
            "DELETE { ?a ?p ?b }",
            "INSERT { ?b <http://example.com/q> ?a }",
        ),
        (
            "DELETE { ?a <http://example.com/p1> ?b }",
            "INSERT { _:x <http://example.com/r> ?a . _:x <http://example.com/r> ?b }",
        ),
        ("", "INSERT { GRAPH <http://example.com/g> { ?a ?p ?b } }"),
        (
            "DELETE { ?b ?p ?a }",
            "INSERT { GRAPH ?a { ?a <http://example.com/s> ?c } }",
        ),
        ("DELETE { ?a ?p ?b }", ""),
    ];
    let mut checked = 0;
    for _ in 0..60 {
        let data = random_dataset(&mut rng);
        let engines = [
            Engine::new(EngineConfig::default()).unwrap(),
            Engine::new(EngineConfig::default()).unwrap(),
        ];
        for engine in &engines {
            let mut tx = engine.transaction();
            for quad in &data {
                tx.insert(quad.as_ref());
            }
            tx.commit().unwrap();
        }
        for _ in 0..10 {
            let pattern = group_pattern(&mut rng, 0);
            let (delete, insert) = *rng.pick(&templates);
            let text = format!("{delete} {insert} WHERE {{ ?a ?p ?b . {pattern} }}");
            let update = SparqlParser::new()
                .parse_update(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            for (engine, reference) in engines.iter().zip([false, true]) {
                let options = UpdateOptions {
                    ..UpdateOptions::default()
                };
                update_engine(engine, &update, &options, reference);
            }
            let (native, oracle) = (contents(&engines[0]), contents(&engines[1]));
            if native != oracle {
                let only = |a: &[String], b: &[String]| -> Vec<String> {
                    a.iter().filter(|x| !b.contains(x)).cloned().collect()
                };
                panic!(
                    "{text}
only native: {:?}
only the reference: {:?}",
                    only(&native, &oracle),
                    only(&oracle, &native)
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 600);
}

/// A quad one solution deletes and another inserts is present afterwards: every deletion
/// comes before any insertion (SPARQL 1.1 Update 3.1.3), on both paths.
#[test]
fn deletions_precede_insertions() {
    use nrese_sparql::UpdateOptions;
    let text = format!(
        "DELETE {{ ?d <{EX}q> <{EX}y> }} INSERT {{ ?i <{EX}q> <{EX}y> }} WHERE {{ {{ BIND(<{EX}x> AS ?i) }} UNION {{ BIND(<{EX}x> AS ?d) }} }}"
    );
    let update = SparqlParser::new().parse_update(&text).unwrap();
    for reference in [false, true] {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let quad = Quad::new(ex("x"), ex("q"), ex("y"), GraphName::DefaultGraph);
        let mut tx = engine.transaction();
        tx.insert(quad.as_ref());
        tx.commit().unwrap();
        update_engine(&engine, &update, &UpdateOptions::default(), reference);
        assert_eq!(contents(&engine).len(), 1, "reference: {reference}");
    }
}

/// SPARQL JSON, TSV and CSV written straight from the id table are byte for byte what the
/// results serialiser writes from the decoded solutions, over random queries and data with
/// every term kind, number forms and every character the formats escape.
#[test]
fn direct_results_equal_the_results_serialiser() {
    use nrese_sparql::{ResultsFormat, write_results};
    use sparesults::{QueryResultsFormat, QueryResultsSerializer};

    let serialised = |results: QueryResults<'_>, format: QueryResultsFormat| -> Vec<u8> {
        let serializer = QueryResultsSerializer::from_format(format);
        let mut out = Vec::new();
        match results {
            QueryResults::Solutions(solutions) => {
                let mut writer = serializer
                    .serialize_solutions_to_writer(&mut out, solutions.variables().to_vec())
                    .unwrap();
                for solution in solutions {
                    writer.serialize(&solution.unwrap()).unwrap();
                }
                writer.finish().unwrap();
            }
            QueryResults::Boolean(value) => {
                serializer
                    .serialize_boolean_to_writer(&mut out, value)
                    .unwrap();
            }
            QueryResults::Graph(_) => unreachable!(),
        }
        out
    };
    let tricky: Vec<Term> = vec![
        Literal::new_simple_literal("quote \" backslash \\ newline \n tab \t").into(),
        Literal::new_simple_literal("control \u{1} \u{1f} \u{8} \u{c} \r and unicode é ü 漢字 😀")
            .into(),
        Literal::new_language_tagged_literal_unchecked("hello", "en-GB").into(),
        Literal::new_typed_literal("custom", NamedNode::new_unchecked("http://example.com/dt"))
            .into(),
        Literal::new_typed_literal("typed string", xsd::STRING).into(),
        oxrdf::BlankNode::new_unchecked("b1").into(),
        NamedNode::new_unchecked("http://example.com/a\"b").into(),
        Literal::new_simple_literal("comma, and \"quotes\"").into(),
        Literal::new_typed_literal("+007", xsd::INTEGER).into(),
        Literal::new_typed_literal("1.", xsd::DECIMAL).into(),
        Literal::new_typed_literal(".5e3", xsd::DOUBLE).into(),
        Literal::new_typed_literal("NaN", xsd::DOUBLE).into(),
        Literal::new_typed_literal("1", xsd::BOOLEAN).into(),
    ];
    let mut rng = Rng(20_260_933);
    let mut checked = 0;
    for _ in 0..60 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in random_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        for (i, object) in tricky.iter().enumerate() {
            let quad = Quad::new(
                ex(&format!("e{}", i % 6)),
                ex("p0"),
                object.clone(),
                GraphName::DefaultGraph,
            );
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..25 {
            let (text, _) = random_query(&mut rng);
            let text = match rng.below(4) {
                // A computed value (not in the dictionary) with characters JSON escapes.
                0 if !text.starts_with("SELECT *") => {
                    text.replacen("SELECT", "SELECT (CONCAT(\"\\\"x\\n\", STR(?a)) AS ?z)", 1)
                }
                1 => format!("ASK {{ {} }}", triple(&mut rng)),
                _ => text,
            };
            let Ok(query) = SparqlParser::new().parse_query(&text) else {
                continue;
            };
            for (format, reference) in [
                (ResultsFormat::Json, QueryResultsFormat::Json),
                (ResultsFormat::Tsv, QueryResultsFormat::Tsv),
                (ResultsFormat::Csv, QueryResultsFormat::Csv),
            ] {
                let mut direct = Vec::new();
                let Some(written) = write_results(
                    &snapshot,
                    &query,
                    &QueryOptions::default(),
                    format,
                    &mut direct,
                ) else {
                    continue;
                };
                written.unwrap();
                let expected = serialised(
                    evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                    reference,
                );
                assert_eq!(
                    String::from_utf8(direct).unwrap(),
                    String::from_utf8(expected).unwrap(),
                    "{format:?}: {text}"
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 3000, "{checked}");
}

/// A random dataset for the merged default graph: statements in the default graph and in
/// three named graphs, a third of them in a second graph as well, and some default-graph
/// statements inferred.
fn load_spread_over_graphs(engine: &Engine, rng: &mut Rng) {
    let graphs = |rng: &mut Rng| -> GraphName {
        match rng.below(4) {
            0 => GraphName::DefaultGraph,
            1 => ex("g0").into(),
            2 => ex("g1").into(),
            _ => ex("e1").into(),
        }
    };
    let mut tx = engine.transaction();
    for quad in random_dataset(rng) {
        let first = graphs(rng);
        if first.is_default_graph() && rng.below(3) == 0 {
            let triple = EncodedTriple::new(
                tx.intern(quad.subject.as_ref().into()),
                tx.intern(quad.predicate.as_ref().into()),
                tx.intern(quad.object.as_ref()),
            );
            tx.insert_inferred(triple);
        } else {
            let copy = Quad::new(
                quad.subject.clone(),
                quad.predicate.clone(),
                quad.object.clone(),
                first,
            );
            tx.insert(copy.as_ref());
        }
        if rng.below(3) == 0 {
            let second = graphs(rng);
            let copy = Quad::new(quad.subject, quad.predicate, quad.object, second);
            tx.insert(copy.as_ref());
        }
    }
    tx.commit().unwrap();
}

/// The merged default graph (`union_default_graph`): the native executor equals the reference evaluator
/// over the merging dataset adapter, on the random queries of the main test and on
/// queries that mix `GRAPH` blocks with merged patterns, under every read model. A
/// statement held by several graphs counts once in both.
#[test]
fn the_merged_default_graph_equals_the_reference() {
    let mut rng = Rng(20_260_933);
    let (mut checked, mut fallbacks, mut native_runs, mut native_runs_plain) = (0, 0, 0, 0);
    for dataset_case in 0..120 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        load_spread_over_graphs(&engine, &mut rng);
        let snapshot = engine.snapshot();
        for query_case in 0..40 {
            let model = *rng.pick(&[
                ReadModel::Materialised,
                ReadModel::Asserted,
                ReadModel::Inferred,
            ]);
            let native_options = QueryOptions {
                read_model: model,
                union_default_graph: true,
                ..QueryOptions::default()
            };
            let oracle = QueryOptions {
                ..native_options.clone()
            };
            let (text, ordered) = if rng.below(4) == 0 {
                let inner = group_pattern(&mut rng, 0);
                let other = triple(&mut rng);
                let graph =
                    *rng.pick(&["?g", "<http://example.com/g0>", "<http://example.com/e1>"]);
                let text = match rng.below(3) {
                    0 => format!("SELECT * WHERE {{ {other} GRAPH {graph} {{ {inner} }} }}"),
                    1 => format!(
                        "SELECT * WHERE {{ {inner} OPTIONAL {{ GRAPH ?g {{ {other} }} }} }}"
                    ),
                    _ => format!(
                        "SELECT ?g (COUNT(*) AS ?n) WHERE {{ {other} GRAPH ?g {{ {inner} }} }} GROUP BY ?g"
                    ),
                };
                (text, false)
            } else {
                random_query(&mut rng)
            };
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            if !runs_natively(&query) {
                fallbacks += 1;
                continue;
            }
            let native_in = |options: &QueryOptions| {
                explain_query(&snapshot, &query, options).unwrap().executor == "native"
            };
            native_runs += usize::from(native_in(&native_options));
            native_runs_plain += usize::from(native_in(&QueryOptions {
                read_model: model,
                ..QueryOptions::default()
            }));
            let native = rows(
                evaluate_query(&snapshot, &query, &native_options).unwrap(),
                ordered,
            );
            let context = format!("dataset {dataset_case}, query {query_case}, {model:?}: {text}");
            if let Some(limit) = limited(&text) {
                let unlimited = text[..text.rfind(" LIMIT ").unwrap()].to_owned();
                let query = SparqlParser::new().parse_query(&unlimited).unwrap();
                let mut all = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
                assert_eq!(native.len(), all.len().min(limit), "{context}");
                for row in &native {
                    let position = all.iter().position(|r| r == row);
                    assert!(position.is_some(), "{row} is not a solution: {context}");
                    all.remove(position.unwrap());
                }
            } else {
                let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), ordered);
                assert_eq!(native, expected, "{context}");
            }
            checked += 1;
        }
    }
    // The queries must really have run on the native executor (a silent fallback would
    // compare spareval with itself), and about as often as with the plain default graph:
    // what hands a query back at run time (unbound join keys) isn't the merge.
    assert!(
        checked > 4000
            && fallbacks * 20 < checked
            && native_runs * 4 > checked * 3
            && native_runs * 20 > native_runs_plain * 19,
        "{checked} checked, {fallbacks} not native by shape, {native_runs} ran natively, \
         {native_runs_plain} with the plain default graph"
    );
}

/// The shortcuts that read counts or probe the index, in the merged default graph, over
/// data where most statements are held by two or three graphs: `COUNT(*)` of one pattern,
/// `GROUP BY` with row counts, a streamed `LIMIT`, and an index nested-loop join. Each
/// statement must count once (the index itself counts one per graph).
#[test]
fn merged_default_graph_shortcuts_count_statements_once() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rdf_type = NamedNode::new_unchecked("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let graphs: [GraphName; 3] = [GraphName::DefaultGraph, ex("g0").into(), ex("g1").into()];
    let mut statements = 0usize;
    let mut insert = |subject: NamedNode, predicate: NamedNode, object: NamedNode, i: usize| {
        statements += 1;
        // In one, two or all three graphs.
        for (n, graph) in graphs.iter().enumerate() {
            if n == i % 3 || i.is_multiple_of(2) || (n == 0 && i.is_multiple_of(5)) {
                let quad = Quad::new(
                    subject.clone(),
                    predicate.clone(),
                    object.clone(),
                    graph.clone(),
                );
                tx.insert(quad.as_ref());
            }
        }
    };
    for i in 0..3_000 {
        insert(ex(&format!("t{i}")), rdf_type.clone(), ex("T"), i);
        for j in 0..(i % 4) {
            insert(
                ex(&format!("t{i}")),
                ex("p"),
                ex(&format!("o{}", (i * 7 + j) % 900)),
                i + j,
            );
        }
    }
    for i in 0..60_000 {
        insert(
            ex(&format!("u{}", i / 4)),
            ex("p"),
            ex(&format!("o{}", i % 900)),
            i,
        );
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let native_options = QueryOptions {
        union_default_graph: true,
        ..QueryOptions::default()
    };
    let oracle = QueryOptions {
        ..native_options.clone()
    };
    // More quads than statements: the copies are there.
    assert!(snapshot.len() as usize > statements * 3 / 2);

    let run = |text: &str, options: &QueryOptions| {
        let query = SparqlParser::new().parse_query(text).unwrap();
        assert!(runs_natively(&query), "{text}");
        rows(evaluate_query(&snapshot, &query, options).unwrap(), false)
    };
    let run_reference = |text: &str| {
        let query = SparqlParser::new().parse_query(text).unwrap();
        rows(reference(&snapshot, &query, &oracle).unwrap(), false)
    };
    let integer = |n: usize| format!("\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>");
    assert_eq!(
        run(
            "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
            &native_options
        ),
        [integer(statements)]
    );
    for text in [
        format!("SELECT (COUNT(*) AS ?n) WHERE {{ ?s a <{EX}T> }}"),
        format!("SELECT (COUNT(*) AS ?n) WHERE {{ ?s <{EX}p> ?o }}"),
        "SELECT ?p (COUNT(*) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p".to_owned(),
        format!("SELECT ?o (COUNT(*) AS ?n) WHERE {{ ?s <{EX}p> ?o }} GROUP BY ?o"),
        format!("SELECT ?x ?y WHERE {{ ?x a <{EX}T> . ?x <{EX}p> ?y }}"),
        format!("SELECT ?x ?y WHERE {{ ?x a <{EX}T> OPTIONAL {{ ?x <{EX}p> ?y }} }}"),
        format!("SELECT ?x ?z WHERE {{ <{EX}t7> <{EX}p> ?y . ?x <{EX}p> ?y . ?x <{EX}p> ?z }}"),
    ] {
        let native = run(&text, &native_options);
        assert!(!native.is_empty(), "{text}");
        assert_eq!(native, run_reference(&text), "{text}");
        let explained = explain_query(
            &snapshot,
            &SparqlParser::new().parse_query(&text).unwrap(),
            &native_options,
        )
        .unwrap();
        assert_eq!(explained.executor, "native", "{text}");
    }
    // The join above must have probed the index: that is the path under test.
    let probes = explain_query(
        &snapshot,
        &SparqlParser::new()
            .parse_query(&format!(
                "SELECT ?x ?z WHERE {{ <{EX}t7> <{EX}p> ?y . ?x <{EX}p> ?y . ?x <{EX}p> ?z }}"
            ))
            .unwrap(),
        &native_options,
    )
    .unwrap();
    assert!(
        probes
            .steps
            .iter()
            .any(|step| step.operator == "index join"),
        "{:?}",
        probes
            .steps
            .iter()
            .map(|s| s.operator.clone())
            .collect::<Vec<_>>()
    );

    // A streamed LIMIT: that many rows, each a distinct solution of the unlimited query.
    let limited = format!("SELECT * WHERE {{ <{EX}t7> ?p ?o FILTER(isIRI(?o)) }} LIMIT 3");
    let all = run_reference(&format!(
        "SELECT * WHERE {{ <{EX}t7> ?p ?o FILTER(isIRI(?o)) }}"
    ));
    let native = run(&limited, &native_options);
    assert_eq!(native.len(), all.len().min(3));
    let distinct: HashSet<&String> = native.iter().collect();
    assert_eq!(
        distinct.len(),
        native.len(),
        "a statement came out twice: {native:?}"
    );
    assert!(native.iter().all(|row| all.contains(row)));
}

/// Updates whose `WHERE` reads the merged default graph change the store the same way on
/// both executors. What an update writes or deletes without `GRAPH` is the default graph.
#[test]
fn updates_over_the_merged_default_graph_equal_the_reference() {
    use nrese_sparql::UpdateOptions;
    let mut rng = Rng(20_260_934);
    let templates = [
        (
            "DELETE { ?a ?p ?b }",
            "INSERT { ?b <http://example.com/q> ?a }",
        ),
        ("", "INSERT { GRAPH <http://example.com/g> { ?a ?p ?b } }"),
        (
            "DELETE { GRAPH <http://example.com/g0> { ?a ?p ?b } }",
            "INSERT { ?a <http://example.com/moved> ?b }",
        ),
        ("DELETE { ?a ?p ?b }", ""),
    ];
    let mut changed = 0;
    for _ in 0..60 {
        let engines = [
            Engine::new(EngineConfig::default()).unwrap(),
            Engine::new(EngineConfig::default()).unwrap(),
        ];
        let seed = rng.0;
        for engine in &engines {
            rng.0 = seed;
            load_spread_over_graphs(engine, &mut rng);
        }
        for _ in 0..8 {
            // Half simple (so that many updates change something), half arbitrary.
            let pattern = if rng.below(2) == 0 {
                triple(&mut rng)
            } else {
                group_pattern(&mut rng, 0)
            };
            let (delete, insert) = *rng.pick(&templates);
            let text = format!("{delete} {insert} WHERE {{ ?a ?p ?b . {pattern} }}");
            let update = SparqlParser::new()
                .parse_update(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let before = contents(&engines[0]);
            for (engine, reference) in engines.iter().zip([false, true]) {
                let options = UpdateOptions {
                    union_default_graph: true,
                    ..UpdateOptions::default()
                };
                update_engine(engine, &update, &options, reference);
            }
            let (native, oracle) = (contents(&engines[0]), contents(&engines[1]));
            assert_eq!(native, oracle, "{text}");
            changed += usize::from(native != before);
        }
    }
    assert!(changed > 100, "only {changed} updates changed anything");
}

/// A `WHERE` body for the dataset tests: what the default graph of the dataset holds,
/// `GRAPH` with a name or a variable over every kind of pattern (triple patterns, paths,
/// subqueries, `VALUES` and `BIND` without a triple pattern, `MINUS`, (NOT) EXISTS, nested
/// `GRAPH`, the graph variable as a subject or object), and joins of the two. Patterns
/// are connected, so that they have solutions.
///
/// Where spareval departs from SPARQL 1.1 §18.6, the generator stays out and
/// [`a_dataset_is_what_the_query_names`] checks the native executor by example: a pattern
/// that gives rows without reading a statement is only put under `GRAPH ?g`, and isn't
/// `VALUES` (under the name of a graph outside the dataset spareval gives its rows, where
/// the standard gives none; under `GRAPH ?g` it leaves `?g` unbound in the rows of
/// `VALUES`), and a subquery or a `MINUS` without a variable in common is only put under
/// a graph's name (under `GRAPH ?g` spareval evaluates the subquery once over all graphs,
/// with `?g` unbound, and takes `?g` for a variable the two sides of `MINUS` share).
fn dataset_body(rng: &mut Rng) -> String {
    let p = |rng: &mut Rng| format!("<{EX}p{}>", rng.below(4));
    let graph = |rng: &mut Rng| {
        rng.pick(&[
            "?g",
            "?g",
            "<http://example.com/g0>",
            "<http://example.com/g1>",
            "<http://example.com/e1>",
            "<http://example.com/g9>",
        ])
        .to_string()
    };
    let path = |rng: &mut Rng| {
        rng.pick(&[
            "<P0>+",
            "(<P0>|<P1>)+",
            "<P1>/<P2>",
            "^<P2>/<P0>",
            "!(<P0>|<P1>)",
            "<P0>|<P3>",
        ])
        .replace("<P", &format!("<{EX}p"))
    };
    match rng.below(16) {
        0 => group_pattern(rng, 0),
        1 => format!(
            "{} GRAPH {} {{ {} }}",
            triple(rng),
            graph(rng),
            group_pattern(rng, 0)
        ),
        // Paths in a named graph, from a free and from a bound start.
        2 => format!("GRAPH {} {{ ?a {} ?c }}", graph(rng), path(rng)),
        3 => format!(
            "GRAPH {} {{ ?a {} ?b . ?b {} ?c }}",
            graph(rng),
            p(rng),
            path(rng)
        ),
        4 => format!(
            "?a {} ?b . GRAPH {} {{ ?b {} ?c }}",
            p(rng),
            graph(rng),
            path(rng)
        ),
        // Zero-length paths from a constant: a term is a node of a graph that names it.
        5 => format!(
            "GRAPH ?g {{ <{EX}e{}> ({}|{})* ?c }}",
            rng.below(7),
            p(rng),
            p(rng)
        ),
        // A subquery.
        6 => format!(
            "GRAPH {} {{ {{ SELECT ?a (COUNT(?b) AS ?n) (MAX(?b) AS ?m) WHERE {{ ?a {} ?b }} GROUP BY ?a }} }}",
            rng.pick(&[
                "<http://example.com/g0>",
                "<http://example.com/e1>",
                "<http://example.com/g9>"
            ]),
            p(rng)
        ),
        // MINUS: no variable in common removes nothing.
        7 => {
            let graph = graph(rng);
            let shared: &[&str] = if graph == "?g" {
                &["?a", "?b"]
            } else {
                &["?a", "?b", "?c"]
            };
            format!(
                "GRAPH {graph} {{ ?a {} ?b MINUS {{ {} {} ?d }} }}",
                p(rng),
                rng.pick(shared),
                p(rng)
            )
        }
        // Rows that no triple pattern gives.
        8 | 9 => format!(
            "GRAPH ?g {{ {} }}",
            *rng.pick(&["", "BIND(1 AS ?x)", "{ BIND(1 AS ?x) } { BIND(2 AS ?y) }"])
        ),
        10 => format!(
            "GRAPH {} {{ ?a {} ?b GRAPH ?h {{ ?b {} ?c }} }}",
            graph(rng),
            p(rng),
            p(rng)
        ),
        // The default graph of the dataset, read by paths.
        11 => format!("?a {} ?c . OPTIONAL {{ ?c {} ?d }}", path(rng), p(rng)),
        12 => format!(
            "GRAPH {} {{ ?a {} ?b FILTER {}EXISTS {{ ?b {} ?c }} }}",
            graph(rng),
            p(rng),
            if rng.below(2) == 0 { "NOT " } else { "" },
            p(rng)
        ),
        // The graph's name among its own terms (e1 names a graph and an entity).
        13 => {
            let path = path(rng);
            format!(
                "GRAPH ?g {{ {} }}",
                rng.pick(&[
                    "?g ?p ?b".to_owned(),
                    "?a ?p ?g".to_owned(),
                    "?a ?p ?b BIND(?g AS ?x)".to_owned(),
                    format!("?g {path} ?b"),
                    format!("?a {path} ?g"),
                ])
            )
        }
        14 => format!(
            "GRAPH ?g {{ ?a {} ?b }} GRAPH ?h {{ ?a {} ?c }} FILTER(?g != ?h)",
            p(rng),
            p(rng)
        ),
        _ => format!(
            "{{ ?a {} ?b }} UNION {{ GRAPH {} {{ ?a {} ?b OPTIONAL {{ ?b {} ?c }} }} }}",
            p(rng),
            graph(rng),
            p(rng),
            path(rng)
        ),
    }
}

/// `FROM` and `FROM NAMED` clauses: none, one graph, several (their merge), a graph the
/// store doesn't hold, named graphs only, and both kinds.
fn dataset_clause(rng: &mut Rng) -> String {
    let from: &[&str] = match rng.below(7) {
        0 | 1 => &[],
        2 => &["g0"],
        3 => &["g0", "g1"],
        4 => &["g0", "g1", "e1"],
        5 => &["g9"],
        _ => &["g1", "g9"],
    };
    let named: &[&str] = match rng.below(5) {
        0 | 1 => &[],
        2 => &["g0"],
        3 => &["g1", "e1"],
        _ => &["g0", "g1", "e1", "g9"],
    };
    from.iter()
        .map(|g| format!("FROM <{EX}{g}> "))
        .chain(named.iter().map(|g| format!("FROM NAMED <{EX}{g}> ")))
        .collect()
}

/// The protocol's dataset (`default-graph-uri`, `named-graph-uri`), which replaces the
/// query's: the store's default graph may be one of its default graphs.
fn protocol_dataset(rng: &mut Rng) -> Option<nrese_sparql::QueryDatasetSpecification> {
    let default: Vec<GraphName> = match rng.below(7) {
        0..=2 => return None,
        3 => vec![ex("g0").into()],
        4 => vec![GraphName::DefaultGraph, ex("g1").into()],
        5 => vec![GraphName::DefaultGraph],
        _ => vec![ex("g0").into(), ex("e1").into(), ex("g9").into()],
    };
    let only_the_store_default = default == [GraphName::DefaultGraph];
    let mut dataset = nrese_sparql::QueryDatasetSpecification::new();
    dataset.set_default_graph(default);
    if only_the_store_default || rng.below(2) == 0 {
        dataset.set_available_named_graphs(vec![ex("g1").into(), ex("e1").into()]);
    }
    Some(dataset)
}

/// Datasets (`FROM`, `FROM NAMED`, the protocol's parameters) and `GRAPH` over any
/// pattern run on the native executor and equal the reference evaluator, under every read model and
/// with the store's default graph plain or merged.
#[test]
fn datasets_and_graph_patterns_equal_the_reference() {
    let mut rng = Rng(20_260_935);
    let (mut checked, mut with_solutions, mut with_dataset, mut fallbacks) = (0, 0, 0, 0);
    for dataset_case in 0..100 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        load_spread_over_graphs(&engine, &mut rng);
        let snapshot = engine.snapshot();
        for query_case in 0..40 {
            let clause = dataset_clause(&mut rng);
            let body = dataset_body(&mut rng);
            let text = match rng.below(4) {
                0 => format!("SELECT DISTINCT * {clause}WHERE {{ {body} }}"),
                1 => format!("ASK {clause}WHERE {{ {body} }}"),
                _ => format!("SELECT * {clause}WHERE {{ {body} }}"),
            };
            let options = QueryOptions {
                read_model: *rng.pick(&[
                    ReadModel::Materialised,
                    ReadModel::Asserted,
                    ReadModel::Inferred,
                ]),
                union_default_graph: rng.below(3) == 0,
                dataset: protocol_dataset(&mut rng),
                ..QueryOptions::default()
            };
            let context = format!(
                "dataset {dataset_case}, query {query_case}, {:?}, merged {}, protocol {:?}: {text}",
                options.read_model, options.union_default_graph, options.dataset
            );
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            // The random patterns of the main test hold operators that don't run natively
            // yet (EXISTS in an OPTIONAL's condition).
            if !runs_natively(&query) {
                fallbacks += 1;
                continue;
            }
            // A silent fallback would hide a query the executor can't take.
            assert_eq!(
                explain_query(&snapshot, &query, &options).unwrap().executor,
                "native",
                "{context}"
            );
            let native = rows(evaluate_query(&snapshot, &query, &options).unwrap(), false);
            let oracle = QueryOptions { ..options.clone() };
            let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
            assert_same_rows(&native, &expected, &context);
            checked += 1;
            with_solutions += usize::from(!native.is_empty() && native != ["false"]);
            with_dataset += usize::from(!clause.is_empty() || options.dataset.is_some());
        }
    }
    assert!(
        fallbacks * 50 < checked && with_solutions * 3 > checked && with_dataset * 2 > checked,
        "{checked} checked, {fallbacks} not native by shape, {with_solutions} with solutions, \
         {with_dataset} with a dataset"
    );
}

/// What a dataset is (SPARQL 1.1 §13.2), by example and not by comparison: `FROM` alone
/// leaves no named graphs, `FROM NAMED` alone leaves an empty default graph, several
/// `FROM` merge (a statement two of them hold counts once), the protocol's dataset
/// replaces the query's, and `GRAPH` naming a graph outside the dataset has no solutions
/// (§18.6), even for a pattern that reads no statement.
#[test]
fn a_dataset_is_what_the_query_names() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let quad = |s: &str, o: &str, g: GraphName| Quad::new(ex(s), ex("p"), ex(o), g);
    for q in [
        quad("d", "d1", GraphName::DefaultGraph),
        quad("a", "b", ex("g0").into()),
        quad("b", "c", ex("g0").into()),
        quad("a", "b", ex("g1").into()),
        quad("c", "d", ex("g1").into()),
        quad("x", "y", ex("g2").into()),
    ] {
        tx.insert(q.as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let count = |text: &str, options: &QueryOptions| -> usize {
        let query = SparqlParser::new().parse_query(text).unwrap();
        assert_eq!(
            explain_query(&snapshot, &query, options).unwrap().executor,
            "native",
            "{text}"
        );
        let native = rows(evaluate_query(&snapshot, &query, options).unwrap(), false);
        let oracle = QueryOptions { ..options.clone() };
        let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
        assert_eq!(native, expected, "{text}");
        native.len()
    };
    let plain = QueryOptions::default();
    let g = |name: &str| format!("<{EX}{name}>");
    let (g0, g1, g2, g9) = (g("g0"), g("g1"), g("g2"), g("g9"));
    for (text, solutions) in [
        ("SELECT * WHERE { ?s ?p ?o }".to_owned(), 1),
        (format!("SELECT * FROM {g0} WHERE {{ ?s ?p ?o }}"), 2),
        // a-b is in both graphs: once in their merge.
        (
            format!("SELECT * FROM {g0} FROM {g1} WHERE {{ ?s ?p ?o }}"),
            3,
        ),
        (
            format!("SELECT * FROM {g0} FROM {g1} WHERE {{ ?s <{EX}p>+ ?o }}"),
            6,
        ),
        (format!("SELECT * FROM {g0} WHERE {{ ?s <{EX}p>+ ?o }}"), 3),
        (format!("SELECT * FROM {g9} WHERE {{ ?s ?p ?o }}"), 0),
        // FROM alone: no named graphs.
        (
            format!("SELECT * FROM {g0} WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }}"),
            0,
        ),
        (
            format!("SELECT * FROM {g0} WHERE {{ GRAPH {g1} {{ ?s ?p ?o }} }}"),
            0,
        ),
        (format!("SELECT * FROM {g0} WHERE {{ GRAPH ?g {{ }} }}"), 0),
        // FROM NAMED alone: an empty default graph.
        (format!("SELECT * FROM NAMED {g0} WHERE {{ ?s ?p ?o }}"), 0),
        (
            format!("SELECT * FROM NAMED {g0} WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }}"),
            2,
        ),
        (
            format!("SELECT * FROM NAMED {g0} WHERE {{ GRAPH {g1} {{ ?s ?p ?o }} }}"),
            0,
        ),
        (
            format!(
                "SELECT * FROM NAMED {g0} FROM NAMED {g2} WHERE {{ GRAPH ?g {{ ?s <{EX}p>+ ?o }} }}"
            ),
            4,
        ),
        (
            format!(
                "SELECT * FROM NAMED {g0} FROM NAMED {g2} FROM NAMED {g9} WHERE {{ GRAPH ?g {{ }} }}"
            ),
            2,
        ),
        // Without a dataset: every named graph, and not the default graph.
        ("SELECT * WHERE { GRAPH ?g { } }".to_owned(), 3),
        (
            format!("SELECT * WHERE {{ GRAPH ?g {{ ?s <{EX}p>+ ?o }} }}"),
            6,
        ),
        (
            format!(
                "SELECT * FROM {g2} FROM NAMED {g0} WHERE {{ ?x ?q ?y GRAPH ?g {{ ?s ?p ?o }} }}"
            ),
            2,
        ),
        // a-b is in g0 as well, which the merge doesn't hold: it counts, once.
        (
            format!("SELECT * FROM {g1} FROM {g2} WHERE {{ ?s ?p ?o }}"),
            3,
        ),
    ] {
        assert_eq!(count(&text, &plain), solutions, "{text}");
    }
    // The store's default graph named in the protocol's dataset.
    for (default, named, text, solutions) in [
        (
            vec![GraphName::DefaultGraph],
            Some(vec![ex("g2").into()]),
            "SELECT * WHERE { ?s ?p ?o }",
            1,
        ),
        (
            vec![GraphName::DefaultGraph],
            Some(vec![ex("g2").into()]),
            "SELECT * WHERE { GRAPH ?g { ?s ?p ?o } }",
            1,
        ),
        (
            vec![GraphName::DefaultGraph, ex("g1").into()],
            None,
            "SELECT * WHERE { ?s ?p ?o }",
            3,
        ),
    ] {
        let mut dataset = nrese_sparql::QueryDatasetSpecification::new();
        dataset.set_default_graph(default);
        if let Some(named) = named {
            dataset.set_available_named_graphs(named);
        }
        let options = QueryOptions {
            dataset: Some(dataset),
            union_default_graph: true,
            ..QueryOptions::default()
        };
        assert_eq!(
            count(text, &options),
            solutions,
            "{text} over {:?}",
            options.dataset
        );
    }
    // spareval evaluates the pattern in a graph that isn't there: only the native executor
    // is checked.
    let native_count = |text: &str| {
        let query = SparqlParser::new().parse_query(text).unwrap();
        rows(evaluate_query(&snapshot, &query, &plain).unwrap(), false).len()
    };
    let subquery = "SELECT ?g ?n WHERE { GRAPH ?g { SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o } } }";
    let query = SparqlParser::new().parse_query(subquery).unwrap();
    let integer = "^^<http://www.w3.org/2001/XMLSchema#integer>";
    assert_eq!(
        rows(evaluate_query(&snapshot, &query, &plain).unwrap(), false),
        [
            format!("{g0}\t\"2\"{integer}"),
            format!("{g1}\t\"2\"{integer}"),
            format!("{g2}\t\"1\"{integer}"),
        ],
        "a subquery under GRAPH ?g runs in each graph"
    );
    for (text, solutions) in [
        (
            "SELECT * WHERE { GRAPH ?g { ?s ?p ?o MINUS { ?x ?y ?z } } }".to_owned(),
            5,
        ),
        (
            format!("SELECT * WHERE {{ GRAPH {g9} {{ VALUES ?x {{ 1 2 }} }} }}"),
            0,
        ),
        (
            format!("SELECT * WHERE {{ GRAPH {g0} {{ VALUES ?x {{ 1 2 }} }} }}"),
            2,
        ),
        (
            format!("SELECT * FROM NAMED {g1} WHERE {{ GRAPH {g0} {{ BIND(1 AS ?x) }} }}"),
            0,
        ),
        (
            format!(
                "SELECT * FROM NAMED {g0} FROM NAMED {g2} WHERE {{ GRAPH ?g {{ VALUES ?x {{ 1 2 }} }} }}"
            ),
            4,
        ),
        (
            format!("SELECT * WHERE {{ GRAPH {g9} {{ <{EX}x> <{EX}p>* ?o }} }}"),
            0,
        ),
        // A BIND to the graph variable keeps the rows of the graph it names.
        (
            format!("SELECT * WHERE {{ GRAPH ?g {{ ?s ?p ?o BIND({g0} AS ?g) }} }}"),
            2,
        ),
        (
            format!("SELECT * WHERE {{ GRAPH {g2} {{ <{EX}x> <{EX}p>* ?o }} }}"),
            2,
        ),
    ] {
        assert_eq!(native_count(&text), solutions, "{text}");
    }
    // The protocol's dataset replaces the query's.
    let mut dataset = nrese_sparql::QueryDatasetSpecification::new();
    dataset.set_default_graph(vec![ex("g2").into()]);
    dataset.set_available_named_graphs(vec![ex("g1").into()]);
    let protocol = QueryOptions {
        dataset: Some(dataset),
        ..QueryOptions::default()
    };
    for (text, solutions) in [
        (format!("SELECT * FROM {g0} WHERE {{ ?s ?p ?o }}"), 1),
        (
            format!("SELECT * FROM NAMED {g0} WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }}"),
            2,
        ),
        (format!("SELECT * WHERE {{ GRAPH {g0} {{ ?s ?p ?o }} }}"), 0),
    ] {
        assert_eq!(count(&text, &protocol), solutions, "{text}");
    }
    // The merged default graph is the store's default only: a dataset names its own.
    let merged = QueryOptions {
        union_default_graph: true,
        ..QueryOptions::default()
    };
    assert_eq!(count("SELECT * WHERE { ?s ?p ?o }", &merged), 5);
    assert_eq!(
        count(&format!("SELECT * FROM {g0} WHERE {{ ?s ?p ?o }}"), &merged),
        2
    );
}

/// Updates with a dataset of their own (`WITH`, `USING`, `USING NAMED`, the protocol's
/// parameters) change the store the same way on both executors.
#[test]
fn updates_with_a_dataset_equal_the_reference() {
    use nrese_sparql::UpdateOptions;
    let mut rng = Rng(20_260_936);
    let (mut checked, mut changed) = (0, 0);
    for _ in 0..60 {
        let engines = [
            Engine::new(EngineConfig::default()).unwrap(),
            Engine::new(EngineConfig::default()).unwrap(),
        ];
        let seed = rng.0;
        for engine in &engines {
            rng.0 = seed;
            load_spread_over_graphs(engine, &mut rng);
        }
        for _ in 0..8 {
            let p = format!("<{EX}p{}>", rng.below(4));
            let pattern = match rng.below(4) {
                0 => format!("?a {p} ?b"),
                1 => format!("?a {p} ?b . GRAPH ?g {{ ?b ?q ?c }}"),
                2 => format!("?a {p}+ ?b"),
                _ => format!("?a {p} ?b OPTIONAL {{ GRAPH <{EX}g1> {{ ?b ?q ?c }} }}"),
            };
            let template = *rng.pick(&[
                "DELETE { ?a ?p ?b } INSERT { ?b <http://example.com/q> ?a }",
                "INSERT { GRAPH <http://example.com/new> { ?a <http://example.com/q> ?b } }",
                "DELETE { ?a <http://example.com/p0> ?b }",
                "INSERT { ?b <http://example.com/q> ?c }",
            ]);
            let text = match rng.below(5) {
                0 => format!("WITH <{EX}g0> {template} WHERE {{ {pattern} }}"),
                1 => format!("{template} USING <{EX}g0> WHERE {{ {pattern} }}"),
                2 => format!("{template} USING <{EX}g0> USING <{EX}g1> WHERE {{ {pattern} }}"),
                3 => format!(
                    "{template} USING <{EX}e1> USING NAMED <{EX}g0> USING NAMED <{EX}g1> WHERE {{ {pattern} }}"
                ),
                _ => format!("{template} WHERE {{ {pattern} }}"),
            };
            let using = (rng.below(4) == 0).then(|| {
                let mut dataset = nrese_sparql::QueryDatasetSpecification::new();
                dataset.set_default_graph(vec![ex("g1").into(), ex("g0").into()]);
                dataset.set_available_named_graphs(vec![ex("e1").into()]);
                dataset
            });
            let update = SparqlParser::new()
                .parse_update(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let before = contents(&engines[0]);
            for (engine, reference) in engines.iter().zip([false, true]) {
                let options = UpdateOptions {
                    using: using.clone(),
                    ..UpdateOptions::default()
                };
                update_engine(engine, &update, &options, reference);
            }
            let (native, oracle) = (contents(&engines[0]), contents(&engines[1]));
            assert_eq!(native, oracle, "{text} (protocol {using:?})");
            checked += 1;
            changed += usize::from(native != before);
        }
    }
    assert!(
        checked == 480 && changed * 3 > checked,
        "{changed} of {checked} updates changed something"
    );
}

/// The default graph merged from listed graphs (`FROM <g0> FROM <g1>`) through the
/// shortcuts that read the index directly (an index nested-loop join, a streamed `LIMIT`,
/// `COUNT(*)` of one pattern): each statement once, and none from another graph.
#[test]
fn a_merge_of_listed_graphs_takes_no_shortcut_through_other_graphs() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let quad = |s: &str, o: String, g: &str| Quad::new(ex(s), ex("p"), ex(&o), ex(g));
    tx.insert(quad("a", "b".to_owned(), "g0").as_ref());
    for i in 0..40 {
        for g in ["g0", "g1"] {
            tx.insert(quad("b", format!("x{i}"), g).as_ref());
        }
        tx.insert(quad("b", format!("y{i}"), "g2").as_ref());
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let from = format!("FROM <{EX}g0> FROM <{EX}g1>");
    for (text, solutions) in [
        (
            format!("SELECT * {from} WHERE {{ <{EX}a> <{EX}p> ?b . ?b <{EX}p> ?c }}"),
            40,
        ),
        (
            format!("SELECT * {from} WHERE {{ ?b <{EX}p> ?c }} LIMIT 100"),
            41,
        ),
        (
            format!("SELECT (COUNT(*) AS ?n) {from} WHERE {{ ?b <{EX}p> ?c }}"),
            1,
        ),
    ] {
        let query = SparqlParser::new().parse_query(&text).unwrap();
        let options = QueryOptions::default();
        assert_eq!(
            explain_query(&snapshot, &query, &options).unwrap().executor,
            "native"
        );
        let native = rows(evaluate_query(&snapshot, &query, &options).unwrap(), false);
        assert_eq!(native.len(), solutions, "{text}");
        assert!(
            native.iter().all(|row| !row.contains("/y")),
            "{text}: {native:?}"
        );
        if text.contains("COUNT") {
            assert!(native[0].starts_with("\"41\""), "{text}: {native:?}");
        }
    }
}

/// An `EXISTS` for [`exists_equal_the_reference`]: over any pattern, correlated with the outer
/// solution through the variables it binds, through filters at its top, or deeper (inside
/// an OPTIONAL, MINUS or BIND, where the native executor hands the query back).
fn exists_pattern(rng: &mut Rng) -> String {
    let p = |rng: &mut Rng| format!("<{EX}p{}>", rng.below(4));
    let outer = |rng: &mut Rng| rng.pick(&["?a", "?b", "?c"]).to_string();
    match rng.below(17) {
        0 => format!("?b {} ?x", p(rng)),
        1 => format!("?x {} {}", p(rng), outer(rng)),
        // Correlated filters at the top.
        2 => format!("?x {} ?y FILTER(?y > {})", p(rng), outer(rng)),
        3 => format!("?a {} ?y FILTER(?y != ?b && ?y != {})", p(rng), outer(rng)),
        4 => format!("?x {} ?y FILTER(sameTerm(?x, {}))", p(rng), outer(rng)),
        5 => format!("{{ ?b {} ?x }} UNION {{ ?x {} ?b }}", p(rng), p(rng)),
        6 => format!("?a {}+ ?x", p(rng)),
        7 => format!("?a {} ?x OPTIONAL {{ ?x {} ?y }}", p(rng), p(rng)),
        8 => format!("VALUES ?b {{ <{EX}e1> 3 \"s1\"@en }}"),
        9 => format!(
            "{{ SELECT ?a (COUNT(?x) AS ?n) WHERE {{ ?a {} ?x }} GROUP BY ?a }} FILTER(?n > 1)",
            p(rng)
        ),
        10 => format!("?a {} ?x MINUS {{ ?x {} ?y }}", p(rng), p(rng)),
        11 => format!(
            "?a {} ?x FILTER NOT EXISTS {{ ?x {} {} }}",
            p(rng),
            p(rng),
            outer(rng)
        ),
        // Deeper correlation: the general evaluator's.
        12 => format!(
            "?x {} ?y OPTIONAL {{ ?y {} {} }}",
            p(rng),
            p(rng),
            outer(rng)
        ),
        13 => format!(
            "?x {} ?y BIND({} AS ?z) FILTER(?z = ?y)",
            p(rng),
            outer(rng)
        ),
        14 => format!("?x {} ?y MINUS {{ ?y {} {} }}", p(rng), p(rng), outer(rng)),
        // A subquery's own variable that shares a name with an outer one isn't the outer one.
        15 => format!(
            "{{ SELECT ?x WHERE {{ ?x {} ?y OPTIONAL {{ ?y {} {} }} }} }}",
            p(rng),
            p(rng),
            outer(rng)
        ),
        _ => format!("{{ SELECT ?b WHERE {{ ?b {} ?y }} LIMIT 1 }}", p(rng)),
    }
}

/// `EXISTS` over any pattern and anywhere an expression may stand (FILTER, inside `||`
/// and `!`, BIND, an OPTIONAL's condition, under GRAPH), correlated with the outer
/// solution in every way, on outer solutions with unbound variables: equal to the reference evaluator,
/// and on the native executor wherever the correlation allows.
#[test]
fn exists_equal_the_reference() {
    let mut rng = Rng(20_260_937);
    let (mut checked, mut native_runs, mut with_solutions) = (0, 0, 0);
    for _ in 0..80 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        load_spread_over_graphs(&engine, &mut rng);
        let snapshot = engine.snapshot();
        for query_case in 0..50 {
            let outer = format!(
                "?a <{EX}p{}> ?b . {}",
                rng.below(4),
                if rng.below(2) == 0 {
                    format!("OPTIONAL {{ ?b <{EX}p{}> ?c }}", rng.below(4))
                } else {
                    format!("?b <{EX}p{}> ?c", rng.below(4))
                }
            );
            let exists = exists_pattern(&mut rng);
            let not = if rng.below(3) == 0 { "NOT " } else { "" };
            let text = match rng.below(7) {
                0 | 1 => format!("SELECT * WHERE {{ {outer} FILTER {not}EXISTS {{ {exists} }} }}"),
                2 => format!(
                    "SELECT * WHERE {{ {outer} FILTER(BOUND(?c) || {not}EXISTS {{ {exists} }}) }}"
                ),
                3 => format!("SELECT * WHERE {{ {outer} BIND({not}EXISTS {{ {exists} }} AS ?e) }}"),
                4 => format!(
                    "SELECT * WHERE {{ ?a <{EX}p{}> ?b OPTIONAL {{ ?b <{EX}p{}> ?c FILTER {not}EXISTS {{ {exists} }} }} }}",
                    rng.below(4),
                    rng.below(4)
                ),
                // Under GRAPH ?g spareval evaluates a subquery once over all graphs
                // (`dataset_body`).
                5 if !exists.contains("SELECT") => format!(
                    "SELECT * WHERE {{ GRAPH ?g {{ {outer} FILTER {not}EXISTS {{ {exists} }} }} }}"
                ),
                _ => format!(
                    "SELECT ?a (COUNT(*) AS ?n) WHERE {{ {outer} FILTER {not}EXISTS {{ {exists} }} }} GROUP BY ?a"
                ),
            };
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let options = QueryOptions {
                read_model: *rng.pick(&[ReadModel::Materialised, ReadModel::Asserted]),
                union_default_graph: rng.below(2) == 0,
                ..QueryOptions::default()
            };
            let context = format!(
                "query {query_case}, merged {}: {text}",
                options.union_default_graph
            );
            if runs_natively(&query) {
                assert_eq!(
                    explain_query(&snapshot, &query, &options).unwrap().executor,
                    "native",
                    "{context}"
                );
                native_runs += 1;
            }
            let native = rows(evaluate_query(&snapshot, &query, &options).unwrap(), false);
            let oracle = QueryOptions { ..options.clone() };
            let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
            assert_same_rows(&native, &expected, &context);
            checked += 1;
            with_solutions += usize::from(!native.is_empty());
        }
    }
    assert!(
        // A quarter of the EXISTS patterns are correlated deeper than the native executor
        // takes.
        native_runs * 10 > checked * 7 && with_solutions * 2 > checked,
        "{checked} checked, {native_runs} native, {with_solutions} with solutions"
    );
}

/// DESCRIBE over variables, IRIs and `*`, in the store's default graph, the merged one and
/// a dataset's, with blank node objects (described in turn, chains and a cycle among them):
/// the native executor answers it, as the reference evaluator does.
#[test]
fn describe_equals_the_reference() {
    use oxrdf::BlankNode;
    let mut rng = Rng(20_260_938);
    let mut checked = 0;
    for _ in 0..40 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        load_spread_over_graphs(&engine, &mut rng);
        let mut tx = engine.transaction();
        let (b0, b1) = (BlankNode::default(), BlankNode::default());
        for (s, p, o) in [
            (ex("e1").into(), ex("p1"), Term::from(b0.clone())),
            (b0.clone().into(), ex("p2"), b1.clone().into()),
            (b1.clone().into(), ex("p2"), b0.clone().into()),
            (b1.clone().into(), ex("p3"), ex("e2").into()),
        ] {
            let s: oxrdf::NamedOrBlankNode = s;
            tx.insert(Quad::new(s, p, o, GraphName::DefaultGraph).as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..25 {
            let p = rng.below(4);
            let text = match rng.below(6) {
                0 => format!("DESCRIBE ?a WHERE {{ ?a <{EX}p{p}> ?b }}"),
                1 => format!("DESCRIBE ?a ?b WHERE {{ ?a <{EX}p{p}> ?b }}"),
                2 => format!("DESCRIBE <{EX}e{}>", rng.below(7)),
                // Which rows a LIMIT keeps is open without an order that fixes them.
                3 => format!("DESCRIBE * WHERE {{ ?a <{EX}p{p}> ?b }} ORDER BY ?a ?b LIMIT 3"),
                4 => format!("DESCRIBE ?a FROM <{EX}g0> WHERE {{ ?a <{EX}p{p}> ?b }}"),
                _ => format!("DESCRIBE <{EX}e1> ?b WHERE {{ <{EX}e1> ?p ?b }}"),
            };
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let options = QueryOptions {
                union_default_graph: rng.below(3) == 0,
                ..QueryOptions::default()
            };
            assert!(runs_natively(&query), "{text}");
            assert_eq!(
                explain_query(&snapshot, &query, &options).unwrap().executor,
                "native"
            );
            let native = graph(evaluate_query(&snapshot, &query, &options).unwrap());
            let oracle = QueryOptions { ..options.clone() };
            let expected = graph(reference(&snapshot, &query, &oracle).unwrap());
            assert_eq!(
                native, expected,
                "{text} (merged {})",
                options.union_default_graph
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 1000);
}

/// TIMEZONE and TZ of dates and times of every kind, the hashes, and IRI() resolved against
/// the query's BASE equal the reference evaluator; RAND, UUID, STRUUID, BNODE() and NOW() have the
/// properties SPARQL gives them (fresh per call, or one value per query).
#[test]
fn the_remaining_functions_run_natively() {
    let mut rng = Rng(20_260_939);
    let engine = Engine::new(EngineConfig::default()).unwrap();
    load_spread_over_graphs(&engine, &mut rng);
    let snapshot = engine.snapshot();
    let xsd = "http://www.w3.org/2001/XMLSchema#";
    let temporal = [
        format!("\"2001-01-01T10:00:00Z\"^^<{xsd}dateTime>"),
        format!("\"2001-01-01T10:00:00-05:30\"^^<{xsd}dateTime>"),
        format!("\"2001-01-01T10:00:00\"^^<{xsd}dateTime>"),
        format!("\"2001-01-01+14:00\"^^<{xsd}date>"),
        format!("\"10:00:00+01:00\"^^<{xsd}time>"),
        format!("\"2001-02Z\"^^<{xsd}gYearMonth>"),
        format!("\"2001\"^^<{xsd}gYear>"),
        format!("\"--02-03-01:00\"^^<{xsd}gMonthDay>"),
        format!("\"---03Z\"^^<{xsd}gDay>"),
        format!("\"--02\"^^<{xsd}gMonth>"),
        format!("\"not a date\"^^<{xsd}dateTime>"),
        "\"2001-01-01\"".to_owned(),
    ];
    let mut texts: Vec<String> = Vec::new();
    for value in &temporal {
        texts.push(format!(
            "SELECT (TIMEZONE({value}) AS ?d) (TZ({value}) AS ?t) WHERE {{}}"
        ));
    }
    // Over the stored dates (with and without timezones) and strings.
    texts.push("SELECT ?o (TIMEZONE(?o) AS ?d) (TZ(?o) AS ?t) WHERE { ?s ?p ?o }".to_owned());
    for hash in ["MD5", "SHA1", "SHA256", "SHA384", "SHA512"] {
        texts.push(format!("SELECT ?o ({hash}(?o) AS ?h) WHERE {{ ?s ?p ?o }}"));
        texts.push(format!(
            "SELECT ({hash}(\"abc\") AS ?h) ({hash}(\"é\"^^<{xsd}string>) AS ?k) WHERE {{}}"
        ));
    }
    for base in ["http://example.com/dir/", "http://example.com/dir/doc#frag"] {
        texts.push(format!(
            "BASE <{base}> SELECT ?o (IRI(?v) AS ?i) WHERE {{ ?s ?p ?o BIND(STR(?o) AS ?v) }}"
        ));
        texts.push(format!(
            "BASE <{base}> SELECT (IRI(\"../up\") AS ?a) (IRI(\"x?q\") AS ?b) (IRI(\"\") AS ?c) (IRI(<rel>) AS ?d) WHERE {{}}"
        ));
    }
    for text in &texts {
        let query = SparqlParser::new()
            .parse_query(text)
            .unwrap_or_else(|e| panic!("{e}: {text}"));
        assert!(runs_natively(&query), "{text}");
        let native = rows(
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
            false,
        );
        let oracle = QueryOptions {
            ..QueryOptions::default()
        };
        let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
        assert_eq!(native, expected, "{text}");
    }
    let one = |text: &str| -> Vec<String> {
        let query = SparqlParser::new().parse_query(text).unwrap();
        assert!(runs_natively(&query), "{text}");
        rows(
            evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
            false,
        )
    };
    let count = one("SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }")[0].clone();
    // Fresh per call: as many distinct values as rows; RAND in [0, 1).
    for fresh in ["UUID()", "STRUUID()", "BNODE()", "RAND()"] {
        assert_eq!(
            one(&format!(
                "SELECT (COUNT(DISTINCT ?x) AS ?n) WHERE {{ ?s ?p ?o BIND({fresh} AS ?x) }}"
            ))[0],
            count,
            "{fresh}"
        );
    }
    assert_eq!(
        one(
            "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o FILTER(RAND() >= 0 && RAND() < 1 && isNumeric(RAND())) }"
        )[0],
        count
    );
    let uuids = one(
        "SELECT ?u ?s WHERE { BIND(UUID() AS ?u) BIND(STRUUID() AS ?s) FILTER(isIRI(?u) && STRSTARTS(STR(?u), \"urn:uuid:\") && STRLEN(?s) = 36 && REGEX(?s, \"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$\")) }",
    );
    assert_eq!(uuids.len(), 1, "{uuids:?}");
    assert_eq!(
        one("SELECT ?b WHERE { BIND(BNODE() AS ?b) FILTER(isBlank(?b)) }").len(),
        1
    );
    // NOW: one instant for the whole query, a dateTime with a timezone.
    assert_eq!(
        one(
            "SELECT (COUNT(DISTINCT ?t) AS ?n) WHERE { ?s ?p ?o BIND(NOW() AS ?t) FILTER(DATATYPE(?t) = <http://www.w3.org/2001/XMLSchema#dateTime> && TZ(?t) != \"\") }"
        )[0],
        "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    );
}

/// Data where one predicate is rare and the others common, so that a join across group
/// parts pays to evaluate from the rare side (native/sideways.rs).
fn skewed_dataset(rng: &mut Rng) -> Vec<Quad> {
    let mut quads = Vec::new();
    for i in 0..600 {
        for p in 0..3 {
            if rng.below(3) > 0 {
                quads.push(Quad::new(
                    ex(&format!("e{i}")),
                    ex(&format!("p{p}")),
                    ex(&format!("e{}", rng.below(600))),
                    GraphName::DefaultGraph,
                ));
            }
        }
        quads.push(Quad::new(
            ex(&format!("e{i}")),
            ex("name"),
            Literal::new_simple_literal(format!("n{}", i % 97)),
            GraphName::DefaultGraph,
        ));
    }
    for _ in 0..6 {
        quads.push(Quad::new(
            ex(&format!("e{}", rng.below(600))),
            ex("rare"),
            ex(&format!("e{}", rng.below(600))),
            GraphName::DefaultGraph,
        ));
    }
    quads
}

/// Groups whose parts a join crosses (OPTIONAL, UNION, BIND, FILTER, paths, subqueries,
/// MINUS), with a rare pattern in one part and common ones in the others, in both orders:
/// evaluating one part from another's rows equals the reference evaluator, and the rare side does seed
/// index probes into the common patterns.
#[test]
fn sideways_joins_equal_the_reference() {
    let mut rng = Rng(20_260_940);
    let (mut checked, mut probed) = (0, 0);
    for _ in 0..4 {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in skewed_dataset(&mut rng) {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..60 {
            let p = |rng: &mut Rng| format!("<{EX}p{}>", rng.below(3));
            let rare = format!("?a <{EX}rare> ?b .");
            let common = |rng: &mut Rng| {
                let v = *rng.pick(&["?a", "?b"]);
                match rng.below(11) {
                    // ?b is out of this group's scope: the seed must not show it.
                    10 => format!(
                        "?a <{EX}name> ?n . BIND(COALESCE(?b, \"none\") AS ?m{})",
                        rng.below(1_000_000)
                    ),
                    0 => format!("{v} {} ?c . ?c {} ?d .", p(rng), p(rng)),
                    1 => format!("OPTIONAL {{ {v} {} ?c . ?c <{EX}name> ?n }}", p(rng)),
                    2 => format!("OPTIONAL {{ {v} {} ?c FILTER(?c != ?a) }}", p(rng)),
                    3 => format!("{{ {v} {} ?c }} UNION {{ ?c {} {v} }}", p(rng), p(rng)),
                    4 => format!(
                        "{v} <{EX}name> ?n . BIND(CONCAT(?n, \"!\") AS ?m{})",
                        rng.below(1_000_000)
                    ),
                    5 => format!("{v} {} ?c FILTER(?c != ?b)", p(rng)),
                    6 => format!("{v} {}+ ?c .", p(rng)),
                    7 => format!(
                        "{{ SELECT {v} (COUNT(?c) AS ?k) WHERE {{ {v} {} ?c }} GROUP BY {v} }}",
                        p(rng)
                    ),
                    8 => format!("{v} {} ?c MINUS {{ ?c {} ?a }}", p(rng), p(rng)),
                    _ => format!(
                        "OPTIONAL {{ {v} {} ?c }} OPTIONAL {{ ?c <{EX}name> ?n }}",
                        p(rng)
                    ),
                }
            };
            let (x, y) = (common(&mut rng), common(&mut rng));
            let body = match rng.below(4) {
                0 => format!("{rare} {x}"),
                1 => format!("{x} {rare}"),
                2 => format!("{{ {x} }} {{ {rare} }} {y}"),
                _ => format!("{x} {{ {rare} {y} }}"),
            };
            let text = format!("SELECT * WHERE {{ {body} }}");
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            assert!(runs_natively(&query), "{text}");
            let native = rows(
                evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
                false,
            );
            let oracle = QueryOptions {
                ..QueryOptions::default()
            };
            let expected = rows(reference(&snapshot, &query, &oracle).unwrap(), false);
            assert_same_rows(&native, &expected, &text);
            let plan = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
            probed += usize::from(plan.steps.iter().any(|s| s.operator == "index join"));
            checked += 1;
        }
    }
    assert!(
        probed * 3 > checked,
        "{probed} of {checked} probed the index"
    );
}

/// Requests of several operations, where later WHERE clauses read what earlier ones
/// inserted and deleted (on the pending state's snapshot): the store ends as with the reference evaluator.
#[test]
fn multi_operation_updates_equal_the_reference() {
    use nrese_sparql::UpdateOptions;
    let mut rng = Rng(20_260_941);
    let mut changed = 0;
    for _ in 0..40 {
        let data = random_dataset(&mut rng);
        let engines = [
            Engine::new(EngineConfig::default()).unwrap(),
            Engine::new(EngineConfig::default()).unwrap(),
        ];
        for engine in &engines {
            let mut tx = engine.transaction();
            for quad in &data {
                tx.insert(quad.as_ref());
            }
            tx.commit().unwrap();
        }
        for _ in 0..8 {
            let mut operations = Vec::new();
            for _ in 0..2 + rng.below(3) {
                let p = rng.below(4);
                let q = rng.below(4);
                operations.push(match rng.below(5) {
                    0 => format!(
                        "INSERT DATA {{ <{EX}e{}> <{EX}p{p}> <{EX}e{}> . <{EX}new{}> <{EX}p{q}> \"s1\" }}",
                        rng.below(6),
                        rng.below(6),
                        rng.below(3)
                    ),
                    1 => format!(
                        "DELETE {{ ?a <{EX}p{p}> ?b }} INSERT {{ ?b <{EX}p{q}> ?a }} WHERE {{ ?a <{EX}p{p}> ?b . {} }}",
                        group_pattern(&mut rng, 1)
                    ),
                    2 => format!("DELETE WHERE {{ ?a <{EX}p{p}> ?b . ?b <{EX}p{q}> ?c }}"),
                    3 => format!(
                        "INSERT {{ GRAPH <{EX}g> {{ ?a <{EX}copy> ?b }} }} WHERE {{ ?a <{EX}p{p}> ?b }}"
                    ),
                    _ => format!(
                        "INSERT {{ ?a <{EX}seen> ?c }} WHERE {{ GRAPH <{EX}g> {{ ?a <{EX}copy> ?b }} ?b <{EX}p{q}> ?c }}"
                    ),
                });
            }
            let text = operations.join(" ;\n");
            let update = SparqlParser::new()
                .parse_update(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let before = contents(&engines[0]);
            for (engine, reference) in engines.iter().zip([false, true]) {
                let options = UpdateOptions {
                    ..UpdateOptions::default()
                };
                update_engine(engine, &update, &options, reference);
            }
            let (native, oracle) = (contents(&engines[0]), contents(&engines[1]));
            assert_eq!(native, oracle, "{text}");
            changed += usize::from(native != before);
        }
    }
    assert!(changed > 150, "only {changed} requests changed anything");
}

/// Data closed under `owl:sameAs` (every fact replicated over identity classes, the
/// `sameAs` relation complete): with `equality_closed`, grouped queries whose OPTIONALs
/// feed `COUNT(DISTINCT …)`, MIN, MAX or SAMPLE join on one representative per class;
/// the answers equal the reference evaluator's, and the OPTIONAL's join is smaller.
#[test]
fn identity_classes_shrink_detached_optionals() {
    let same_as = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let mut rng = Rng(20_260_942);
    let (mut checked, mut shrunk) = (0, 0);
    for _ in 0..12 {
        // Classes of 1 to 5 identities; facts between classes, replicated.
        let mut classes: Vec<Vec<NamedNode>> = Vec::new();
        for c in 0..12 {
            classes.push(
                (0..1 + rng.below(5))
                    .map(|m| ex(&format!("c{c}m{m}")))
                    .collect(),
            );
        }
        let mut quads = Vec::new();
        for class in &classes {
            for a in class {
                for b in class {
                    if class.len() > 1 {
                        quads.push(Quad::new(
                            a.clone(),
                            same_as.clone(),
                            b.clone(),
                            GraphName::DefaultGraph,
                        ));
                    }
                }
            }
        }
        for _ in 0..40 {
            let (s, o) = (rng.below(12) as usize, rng.below(12) as usize);
            let p = ex(&format!("p{}", rng.below(3)));
            let literal = rng.below(4) == 0;
            for a in &classes[s] {
                if literal {
                    quads.push(Quad::new(
                        a.clone(),
                        p.clone(),
                        Literal::new_simple_literal(format!("v{o}")),
                        GraphName::DefaultGraph,
                    ));
                } else {
                    for b in &classes[o] {
                        quads.push(Quad::new(
                            a.clone(),
                            p.clone(),
                            b.clone(),
                            GraphName::DefaultGraph,
                        ));
                    }
                }
            }
        }
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        for quad in &quads {
            tx.insert(quad.as_ref());
        }
        tx.commit().unwrap();
        let snapshot = engine.snapshot();
        for _ in 0..15 {
            let (p, q) = (rng.below(3), rng.below(3));
            let aggregate = *rng.pick(&[
                "COUNT(DISTINCT ?x)",
                "MIN(?x)",
                "MAX(?x)",
                "SAMPLE(?x)",
                "COUNT(DISTINCT ?x) AS ?m) (MIN(?x)",
            ]);
            let text = format!(
                "SELECT ?k ({aggregate} AS ?n) WHERE {{ ?k <{EX}p{p}> ?y . OPTIONAL {{ ?y <{EX}p{q}> ?x }} }} GROUP BY ?k"
            );
            let query = SparqlParser::new()
                .parse_query(&text)
                .unwrap_or_else(|e| panic!("{e}: {text}"));
            let closed = QueryOptions {
                equality_closed: true,
                ..QueryOptions::default()
            };
            // SAMPLE may pick another value; compare it as "bound or not".
            let normalise = |rows: Vec<String>| -> Vec<String> {
                if aggregate.starts_with("SAMPLE") {
                    rows.into_iter()
                        .map(|r| {
                            // Columns are tab-separated: the key, then the sample.
                            let bound = !r.ends_with("UNDEF");
                            format!("{} {bound}", r.split('\t').next().unwrap())
                        })
                        .collect::<Vec<_>>()
                } else {
                    rows
                }
            };
            let native = normalise(rows(
                evaluate_query(&snapshot, &query, &closed).unwrap(),
                false,
            ));
            let oracle = QueryOptions {
                ..QueryOptions::default()
            };
            let mut expected =
                normalise(rows(reference(&snapshot, &query, &oracle).unwrap(), false));
            expected.sort();
            let mut native_sorted = native.clone();
            native_sorted.sort();
            assert_eq!(native_sorted, expected, "{text}");
            let rows_of = |options: &QueryOptions| -> u64 {
                explain_query(&snapshot, &query, options)
                    .unwrap()
                    .steps
                    .iter()
                    .filter(|s| s.operator == "optional")
                    .map(|s| s.rows)
                    .sum()
            };
            let open = QueryOptions::default();
            shrunk += usize::from(rows_of(&closed) < rows_of(&open));
            checked += 1;
        }
    }
    assert!(shrunk * 3 > checked, "{shrunk} of {checked} joins shrank");
}

/// A selective filter makes its pattern count as smaller when the join order is planned:
/// the equality-filtered larger pattern starts, and the answers don't change.
#[test]
fn selective_filters_move_their_pattern_first() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for i in 0..3000 {
        tx.insert(
            Quad::new(
                ex(&format!("s{}", i % 1500)),
                ex("label"),
                Literal::new_simple_literal(format!("v{i}")),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    for i in 0..500 {
        tx.insert(
            Quad::new(
                ex(&format!("s{i}")),
                ex("kind"),
                ex(&format!("k{}", i % 7)),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    let snapshot = engine.snapshot();
    let text =
        format!("SELECT * WHERE {{ ?s <{EX}label> ?l . ?s <{EX}kind> ?k FILTER(?l = \"v42\") }}");
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let plan = explain_query(&snapshot, &query, &QueryOptions::default()).unwrap();
    let first = plan
        .steps
        .iter()
        .find(|s| s.operator == "scan" || s.operator == "range scan")
        .unwrap();
    assert!(first.detail.contains("label"), "{:#?}", plan.steps);
    let optimised = rows(
        evaluate_query(&snapshot, &query, &QueryOptions::default()).unwrap(),
        false,
    );
    let written = QueryOptions {
        as_written: true,
        ..QueryOptions::default()
    };
    assert_eq!(
        optimised,
        rows(evaluate_query(&snapshot, &query, &written).unwrap(), false)
    );
    assert_eq!(optimised.len(), 1);
}
