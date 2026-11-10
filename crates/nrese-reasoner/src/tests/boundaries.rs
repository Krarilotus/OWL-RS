//! REAS-2 fix check r2 (`Harness/notes/review-reas2-r3/review.md`): a stop is an error at
//! every level a read's answer reaches, the job lists, the parallel passes and the merges
//! built from reads included, so the loop around a stopped one ends there (design note,
//! "Fix check r2", rule 2 completed). Each witness counts, below the producer, the units a
//! pass starts after its stop fired (`witness`, per thread): every pass runs on a one-thread
//! pool, so all of them count on the thread that asks the stop. The stop is
//! [`witness::FirstWork`] on the counted site: it fires at the first poll after the first
//! counted unit. Bound: at most the one unit that saw it.

use super::load;
use crate::batch::{self, Schema};
use crate::delta::{MemoryBase, Rules, update_until};
use crate::eval::{OperationStop, Poller, Stopped as ReadStopped};
use crate::graph_sets::{Change, Limits, Previous, Sets, UNWATCHED};
use crate::ir::{Triple, parse_rules};
use crate::rulesets::Ruleset;
use crate::vocabulary::LocalVocabulary;
use crate::witness::{FirstWork, Site};

/// Four poll intervals of units: a pass that goes on after its stop passes over about three.
fn n() -> usize {
    4 * nrese_exec::POLL_UNITS as usize
}

fn one_thread() -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("a pool")
}

/// `n` subclass edges `cᵢ ⊑ dᵢ`, each with a member `xᵢ`: OWL 2 RL grounds one `cax-sco`
/// instance per edge, so a round's jobs, a change's variants and a deletion's dependency
/// keys grow with `n`, not with the ruleset.
fn subclasses(n: usize) -> (Vec<String>, Vec<String>) {
    let axioms = (0..n)
        .map(|i| format!("ex:c{i} rdfs:subClassOf ex:d{i}"))
        .collect();
    let members = (0..n)
        .map(|i| format!("ex:x{i} rdf:type ex:c{i}"))
        .collect();
    (axioms, members)
}

/// R3-F1 (SW118–SW128). Spec: a job's construction that a stopped read cut short is an
/// error, and the loop over the program's rules ends with it. Production-grounded OWL 2 RL
/// over `n` subclass edges: (a) a materialisation's first round, one full job per instance;
/// (b) an update inserting a member of every `cᵢ`, one variant per instance. The stop fires
/// at the first poll after a job's construction started: at most one more construction
/// starts. A stopped one was "no job", and every later instance's construction started.
#[test]
fn job_construction_ends_at_its_stop() {
    let n = n();
    let one = one_thread();
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let (axioms, members) = subclasses(n);
    let axioms = load(&mut vocabulary, &axioms.join("\n"));
    let members = load(&mut vocabulary, &members.join("\n"));
    let mut failed = Vec::new();
    let input: Vec<Triple> = axioms.iter().chain(&members).copied().collect();
    let (after, interrupted) = one.install(|| {
        let first = FirstWork::new(Site::JobConstruction);
        let stop = || first.ask();
        let unlimited = || None;
        let result = batch::materialise_owned_with(
            input.clone(),
            &rules,
            None,
            &schema,
            &OperationStop::with_poll(&stop, &unlimited),
        );
        (first.after(), result.is_err())
    });
    if after > 1 || !interrupted {
        failed.push(format!(
            "a materialisation's round: {after} constructions after the stop, interrupted \
             {interrupted}"
        ));
    }
    let compiled = Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let mut asserted = input.clone();
    asserted.sort_unstable();
    let base = MemoryBase::new(&asserted, &[]);
    let (after, interrupted) = one.install(|| {
        let first = FirstWork::new(Site::JobConstruction);
        let stop = || first.ask();
        let result = update_until(&base, &members, &[], compiled, None, &stop);
        (first.after(), result.is_err())
    });
    if after > 1 || !interrupted {
        failed.push(format!(
            "an update's variants: {after} constructions after the stop, interrupted \
             {interrupted}"
        ));
    }
    assert!(failed.is_empty(), "of {n} instances: {failed:#?}");
}

/// R3-F1 (SW106). Spec: a dependency key's `sameAs` partners that a stopped read cut short
/// are an error, and the enumeration of the grounded rules' keys ends with it (its keys grow
/// with the schema: one class pair per subclass edge). A deletion under OWL 2 RL (which
/// reads `sameAs`) over `n` subclass edges, the stop firing at the first poll after a
/// partner read started: at most one more starts. Read as "no partners", the stopped read
/// let the loop read every later key's.
#[test]
fn a_deletion_s_dependency_keys_end_at_its_stop() {
    let n = n();
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let compiled = Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let (axioms, members) = subclasses(n);
    let axioms = load(&mut vocabulary, &axioms.join("\n"));
    let members = load(&mut vocabulary, &members.join("\n"));
    let deleted = members[..1].to_vec();
    let mut asserted: Vec<Triple> = axioms.iter().chain(&members[1..]).copied().collect();
    asserted.sort_unstable();
    let base = MemoryBase::new(&asserted, &[]);
    let (after, interrupted) = one_thread().install(|| {
        let first = FirstWork::new(Site::PartnerRead);
        let stop = || first.ask();
        let result = update_until(&base, &[], &deleted, compiled, None, &stop);
        (first.after(), result.is_err())
    });
    assert!(
        after <= 1,
        "{after} partner reads started after the stop, of about {}",
        2 * n
    );
    assert!(interrupted, "a stopped update is interrupted");
}

/// R3-F2 (SW141). Spec: an expanded listing's fold fails on a stopped poller before it
/// touches the next fact. A class of two identities and `n` facts mentioning one of them:
/// the stop fires at the first poll after the fold took a fact; at most one more is taken.
/// A stopped thread's fold went on: three class lookups and the copies' bound per later fact.
#[test]
fn an_expanded_listing_s_fold_ends_at_its_stop() {
    let n = n();
    let mut vocabulary = LocalVocabulary::default();
    let text: Vec<String> = std::iter::once("ex:a1 owl:sameAs ex:a0".to_owned())
        .chain((0..n).map(|j| format!("ex:s{j} rdfs:seeAlso ex:a1")))
        .collect();
    let input = load(&mut vocabulary, &text.join("\n"));
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary).unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let (after, interrupted) = one_thread().install(|| {
        let first = FirstWork::new(Site::ExpandedFact);
        let stop = || first.ask();
        let result = batch::materialise_representatives_until(
            batch::Input::Facts(input.clone()),
            &rules,
            None,
            &schema,
            batch::Listing::Expanded,
            &stop,
        );
        (first.after(), result.is_err())
    });
    assert!(
        after <= 1,
        "{after} facts taken by the fold after the stop, of {n}"
    );
    assert!(interrupted, "a stopped listing is interrupted");
}

/// Two rules deriving `xᵢ r yᵢ`, from `xᵢ p yᵢ` (graph 1) and from `xᵢ q yᵢ` (graph 2):
/// each derived fact has two support sets, so at cap 1 its merge asks whether the second is
/// its own graph (a store read, one unit). The closure, the ground program and the graphs.
struct TwoWays {
    asserted: Vec<Triple>,
    derived: Vec<Triple>,
    closure: Vec<Triple>,
    ground: crate::eval::GroundProgram,
    p: u64,
    q: u64,
}

fn two_ways(n: usize) -> TwoWays {
    let mut vocabulary = LocalVocabulary::default();
    let rules = parse_rules(
        "viaP: (?x rdfs:p ?y) -> (?x rdfs:r ?y)\nviaQ: (?x rdfs:q ?y) -> (?x rdfs:r ?y)",
        &mut vocabulary,
    )
    .unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let text: Vec<String> = (0..n)
        .flat_map(|i| {
            [
                format!("ex:x{i} rdfs:p ex:y{i}"),
                format!("ex:x{i} rdfs:q ex:y{i}"),
            ]
        })
        .collect();
    let mut asserted = load(&mut vocabulary, &text.join("\n"));
    asserted.sort_unstable();
    let [p, q] = [asserted[0][1], asserted[1][1]];
    let mut derived = batch::materialise(&asserted, &rules, None, &schema).derived;
    derived.sort_unstable();
    assert_eq!(derived.len(), n, "one derived fact per pair");
    let compiled = Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let ground = crate::delta::program(&MemoryBase::new(&asserted, &derived), compiled);
    let mut closure: Vec<Triple> = asserted.iter().chain(&derived).copied().collect();
    closure.sort_unstable();
    TwoWays {
        asserted,
        derived,
        closure,
        ground,
        p: p.min(q),
        q: p.max(q),
    }
}

/// R3-F2 (SW138, SW137). Spec: a parallel pass whose task poller has stopped fails, and no
/// later item starts. (a) `rederived`'s one-step pass over `n` candidates, (b) the support
/// round's per-head pass over `n` heads (each asks for its own graphs: two sets at cap 1).
/// The stop fires at the first poll after the pass started an item: at most one more
/// starts. A task whose poller had stopped answered "not derived" / "no change" for every
/// later item.
#[test]
fn a_support_pass_ends_at_its_stop() {
    let n = n();
    let ways = two_ways(n);
    let (p, q) = (ways.p, ways.q);
    let graphs = move |fact: Triple| match fact[1] {
        x if x == p => Some(1),
        x if x == q => Some(2),
        _ => None,
    };
    let scratch = std::sync::Arc::new(nrese_exec::Budget::unlimited());
    let source = batch::Store::new(ways.closure.clone());
    let one = one_thread();
    let mut failed = Vec::new();
    let (after, stopped) = one.install(|| {
        let first = FirstWork::new(Site::OneStepCandidate);
        let stop = || first.ask();
        let limits = Limits {
            watch: UNWATCHED,
            stop: &stop,
            scratch: &scratch,
        };
        let asserted = |_: Triple, _: &Poller<'_>| Ok::<_, ReadStopped>(true);
        let result = crate::graph_sets::rederived(
            &source,
            &ways.ground,
            &[],
            &asserted,
            &ways.derived,
            usize::MAX,
            limits,
        );
        (first.after(), result.is_err())
    });
    if after > 1 || !stopped {
        failed.push(format!(
            "rederived's one-step pass: {after} candidates after the stop, stopped {stopped}"
        ));
    }
    let (after, stopped) = one.install(|| {
        let first = FirstWork::new(Site::HeadGroup);
        let stop = || first.ask();
        let limits = Limits {
            watch: UNWATCHED,
            stop: &stop,
            scratch: &scratch,
        };
        let result =
            crate::graph_sets::support_sets(&ways.closure, &ways.ground, &[], &graphs, 1, limits);
        (first.after(), result.is_err())
    });
    if after > 1 || !stopped {
        failed.push(format!(
            "the per-head pass: {after} heads after the stop, stopped {stopped}"
        ));
    }
    assert!(failed.is_empty(), "of {n}: {failed:#?}");
}

/// R3-F3 (SW116, SW117). Spec: a merge whose own-graph read stopped is an error, and the
/// update's merge loop ends with it; no sets are returned. An update touching the `2n`
/// asserted facts of [`two_ways`] at cap 1, so each of the `n` derived facts' merges reads
/// its own graphs; the stop fires at the first poll after a merge started: at most one more
/// starts. A stopped merge answered an ordinary "changed", and every later fact was merged.
#[test]
fn an_update_s_merges_end_at_their_stop() {
    let n = n();
    let ways = two_ways(n);
    let (p, q) = (ways.p, ways.q);
    let graphs = move |fact: Triple| match fact[1] {
        x if x == p => Some(1),
        x if x == q => Some(2),
        _ => None,
    };
    let scratch = std::sync::Arc::new(nrese_exec::Budget::unlimited());
    let source = batch::Store::new(ways.closure.clone());
    let closure = &ways.closure;
    let previous = |_: Triple, _: &Poller<'_>| Ok(Previous::Sets(Sets::NONE));
    let change = Change {
        source: &source,
        in_closure: &|f, _| Ok(closure.binary_search(&f).is_ok()),
        touched: &ways.asserted,
        previous: &previous,
        limit: usize::MAX,
    };
    let (after, stopped) = one_thread().install(|| {
        let first = FirstWork::new(Site::Merge);
        let stop = || first.ask();
        let limits = Limits {
            watch: UNWATCHED,
            stop: &stop,
            scratch: &scratch,
        };
        let result =
            crate::graph_sets::update_support_sets(&change, &ways.ground, &[], &graphs, 1, limits);
        (first.after(), result.is_err())
    });
    assert!(
        after <= 1,
        "{after} merges started after the stop, of {}",
        3 * n
    );
    assert!(stopped, "a stopped update returns no sets");
}
