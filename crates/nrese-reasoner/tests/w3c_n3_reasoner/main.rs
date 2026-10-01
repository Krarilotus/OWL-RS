//! The W3C N3 Community Group's reasoner tests (w3c/N3, `manifest-reasoner.ttl`) on the
//! user rules of the reasoner: the N3 compiler ([`nrese_reasoner::v2::n3`]) and the
//! reference evaluator.
//!
//! - **In scope.** Tests whose options are cwm's `--think --data` (rules to a fixpoint, then
//!   the plain data), whose rules compile (datalog N3: no builtins but `log:equalTo` and
//!   `log:notEqualTo`, no existential conclusions), and whose expected output is plain RDF.
//!   Each must pass: the data and its conclusions equal the expected statements, up to blank
//!   node names.
//! - **Out of scope**, counted with the reason: other cwm options (a single pass of the
//!   rules, conclusions only), the builtins this reasoner doesn't have, formulas in the
//!   expected output.
//! - **Source.** The pinned checkout `scripts/fetch-w3c-tests.sh` puts in `.cache/n3`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use nrese_rdf::{BlankNode, Dataset, GraphName, Literal, NamedNode, NamedOrBlankNode, Quad, Term};
use nrese_rdf_io::n3::{N3Parser, N3Quad, N3Term};
use nrese_rdf_io::{RdfFormat, RdfParser};
use nrese_reasoner::v2::ir::Vocabulary;
use nrese_reasoner::v2::n3::compile_quads;
use nrese_reasoner::v2::naive::materialise;
use nrese_reasoner::v2::testing::LocalVocabulary;

const BASE: &str = "https://w3c.github.io/N3/tests/N3Tests/";
const MF: &str = "http://www.w3.org/2001/sw/DataAccess/tests/test-manifest#";
const TEST: &str = "https://w3c.github.io/N3/tests/test.n3#";
const LOG_IMPLIES: &str = "http://www.w3.org/2000/10/swap/log#implies";
const LOG_IMPLIED_BY: &str = "http://www.w3.org/2000/10/swap/log#isImpliedBy";

fn suite_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/n3/tests/N3Tests");
    root.join("manifest-reasoner.ttl").is_file().then_some(root)
}

struct Case {
    iri: String,
    action: String,
    result: String,
    options: Vec<String>,
}

fn cases(root: &Path) -> Vec<Case> {
    let text = std::fs::read(root.join("manifest-reasoner.ttl")).unwrap();
    let quads: Vec<Quad> = RdfParser::from_format(RdfFormat::Turtle)
        .with_base_iri(format!("{BASE}manifest-reasoner.ttl"))
        .unwrap()
        .for_slice(&text)
        .collect::<Result<_, _>>()
        .unwrap();
    let mut by_subject: HashMap<String, Vec<(String, Term)>> = HashMap::new();
    for q in &quads {
        by_subject
            .entry(q.subject.to_string())
            .or_default()
            .push((q.predicate.as_str().to_owned(), q.object.clone()));
    }
    let get = |subject: &str, predicate: &str| -> Option<Term> {
        by_subject
            .get(subject)?
            .iter()
            .find(|(p, _)| p == predicate)
            .map(|(_, o)| o.clone())
    };
    let iri = |t: &Term| match t {
        Term::NamedNode(n) => n.as_str().to_owned(),
        other => other.to_string(),
    };
    let mut out = Vec::new();
    for (subject, properties) in &by_subject {
        if !properties
            .iter()
            .any(|(p, o)| p.ends_with("#type") && iri(o) == format!("{TEST}TestN3Reason"))
        {
            continue;
        }
        let (Some(action), Some(result)) = (
            get(subject, &format!("{MF}action")),
            get(subject, &format!("{MF}result")),
        ) else {
            continue;
        };
        let options = get(subject, &format!("{TEST}options"))
            .map(|node| {
                by_subject
                    .get(&node.to_string())
                    .map(|ps| {
                        ps.iter()
                            .filter(|(_, o)| o.to_string().contains("true"))
                            .filter_map(|(p, _)| p.strip_prefix(TEST).map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        out.push(Case {
            iri: subject.clone(),
            action: iri(&action),
            result: iri(&result),
            options,
        });
    }
    out.sort_by(|a, b| a.iri.cmp(&b.iri));
    out
}

/// A vocabulary that also remembers the term of every id, to read the closure back.
#[derive(Default)]
struct Terms {
    ids: LocalVocabulary,
    terms: HashMap<u64, Term>,
}

impl Terms {
    fn keep(&mut self, id: u64, term: Term) -> u64 {
        self.terms.entry(id).or_insert(term);
        id
    }

    fn term(&mut self, term: &N3Term) -> Option<u64> {
        Some(match term {
            N3Term::NamedNode(n) => self.iri(n.as_str()),
            N3Term::BlankNode(b) => {
                let id = self.ids.term(&format!("_:{}", b.as_str()));
                self.keep(id, b.clone().into())
            }
            N3Term::Literal(l) => match l.language() {
                Some(language) => self.language_literal(l.value(), language),
                None => self.literal(l.value(), l.datatype().as_str()),
            },
            _ => return None,
        })
    }
}

impl Vocabulary for Terms {
    fn iri(&mut self, iri: &str) -> u64 {
        let id = self.ids.iri(iri);
        self.keep(id, NamedNode::new_unchecked(iri).into())
    }

    fn literal(&mut self, lexical: &str, datatype: &str) -> u64 {
        let id = self.ids.literal(lexical, datatype);
        self.keep(
            id,
            Literal::new_typed_literal(lexical, NamedNode::new_unchecked(datatype)).into(),
        )
    }

    fn language_literal(&mut self, lexical: &str, language: &str) -> u64 {
        let id = self.ids.language_literal(lexical, language);
        self.keep(
            id,
            Literal::new_language_tagged_literal_unchecked(lexical, language).into(),
        )
    }
}

enum Outcome {
    Pass,
    Fail(String),
    OutOfScope(String),
}

fn read_n3(root: &Path, iri: &str) -> Result<Vec<N3Quad>, String> {
    let path = root.join(iri.strip_prefix(BASE).ok_or("a file outside the suite")?);
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    N3Parser::new()
        .with_base_iri(iri)
        .map_err(|e| e.to_string())?
        .for_slice(&bytes)
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())
}

fn canonical(quads: BTreeSet<Quad>) -> Dataset {
    let mut dataset: Dataset = quads.into_iter().collect();
    dataset.canonicalize();
    dataset
}

fn run(root: &Path, case: &Case) -> Outcome {
    if !(case.options.iter().any(|o| o == "think") && case.options.iter().any(|o| o == "data")) {
        return Outcome::OutOfScope(format!("cwm options {:?}", case.options));
    }
    let action = match read_n3(root, &case.action) {
        Ok(quads) => quads,
        Err(e) => return Outcome::OutOfScope(format!("unreadable input: {e}")),
    };
    let is_rule = |q: &N3Quad| matches!(&q.predicate, N3Term::NamedNode(p) if p.as_str() == LOG_IMPLIES || p.as_str() == LOG_IMPLIED_BY);
    let formula_nodes: BTreeSet<BlankNode> = action
        .iter()
        .filter_map(|q| match &q.graph_name {
            GraphName::BlankNode(b) => Some(b.clone()),
            _ => None,
        })
        .collect();
    let mentions_formula = |q: &N3Quad| {
        [&q.subject, &q.object]
            .iter()
            .any(|t| matches!(t, N3Term::BlankNode(b) if formula_nodes.contains(b)))
    };
    // Rules and the formulas they use go to the compiler; plain statements are the data.
    let (rules, data): (Vec<N3Quad>, Vec<N3Quad>) = action
        .into_iter()
        .partition(|q| !q.graph_name.is_default_graph() || is_rule(q));
    if data.iter().any(mentions_formula) {
        return Outcome::OutOfScope("formulas used as data".to_owned());
    }
    let mut terms = Terms::default();
    let program = match compile_quads("test", &rules, &mut terms) {
        Ok(program) => program,
        Err(e) => return Outcome::OutOfScope(e.message),
    };
    let mut facts = Vec::new();
    for q in &data {
        let (Some(s), Some(p), Some(o)) = (
            terms.term(&q.subject),
            terms.term(&q.predicate),
            terms.term(&q.object),
        ) else {
            return Outcome::OutOfScope("variables or triple terms in the data".to_owned());
        };
        facts.push([s, p, o]);
    }
    let closure = materialise(&facts, &program.rules, None);
    if !closure.violations.is_empty() {
        return Outcome::OutOfScope("an inconsistency (=> false) fired".to_owned());
    }
    let mut actual = BTreeSet::new();
    for [s, p, o] in facts.iter().copied().chain(closure.derived.iter().copied()) {
        let (s, p, o) = (&terms.terms[&s], &terms.terms[&p], &terms.terms[&o]);
        let (Ok(subject), Term::NamedNode(predicate)) = (NamedOrBlankNode::try_from(s.clone()), p)
        else {
            return Outcome::OutOfScope("generalised triples in the closure".to_owned());
        };
        actual.insert(Quad::new(
            subject,
            predicate.clone(),
            o.clone(),
            GraphName::DefaultGraph,
        ));
    }
    let expected = match read_n3(root, &case.result) {
        Ok(quads) => quads,
        Err(e) => return Outcome::OutOfScope(format!("unreadable expected output: {e}")),
    };
    let mut wanted = BTreeSet::new();
    for q in expected {
        match q.into_quad() {
            Ok(quad) if quad.graph_name.is_default_graph() && !is_rule_quad(&quad) => {
                wanted.insert(quad);
            }
            Ok(_) => {}
            Err(_) => {
                return Outcome::OutOfScope(
                    "formulas or variables in the expected output".to_owned(),
                );
            }
        }
    }
    let (a, b) = (canonical(actual), canonical(wanted));
    if a == b {
        Outcome::Pass
    } else {
        Outcome::Fail(format!("closure:\n{a}\nexpected:\n{b}"))
    }
}

fn is_rule_quad(quad: &Quad) -> bool {
    quad.predicate.as_str() == LOG_IMPLIES || quad.predicate.as_str() == LOG_IMPLIED_BY
}

#[test]
fn w3c_n3_reasoner_suite() {
    let Some(root) = suite_root() else {
        assert!(
            std::env::var_os("NRESE_W3C_REQUIRED").is_none_or(|v| v.is_empty()),
            "N3 tests not found; run scripts/fetch-w3c-tests.sh"
        );
        eprintln!("skipped: N3 tests not found (run scripts/fetch-w3c-tests.sh)");
        return;
    };
    let cases = cases(&root);
    assert!(cases.len() > 80, "only {} reasoner tests", cases.len());
    let (mut passed, mut failures) = (0, Vec::new());
    let mut out_of_scope: BTreeMap<String, usize> = BTreeMap::new();
    for case in &cases {
        match run(&root, case) {
            Outcome::Pass => passed += 1,
            Outcome::Fail(why) => failures.push(format!("{}: {why}", case.iri)),
            Outcome::OutOfScope(why) => {
                let reason = why.split(" <").next().unwrap_or(&why).to_owned();
                *out_of_scope.entry(reason).or_default() += 1;
            }
        }
    }
    println!(
        "N3 reasoner tests: {} in scope, {passed} passed; {} out of scope:",
        passed + failures.len(),
        cases.len() - passed - failures.len()
    );
    for (reason, count) in &out_of_scope {
        println!("  {count:>3}  {reason}");
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
