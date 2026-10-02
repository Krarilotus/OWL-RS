//! The packaging files stay in step with what they package.

use std::path::Path;

fn read(relative: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The image is built with the compiler `rust-toolchain.toml` pins.
#[test]
fn the_image_builds_with_the_pinned_toolchain() {
    let toolchain = read("rust-toolchain.toml");
    let pinned = toolchain
        .lines()
        .find_map(|line| line.strip_prefix("channel = "))
        .expect("a channel")
        .trim_matches('"');
    let dockerfile = read("Dockerfile");
    let image = dockerfile
        .lines()
        .find_map(|line| line.strip_prefix("ARG RUST_VERSION="))
        .expect("ARG RUST_VERSION");
    assert_eq!(
        image, pinned,
        "bump the Dockerfile's RUST_VERSION with rust-toolchain.toml"
    );
}

/// The compose file for ResearchSpace points it at an endpoint the server has, and sets
/// the default graph ResearchSpace needs.
#[test]
fn the_researchspace_setup_uses_the_combined_endpoint() {
    let compose = read("ops/researchspace/docker-compose.yml");
    assert!(compose.contains("sparqlEndpoint=http://nrese:8080/dataset/sparql"));
    assert!(compose.contains("NRESE_DEFAULT_GRAPH: union"));
}
