//! Guards of the batch executor's memory and probe wins (docs/design/performance.md §0):
//! counts, not times, on a small LUBM-shaped input under OWL 2 RL, on one thread so the
//! allocations are deterministic. Measured with a counting allocator, so this file is a
//! test binary of its own, and its tests run one at a time.

use std::sync::Mutex;

use nrese_exec::heap;
use nrese_reasoner::batch::{self, Materialisation, Schema};
use nrese_reasoner::ir::{OWL, RDF, RDFS, Vocabulary};
use nrese_reasoner::lists::ListVocabulary;
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::vocabulary::LocalVocabulary;

#[global_allocator]
static ALLOCATOR: heap::Counting<std::alloc::System> = heap::Counting(std::alloc::System);

/// The tests share the allocator's counts.
static SERIAL: Mutex<()> = Mutex::new(());

const EX: &str = "http://example.com/";

/// The input's size: about 60,000 facts, 37,000 derived.
const DEPARTMENTS: u64 = 200;

/// Facts per predicate as the store hands them over: each predicate once, its
/// `(object, subject)` pairs sorted and distinct.
type Grouped = Vec<(u64, Vec<(u64, u64)>)>;

/// A LUBM-shaped input: a class hierarchy, sub-, inverse and transitive properties,
/// domains and ranges, and `departments` departments of people taking and teaching
/// courses.
fn lubm_like(vocabulary: &mut LocalVocabulary, departments: u64) -> Vec<[u64; 3]> {
    let mut iri = |name: &str| match name.split_once(':') {
        Some(("rdf", local)) => vocabulary.iri(&format!("{RDF}{local}")),
        Some(("rdfs", local)) => vocabulary.iri(&format!("{RDFS}{local}")),
        Some(("owl", local)) => vocabulary.iri(&format!("{OWL}{local}")),
        _ => vocabulary.iri(&format!("{EX}{name}")),
    };
    let mut facts = Vec::new();
    let schema = [
        "Employee rdfs:subClassOf Person",
        "Faculty rdfs:subClassOf Employee",
        "Professor rdfs:subClassOf Faculty",
        "FullProfessor rdfs:subClassOf Professor",
        "AssociateProfessor rdfs:subClassOf Professor",
        "Student rdfs:subClassOf Person",
        "GraduateStudent rdfs:subClassOf Student",
        "UndergraduateStudent rdfs:subClassOf Student",
        "Department rdfs:subClassOf Organization",
        "University rdfs:subClassOf Organization",
        "GraduateCourse rdfs:subClassOf Course",
        "worksFor rdfs:subPropertyOf memberOf",
        "headOf rdfs:subPropertyOf worksFor",
        "member owl:inverseOf memberOf",
        "subOrganizationOf rdf:type owl:TransitiveProperty",
        "takesCourse rdfs:domain Student",
        "teacherOf rdfs:domain Faculty",
        "teacherOf rdfs:range Course",
        "advisor rdfs:range Professor",
        "degreeFrom rdfs:range University",
    ];
    for line in schema {
        let [s, p, o]: [&str; 3] = line
            .split(' ')
            .collect::<Vec<_>>()
            .try_into()
            .expect("three terms");
        facts.push([iri(s), iri(p), iri(o)]);
    }
    let (ty, university) = (iri("rdf:type"), iri("University0"));
    facts.push([university, ty, iri("University")]);
    let names = [
        "Department",
        "FullProfessor",
        "AssociateProfessor",
        "GraduateStudent",
        "UndergraduateStudent",
        "GraduateCourse",
        "subOrganizationOf",
        "worksFor",
        "headOf",
        "teacherOf",
        "takesCourse",
        "advisor",
        "memberOf",
        "degreeFrom",
    ];
    let ids: Vec<u64> = names.iter().map(|name| iri(name)).collect();
    let [
        dept,
        full,
        assoc,
        grad,
        under,
        course,
        sub,
        works,
        head,
        teacher,
        takes,
        advisor,
        member,
        degree,
    ]: [u64; 14] = ids.try_into().expect("fourteen names");
    for d in 0..departments {
        let department = iri(&format!("Department{d}"));
        facts.push([department, ty, dept]);
        facts.push([department, sub, university]);
        let faculty: Vec<u64> = (0..8).map(|f| iri(&format!("Faculty{d}.{f}"))).collect();
        for (f, &person) in faculty.iter().enumerate() {
            facts.push([person, ty, if f % 2 == 0 { full } else { assoc }]);
            facts.push([person, if f == 0 { head } else { works }, department]);
            facts.push([person, degree, university]);
            for c in 0..2 {
                let taught = iri(&format!("Course{d}.{f}.{c}"));
                facts.push([taught, ty, course]);
                facts.push([person, teacher, taught]);
            }
        }
        for s in 0..40u64 {
            let student = iri(&format!("Student{d}.{s}"));
            facts.push([student, ty, if s % 4 == 0 { grad } else { under }]);
            facts.push([student, member, department]);
            facts.push([student, advisor, faculty[(s % 8) as usize]]);
            for c in 0..3 {
                let f = (s + c) % 8;
                let taken = iri(&format!("Course{d}.{f}.{}", c % 2));
                facts.push([student, takes, taken]);
            }
        }
    }
    facts
}

fn grouped(mut facts: Vec<[u64; 3]>) -> Grouped {
    facts.sort_unstable_by_key(|&[s, p, o]| (p, o, s));
    facts.dedup();
    let mut groups: Grouped = Vec::new();
    for [s, p, o] in facts {
        match groups.last_mut() {
            Some((last, pairs)) if *last == p => pairs.push((o, s)),
            _ => groups.push((p, vec![(o, s)])),
        }
    }
    groups
}

/// OWL 2 RL over [`lubm_like`] with `departments`, on one thread, as the store runs it
/// (grouped input), with the heap profile of the run and the bytes live before it besides
/// the input's pairs (which become the working set's input runs).
fn materialise(departments: u64) -> (Materialisation, Vec<heap::Phase>, usize) {
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl
        .rules(&mut vocabulary)
        .expect("OWL 2 RL parses");
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let facts = lubm_like(&mut vocabulary, departments);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("a pool");
    let input = grouped(facts);
    let pairs: usize = input.iter().map(|(_, pairs)| pairs.capacity() * 16).sum();
    let before = heap::live() - pairs;
    heap::start("input");
    let result = pool.install(|| batch::materialise_grouped(input, &rules, Some(&lists), &schema));
    (result, heap::finish(), before)
}

/// P1-F1 and P1-F4. Rule jobs read their drivers in the working set's runs instead of
/// copying them, and the rounds keep no list of the facts they derived beside the
/// working set (the base and recent runs are that list). Reverting either shows here:
/// copied drivers count bytes, and a derived-facts list holds 24 bytes per derived fact
/// outside the runs at the end of every round.
#[test]
fn rule_jobs_copy_no_drivers_and_rounds_keep_no_derived_facts_list() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (result, phases, before) = materialise(DEPARTMENTS);
    assert_eq!(
        result.counters.driver_bytes_copied, 0,
        "rule jobs copied their drivers"
    );
    // The heap live at the end of each round (when the next one grounds, or the
    // consistency check begins), against the runs it holds.
    let ends: Vec<usize> = phases
        .windows(2)
        .filter(|pair| match pair[1].label {
            "reasoner: grounding" => pair[0].label != "input",
            next => next == "reasoner: consistency",
        })
        .map(|pair| pair[0].live_after)
        .collect();
    assert_eq!(ends.len(), result.rounds);
    assert_eq!(result.counters.store_bytes.len(), result.rounds);
    let derived = result.derived.len();
    assert!(derived > 20_000, "{derived} derived facts: too few to tell");
    let (&end, store) = ends
        .last()
        .zip(result.counters.store_bytes.last())
        .expect("rounds");
    let outside = end - before - store.total();
    // What else a round holds is the program (schema-sized), not data: 38,812 bytes on
    // 5 October 2026, where a derived-facts list would take 893,280.
    assert!(
        outside < derived * 24 / 4,
        "{outside} bytes beside the working set's {} after the last round, {derived} derived facts",
        store.total()
    );
}

/// P1-F9 and P1-F10. A round holds its candidates as its morsels found them (not
/// concatenated, never grown by doubling), merges each predicate's into chunks without a
/// sorted copy of all of them, folds the recent run into the base a chunk at a time, and
/// the derived facts are listed while the working set is taken apart. So no phase holds
/// much beside the working set. With chunks of 1,024 pairs, so that this input's runs
/// have many, the rounds peaked 9.6 % and the listing 1.7 % above the final working set
/// on 5 October 2026; runs of one chunk each (no fold by chunks) peak 23.6 % above it.
#[test]
fn rounds_hold_no_second_copy_beside_the_working_set() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    batch::set_chunk_pairs(1024);
    let (result, phases, before) = materialise(DEPARTMENTS);
    batch::set_chunk_pairs(1 << 20);
    let store = result.counters.store_bytes.last().expect("rounds").total();
    let peak = |labels: &[&str]| {
        phases
            .iter()
            .filter(|phase| labels.contains(&phase.label))
            .map(|phase| phase.peak - before)
            .max()
            .expect("the phases ran")
    };
    let rounds = peak(&[
        "reasoner: grounding",
        "reasoner: joins",
        "reasoner: modules",
        "reasoner: merge",
        "reasoner: install",
    ]);
    assert!(
        rounds * 100 <= store * 115,
        "the rounds peaked at {rounds} bytes, the working set holds {store}"
    );
    let listing = peak(&["reasoner: derived"]);
    assert!(
        listing * 100 <= store * 110,
        "listing the derived facts peaked at {listing} bytes, the working set holds {store}"
    );
}

/// P1-F8. A morsel's candidates are checked against the working set once each, in
/// (predicate, subject, object) order. Unsorted, every probe misses the cache at each
/// level of each binary search; without the deduplication about half the probes repeat.
#[test]
fn membership_probes_are_distinct_and_ordered_per_morsel() {
    let _serial = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
    let (result, _, _) = materialise(DEPARTMENTS);
    let counters = &result.counters;
    assert_eq!(counters.unordered_probes, 0, "probes out of order");
    let probes: u64 = counters.probes.iter().sum();
    assert!(
        probes <= PROBES,
        "{probes} membership probes over {} rounds ({:?}), at most {PROBES}",
        result.rounds,
        counters.probes
    );
}

/// The probes of [`membership_probes_are_distinct_and_ordered_per_morsel`]'s run on
/// 5 October 2026 (37,014, 49,813, 8,016 and 200 in its four rounds). Deterministic: a
/// change of the rules or the input shape that adds probes for a reason re-measures it.
const PROBES: u64 = 95_043;
