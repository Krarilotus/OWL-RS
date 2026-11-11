//! Full materialisation of a snapshot's asserted statements ([`super::materialise`]).

use std::sync::Arc;
use std::time::Instant;

use nrese_engine::quad::Permutation;
use nrese_engine::{EncodedTriple, QuadPattern, ReadModel, Snapshot, TermId};
use nrese_exec::Budget;
use rayon::prelude::*;

use super::{Program, encode, storable};
use crate::batch::{self, Phases, Schema};
use crate::delta;
use crate::eval::OperationStop;
use crate::ir::{Triple, Violations};
use crate::lists::ListDiagnostic;

/// A closure computed over engine term ids.
#[derive(Debug, Default)]
pub struct Closure {
    /// The storable inferred statements (in the default graph).
    pub inferred: Vec<EncodedTriple>,
    /// Its consistency violations, sorted: the rules', then the datatype checks'.
    pub violations: Violations,
    /// List axioms that weren't instantiated.
    pub diagnostics: Vec<ListDiagnostic>,
    pub rounds: usize,
    pub phases: Phases,
}

/// The closure of `asserted` (any graphs) under `program`, its axioms included.
pub fn materialise(program: &Program, snapshot: &Snapshot) -> Closure {
    materialise_until(program, snapshot, crate::eval::NEVER).expect("never stopped")
}

/// [`materialise`], polling `stop` throughout; `Err` if it fired.
pub fn materialise_until(
    program: &Program,
    snapshot: &Snapshot,
    stop: crate::eval::Stop<'_>,
) -> Result<Closure, delta::Interrupted> {
    materialise_within(program, snapshot, stop, &crate::violations::unlimited())
}

/// [`materialise_until`], its consistency violations (the rules' and the datatype checks')
/// charged to `memory`: the operation's own part, whose refusal ends their list and keeps
/// the state inconsistent ([`Violations`]).
pub fn materialise_within(
    program: &Program,
    snapshot: &Snapshot,
    stop: crate::eval::Stop<'_>,
    memory: &Arc<Budget>,
) -> Result<Closure, delta::Interrupted> {
    materialise_with(program, snapshot, &OperationStop::new(stop), memory)
}

/// [`materialise_within`] under one operation's `stop`, whose memory limit and room its
/// caller chose ([`OperationStop::within`]): the equality copies are admitted by its room.
pub fn materialise_with(
    program: &Program,
    snapshot: &Snapshot,
    stop: &OperationStop<'_>,
    memory: &Arc<Budget>,
) -> Result<Closure, delta::Interrupted> {
    closure_with(program, snapshot, stop, memory, None)
}

/// [`materialise_with`], with the derived facts no store can hold (a literal subject, a
/// non-IRI predicate) appended to `unstorable`, for a first load that reports them
/// ([`super::maintain`]). Nothing else keeps them: a rematerialisation would hold them
/// through its heap's peak.
pub(super) fn materialise_with_unstorable(
    program: &Program,
    snapshot: &Snapshot,
    operation: &OperationStop<'_>,
    memory: &Arc<Budget>,
    unstorable: &mut Vec<Triple>,
) -> Result<Closure, delta::Interrupted> {
    closure_with(program, snapshot, operation, memory, Some(unstorable))
}

impl Program {
    /// Whether the closure of the axioms alone concludes only facts a store can hold, read
    /// from `empty`, a snapshot with no asserted statement: then each such fact a first
    /// load's closure holds is new to the state (review r1 F1, [`super::maintain`]).
    /// Computed once for the program; `Err` if `operation` fired first, its cause kept. The
    /// axioms' own violations are those of the state before: charged to `memory` while it
    /// runs, and their equality copies to the operation's room.
    pub(super) fn axioms_conclude_only_storable(
        &self,
        empty: &Snapshot,
        operation: &OperationStop<'_>,
        memory: &Arc<Budget>,
    ) -> Result<bool, delta::Interrupted> {
        if let Some(&known) = self.axioms_storable.get() {
            return Ok(known);
        }
        debug_assert_eq!(empty.len_in(ReadModel::Asserted), 0);
        let mut unstorable = Vec::new();
        closure_with(self, empty, operation, memory, Some(&mut unstorable))?;
        Ok(*self.axioms_storable.get_or_init(|| unstorable.is_empty()))
    }
}

/// [`materialise_with`]'s work, the unstorable facts kept where `unstorable` asks for them.
/// Every full closure is a bulk phase ([`nrese_exec::memory::HugePages::Bulk`]): a store's
/// rematerialisation and a commit's or first load's rebuild ([`super::maintain`]) alike.
fn closure_with(
    program: &Program,
    snapshot: &Snapshot,
    stop: &OperationStop<'_>,
    memory: &Arc<Budget>,
    mut unstorable: Option<&mut Vec<Triple>>,
) -> Result<Closure, delta::Interrupted> {
    let _huge_pages = nrese_exec::memory::HugePagesPhase::begin();
    nrese_exec::heap::phase("reasoner: input");
    let (input, axioms) = input_of(program, snapshot);
    let schema = program.schema_for(snapshot);
    let result = match program.same_as.filter(|_| program.by_representatives) {
        Some(same_as) => {
            match by_representatives(program, input, same_as, &schema, stop, memory)? {
                // A class holding rdf:nil reads every list through it as empty: computed
                // again by replication. A compact stack keeps the closure over
                // representatives.
                result if result.nil_merged && !program.store_representatives => {
                    batch::materialise_grouped_with(
                        input_of(program, snapshot).0,
                        &program.rules,
                        program.lists.as_ref(),
                        &schema,
                        stop,
                        memory,
                    )?
                }
                result => result,
            }
        }
        None => batch::materialise_grouped_with(
            input,
            &program.rules,
            program.lists.as_ref(),
            &schema,
            stop,
            memory,
        )?,
    };
    nrese_exec::heap::phase("reasoner: datatype checks");
    let mut violations = result.violations;
    // A list that already stopped at its budget makes the state inconsistent: the
    // datatype checks would add nothing to that.
    if violations.complete() {
        violations.append(super::datatypes::violations(
            &result.derived,
            program.rdf_type,
            program.same_as,
            &|id| snapshot.decode(TermId::from_raw(id)),
            memory,
        ));
    }
    nrese_exec::heap::phase("store: encode");
    // At its exact size: collected by doubling, the list held up to twice the facts, and
    // at a doubling three times, beside them (LUBM 1000: 6.6 GB at this step's peak).
    let mut inferred = Vec::with_capacity(result.derived.len() + axioms.len());
    for t in result.derived.into_iter().chain(axioms) {
        if storable(t) {
            inferred.push(encode(t));
        } else if let Some(unstorable) = unstorable.as_deref_mut() {
            unstorable.push(t);
        }
    }
    Ok(Closure {
        inferred,
        violations,
        diagnostics: result.diagnostics,
        rounds: result.rounds,
        phases: result.phases,
    })
}

/// Facts grouped by predicate: each predicate once, its `(object, subject)` pairs sorted.
type Grouped = Vec<(u64, Vec<(u64, u64)>)>;

/// The input of a full materialisation of `snapshot`: its asserted statements (any graph)
/// grouped by predicate, with the ruleset's axioms; and the axioms nothing asserts (they
/// are inferred statements, and premises of the rules).
fn input_of(program: &Program, snapshot: &Snapshot) -> (Grouped, Vec<Triple>) {
    let mut input = asserted_by_predicate(snapshot);
    let mut axioms: Vec<Triple> = Vec::new();
    for &axiom in &program.axioms {
        let [s, p, o] = axiom;
        let at = input.partition_point(|(predicate, _)| *predicate < p);
        match input.get_mut(at) {
            Some((predicate, pairs)) if *predicate == p => match pairs.binary_search(&(o, s)) {
                Ok(_) => continue,
                Err(position) => pairs.insert(position, (o, s)),
            },
            _ => input.insert(at, (p, vec![(o, s)])),
        }
        axioms.push(axiom);
    }
    (input, axioms)
}

/// The closure of `input` (grouped by predicate) over representatives of the `owl:sameAs`
/// classes, in one materialisation whose rounds merge the classes
/// ([`batch::materialise_representatives_until`]): expanded to every identity, as the
/// replacement rules derive it, or as the inferred stack keeps it over representatives,
/// with each other identity as `identity sameAs representative`. Violations are over
/// representatives. Without equalities, this is the plain closure, without the
/// replacement rules.
fn by_representatives(
    program: &Program,
    input: Grouped,
    same_as: u64,
    schema: &Schema,
    stop: &OperationStop<'_>,
    memory: &Arc<Budget>,
) -> Result<batch::Materialisation, delta::Interrupted> {
    let listing = match program.store_representatives {
        true => batch::Listing::Stored,
        false => batch::Listing::Expanded,
    };
    let mut result = batch::materialise_representatives_with(
        batch::Input::Grouped(input),
        &program.rules,
        program.lists.as_ref(),
        schema,
        listing,
        stop,
        memory,
    )?;
    if program.store_representatives && !result.classes.is_empty() {
        let classes = &result.classes;
        let members: Vec<Triple> = classes
            .classes()
            .flat_map(|(representative, members)| {
                members
                    .iter()
                    .filter(move |&&member| member != representative)
                    .map(move |&member| [member, same_as, representative])
            })
            .collect();
        result.derived.extend(members);
        result.derived.par_sort_unstable();
        result.derived.dedup();
    }
    Ok(result)
}

/// Equality by representatives against replication on the asserted data of `snapshot`
/// (work package W4): facts and time of both closures, and the classes. A measurement,
/// not a store operation; nothing when `stop` or the process memory limit interrupts it.
pub fn equality_report_until(
    program: &Program,
    snapshot: &Snapshot,
    stop: crate::eval::Stop<'_>,
) -> Result<String, crate::delta::Interrupted> {
    equality_report_with(program, snapshot, &OperationStop::new(stop))
}

/// [`equality_report_until`] under one operation's `stop`, whose memory limit and room its
/// caller chose ([`OperationStop::within`]): replication's copies are admitted by its room.
pub fn equality_report_with(
    program: &Program,
    snapshot: &Snapshot,
    operation: &OperationStop<'_>,
) -> Result<String, crate::delta::Interrupted> {
    if operation.fired() {
        return Err(operation.interrupted());
    }
    let stop: crate::eval::Stop<'_> = &|| operation.fired();
    let input: Vec<Triple> = asserted_by_predicate(snapshot)
        .into_iter()
        .flat_map(|(p, pairs)| pairs.into_iter().map(move |(o, s)| [s, p, o]))
        .collect();
    let started = Instant::now();
    let closure = crate::representatives::materialise_until(
        &input,
        &program.rules,
        program.lists.as_ref(),
        &program.schema,
        stop,
    )?;
    let representative_time = started.elapsed();
    let asserted = input.len();
    let started = Instant::now();
    let replicated = batch::materialise_owned_with(
        input,
        &program.rules,
        program.lists.as_ref(),
        &program.schema,
        operation,
    )?;
    let replicated_time = started.elapsed();
    let classes = closure.classes.classes().count();
    let members: usize = closure.classes.classes().map(|(_, m)| m.len()).sum();
    let largest = closure
        .classes
        .classes()
        .map(|(_, m)| m.len())
        .max()
        .unwrap_or(0);
    Ok(format!(
        "equality: asserted {} | replicated closure {} facts in {:.3} s | representatives {} facts in {:.3} s, {} merges | {} classes, {} members, largest {}",
        asserted,
        asserted + replicated.derived.len(),
        replicated_time.as_secs_f64(),
        closure.facts.len(),
        representative_time.as_secs_f64(),
        closure.merges,
        classes,
        members,
        largest
    ))
}

/// The asserted facts (any graph) of `snapshot` per predicate, as sorted, distinct
/// `(object, subject)` pairs: one POSG scan, where a fact asserted in several graphs comes
/// out adjacently.
fn asserted_by_predicate(snapshot: &Snapshot) -> Vec<(u64, Vec<(u64, u64)>)> {
    let scan = snapshot
        .scan_sorted_in(ReadModel::Asserted, &QuadPattern::all(), Permutation::Posg)
        .expect("the asserted stack keeps POSG");
    let mut groups: Vec<(u64, Vec<(u64, u64)>)> = Vec::new();
    for quad in scan {
        let (p, pair) = (
            quad.predicate.raw(),
            (quad.object.raw(), quad.subject.raw()),
        );
        match groups.last_mut() {
            Some((last, pairs)) if *last == p => {
                if pairs.last() != Some(&pair) {
                    pairs.push(pair);
                }
            }
            _ => groups.push((p, vec![pair])),
        }
    }
    groups
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use nrese_engine::{Engine, EngineConfig};
    use nrese_exec::memory::{self, HugePages};
    use nrese_rdf_io::{RdfFormat, RdfParser};

    use crate::rulesets::Ruleset;

    fn enabled() -> bool {
        memory::transparent_huge_pages().expect("shown: checked before")
    }

    /// An engine holding `:x a :A` with `:A rdfs:subClassOf :B`, and the RDFS program.
    fn a_class_and_its_member() -> (Engine, super::super::Program) {
        let engine = Engine::new(EngineConfig::default()).unwrap();
        let mut tx = engine.transaction();
        let text = "@prefix : <http://example.com/> .
            @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
            :A rdfs:subClassOf :B . :x a :A .";
        for quad in RdfParser::from_format(RdfFormat::Turtle).for_reader(text.as_bytes()) {
            tx.insert(quad.unwrap().as_ref());
        }
        let program = super::super::Program::compile(&Ruleset::Rdfs.into(), &|t| tx.intern(t));
        tx.commit().unwrap();
        (engine, program)
    }

    /// Under the default (`on`), a closure (a commit's rebuild among them) leaves the
    /// process's switch as the process started with it: the same during and after, finished
    /// or stopped. Nothing here sets the switch.
    #[test]
    fn under_the_default_a_closure_leaves_huge_pages_as_the_process_started() {
        if !nrese_exec::measure::alone() {
            return;
        }
        let Some(started) = memory::transparent_huge_pages() else {
            eprintln!("not checked: this kernel doesn't show the process's huge pages");
            return;
        };
        let (engine, program) = a_class_and_its_member();
        let snapshot = engine.snapshot();
        for stop in [false, true] {
            let differed = AtomicBool::new(false);
            let polled = AtomicBool::new(false);
            let _ = super::materialise_until(&program, &snapshot, &|| {
                polled.store(true, Ordering::Relaxed);
                differed.fetch_or(enabled() != started, Ordering::Relaxed);
                stop
            });
            assert!(polled.load(Ordering::Relaxed));
            assert!(
                !differed.load(Ordering::Relaxed),
                "the same during a closure"
            );
            assert_eq!(enabled(), started, "the same after a closure");
        }
    }

    /// Under `budgets.huge_pages = "bulk"`, a full closure has huge pages however its
    /// caller reached it (a commit's rebuild runs no store rematerialisation) and is off
    /// again once it ends, finished or stopped. In a process of its own: the switch is the
    /// process's; where the kernel doesn't show or switch it, the test says so.
    #[test]
    fn a_closure_has_huge_pages_under_bulk_while_it_runs() {
        if !nrese_exec::measure::alone() {
            return;
        }
        if !memory::set_transparent_huge_pages(true)
            || memory::transparent_huge_pages() != Some(true)
        {
            eprintln!("not checked: this kernel doesn't show or switch the process's huge pages");
            return;
        }
        let (engine, program) = a_class_and_its_member();
        let snapshot = engine.snapshot();
        memory::set_huge_pages(HugePages::Bulk);
        assert!(!enabled(), "off before a closure");

        let during = AtomicBool::new(false);
        let finished = super::materialise_until(&program, &snapshot, &|| {
            during.fetch_or(enabled(), Ordering::Relaxed);
            false
        });
        assert!(!finished.unwrap().inferred.is_empty());
        assert!(during.load(Ordering::Relaxed), "on while a closure runs");
        assert!(!enabled(), "off after a closure");

        let during = AtomicBool::new(false);
        let stopped = super::materialise_until(&program, &snapshot, &|| {
            during.store(enabled(), Ordering::Relaxed);
            true
        });
        assert!(stopped.is_err());
        assert!(during.load(Ordering::Relaxed), "on while a closure runs");
        assert!(!enabled(), "off after a closure that was stopped");
    }
}
