//! Long and deep queries on the server's stack (ADR-0012): parsed, planned, explained and
//! evaluated with the result cache on a thread of 2 MiB (tokio's threads, which run the
//! server's queries), each query answers or is refused with the nesting error; none aborts
//! the process. Sequences (UNION, joined groups, `&&`, `||`, path sequences and
//! alternatives, BIND runs, SELECT lists) are n-ary or balanced, so thousands of them
//! answer; what nests (OPTIONAL, MINUS, LATERAL and a BIND after a triple each wrapping the
//! group before them, groups, subqueries, EXISTS, GRAPH, arithmetic) answers up to the
//! nesting limit and is refused past it.

use std::process::Command;
use std::sync::Arc;

use nrese_engine::{Engine, EngineConfig};
use nrese_rdf::{GraphName, NamedNode, Quad};
use nrese_sparql::{
    QueryOptions, QueryResults, ResultCache, UpdateOptions, apply_update, evaluate_query,
    explain_query, plan_query,
};
use nrese_sparql_syntax::SparqlParser;

/// The stack the server's queries run on (tokio's blocking threads).
const STACK: usize = 2 << 20;

/// Shapes whose length is not depth.
const LONG: [&str; 9] = [
    "union",
    "groups",
    "and",
    "or",
    "sequence",
    "alternative",
    "values",
    "bind",
    "select",
];

/// Shapes whose length is depth.
const NESTED: [&str; 12] = [
    "bind-triples",
    "optional",
    "minus",
    "lateral",
    "groups-nested",
    "optional-nested",
    "union-nested",
    "exists-nested",
    "graph-nested",
    "subquery-nested",
    "parentheses",
    "sum",
];

fn p(i: usize) -> String {
    format!("<http://e/p{}>", i % 7)
}

/// A query of `shape`, `n` long or deep.
fn query(shape: &str, n: usize) -> String {
    let repeat = |f: &dyn Fn(usize) -> String| (0..n).map(f).collect::<String>();
    let joined = |f: &dyn Fn(usize) -> String, by: &str| (0..n).map(f).collect::<Vec<_>>().join(by);
    match shape {
        "union" => format!(
            "SELECT * WHERE {{ {} }}",
            joined(&|i| format!("{{ ?s {} ?o }}", p(i)), " UNION ")
        ),
        "groups" => format!(
            "SELECT ?s WHERE {{ {} }} LIMIT 1",
            repeat(&|i| format!("{{ ?s {} ?o{i} FILTER(?s != <http://e/x{i}>) }} ", p(i)))
        ),
        "and" => format!(
            "SELECT * WHERE {{ ?s ?q ?o FILTER({}) }}",
            joined(&|i| format!("?o != {i}"), " && ")
        ),
        "or" => format!(
            "SELECT * WHERE {{ ?s ?q ?o FILTER({}) }}",
            joined(&|i| format!("?o = {i}"), " || ")
        ),
        // Optional, so that it stays a path: a sequence alone is spelled out as a chain of
        // triple patterns, which is long but not deep.
        "sequence" => format!("SELECT * WHERE {{ ?s ({})? ?o }}", joined(&p, "/")),
        "alternative" => format!("SELECT * WHERE {{ ?s {} ?o }}", joined(&p, "|")),
        "values" => format!(
            "SELECT * WHERE {{ ?s ?q ?o VALUES ?q {{ {} }} }}",
            joined(&p, " ")
        ),
        "bind" => format!(
            "SELECT * WHERE {{ ?s ?q ?o {} }}",
            repeat(&|i| format!("BIND({i} AS ?b{i}) "))
        ),
        "select" => format!(
            "SELECT ?s {} WHERE {{ ?s ?q ?o }}",
            repeat(&|i| format!("(?o AS ?o{i}) "))
        ),
        "bind-triples" => format!(
            "SELECT * WHERE {{ ?s ?q ?o {} }}",
            repeat(&|i| format!("BIND({i} AS ?b{i}) ?s {} ?o{i} . ", p(i)))
        ),
        "optional" => format!(
            "SELECT * WHERE {{ ?s ?q ?o {} }}",
            repeat(&|i| format!("OPTIONAL {{ ?s {} ?o{i} }} ", p(i)))
        ),
        "minus" => format!(
            "SELECT * WHERE {{ ?s ?q ?o {} }}",
            repeat(&|i| format!("MINUS {{ ?s {} ?x{i} }} ", p(i)))
        ),
        "lateral" => format!(
            "SELECT * WHERE {{ ?s ?q ?o {} }}",
            repeat(&|i| format!(
                "LATERAL {{ SELECT ?s ?l{i} WHERE {{ ?s {} ?l{i} }} LIMIT 1 }} ",
                p(i)
            ))
        ),
        "groups-nested" => format!(
            "SELECT * WHERE {} ?s ?q ?o {}",
            "{ ".repeat(n),
            "} ".repeat(n)
        ),
        "optional-nested" => format!(
            "SELECT * WHERE {{ ?s ?q ?o {} }}",
            "OPTIONAL { ?s ?q ?o ".repeat(n) + &"} ".repeat(n)
        ),
        "union-nested" => format!(
            "SELECT * WHERE {{ ?s ?q ?o {} }}",
            "{ ?s ?q ?o } UNION { ?s ?q ?o ".repeat(n) + &"} ".repeat(n)
        ),
        "exists-nested" => format!(
            "SELECT * WHERE {{ ?s ?q ?o {} }}",
            "FILTER EXISTS { ?s ?q ?o ".repeat(n) + &"} ".repeat(n)
        ),
        "graph-nested" => format!(
            "SELECT * WHERE {{ {} ?s ?q ?o {} }}",
            "GRAPH ?g { ".repeat(n),
            "} ".repeat(n)
        ),
        "subquery-nested" => format!(
            "SELECT * WHERE {{ {} ?s ?q ?o {} }}",
            "{ SELECT * WHERE { ".repeat(n),
            "} } ".repeat(n)
        ),
        "parentheses" => format!(
            "SELECT * WHERE {{ ?s ?q ?o FILTER({}?o{}) }}",
            "(".repeat(n),
            ")".repeat(n)
        ),
        "sum" => format!(
            "SELECT * WHERE {{ ?s ?q ?o BIND(1{} AS ?sum) }}",
            " + 1".repeat(n)
        ),
        other => panic!("no shape {other}"),
    }
}

/// Five subjects, each with an edge of each of seven predicates.
fn engine() -> Engine {
    let engine = Engine::new(EngineConfig::default()).unwrap();
    let mut tx = engine.transaction();
    let node = |name: String| NamedNode::new_unchecked(format!("http://e/{name}"));
    for s in 0..5 {
        for i in 0..7 {
            tx.insert(
                Quad::new(
                    node(format!("s{s}")),
                    node(format!("p{i}")),
                    node(format!("s{}", (s + i) % 5)),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            );
        }
    }
    tx.commit().unwrap();
    engine
}

/// `f` on a thread of `stack` bytes. A stack overflow aborts the process.
fn on_stack<T: Send>(stack: usize, f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(stack)
            .spawn_scoped(scope, f)
            .expect("a thread")
            .join()
            .expect("no panic")
    })
}

/// What `text` came to through `parser`, the planner, EXPLAIN and evaluation with the
/// result cache: `Ok` the rows it answered, `Err` the syntax error that refused it.
fn run(engine: &Engine, parser: &SparqlParser, text: &str) -> Result<usize, String> {
    let query = parser.parse_query(text).map_err(|e| e.to_string())?;
    let snapshot = engine.snapshot();
    let options = QueryOptions {
        result_cache: Some(Arc::new(ResultCache::new(1 << 20))),
        ..QueryOptions::default()
    };
    plan_query(&snapshot, &query, &options).expect("planned");
    explain_query(&snapshot, &query, &options).expect("explained");
    match evaluate_query(&snapshot, &query, &options).expect("evaluated") {
        QueryResults::Solutions(solutions) => {
            let mut rows = 0;
            for solution in solutions {
                solution.expect("a row");
                rows += 1;
            }
            Ok(rows)
        }
        _ => Ok(0),
    }
}

/// The deepest `shape` the parser accepts (the limit, as near as the shape's steps allow).
fn deepest(shape: &str) -> usize {
    let parser = SparqlParser::new();
    let (mut accepted, mut refused) = (0, 1);
    while parser.parse_query(&query(shape, refused)).is_ok() {
        (accepted, refused) = (refused, refused * 2);
    }
    while refused - accepted > 1 {
        let middle = (accepted + refused) / 2;
        match parser.parse_query(&query(shape, middle)) {
            Ok(_) => accepted = middle,
            Err(_) => refused = middle,
        }
    }
    accepted
}

#[test]
fn long_queries_answer_and_deep_ones_up_to_the_limit() {
    let engine = engine();
    let parser = SparqlParser::new();
    for shape in LONG {
        let answered = on_stack(STACK, || run(&engine, &parser, &query(shape, 5_000)));
        assert!(answered.is_ok(), "{shape} of 5,000: {answered:?}");
    }
    assert_eq!(
        on_stack(STACK, || run(&engine, &parser, &query("union", 5_000))),
        Ok(5_000 * 5),
        "each arm's five rows"
    );
    for shape in NESTED {
        let n = deepest(shape);
        assert!(n >= 8, "{shape}: only {n} deep accepted");
        let at_limit = on_stack(STACK, || run(&engine, &parser, &query(shape, n)));
        assert!(at_limit.is_ok(), "{shape} of {n}: {at_limit:?}");
        let past = on_stack(STACK, || run(&engine, &parser, &query(shape, 5_000)));
        assert!(
            past.as_ref()
                .is_err_and(|e| e.contains("nested deeper than")),
            "{shape} of 5,000: {past:?}"
        );
    }
}

#[test]
fn an_update_of_a_long_union_applies() {
    let engine = engine();
    let text = format!(
        "INSERT {{ ?s <http://e/seen> true }} WHERE {{ {} }}",
        (0..5_000)
            .map(|i| format!("{{ ?s {} ?o }}", p(i)))
            .collect::<Vec<_>>()
            .join(" UNION ")
    );
    on_stack(STACK, || {
        let update = SparqlParser::new().parse_update(&text).expect("parsed");
        let mut tx = engine.transaction();
        apply_update(&mut tx, &update, &UpdateOptions::default()).expect("applied");
        tx.commit().expect("committed");
    });
    assert_eq!(
        run(
            &engine,
            &SparqlParser::new(),
            "SELECT * WHERE { ?s <http://e/seen> true }"
        ),
        Ok(5)
    );
}

/// The probe's child: `NRESE_STACK_PROBE=shape,n,bytes` runs that query with no nesting
/// limit on a thread of that stack. It returns, or the overflow aborts it.
#[test]
#[ignore = "run by stack_per_level"]
fn stack_probe() {
    let Ok(spec) = std::env::var("NRESE_STACK_PROBE") else {
        return;
    };
    let [shape, n, bytes] = spec.split(',').collect::<Vec<_>>()[..] else {
        panic!("shape,n,bytes: {spec}");
    };
    let (n, bytes) = (n.parse().unwrap(), bytes.parse().unwrap());
    let engine = engine();
    let parser = SparqlParser::new().with_max_nesting(usize::MAX);
    on_stack(bytes, || run(&engine, &parser, &query(shape, n))).expect("answered");
}

/// The stack a query takes, by shape and length, and so the bytes a level of nesting
/// takes in the deepest pass: what the nesting limit is calibrated by (ADR-0012). Each
/// measurement is the smallest stack (to 1 KiB) that [`stack_probe`] answers on, in a
/// process of its own. Run in the profile to calibrate, e.g.
/// `cargo test --release -p nrese-sparql --test it depth_tests::stack_per_level -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement"]
fn stack_per_level() {
    let fits = |shape: &str, n: usize, bytes: usize| {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "depth_tests::stack_probe", "--ignored", "-q"])
            .env("NRESE_STACK_PROBE", format!("{shape},{n},{bytes}"))
            .output()
            .unwrap()
            .status
            .success()
    };
    let smallest = |shape: &str, n: usize| {
        let (mut low, mut high) = (16 << 10, 64 << 10);
        while !fits(shape, n, high) {
            (low, high) = (high, high * 2);
            assert!(high <= 1 << 30, "{shape} of {n} needs over 1 GiB");
        }
        while high - low > 1 << 10 {
            let middle = (low + high) / 2;
            if fits(shape, n, middle) {
                high = middle;
            } else {
                low = middle;
            }
        }
        high
    };
    // Nested: at the deepest the parser accepts (n), and at 4 n with the limit lifted.
    println!("n at the limit, KiB at n and at 4 n, bytes per step:");
    for shape in NESTED {
        let n = deepest(shape);
        let (a, b) = (smallest(shape, n), smallest(shape, 4 * n));
        println!(
            "  {shape}: {n} {} {} {}",
            a >> 10,
            b >> 10,
            b.saturating_sub(a) / (3 * n)
        );
    }
    println!("KiB at 500 and 2,000:");
    for shape in LONG {
        println!(
            "  {shape}: {} {}",
            smallest(shape, 500) >> 10,
            smallest(shape, 2_000) >> 10
        );
    }
}
