//! The QL printer against NRESE (docs/design/ql-rewriting.md §8): each query of the
//! query directories printed as standard SPARQL 1.1 in both forms, run on a store holding
//! the same files without any reasoning, and its rows compared with NRESE's own (`owl2-rl`,
//! the QL rewriting on).
//!
//! ```text
//! cargo run --release -p nrese-store --example ql_print_check -- \
//!     --queries DIR [--queries DIR]... [--out DIR] input.{nt,ttl,...}...
//! ```
//!
//! Prints one line per query and form: NRESE's rows, the printed query's rows on the store
//! without reasoning, whether the rows are the same (as bags), NRESE's completeness, the
//! printed query's size and the times (printing, NRESE, the printed query); or why the
//! query isn't expressible. `--out` writes the printed queries (`NAME.paths.rq`,
//! `NAME.values.rq`). Exits with 1 if any printed query answers differently.

use std::path::PathBuf;
use std::time::Instant;

use nrese_reasoner::rulesets::Ruleset;
use nrese_sparql::ql::{PrintForm, Printed};
use nrese_store::{
    BulkLoadRequest, GraphTarget, QlRewritingMode, SolutionsResultFormat, SparqlQueryRequest,
    StoreConfig, StoreService,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (mut dirs, mut files, mut out) = (Vec::new(), Vec::new(), None);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--queries" => dirs.push(PathBuf::from(args.next().ok_or("--queries DIR")?)),
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out DIR")?)),
            _ => files.push(PathBuf::from(arg)),
        }
    }
    if dirs.is_empty() || files.is_empty() {
        return Err("usage: ql_print_check --queries DIR [--out DIR] FILE...".into());
    }
    let load = |mode| -> Result<StoreService, Box<dyn std::error::Error>> {
        let store = StoreService::new(StoreConfig {
            ql_rewriting: mode,
            ..StoreConfig::in_memory()
        })?;
        store.bulk_load(&BulkLoadRequest {
            files: files.clone(),
            replace: false,
            graph: GraphTarget::DefaultGraph,
            skip_errors: false,
        })?;
        Ok(store)
    };
    let started = Instant::now();
    let nrese = load(QlRewritingMode::On)?;
    nrese.rematerialise(Ruleset::Owl2Rl)?;
    let plain = load(QlRewritingMode::Off)?;
    eprintln!(
        "loaded and reasoned in {:.1} s",
        started.elapsed().as_secs_f64()
    );
    if let Some(dir) = &out {
        std::fs::create_dir_all(dir)?;
    }

    let mut queries: Vec<PathBuf> = Vec::new();
    for dir in &dirs {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "rq") {
                queries.push(path);
            }
        }
    }
    queries.sort();
    println!(
        "{:<10}{:<8}{:>9}{:>9}  {:<5}{:<12}{:>9}{:>9}{:>9}{:>9}",
        "query",
        "form",
        "nrese",
        "printed",
        "same",
        "status",
        "bytes",
        "print ms",
        "nrese ms",
        "plain ms"
    );
    let mut differ = 0;
    for path in &queries {
        let name = path.file_stem().unwrap_or_default().to_string_lossy();
        let text = std::fs::read_to_string(path)?;
        let t = Instant::now();
        let expected = rows(&nrese, &text)?;
        let nrese_ms = t.elapsed().as_secs_f64() * 1e3;
        for form in [PrintForm::Paths, PrintForm::Values] {
            let t = Instant::now();
            let printed = nrese.print_query(&text, form)?;
            let print_ms = t.elapsed().as_secs_f64() * 1e3;
            match printed {
                Printed::NotExpressible(reasons) => {
                    println!(
                        "{name:<10}{:<8}{:>9}{:>9}  not expressible: {}",
                        form.name(),
                        expected.len(),
                        "-",
                        reasons.join("; ")
                    );
                }
                Printed::Query {
                    text: printed,
                    completeness,
                } => {
                    if let Some(dir) = &out {
                        std::fs::write(
                            dir.join(format!("{name}.{}.rq", form.name())),
                            format!(
                                "# NRESE completeness: {}\n{printed}\n",
                                completeness.header()
                            ),
                        )?;
                    }
                    let t = Instant::now();
                    let got = rows(&plain, &printed)?;
                    let plain_ms = t.elapsed().as_secs_f64() * 1e3;
                    let same = got == expected;
                    differ += usize::from(!same);
                    println!(
                        "{name:<10}{:<8}{:>9}{:>9}  {:<5}{:<12}{:>9}{print_ms:>9.1}{nrese_ms:>9.1}{plain_ms:>9.1}",
                        form.name(),
                        expected.len(),
                        got.len(),
                        if same { "yes" } else { "NO" },
                        completeness.as_str(),
                        printed.len(),
                    );
                }
            }
        }
    }
    if differ > 0 {
        eprintln!("{differ} printed queries answer differently");
        std::process::exit(1);
    }
    Ok(())
}

/// A query's rows as sorted TSV lines (a bag).
fn rows(store: &StoreService, query: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut request = SparqlQueryRequest::all(query);
    request.solutions_format = SolutionsResultFormat::Tsv;
    let result = store.execute_query(&request)?;
    let text = String::from_utf8(result.payload)?;
    let mut lines: Vec<String> = text.lines().skip(1).map(str::to_owned).collect();
    lines.sort_unstable();
    Ok(lines)
}
