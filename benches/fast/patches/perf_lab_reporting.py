#!/usr/bin/env python3
"""Legacy reporting owner: default loaded/phase observation; private common201 recipe.

The legacy main calls rematerialise once per process (as does the current default
reason_runs=1). Its MaterialisationReport already holds elapsed/inferred/rounds.
SHACL already measures every validation. Expose those actual observations in the
current JSON schema; the store, phase boundaries, loops and outcomes stay intact.
"""
from pathlib import Path
import sys
import hashlib

MARKER = '// Legacy phase observations (perf_lab_reporting.py).'
REASON = '''    if let Some(ruleset) = args.reason {
        let started = Instant::now();
        let report = store.rematerialise(ruleset)?;
        eprintln!(
            "reasoned ({}): {} inferred in {:.2} s",
            ruleset.name(),
            report.inferred,
            started.elapsed().as_secs_f64()
        );
    }'''
REASON_OBSERVATION = r'''
        // The singleton's median is its one actual internal phase observation.
        let elapsed_ms = ms(report.elapsed);
        phase_sections.push_str(&format!(
            ",\n  \"reason\": {{\"inferred\": {}, \"rounds\": {}, \"runs\": 1, \
             \"p50_ms\": {elapsed_ms:.3}, \"max_ms\": {elapsed_ms:.3}, \"samples_ms\": [{elapsed_ms:.3}]}}",
            report.inferred, report.rounds,
        ));
'''
SHACL_END = '''        times.sort();
        eprintln!(
            "shacl: {results} results, p50 {:.2} ms (min {:.2})",
            ms(times[times.len() / 2]),
            ms(times[0])
        );'''
SHACL_OBSERVATION = r'''
        let samples: Vec<String> = times.iter().map(|t| format!("{:.3}", ms(*t))).collect();
        phase_sections.push_str(&format!(
            ",\n  \"shacl\": {{\"results\": {results}, \"runs\": {}, \"p50_ms\": {:.3}, \
             \"max_ms\": {:.3}, \"samples_ms\": [{}]}}",
            times.len(), ms(times[times.len() / 2]), ms(*times.last().unwrap()), samples.join(", "),
        ));
'''
WRITE = '        std::fs::write(path, json)?;'
JSON_END = r'\"queries\": [\n{}\n  ]\n}}\n",'
LOAD_START = '    let mut load_s = 0.0;'
LOAD_DONE = '        load_s = started.elapsed().as_secs_f64();'


def patch_loaded(text: str) -> tuple[str, str]:
    """Apply only the known legacy body; native structured reports remain byte-identical."""
    if MARKER in text:
        return text, 'present'
    if ('"reason",' in text and '"shacl",' in text
            and 'let done = store.rematerialise(program)?;' in text):
        return text, 'native'
    if any(text.count(anchor) != 1 for anchor in
           (REASON, SHACL_END, WRITE, JSON_END, LOAD_START, LOAD_DONE)):
        return text, 'no match'
    text = text.replace(LOAD_START, LOAD_START + '\n    let mut loaded: Option<u64> = None;')
    text = text.replace(LOAD_DONE, LOAD_DONE + '\n        loaded = Some(report.inserted);')
    observed = (f'    {MARKER}\n    let mut phase_sections = String::new();\n'
                + '    if let Some(inserted) = loaded {\n'
                + r'        phase_sections.push_str(&format!(",\n  \"loaded\": {inserted}"));' + '\n    }\n'
                + REASON[:-6] + REASON_OBSERVATION + '    }')
    text = text.replace(REASON, observed).replace(SHACL_END, SHACL_END + SHACL_OBSERVATION)
    text = text.replace(WRITE, '        let mut json = json;\n'
                        '        json.insert_str(json.len() - 2, &phase_sections);\n' + WRITE)
    return text, 'applied'


# Selection is materialized in this file's bytes before build_identity/cache lookup.
PROFILE = 'loaded'
COMMON_MARKER = '// COMMON21 Option B only:'
COMMON_PREIMAGES = ('3ab9dd6bd16fd5d1665434b454c3fa255227d606f3f5c8c98f57d46a9ea37408', 'c07a2af0bf571112d17665e145614700f7f3951a78fa8f268ca2f6eb56274bc4', '718eb81457f9ebb410582f748aab2f5b1fda83a76654cc664bf1bfb1ebd7110a', '1871ae9decacb2dfed4df45ac4a5201e90e85680a906e55bcb5dcb2e170b27eb')
COMMON_PAYLOAD = '// COMMON21 Option B only: diagnostics run after the original query samples.\nmod common201_reporting {\n    use super::*;\n    use nrese_sparql_results::{QueryResultsFormat, QueryResultsParser, SliceQueryResultsParserOutput};\n\n    #[derive(Default)]\n    struct Calls {\n        scalar_attempts: u64,\n        scalar_completed: u64,\n        explain_attempts: u64,\n        explain_completed: u64,\n        ordinary_attempts: u64,\n        ordinary_completed: u64,\n    }\n\n    fn prepared(text: &str) -> Result<PreparedQuery, String> {\n        let mut request = SparqlQueryRequest::all(text);\n        request.solutions_format = SolutionsResultFormat::Tsv;\n        PreparedQuery::parse(&request).map_err(|e| e.to_string())\n    }\n\n    fn explain(store: &StoreService, text: &str, calls: &mut Calls) -> Result<Vec<String>, String> {\n        let query = prepared(text)?;\n        calls.explain_attempts += 1;\n        let explanation = store.explain_query(&query, &CancellationToken::new()).map_err(|e| e.to_string())?;\n        calls.explain_completed += 1;\n        let mut operators = Vec::new();\n        for step in explanation.steps {\n            if !operators.contains(&step.operator) {\n                operators.push(step.operator);\n            }\n        }\n        Ok(operators)\n    }\n\n    pub(super) fn observe(store: &StoreService, text: &str, rows: u64) -> String {\n        let mut calls = Calls::default();\n        let mut errors = Vec::new();\n        let mut scalar = None;\n        let mut scalar_solutions = false;\n        if rows == 1 {\n            let value = (|| -> Result<Option<String>, String> {\n                let query = prepared(text)?;\n                if query.kind() != QueryResultKind::Solutions {\n                    return Ok(None);\n                }\n                scalar_solutions = true;\n                let mut output = Vec::new();\n                calls.scalar_attempts += 1;\n                store.run_query(&query, &CancellationToken::new(), &mut output).map_err(|e| e.to_string())?;\n                calls.scalar_completed += 1;\n                let parsed = QueryResultsParser::from_format(QueryResultsFormat::Tsv)\n                    .for_slice(&output).map_err(|e| e.to_string())?;\n                let SliceQueryResultsParserOutput::Solutions(mut solutions) = parsed else {\n                    return Err("scalar diagnostic returned non-solutions TSV".into());\n                };\n                let row = solutions.next().ok_or("scalar diagnostic returned no row")?\n                    .map_err(|e| e.to_string())?;\n                if solutions.next().is_some() {\n                    return Err("scalar diagnostic returned more than one row".into());\n                }\n                let cell = row.get_index(0).ok_or("scalar diagnostic first cell unbound")?;\n                Ok(Some(match cell {\n                    nrese_rdf::Term::Literal(literal) => literal.value().to_owned(),\n                    other => other.to_string(),\n                }))\n            })();\n            match value {\n                Ok(value) => scalar = value,\n                Err(error) => errors.push(format!("scalar: {error}")),\n            }\n        }\n\n        // Match candidate rewrites()\'s executing EXPLAIN, without inventing rewrites.\n        if let Err(error) = explain(store, text, &mut calls) {\n            errors.push(format!("first explain: {error}"));\n        }\n\n        // Match run_query_dl\'s real non-DL work; main has no status-reporting API.\n        let ordinary = (|| -> Result<(), String> {\n            let request = SparqlQueryRequest::all(text);\n            let query = PreparedQuery::parse(&request).map_err(|e| e.to_string())?;\n            calls.ordinary_attempts += 1;\n            store.run_query(&query, &CancellationToken::new(), CountingSink::default())\n                .map_err(|e| e.to_string())?;\n            calls.ordinary_completed += 1;\n            Ok(())\n        })();\n        if let Err(error) = ordinary {\n            errors.push(format!("ordinary query: {error}"));\n        }\n\n        let operators = match explain(store, text, &mut calls) {\n            Ok(operators) => operators,\n            Err(error) => {\n                errors.push(format!("second explain: {error}"));\n                Vec::new()\n            }\n        };\n        let count = serde_json::json!({\n            "profile": "common201", "queries_observed": 1,\n            "measured_rows": rows, "scalar_solutions_kind": scalar_solutions,\n            "scalar_attempts": calls.scalar_attempts, "scalar_completed": calls.scalar_completed,\n            "explain_attempts": calls.explain_attempts, "explain_completed": calls.explain_completed,\n            "ordinary_attempts": calls.ordinary_attempts, "ordinary_completed": calls.ordinary_completed,\n            "executions_attempted": calls.scalar_attempts + calls.explain_attempts + calls.ordinary_attempts,\n            "executions_completed": calls.scalar_completed + calls.explain_completed + calls.ordinary_completed,\n            "new_timed_samples": 0, "status_available": false, "rewrites_available": false,\n            "errors": errors,\n        });\n        let mut extra = format!(",\\n      \\"operators\\": {},\\n      \\"common201\\": {count}",\n            serde_json::to_string(&operators).expect("operator strings serialize"));\n        if let Some(value) = scalar {\n            extra.push_str(&format!(",\\n      \\"value\\": {}",\n                serde_json::to_string(&value).expect("scalar string serializes")));\n        }\n        extra\n    }\n}\n'
QUERY_REPORT = '        report.push(format!(\n            "    {{\\n      \\"name\\": \\"{name}\\",\\n      \\"rows\\": {},\\n      \\"p50_ms\\": {p50:.3},\\n      \\"min_ms\\": {min:.3},\\n      \\"max_ms\\": {max:.3}\\n    }}",\n            m.rows\n        ));'


def patch_common(text: str) -> tuple[str, str]:
    if COMMON_MARKER in text:
        return text, 'present'
    if ('"reason",' in text and '"shacl",' in text
            and 'let done = store.rematerialise(program)?;' in text):
        return text, 'native'
    if hashlib.sha256(text.encode()).hexdigest() not in COMMON_PREIMAGES:
        return text, 'no match'
    text, outcome = patch_loaded(text)
    if outcome not in ('applied', 'present') or text.count(QUERY_REPORT) != 1:
        return text, 'no match'
    diagnostics = '        let diagnostics = common201_reporting::observe(&store, &text, m.rows);\n'
    report = QUERY_REPORT.replace(r'\n    }}",', r'{diagnostics}\n    }}",')
    text = text.replace(QUERY_REPORT, diagnostics + report)
    anchor = '/// Counts bytes and lines written, discarding the data.'
    if text.count(anchor) != 1:
        return text, 'no match'
    return text.replace(anchor, COMMON_PAYLOAD + '\n' + anchor), 'applied'


def patch(text: str) -> tuple[str, str]:
    return patch_common(text) if PROFILE == 'common201' else patch_loaded(text)


def main() -> int:
    path = Path(sys.argv[1])
    text, outcome = patch(path.read_text(encoding='utf-8'))
    if outcome == 'applied':
        path.write_text(text, encoding='utf-8')
    print(outcome)
    return 1 if outcome == 'no match' else 0


if __name__ == '__main__':
    sys.exit(main())
