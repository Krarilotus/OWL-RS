use super::*;
use crate::{DeltaPlanner, ReadScope, SparqlQueryRequest, UpdateContext};
use nrese_engine::{QuadPattern, ReadModel, Transaction};
use nrese_rdf::Quad;
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_sparql::GraphAccess;
use std::collections::BTreeSet;

#[path = "../../../nrese-reasoner/src/tests/ontology_generator.rs"]
mod generator;

#[path = "prepare_fixed_edits.rs"]
mod fixed;

#[derive(Clone, Debug, Default)]
struct Edit {
    insert: Vec<Quad>,
    remove: Vec<Quad>,
}

impl Edit {
    fn change(&self, tx: &mut Transaction<'_>) {
        for quad in &self.remove {
            tx.remove(quad.as_ref());
        }
        for quad in &self.insert {
            tx.insert(quad.as_ref());
        }
    }
}

impl DeltaPlanner for Edit {
    fn name(&self) -> &str {
        "seeded edit"
    }

    fn plan(
        &self,
        tx: &mut Transaction<'_>,
        _: &Requester,
        _: &UpdateContext<'_>,
    ) -> Result<(), StoreError> {
        self.change(tx);
        Ok(())
    }
}

fn parse(data: &str) -> Vec<Quad> {
    const NS: &str = "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>";
    let input = format!("{PREFIX} {NS}\n{data}");
    RdfParser::from_format(RdfFormat::TriG)
        .for_reader(input.as_bytes())
        .map(Result::unwrap)
        .collect()
}

fn fill(
    config: StoreConfig,
    edit: &Edit,
    rules: &nrese_reasoner::RuleProgram,
) -> Arc<StoreService> {
    let store = Arc::new(StoreService::new(config).unwrap());
    let mut tx = store.engine().transaction();
    edit.change(&mut tx);
    tx.commit().unwrap();
    store.rematerialise(rules).unwrap();
    store
}

fn inferred(store: &StoreService) -> BTreeSet<String> {
    let snapshot = store.engine().snapshot();
    snapshot
        .quads_for_pattern_in(ReadModel::Inferred, &QuadPattern::all())
        .map(|quad| snapshot.decode_quad(quad).unwrap().to_string())
        .collect()
}

fn settled(store: &StoreService, edit: &Edit, rules: &nrese_reasoner::RuleProgram) -> bool {
    let mut tx = store.engine().transaction();
    let program = crate::reasoning::Program::compile(rules, &|term| tx.intern(term))
        .hiding_unnamed_classes(true)
        .unwrap()
        .by_representatives(store.config().equality_by_representatives)
        .storing_representatives(store.config().equality_compact);
    let before = crate::reasoning::materialise(&program, tx.base()).violations;
    edit.change(&mut tx);
    let after = crate::reasoning::materialise(&program, &tx.pending_snapshot()).violations;
    let old: BTreeSet<_> = before
        .iter()
        .map(|(name, binding)| (name, binding.to_vec()))
        .collect();
    let accept = after
        .iter()
        .all(|(name, binding)| old.contains(&(name, binding.to_vec())));
    if accept {
        tx.commit().unwrap();
        store.rematerialise(rules).unwrap();
    }
    accept
}

fn run_edit(writer: &MutationPipeline, edit: &Edit) -> bool {
    match writer.apply_as_owner(MutationCommand::Delta(Arc::new(edit.clone()))) {
        Ok(_) => true,
        Err(MutationError::Rejected(_)) => false,
        Err(error) => panic!("edit failed before the verdict: {error:?}"),
    }
}

fn replay(
    reasoner: Arc<ReasonerService>,
    compact: bool,
    setup: Edit,
    edits: impl IntoIterator<Item = Edit>,
    finish: impl Fn(&StoreService),
) {
    let dir = tempfile::tempdir().unwrap();
    let config = StoreConfig {
        hide_unnamed_classes: true,
        equality_compact: compact,
        ..StoreConfig::in_memory()
    };
    let disk = StoreConfig {
        mode: crate::StoreMode::OnDisk,
        data_dir: dir.path().to_owned(),
        ..config.clone()
    };
    let rules = reasoner.config().materialised_program().unwrap();
    let warm = MutationPipeline::new(fill(config.clone(), &setup, &rules), reasoner.clone())
        .prepare_from_start();
    ready(&warm);
    let cold = fill(config.clone(), &setup, &rules);
    let oracle = fill(config, &setup, &rules);
    drop(fill(disk.clone(), &setup, &rules));
    let worker = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let mut previous = std::sync::Weak::<StoreService>::new();
    for edit in edits {
        until(|| previous.strong_count() == 0);
        let store = Arc::new(StoreService::new(disk.clone()).unwrap());
        previous = Arc::downgrade(&store);
        let reopened = MutationPipeline::new(store, reasoner.clone()).prepare_from_start();
        ready(&reopened);
        let verdict = settled(&oracle, &edit, &rules);
        let cold_writer = MutationPipeline::new(cold.clone(), reasoner.clone());
        assert_eq!(
            [
                run_edit(&warm, &edit),
                worker.install(|| run_edit(&cold_writer, &edit)),
                run_edit(&reopened, &edit)
            ],
            [verdict; 3],
            "{edit:?}"
        );
        let expected = inferred(&oracle);
        for store in [warm.store(), &cold, reopened.store()] {
            assert_eq!(inferred(store), expected, "{edit:?}");
            finish(store);
        }
    }
}

/// Spec: the warm, cold and reopened writers give the settled verdict and closure
/// after each step of the same seeded insert/delete histories.
#[test]
fn prepared_writes_replay_seeded_histories_against_rematerialisation() {
    for seed in [0, 4, 9] {
        let mut next = generator::rng(0x2545_F491_4F6C_DD1D ^ seed);
        let text = generator::random_ontology(&mut next).join(" .\n") + " .";
        let pool: Vec<_> = parse(&text);
        let mut active: BTreeSet<_> = (0..pool.len()).filter(|_| next(10) < 7).collect();
        let setup = Edit {
            insert: active.iter().map(|&i| pool[i].clone()).collect(),
            ..Edit::default()
        };
        let edits = (0..4).map(|_| {
            let mut edit = Edit::default();
            for (index, quad) in pool.iter().enumerate() {
                if active.contains(&index) {
                    if next(6) == 0 {
                        edit.remove.push(quad.clone());
                        active.remove(&index);
                    }
                } else if next(4) == 0 {
                    edit.insert.push(quad.clone());
                    active.insert(index);
                }
            }
            edit
        });
        replay(rl(), false, setup, edits, |_| {});
    }
}

/// Spec: keeping one graph support preserves a conclusion only for readers who
/// still hold a complete support, for every writer history.
#[test]
fn keeping_one_support_preserves_the_correct_graph_view() {
    let setup = Edit {
        insert: parse(
            "ex:g1 { ex:x a ex:A } ex:g2 { ex:x a ex:A } ex:gs { ex:A rdfs:subClassOf ex:B }",
        ),
        ..Edit::default()
    };
    let edit = Edit {
        remove: parse("ex:g1 { ex:x a ex:A }"),
        ..Edit::default()
    };
    replay(rl(), false, setup, [edit], |store| {
        for (graphs, expected) in [(None, true), (Some("g1"), false), (Some("g2"), true)] {
            let scope = ReadScope::of(graphs.map(|g| {
                Arc::new(GraphAccess {
                    graphs: vec![
                        format!("http://example.org/{g}"),
                        "http://example.org/gs".into(),
                    ],
                    default_graph: true,
                    inferred: true,
                    inferred_by_support: true,
                    ..GraphAccess::default()
                })
            }));
            let query = SparqlQueryRequest::new(format!("{PREFIX} ASK {{ ex:x a ex:B }}"), scope);
            let answer = store.execute_query(&query).unwrap();
            let json: serde_json::Value = serde_json::from_slice(&answer.payload).unwrap();
            assert_eq!(json["boolean"], expected, "{graphs:?}");
        }
    });
}
