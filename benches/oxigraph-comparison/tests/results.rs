//! Every result file (JSON, XML, TSV) of the pinned W3C SPARQL 1.0, 1.1 and 1.2 suites read
//! by `nrese-sparql-results` and by `sparesults` (with `sparql-12`): the same variables and
//! solutions, or both refusing the file. Terms are compared in N-Triples form. Without the
//! suites in full the test fails where they are required (`NRESE_W3C_REQUIRED`) and else
//! says it doesn't qualify.

use oxigraph_comparison::w3c;

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
    let Some(files) = w3c::admitted(w3c::sparql(&["srj", "srx", "tsv"])) else {
        return;
    };
    let (mut same, mut both_refuse, mut differences) = (0, 0, Vec::new());
    for file in &files {
        let bytes = file.bytes.as_slice();
        let (a, b) = match file.path.extension().and_then(|e| e.to_str()) {
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
                file.name
            )),
        }
    }
    println!(
        "{} result files ({}): {same} read alike, {both_refuse} refused by both, {} differ",
        files.len(),
        w3c::counts(&files),
        differences.len()
    );
    for difference in &differences {
        println!("{difference}");
    }
    assert!(differences.is_empty());
}
