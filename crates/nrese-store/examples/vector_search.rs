//! Vector search end to end, timed: N random vectors of dimension D as statements
//! (`<item/i> <embedding> "[...]"^^nrv:vector`), bulk-loaded; then SPARQL searches
//! through `SERVICE nrv:search`, approximate and exact, with the share of the exact
//! nearest the approximate search finds.
//!
//! ```text
//! cargo run --release -p nrese-store --example vector_search -- [--n N] [--dim D] [--k K] [--queries Q]
//! ```

use std::io::Write;
use std::time::Instant;

use nrese_store::{BulkLoadRequest, GraphTarget, SparqlQueryRequest, StoreConfig, StoreService};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut n, mut dim, mut k, mut queries) = (100_000usize, 384usize, 10usize, 20usize);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let value = args.next().and_then(|v| v.parse().ok());
        match arg.as_str() {
            "--n" => n = value.unwrap_or(n),
            "--dim" => dim = value.unwrap_or(dim),
            "--k" => k = value.unwrap_or(k),
            "--queries" => queries = value.unwrap_or(queries),
            _ => {}
        }
    }
    let mut state = 0x1234_5678_9abc_def0u64;
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 40) as f32 / (1u64 << 24) as f32 - 0.5
    };
    // Clustered: 100 centres, each vector near one, as embeddings of related texts are.
    let centres: Vec<Vec<f32>> = (0..100)
        .map(|_| (0..dim).map(|_| next()).collect())
        .collect();
    let scratch = tempfile::tempdir_in(".")?;
    let file = scratch.path().join("vectors.nt");
    {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&file)?);
        for i in 0..n {
            let centre = &centres[i % centres.len()];
            let vector: Vec<f32> = centre.iter().map(|c| c + 0.3 * next()).collect();
            writeln!(
                out,
                "<http://example.com/item/{i}> <http://example.com/embedding> \"{}\"^^<urn:nrese:vector:vector> .",
                nrese_engine::vector::lexical(&vector)
            )?;
        }
    }
    let store = StoreService::new(StoreConfig::in_memory())?;
    let started = Instant::now();
    store.bulk_load(&BulkLoadRequest {
        files: vec![file],
        replace: false,
        graph: GraphTarget::DefaultGraph,
        skip_errors: false,
    })?;
    println!(
        "{n} vectors of {dim}: loaded in {:.2} s",
        started.elapsed().as_secs_f64()
    );
    let query_vectors: Vec<Vec<f32>> = (0..queries)
        .map(|q| {
            centres[q % centres.len()]
                .iter()
                .map(|c| c + 0.3 * next())
                .collect()
        })
        .collect();
    let search = |vector: &[f32],
                  exact: bool|
     -> Result<(Vec<String>, f64), Box<dyn std::error::Error>> {
        let query = format!(
            "PREFIX nrv: <urn:nrese:vector:> SELECT ?item WHERE {{ ?item <http://example.com/embedding> ?v . \
             SERVICE nrv:search {{ ?v nrv:near \"{}\"^^nrv:vector ; nrv:k {k} {} }} }}",
            nrese_engine::vector::lexical(vector),
            if exact { "; nrv:exact true" } else { "" }
        );
        let started = Instant::now();
        let result =
            store.execute_query(&SparqlQueryRequest::new(query, nrese_store::ReadScope::All))?;
        let elapsed = started.elapsed().as_secs_f64() * 1e3;
        let text = String::from_utf8(result.payload)?;
        let items = text
            .split("http://example.com/item/")
            .skip(1)
            .map(|rest| rest.split('"').next().unwrap_or_default().to_owned())
            .collect();
        Ok((items, elapsed))
    };
    let (_, first) = search(&query_vectors[0], false)?;
    println!("first search (the index and graph built): {first:.0} ms");
    // The search alone: no pattern joined.
    let mut alone = Vec::new();
    for vector in &query_vectors {
        let query = format!(
            "PREFIX nrv: <urn:nrese:vector:> SELECT ?v WHERE {{ SERVICE nrv:search {{ ?v nrv:near \"{}\"^^nrv:vector ; nrv:k {k} }} }}",
            nrese_engine::vector::lexical(vector)
        );
        let started = Instant::now();
        store.execute_query(&SparqlQueryRequest::new(query, nrese_store::ReadScope::All))?;
        alone.push(started.elapsed().as_secs_f64() * 1e3);
    }
    alone.sort_by(f64::total_cmp);
    println!("median search alone: {:.2} ms", alone[alone.len() / 2]);
    let (mut approximate_ms, mut exact_ms, mut found) = (Vec::new(), Vec::new(), 0usize);
    for vector in &query_vectors {
        let (approximate, a) = search(vector, false)?;
        let (exact, e) = search(vector, true)?;
        approximate_ms.push(a);
        exact_ms.push(e);
        found += approximate
            .iter()
            .filter(|item| exact.contains(item))
            .count();
    }
    let median = |times: &mut Vec<f64>| {
        times.sort_by(f64::total_cmp);
        times[times.len() / 2]
    };
    println!(
        "median search: approximate {:.2} ms, exact {:.2} ms; recall@{k} {:.3}",
        median(&mut approximate_ms),
        median(&mut exact_ms),
        found as f64 / (queries * k) as f64
    );
    Ok(())
}
