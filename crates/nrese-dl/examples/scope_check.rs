//! NRESE's answers to every case of a scope (`nrese_owl::scope`, `nrese_dl::scope`): the
//! recorded campaign of docs/design/owl2-dl.md §11, compared with the references by
//! `benches/reasoning/dl/scope.py`.
//!
//! ```text
//! cargo run --release -p nrese-dl --example scope_check -- --scope 4,4,4 --out FILE [--from N --to M]
//! ```
//!
//! Each case is rendered as the references get it (`nrese_owl::scope::document`), read
//! back by the functional-syntax reader and answered; the cases run in parallel on every
//! core. A line per case, in order: `index<TAB>code<TAB>certified<TAB>millis`. `--covering
//! T`: the cases of the covering array of strength `T` instead
//! (`nrese_owl::scope::Covering::campaign`).

use std::io::Write as _;
use std::time::Instant;

use nrese_owl::scope::{self, Scope};
use rayon::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut spec, mut out) = (String::from("3,3"), None);
    let (mut from, mut to) = (0u64, u64::MAX);
    let mut strength: Option<usize> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scope" => spec = args.next().ok_or("--scope a,b,..")?,
            "--out" => out = args.next(),
            "--from" => from = args.next().ok_or("--from N")?.parse()?,
            "--to" => to = args.next().ok_or("--to N")?.parse()?,
            "--covering" => strength = Some(args.next().ok_or("--covering T")?.parse()?),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let scope = Scope::parse(&spec)?;
    let (t, array) = match strength {
        Some(s) => {
            let t = scope::Covering::tables();
            let array = scope::covering(&t, scope::Covering::campaign(s)).cases;
            (t, Some(array))
        }
        None => (scope.tables(), None),
    };
    let (u, names) = scope::universe();
    let path = out.ok_or("--out FILE")?;
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
    let started = Instant::now();
    let mut done = 0u64;
    let mut batch: Vec<(u64, String)> = Vec::new();
    let flush = |batch: &mut Vec<(u64, String)>,
                 file: &mut std::io::BufWriter<std::fs::File>|
     -> std::io::Result<()> {
        let lines: Vec<String> = batch
            .par_iter()
            .map(|(index, document)| {
                let at = Instant::now();
                let (code, certified) = match nrese_dl::scope::read(document) {
                    Ok((o, names)) => {
                        let a = nrese_dl::scope::answer(&o, &names);
                        (a.code, a.certified)
                    }
                    Err(why) => (format!("p {why}"), "-".repeat(11)),
                };
                let code = code.replace(['\t', '\n'], " ");
                format!("{index}\t{code}\t{certified}\t{}", at.elapsed().as_millis())
            })
            .collect();
        for l in lines {
            writeln!(file, "{l}")?;
        }
        batch.clear();
        file.flush()
    };
    let cases: Box<dyn Iterator<Item = scope::Case>> = match array {
        Some(array) => Box::new(array.into_iter()),
        None => Box::new(scope::cases(&scope, &t)),
    };
    for case in cases {
        if case.index < from {
            continue;
        }
        if case.index >= to {
            break;
        }
        let o = u.ontology(&t, &case);
        batch.push((case.index, scope::document(&o, case.index, &names)));
        if batch.len() == 20_000 {
            done += batch.len() as u64;
            flush(&mut batch, &mut file)?;
            eprintln!("{done} cases, {:.0} s", started.elapsed().as_secs_f64());
        }
    }
    done += batch.len() as u64;
    flush(&mut batch, &mut file)?;
    eprintln!(
        "{done} cases in {:.0} s: {path}",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
