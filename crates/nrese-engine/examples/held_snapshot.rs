//! A held handle, unlike HTTP transactions' per-read snapshots, retains an old revision.
//! Run alongside scripts/demo/capabilities/run.py; no durable named history is demonstrated.
use nrese_engine::{Engine, EngineConfig, QuadPattern, ReadModel, Snapshot};
use nrese_rdf::{Literal, Quad};
use nrese_rdf_io::{RdfFormat, RdfParser};

fn sorted(rows: impl Iterator<Item = String>) -> Vec<String> {
    let mut rows: Vec<_> = rows.collect();
    rows.sort();
    rows
}

fn contents(snapshot: &Snapshot) -> Vec<String> {
    sorted(
        snapshot
            .quads_for_pattern_in(ReadModel::Asserted, &QuadPattern::all())
            .map(|quad| snapshot.decode_quad(quad).expect("decode").to_string()),
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new(EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    })?;
    let fixture: Vec<_> = RdfParser::from_format(RdfFormat::NTriples)
        .for_slice(include_bytes!(
            "../../../scripts/demo/capabilities/contract.nt"
        ))
        .collect::<Result<_, _>>()?;
    let mut tx = engine.transaction();
    for quad in &fixture {
        tx.insert(quad.as_ref());
    }
    tx.commit()?;
    let held = engine.snapshot();
    let before = contents(&held);
    // The oracle is the fixture as parsed, never the store's own first answer.
    assert_eq!(before, sorted(fixture.iter().map(Quad::to_string)));
    let old = fixture
        .iter()
        .find(|quad| quad.subject.to_string() == "<urn:demo:brake>")
        .expect("component version");
    let new = Quad::new(
        old.subject.clone(),
        old.predicate.clone(),
        Literal::new_simple_literal("revised"),
        old.graph_name.clone(),
    );
    let mut tx = engine.transaction();
    tx.remove(old.as_ref());
    tx.insert(new.as_ref());
    tx.commit()?;
    let live = engine.snapshot();
    let expected = sorted(
        fixture
            .iter()
            .filter(|quad| *quad != old)
            .chain([&new])
            .map(Quad::to_string),
    );
    assert_eq!(held.revision() + 1, live.revision());
    assert_eq!(contents(&held), before);
    assert_eq!(contents(&live), expected);
    println!("held snapshot: revision {} {before:?}", held.revision());
    println!("live snapshot: revision {} {expected:?}", live.revision());
    println!("LIMIT: held handle lifetime only; not durable named history");
    Ok(())
}
