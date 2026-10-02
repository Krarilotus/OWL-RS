//! The fuzz targets on stable Rust: each target reads its corpus (the W3C test suites in
//! `.cache`, fetched by `scripts/fetch-w3c-tests.sh`, and the seeds below) and mutations
//! of it, drawn from `NRESE_FUZZ_SEED`. A failing input is saved under
//! `tmp/fuzz-findings/` (the target's name and a hash) and the test fails with the list.
//!
//! `NRESE_FUZZ_CASES` sets the mutations per corpus file (default 8), `NRESE_FUZZ_FILES`
//! the corpus files per target (default 300), `NRESE_FUZZ_TARGET` one target by name.

use std::path::{Path, PathBuf};

use nrese_fuzz::Target;

/// Inputs every target starts from, besides the W3C files: small documents that reach the
/// interesting parts of each grammar.
fn seeds(target: Target) -> Vec<&'static [u8]> {
    match target {
        Target::NTriples => vec![
            b"<http://a/s> <http://a/p> \"x\\u00e9\\n\"@en .\n_:b <http://a/p> \"1\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n",
            b"<< <http://a/s> <http://a/p> <http://a/o> >> <http://a/q> \"v\"@en--ltr .\n",
        ],
        Target::NQuads => vec![
            b"<http://a/s> <http://a/p> <http://a/o> <http://a/g> .\n_:x <http://a/p> \"y\" _:g .\n",
        ],
        Target::Turtle => vec![
            b"@prefix : <http://a/> . @base <http://b/> .\n:s :p ( 1 2.5 3e1 ) ; :q [ :r \"\"\"long\nstring\"\"\" ] , <rel> .\n",
            b"PREFIX : <http://a/>\n:s :p :o ~ :r {| :q 1 |} .\nVERSION \"1.2\"\n",
        ],
        Target::TriG => vec![
            b"@prefix : <http://a/> .\n:g { :s :p :o . } GRAPH _:b { :s :p [ :q true ] }\n{ :x :y :z }\n",
        ],
        Target::N3 => vec![
            b"@prefix : <http://a/> .\n{ ?x :p ?y } => { ?y :q ?x } .\n:s :p :o ; is :r of :t .\n",
        ],
        Target::RdfXml => vec![
            b"<?xml version=\"1.0\"?>\n<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\" xmlns:a=\"http://a/\" xml:base=\"http://b/\">\n<rdf:Description rdf:about=\"s\" a:p=\"v\"><a:q rdf:parseType=\"Collection\"><rdf:Description rdf:about=\"x\"/></a:q><a:r xml:lang=\"en\">t</a:r></rdf:Description>\n</rdf:RDF>\n",
        ],
        Target::JsonLd => vec![
            b"{\"@context\": {\"a\": \"http://a/\", \"p\": {\"@id\": \"a:p\", \"@type\": \"@id\"}}, \"@id\": \"a:s\", \"p\": \"a:o\", \"a:q\": [{\"@value\": 1}, {\"@list\": [1, 2]}], \"@graph\": [{\"@id\": \"a:x\", \"a:y\": \"z\"}]}",
        ],
        Target::SparqlQuery => vec![
            b"PREFIX : <http://a/> SELECT ?x (COUNT(*) AS ?n) WHERE { ?x :p/:q* ?y OPTIONAL { ?y :r ?z FILTER(?z > 3) } MINUS { ?x a :C } } GROUP BY ?x HAVING (?n > 1) ORDER BY DESC(?n) LIMIT 10",
            b"CONSTRUCT { ?s <http://a/p> ?o } WHERE { SERVICE SILENT <http://e/> { ?s ?p ?o } VALUES ?o { 1 \"x\"@en UNDEF } BIND(STRLEN(?o) AS ?l) }",
            b"SELECT * WHERE { << ?s ?p ?o >> ?q ?v . ?s ?p ?o ~ ?r {| ?a ?b |} FILTER EXISTS { ?s ?p ?o } }",
        ],
        Target::SparqlUpdate => vec![
            b"PREFIX : <http://a/> INSERT DATA { :s :p :o . GRAPH :g { :s :p 1 } } ; DELETE { ?s :p ?o } INSERT { ?s :q ?o } WHERE { ?s :p ?o } ; CLEAR SILENT GRAPH :g",
            b"WITH <http://a/g> DELETE WHERE { ?s ?p ?o } ; LOAD <http://a/x> INTO GRAPH <http://a/y> ; COPY DEFAULT TO <http://a/z>",
        ],
        Target::ResultsJson => vec![
            b"{\"head\":{\"vars\":[\"x\",\"y\"]},\"results\":{\"bindings\":[{\"x\":{\"type\":\"uri\",\"value\":\"http://a/\"},\"y\":{\"type\":\"literal\",\"value\":\"1\",\"datatype\":\"http://www.w3.org/2001/XMLSchema#integer\"}},{\"x\":{\"type\":\"bnode\",\"value\":\"b\"}}]}}",
            b"{\"head\":{},\"boolean\":true}",
        ],
        Target::ResultsXml => vec![
            b"<?xml version=\"1.0\"?><sparql xmlns=\"http://www.w3.org/2005/sparql-results#\"><head><variable name=\"x\"/></head><results><result><binding name=\"x\"><literal xml:lang=\"en\">a</literal></binding></result></results></sparql>",
        ],
        Target::ResultsTsv => vec![
            b"?x\t?y\n<http://a/>\t\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>\n_:b\t\n",
        ],
    }
}

fn env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(default)
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The target's corpus files under `.cache`, sorted, at most `limit`.
fn corpus(target: Target, limit: usize) -> Vec<Vec<u8>> {
    let mut files = Vec::new();
    let mut stack = vec![root().join(".cache")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != ".git") {
                    stack.push(path);
                }
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| target.extensions().contains(&e))
            {
                files.push(path);
            }
        }
    }
    files.sort();
    // Spread over the suites rather than the first directory's files.
    let step = files.len().div_ceil(limit.max(1)).max(1);
    files
        .iter()
        .step_by(step)
        .filter_map(|path| std::fs::read(path).ok())
        .collect()
}

/// A xorshift generator.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n.max(1) as u64) as usize
    }
}

/// Tokens worth inserting: the grammars' delimiters and escapes.
const TOKENS: &[&[u8]] = &[
    b"<",
    b">",
    b"\"",
    b"\"\"\"",
    b"'",
    b"\\",
    b"\\u",
    b"\\U0010FFFF",
    b"{",
    b"}",
    b"[",
    b"]",
    b"(",
    b")",
    b"<<",
    b">>",
    b"{|",
    b"|}",
    b"~",
    b"@",
    b"@prefix",
    b"PREFIX",
    b"_:",
    b"^^",
    b".",
    b";",
    b",",
    b"#",
    b"\n",
    b"\t",
    b"\r",
    b"\0",
    b"\xff",
    b"\xc3",
    b"?",
    b"$",
    b"*",
    b"rdf:",
    b"@id",
    b"@context",
    b"@type",
    b"@list",
    b"@graph",
    b"<rdf:RDF",
    b"/>",
    b"</",
    b"&amp;",
    b"&#",
    b"]]>",
    b"OPTIONAL",
    b"FILTER",
    b"SERVICE",
    b"GRAPH",
    b"{}",
    b"\"x\"@",
    b"1e999",
    b"-0",
    b"9999999999999999999999",
];

fn mutate(rng: &mut Rng, input: &[u8], other: &[u8]) -> Vec<u8> {
    let mut out = input.to_vec();
    for _ in 0..1 + rng.below(4) {
        let at = rng.below(out.len() + 1);
        match rng.below(7) {
            0 if !out.is_empty() => {
                let i = rng.below(out.len());
                out[i] ^= 1 << rng.below(8);
            }
            1 => {
                let token = TOKENS[rng.below(TOKENS.len())];
                out.splice(at..at, token.iter().copied());
            }
            2 if !out.is_empty() => {
                let end = (at + rng.below(16) + 1).min(out.len());
                out.drain(at.min(end)..end);
            }
            3 if !out.is_empty() => {
                let start = rng.below(out.len());
                let end = (start + rng.below(64) + 1).min(out.len());
                let piece = out[start..end].to_vec();
                out.splice(at..at, piece);
            }
            4 if !other.is_empty() => {
                let start = rng.below(other.len());
                let end = (start + rng.below(128) + 1).min(other.len());
                out.splice(at..at, other[start..end].iter().copied());
            }
            5 => out.truncate(at),
            _ => {
                // Nesting: the same opening (or prefix operator) many times.
                const OPENINGS: [&[u8]; 8] =
                    [b"[", b"(", b"{", b"<<", b"!", b"-", b"NOT EXISTS {", b"^"];
                let token = OPENINGS[rng.below(OPENINGS.len())];
                for _ in 0..rng.below(300) {
                    out.splice(at..at, token.iter().copied());
                }
            }
        }
    }
    out
}

fn save(target: Target, input: &[u8]) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    input.hash(&mut hasher);
    let dir = root().join("tmp/fuzz-findings");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{}-{:016x}.bin", target.name(), hasher.finish()));
    let _ = std::fs::write(&path, input);
    path
}

/// The stack each target runs on: a release build's parsers stay within 1 MiB at their
/// nesting limits (less than a server thread has); a debug build's frames are several
/// times larger.
const STACK: usize = if cfg!(debug_assertions) {
    16 << 20
} else {
    1 << 20
};

/// Runs `target` on its corpus and `cases` mutations of each input; the number of runs
/// and the findings (saved inputs).
fn fuzz(target: Target, seed: u64, cases: usize, files: usize) -> (usize, Vec<String>) {
    let mut inputs: Vec<Vec<u8>> = seeds(target).into_iter().map(<[u8]>::to_vec).collect();
    inputs.extend(corpus(target, files));
    let mut rng = Rng((seed ^ 0x9e37_79b9_7f4a_7c15).wrapping_mul(target as u64 + 1) | 1);
    let (mut runs, mut findings) = (0, Vec::new());
    for (i, input) in inputs.iter().enumerate() {
        let other = &inputs[rng.below(inputs.len())];
        let candidates = std::iter::once(input.clone())
            .chain((0..cases).map(|_| mutate(&mut rng, input, other)));
        for candidate in candidates {
            runs += 1;
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| target.run(&candidate)));
            if outcome.is_err() {
                findings.push(format!(
                    "{} (corpus input {i}): {}",
                    target.name(),
                    save(target, &candidate).display()
                ));
            }
        }
    }
    (runs, findings)
}

#[test]
fn parsers_hold_on_their_corpora_and_mutations() {
    let seed = env("NRESE_FUZZ_SEED", 0);
    let cases = env("NRESE_FUZZ_CASES", 8) as usize;
    let files = env("NRESE_FUZZ_FILES", 300) as usize;
    let only = std::env::var("NRESE_FUZZ_TARGET").ok();
    let (mut runs, mut findings) = (0, Vec::new());
    for target in Target::ALL {
        if only.as_deref().is_some_and(|name| name != target.name()) {
            continue;
        }
        // A stack overflow aborts the process: NRESE_FUZZ_TARGET narrows it down.
        let (n, found) = std::thread::Builder::new()
            .stack_size(STACK)
            .spawn(move || fuzz(target, seed, cases, files))
            .expect("a thread")
            .join()
            .expect("the target's thread");
        runs += n;
        findings.extend(found);
    }
    assert!(
        findings.is_empty(),
        "{} findings in {runs} runs (seed {seed}):
{}",
        findings.len(),
        findings.join(
            "
"
        )
    );
}

mod replay;
