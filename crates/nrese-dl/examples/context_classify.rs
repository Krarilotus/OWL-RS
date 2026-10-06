//! Classification with the context core (`nrese_dl::context`, Horn stage) of N-Triples or
//! RDF/XML files, for the DL lab.
//!
//! ```text
//! cargo run --release -p nrese-dl --example context_classify -- [options] input.nt...
//!   --out FILE        the closure: `sub<TAB>super`, `C<TAB>owl:Nothing`, `owl:Thing<TAB>C`
//!                     (the EL classifier's example's format, `canonical.py`'s input)
//!   --tax FILE        the canonical taxonomy (`benches/reasoning/dl/canonical.py`'s format)
//!   --threads N       saturation workers (default 1)
//!   --strategy S      `cautious` (default), `eager` or `split`
//!   --no-proofs       don't record derivations
//!   --repeat N        classify N times; times are the median of the runs
//!   --compare-el      also classify with the EL classifier (`nrese_reasoner::classify`) on
//!                     the same triples, runs interleaved (ABAB), and compare the taxonomies
//! ```
//!
//! Prints `unsupported: <why>` and exits with 2 where the Horn stage gives up. Its last line
//! is the profile, `profile name=value …` (times in ms, medians over `--repeat`); with
//! `--compare-el` a line `el-profile …` before it and `el-compare equal` or
//! `el-compare differ …`.

use std::collections::HashMap;
use std::io::{BufRead, BufWriter, Write};
use std::time::{Duration, Instant};

use nrese_dl::context::{self, Classification, Options, Strategy};
use nrese_owl::{Statement, Term, TermKind, Terms};
use nrese_rdf_io::{RdfFormat, RdfParser};

/// Terms by their N-Triples form (what the EL classifier's vocabulary keys by, too).
#[derive(Default, Clone)]
struct Table {
    terms: Vec<String>,
    ids: HashMap<String, u64>,
}

impl Table {
    fn term(&mut self, text: &str) -> u64 {
        if let Some(&id) = self.ids.get(text) {
            return id;
        }
        let id = self.terms.len() as u64;
        self.terms.push(text.to_owned());
        self.ids.insert(text.to_owned(), id);
        id
    }

    fn name(&self, id: u64) -> String {
        self.terms[id as usize].trim_matches(['<', '>']).to_owned()
    }
}

impl Terms for Table {
    fn kind(&self, term: Term) -> TermKind {
        match self.terms[term as usize].as_bytes().first() {
            Some(b'<') => TermKind::Iri,
            Some(b'_') => TermKind::Blank,
            _ => TermKind::Literal,
        }
    }

    fn lexical(&self, term: Term) -> Option<String> {
        let text = self.terms[term as usize].strip_prefix('"')?;
        Some(text[..text.rfind('"')?].to_owned())
    }

    fn iri(&self, iri: &str) -> Option<Term> {
        self.ids.get(&format!("<{iri}>")).copied()
    }
}

impl nrese_reasoner::ir::Vocabulary for Table {
    fn iri(&mut self, iri: &str) -> u64 {
        self.term(&format!("<{iri}>"))
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        self.term(&format!("\"{lexical}\"^^<{datatype}>"))
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        self.term(&format!("\"{lexical}\"@{language}"))
    }
}

/// An N-Triples line's three terms, as the EL classifier's example splits them.
fn split(line: &str) -> Option<[&str; 3]> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (subject, rest) = line.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    let (predicate, rest) = rest.split_once(char::is_whitespace)?;
    let object = rest.trim().strip_suffix('.')?.trim_end();
    Some([subject, predicate, object])
}

fn load(
    path: &str,
    table: &mut Table,
    out: &mut Vec<[u64; 3]>,
) -> Result<(), Box<dyn std::error::Error>> {
    if path.ends_with(".nt") {
        for line in std::io::BufReader::new(std::fs::File::open(path)?).lines() {
            if let Some([s, p, o]) = split(&line?) {
                out.push([table.term(s), table.term(p), table.term(o)]);
            }
        }
        return Ok(());
    }
    let file = std::io::BufReader::new(std::fs::File::open(path)?);
    for quad in RdfParser::from_format(RdfFormat::RdfXml).for_reader(file) {
        let quad = quad?;
        let (s, p, o) = (
            quad.subject.to_string(),
            quad.predicate.to_string(),
            quad.object.to_string(),
        );
        out.push([table.term(&s), table.term(&p), table.term(&o)]);
    }
    Ok(())
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort_unstable();
    times.get(times.len() / 2).copied().unwrap_or_default()
}

fn ms(d: Duration) -> String {
    format!("{:.1}", d.as_secs_f64() * 1000.0)
}

struct Args {
    out: Option<String>,
    tax: Option<String>,
    options: Options,
    repeat: usize,
    compare: bool,
    inputs: Vec<String>,
}

fn args() -> Result<Args, String> {
    let mut a = Args {
        out: None,
        tax: None,
        options: Options::default(),
        repeat: 1,
        compare: false,
        inputs: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut number = |what: &str| -> Result<usize, String> {
            it.next()
                .and_then(|n| n.parse().ok())
                .ok_or(format!("{what} N"))
        };
        match arg.as_str() {
            "--threads" => a.options.threads = number("--threads")?,
            "--repeat" => a.repeat = number("--repeat")?.max(1),
            "--out" => a.out = it.next(),
            "--tax" => a.tax = it.next(),
            "--no-proofs" => a.options.proofs = false,
            "--compare-el" => a.compare = true,
            "--strategy" => {
                a.options.strategy = match it.next().as_deref() {
                    Some("cautious") => Strategy::Cautious,
                    Some("eager") => Strategy::Eager,
                    Some("split") => Strategy::Split,
                    other => {
                        return Err(format!("--strategy cautious|eager|split, not {other:?}"));
                    }
                }
            }
            _ => a.inputs.push(arg),
        }
    }
    Ok(a)
}

/// The EL classifier's result as a [`Classification`] (inconsistency: all unsatisfiable).
fn el_view(el: &nrese_reasoner::classify::Classification, ours: &Classification) -> Classification {
    Classification {
        classes: ours.classes.clone(),
        subsumptions: el.subsumptions.clone(),
        unsatisfiable: el.unsatisfiable.clone(),
        top: if ours.consistent {
            el.top.clone()
        } else {
            Vec::new()
        },
        consistent: ours.consistent,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a = args()?;
    let started = Instant::now();
    let (mut table, mut triples) = (Table::default(), Vec::new());
    for path in &a.inputs {
        load(path, &mut table, &mut triples)?;
    }
    let read = started.elapsed();
    let statements: Vec<Statement> = triples
        .iter()
        .map(|&triple| Statement { triple, graph: 0 })
        .collect();
    let (mut ours, mut profiles, mut el_runs) = (None, Vec::new(), Vec::new());
    let mut el_result = None;
    let snapshot = table.clone();
    for _ in 0..a.repeat {
        let started = Instant::now();
        let ontology = nrese_owl::read(&statements, &table);
        let owl = started.elapsed();
        match context::classify(&ontology, &a.options) {
            Ok((c, p)) => {
                ours = Some(c);
                profiles.push((owl, p));
            }
            Err(why) => {
                println!("unsupported: {why}");
                eprintln!("unsupported: {why}");
                std::process::exit(2);
            }
        }
        if a.compare {
            let mut scratch = snapshot.clone();
            let (c, p) = nrese_reasoner::classify::classify_profiled(
                &triples,
                &mut scratch,
                &|id| snapshot.terms[id as usize].starts_with('<'),
                a.options.threads,
            );
            el_result = Some(c);
            el_runs.push(p);
        }
    }
    let ours = ours.ok_or("no run")?;
    let started = Instant::now();
    let name = |t: Term| table.name(t);
    if let Some(path) = &a.out {
        let mut file = BufWriter::new(std::fs::File::create(path)?);
        file.write_all(ours.closure(&name).as_bytes())?;
        file.flush()?;
    }
    if let Some(path) = &a.tax {
        std::fs::write(path, ours.canonical(&name))?;
    }
    let write = started.elapsed();
    eprintln!(
        "{} triples; {} classes, {} subsumptions, {} unsatisfiable, consistent {}",
        triples.len(),
        ours.classes.len(),
        ours.subsumptions.len(),
        ours.unsatisfiable.len(),
        ours.consistent
    );
    if let Some(el) = &el_result {
        let theirs = el_view(el, &ours);
        let mine = Classification {
            classes: ours.classes.clone(),
            ..ours.clone()
        };
        if theirs == mine && el.skipped.is_empty() {
            eprintln!("el-compare equal");
        } else {
            let only_ours = mine
                .subsumptions
                .iter()
                .filter(|p| theirs.subsumptions.binary_search(p).is_err())
                .count();
            let only_el = theirs
                .subsumptions
                .iter()
                .filter(|p| mine.subsumptions.binary_search(p).is_err())
                .count();
            eprintln!(
                "el-compare differ: subsumptions only ours {only_ours}, only EL {only_el}; unsatisfiable {} vs {}; top {} vs {}; EL skipped {}",
                mine.unsatisfiable.len(),
                theirs.unsatisfiable.len(),
                mine.top.len(),
                theirs.top.len(),
                el.skipped.len()
            );
        }
        let pick = |f: &dyn Fn(&nrese_reasoner::classify::Profile) -> Duration| {
            median(el_runs.iter().map(f).collect())
        };
        eprintln!(
            "el-profile normalise={} prepare={} saturate={} assemble={} total={} contexts={} threads={}",
            ms(pick(&|p| p.normalise)),
            ms(pick(&|p| p.prepare)),
            ms(pick(&|p| p.saturate)),
            ms(pick(&|p| p.assemble)),
            ms(pick(&|p| p.normalise + p.prepare + p.saturate + p.assemble)),
            el_runs.last().map_or(0, |p| p.contexts),
            a.options.threads
        );
    }
    let last = profiles.last().map(|(_, p)| p.clone()).unwrap_or_default();
    let pick = |f: &dyn Fn(&(Duration, context::Profile)) -> Duration| {
        median(profiles.iter().map(f).collect())
    };
    let total = pick(&|(owl, p)| *owl + p.normalise + p.compile + p.saturate + p.assemble);
    let mut profile = last.clone();
    profile.normalise = pick(&|(_, p)| p.normalise);
    profile.compile = pick(&|(_, p)| p.compile);
    profile.saturate = pick(&|(_, p)| p.saturate);
    profile.assemble = pick(&|(_, p)| p.assemble);
    eprintln!(
        "profile read={} owl={} total={} write={} {}",
        ms(read),
        ms(pick(&|(owl, _)| *owl)),
        ms(total),
        ms(write),
        profile.line()
    );
    Ok(())
}
