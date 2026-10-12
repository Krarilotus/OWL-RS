//! Diagnostic: recall of a one-in-ten filtered k-NN on the fast suite's vector data
//! (200 k uniform vectors in [-1, 1]^64, kind = i % 10), through each route the SPARQL
//! layer can take. Counts only, no times.
//!
//!   cargo run --release -p nrese-vector --example filtered_recall [items] [queries] [dim]

use nrese_vector::{Hnsw, HnswParameters, Metric, Vectors, exact, merge};

struct Rng(u64);

impl Rng {
    fn unit(&mut self) -> f32 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        ((z >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

const K: usize = 10;
const KINDS: u64 = 10;

/// The SPARQL probe-first loop (`vector_join` → `nearest`): unfiltered rounds of k, 4k,
/// 16k … with a beam of max(ef, k) widened by `1 / share`, the filter applied to the hits,
/// stopping once K are accepted.
fn probe_first(
    graph: &Hnsw,
    vectors: &Vectors,
    query: &[f32],
    ef0: usize,
    share: f64,
) -> (Vec<u64>, usize) {
    let accept = |id: u64| id % KINDS == 3;
    let (mut k, mut ef) = (K, ef0);
    let mut kept: Vec<(f32, u64)> = Vec::new();
    let mut fetched_total = 0;
    loop {
        let beam = ((ef.max(k) as f64) / share).ceil() as usize;
        let hits = graph.search(vectors, query, k, beam, &|_| true);
        fetched_total += hits.len();
        for hit in &hits {
            if accept(hit.id) && !kept.iter().any(|&(_, id)| id == hit.id) {
                kept.push((hit.distance, hit.id));
            }
        }
        kept.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        kept.truncate(K);
        if kept.len() == K || hits.len() < k || k >= vectors.len() {
            return (kept.into_iter().map(|(_, id)| id).collect(), fetched_total);
        }
        k = (k * 4).min(vectors.len());
        ef = ef.max(k);
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let items: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(200_000);
    let queries: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(50);
    let dim: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(64);
    let mut rng = Rng(42);
    let mut vectors = Vectors::new(dim);
    for i in 0..items as u64 {
        let v: Vec<f32> = (0..dim).map(|_| rng.unit()).collect();
        vectors.push(i, &v);
    }
    let started = std::time::Instant::now();
    let mut graph = Hnsw::new(Metric::Cosine, HnswParameters::default());
    graph.extend(&vectors);
    eprintln!("graph of {items} built in {:?}", started.elapsed());
    let filter = |id: u64| id % KINDS == 3;
    let share = 1.0 / KINDS as f64;
    let mut score = std::collections::BTreeMap::<&str, (usize, usize)>::new();
    let mut add = |name: &'static str, got: &[u64], truth: &[u64], fetched: usize| {
        let entry = score.entry(name).or_default();
        entry.0 += got.iter().filter(|id| truth.contains(id)).count();
        entry.1 += fetched;
    };
    for _ in 0..queries {
        let query: Vec<f32> = (0..dim).map(|_| rng.unit()).collect();
        let truth_all: Vec<u64> = exact(&vectors, &query, K, Metric::Cosine, &|_| true)
            .iter()
            .map(|h| h.id)
            .collect();
        let truth: Vec<u64> = exact(&vectors, &query, K, Metric::Cosine, &filter)
            .iter()
            .map(|h| h.id)
            .collect();
        for ef in [64usize, 256, 1024] {
            let got: Vec<u64> = graph
                .search(&vectors, &query, K, ef, &|_| true)
                .iter()
                .map(|h| h.id)
                .collect();
            add(
                match ef {
                    64 => "A unfiltered ef=64",
                    256 => "A unfiltered ef=256",
                    _ => "A unfiltered ef=1024",
                },
                &got,
                &truth_all,
                K,
            );
        }
        let (got, fetched) = probe_first(&graph, &vectors, &query, 64, 1.0);
        add(
            "B probe-first as built (ef 64, share unknown)",
            &got,
            &truth,
            fetched,
        );
        let (got, fetched) = probe_first(&graph, &vectors, &query, 256, 1.0);
        add("C probe-first, nrv:searchBudget 256", &got, &truth, fetched);
        let (got, fetched) = probe_first(&graph, &vectors, &query, 64, share);
        add(
            "D probe-first, beam widened by 1/share",
            &got,
            &truth,
            fetched,
        );
        let got: Vec<u64> = graph
            .search(
                &vectors,
                &query,
                K,
                ((64.0 / share).ceil()) as usize,
                &filter,
            )
            .iter()
            .map(|h| h.id)
            .collect();
        add(
            "E candidate-first: filter in the beam, ef 64/share",
            &got,
            &truth,
            K,
        );
        let first = graph.search(
            &vectors,
            &query,
            (K as f64 / share) as usize * 2,
            ((64.0 / share).ceil()) as usize,
            &|_| true,
        );
        let fetched = first.len();
        let got: Vec<u64> = merge(first.into_iter().filter(|h| filter(h.id)), K)
            .iter()
            .map(|h| h.id)
            .collect();
        add(
            "F probe-first, one round of 2k/share, beam 64/share",
            &got,
            &truth,
            fetched,
        );
    }
    println!("items={items} dim={dim} queries={queries} k={K} filter share={share}");
    for (name, (found, fetched)) in score {
        println!(
            "{name:<55} recall {:.3}  hits fetched per query {:.0}",
            found as f64 / (queries * K) as f64,
            fetched as f64 / queries as f64
        );
    }
}
