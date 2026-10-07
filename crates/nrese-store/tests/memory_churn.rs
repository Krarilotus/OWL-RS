//! Guard of the store's memory under churn (the soak's question, `benches/probes/soak.py`):
//! the soak's mix of work, run in process on an on-disk store through the mutation
//! pipeline as the server runs it: inserts and deletes over a bounded pool of terms, its
//! queries and plans, client transactions committed or rolled back, Graph Store writes.
//! Once every term of the pool is in the dictionary, more of the same work must leave
//! the heap the program holds where it was: what it keeps is the data, the dictionary and
//! the result cache within its budget, none of which grows past that point. A leak (state
//! kept per commit, per revision, per session or per query) fails here, whatever the
//! allocator does with the memory freed. Counted with a counting allocator, so this file
//! is a test binary of its own.

use std::sync::Arc;

use nrese_exec::heap;
use nrese_reasoner::{ReasonerConfig, ReasonerService};
use nrese_store::{
    GraphResultFormat, GraphTarget, GraphWriteRequest, MutationCommand, MutationPipeline,
    MutationTicket, Requester, SparqlQueryRequest, SparqlUpdateRequest, StatementOp,
    StatementsRequest, StoreConfig, StoreService,
};

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

const SOAK: &str = "urn:soak:g";

/// The soak's queries (`soak.py`'s `QUERIES`).
const QUERIES: [&str; 9] = [
    "SELECT (COUNT(*) AS ?n) WHERE { ?s ?p ?o }",
    "SELECT ?p (COUNT(*) AS ?n) WHERE { ?s ?p ?o } GROUP BY ?p ORDER BY DESC(?n) LIMIT 10",
    "SELECT * WHERE { ?s ?p ?o } LIMIT 100",
    "SELECT * WHERE { ?s a ?c . OPTIONAL { ?s ?p ?o } } LIMIT 50",
    "ASK { ?s ?p ?o }",
    "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o } LIMIT 200",
    "SELECT (COUNT(*) AS ?n) WHERE { GRAPH <urn:soak:g> { ?s ?p ?o } }",
    "SELECT DISTINCT ?c WHERE { ?s a ?c } LIMIT 20",
    "SELECT ?s WHERE { ?s ?p ?o FILTER(isIRI(?o)) } ORDER BY ?s LIMIT 30",
];

/// The pool of subjects (the soak's is 100,000; the guard's is smaller, the same shape).
const POOL: u64 = 2_000;

/// xorshift64*: the same work on every run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % n
    }
}

fn triple(i: u64) -> String {
    format!("<urn:soak:s{i}> <urn:soak:p> {i} .")
}

struct Soak {
    pipeline: MutationPipeline,
    rng: Rng,
}

impl Soak {
    fn write(&self, command: MutationCommand) {
        self.pipeline
            .apply(command, &Requester::all(), &MutationTicket::new())
            .expect("committed");
    }

    fn update(&self, update: String) {
        self.write(MutationCommand::Update(SparqlUpdateRequest::new(update)));
    }

    /// One to `most` triples of the pool.
    fn some(&mut self, most: u64) -> Vec<String> {
        let n = 1 + self.rng.below(most);
        (0..n).map(|_| triple(self.rng.below(POOL))).collect()
    }

    /// One round of the soak's mix.
    fn round(&mut self) {
        let store = Arc::clone(self.pipeline.store());
        let inserted = self.some(20);
        self.update(format!(
            "INSERT DATA {{ GRAPH <{SOAK}> {{ {} }} }}",
            inserted.join(" ")
        ));
        let deleted = self.some(10);
        self.update(format!(
            "DELETE DATA {{ GRAPH <{SOAK}> {{ {} }} }}",
            deleted.join(" ")
        ));
        for query in QUERIES {
            store
                .execute_query(&SparqlQueryRequest::all(query))
                .expect("answered");
        }
        let query = QUERIES[self.rng.below(QUERIES.len() as u64) as usize];
        let prepared = store
            .prepare_query(&SparqlQueryRequest::all(query))
            .expect("parsed");
        store.plan_query(&prepared).expect("planned");
        // A client transaction: committed seven times in ten, else rolled back.
        let session = store.sessions().begin(None).expect("began");
        let update = format!(
            "INSERT DATA {{ GRAPH <{SOAK}> {{ {} }} }}",
            self.some(5).join(" ")
        );
        store.sessions().add(
            &session,
            None,
            vec![StatementOp::Update(SparqlUpdateRequest::new(update))],
        );
        if self.rng.below(10) < 7 {
            let request = store.sessions().take(&session, None).expect("open");
            self.write(MutationCommand::Statements(StatementsRequest::new(
                request.ops,
            )));
        } else {
            assert!(store.sessions().rollback(&session, None));
        }
        // Graph Store writes over ten graphs and a hundred subjects.
        let graph = GraphTarget::NamedGraph(format!("urn:soak:gsp:{}", self.rng.below(10)));
        let data: String = (0..1 + self.rng.below(30))
            .map(|i| {
                format!(
                    "<urn:soak:x{}> <urn:soak:q> \"{i}\" .\n",
                    self.rng.below(100)
                )
            })
            .collect();
        match self.rng.below(3) {
            0 => self.write(MutationCommand::GraphDelete(graph)),
            replace => self.write(MutationCommand::GraphWrite(GraphWriteRequest {
                target: graph,
                format: GraphResultFormat::NTriples,
                base_iri: None,
                payload: data.into_bytes(),
                replace: replace == 1,
            })),
        }
    }
}

#[test]
fn churn_over_a_bounded_pool_of_terms_holds_the_heap_level() {
    let dir = tempfile::tempdir().unwrap();
    let store = StoreService::new(StoreConfig {
        // A small cache: it fills to its budget at most, which the bound below allows.
        query_cache_bytes: 1 << 20,
        ..StoreConfig::on_disk(dir.path())
    })
    .unwrap();
    let mut soak = Soak {
        pipeline: MutationPipeline::new(
            Arc::new(store),
            Arc::new(ReasonerService::new(ReasonerConfig::default())),
        ),
        rng: Rng(20_261_007),
    };
    // Every term of the pool in the dictionary first.
    for chunk in (0..POOL).collect::<Vec<_>>().chunks(500) {
        let triples: Vec<String> = chunk.iter().map(|&i| triple(i)).collect();
        soak.update(format!(
            "INSERT DATA {{ GRAPH <{SOAK}> {{ {} }} }}",
            triples.join(" ")
        ));
    }
    let rounds = 300;
    for _ in 0..rounds {
        soak.round();
    }
    let level = heap::live();
    for _ in 0..3 * rounds {
        soak.round();
    }
    let after = heap::live();
    let grew = after.saturating_sub(level);
    eprintln!(
        "{level} bytes live after {rounds} rounds, {after} after {} more: {grew} grown",
        3 * rounds
    );
    // The data's runs between compactions and the cache within its budget move the level
    // by about a kilobyte either way (it fell by 14 kB on 7 October); state kept per
    // commit (5 a round, 4,500 here), per session or per query passes 32 KiB: 36 bytes a
    // round. Sessions kept after their rollback (270 here) grew it by 299 kB.
    assert!(
        grew < 32 << 10,
        "the heap grew by {grew} bytes over {} rounds of work on the same terms",
        3 * rounds
    );
}
