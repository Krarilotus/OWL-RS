//! Throughput of nrese's RDF bundle against the Oxigraph crates, on the same inputs.
//!
//! Each case runs both implementations over the same input list, `ROUNDS` times, and
//! reports the median time per item. Inputs are generated from fixed seeds, and each
//! result is folded into a checksum so the optimiser can't drop the work.
//!
//! `cargo run --release --bin bench [filter]`

use std::hint::black_box;
use std::time::Instant;

use oxigraph_comparison::{decimal_lexical, iri_reference, numeric_lexical, rng, temporal_lexical};

const ROUNDS: usize = 9;

/// The median nanoseconds per item of `work` over `items` items.
fn measure(items: usize, mut work: impl FnMut() -> usize) -> f64 {
    let mut times: Vec<f64> = (0..ROUNDS)
        .map(|_| {
            let start = Instant::now();
            black_box(work());
            start.elapsed().as_nanos() as f64 / items as f64
        })
        .collect();
    times.sort_by(f64::total_cmp);
    times[ROUNDS / 2]
}

struct Report {
    filter: Option<String>,
    rows: Vec<(String, f64, f64)>,
}

impl Report {
    fn case(
        &mut self,
        name: &str,
        items: usize,
        ours: impl FnMut() -> usize,
        theirs: impl FnMut() -> usize,
    ) {
        if self
            .filter
            .as_ref()
            .is_some_and(|f| !name.contains(f.as_str()))
        {
            return;
        }
        let (a, b) = (measure(items, ours), measure(items, theirs));
        println!(
            "{name:<44} nrese {a:>9.1} ns   oxigraph {b:>9.1} ns   ratio {:>5.2}",
            a / b
        );
        self.rows.push((name.to_owned(), a, b));
    }
}

fn main() {
    let mut report = Report {
        filter: std::env::args().nth(1),
        rows: Vec::new(),
    };

    // IRIs.
    let mut r = rng(11);
    let references: Vec<String> = (0..100_000).map(|_| iri_reference(&mut r)).collect();
    let absolute: Vec<String> = (0..100_000)
        .map(|i| format!("http://example.org/data/{}/item{}#part{}", i % 97, i, i % 7))
        .collect();
    report.case(
        "IRI parse (absolute, valid)",
        absolute.len(),
        || {
            absolute
                .iter()
                .filter(|t| nrese_rdf::Iri::parse(t.as_str()).is_ok())
                .count()
        },
        || {
            absolute
                .iter()
                .filter(|t| oxiri::Iri::parse(t.as_str()).is_ok())
                .count()
        },
    );
    report.case(
        "IRI parse (generated references)",
        references.len(),
        || {
            references
                .iter()
                .filter(|t| nrese_rdf::Iri::parse(t.as_str()).is_ok())
                .count()
        },
        || {
            references
                .iter()
                .filter(|t| oxiri::Iri::parse(t.as_str()).is_ok())
                .count()
        },
    );
    let (our_base, their_base) = (
        nrese_rdf::Iri::parse("http://a/b/c/d;p?q").unwrap(),
        oxiri::Iri::parse("http://a/b/c/d;p?q").unwrap(),
    );
    let relative: Vec<String> = (0..100_000)
        .map(|i| format!("../x{}/y{}#f", i % 13, i))
        .collect();
    // The shapes Turtle documents use most: a name, a fragment, a path under the base.
    let plain: Vec<String> = (0..100_000)
        .map(|i| match i % 3 {
            0 => format!("item{i}"),
            1 => format!("#part{i}"),
            _ => format!("data/{}/x{i}", i % 7),
        })
        .collect();
    report.case(
        "IRI resolve (names, fragments, sub-paths)",
        plain.len(),
        || {
            plain
                .iter()
                .filter_map(|t| our_base.resolve(t).ok())
                .map(|i| i.as_str().len())
                .sum()
        },
        || {
            plain
                .iter()
                .filter_map(|t| their_base.resolve(t).ok())
                .map(|i| i.as_str().len())
                .sum()
        },
    );
    report.case(
        "IRI resolve (relative)",
        relative.len(),
        || {
            relative
                .iter()
                .filter_map(|t| our_base.resolve(t).ok())
                .map(|i| i.as_str().len())
                .sum()
        },
        || {
            relative
                .iter()
                .filter_map(|t| their_base.resolve(t).ok())
                .map(|i| i.as_str().len())
                .sum()
        },
    );
    let mut buffer = String::new();
    report.case(
        "IRI resolve into a buffer (nrese) / resolve",
        relative.len(),
        || {
            relative
                .iter()
                .map(|t| {
                    our_base
                        .resolve_into(t, &mut buffer)
                        .map_or(0, |()| buffer.len())
                })
                .sum()
        },
        || {
            relative
                .iter()
                .filter_map(|t| their_base.resolve(t).ok())
                .map(|i| i.as_str().len())
                .sum()
        },
    );

    // XSD parsing.
    let mut r = rng(12);
    let decimals: Vec<String> = (0..200_000).map(|_| decimal_lexical(&mut r)).collect();
    let numerics: Vec<String> = (0..200_000).map(|_| numeric_lexical(&mut r)).collect();
    let temporals: Vec<String> = (0..200_000).map(|_| temporal_lexical(&mut r)).collect();
    report.case(
        "xsd:decimal parse",
        decimals.len(),
        || {
            decimals
                .iter()
                .filter(|t| t.parse::<nrese_xsd::Decimal>().is_ok())
                .count()
        },
        || {
            decimals
                .iter()
                .filter(|t| t.parse::<oxsdatatypes::Decimal>().is_ok())
                .count()
        },
    );
    report.case(
        "xsd:double parse (mixed texts)",
        numerics.len(),
        || {
            numerics
                .iter()
                .filter(|t| t.parse::<nrese_xsd::Double>().is_ok())
                .count()
        },
        || {
            numerics
                .iter()
                .filter(|t| t.parse::<oxsdatatypes::Double>().is_ok())
                .count()
        },
    );
    report.case(
        "xsd:double parse (mixed) vs Rust's f64 parse",
        numerics.len(),
        || {
            numerics
                .iter()
                .filter(|t| t.parse::<nrese_xsd::Double>().is_ok())
                .count()
        },
        || numerics.iter().filter(|t| t.parse::<f64>().is_ok()).count(),
    );
    report.case(
        "xsd:integer parse (mixed texts)",
        numerics.len(),
        || {
            numerics
                .iter()
                .filter(|t| t.parse::<nrese_xsd::Integer>().is_ok())
                .count()
        },
        || {
            numerics
                .iter()
                .filter(|t| t.parse::<oxsdatatypes::Integer>().is_ok())
                .count()
        },
    );
    report.case(
        "xsd:dateTime parse (mixed texts)",
        temporals.len(),
        || {
            temporals
                .iter()
                .filter(|t| t.parse::<nrese_xsd::DateTime>().is_ok())
                .count()
        },
        || {
            temporals
                .iter()
                .filter(|t| t.parse::<oxsdatatypes::DateTime>().is_ok())
                .count()
        },
    );
    report.case(
        "xsd:duration parse (mixed texts)",
        temporals.len(),
        || {
            temporals
                .iter()
                .filter(|t| t.parse::<nrese_xsd::Duration>().is_ok())
                .count()
        },
        || {
            temporals
                .iter()
                .filter(|t| t.parse::<oxsdatatypes::Duration>().is_ok())
                .count()
        },
    );

    // XSD values: formatting and arithmetic.
    let ours: Vec<nrese_xsd::Decimal> = decimals.iter().map(|t| t.parse().unwrap()).collect();
    let theirs: Vec<oxsdatatypes::Decimal> = decimals.iter().map(|t| t.parse().unwrap()).collect();
    report.case(
        "xsd:decimal to string",
        ours.len(),
        || ours.iter().map(|d| d.to_string().len()).sum(),
        || theirs.iter().map(|d| d.to_string().len()).sum(),
    );
    report.case(
        "xsd:decimal add",
        ours.len(),
        || {
            ours.windows(2)
                .filter_map(|w| w[0].checked_add(w[1]))
                .count()
        },
        || {
            theirs
                .windows(2)
                .filter_map(|w| w[0].checked_add(w[1]))
                .count()
        },
    );
    report.case(
        "xsd:decimal multiply",
        ours.len(),
        || {
            ours.windows(2)
                .filter_map(|w| w[0].checked_mul(w[1]))
                .count()
        },
        || {
            theirs
                .windows(2)
                .filter_map(|w| w[0].checked_mul(w[1]))
                .count()
        },
    );
    report.case(
        "xsd:decimal divide",
        ours.len(),
        || {
            ours.windows(2)
                .filter_map(|w| w[0].checked_div(w[1]))
                .count()
        },
        || {
            theirs
                .windows(2)
                .filter_map(|w| w[0].checked_div(w[1]))
                .count()
        },
    );
    report.case(
        "xsd:decimal to double",
        ours.len(),
        || {
            ours.iter()
                .map(|d| f64::from(nrese_xsd::Double::from(*d)) as usize)
                .sum()
        },
        || {
            theirs
                .iter()
                .map(|d| f64::from(oxsdatatypes::Double::from(*d)) as usize)
                .sum()
        },
    );
    let our_dates: Vec<nrese_xsd::DateTime> =
        temporals.iter().filter_map(|t| t.parse().ok()).collect();
    let their_dates: Vec<oxsdatatypes::DateTime> =
        temporals.iter().filter_map(|t| t.parse().ok()).collect();
    report.case(
        "xsd:dateTime compare (mixed timezones)",
        our_dates.len(),
        || our_dates.windows(2).filter(|w| w[0] < w[1]).count(),
        || their_dates.windows(2).filter(|w| w[0] < w[1]).count(),
    );
    let our_zoned: Vec<nrese_xsd::DateTime> = our_dates
        .iter()
        .copied()
        .filter(|d| d.timezone_offset().is_some())
        .collect();
    let their_zoned: Vec<oxsdatatypes::DateTime> = their_dates
        .iter()
        .copied()
        .filter(|d| d.timezone_offset().is_some())
        .collect();
    report.case(
        "xsd:dateTime compare (all with timezones)",
        our_zoned.len(),
        || our_zoned.windows(2).filter(|w| w[0] < w[1]).count(),
        || their_zoned.windows(2).filter(|w| w[0] < w[1]).count(),
    );
    report.case(
        "xsd:dateTime to string",
        our_dates.len(),
        || our_dates.iter().map(|d| d.to_string().len()).sum(),
        || their_dates.iter().map(|d| d.to_string().len()).sum(),
    );

    // Graph canonicalisation: a chain and cycles of blank nodes. On a thread with a large
    // stack: oxrdf's canonicalisation overflows the 1 MiB main-thread stack on the chain.
    let filter = report.filter.clone();
    let rows = std::thread::Builder::new()
        .stack_size(1 << 30)
        .spawn(move || {
            let mut inner = Report {
                filter,
                rows: Vec::new(),
            };
            for (name, size) in [
                ("canonicalise (a chain of 1,000 blank nodes)", 1000),
                ("canonicalise (5 cycles of 8 blank nodes)", 8),
            ] {
                let (ours, theirs) = blank_graphs(name.contains("cycles"), size);
                inner.case(
                    name,
                    1,
                    || {
                        let mut g = ours.clone();
                        g.canonicalize();
                        g.len()
                    },
                    || {
                        let mut g = theirs.clone();
                        g.canonicalize(oxrdf::graph::CanonicalizationAlgorithm::Unstable);
                        g.len()
                    },
                );
            }
            // RDFC-1.0, the W3C algorithm, on the same graphs and on data where each blank
            // node has values of its own (the common case: no ties to break).
            let rdfc = oxrdf::graph::CanonicalizationAlgorithm::Rdfc10 {
                hash_algorithm: oxrdf::graph::CanonicalizationHashAlgorithm::Sha256,
            };
            for (name, graphs) in [
                (
                    "RDFC-1.0 (a chain of 1,000 blank nodes)",
                    blank_graphs(false, 1000),
                ),
                (
                    "RDFC-1.0 (5 cycles of 8 blank nodes)",
                    blank_graphs(true, 8),
                ),
                (
                    "RDFC-1.0 (2,000 distinct blank nodes)",
                    distinct_graphs(2000),
                ),
            ] {
                let (ours, theirs) = graphs;
                inner.case(
                    name,
                    1,
                    || {
                        nrese_rdf::rdfc::Rdfc10::new()
                            .with_work_limit(u64::MAX)
                            .canonicalize(
                                ours.iter()
                                    .map(|t| t.in_graph(nrese_rdf::GraphNameRef::DefaultGraph)),
                            )
                            .unwrap()
                            .quads
                            .len()
                    },
                    || {
                        let mut g = theirs.clone();
                        g.canonicalize(rdfc);
                        g.len()
                    },
                );
            }
            inner.rows
        })
        .unwrap()
        .join()
        .unwrap();
    report.rows.extend(rows);

    let geometric: f64 = report
        .rows
        .iter()
        .map(|(_, a, b)| (a / b).ln())
        .sum::<f64>()
        / report.rows.len().max(1) as f64;
    println!(
        "\ngeometric mean of time ratios (nrese / oxigraph): {:.2}",
        geometric.exp()
    );
}

/// The same graph in both models: `size` blank nodes, each with a name of its own and a
/// link to another, all hanging off one IRI.
fn distinct_graphs(size: usize) -> (nrese_rdf::Graph, oxrdf::Graph) {
    let (root, has, name, knows) = (
        "http://example.org/root",
        "http://example.org/has",
        "http://example.org/name",
        "http://example.org/knows",
    );
    let mut ours = nrese_rdf::Graph::new();
    let mut theirs = oxrdf::Graph::new();
    for i in 0..size {
        let (node, other) = (format!("b{i}"), format!("b{}", (i * 7 + 3) % size));
        let value = format!("person {i}");
        ours.insert(&nrese_rdf::Triple::new(
            nrese_rdf::NamedNode::new_unchecked(root),
            nrese_rdf::NamedNode::new_unchecked(has),
            nrese_rdf::BlankNode::new_unchecked(node.as_str()),
        ));
        ours.insert(&nrese_rdf::Triple::new(
            nrese_rdf::BlankNode::new_unchecked(node.as_str()),
            nrese_rdf::NamedNode::new_unchecked(name),
            nrese_rdf::Literal::new_simple_literal(value.as_str()),
        ));
        ours.insert(&nrese_rdf::Triple::new(
            nrese_rdf::BlankNode::new_unchecked(node.as_str()),
            nrese_rdf::NamedNode::new_unchecked(knows),
            nrese_rdf::BlankNode::new_unchecked(other.as_str()),
        ));
        theirs.insert(&oxrdf::Triple::new(
            oxrdf::NamedNode::new_unchecked(root),
            oxrdf::NamedNode::new_unchecked(has),
            oxrdf::BlankNode::new_unchecked(node.as_str()),
        ));
        theirs.insert(&oxrdf::Triple::new(
            oxrdf::BlankNode::new_unchecked(node.as_str()),
            oxrdf::NamedNode::new_unchecked(name),
            oxrdf::Literal::new_simple_literal(value.as_str()),
        ));
        theirs.insert(&oxrdf::Triple::new(
            oxrdf::BlankNode::new_unchecked(node.as_str()),
            oxrdf::NamedNode::new_unchecked(knows),
            oxrdf::BlankNode::new_unchecked(other.as_str()),
        ));
    }
    (ours, theirs)
}

/// The same blank-node graph in both models: chains (`size` nodes, each with a literal)
/// or five `size`-cycles.
fn blank_graphs(cycles: bool, size: usize) -> (nrese_rdf::Graph, oxrdf::Graph) {
    let mut edges: Vec<(String, String, Option<String>)> = Vec::new();
    if cycles {
        for c in 0..5 {
            for i in 0..size {
                edges.push((
                    format!("c{c}n{i}"),
                    format!("c{c}n{}", (i + 1) % size),
                    None,
                ));
            }
        }
    } else {
        for i in 0..size {
            edges.push((
                format!("n{i}"),
                format!("n{}", i + 1),
                Some(format!("v{}", i % 10)),
            ));
        }
    }
    let p = "http://example.org/p";
    let v = "http://example.org/v";
    let mut ours = nrese_rdf::Graph::new();
    let mut theirs = oxrdf::Graph::new();
    for (a, b, value) in &edges {
        ours.insert(&nrese_rdf::Triple::new(
            nrese_rdf::BlankNode::new_unchecked(a.as_str()),
            nrese_rdf::NamedNode::new_unchecked(p),
            nrese_rdf::BlankNode::new_unchecked(b.as_str()),
        ));
        theirs.insert(&oxrdf::Triple::new(
            oxrdf::BlankNode::new_unchecked(a.as_str()),
            oxrdf::NamedNode::new_unchecked(p),
            oxrdf::BlankNode::new_unchecked(b.as_str()),
        ));
        if let Some(value) = value {
            ours.insert(&nrese_rdf::Triple::new(
                nrese_rdf::BlankNode::new_unchecked(a.as_str()),
                nrese_rdf::NamedNode::new_unchecked(v),
                nrese_rdf::Literal::new_simple_literal(value.as_str()),
            ));
            theirs.insert(&oxrdf::Triple::new(
                oxrdf::BlankNode::new_unchecked(a.as_str()),
                oxrdf::NamedNode::new_unchecked(v),
                oxrdf::Literal::new_simple_literal(value.as_str()),
            ));
        }
    }
    (ours, theirs)
}
