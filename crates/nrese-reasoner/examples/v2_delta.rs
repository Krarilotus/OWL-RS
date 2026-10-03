//! Delta executor latency over in-memory facts: materialise N-Triples files, then delete
//! and re-insert asserted ABox `rdf:type` statements one at a time, checking each result
//! against rematerialisation with `--check`.
//!
//! With `--batch N`, each change deletes N statements in one commit and puts them back
//! in another: the per-phase times show where large deletes spend theirs, beside one full
//! rematerialisation for comparison.
//!
//! ```text
//! cargo run --release -p nrese-reasoner --example v2_delta -- [--changes N] [--batch N] [--check] input.nt...
//! ```

use std::io::BufRead;
use std::time::Instant;

use nrese_reasoner::v2::batch::{self, Schema};
use nrese_reasoner::v2::delta::{MemoryBase, Rules, program, update};
use nrese_reasoner::v2::ir::Triple;
use nrese_reasoner::v2::ir::Vocabulary;
use nrese_reasoner::v2::lists::ListVocabulary;
use nrese_reasoner::v2::rulesets::Ruleset;
use nrese_reasoner::v2::vocabulary::LocalVocabulary;

fn split(line: &str) -> Option<[&str; 3]> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (subject, rest) = line.split_once(char::is_whitespace)?;
    let (predicate, rest) = rest.trim_start().split_once(char::is_whitespace)?;
    let object = rest.trim().strip_suffix('.')?.trim_end();
    Some([subject, predicate, object])
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut changes, mut check, mut inputs) = (20usize, false, Vec::new());
    let mut batch_size = 1usize;
    let mut predicate: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--changes" => changes = args.next().and_then(|n| n.parse().ok()).unwrap_or(20),
            "--check" => check = true,
            "--batch" => batch_size = args.next().and_then(|n| n.parse().ok()).unwrap_or(1),
            "--predicate" => predicate = args.next(),
            _ => inputs.push(arg),
        }
    }
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl.rules(&mut vocabulary)?;
    let lists = ListVocabulary::new(&mut vocabulary);
    let schema = Schema::owl(&mut vocabulary);
    let rdf_type = vocabulary.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let target_predicate = predicate.map_or(rdf_type, |iri| vocabulary.iri(&iri));
    let mut asserted: Vec<Triple> = Vec::new();
    for path in &inputs {
        for line in std::io::BufReader::new(std::fs::File::open(path)?).lines() {
            let line = line?;
            if let Some([s, p, o]) = split(&line) {
                asserted.push([vocabulary.term(s), vocabulary.term(p), vocabulary.term(o)]);
            }
        }
    }
    asserted.sort_unstable();
    asserted.dedup();
    let compiled = Rules {
        rules: &rules,
        lists: Some(&lists),
        schema: &schema,
    };
    let started = Instant::now();
    let mut inferred = batch::materialise(&asserted, &rules, Some(&lists), &schema).derived;
    let remat_ms = started.elapsed().as_secs_f64() * 1000.0;
    eprintln!(
        "{} asserted, {} inferred; rematerialisation {remat_ms:.0} ms",
        asserted.len(),
        inferred.len()
    );
    // ABox type statements: IRI subjects, classes outside the W3C vocabularies.
    let targets: Vec<Triple> = asserted
        .iter()
        .copied()
        .filter(|t| {
            t[1] == target_predicate
                && vocabulary.text(t[0]).starts_with('<')
                && !vocabulary.text(t[2]).starts_with("<http://www.w3.org/")
        })
        .step_by(97)
        .take(changes * batch_size)
        .collect();
    if batch_size > 1 {
        return batches(
            &targets, batch_size, asserted, inferred, compiled, check, &rules, &lists, &schema,
        );
    }
    let mut cache = Some(program(&MemoryBase::new(&asserted, &inferred), compiled));
    let (mut deletes, mut inserts) = (Vec::new(), Vec::new());
    let mut phases = [[std::time::Duration::ZERO; 5]; 2];
    // Per target: delete it (timed), put it back, insert a statement about a new entity
    // (timed: a fact new to the state), remove it again.
    let fresh: Vec<Triple> = (0..targets.len())
        .map(|i| {
            [
                vocabulary.term(&format!("<urn:nrese:bench:{i}>")),
                target_predicate,
                targets[i][2],
            ]
        })
        .collect();
    for (i, &existing) in targets.iter().enumerate() {
        for (target, deleting, timed) in [
            (existing, true, Some(0)),
            (existing, false, None),
            (fresh[i], false, Some(1)),
            (fresh[i], true, None),
        ] {
            let after: Vec<Triple> = if deleting {
                asserted.iter().copied().filter(|&t| t != target).collect()
            } else {
                let mut a = asserted.clone();
                a.push(target);
                a.sort_unstable();
                a
            };
            let stack: Vec<Triple> = inferred
                .iter()
                .copied()
                .filter(|t| after.binary_search(t).is_err())
                .collect();
            let base = MemoryBase::new(&after, &stack);
            let new_fact = !deleting && inferred.binary_search(&target).is_err();
            let (ins, del): (&[Triple], &[Triple]) = if deleting {
                (&[], std::slice::from_ref(&target))
            } else if new_fact {
                (std::slice::from_ref(&target), &[])
            } else {
                (&[], &[])
            };
            if std::env::var_os("NRESE_DELTA_DEBUG").is_some() {
                eprintln!("prepared {:?}", Instant::now());
            }
            let started = Instant::now();
            let result = update(&base, ins, del, compiled, cache.as_ref());
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            if let Some(k) = timed {
                for (total, phase) in phases[k].iter_mut().zip(result.phases) {
                    *total += phase;
                }
            }
            if std::env::var_os("NRESE_DELTA_DEBUG").is_some() {
                let text = |t: &Triple| t.map(|id| vocabulary.text(id).to_owned()).join(" ");
                eprintln!(
                    "{} {} -> +{} -{} in {ms:.2} ms",
                    if deleting { "DELETE" } else { "INSERT" },
                    text(&target),
                    result.insert.len(),
                    result.remove.len()
                );
                eprintln!("  phases {:?}", result.phases);
            }
            if let Some(program) = result.program {
                cache = Some(program);
            }
            let removal: std::collections::HashSet<Triple> =
                result.remove.iter().copied().collect();
            let mut next: Vec<Triple> = stack
                .into_iter()
                .filter(|t| !removal.contains(t))
                .chain(result.insert)
                .collect();
            next.sort_unstable();
            next.dedup();
            if check {
                let expected = batch::materialise(&after, &rules, Some(&lists), &schema).derived;
                assert_eq!(next, expected, "delta differs from rematerialisation");
            }
            inferred = next;
            asserted = after;
            match timed {
                Some(0) => deletes.push(ms),
                Some(_) => inserts.push(ms),
                None => {}
            }
        }
    }
    let stats = |times: &mut Vec<f64>| {
        times.sort_by(f64::total_cmp);
        let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
        (at(0.5), at(0.99))
    };
    let (d50, d99) = stats(&mut deletes);
    let (i50, i99) = stats(&mut inserts);
    eprintln!(
        "phases (program, overdelete, rederive, insert, consistency): deletes {:?} inserts {:?}",
        phases[0], phases[1]
    );
    println!(
        "{} changes: deletes p50 {d50:.3} ms p99 {d99:.3} ms | inserts p50 {i50:.3} ms p99 {i99:.3} ms",
        deletes.len()
    );
    Ok(())
}

/// `--batch`: each group of `size` targets deleted in one commit (timed, with its phases)
/// and put back in another.
#[allow(clippy::too_many_arguments)]
fn batches(
    targets: &[Triple],
    size: usize,
    mut asserted: Vec<Triple>,
    mut inferred: Vec<Triple>,
    compiled: Rules<'_>,
    check: bool,
    rules: &[nrese_reasoner::v2::ir::Rule],
    lists: &ListVocabulary,
    schema: &Schema,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut cache = Some(program(&MemoryBase::new(&asserted, &inferred), compiled));
    let mut deletes = Vec::new();
    for group in targets.chunks(size) {
        for deleting in [true, false] {
            let after: Vec<Triple> = if deleting {
                let gone: std::collections::HashSet<&Triple> = group.iter().collect();
                asserted
                    .iter()
                    .copied()
                    .filter(|t| !gone.contains(t))
                    .collect()
            } else {
                let mut a = asserted.clone();
                a.extend(group);
                a.sort_unstable();
                a.dedup();
                a
            };
            let stack: Vec<Triple> = inferred
                .iter()
                .copied()
                .filter(|t| after.binary_search(t).is_err())
                .collect();
            let base = MemoryBase::new(&after, &stack);
            let new: Vec<Triple> = if deleting {
                Vec::new()
            } else {
                group
                    .iter()
                    .copied()
                    .filter(|t| inferred.binary_search(t).is_err())
                    .collect()
            };
            let del: &[Triple] = if deleting { group } else { &[] };
            let started = Instant::now();
            let result = update(&base, &new, del, compiled, cache.as_ref());
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            deletes.push((
                if deleting { "delete" } else { "insert" },
                ms,
                result.phases,
                if deleting {
                    result.remove.len()
                } else {
                    result.insert.len()
                },
                result.rounds,
            ));
            if let Some(program) = result.program {
                cache = Some(program);
            }
            let removal: std::collections::HashSet<Triple> =
                result.remove.iter().copied().collect();
            let mut next: Vec<Triple> = stack
                .into_iter()
                .filter(|t| !removal.contains(t))
                .chain(result.insert)
                .collect();
            next.sort_unstable();
            next.dedup();
            if check {
                let expected = batch::materialise(&after, rules, Some(lists), schema).derived;
                assert_eq!(next, expected, "delta differs from rematerialisation");
            }
            inferred = next;
            asserted = after;
        }
    }
    for (what, ms, phases, changed, rounds) in &deletes {
        let phase = |i: usize| phases[i].as_secs_f64() * 1000.0;
        println!(
            "{what} {size}: {ms:.1} ms (program {:.1}, overdelete {:.1}, rederive {:.1}, insert {:.1}, consistency {:.1}); {changed} inferred changed, {rounds} rounds",
            phase(0),
            phase(1),
            phase(2),
            phase(3),
            phase(4)
        );
    }
    Ok(())
}
