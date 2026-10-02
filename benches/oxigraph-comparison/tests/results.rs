//! Every W3C SPARQL 1.1 and 1.2 result file (JSON, XML, TSV) read by `nrese-sparql-results`
//! and by `sparesults` (with `sparql-12`): the same variables and solutions, or both
//! refusing the file. Terms are compared in N-Triples form.

use std::path::{Path, PathBuf};

fn root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/rdf-tests/sparql");
    root.join("sparql12").is_dir().then_some(root)
}

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("srj" | "srx" | "tsv")
        ) {
            out.push(path);
        }
    }
}

/// Variables, and per solution its bindings as `name=term` in N-Triples form (sorted).
type Read = Result<(Vec<String>, Vec<Vec<String>>), String>;

fn ours(format: nrese_sparql_results::QueryResultsFormat, bytes: &[u8]) -> Read {
    use nrese_sparql_results::{QueryResultsParser, SliceQueryResultsParserOutput};
    match QueryResultsParser::from_format(format)
        .for_slice(bytes)
        .map_err(|e| e.to_string())?
    {
        SliceQueryResultsParserOutput::Boolean(b) => Ok((vec![format!("boolean {b}")], Vec::new())),
        SliceQueryResultsParserOutput::Solutions(parser) => {
            let variables = parser
                .variables()
                .iter()
                .map(|v| v.as_str().to_owned())
                .collect();
            let mut rows = Vec::new();
            for solution in parser {
                let solution = solution.map_err(|e| e.to_string())?;
                let mut row: Vec<String> = solution
                    .iter()
                    .map(|(v, t)| format!("{}={t}", v.as_str()))
                    .collect();
                row.sort();
                rows.push(row);
            }
            Ok((variables, rows))
        }
    }
}

fn theirs(format: sparesults::QueryResultsFormat, bytes: &[u8]) -> Read {
    use sparesults::{QueryResultsParser, SliceQueryResultsParserOutput};
    match QueryResultsParser::from_format(format)
        .for_slice(bytes)
        .map_err(|e| e.to_string())?
    {
        SliceQueryResultsParserOutput::Boolean(b) => Ok((vec![format!("boolean {b}")], Vec::new())),
        SliceQueryResultsParserOutput::Solutions(parser) => {
            let variables = parser
                .variables()
                .iter()
                .map(|v| v.as_str().to_owned())
                .collect();
            let mut rows = Vec::new();
            for solution in parser {
                let solution = solution.map_err(|e| e.to_string())?;
                let mut row: Vec<String> = solution
                    .iter()
                    .map(|(v, t)| format!("{}={t}", v.as_str()))
                    .collect();
                row.sort();
                rows.push(row);
            }
            Ok((variables, rows))
        }
    }
}

#[test]
fn w3c_result_files_read_alike() {
    let Some(root) = root() else {
        eprintln!("skipped: run scripts/fetch-w3c-tests.sh");
        return;
    };
    let mut paths = Vec::new();
    files(&root, &mut paths);
    assert!(paths.len() > 300, "only {} result files", paths.len());
    let (mut same, mut both_refuse, mut differences) = (0, 0, Vec::new());
    for path in &paths {
        let bytes = std::fs::read(path).unwrap();
        let (a, b) = match path.extension().and_then(|e| e.to_str()) {
            Some("srj") => (
                ours(nrese_sparql_results::QueryResultsFormat::Json, &bytes),
                theirs(sparesults::QueryResultsFormat::Json, &bytes),
            ),
            Some("srx") => (
                ours(nrese_sparql_results::QueryResultsFormat::Xml, &bytes),
                theirs(sparesults::QueryResultsFormat::Xml, &bytes),
            ),
            _ => (
                ours(nrese_sparql_results::QueryResultsFormat::Tsv, &bytes),
                theirs(sparesults::QueryResultsFormat::Tsv, &bytes),
            ),
        };
        match (&a, &b) {
            (Ok(x), Ok(y)) if x == y => same += 1,
            (Err(_), Err(_)) => both_refuse += 1,
            _ => differences.push(format!(
                "{}:\n  nrese:     {a:?}\n  sparesults: {b:?}",
                path.strip_prefix(&root).unwrap().display()
            )),
        }
    }
    println!(
        "{} result files: {same} read alike, {both_refuse} refused by both, {} differ",
        paths.len(),
        differences.len()
    );
    for difference in &differences {
        println!("{difference}");
    }
    assert!(differences.is_empty());
}
