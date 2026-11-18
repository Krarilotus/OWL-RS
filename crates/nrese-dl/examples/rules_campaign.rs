//! A differential campaign for DL-safe rules (docs/design/owl2-dl.md §16): random SROIQ(D)
//! ontologies (`nrese_owl::fuzz`) with random DL-safe rules, rendered in the functional
//! syntax over the small-scope names (`A`, `B`, `a`, `b` of `nrese_owl::scope`), answered
//! by NRESE (`nrese_dl::scope::answer_beyond`) and, from the cases file, by the reference
//! runner (`benches/reasoning/dl/runner`'s `nrese.dl.Scope`).
//!
//! ```text
//! cargo run --release -p nrese-dl --example rules_campaign -- --count N --seed S \
//!   [--built-ins] [--no-data] [--clean] [--axioms K] [--rules R] [--timeout SECS] \
//!   --cases FILE --out FILE
//! ```
//!
//! `--cases`: lines `index<TAB>document` (the runner's input); `--out`: NRESE's answers,
//! `index<TAB>code<TAB>certified<TAB>millis` (the codes of `nrese_dl::scope`).

use std::collections::HashMap;
use std::io::Write as _;
use std::time::{Duration, Instant};

use nrese_owl::fuzz::{self, Name, Profile, Rng, RuleTerms, Signature, Sizes};
use nrese_owl::{Ontology, Term};
use rayon::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut count, mut seed) = (100u64, 1u64);
    let (mut built_ins, mut data, mut clean) = (false, true, false);
    let (mut axioms, mut rules, mut timeout) = (6usize, 2u64, 20u64);
    let (mut cases, mut out) = (None, None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--count" => count = args.next().ok_or("--count N")?.parse()?,
            "--seed" => seed = args.next().ok_or("--seed N")?.parse()?,
            "--built-ins" => built_ins = true,
            "--no-data" => data = false,
            "--clean" => clean = true,
            "--axioms" => axioms = args.next().ok_or("--axioms K")?.parse()?,
            "--rules" => rules = args.next().ok_or("--rules R")?.parse()?,
            "--timeout" => timeout = args.next().ok_or("--timeout SECS")?.parse()?,
            "--cases" => cases = args.next(),
            "--out" => out = args.next(),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let scope = nrese_owl::scope::SCOPE;
    let rename: HashMap<String, String> = [("C0", "A"), ("C1", "B"), ("a0", "a"), ("a1", "b")]
        .into_iter()
        .map(|(f, s)| (format!("{}{f}", fuzz::FUZZ), format!("{scope}{s}")))
        .collect();
    let mut rng = Rng::new(seed);
    let mut documents: Vec<(u64, String)> = Vec::new();
    for n in 0..count {
        let mut names: Vec<String> = Vec::new();
        let mut ids: HashMap<String, Term> = HashMap::new();
        let mut intern = |name: &Name| {
            let text = match name {
                Name::Iri(iri) => format!("<{}>", rename.get(iri).unwrap_or(iri)),
                Name::Integer(i) => {
                    format!("\"{i}\"^^<http://www.w3.org/2001/XMLSchema#integer>")
                }
            };
            *ids.entry(text.clone()).or_insert_with(|| {
                names.push(text);
                (names.len() - 1) as Term
            })
        };
        let sig = Signature::new(
            Sizes {
                data_properties: u32::from(data),
                ..Sizes::default()
            },
            &mut intern,
        );
        let rt = RuleTerms::new(&mut intern);
        let profile = Profile {
            axioms,
            data,
            ..Profile::sroiq()
        };
        let mut o: Ontology = fuzz::ontology(&mut rng, &sig, profile);
        for _ in 0..1 + rng.below(rules) {
            let r = fuzz::rule(&mut rng, &sig, &rt, &mut o, data, built_ins, clean);
            o.rules.push(r);
        }
        let name = |t: Term| names[t as usize].clone();
        let mut text = String::from(
            "Prefix(owl:=<http://www.w3.org/2002/07/owl#>) \
             Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>) \
             Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>) \
             Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) ",
        );
        text.push_str(&format!("Ontology(<urn:nrese:rules:{seed}:{n}>"));
        for a in &o.axioms {
            text.push(' ');
            text.push_str(&o.functional(a, &name));
        }
        for r in &o.rules {
            text.push(' ');
            text.push_str(&o.functional_rule(r, &name));
        }
        text.push(')');
        documents.push((n, text));
    }
    let mut file = std::io::BufWriter::new(std::fs::File::create(cases.ok_or("--cases FILE")?)?);
    for (n, d) in &documents {
        writeln!(file, "{n}\t{d}")?;
    }
    drop(file);
    let options = nrese_dl::beyond::Options {
        timeout: Some(Duration::from_secs(timeout)),
        tableau: nrese_dl::tableau::Config {
            timeout: Some(Duration::from_secs(timeout)),
            ..nrese_dl::tableau::Config::default()
        },
        ..nrese_dl::beyond::Options::default()
    };
    let lines: Vec<String> = documents
        .par_iter()
        .map(|(n, d)| {
            let at = Instant::now();
            let (code, certified) = match nrese_dl::scope::read_with_rules(d) {
                Ok((o, names)) => {
                    let a = nrese_dl::scope::answer_beyond(&o, &names, &options);
                    (a.code, a.certified)
                }
                Err(why) => (format!("x read: {why}"), "-".repeat(11)),
            };
            format!("{n}\t{code}\t{certified}\t{}", at.elapsed().as_millis())
        })
        .collect();
    let mut file = std::io::BufWriter::new(std::fs::File::create(out.ok_or("--out FILE")?)?);
    for l in lines {
        writeln!(file, "{l}")?;
    }
    Ok(())
}
