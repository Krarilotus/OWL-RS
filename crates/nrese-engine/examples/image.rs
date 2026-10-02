//! Writes an image of a store in the current checkpoint format: the dictionary and both
//! stacks of its latest revision, as a data directory that opens at that revision. For
//! moving a store to a new format, and for measuring one.
//!
//! ```text
//! cargo run --release -p nrese-engine --example image -- STORE_DIR OUT_DIR
//! ```
//!
//! `NRESE_VOCABULARY=fsst` writes the dictionary's keys compressed, `NRESE_INDEX_ENCODING`
//! takes `fast` or `compact` for the index blocks, as the server reads them.

use std::time::Instant;

use nrese_engine::{Engine, EngineConfig};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(store), Some(out)) = (args.next(), args.next()) else {
        eprintln!("usage: image STORE_DIR OUT_DIR");
        std::process::exit(2);
    };
    if let Some(encoding) = std::env::var("NRESE_VOCABULARY")
        .ok()
        .and_then(|name| nrese_engine::VocabularyEncoding::from_name(&name))
    {
        nrese_engine::set_vocabulary_encoding(encoding);
    }
    if let Some(encoding) = std::env::var("NRESE_INDEX_ENCODING")
        .ok()
        .and_then(|name| nrese_engine::IndexEncoding::from_name(&name))
    {
        nrese_engine::set_index_encoding(encoding);
    }
    let config = EngineConfig {
        background_maintenance: false,
        ..EngineConfig::default()
    };
    let engine = Engine::open(&store, config).expect("open the store");
    std::fs::create_dir_all(&out).expect("create the output directory");
    let started = Instant::now();
    let image = engine
        .write_image(std::path::Path::new(&out))
        .expect("write the image");
    let size = |path: &std::path::Path| std::fs::metadata(path).map_or(0, |m| m.len());
    let before: u64 = std::fs::read_dir(&store)
        .expect("read the store")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "nck"))
        .map(|path| size(&path))
        .max()
        .unwrap_or(0);
    println!(
        "revision {}: {} quads, {} inferred; checkpoint {:.1} MiB → {:.1} MiB in {:.1} s ({})",
        image.revision,
        image.quads,
        image.inferred,
        before as f64 / 1_048_576.0,
        size(&image.path) as f64 / 1_048_576.0,
        started.elapsed().as_secs_f64(),
        image.path.display(),
    );
}
