//! Full-text search with Blazegraph's `bds:` vocabulary (native/search.rs): what matches,
//! relevance and rank, the options, the graph a pattern reads, and literals that the data
//! no longer uses.

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, Literal, NamedNode, Quad, Term};
use nrese_sparql::{GraphAccess, QueryOptions, QueryResults, evaluate_query, explain_query};
use nrese_sparql_syntax::SparqlParser;

const EX: &str = "http://example.com/";
const PREFIXES: &str = "PREFIX bds: <http://www.bigdata.com/rdf/search#> \
                        PREFIX text: <http://jena.apache.org/text#> \
                        PREFIX luc: <http://www.ontotext.com/owlim/lucene#> \
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

/// `text:query`'s limit counts the literals of its property: a literal of another
/// property that ranks higher ("bridge" three times in three words, BM25) takes none of
/// the limit, so `(rdfs:label "bridge" 1)` finds the one label that matches.
#[test]
fn jena_limit_counts_the_property_literals_only() {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let rdfs = |local: &str| {
        NamedNode::new_unchecked(format!("http://www.w3.org/2000/01/rdf-schema#{local}"))
    };
    for (subject, property, text) in [
        ("note", "comment", "bridge bridge bridge"),
        ("span", "label", "the old bridge over the river"),
    ] {
        tx.insert(
            Quad::new(
                ex(subject),
                rdfs(property),
                Literal::new_simple_literal(text),
                GraphName::DefaultGraph,
            )
            .as_ref(),
        );
    }
    tx.commit().unwrap();
    let plain = QueryOptions::default();
    assert_eq!(
        rows(
            &engine,
            "SELECT ?s WHERE { ?s text:query (rdfs:label \"bridge\" 1) }",
            &plain
        ),
        table(&[&["span"]])
    );
    // Without a property, the comment ranks first and the limit keeps it.
    assert_eq!(
        rows(
            &engine,
            "SELECT ?s WHERE { ?s text:query (\"bridge\" 1) }",
            &plain
        ),
        table(&[&["note"]])
    );
}

/// How long the cyclic lists below may take before the test calls them hung: a bound
/// that contains a walk unable to finish, not a measurement.
const CYCLE_HANG: std::time::Duration = std::time::Duration::from_secs(2);

/// A cyclic RDF list (`_:a rdf:rest _:a`) as `text:query`'s subject or object is not an
/// argument list, as a list without `rdf:nil` isn't: the pattern is read as written and
/// finds nothing here, instead of walking the cycle forever.
#[test]
fn cyclic_text_query_lists_answer() {
    let queries = [
        "SELECT ?s WHERE { ?s text:query _:a . _:a rdf:first \"bridge\" ; rdf:rest _:a }",
        "SELECT ?s WHERE { ?s text:query _:a .          _:a rdf:first rdfs:label ; rdf:rest _:b . _:b rdf:first \"bridge\" ; rdf:rest _:a }",
        "SELECT ?s WHERE { _:l text:query \"bridge\" . _:l rdf:first ?s ; rdf:rest _:l }",
    ];
    // Each list on a thread of its own, so that one that hangs doesn't hide the others.
    let answers: Vec<_> = queries
        .iter()
        .map(|query| {
            let (send, receive) = std::sync::mpsc::channel();
            let query =
                format!("PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> {query}");
            std::thread::spawn(move || {
                let _ = send.send(rows(&engine(), &query, &QueryOptions::default()));
            });
            receive
        })
        .collect();
    let failures: Vec<String> = answers
        .into_iter()
        .enumerate()
        .filter_map(|(case, receive)| match receive.recv_timeout(CYCLE_HANG) {
            Ok(answer) if answer.is_empty() => None,
            Ok(answer) => Some(format!("cyclic list {case}: {answer:?}")),
            Err(e) => Some(format!("cyclic list {case}: no answer ({e})")),
        })
        .collect();
    assert!(failures.is_empty(), "{failures:#?}");
}

/// A search under `GRAPH ?g` matches only literals of the named graphs the query's dataset
/// has (SPARQL 1.1 §13.3, §18.6): those `FROM NAMED` lists, and of those only the graphs its
/// reader may read (`QueryOptions::access`: the others are absent). One row each, every row
/// reported: the reader of every graph (a guard), a reader who may not read `g`, one who
/// may, and `FROM NAMED` naming another graph or `g`.
#[test]
fn a_graph_search_matches_only_the_datasets_named_graphs() {
    let engine = engine();
    let reader = |graphs: &[&str]| QueryOptions {
        access: Some(std::sync::Arc::new(GraphAccess {
            default_graph: true,
            graphs: graphs.iter().map(|g| format!("{EX}{g}")).collect(),
            ..GraphAccess::default()
        })),
        ..QueryOptions::default()
    };
    let search = |from: &str| {
        format!("SELECT ?o {from} WHERE {{ GRAPH ?g {{ ?o bds:search \"bridge\" }} }}")
    };
    let hidden = table(&[&["A bridge in another graph"]]);
    let rows_of = [
        (
            "every graph",
            search(""),
            QueryOptions::default(),
            hidden.clone(),
        ),
        ("g unreadable", search(""), reader(&[]), table(&[])),
        ("g readable", search(""), reader(&["g"]), hidden.clone()),
        (
            "FROM NAMED another graph",
            search("FROM NAMED <http://example.com/other>"),
            QueryOptions::default(),
            table(&[]),
        ),
        (
            "FROM NAMED g",
            search("FROM NAMED <http://example.com/g>"),
            QueryOptions::default(),
            hidden,
        ),
    ];
    let mut failed = Vec::new();
    for (name, query, options, want) in rows_of {
        let got = sorted(rows(&engine, &query, &options));
        if got != want {
            failed.push(format!("{name}: got {got:?}, want {want:?}"));
        }
    }
    assert!(
        failed.is_empty(),
        "{} rows failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// Another store's name for the default graph (`urn:x-arq:DefaultGraph`, rdflib's
/// `urn:x-rdflib:default`; `compat`) reads the store's default graph only for a reader
/// who may read it: for one who may not, the graph is absent from its dataset
/// (SPARQL 1.1 §13.3), as an unreadable named graph is, whatever reads it (a search,
/// `text:query`, a sub-select, nested under `GRAPH ?g`, a vector search). The fixture's
/// default graph alone holds `British Museum`. Rows: two guards (every graph; a reader
/// of the default graph), then a reader of `g` alone through each path. Every row
/// reported.
#[test]
fn default_graph_aliases_respect_reader_scope() {
    let engine = engine();
    let reader = |default_graph: bool| QueryOptions {
        access: Some(std::sync::Arc::new(GraphAccess {
            graphs: vec![format!("{EX}g")],
            default_graph,
            ..GraphAccess::default()
        })),
        ..QueryOptions::default()
    };
    let museum = table(&[&["British Museum"]]);
    let in_alias =
        |alias: &str, inner: &str| format!("SELECT ?o WHERE {{ GRAPH <{alias}> {{ {inner} }} }}");
    let search = "?o bds:search \"museum\"";
    let arq = "urn:x-arq:DefaultGraph";
    let rdflib = "urn:x-rdflib:default";
    let rows_of = [
        (
            "every graph",
            in_alias(arq, search),
            QueryOptions::default(),
            museum.clone(),
        ),
        (
            "reader of the default graph",
            in_alias(arq, search),
            reader(true),
            museum.clone(),
        ),
        (
            "arq alias",
            in_alias(arq, search),
            reader(false),
            table(&[]),
        ),
        (
            "rdflib alias",
            in_alias(rdflib, search),
            reader(false),
            table(&[]),
        ),
        (
            "text:query",
            in_alias(arq, "?s text:query \"museum\" ; rdfs:label ?o"),
            reader(false),
            table(&[]),
        ),
        (
            "sub-select",
            in_alias(arq, &format!("{{ SELECT ?o WHERE {{ {search} }} }}")),
            reader(false),
            table(&[]),
        ),
        (
            "under GRAPH ?g",
            format!("SELECT ?o WHERE {{ GRAPH ?g {{ GRAPH <{arq}> {{ {search} }} }} }}"),
            reader(false),
            table(&[]),
        ),
    ];
    let mut failed = Vec::new();
    for (name, query, options, want) in rows_of {
        let got = sorted(rows(&engine, &query, &options));
        if got != want {
            failed.push(format!("{name}: got {got:?}, want {want:?}"));
        }
    }
    // A vector search under the alias: the default graph's vector, for a reader of nothing.
    let vectors = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = vectors.transaction();
    let vector = Literal::new_typed_literal(
        "[1, 0]",
        NamedNode::new_unchecked(nrese_engine::vector::DATATYPE),
    );
    tx.insert(Quad::new(ex("s"), ex("p"), vector, GraphName::DefaultGraph).as_ref());
    tx.commit().unwrap();
    let nobody = QueryOptions {
        access: Some(std::sync::Arc::new(GraphAccess::default())),
        ..QueryOptions::default()
    };
    let near = format!(
        "SELECT ?v WHERE {{ GRAPH <{arq}> {{ SERVICE <urn:nrese:vector:search> {{ \
         ?v <urn:nrese:vector:near> \"[1, 0]\"^^<{}> ; <urn:nrese:vector:k> 1 ; \
         <urn:nrese:vector:exact> true }} }} }}",
        nrese_engine::vector::DATATYPE
    );
    for (name, options, want) in [
        ("vector, every graph", QueryOptions::default(), 1),
        ("vector, a reader of nothing", nobody, 0),
    ] {
        let got = rows(&vectors, &near, &options).len();
        if got != want {
            failed.push(format!("{name}: got {got} rows, want {want}"));
        }
    }
    assert!(
        failed.is_empty(),
        "{} rows failed:\n{}",
        failed.len(),
        failed.join("\n")
    );
}

/// An engine with these Turtle statements.
fn engine_of(turtle: &str) -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    for quad in nrese_rdf_io::RdfParser::from_format(nrese_rdf_io::RdfFormat::Turtle)
        .for_reader(turtle.as_bytes())
    {
        tx.insert(quad.unwrap().as_ref());
    }
    tx.commit().unwrap();
    engine
}

/// The query's answers, optimised and as written; each must be `want`.
fn answers_as_written(engine: &Engine, shape: &str, query: &str, want: &[&[&str]]) {
    answers_as_written_with(engine, shape, query, QueryOptions::default(), want);
}

/// [`answers_as_written`] with these options besides.
fn answers_as_written_with(
    engine: &Engine,
    shape: &str,
    query: &str,
    mut options: QueryOptions,
    want: &[&[&str]],
) {
    for written in [false, true] {
        options.as_written = written;
        let got = sorted(rows(engine, query, &options));
        assert_eq!(got, table(want), "{shape}, as_written={written}: {query}");
    }
}

/// Spec: a search is read within the group it is written in (`search`), so a query
/// holding one gives the answers its groups give as written: a `bds:` option in another
/// group is a statement no data has, and a Jena call without a literal is read as
/// statements. Each shape below is one the planner could otherwise rewrite.
#[test]
fn a_query_holding_a_search_answers_as_its_groups_are_written() {
    let engine = engine_of(
        "@prefix ex: <http://example.com/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:a rdfs:label \"alpha one\" . ex:b rdfs:label \"alpha two\" .
         ex:t <http://jena.apache.org/text#query> ex:o . ex:m ex:p ex:n .",
    );
    let none: &[&[&str]] = &[];
    for (shape, query, want) in [
        (
            "groups across a VALUES, an option apart",
            "SELECT ?o ?r { { ?o bds:search \"alpha\" } VALUES ?x { 1 } { ?o bds:relevance ?r } }",
            none,
        ),
        (
            "groups across a VALUES, a Jena call apart",
            "SELECT ?s ?t { { ?s text:query \"alpha\" } VALUES ?x { 1 } { ?t text:query [] } }",
            &[&["a", "t"], &["b", "t"]],
        ),
        (
            "an ordered subquery first, an option apart",
            "SELECT ?o ?r { { SELECT ?k { VALUES ?k { 1 } } ORDER BY ?k } \
             { ?o bds:search \"alpha\" } { ?o bds:relevance ?r } }",
            none,
        ),
        (
            "a SERVICE between the groups",
            "SELECT ?o ?r { { ?o bds:search \"alpha\" } SERVICE SILENT ?e { ?a ?b ?c } \
             { ?o bds:relevance ?r } }",
            none,
        ),
        (
            "a path between the groups",
            "SELECT ?s ?t ?x ?y { { ?s text:query \"alpha\" } ?x ex:p+ ?y . { ?t text:query [] } }",
            &[&["a", "t", "m", "n"], &["b", "t", "m", "n"]],
        ),
        (
            "a path from the search's subject",
            "SELECT ?s ?t ?x { { ?s text:query \"alpha\" } ?s ex:p* ?x . { ?t text:query [] } }",
            &[&["a", "t", "a"], &["b", "t", "b"]],
        ),
    ] {
        answers_as_written(&engine, shape, query, want);
    }
    // One pattern whose second Jena call has no literal, so it is read as statements:
    // no statement has `text:query "alpha"`, so there is no group to count.
    let grouped = engine_of(
        "@prefix ex: <http://example.com/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:a rdfs:label \"alpha one\" ; ex:p ex:u .
         ex:u <http://jena.apache.org/text#query> \"x1\", \"x2\", \"x3\", \"x4\", \"x5\",
             \"x6\", \"x7\", \"x8\", \"x9\", \"x10\" .",
    );
    let query = "SELECT ?s (COUNT(?v) AS ?n) WHERE { ?s text:query \"alpha\" . ?s ex:p ?t . \
                 ?t text:query ?v } GROUP BY ?s";
    answers_as_written(&grouped, "a grouped pattern", query, none);
}

/// Spec: a query holding a search is evaluated as written, and a query without one keeps
/// its plan: here join-groups still makes one pattern of two plain groups.
#[test]
fn only_a_query_holding_a_search_is_evaluated_as_written() {
    let engine = engine();
    let rewrites = |query: &str| {
        let text = format!("{PREFIXES}{query}");
        let query = SparqlParser::new().parse_query(&text).unwrap();
        explain_query(&engine.snapshot(), &query, &QueryOptions::default())
            .unwrap()
            .rewrites
    };
    let plain = "SELECT * { { ?s rdfs:label ?o } VALUES ?x { 1 } { ?s ex:p ?u } }";
    assert_eq!(rewrites(plain), ["join-groups"], "{plain}");
    let search = "SELECT * { { ?o bds:search \"alpha\" } VALUES ?x { 1 } { ?s rdfs:label ?o } }";
    assert!(rewrites(search).is_empty(), "{search}");
}

/// One label, "alpha one", and no `bds:` statement.
fn alpha_engine() -> Engine {
    engine_of(
        "@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         <http://example.com/a> rdfs:label \"alpha one\" .",
    )
}

/// The number of `query`'s answers.
fn count(engine: &Engine, query: &nrese_sparql_syntax::Query, options: &QueryOptions) -> usize {
    match evaluate_query(&engine.snapshot(), query, options).unwrap() {
        QueryResults::Solutions(solutions) => solutions.count(),
        _ => panic!("solutions"),
    }
}

fn parsed(query: &str) -> nrese_sparql_syntax::Query {
    let text = format!("{PREFIXES}{query}");
    SparqlParser::new()
        .parse_query(&text)
        .unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// Spec: a search whose literal `pre_bound` puts in is read within its group as a written
/// one is: the `bds:` option in the other group is a statement no data has, so no answer.
#[test]
fn a_pre_bound_search_keeps_its_groups() {
    let values = std::collections::HashMap::from([(
        nrese_rdf::Variable::new_unchecked("q"),
        Term::from(Literal::new_simple_literal("alpha")),
    )]);
    let options = QueryOptions {
        pre_bound: Some(std::sync::Arc::new(values)),
        ..QueryOptions::default()
    };
    let query = "SELECT ?o ?r { { ?o bds:search ?q } VALUES ?x { 1 } { ?o bds:relevance ?r } }";
    let found = count(&alpha_engine(), &parsed(query), &options);
    assert_eq!(found, 0, "{query}");
}

/// Spec: a search whose literal a `LATERAL`'s binding puts in is read within its group as
/// a written one is: the `bds:` option in the other group matches nothing, so no answer.
#[test]
fn a_search_bound_by_a_lateral_keeps_its_groups() {
    let query = "SELECT ?o ?r { VALUES ?q { \"alpha\" } \
                 LATERAL { { ?o bds:search ?q } VALUES ?x { 1 } { ?o bds:relevance ?r } } }";
    let options = QueryOptions::default();
    let found = count(&alpha_engine(), &parsed(query), &options);
    assert_eq!(found, 0, "{query}");
}

/// Spec: a SERVICE beside one local input keeps that input's groups apart: the `bds:`
/// option in the second group matches nothing, so no answer. No text writes this input,
/// a join of two groups (the parser makes one group of `{ {A} {B} }`); the algebra does.
#[test]
fn a_service_beside_one_joined_input_keeps_its_groups() {
    use nrese_sparql_syntax::algebra::GraphPattern;
    let mut query = parsed(
        "SELECT ?o ?r { { ?o bds:search \"alpha\" } VALUES ?x { 1 } { ?o bds:relevance ?r } \
         SERVICE SILENT ?e { ?a ?b ?c } }",
    );
    let nrese_sparql_syntax::Query::Select { pattern, .. } = &mut query else {
        panic!("a SELECT")
    };
    let GraphPattern::Project { inner, .. } = pattern else {
        panic!("a projection: {pattern:?}")
    };
    let GraphPattern::Join { inputs } = &mut **inner else {
        panic!("a join: {inner:?}")
    };
    let groups: Vec<GraphPattern> = inputs
        .iter()
        .filter(|input| matches!(input, GraphPattern::Bgp { .. }))
        .cloned()
        .collect();
    let service = inputs.pop().expect("the SERVICE last");
    assert!(matches!(service, GraphPattern::Service { .. }) && groups.len() == 2);
    *inputs = vec![GraphPattern::Join { inputs: groups }, service];
    let options = QueryOptions::default();
    assert_eq!(count(&alpha_engine(), &query, &options), 0, "{query:?}");
}

/// Spec: a search whose predicates a `VALUES` binds before a `LATERAL` is read within its
/// group as a written one is: the `bds:` option in the other group matches nothing, and the
/// Jena call without a literal is read as the one statement stored under `text:query`.
#[test]
fn a_search_whose_predicates_a_values_binds_keeps_its_groups() {
    let engine = engine_of(
        "@prefix ex: <http://example.com/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:a rdfs:label \"alpha one\" . ex:b rdfs:label \"alpha two\" .
         ex:t <http://jena.apache.org/text#query> ex:o .",
    );
    let option = "SELECT ?o ?r { VALUES (?p ?q) { (bds:search bds:relevance) } \
                  LATERAL { { ?o ?p \"alpha\" } VALUES ?x { 1 } { ?o ?q ?r } } }";
    answers_as_written(&engine, "an option apart", option, &[]);
    let jena = "SELECT ?s ?t { VALUES ?p { text:query } \
                LATERAL { { ?s ?p \"alpha\" } VALUES ?x { 1 } { ?t ?p [] } } }";
    let want: &[&[&str]] = &[&["a", "t"], &["b", "t"]];
    answers_as_written(&engine, "a Jena call apart", jena, want);
}

/// Two labels with "alpha", one statement stored under `text:query`, and statements that
/// name the search predicates as data.
fn named_in_data_engine() -> Engine {
    engine_of(
        "@prefix ex: <http://example.com/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         @prefix bds: <http://www.bigdata.com/rdf/search#> .
         ex:a rdfs:label \"alpha one\" . ex:b rdfs:label \"alpha two\" .
         ex:t <http://jena.apache.org/text#query> ex:o .
         ex:cfg ex:search bds:search ; ex:option bds:relevance .
         ex:jena ex:search <http://jena.apache.org/text#query> .",
    )
}

/// Spec: a search whose predicates are read from the data before a `LATERAL` is read within
/// its group as a written one is: the `bds:` option in the other group matches nothing.
#[test]
fn a_search_whose_predicates_the_data_names_keeps_its_groups() {
    let query = "SELECT ?o ?r { ex:cfg ex:search ?p ; ex:option ?q . \
                 LATERAL { { ?o ?p \"alpha\" } VALUES ?x { 1 } { ?o ?q ?r } } }";
    answers_as_written(&named_in_data_engine(), "an option apart", query, &[]);
}

/// Spec: a Jena search whose predicate is read from the data before a `LATERAL` is read
/// within its group: the call without a literal in the other group is the stored statement.
#[test]
fn a_jena_search_whose_predicate_the_data_names_keeps_its_groups() {
    let query = "SELECT ?s ?t { ex:jena ex:search ?p . \
                 LATERAL { { ?s ?p \"alpha\" } VALUES ?x { 1 } { ?t ?p [] } } }";
    let want: &[&[&str]] = &[&["a", "t"], &["b", "t"]];
    answers_as_written(&named_in_data_engine(), "a Jena call apart", query, want);
}

/// Spec: a search whose predicates are built by an expression before a `LATERAL`, with no
/// search IRI written anywhere, is read within its group: the option apart matches nothing.
#[test]
fn a_search_whose_predicates_an_expression_builds_keeps_its_groups() {
    let query = "SELECT ?o ?r { \
                 BIND(IRI(CONCAT(\"http://www.bigdata.com/rdf/search\", \"#search\")) AS ?p) \
                 BIND(IRI(CONCAT(\"http://www.bigdata.com/rdf/search\", \"#relevance\")) AS ?q) \
                 LATERAL { { ?o ?p \"alpha\" } VALUES ?x { 1 } { ?o ?q ?r } } }";
    answers_as_written(&named_in_data_engine(), "an option apart", query, &[]);
}

/// `QueryOptions` with OWL 2 QL rewriting on.
fn ql_on() -> QueryOptions {
    use nrese_sparql::ql::{Closure, QlRewriting};
    QueryOptions {
        ql: Some(std::sync::Arc::new(QlRewriting::new(Closure {
            lists: true,
        }))),
        ..QueryOptions::default()
    }
}

/// `text:query` is an `ex:C`, and every `ex:C` has an `ex:r` successor; `a` and `b` are
/// labelled "alpha one" and "alpha two", and `c`, labelled "gamma", has one statement
/// under `text:query` itself.
fn ql_engine() -> Engine {
    engine_of(
        "@prefix ex: <http://example.com/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         @prefix owl: <http://www.w3.org/2002/07/owl#> .
         ex:a rdfs:label \"alpha one\" . ex:b rdfs:label \"alpha two\" . ex:c rdfs:label \"gamma\" .
         ex:c <http://jena.apache.org/text#query> \"alpha\" .
         ex:C rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:r ; owl:someValuesFrom owl:Thing ] .
         <http://jena.apache.org/text#query> a ex:C .",
    )
}

/// Spec: a search is what the query writes, never what OWL 2 QL rewriting makes. The
/// shared `ex:r` successor makes `?p` `text:query`, and the triple with it still reads the
/// stored statements, as a joined variable predicate does: `c`'s one, not the search's
/// hits `a` and `b`. A search the query writes stays one under the rewriting: "one" is
/// `a`'s.
#[test]
fn a_predicate_ql_rewriting_binds_to_a_search_iri_reads_stored_statements() {
    let engine = ql_engine();
    let query = "SELECT DISTINCT ?s ?p { text:query ex:r ?anon . ?p ex:r ?anon . \
                 ?s ?p \"alpha\" . ?s rdfs:label ?l }";
    let jena = "http://jena.apache.org/text#query";
    answers_as_written_with(
        &engine,
        "bound by the rewriting",
        query,
        ql_on(),
        &[&["c", jena]],
    );
    let written = "SELECT ?s { ?s text:query \"one\" . ?s rdfs:label ?l }";
    answers_as_written_with(&engine, "written", written, ql_on(), &[&["a"]]);
}

/// An engine whose `bds:search` and `ex:find`, and `bds:relevance` and `ex:score`, are each
/// one `owl:sameAs` class, the search vocabulary's IRIs the representatives if
/// `search_first` (interned first), else the `ex:` ones. `a` and `b` are labelled
/// "alpha one" and "alpha two", and `c`'s statement `ex:find "gamma"` is held over the
/// class's representative, as a reasoner by representatives leaves it.
fn equal_names_engine(search_first: bool) -> Engine {
    use nrese_engine::EncodedTriple;
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let mut id = |iri: &str| tx.intern(NamedNode::new_unchecked(iri).as_ref().into());
    let (search, relevance, find, score) = if search_first {
        let [s, r, f, c] = [
            "http://www.bigdata.com/rdf/search#search",
            "http://www.bigdata.com/rdf/search#relevance",
            "http://example.com/find",
            "http://example.com/score",
        ]
        .map(&mut id);
        (s, r, f, c)
    } else {
        let [f, c, s, r] = [
            "http://example.com/find",
            "http://example.com/score",
            "http://www.bigdata.com/rdf/search#search",
            "http://www.bigdata.com/rdf/search#relevance",
        ]
        .map(&mut id);
        (s, r, f, c)
    };
    let same_as = id("http://www.w3.org/2002/07/owl#sameAs");
    tx.insert_inferred(EncodedTriple::new(find, same_as, search));
    tx.insert_inferred(EncodedTriple::new(score, same_as, relevance));
    let label = NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label");
    for (subject, text) in [("a", "alpha one"), ("b", "alpha two")] {
        let text = Literal::new_simple_literal(text);
        tx.insert(Quad::new(ex(subject), label.clone(), text, GraphName::DefaultGraph).as_ref());
    }
    let c = tx.intern(ex("c").as_ref().into());
    let gamma = tx.intern(Literal::new_simple_literal("gamma").as_ref().into());
    let representative = if search_first { search } else { find };
    tx.insert_inferred(EncodedTriple::new(c, representative, gamma));
    tx.commit().unwrap();
    engine.transaction().install(Some(same_as), None);
    engine
}

fn canonical() -> QueryOptions {
    QueryOptions {
        equality_canonical: true,
        ..QueryOptions::default()
    }
}

/// Spec: a predicate whose class's representative is a search predicate is not a search
/// under canonical answers: `ex:find` reads the stored statements of its class, alone (`c`)
/// and beside an option in another group (no `ex:score` statement, so no answer).
#[test]
fn an_alias_of_a_search_predicate_reads_stored_statements() {
    let engine = equal_names_engine(true);
    let alone = "SELECT ?o { ?o ex:find \"gamma\" }";
    answers_as_written_with(&engine, "alone", alone, canonical(), &[&["c"]]);
    let apart = "SELECT ?o ?r { { ?o ex:find \"gamma\" } VALUES ?x { 1 } { ?o ex:score ?r } }";
    answers_as_written_with(&engine, "an option apart", apart, canonical(), &[]);
}

/// Spec: a search written by its own IRI stays a search under canonical answers, whatever
/// its class's representative: here `ex:find`. Its two hits.
#[test]
fn a_written_search_stays_one_under_canonical_names() {
    let engine = equal_names_engine(false);
    let query = "SELECT ?o { ?o bds:search \"alpha\" }";
    let hits: &[&[&str]] = &[&["alpha one"], &["alpha two"]];
    answers_as_written_with(
        &engine,
        "its representative ex:find",
        query,
        canonical(),
        hits,
    );
}

/// Spec: under canonical answers, a search the request puts in stays a search, and the
/// pre-bound value is its class's elsewhere. `?p` is pre-bound to `bds:search`, whose class's
/// representative is `ex:find`: `?o ?p "alpha"` is the search (its two hits), and
/// `sameTerm(?p, ex:find)` holds in a filter, alone (`a` and `b`, by their labels) and beside
/// the search.
#[test]
fn a_pre_bound_search_is_one_and_its_class_s_name_elsewhere() {
    let engine = equal_names_engine(false);
    let search = NamedNode::new_unchecked("http://www.bigdata.com/rdf/search#search");
    let values = std::collections::HashMap::from([(
        nrese_rdf::Variable::new_unchecked("p"),
        Term::from(search),
    )]);
    let options = QueryOptions {
        pre_bound: Some(std::sync::Arc::new(values)),
        ..canonical()
    };
    let hits: &[&[&str]] = &[&["alpha one"], &["alpha two"]];
    for (shape, query, want) in [
        ("the search", "SELECT ?o { ?o ?p \"alpha\" }", hits),
        (
            "its name in a filter",
            "SELECT ?s { ?s rdfs:label ?l FILTER(sameTerm(?p, ex:find)) }",
            &[&["a"], &["b"]],
        ),
        (
            "both",
            "SELECT ?o { ?o ?p \"alpha\" FILTER(sameTerm(?p, ex:find)) }",
            hits,
        ),
    ] {
        answers_as_written_with(&engine, shape, query, options.clone(), want);
    }
}

/// Spec: under canonical answers, a pre-bound blank node whose class's representative is
/// `bds:search` is not a search, as an IRI alias is not: `?s ?p "alpha"` reads stored
/// statements (none has that object), `?s ?p "stored"` reads the class's one (`c`), and in a
/// filter the value is its class's name (`a`, by its label).
#[test]
fn a_pre_bound_blank_node_a_search_names_reads_stored_statements() {
    use nrese_engine::EncodedTriple;
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let search = NamedNode::new_unchecked("http://www.bigdata.com/rdf/search#search");
    let node = nrese_rdf::BlankNode::new("search-alias").unwrap();
    let representative = tx.intern(search.as_ref().into());
    let alias = tx.intern(node.as_ref().into());
    let same_as = NamedNode::new_unchecked("http://www.w3.org/2002/07/owl#sameAs");
    let same_as = tx.intern(same_as.as_ref().into());
    tx.insert_inferred(EncodedTriple::new(alias, same_as, representative));
    let label = NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#label");
    let one = Literal::new_simple_literal("alpha one");
    tx.insert(Quad::new(ex("a"), label, one, GraphName::DefaultGraph).as_ref());
    let stored = Literal::new_simple_literal("stored");
    tx.insert(Quad::new(ex("c"), search, stored, GraphName::DefaultGraph).as_ref());
    tx.commit().unwrap();
    engine.transaction().install(Some(same_as), None);
    let values = std::collections::HashMap::from([(
        nrese_rdf::Variable::new_unchecked("p"),
        Term::from(node),
    )]);
    let options = QueryOptions {
        pre_bound: Some(std::sync::Arc::new(values)),
        ..canonical()
    };
    for (shape, query, want) in [
        ("no search", "SELECT ?s { ?s ?p \"alpha\" }", &[][..]),
        (
            "its stored statement",
            "SELECT ?s { ?s ?p \"stored\" }",
            &[&["c"][..]][..],
        ),
        (
            "its name in a filter",
            "SELECT ?s { ?s rdfs:label ?l FILTER(sameTerm(?p, bds:search)) }",
            &[&["a"][..]][..],
        ),
    ] {
        answers_as_written_with(&engine, shape, query, options.clone(), want);
    }
}
