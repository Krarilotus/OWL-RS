//! The process's memory limit stops reasoning on every path, whatever `stop` the caller
//! passes: a DL bound's evaluation, called with a deadline only, reached 49 GB on
//! 6 October 2026. A test binary of its own: the limit is process-wide.

use nrese_reasoner::batch::{self, Schema};
use nrese_reasoner::delta::{self, Interrupted, MemoryBase};
use nrese_reasoner::eval::{NEVER, over_memory_limit};
use nrese_reasoner::ir::{RDF, RDFS, Vocabulary};
use nrese_reasoner::rulesets::Ruleset;
use nrese_reasoner::vocabulary::LocalVocabulary;

#[test]
fn reasoning_stops_at_the_process_memory_limit() {
    if nrese_exec::memory::process_bytes().is_none() {
        return;
    }
    let mut vocabulary = LocalVocabulary::default();
    let rules = Ruleset::Owl2Rl
        .rules(&mut vocabulary)
        .expect("OWL 2 RL parses");
    let schema = Schema::owl(&mut vocabulary);
    let (ty, sub) = (
        vocabulary.iri(&format!("{RDF}type")),
        vocabulary.iri(&format!("{RDFS}subClassOf")),
    );
    let mut iri = |name: &str| vocabulary.iri(&format!("http://example.com/{name}"));
    let (a, b, x) = (iri("A"), iri("B"), iri("x"));
    let input = vec![[a, sub, b], [x, ty, a]];
    // Under a limit the process is past, with a stop that never fires: stopped.
    nrese_exec::memory::set_process_limit(1);
    assert!(over_memory_limit());
    let stopped = batch::materialise_owned_until(input.clone(), &rules, None, &schema, NEVER);
    assert!(matches!(stopped, Err(Interrupted)), "materialisation");
    let base = MemoryBase::new(&input, &[]);
    let compiled = delta::Rules {
        rules: &rules,
        lists: None,
        schema: &schema,
    };
    let update = delta::update_until(&base, &[[x, ty, b]], &[], compiled, None, NEVER);
    assert!(matches!(update, Err(Interrupted)), "commit");
    // Without a limit, the same runs.
    nrese_exec::memory::set_process_limit(0);
    let closure =
        batch::materialise_owned_until(input, &rules, None, &schema, NEVER).expect("no limit");
    assert!(closure.derived.contains(&[x, ty, b]));
}
