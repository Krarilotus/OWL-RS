//! Fuzzed ontologies through the hypertableau, for diagnosis and for differential runs on
//! the reference reasoners (benches/reasoning/dl).
//!
//! ```text
//! cargo run --release -p nrese-dl --example tableau_fuzz -- [--seed S] [--count N] [--only K]
//!     [--profile alc|alchi|shiq|sroiq|d|sroiqd|ni] [--out DIR] [--timeout SECS] [--switches]
//! ```
//!
//! Prints `case<TAB>answer<TAB>telemetry` per ontology; `--switches` adds a
//! `case<TAB>disagree<TAB>…` line for every switch combination that decides otherwise. The default profile and seed
//! generate the same ontologies as the `tableau_fuzz` test (`--profile test`). With
//! `--out`, writes `DIR/fuzz-S-K.ofn` and `DIR/manifest.tsv` (consistency tasks for
//! `reference.py run`, HermiT, Openllet and Konclude) and `DIR/nrese.tsv` with the
//! answers, so the two can be joined by task id.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Duration;

use nrese_dl::tableau::{Answer, Config, Model, consistency};
use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
use nrese_owl::{Axiom, ClassExpr, ObjProp, Ontology, Term};

/// The test's semantics: the direct semantics on finite interpretations.
#[path = "../tests/tableau_fuzz/semantics.rs"]
mod semantics;

/// Ontologies biased to the NI rule (`--profile ni`).
#[path = "../tests/tableau_fuzz/ni_gen.rs"]
mod ni_gen;

/// A consistent answer's folded model checked against the axioms, or an inconsistent
/// answer's minimal core (axioms dropped while it stays inconsistent) with a search for a
/// small model of the core.
fn witness(
    o: &Ontology,
    sig: &Signature,
    answer: &Answer,
    model: Option<&Model>,
    config: &Config,
    names: &[String],
) {
    let name = |t: Term| names.get(t as usize).cloned().unwrap_or(format!("t{t}"));
    match answer {
        Answer::Consistent => {
            let checked = model
                .filter(|m| m.size > 0 && m.size <= 128)
                .is_some_and(|m| {
                    let mut i = semantics::Interp {
                        n: m.size as u32,
                        ..semantics::Interp::default()
                    };
                    for &(c, e) in &m.concepts {
                        if let nrese_owl::Concept::Named(t) = c {
                            *i.concepts.entry(t).or_default() |= 1 << e;
                        }
                    }
                    for &(r, a, b) in &m.roles {
                        i.add_edge(r, a as u32, b as u32);
                    }
                    for &(t, e) in &m.individuals {
                        i.individuals.insert(t, e as u32);
                    }
                    semantics::close(o, &mut i);
                    semantics::model_of(o, &i)
                });
            eprintln!(
                "witness: the folded model {}",
                if checked {
                    "is a model of every axiom"
                } else {
                    "fails (number restrictions under pairwise blocking?)"
                }
            );
        }
        Answer::Inconsistent => {
            let mut core = o.clone();
            let mut i = 0;
            while i < core.axioms.len() {
                let mut next = core.clone();
                next.axioms.remove(i);
                next.sources.remove(i);
                if consistency(&next, config).answer == Answer::Inconsistent {
                    core = next;
                } else {
                    i += 1;
                }
            }
            eprintln!("witness: a minimal inconsistent core:");
            for a in &core.axioms {
                eprintln!("  {}", core.functional(a, &name));
            }
            let mut rng = Rng::new(1);
            let found = semantics::find_model(
                &core,
                &sig.classes,
                &sig.object_properties,
                &sig.individuals,
                3,
                200_000,
                &mut rng,
            );
            eprintln!(
                "witness: a model of the core with up to 3 elements: {}",
                if found.is_some() {
                    "FOUND"
                } else {
                    "none found"
                }
            );
        }
        _ => {}
    }
}

fn profile(name: &str, case: u64) -> Option<(Sizes, Profile)> {
    let base = Profile {
        axioms: 4 + (case % 5) as usize,
        depth: 2,
        el: false,
        data: false,
        nominals: false,
        numbers: false,
        chains: false,
        abox: true,
    };
    let small = Sizes {
        classes: 3,
        object_properties: 2,
        simple: 1,
        data_properties: 0,
        individuals: 2,
        literals: 0,
    };
    Some(match name {
        // The sizes only: `ni_gen` makes the ontology.
        "ni" => (ni_gen::sizes(), base),
        "test" => (
            small,
            Profile {
                nominals: case.is_multiple_of(3),
                numbers: case.is_multiple_of(2),
                chains: case % 4 == 1,
                ..base
            },
        ),
        "alc" | "alchi" => (
            Sizes {
                classes: 5,
                object_properties: 3,
                simple: 3,
                ..small
            },
            Profile {
                axioms: 8 + (case % 6) as usize,
                ..base
            },
        ),
        "shiq" => (
            Sizes {
                classes: 5,
                object_properties: 3,
                simple: 2,
                ..small
            },
            Profile {
                axioms: 8 + (case % 6) as usize,
                numbers: true,
                chains: true,
                ..base
            },
        ),
        "sroiq" => (
            Sizes {
                classes: 5,
                object_properties: 3,
                simple: 2,
                ..small
            },
            Profile {
                axioms: 8 + (case % 6) as usize,
                numbers: true,
                chains: true,
                nominals: true,
                ..base
            },
        ),
        // Data properties over integer ranges, facets, enumerations and complements: `d`
        // with few classes so that the data part decides, `sroiqd` with everything.
        "d" => (
            Sizes {
                classes: 3,
                object_properties: 1,
                simple: 1,
                data_properties: 2,
                individuals: 3,
                literals: 4,
            },
            Profile {
                axioms: 6 + (case % 6) as usize,
                data: true,
                numbers: case.is_multiple_of(2),
                ..base
            },
        ),
        "sroiqd" => (
            Sizes {
                classes: 4,
                object_properties: 3,
                simple: 2,
                data_properties: 2,
                individuals: 3,
                literals: 4,
            },
            Profile {
                axioms: 8 + (case % 6) as usize,
                data: true,
                numbers: true,
                chains: true,
                nominals: true,
                ..base
            },
        ),
        _ => return None,
    })
}

/// Whether an expression uses an inverse.
fn inverse_free(o: &Ontology, e: nrese_owl::ExprId) -> bool {
    let r = |p: &ObjProp| matches!(p, ObjProp::Named(_));
    match o.classes.get(e.0) {
        ClassExpr::And(xs) | ClassExpr::Or(xs) => xs.iter().all(|&x| inverse_free(o, x)),
        ClassExpr::Not(x) => inverse_free(o, *x),
        ClassExpr::Some(p, x) | ClassExpr::All(p, x) => r(p) && inverse_free(o, *x),
        ClassExpr::Min(_, p, x) | ClassExpr::Max(_, p, x) | ClassExpr::Exact(_, p, x) => {
            r(p) && inverse_free(o, *x)
        }
        ClassExpr::HasValue(p, _) | ClassExpr::HasSelf(p) => r(p),
        _ => true,
    }
}

/// Keeps the axioms of ALC (`hierarchy`: ALCH, `inverses`: ALCHI).
fn restrict(o: &Ontology, hierarchy: bool, inverses: bool) -> Ontology {
    let mut out = o.clone();
    let keep = |a: &Axiom| match a {
        Axiom::Declaration(..)
        | Axiom::ClassAssertion(..)
        | Axiom::ObjectPropertyAssertion(..)
        | Axiom::NegativeObjectPropertyAssertion(..) => true,
        Axiom::SubClassOf(x, y) => inverses || (inverse_free(o, *x) && inverse_free(o, *y)),
        Axiom::EquivalentClasses(xs) | Axiom::DisjointClasses(xs) => {
            inverses || xs.iter().all(|&x| inverse_free(o, x))
        }
        Axiom::DisjointUnion(_, xs) => inverses || xs.iter().all(|&x| inverse_free(o, x)),
        Axiom::ObjectPropertyDomain(p, x) | Axiom::ObjectPropertyRange(p, x) => {
            (inverses || matches!(p, ObjProp::Named(_))) && (inverses || inverse_free(o, *x))
        }
        Axiom::SubObjectPropertyOf(chain, _) => {
            hierarchy && chain.len() == 1 && (inverses || matches!(chain[0], ObjProp::Named(_)))
        }
        Axiom::EquivalentObjectProperties(_) => hierarchy,
        Axiom::InverseObjectProperties(..) => inverses,
        _ => false,
    };
    let kept: Vec<usize> = (0..o.axioms.len())
        .filter(|&i| keep(&o.axioms[i]))
        .collect();
    out.axioms = kept.iter().map(|&i| o.axioms[i].clone()).collect();
    out.sources = kept.iter().map(|&i| o.sources[i].clone()).collect();
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut seed, mut count, mut only, mut out) = (0x0020_2610_0333_u64, 100u64, None, None);
    let (mut profile_name, mut timeout) = ("test".to_owned(), 60u64);
    let (mut witness_mode, mut switches) = (false, false);
    while let Some(arg) = args.next() {
        let mut value = || args.next().ok_or(format!("{arg} needs a value"));
        if arg == "--witness" {
            witness_mode = true;
            continue;
        }
        if arg == "--switches" {
            switches = true;
            continue;
        }
        match arg.as_str() {
            "--seed" => seed = value()?.parse()?,
            "--count" => count = value()?.parse()?,
            "--only" => only = Some(value()?.parse::<u64>()?),
            "--out" => out = Some(std::path::PathBuf::from(value()?)),
            "--profile" => profile_name = value()?,
            "--timeout" => timeout = value()?.parse()?,
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    if let Some(dir) = &out {
        std::fs::create_dir_all(dir)?;
    }
    let config = Config {
        timeout: Some(Duration::from_secs(timeout)),
        ..Config::default()
    };
    let mut rng = Rng::new(seed);
    let (mut manifest, mut answers) = (String::new(), String::new());
    for case in 0..count {
        let (sizes, prof) = profile(&profile_name, case).ok_or("unknown profile")?;
        let mut names: Vec<String> = Vec::new();
        let mut ids: HashMap<String, Term> = HashMap::new();
        let mut intern = |name: &Name| {
            let text = match name {
                Name::Iri(iri) => format!("<{iri}>"),
                Name::Integer(i) => format!("\"{i}\"^^<http://www.w3.org/2001/XMLSchema#integer>"),
            };
            *ids.entry(text.clone()).or_insert_with(|| {
                names.push(text);
                (names.len() - 1) as Term
            })
        };
        let sig = Signature::new(sizes, &mut intern);
        let o = if profile_name == "ni" {
            ni_gen::ontology(&mut rng, &sig)
        } else {
            fuzz::ontology(&mut rng, &sig, prof)
        };
        let _ = fuzz::shuffle(&o, &mut rng);
        let o = match profile_name.as_str() {
            "alc" => restrict(&o, false, false),
            "alchi" => restrict(&o, true, true),
            _ => o,
        };
        if only.is_some_and(|k| k != case) {
            continue;
        }
        let config = Config {
            keep_model: witness_mode,
            ..config.clone()
        };
        let result = consistency(&o, &config);
        let id = format!("fuzz-{seed}-{case}");
        if switches {
            // Every switch combination and at-most encoding: a decided answer other than
            // the default's is a disagreement.
            for bits in 0..32u32 {
                let other = Config {
                    semantic_branching: bits & 1 != 0,
                    backjumping: bits & 2 != 0,
                    anywhere_blocking: bits & 4 != 0,
                    single_blocking: bits & 8 != 0,
                    expand_at_most_up_to: if bits & 16 != 0 { 0 } else { 2 },
                    ..config.clone()
                };
                let answer = consistency(&o, &other).answer;
                let decided = |a: &Answer| matches!(a, Answer::Consistent | Answer::Inconsistent);
                if decided(&answer) && decided(&result.answer) && answer != result.answer {
                    println!("{id}	disagree	switches {bits:05b}: {}", answer.class());
                }
            }
        }
        if witness_mode {
            witness(
                &o,
                &sig,
                &result.answer,
                result.model.as_ref(),
                &config,
                &names,
            );
        }
        println!("{id}\t{}\t{}", result.answer.class(), result.telemetry);
        if let fuzz_answer @ nrese_dl::tableau::Answer::GaveUp(why)
        | fuzz_answer @ nrese_dl::tableau::Answer::Unsupported(why) = &result.answer
        {
            eprintln!("{id}: {} ({why})", fuzz_answer.class());
        }
        if only.is_some() {
            let name = |t: Term| names[t as usize].clone();
            for a in &o.axioms {
                eprintln!("{}", o.functional(a, &name));
            }
        }
        if let Some(dir) = &out {
            let name = |t: Term| names[t as usize].clone();
            let mut text = String::from(
                "Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
                 Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)\n\
                 Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
                 Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n",
            );
            writeln!(text, "Ontology(<http://example.org/{id}>")?;
            for a in &o.axioms {
                writeln!(text, "{}", o.functional(a, &name))?;
            }
            text.push_str(")\n");
            std::fs::write(dir.join(format!("{id}.ofn")), text)?;
            for reasoner in ["hermit", "openllet", "konclude"] {
                writeln!(manifest, "{id}\t{reasoner}\tconsistency\t/work/{id}.ofn")?;
            }
            writeln!(
                answers,
                "{id}\t{}\t{}",
                result.answer.class(),
                result.telemetry.total.as_secs_f64() * 1000.0
            )?;
        }
    }
    if let Some(dir) = &out {
        std::fs::write(dir.join("manifest.tsv"), manifest)?;
        std::fs::write(dir.join("nrese.tsv"), answers)?;
    }
    Ok(())
}
