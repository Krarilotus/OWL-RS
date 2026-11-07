//! Guard of DRed's one-step derivation checks after their operation stopped (REAS review r1,
//! F2): asked once the stop has fired, `derivable_by` and `derivable_with` do no work at
//! all, not even enumerating the producers of the fact, which copies and sorts a reference
//! per producer. Counted as the bytes they hold (`nrese_exec::measure`), so this file is a
//! test binary of its own.

use nrese_exec::measure;
use nrese_reasoner::batch::{self, Schema};
use nrese_reasoner::delta::{MemoryBase, Rules, program};
use nrese_reasoner::eval::{OperationStop, Seg, Source};
use nrese_reasoner::ir::{RDFS, Vocabulary, parse_rules};
use nrese_reasoner::vocabulary::LocalVocabulary;

#[global_allocator]
static ALLOCATOR: measure::Measured<std::alloc::System> = measure::Measured(std::alloc::System);

const EX: &str = "http://example.com/";

/// `ex:a ex:q ex:b` with 10,000 producers (subproperties of `ex:q`, one ground rule each),
/// checked after its operation stopped: the check holds nothing at any point, where
/// enumerating the producers would hold about 16 bytes each.
#[test]
fn a_stopped_derivation_check_allocates_nothing() {
    if !measure::alone() {
        return;
    }
    let n = 10_000;
    let mut vocabulary = LocalVocabulary::default();
    let rules = parse_rules(
        "spo: (?p rdfs:subPropertyOf ?q), (?x ?p ?y) -> (?x ?q ?y)",
        &mut vocabulary,
    )
    .unwrap();
    let schema = Schema::owl(&mut vocabulary);
    let spo = vocabulary.iri(&format!("{RDFS}subPropertyOf"));
    let q = vocabulary.iri(&format!("{EX}q"));
    let fact = [
        vocabulary.iri(&format!("{EX}a")),
        q,
        vocabulary.iri(&format!("{EX}b")),
    ];
    let mut asserted: Vec<[u64; 3]> = (0..n)
        .map(|i| [vocabulary.iri(&format!("{EX}p{i}")), spo, q])
        .collect();
    asserted.push(fact);
    asserted.sort_unstable();
    let inferred = batch::materialise(&asserted, &rules, None, &schema).derived;
    let ground = program(
        &MemoryBase::new(&asserted, &inferred),
        Rules {
            rules: &rules,
            lists: None,
            schema: &schema,
        },
    );
    assert_eq!(
        ground.producers(fact).len(),
        n,
        "a producer per subproperty"
    );
    let all = vec![true; ground.rules.len()];
    let mut closure = asserted;
    closure.extend(inferred);
    let source = Facts(closure);
    let (cancelled, unlimited) = (|| true, || None::<nrese_exec::memory::MemoryStopCause>);
    let stopped = OperationStop::with_poll(&cancelled, &unlimited);
    assert!(
        stopped.fired(),
        "the operation has stopped before the checks are asked"
    );
    let fired = || stopped.fired();
    let (by, held) = measure::measure(|| ground.derivable_by(&source, fact, &all, true, &fired));
    assert_eq!((by, held), (false, 0), "derivable_by over {n} producers");
    let (with, held) = measure::measure(|| ground.derivable_with(&source, fact, true, &fired));
    assert_eq!(
        (with, held),
        (false, 0),
        "derivable_with over {n} producers"
    );
}

/// The closure's facts in a list: what the checks would read, had they work to do.
struct Facts(Vec<[u64; 3]>);

impl Source for Facts {
    fn scan(&self, pattern: [Option<u64>; 3], _: Seg, f: &mut dyn FnMut([u64; 3])) {
        let matches = |t: &[u64; 3]| (0..3).all(|i| pattern[i].is_none_or(|v| v == t[i]));
        self.0.iter().filter(|t| matches(t)).for_each(|&t| f(t));
    }

    fn estimate(&self, pattern: [Option<u64>; 3], seg: Seg) -> usize {
        let mut n = 0;
        self.scan(pattern, seg, &mut |_| n += 1);
        n
    }

    fn contains(&self, fact: [u64; 3]) -> bool {
        self.0.contains(&fact)
    }
}
