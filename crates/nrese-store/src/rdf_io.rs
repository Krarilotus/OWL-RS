use std::path::Path;

use oxigraph::io::RdfParser;
use url::Url;

use crate::error::{StoreError, StoreResult};
use crate::query::GraphResultFormat;

pub fn parser_for_graph_format(
    format: GraphResultFormat,
    base_iri: Option<&str>,
) -> StoreResult<RdfParser> {
    let parser = RdfParser::from_format(format.into_oxigraph());
    with_base_iri(parser, base_iri)
}

pub fn file_base_iri(path: &Path) -> StoreResult<String> {
    let canonical = path.canonicalize()?;
    Url::from_file_path(&canonical)
        .map(|url| url.into())
        .map_err(|()| {
            StoreError::Configuration(format!(
                "cannot derive file base IRI from ontology path {}",
                canonical.display()
            ))
        })
}

fn with_base_iri(parser: RdfParser, base_iri: Option<&str>) -> StoreResult<RdfParser> {
    let Some(base_iri) = base_iri else {
        return Ok(parser);
    };

    parser.with_base_iri(base_iri).map_err(|error| {
        StoreError::Configuration(format!("invalid RDF parser base IRI '{base_iri}': {error}"))
    })
}
