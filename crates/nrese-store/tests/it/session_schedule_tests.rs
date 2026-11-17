//! A small set interpreter for replayed client transactions. The oracle owns its
//! committed state and pending operations independently of the store driver.
//! A pinned oracle can replace `Model::view` and `Model::commit` while reusing the
//! schedules, operation semantics and driver; no pinned-mode promise is made here.

use std::collections::BTreeSet;

use nrese_store::{
    CancellationToken, MutationCommand, PreparedQuery, SparqlQueryRequest, SparqlUpdateRequest,
    StatementOp, StatementsRequest, StoreConfig, StoreService,
};

use crate::support::Writes;

// A quad in the finite vocabulary: subject, named graph, integer object; predicate urn:n.
type Quad = (u8, u8, u64);
type State = BTreeSet<Quad>;

#[derive(Clone, Copy, Debug)]
enum Op {
    Insert(Quad),
    Delete(Quad),
    Clear(u8),
    Increment(u8, u8),
}

impl Op {
    fn text(self) -> String {
        match self {
            Self::Insert((s, g, n)) => {
                format!("INSERT DATA {{ GRAPH <urn:g{g}> {{ <urn:s{s}> <urn:n> {n} }} }}")
            }
            Self::Delete((s, g, n)) => {
                format!("DELETE DATA {{ GRAPH <urn:g{g}> {{ <urn:s{s}> <urn:n> {n} }} }}")
            }
            Self::Clear(g) => format!("DELETE WHERE {{ GRAPH <urn:g{g}> {{ ?s <urn:n> ?n }} }}"),
            Self::Increment(s, g) => format!(
                "DELETE {{ GRAPH <urn:g{g}> {{ <urn:s{s}> <urn:n> ?n }} }} \
                INSERT {{ GRAPH <urn:g{g}> {{ <urn:s{s}> <urn:n> ?next }} }} \
                WHERE {{ GRAPH <urn:g{g}> {{ <urn:s{s}> <urn:n> ?n }} BIND(?n + 1 AS ?next) }}"
            ),
        }
    }

    fn apply(self, state: &mut State) {
        match self {
            Self::Insert(q) => {
                state.insert(q);
            }
            Self::Delete(q) => {
                state.remove(&q);
            }
            Self::Clear(g) => state.retain(|q| q.1 != g),
            Self::Increment(s, g) => {
                let values: Vec<_> = state
                    .iter()
                    .filter(|q| q.0 == s && q.1 == g)
                    .copied()
                    .collect();
                for q in &values {
                    state.remove(q);
                }
                for (_, _, n) in values {
                    state.insert((s, g, n + 1));
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Begin,
    Read,
    Write(Op),
    Outside(Op),
    Commit,
    Rollback,
}

struct Model {
    committed: State,
    pending: Vec<Option<Vec<Op>>>,
}

impl Model {
    fn new(clients: usize) -> Self {
        Self {
            committed: State::new(),
            pending: vec![None; clients],
        }
    }

    fn view(&self, client: usize) -> State {
        let mut state = self.committed.clone();
        for op in self.pending[client].as_ref().unwrap() {
            op.apply(&mut state);
        }
        state
    }

    fn commit(&mut self, client: usize) {
        for op in self.pending[client].take().unwrap() {
            op.apply(&mut self.committed);
        }
    }
}

struct Driver {
    store: StoreService,
    ids: Vec<Option<String>>,
    query: PreparedQuery,
}

impl Driver {
    fn new(clients: usize, union: bool) -> Self {
        Self {
            store: StoreService::new(StoreConfig {
                union_default_graph: union,
                ..StoreConfig::in_memory()
            })
            .unwrap(),
            ids: vec![None; clients],
            query: PreparedQuery::parse(&SparqlQueryRequest::all(
                "SELECT ?s ?g ?n WHERE { GRAPH ?g { ?s <urn:n> ?n } }",
            ))
            .unwrap(),
        }
    }

    fn read(&self, client: Option<usize>) -> State {
        let pending = client
            .map(|c| StatementsRequest {
                session: self.ids[c].clone(),
                ..StatementsRequest::new(
                    self.store
                        .sessions()
                        .pending(self.ids[c].as_deref().unwrap(), None)
                        .unwrap(),
                )
            })
            .unwrap_or_default();
        let mut bytes = Vec::new();
        self.store
            .run_query_pending(&pending, &self.query, &CancellationToken::new(), &mut bytes)
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json["results"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row["s"]["value"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("urn:s")
                        .unwrap()
                        .parse()
                        .unwrap(),
                    row["g"]["value"]
                        .as_str()
                        .unwrap()
                        .strip_prefix("urn:g")
                        .unwrap()
                        .parse()
                        .unwrap(),
                    row["n"]["value"].as_str().unwrap().parse().unwrap(),
                )
            })
            .collect()
    }

    fn write(&self, client: Option<usize>, op: Op) {
        if let Some(client) = client {
            assert!(
                self.store
                    .sessions()
                    .add(
                        self.ids[client].as_deref().unwrap(),
                        None,
                        vec![StatementOp::Update(SparqlUpdateRequest::new(op.text()))]
                    )
                    .unwrap()
            );
        } else {
            self.store.execute_update_str(&op.text()).unwrap();
        }
    }

    fn commit(&mut self, client: usize) {
        let id = self.ids[client].take().unwrap();
        let request = self.store.sessions().take(&id, None).unwrap();
        self.store
            .apply(
                &MutationCommand::Statements(request),
                &nrese_store::Requester::all(),
            )
            .unwrap();
    }
}

// Same convention as the existing fuzz tests: an optional numeric base seed,
// deterministic by default. The generator is local, with no process-global RNG.
struct Random(u64);
impl Random {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 32) % bound
    }
    fn op(&mut self) -> Op {
        let s = self.next(4) as u8;
        let g = self.next(2) as u8;
        let n = self.next(4);
        match self.next(4) {
            0 => Op::Insert((s, g, n)),
            1 => Op::Delete((s, g, n)),
            2 => Op::Clear(g),
            _ => Op::Increment(s, g),
        }
    }
    fn step(&mut self, live: bool) -> Step {
        match self.next(7) {
            0 if !live => Step::Begin,
            1 if live => Step::Commit,
            2 if live => Step::Rollback,
            3 | 4 if live => Step::Write(self.op()),
            5 => Step::Outside(self.op()),
            _ => Step::Read,
        }
    }
}

#[test]
fn replayed_schedules_match_atomic_commits_and_private_pending_writes() {
    // Spec: RDF4J READ_COMMITTED and documented replayed writes say pending operations -> private; commit -> atomic; rollback -> no trace.
    let first: u64 = std::env::var("NRESE_FUZZ_SEED")
        .ok()
        .map(|s| s.parse().expect("numeric NRESE_FUZZ_SEED"))
        .unwrap_or(0);
    for union in [false, true] {
        for offset in 0..200 {
            let seed = first.wrapping_add(offset);
            let mut rng = Random(seed);
            let clients = 2 + rng.next(2) as usize;
            let mut model = Model::new(clients);
            let mut driver = Driver::new(clients, union);
            let mut trace = Vec::new();
            // A nonempty starting state makes DELETE WHERE and increments observable.
            for s in 0..4 {
                let op = Op::Insert((s, s % 2, 0));
                driver.write(None, op);
                op.apply(&mut model.committed);
            }
            for _ in 0..40 {
                let client = rng.next(clients as u64) as usize;
                let step = rng.step(model.pending[client].is_some());
                trace.push((client, step));
                match step {
                    Step::Begin => {
                        driver.ids[client] = Some(driver.store.sessions().begin(None).unwrap());
                        model.pending[client] = Some(Vec::new());
                    }
                    Step::Write(op) => {
                        driver.write(Some(client), op);
                        model.pending[client].as_mut().unwrap().push(op);
                    }
                    Step::Outside(op) => {
                        driver.write(None, op);
                        op.apply(&mut model.committed);
                    }
                    Step::Commit => {
                        driver.commit(client);
                        model.commit(client);
                    }
                    Step::Rollback => {
                        assert!(
                            driver
                                .store
                                .sessions()
                                .rollback(driver.ids[client].take().as_deref().unwrap(), None)
                        );
                        model.pending[client] = None;
                    }
                    Step::Read => {}
                }
                // The whole committed set is checked after every synchronous step;
                // a partially applied batch, leaked write or rollback changes this answer.
                assert_eq!(
                    driver.read(None),
                    model.committed,
                    "seed={seed} union={union} trace={trace:?}"
                );
                for c in 0..clients {
                    if model.pending[c].is_some() {
                        assert_eq!(
                            driver.read(Some(c)),
                            model.view(c),
                            "seed={seed} union={union} reader={c} trace={trace:?}"
                        );
                    }
                }
            }
            for c in 0..clients {
                if let Some(id) = driver.ids[c].take() {
                    assert!(driver.store.sessions().rollback(&id, None));
                }
            }
            assert_eq!(
                driver.read(None),
                model.committed,
                "seed={seed}: final rollback"
            );
        }
    }
}
