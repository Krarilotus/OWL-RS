//! Embeds the built user console (`apps/nrese-console/dist`) into the server binary, so a
//! packaged server needs no files beside it.
//!
//! Generates `$OUT_DIR/console_assets.rs`: a table of `(path, bytes)` for every file of
//! the console build. Without a console build the table is empty and `/console` says so;
//! the API works either way. Build the console first (`npm run build` in
//! `apps/nrese-console`) to include it.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let dist = manifest.join("../../apps/nrese-console/dist");
    // Cargo reruns this when the directory or a file in it changes (vite names assets by
    // content hash, so every console build changes the directory). Without a build, it
    // watches the console's directory for one to appear; a path that doesn't exist would
    // rerun the script on every build.
    let watched = if dist.is_dir() {
        dist.clone()
    } else {
        manifest.join("../../apps/nrese-console")
    };
    println!("cargo:rerun-if-changed={}", watched.display());

    let mut files = Vec::new();
    collect(&dist, &mut files);
    files.sort();

    let mut table = String::from("pub static CONSOLE_ASSETS: &[(&str, &[u8])] = &[\n");
    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        let relative = file
            .strip_prefix(&dist)
            .expect("collected below dist")
            .to_string_lossy()
            .replace('\\', "/");
        let absolute = file
            .canonicalize()
            .expect("a file that was just listed")
            .to_string_lossy()
            .into_owned();
        writeln!(table, "    ({relative:?}, include_bytes!({absolute:?})),").expect("a string");
    }
    table.push_str("];\n");

    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("set by cargo"));
    std::fs::write(out.join("console_assets.rs"), table).expect("OUT_DIR is writable");
}

/// Every file under `dir`, recursively; nothing if `dir` doesn't exist.
fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}
