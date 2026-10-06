//! Full materialisation of a snapshot's asserted statements ([`super::materialise`]).

use std::time::Instant;

use nrese_engine::quad::Permutation;
use nrese_engine::{EncodedTriple, QuadPattern, ReadModel, Snapshot, TermId};
use rayon::prelude::*;

use super::{Program, encode, storable};
use crate::batch::{self, Phases, Schema};
use crate::delta;
use crate::ir::{Triple, Violation};
use crate::lists::ListDiagnostic;

/// A closure computed over engine term ids.
#[derive(Debug, Default)]
pub struct Closure {
    /// The storable inferred statements (in the default graph).
    pub inferred: Vec<EncodedTriple>,
    pub violations: Vec<Violation>,
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
    let (input, axioms) = input_of(program, snapshot);
    let schema = program.schema_for(snapshot);
    let result = match program.same_as.filter(|_| program.by_representatives) {
        Some(same_as) => by_representatives(
            program,
            input,
            &|| input_of(program, snapshot).0,
            same_as,
            &schema,
            stop,
        )?,
        None => batch::materialise_grouped_until(
            input,
            &program.rules,
            program.lists.as_ref(),
            &schema,
            stop,
        )?,
    };
    let mut violations = result.violations;
    violations.extend(super::datatypes::violations(
        &result.derived,
        program.rdf_type,
        program.same_as,
        &|id| snapshot.decode(TermId::from_raw(id)),
    ));
    Ok(Closure {
        inferred: result
            .derived
            .into_iter()
            .chain(axioms)
            .filter(|&t| storable(t))
            .map(encode)
            .collect(),
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
/// classes, expanded to every identity: what the replacement rules derive, as a batch
/// materialisation reports it (the derived facts beyond the input). Violations are over
/// representatives.
///
/// Where no equality appears, nothing needs representatives: without asserted `sameAs`,
/// the closure is computed first on the compact grouped input without the replacement
/// rules, and if it derives no `sameAs` between two terms either, that is the closure
/// (LUBM: the replicated time and memory, the replacement rules saved). Otherwise the
/// representative closure starts from the input (`rebuild` reads it again) and what that
/// first pass derived.
fn by_representatives(
    program: &Program,
    input: Grouped,
    rebuild: &dyn Fn() -> Grouped,
    same_as: u64,
    schema: &Schema,
    stop: crate::eval::Stop<'_>,
) -> Result<batch::Materialisation, delta::Interrupted> {
    let equal = |pairs: &[(u64, u64)]| pairs.iter().any(|&(o, s)| o != s);
    let asserts_equality = input
        .binary_search_by_key(&same_as, |(p, _)| *p)
        .is_ok_and(|at| equal(&input[at].1));
    let mut seeds: Vec<Triple> = Vec::new();
    let mut phases = batch::Phases::default();
    let input = if asserts_equality {
        input
    } else {
        let rules = crate::representatives::without_replacement(&program.rules);
        let first =
            batch::materialise_grouped_until(input, &rules, program.lists.as_ref(), schema, stop)?;
        if !first
            .derived
            .iter()
            .any(|t| t[1] == same_as && t[0] != t[2])
        {
            return Ok(first);
        }
        phases.add(&first.phases);
        seeds = first.derived;
        rebuild()
    };
    let mut asserted: Vec<Triple> = input
        .into_iter()
        .flat_map(|(p, pairs)| pairs.into_iter().map(move |(o, s)| [s, p, o]))
        .collect();
    asserted.par_sort_unstable();
    let start: Vec<Triple> = match seeds.is_empty() {
        true => asserted.clone(),
        false => {
            let mut all = asserted.clone();
            all.extend(seeds);
            all.par_sort_unstable();
            all.dedup();
            all
        }
    };
    let closure = crate::representatives::materialise_until(
        &start,
        &program.rules,
        program.lists.as_ref(),
        schema,
        stop,
    )?;
    drop(start);
    phases.add(&closure.phases);
    let classes = &closure.classes;
    if program.store_representatives {
        // The closure as it is, and each identity's place in its class. A fact asserted
        // in some graph needs no inferred copy unless its terms have identities (the
        // default graph's reads expand only what it holds).
        let touches = |fact: &Triple| fact.iter().any(|&term| classes.class_of(term).is_some());
        let mut derived: Vec<Triple> = closure
            .facts
            .par_iter()
            .copied()
            .filter(|fact| touches(fact) || asserted.binary_search(fact).is_err())
            .collect();
        derived.extend(classes.classes().flat_map(|(representative, members)| {
            members
                .iter()
                .filter(move |&&member| member != representative)
                .map(move |&member| [member, same_as, representative])
        }));
        derived.par_sort_unstable();
        derived.dedup();
        return Ok(batch::Materialisation {
            derived,
            violations: closure.violations,
            diagnostics: closure.diagnostics,
            rounds: closure.rounds,
            phases,
            ..batch::Materialisation::default()
        });
    }
    let mut derived: Vec<Triple> = closure
        .facts
        .par_iter()
        .flat_map_iter(|&fact| {
            let [s, p, o] = fact;
            match classes.class_of(s).is_none()
                && classes.class_of(p).is_none()
                && classes.class_of(o).is_none()
            {
                true => vec![fact],
                false => classes.expand(fact),
            }
        })
        .filter(|fact| asserted.binary_search(fact).is_err())
        .collect();
    derived.par_sort_unstable();
    derived.dedup();
    Ok(batch::Materialisation {
        derived,
        violations: closure.violations,
        diagnostics: closure.diagnostics,
        rounds: closure.rounds,
        phases,
        ..batch::Materialisation::default()
    })
}

/// Equality by representatives against replication on the asserted data of `snapshot`
/// (work package W4): facts and time of both closures, and the classes. A measurement,
/// not a store operation.
pub fn equality_report(program: &Program, snapshot: &Snapshot) -> String {
    let input: Vec<Triple> = asserted_by_predicate(snapshot)
        .into_iter()
        .flat_map(|(p, pairs)| pairs.into_iter().map(move |(o, s)| [s, p, o]))
        .collect();
    let started = Instant::now();
    let replicated = batch::materialise(
        &input,
        &program.rules,
        program.lists.as_ref(),
        &program.schema,
    );
    let replicated_time = started.elapsed();
    let started = Instant::now();
    let closure = crate::representatives::materialise(
        &input,
        &program.rules,
        program.lists.as_ref(),
        &program.schema,
    );
    let representative_time = started.elapsed();
    let classes = closure.classes.classes().count();
    let members: usize = closure.classes.classes().map(|(_, m)| m.len()).sum();
    let largest = closure
        .classes
        .classes()
        .map(|(_, m)| m.len())
        .max()
        .unwrap_or(0);
    format!(
        "equality: asserted {} | replicated closure {} facts in {:.3} s | representatives {} facts in {:.3} s, {} merges | {} classes, {} members, largest {}",
        input.len(),
        input.len() + replicated.derived.len(),
        replicated_time.as_secs_f64(),
        closure.facts.len(),
        representative_time.as_secs_f64(),
        closure.merges,
        classes,
        members,
        largest
    )
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
