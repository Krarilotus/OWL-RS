//! The split tests' comparison of two parses of one document: their items, error fields
//! and all. The integration tests (`split_contract_tests`) and the crate's own test of each
//! cut alone (`parser::split_cuts`) read the same items.

use std::collections::HashMap;
use std::fmt::Debug;

use nrese_rdf_io::{RdfParseError, RdfSyntaxError};

/// The items read, at most this many (a parser that doesn't end shows as that).
pub const BOUND: usize = 10_000;

/// A parse's items, to compare with another parse's: each quad as written (`Debug`), each
/// syntax error by every field: its message, its location (start and end), and the
/// statement it skipped (span, the directives in effect, the graph). Blank nodes are named
/// by their first occurrence (`b0`, `b1`, ...) in RDF terms only, the quads and the
/// statement's graph: a parser names the generated ones (`[]`, lists) from a key of its
/// own, so only their pattern is the same in two parses.
pub fn items<T: Debug>(parse: impl IntoIterator<Item = Result<T, RdfParseError>>) -> Vec<String> {
    let mut names = HashMap::new();
    parse
        .into_iter()
        .take(BOUND)
        .map(|item| match item {
            Ok(quad) => quad_text(&format!("{quad:?}"), &mut names),
            Err(RdfParseError::Syntax(error)) => error_text(&error, &mut names),
            Err(error) => format!("error: {error}"),
        })
        .collect()
}

/// A syntax error's fields.
fn error_text(error: &RdfSyntaxError, names: &mut HashMap<String, usize>) -> String {
    let statement = match error.statement() {
        None => "no statement".to_owned(),
        Some(statement) => {
            let graph = statement
                .graph
                .as_deref()
                .map(|graph| match graph.strip_prefix("_:") {
                    Some(label) => format!("_:b{}", name(label, names)),
                    None => graph.to_owned(),
                });
            format!(
                "span {:?}, context {:?}, graph {graph:?}",
                statement.span, statement.context
            )
        }
    };
    format!(
        "error {:?} at {:?}; {statement}",
        error.message(),
        error.location()
    )
}

/// A quad's text with its blank nodes named by first occurrence.
fn quad_text(text: &str, names: &mut HashMap<String, usize>) -> String {
    const ID: &str = "BlankNode { id: \"";
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(ID) {
        let (before, after) = rest.split_at(at + ID.len());
        out.push_str(before);
        let end = after.find('"').unwrap_or(after.len());
        out.push_str(&format!("b{}", name(&after[..end], names)));
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

fn name(label: &str, names: &mut HashMap<String, usize>) -> usize {
    let next = names.len();
    *names.entry(label.to_owned()).or_insert(next)
}
