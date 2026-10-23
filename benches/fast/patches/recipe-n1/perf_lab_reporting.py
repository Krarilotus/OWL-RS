#!/usr/bin/env python3
"""Observe legacy perf_lab's existing phases; never add a workload invocation.

The legacy main calls rematerialise once per process (as does the current default
reason_runs=1). Its MaterialisationReport already holds elapsed/inferred/rounds.
SHACL already measures every validation. Expose those actual observations in the
current JSON schema; the store, phase boundaries, loops and outcomes stay intact.
"""
from pathlib import Path
import sys

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


def patch(text: str) -> tuple[str, str]:
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


def main() -> int:
    path = Path(sys.argv[1])
    text, outcome = patch(path.read_text(encoding='utf-8'))
    if outcome == 'applied':
        path.write_text(text, encoding='utf-8')
    print(outcome)
    return 1 if outcome == 'no match' else 0


if __name__ == '__main__':
    sys.exit(main())
