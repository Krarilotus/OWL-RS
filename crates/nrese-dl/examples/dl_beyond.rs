//! Inputs outside OWL 2 DL: what the reader finds in them, for the DL lab.
//!
//! ```text
//! cargo run --release -p nrese-dl --example dl_beyond -- --survey input...
//! cargo run --release -p nrese-dl --example dl_beyond -- --decide [--verbose]
//!     [--timeout SECS] [--max-rounds N] [--max-instances N] input...
//! ```
//!
//! `--decide`: consistency by `nrese_dl::beyond`, a line per input (see `decide`).
//!
//! `--survey`: a line per input, `file<TAB>axioms<TAB>diagnostics<TAB>fatal<TAB>kinds<TAB>left
//! out`, the kinds as `kind:what=count` separated by `;`, then the normalisation's
//! unsupported axioms by reason (`reason=count`; `-` for none, `error: …` where the input
//! can't be read).

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use nrese_owl::Diagnostic;

#[path = "support/read.rs"]
mod read;

/// A diagnostic's kind and what, for counting.
fn kind(d: &Diagnostic) -> String {
    match d {
        Diagnostic::Malformed { what, .. } => format!("Malformed:{what}"),
        Diagnostic::SharedBlankNode { .. } => "SharedBlankNode".into(),
        Diagnostic::BrokenList { .. } => "BrokenList".into(),
        Diagnostic::UndeclaredProperty { .. } => "UndeclaredProperty".into(),
        Diagnostic::AmbiguousProperty { .. } => "AmbiguousProperty".into(),
        Diagnostic::Unsupported { what, .. } => format!("Unsupported:{what}"),
        Diagnostic::NotOwl { .. } => "NotOwl".into(),
        Diagnostic::Unused { .. } => "Unused".into(),
        Diagnostic::NonSimpleProperty { what, .. } => format!("NonSimpleProperty:{what}"),
        Diagnostic::AnonymousIndividual { .. } => "AnonymousIndividual".into(),
        Diagnostic::Syntax { what, .. } => format!("Syntax:{what}"),
        Diagnostic::Rule { .. } => "Rule".into(),
        Diagnostic::RepeatedOperand => "RepeatedOperand".into(),
        Diagnostic::Reserved { .. } => "Reserved".into(),
    }
}

fn survey(path: &str) -> String {
    let mut table = read::Table::default();
    let o = match read::ontology(&[path.to_owned()], &mut table) {
        Ok(o) => o,
        Err(e) => return format!("{path}\t-\t-\t-\terror: {e}"),
    };
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for d in &o.diagnostics {
        *kinds.entry(kind(d)).or_default() += 1;
    }
    let fatal = o.diagnostics.iter().filter(|d| d.is_fatal()).count();
    let kinds = if kinds.is_empty() {
        "-".to_owned()
    } else {
        kinds
            .iter()
            .map(|(k, n)| format!("{k}={n}"))
            .collect::<Vec<_>>()
            .join(";")
    };
    let normalised = nrese_owl::normalise(&nrese_dl::tableau::prepared(&o));
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, why) in &normalised.unsupported {
        *reasons.entry(why).or_default() += 1;
    }
    let left = if reasons.is_empty() {
        "-".to_owned()
    } else {
        reasons
            .iter()
            .map(|(k, n)| format!("{k}={n}"))
            .collect::<Vec<_>>()
            .join(";")
    };
    format!(
        "{path}\t{}\t{}\t{fatal}\t{kinds}\t{left}",
        o.axioms.len(),
        o.diagnostics.len()
    )
}

/// The decision for one input: a line `file<TAB>verdict<TAB>rounds<TAB>chased<TAB>grounded
/// <TAB>millis<TAB>why`, then, with `--verbose`, what was left out, repaired and tried.
fn decide(path: &str, options: &nrese_dl::beyond::Options, verbose: bool) -> String {
    use nrese_dl::beyond::Verdict;
    let started = Instant::now();
    let mut table = read::Table::default();
    let o = match read::ontology(&[path.to_owned()], &mut table) {
        Ok(o) => o,
        Err(e) => return format!("{path}\terror\t-\t-\t-\t-\t{e}"),
    };
    let d = nrese_dl::beyond::consistency(&o, options);
    let (verdict, why) = match &d.verdict {
        Verdict::Consistent { elements, lifted } => (
            "consistent",
            format!(
                "certified model of {elements} elements{}",
                if *lifted { ", lifted" } else { "" }
            ),
        ),
        Verdict::Inconsistent { instances } => (
            "inconsistent",
            format!("the DL part with {instances} rule instances"),
        ),
        Verdict::Undecided(why) => ("undecided", why.clone()),
    };
    let mut out = format!(
        "{path}\t{verdict}\t{}\t{}\t{}\t{}\t{why}",
        d.rounds,
        d.chased,
        d.grounded,
        started.elapsed().as_millis()
    );
    if verbose {
        for l in &d.left_out {
            let checked = if l.checked { " (checked)" } else { "" };
            out.push_str(&format!(
                "\n  left out{checked}: {} [{}]",
                l.what, l.restriction
            ));
        }
        for r in &d.repairs {
            out.push_str(&format!("\n  repaired: {r}"));
        }
        for t in &d.tried {
            out.push_str(&format!("\n  tried: {t}"));
        }
    }
    out
}

/// The classification of one input (`nrese_dl::beyond::classify`): a line
/// `file<TAB>consistency<TAB>exact<TAB>subsumptions<TAB>unsatisfiable<TAB>millis<TAB>why`;
/// with `tax`, the canonical taxonomy in `DIR/<file name>.tax`.
fn classify(
    path: &str,
    options: &nrese_dl::beyond::Options,
    tax: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    use nrese_dl::beyond::Verdict;
    let started = Instant::now();
    let mut table = read::Table::default();
    let o = read::ontology(&[path.to_owned()], &mut table)?;
    let with = nrese_dl::classify::Options {
        timeout: options.timeout,
        ..nrese_dl::classify::Options::default()
    };
    let c = nrese_dl::beyond::classify::classify(&o, options, &with);
    if let Some(dir) = tax {
        let file = std::path::Path::new(path)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = |t: nrese_owl::Term| table.name(t);
        std::fs::write(
            std::path::Path::new(dir).join(format!("{file}.tax")),
            c.classification.canonical(&name),
        )?;
    }
    let verdict = match &c.decision.verdict {
        Verdict::Consistent { .. } => "consistent",
        Verdict::Inconsistent { .. } => "inconsistent",
        Verdict::Undecided(_) => "undecided",
    };
    Ok(format!(
        "{path}\t{verdict}\t{}\t{}\t{}\t{}\t{}",
        c.exact,
        c.classification.subsumptions.len(),
        c.classification.unsatisfiable.len(),
        started.elapsed().as_millis(),
        c.why.join("; ")
    ))
}

/// A minimal inconsistent part of an inconsistent input (`nrese_dl::beyond::justify`) as
/// a functional-syntax document, its declarations included; a comment line where the
/// input isn't found inconsistent.
fn justify(
    path: &str,
    options: &nrese_dl::beyond::Options,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut table = read::Table::default();
    let o = read::ontology(&[path.to_owned()], &mut table)?;
    let Some((axioms, rules)) = nrese_dl::beyond::justify(&o, options) else {
        return Ok(format!("# {path}: not found inconsistent\n"));
    };
    let name = |t: nrese_owl::Term| table.functional(t);
    let mut out = format!(
        "# {path}: {} axioms, {} rules\n\
         Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
         Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
         Ontology(\n",
        axioms.len(),
        rules.len()
    );
    let declarations = o
        .axioms
        .iter()
        .filter(|a| matches!(a, nrese_owl::Axiom::Declaration(..)));
    for a in declarations {
        out.push_str(&o.functional(a, &name));
        out.push('\n');
    }
    for &k in &axioms {
        out.push_str(&o.functional(&o.axioms[k], &name));
        out.push('\n');
    }
    for &k in &rules {
        out.push_str(&o.functional_rule(&o.rules[k], &name));
        out.push('\n');
    }
    out.push_str(")\n");
    Ok(out)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut inputs = Vec::new();
    let mut mode = None;
    let mut verbose = false;
    let mut tax: Option<String> = None;
    let mut options = nrese_dl::beyond::Options::default();
    while let Some(arg) = args.next() {
        let mut number = |what: &str| -> Result<u64, String> {
            args.next()
                .and_then(|n| n.parse().ok())
                .ok_or(format!("{what} N"))
        };
        match arg.as_str() {
            "--survey" => mode = Some("survey"),
            "--decide" => mode = Some("decide"),
            "--classify" => mode = Some("classify"),
            "--justify" => mode = Some("justify"),
            "--tax" => tax = args.next(),
            "--verbose" => verbose = true,
            "--timeout" => options.timeout = Some(Duration::from_secs(number("--timeout")?)),
            "--max-rounds" => options.max_rounds = Some(number("--max-rounds")? as usize),
            "--max-instances" => options.max_instances = Some(number("--max-instances")? as usize),
            "--max-tests" => options.max_tests = Some(number("--max-tests")? as usize),
            other if other.starts_with("--") => {
                return Err(format!("unknown option {other}").into());
            }
            _ => inputs.push(arg),
        }
    }
    match mode {
        Some("survey") => {
            for path in &inputs {
                println!("{}", survey(path));
            }
            Ok(())
        }
        Some("decide") => {
            for path in &inputs {
                println!("{}", decide(path, &options, verbose));
            }
            Ok(())
        }
        Some("classify") => {
            for path in &inputs {
                println!("{}", classify(path, &options, tax.as_deref())?);
            }
            Ok(())
        }
        Some("justify") => {
            for path in &inputs {
                print!("{}", justify(path, &options)?);
            }
            Ok(())
        }
        _ => Err("a mode: --survey, --decide, --classify or --justify".into()),
    }
}
