//! Random OWL 2 DL ontologies in the functional syntax, for differential runs on the
//! reference reasoners (benches/reasoning/dl) and, later, NRESE's own DL engines
//! (docs/design/owl2-dl.md §11, work package 2.5).
//!
//! ```text
//! cargo run --release -p nrese-owl --example fuzz -- --count 200 --seed 1 --profile sroiq --out DIR
//! ```
//!
//! Writes `DIR/fuzz-SEED-N.ofn` and `DIR/manifest.tsv` (one classification task per
//! ontology and reasoner, for `reference.py run`), so `ore.py compare DIR` lists the
//! ontologies the reasoners disagree on.

use std::collections::HashMap;
use std::fmt::Write as _;

use nrese_owl::fuzz::{self, Name, Profile, Rng, Signature, Sizes};
use nrese_owl::{Ontology, Term};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut count, mut seed, mut out, mut profile) = (100u64, 1u64, None, "sroiq".to_owned());
    let mut reasoners = "hermit,openllet,konclude".to_owned();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--count" => count = args.next().ok_or("--count N")?.parse()?,
            "--seed" => seed = args.next().ok_or("--seed N")?.parse()?,
            "--out" => out = args.next(),
            "--profile" => profile = args.next().ok_or("--profile sroiq|el")?,
            "--reasoners" => reasoners = args.next().ok_or("--reasoners a,b")?,
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let out = std::path::PathBuf::from(out.ok_or("--out DIR is required")?);
    std::fs::create_dir_all(&out)?;
    let profile = match profile.as_str() {
        "sroiq" => Profile::sroiq(),
        "el" => Profile::el(),
        other => return Err(format!("unknown profile {other}").into()),
    };
    let mut rng = Rng::new(seed);
    let mut manifest = String::new();
    for n in 0..count {
        // Terms are their own N-Triples-like text, by id.
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
        let sig = Signature::new(Sizes::default(), &mut intern);
        let o = fuzz::ontology(&mut rng, &sig, profile);
        let file = format!("fuzz-{seed}-{n}.ofn");
        std::fs::write(
            out.join(&file),
            document(&o, &names, &format!("urn:nrese:fuzz:{seed}:{n}")),
        )?;
        for reasoner in reasoners.split(',') {
            writeln!(
                manifest,
                "fuzz-{seed}-{n}\t{reasoner}\tclassify\t/work/{file}"
            )?;
        }
    }
    std::fs::write(out.join("manifest.tsv"), manifest)?;
    eprintln!("{count} ontologies in {}", out.display());
    Ok(())
}

/// The ontology document: prefixes, then every axiom (declarations included).
fn document(o: &Ontology, names: &[String], iri: &str) -> String {
    let name = |t: Term| names[t as usize].clone();
    let mut text = String::from(
        "Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
         Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n",
    );
    let _ = writeln!(text, "Ontology(<{iri}>");
    for axiom in &o.axioms {
        let _ = writeln!(text, "{}", o.functional(axiom, &name));
    }
    text.push_str(")\n");
    text
}
