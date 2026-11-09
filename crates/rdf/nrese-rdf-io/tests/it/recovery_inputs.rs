//! Documents for the recovery tests: statements using every grammar routine that opens a
//! bracket, each with a delimiter put at or before every token position, then a valid
//! statement. The parser's own sweep (`reader_contract_tests`) and the splitter's oracle
//! (`split_contract_tests`) read the same documents.

use nrese_rdf_io::RdfFormat;

/// Each of the ten delimiters, the two braces and `.`.
pub const DELIMITERS: [&str; 13] = [
    "[", "]", "(", ")", "<<(", ")>>", "<<", ">>", "{|", "|}", "{", "}", ".",
];

/// Statements using every grammar routine that opens one (objects, lists, triple terms,
/// reified triples, annotations, reifiers, `[]`, datatypes, directives, TriG graph names).
pub const SEEDS: [(RdfFormat, &str); 18] = [
    (
        RdfFormat::Turtle,
        "<urn:s> <urn:p> [ <urn:q> ( 1 <<( <urn:a> <urn:b> <urn:c> )>> ) ; <urn:r> << <urn:a> <urn:b> <urn:c> ~ <urn:x> >> {| <urn:p> <urn:o> |} ] .",
    ),
    (RdfFormat::Turtle, "[ <urn:p> <urn:o> ] <urn:q> <urn:r> ."),
    (RdfFormat::Turtle, "[ ] <urn:p> <urn:o> ."),
    (RdfFormat::Turtle, "( 1 2 ) <urn:p> <urn:o> ."),
    (
        RdfFormat::Turtle,
        "<< << <urn:a> <urn:b> <urn:c> >> <urn:p> << <urn:a> <urn:b> <urn:c> >> >> <urn:q> <urn:r> .",
    ),
    (
        RdfFormat::Turtle,
        "<urn:s> <urn:p> << [ ] <urn:b> <<( <urn:a> <urn:b> \"x\" )>> ~ [ ] >> .",
    ),
    (
        RdfFormat::Turtle,
        "<urn:s> <urn:p> <<( [ ] <urn:b> <<( <urn:a> <urn:b> <urn:c> )>> )>> .",
    ),
    (
        RdfFormat::Turtle,
        "<urn:s> <urn:p> <urn:o> ~ [ ] {| <urn:q> [ <urn:r> <urn:o> ] |} .",
    ),
    (RdfFormat::Turtle, "<urn:s> <urn:p> \"x\"^^ <urn:dt> ."),
    (RdfFormat::Turtle, "@prefix ex: <urn:ex#> ."),
    (RdfFormat::Turtle, "@base <urn:b> ."),
    (RdfFormat::Turtle, "@version \"1.2\" ."),
    (RdfFormat::Turtle, "PREFIX ex: <urn:ex#>"),
    (RdfFormat::TriG, "GRAPH [ ] { <urn:s> <urn:p> <urn:o> . }"),
    (
        RdfFormat::TriG,
        "[ ] { <urn:s> <urn:p> [ <urn:q> <urn:o> ] . }",
    ),
    (
        RdfFormat::TriG,
        "<urn:g> { [ <urn:p> <urn:o> ] <urn:q> ( 1 ) . }",
    ),
    (RdfFormat::TriG, "[ ] <urn:p> <urn:o> ."),
    (RdfFormat::TriG, "[ <urn:p> <urn:o> ] <urn:q> <urn:r> ."),
];

/// Every seed with each delimiter replacing the token at each position or put before it,
/// then ` <urn:after> <urn:p> <urn:o> .`.
pub fn sweep() -> Vec<(RdfFormat, String)> {
    let mut documents = Vec::new();
    for (format, seed) in SEEDS {
        let tokens: Vec<&str> = seed.split(' ').collect();
        for at in 0..=tokens.len() {
            for delimiter in DELIMITERS {
                for replace in [false, true] {
                    if replace && at == tokens.len() {
                        continue;
                    }
                    let mut mutated = tokens.clone();
                    if replace {
                        mutated[at] = delimiter;
                    } else {
                        mutated.insert(at, delimiter);
                    }
                    documents.push((
                        format,
                        format!("{} <urn:after> <urn:p> <urn:o> .", mutated.join(" ")),
                    ));
                }
            }
        }
    }
    documents
}
