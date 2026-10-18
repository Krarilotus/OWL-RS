//! Full-text search with Blazegraph's `bds:` vocabulary (native/search.rs): what matches,
//! relevance and rank, the options, the graph a pattern reads, and literals that the data
//! no longer uses.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{
    QueryEvaluationError, QueryOptions, QueryResults, evaluate_query, explain_query,
};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const PREFIXES: &str = "PREFIX bds: <http://www.bigdata.com/rdf/search#> \
                        PREFIX text: <http://jena.apache.org/text#> \
                        PREFIX luc: <http://www.ontotext.com/owlim/lucene#> \
                        PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> \
                        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> \
                        PREFIX ex: <http://example.com/> ";

fn ex(local: &str) -> NamedNode {
    NamedNode::new_unchecked(format!("{EX}{local}"))
}

fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let label = NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label");
    for (subject, text, graph) in [
        ("bridge", Literal::new_simple_literal("Tower Bridge"), None),
        (
            "tower",
            Literal::new_language_tagged_literal_unchecked("Tower of London", "en"),
            None,
        ),
        (
            "bells",
            Literal::new_simple_literal("Bridge, bridge and more bridges"),
            None,
        ),
        (
            "museum",
            Literal::new_simple_literal("British Museum"),
            None,
        ),
        (
            "hidden",
            Literal::new_simple_literal("A bridge in another graph"),
            Some("g"),
        ),
    ] {
        let graph: GraphName = match graph {
            Some(g) => ex(g).into(),
            None => GraphName::DefaultGraph,
        };
        tx.insert(Quad::new(ex(subject), label.clone(), Term::from(text), graph).as_ref());
    }
    // A literal the data used once: in the dictionary, but no statement has it now.
    let gone = Quad::new(
        ex("gone"),
        label.clone(),
        Literal::new_simple_literal("London bridge is falling down"),
        GraphName::DefaultGraph,
    );
    tx.insert(gone.as_ref());
    tx.commit().unwrap();
    let mut tx = engine.transaction();
    tx.remove(gone.as_ref());
    tx.commit().unwrap();
    engine
}

fn rows(engine: &Engine, query: &str, options: &QueryOptions) -> Vec<Vec<String>> {
    let text = format!("{PREFIXES}{query}");
    let query = SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"));
    let snapshot = engine.snapshot();
    assert_eq!(
        explain_query(&snapshot, &query, options).unwrap().executor,
        "native"
    );
    let QueryResults::Solutions(solutions) = evaluate_query(&snapshot, &query, options).unwrap()
    else {
        panic!("solutions")
    };
    let variables = solutions.variables().to_vec();
    solutions
        .map(|solution| {
            let solution = solution.unwrap();
            variables
                .iter()
                .map(|v| {
                    solution.get(v).map_or("-".to_owned(), |t| match t {
                        Term::NamedNode(n) => n.as_str().trim_start_matches(EX).to_owned(),
                        Term::Literal(l) => l.value().to_owned(),
                        other => other.to_string(),
                    })
                })
                .collect()
        })
        .collect()
}

#[test]
fn search_ranks_and_joins() {
    let engine = engine();
    let plain = QueryOptions::default();
    // Any word; best first; the literal of another graph and the unused one are not found.
    let found = rows(
        &engine,
        "SELECT ?s ?rank WHERE { ?o bds:search \"bridge\" ; bds:rank ?rank . ?s rdfs:label ?o } ORDER BY ?rank",
        &plain,
    );
    assert_eq!(
        found,
        [["bells", "1"], ["bridge", "2"]].map(|r| r.map(str::to_owned).to_vec())
    );
    // Relevance: the best is 1, the others less.
    let scored = rows(
        &engine,
        "SELECT ?o ?score WHERE { ?o bds:search \"tower bridge\" ; bds:relevance ?score } ORDER BY DESC(?score)",
        &plain,
    );
    assert_eq!(scored.len(), 3);
    assert_eq!(scored[0][0], "Tower Bridge");
    assert_eq!(scored[0][1].parse::<f64>().unwrap(), 1.0);
    assert!(scored[2][1].parse::<f64>().unwrap() < 1.0);
    // Every word; a prefix; language-tagged literals; case.
    let all = rows(
        &engine,
        "SELECT ?o WHERE { ?o bds:search \"tower bridge\" ; bds:matchAllTerms \"true\" }",
        &plain,
    );
    assert_eq!(all, [vec!["Tower Bridge".to_owned()]]);
    let prefixed = rows(
        &engine,
        "SELECT ?s WHERE { ?o bds:search \"brit*\" . ?s rdfs:label ?o }",
        &plain,
    );
    assert_eq!(prefixed, [vec!["museum".to_owned()]]);
    let tagged = rows(
        &engine,
        "SELECT ?s WHERE { ?o bds:search \"LONDON\" . ?s rdfs:label ?o }",
        &plain,
    );
    assert_eq!(tagged, [vec!["tower".to_owned()]]);
    // Rank limits and the least relevance.
    let top = rows(
        &engine,
        "SELECT ?o WHERE { ?o bds:search \"bridge\" ; bds:maxRank \"1\" }",
        &plain,
    );
    assert_eq!(top.len(), 1);
    let second = rows(
        &engine,
        "SELECT ?o WHERE { ?o bds:search \"bridge\" ; bds:minRank \"2\" }",
        &plain,
    );
    assert_eq!(second, [vec!["Tower Bridge".to_owned()]]);
    let relevant = rows(
        &engine,
        "SELECT ?o WHERE { ?o bds:search \"tower bridge\" ; bds:minRelevance \"0.99\" }",
        &plain,
    );
    assert_eq!(relevant, [vec!["Tower Bridge".to_owned()]]);
    // In a named graph, and in the merge of all graphs.
    let named = rows(
        &engine,
        "SELECT ?s ?g WHERE { GRAPH ?g { ?o bds:search \"bridge\" . ?s rdfs:label ?o } }",
        &plain,
    );
    assert_eq!(named, [vec!["hidden".to_owned(), "g".to_owned()]]);
    let merged = QueryOptions {
        union_default_graph: true,
        ..QueryOptions::default()
    };
    assert_eq!(
        rows(
            &engine,
            "SELECT ?o WHERE { ?o bds:search \"bridge\" }",
            &merged
        )
        .len(),
        3
    );
    // The shortcuts for one pattern don't read the search as a statement.
    assert_eq!(
        rows(
            &engine,
            "SELECT ?o WHERE { ?o bds:search \"bridge\" } LIMIT 5",
            &plain
        )
        .len(),
        2
    );
    assert_eq!(
        rows(
            &engine,
            "SELECT (COUNT(*) AS ?n) WHERE { ?o bds:search \"bridge\" }",
            &plain
        ),
        [vec!["2".to_owned()]]
    );
    // Nothing matches: no rows, and no error.
    assert!(
        rows(
            &engine,
            "SELECT ?o WHERE { ?o bds:search \"zebra\" }",
            &plain
        )
        .is_empty()
    );
}

/// The matches start the joins: the label pattern is probed per match, not read whole.
#[test]
fn search_results_seed_the_joins() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let label = NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label");
    for i in 0..5000 {
        let text = if i == 4321 {
            "needle".to_owned()
        } else {
            format!("hay {i}")
        };
        tx.insert(
            Quad::new(
                ex(&format!("s{i}")),
                label.clone(),
                Literal::new_simple_literal(text),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    let text =
        format!("{PREFIXES}SELECT ?s WHERE {{ ?s rdfs:label ?o . ?o bds:search \"needle\" }}");
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let explanation = explain_query(&engine.snapshot(), &query, &QueryOptions::default()).unwrap();
    let steps: Vec<(&str, u64)> = explanation
        .steps
        .iter()
        .map(|s| (s.operator.as_str(), s.rows))
        .collect();
    assert!(
        steps.contains(&("text search", 1)) && steps.contains(&("index join", 1)),
        "{steps:?}"
    );
    assert_eq!(explanation.rows, 1);
}

fn sorted(mut rows: Vec<Vec<String>>) -> Vec<Vec<String>> {
    rows.sort();
    rows
}

fn table(rows: &[&[&str]]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|row| row.iter().map(|v| (*v).to_owned()).collect())
        .collect()
}

/// `bds:stem`: words match by their stem in the language given.
#[test]
fn stemmed_search() {
    let engine = engine();
    let plain = QueryOptions::default();
    let query = |stem: &str| {
        sorted(rows(
            &engine,
            &format!("SELECT ?o WHERE {{ ?o bds:search \"bridging\" {stem} }}"),
            &plain,
        ))
    };
    assert!(query("").is_empty());
    assert_eq!(
        query("; bds:stem \"en\""),
        table(&[&["Bridge, bridge and more bridges"], &["Tower Bridge"]])
    );
}

/// Jena's `text:query`: subjects by their literals, with the score and the literal, a
/// property, `AND`, a language, a limit and `NOT`.
#[test]
fn jena_text_query() {
    let engine = engine();
    let plain = QueryOptions::default();
    let query = |q: &str| sorted(rows(&engine, q, &plain));
    // Any property; the literal of another graph and the unused one are not found.
    assert_eq!(
        query("SELECT ?s WHERE { ?s text:query \"bridge\" }"),
        table(&[&["bells"], &["bridge"]])
    );
    // A property, every word, the score.
    assert_eq!(
        query(
            "SELECT ?s ?score WHERE { (?s ?score) text:query (rdfs:label \"tower AND bridge\") }"
        ),
        table(&[&["bridge", "1"]])
    );
    // The literal, in a language; another language finds nothing.
    assert_eq!(
        query(
            "SELECT ?s ?l WHERE { (?s ?score ?l) text:query (rdfs:label \"london\" \"lang:en\") }"
        ),
        table(&[&["tower", "Tower of London"]])
    );
    assert!(query("SELECT ?s WHERE { ?s text:query (\"tower\" \"lang:de\") }").is_empty());
    // A limit on the matched literals; a property the labels don't have.
    assert_eq!(
        query("SELECT ?s WHERE { ?s text:query (\"bridge\" 1) }").len(),
        1
    );
    assert!(query("SELECT ?s WHERE { ?s text:query (ex:other \"bridge\") }").is_empty());
    // NOT excludes a word; fuzzy words; a prefix; joins with the rest of the pattern.
    assert_eq!(
        query("SELECT ?s WHERE { ?s text:query \"tower NOT bridge\" }"),
        table(&[&["tower"]])
    );
    assert_eq!(
        query("SELECT ?s WHERE { ?s text:query \"brigde~1\" }"),
        table(&[&["bells"], &["bridge"]])
    );
    assert_eq!(
        query("SELECT ?s WHERE { ?s text:query \"brit*\" ; rdfs:label ?l }"),
        table(&[&["museum"]])
    );
}

/// GraphDB's legacy Lucene predicates: resources by their literals, and the score.
#[test]
fn graphdb_lucene_predicates() {
    let engine = engine();
    let plain = QueryOptions::default();
    let query = |q: &str| sorted(rows(&engine, q, &plain));
    assert_eq!(
        query("SELECT ?x WHERE { ?x luc:labels \"bridge\" }"),
        table(&[&["bells"], &["bridge"]])
    );
    assert_eq!(
        query("SELECT ?x ?s WHERE { ?x luc:labels \"tower AND bridge\" ; luc:score ?s }"),
        table(&[&["bridge", "1"]])
    );
}

#[test]
fn lucene_rejects_lossy_or_ambiguous_translation() {
    let engine = engine();
    for text in [
        "label:tower",
        "tower^2",
        "+tower",
        "tower +bridge",
        "(tower OR bridge)",
        "tower AND bridge OR museum",
        "tower bridge AND museum",
        "tower AND bridge museum",
        "tower NOT bridge AND museum",
        "tower -bridge AND museum",
        "--tower",
        "tower -AND",
        "tower -OR",
        "tower -NOT",
        "AND~1",
        "OR~",
        "NOT~0",
        "AND tower",
        "tower AND",
        "tower OR OR bridge",
        "tower NOT",
        "NOT NOT tower",
        "tower OR NOT bridge",
        "tower NOT \"london bridge\"",
        "tower -\"london bridge\"",
        "\"tower bridge",
        "\"tower\"bridge",
        "\"tower\"~1",
        "tower && || bridge",
    ] {
        let literal = Literal::new_simple_literal(text).to_string();
        for predicate in ["text:query", "luc:labels"] {
            assert_search_argument_error(&engine, &format!("?s {predicate} {literal}"));
        }
    }
    let plain = QueryOptions::default();
    for (text, expected) in [
        ("tower AND bridge", table(&[&["bridge"]])),
        ("tower && bridge", table(&[&["bridge"]])),
        (
            "tower OR museum",
            table(&[&["bridge"], &["museum"], &["tower"]]),
        ),
        (
            "tower museum",
            table(&[&["bridge"], &["museum"], &["tower"]]),
        ),
        ("tower NOT bridge", table(&[&["tower"]])),
        ("tower AND NOT bridge", table(&[&["tower"]])),
        ("tower -bridge", table(&[&["tower"]])),
        ("tower AND london -bridge", table(&[&["tower"]])),
        ("\"tower: bridge!\"", table(&[&["bridge"]])),
        ("\"tower (bridge)^?\"", table(&[&["bridge"]])),
        ("brigde~1", table(&[&["bells"], &["bridge"]])),
        ("brit*", table(&[&["museum"]])),
    ] {
        let literal = Literal::new_simple_literal(text);
        for predicate in ["text:query", "luc:labels"] {
            let query = format!("SELECT ?s WHERE {{ ?s {predicate} {literal} }}");
            assert_eq!(sorted(rows(&engine, &query, &plain)), expected, "{text}");
        }
    }
}

#[test]
fn engine_text_errors_are_query_arguments_for_every_search_adapter() {
    for engine in [Engine::new(EngineConfig::default()).unwrap(), engine()] {
        for text in [
            "bridge~x",
            "bridge~3",
            "bridge~0.5",
            "bridge~256",
            "bridge~1~2",
            "~1",
            "bridge -tow*",
            "bridge -tower~1",
            "bridge -\"tower bridge\"",
            "bri?ge",
            "bri*dge",
            "bridge**",
            "\"bridge",
        ] {
            let literal = Literal::new_simple_literal(text);
            for body in [
                format!("?l bds:search {literal}"),
                format!("?l bds:search {literal} ; bds:prefixMatch true"),
                format!("?s text:query {literal}"),
                format!("?s luc:labels {literal}"),
            ] {
                assert_search_argument_error(&engine, &body);
            }
        }
    }
}

fn assert_search_argument_error(engine: &Engine, body: &str) {
    let text = format!("{PREFIXES} SELECT * WHERE {{ {body} }}");
    let query = SparqlParser::new().parse_query(&text).unwrap();
    let snapshot = engine.snapshot();
    let options = QueryOptions::default();
    let result = evaluate_query(&snapshot, &query, &options).and_then(|result| {
        if let QueryResults::Solutions(solutions) = result {
            for solution in solutions {
                solution?;
            }
        }
        Ok(())
    });
    assert!(
        matches!(result, Err(QueryEvaluationError::Argument(_))),
        "{text}: {result:?}"
    );
    assert!(
        matches!(
            explain_query(&snapshot, &query, &options),
            Err(QueryEvaluationError::Argument(_))
        ),
        "EXPLAIN: {text}"
    );
}

/// These terminating malformed inputs also provide a safe RED check on the old code.
#[test]
fn jena_malformed_lists_are_query_errors() {
    let engine = engine();
    for links in [
        "_:head rdf:first \"bridge\" .",
        "_:head rdf:rest rdf:nil .",
        "_:head rdf:first \"bridge\" ; rdf:rest _:missing .",
        "_:head rdf:first \"bridge\" ; rdf:rest ex:tail .",
        "_:head rdf:first \"bridge\" ; rdf:rest \"tail\" .",
        "_:head rdf:first \"bridge\" ; rdf:rest ?tail .",
        "_:head rdf:first \"bridge\", \"tower\" ; rdf:rest rdf:nil .",
        "_:head rdf:first \"tower\", \"bridge\" ; rdf:rest rdf:nil .",
        "_:head rdf:first \"bridge\" ; rdf:rest rdf:nil, _:other . _:other rdf:first 1 ; rdf:rest rdf:nil .",
        "_:head rdf:first \"bridge\" ; rdf:rest _:other, rdf:nil . _:other rdf:first 1 ; rdf:rest rdf:nil .",
    ] {
        assert_search_argument_error(&engine, &format!("?s text:query _:head . {links}"));
        assert_search_argument_error(&engine, &format!("_:head text:query \"bridge\" . {links}"));
    }
    for body in [
        "?s text:query () .",
        "() text:query \"bridge\" .",
        "?s text:query (rdfs:label) .",
        "?valid text:query \"bridge\" . ?s text:query _:head . _:head rdf:first \"bridge\" .",
    ] {
        assert_search_argument_error(&engine, body);
    }
}

/// Run cycle coverage on the fixed build; unfixed builds need an external memory/time cap.
#[test]
fn jena_cyclic_lists_are_query_errors() {
    let engine = engine();
    for links in [
        "_:head rdf:first \"bridge\" ; rdf:rest _:head .",
        "_:head rdf:first \"bridge\" ; rdf:rest _:tail . _:tail rdf:first 1 ; rdf:rest _:head .",
        "_:head rdf:first \"bridge\" ; rdf:rest _:tail . _:tail rdf:first 1 ; rdf:rest _:tail .",
    ] {
        assert_search_argument_error(&engine, &format!("?s text:query _:head . {links}"));
        assert_search_argument_error(&engine, &format!("_:head text:query \"bridge\" . {links}"));
    }
}

#[test]
fn jena_valid_lists_keep_duplicate_items_links_and_shared_tails() {
    let engine = engine();
    let expected = table(&[&["bells"], &["bridge"]]);
    for body in [
        // Repeated identical links are all consumed, not left as data joins.
        "?s text:query _:head . _:head rdf:first \"bridge\", \"bridge\" ; rdf:rest rdf:nil, rdf:nil .",
        // Identical items at different cells are not cycles.
        "?s text:query (\"bridge\" 10 10) .",
        // Two independently parsed argument lists may share their tail.
        "?s text:query _:head, _:other . _:head rdf:first \"bridge\" ; rdf:rest _:tail . _:other rdf:first \"bridge\" ; rdf:rest _:tail . _:tail rdf:first 10 ; rdf:rest rdf:nil .",
        // Structural triples may precede the head, and unrelated patterns still join.
        "_:tail rdf:rest rdf:nil ; rdf:first \"bridge\" . _:head rdf:rest _:tail ; rdf:first rdfs:label . ?s text:query _:head . ?s rdfs:label ?label .",
    ] {
        let query = format!("SELECT ?s WHERE {{ {body} }}");
        let found = sorted(rows(&engine, &query, &QueryOptions::default()));
        assert_eq!(found, expected, "{query}");
    }
    // A blank subject without list links remains an ordinary existential subject.
    assert_eq!(
        rows(
            &engine,
            "SELECT (COUNT(*) AS ?n) WHERE { _:subject text:query \"bridge\" }",
            &QueryOptions::default()
        ),
        table(&[&["2"]])
    );
}
