//! Every OWL 2 DL ontology up to a bound (`nrese_owl::scope`), counted or written for the
//! reference reasoners (benches/reasoning/dl, the runner's `nrese.dl.Scope`).
//!
//! ```text
//! cargo run --release -p nrese-owl --example scope -- --scope 4,4,4 --count
//! cargo run --release -p nrese-owl --example scope -- --scope 4,4,4 --out FILE [--from N --to M]
//! ```
//!
//! `--scope a,b,c`: a one-axiom ontology has size at most `a`, a two-axiom one a total
//! size of at most `b`, and so on. `--count` prints the cases per number of axioms and how
//! many use each feature. `--out` writes the cases (`--from`, `--to`: a slice of them by
//! index; `--only FILE`: the indexes the file lists, one per line) one per line,
//! `index<TAB>document`, the document one line of the functional syntax
//! (`nrese_owl::scope::document`). `--covering T` takes the cases of the covering array of
//! strength `T` instead (`nrese_owl::scope::Covering::campaign`), numbered from 0, and
//! `--count` then prints the tuples it covers and those it doesn't.

use std::collections::BTreeMap;
use std::io::Write as _;

use nrese_owl::scope::{self, Feature, Scope};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut spec, mut out, mut count) = (String::from("3,3"), None, false);
    let (mut from, mut to) = (0u64, u64::MAX);
    let mut only: Option<std::collections::HashSet<u64>> = None;
    let mut strength: Option<usize> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scope" => spec = args.next().ok_or("--scope a,b,..")?,
            "--out" => out = args.next(),
            "--count" => count = true,
            "--from" => from = args.next().ok_or("--from N")?.parse()?,
            "--to" => to = args.next().ok_or("--to N")?.parse()?,
            "--covering" => strength = Some(args.next().ok_or("--covering T")?.parse()?),
            "--only" => {
                let text = std::fs::read_to_string(args.next().ok_or("--only FILE")?)?;
                only = Some(
                    text.split_whitespace()
                        .map(str::parse)
                        .collect::<Result<_, _>>()?,
                );
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let scope = Scope::parse(&spec)?;
    if let Some(strength) = strength {
        let t = scope::Covering::tables();
        let array = scope::covering(&t, scope::Covering::campaign(strength));
        if count {
            println!(
                "{} cases cover {} tuples of {strength} features; uncovered: {}",
                array.cases.len(),
                array.covered,
                array.uncovered.len()
            );
            for tuple in &array.uncovered {
                println!("  {tuple:?}");
            }
            return Ok(());
        }
        return write(
            &t,
            array.cases.into_iter(),
            out.ok_or("--out FILE or --count")?,
            from,
            to,
            only,
        );
    }
    let t = scope.tables();
    eprintln!(
        "scope {scope}: {} class expressions, {} data ranges, {} axioms",
        t.classes.len(),
        t.ranges.len(),
        t.axioms.len()
    );
    if count {
        let mut per_k: BTreeMap<usize, u64> = BTreeMap::new();
        let mut features = vec![0u64; Feature::COUNT];
        for case in scope::cases(&scope, &t) {
            *per_k.entry(case.axioms.len()).or_default() += 1;
            let f = case
                .axioms
                .iter()
                .fold(0u128, |m, &a| m | t.axiom_features(a));
            for (i, n) in features.iter_mut().enumerate() {
                if f & (1 << i) != 0 {
                    *n += 1;
                }
            }
        }
        for (k, n) in per_k {
            println!("{k} axioms: {n} ontologies");
        }
        for (f, n) in Feature::all().zip(&features) {
            println!("{:<34} {n}", f.name());
        }
        return Ok(());
    }
    let cases = scope::cases(&scope, &t);
    write(
        &t,
        cases,
        out.ok_or("--out FILE or --count")?,
        from,
        to,
        only,
    )
}

/// Writes `cases` (`from..to`, those `only` lists) to `path`, a line each.
fn write(
    t: &scope::Tables,
    cases: impl Iterator<Item = scope::Case>,
    path: String,
    from: u64,
    to: u64,
    only: Option<std::collections::HashSet<u64>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (u, names) = scope::universe();
    let mut file = std::io::BufWriter::new(std::fs::File::create(&path)?);
    let mut written = 0u64;
    for case in cases {
        if case.index < from {
            continue;
        }
        if case.index >= to {
            break;
        }
        if only.as_ref().is_some_and(|set| !set.contains(&case.index)) {
            continue;
        }
        let o = u.ontology(t, &case);
        writeln!(
            file,
            "{}\t{}",
            case.index,
            scope::document(&o, case.index, &names)
        )?;
        written += 1;
    }
    file.flush()?;
    eprintln!("{written} cases in {path}");
    Ok(())
}
