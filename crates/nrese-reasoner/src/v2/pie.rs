//! GraphDB rulesets (`.pie`), compiled into the rule IR ([`super::ir`]) (parity slice B2).
//!
//! A GraphDB repository's reasoning is a `.pie` file: prefixes, axioms and rules. Reading
//! it lets a GraphDB user bring their ruleset, custom rules included, as they are.
//!
//! ```text
//! Prefices { rdf : http://www.w3.org/1999/02/22-rdf-syntax-ns# }
//! Axioms   { <rdf:type> <rdf:type> <rdf:Property> }
//! Rules
//! {
//! Id: rdfs2
//!   x  a  y                [Constraint a != <rdf:type>]
//!   a  <rdfs:domain>  z
//!   -------------------------------
//!   x  <rdf:type>  z
//!
//! Consistency: disjoint
//!   x <rdf:type> c ; d <owl:disjointWith> c ; x <rdf:type> d
//!   -------------------------------
//! }
//! ```
//!
//! What compiles:
//! - Rules (`Id:`): premises, a line of dashes, conclusions. Bare words are variables;
//!   `<prefix:local>` and `<iri>` are IRIs; `"…"`, `"…"^^<…>` and `"…"@lang` literals.
//! - `[Constraint a != b, …]` on a premise: guards (the two must differ).
//! - Consistency checks (`Consistency:`): premises and no conclusion; the commit is
//!   rejected, like an OWL 2 RL violation.
//! - Axioms: facts that hold whatever the data.
//! - `[Constraint x != blank_node]`: `x` mustn't be a blank node; constraints may stand on
//!   conclusions too (they apply to the rule).
//! - `[Context <g>]`: GraphDB's internal contexts for auxiliary statements (`onto:_allDiff`,
//!   `onto:scm_int`). A statement in a context becomes one with a predicate of its own,
//!   `urn:nrese:pie-context:<g>#<p>`, so rules that read the context see exactly what rules
//!   that write it derived. Unlike GraphDB, which hides the contexts, those statements are
//!   in the inferred stack.
//! - `[Cut]`: an evaluation hint of GraphDB's; ignored (it changes no conclusion).
//! - Comments: `//` to the end of a line, and `/* … */`.
//!
//! What doesn't, with an error that names it: a context on a statement with a variable
//! predicate (GraphDB's OWL 2 RL `prp_spo2`), a conclusion variable that no premise binds
//! (an existential: the OWL 2 QL `exst` rules make blank nodes), and blank nodes. NRESE's
//! own `owl2-rl` and `owl2-ql` rulesets cover those profiles natively.

use std::collections::HashMap;

use super::ir::{Atom, Guard, Head, ParseError, Rule, Term, Vocabulary};
use super::n3::N3Program;

/// Compiles `text`, a `.pie` document (`name` prefixes errors).
pub fn compile(
    name: &str,
    text: &str,
    vocabulary: &mut impl Vocabulary,
) -> Result<N3Program, ParseError> {
    // Errors name the line they are on (comments are stripped line for line, so the
    // numbers are the document's); 0 for the document as a whole.
    let error = |rule: &str, line: usize, message: String| {
        let error = ParseError::new(
            if rule.is_empty() {
                name.to_owned()
            } else {
                format!("{name}#{rule}")
            },
            message,
        );
        match line {
            0 => error,
            line => error.at(line, 1),
        }
    };
    let text = strip_comments(text);
    let mut prefixes: HashMap<String, String> = HashMap::new();
    let mut axioms_text = (String::new(), 0);
    let mut rules_text = (String::new(), 0);
    for (section, body, first) in sections(&text).map_err(|m| error("", 0, m))? {
        match section.as_str() {
            "Prefices" | "Prefixes" => {
                for (line, number) in lines(&body, first) {
                    let (prefix, iri) = line.split_once(':').ok_or_else(|| {
                        error("", number, format!("a prefix line without ':': {line}"))
                    })?;
                    prefixes.insert(prefix.trim().to_owned(), iri.trim().to_owned());
                }
            }
            "Axioms" => axioms_text = (body, first),
            "Rules" => rules_text = (body, first),
            other => return Err(error("", first, format!("unknown section {other}"))),
        }
    }
    let mut facts = Vec::new();
    for (line, number) in lines(&axioms_text.0, axioms_text.1) {
        let (terms, _) = tokens(line).map_err(|m| error("axioms", number, m))?;
        let [s, p, o] = terms.as_slice() else {
            return Err(error(
                "axioms",
                number,
                format!("an axiom isn't three terms: {line}"),
            ));
        };
        let constant = |token: &Token, vocabulary: &mut _| match token {
            Token::Var(v) => Err(error(
                "axioms",
                number,
                format!("a variable in an axiom: {v}"),
            )),
            other => constant(other, &prefixes, vocabulary).map_err(|m| error("axioms", number, m)),
        };
        facts.push([
            constant(s, vocabulary)?,
            constant(p, vocabulary)?,
            constant(o, vocabulary)?,
        ]);
    }
    let mut rules = Vec::new();
    for (kind, rule_name, body, number) in
        rule_blocks(&rules_text.0, rules_text.1).map_err(|m| error("", 0, m))?
    {
        let rule = compile_rule(&kind, &rule_name, &body, &prefixes, vocabulary)
            .map_err(|m| error(&rule_name, number, m))?;
        rules.push(rule);
    }
    Ok(N3Program { rules, facts })
}

/// The non-empty lines of `text`, trimmed, with their line numbers in the document
/// (`text`'s first line is line `first`).
fn lines(text: &str, first: usize) -> impl Iterator<Item = (&str, usize)> {
    text.lines()
        .enumerate()
        .map(move |(i, line)| (line.trim(), first + i))
        .filter(|(line, _)| !line.is_empty())
}

/// `text` without `//` and `/* */` comments (outside `<…>` and quotes; `//` starts a
/// comment at the start of a line or after a space, so `http://` in a prefix isn't one).
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let (mut in_iri, mut in_string) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '"' if !in_iri => {
                in_string = !in_string;
                out.push(c);
            }
            '\\' if in_string => {
                out.push(c);
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            '<' if !in_string => {
                in_iri = true;
                out.push(c);
            }
            '>' if in_iri => {
                in_iri = false;
                out.push(c);
            }
            '/' if !in_iri
                && !in_string
                && chars.peek() == Some(&'/')
                && out.chars().next_back().is_none_or(char::is_whitespace) =>
            {
                for next in chars.by_ref() {
                    if next == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if !in_iri && !in_string && chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = ' ';
                for next in chars.by_ref() {
                    if previous == '*' && next == '/' {
                        break;
                    }
                    if next == '\n' {
                        out.push('\n');
                    }
                    previous = next;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// The top-level sections: a name, then a body in braces, and the line number of the
/// body's first line.
fn sections(text: &str) -> Result<Vec<(String, String, usize)>, String> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let trimmed = rest.trim_start();
        if trimmed.is_empty() {
            return Ok(out);
        }
        let line_of = |at: &str| text[..text.len() - at.len()].matches('\n').count() + 1;
        let open = trimmed
            .find('{')
            .ok_or_else(|| format!("a section without '{{': {}", head(trimmed)))?;
        let name = trimmed[..open].trim().to_owned();
        // The body ends at the matching '}' (braces don't nest in .pie, but '}' may occur
        // inside an IRI or a string).
        let body_start = open + 1;
        let mut end = None;
        let (mut in_iri, mut in_string) = (false, false);
        for (i, c) in trimmed[body_start..].char_indices() {
            match c {
                '"' if !in_iri => in_string = !in_string,
                '<' if !in_string => in_iri = true,
                '>' if in_iri => in_iri = false,
                '}' if !in_iri && !in_string => {
                    end = Some(body_start + i);
                    break;
                }
                _ => {}
            }
        }
        let end = end.ok_or_else(|| format!("section {name} isn't closed"))?;
        let first = line_of(&trimmed[body_start..]);
        out.push((name, trimmed[body_start..end].to_owned(), first));
        rest = &trimmed[end + 1..];
    }
}

fn head(text: &str) -> &str {
    let end = text.char_indices().nth(40).map_or(text.len(), |(i, _)| i);
    &text[..end]
}

/// The rules of the `Rules` section: kind (`Id` or `Consistency`), name, body lines, and
/// the line number of its `Id:` (`text`'s first line is line `first`).
#[allow(clippy::type_complexity)]
fn rule_blocks(
    text: &str,
    first: usize,
) -> Result<Vec<(String, String, Vec<String>, usize)>, String> {
    let mut out: Vec<(String, String, Vec<String>, usize)> = Vec::new();
    for (line, number) in lines(text, first) {
        let start = ["Id:", "Consistency:"]
            .into_iter()
            .find(|keyword| line.starts_with(keyword));
        match start {
            Some(keyword) => out.push((
                keyword.trim_end_matches(':').to_owned(),
                line[keyword.len()..].trim().to_owned(),
                Vec::new(),
                number,
            )),
            None => match out.last_mut() {
                Some((_, _, lines, _)) => lines.push(line.to_owned()),
                None => {
                    return Err(format!(
                        "line {number}: a rule line before any 'Id:': {line}"
                    ));
                }
            },
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Var(String),
    Iri(String),
    Literal {
        lexical: String,
        datatype: Option<String>,
        language: Option<String>,
    },
}

/// The terms of a line, and the contents of its `[…]` options.
fn tokens(line: &str) -> Result<(Vec<Token>, Vec<String>), String> {
    let mut terms = Vec::new();
    let mut options = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let take_until = |i: &mut usize, end: char| -> Result<String, String> {
        let start = *i;
        while *i < chars.len() && chars[*i] != end {
            *i += 1;
        }
        if *i == chars.len() {
            return Err(format!("unterminated term in: {line}"));
        }
        let text: String = chars[start..*i].iter().collect();
        *i += 1;
        Ok(text)
    };
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() || c == ';' || c == ',' {
            i += 1;
        } else if c == '<' {
            i += 1;
            terms.push(Token::Iri(take_until(&mut i, '>')?));
        } else if c == '[' {
            i += 1;
            options.push(take_until(&mut i, ']')?);
        } else if c == '"' {
            i += 1;
            let mut lexical = String::new();
            loop {
                let Some(&c) = chars.get(i) else {
                    return Err(format!("unterminated string in: {line}"));
                };
                i += 1;
                match c {
                    '"' => break,
                    '\\' => {
                        let escaped = chars.get(i).copied().unwrap_or('\\');
                        i += 1;
                        lexical.push(match escaped {
                            'n' => '\n',
                            't' => '\t',
                            'r' => '\r',
                            other => other,
                        });
                    }
                    other => lexical.push(other),
                }
            }
            let (mut datatype, mut language) = (None, None);
            if chars.get(i) == Some(&'^') && chars.get(i + 1) == Some(&'^') {
                i += 2;
                if chars.get(i) == Some(&'<') {
                    i += 1;
                    datatype = Some(take_until(&mut i, '>')?);
                } else {
                    // A prefixed name without brackets: `"1"^^xsd:integer`.
                    let start = i;
                    while i < chars.len()
                        && !chars[i].is_whitespace()
                        && !matches!(chars[i], '[' | ';' | ',')
                    {
                        i += 1;
                    }
                    datatype = Some(chars[start..i].iter().collect());
                }
            } else if chars.get(i) == Some(&'@') {
                i += 1;
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '-') {
                    i += 1;
                }
                language = Some(chars[start..i].iter().collect());
            }
            terms.push(Token::Literal {
                lexical,
                datatype,
                language,
            });
        } else if c == '_' && chars.get(i + 1) == Some(&':') {
            return Err(format!("blank nodes aren't supported in rules: {line}"));
        } else {
            let start = i;
            while i < chars.len()
                && !chars[i].is_whitespace()
                && !matches!(chars[i], '[' | ';' | ',')
            {
                i += 1;
            }
            terms.push(Token::Var(chars[start..i].iter().collect()));
        }
    }
    Ok((terms, options))
}

/// An IRI token's full IRI: `prefix:local` with a declared prefix, else as written.
fn expand(iri: &str, prefixes: &HashMap<String, String>) -> String {
    if let Some((prefix, local)) = iri.split_once(':')
        && let Some(namespace) = prefixes.get(prefix)
    {
        return format!("{namespace}{local}");
    }
    iri.to_owned()
}

fn constant(
    token: &Token,
    prefixes: &HashMap<String, String>,
    vocabulary: &mut impl Vocabulary,
) -> Result<u64, String> {
    Ok(match token {
        Token::Var(v) => return Err(format!("{v} isn't a constant")),
        Token::Iri(iri) => vocabulary.iri(&expand(iri, prefixes)),
        Token::Literal {
            lexical,
            language: Some(language),
            ..
        } => vocabulary.language_literal(lexical, language),
        Token::Literal {
            lexical, datatype, ..
        } => {
            let datatype = datatype.as_deref().map_or_else(
                || "http://www.w3.org/2001/XMLSchema#string".to_owned(),
                |datatype| expand(datatype, prefixes),
            );
            vocabulary.literal(lexical, &datatype)
        }
    })
}

/// One statement line's options: its contexts and constraints (`[Cut]` dropped).
struct Options {
    context: Option<Token>,
    constraints: Vec<String>,
}

fn options(raw: Vec<String>) -> Result<Options, String> {
    let mut out = Options {
        context: None,
        constraints: Vec::new(),
    };
    for option in raw {
        let option = option.trim();
        if let Some(rest) = option.strip_prefix("Constraint") {
            out.constraints
                .extend(rest.split(',').map(|c| c.trim().to_owned()));
        } else if let Some(rest) = option.strip_prefix("Context") {
            let (tokens, _) = tokens(rest.trim())?;
            match tokens.as_slice() {
                [iri @ Token::Iri(_)] => out.context = Some(iri.clone()),
                _ => return Err(format!("a context isn't one IRI: [{option}]")),
            }
        } else if option != "Cut" {
            return Err(format!("unknown option [{option}]"));
        }
    }
    Ok(out)
}

/// The predicate a statement in `context` gets ([`compile`]'s docs).
fn context_predicate(
    context: &Token,
    predicate: &Token,
    prefixes: &HashMap<String, String>,
) -> Result<Token, String> {
    match (context, predicate) {
        (Token::Iri(context), Token::Iri(predicate)) => Ok(Token::Iri(format!(
            "urn:nrese:pie-context:{}#{}",
            expand(context, prefixes),
            expand(predicate, prefixes)
        ))),
        _ => Err("a context on a statement whose predicate is a variable".into()),
    }
}

fn compile_rule(
    kind: &str,
    name: &str,
    lines: &[String],
    prefixes: &HashMap<String, String>,
    vocabulary: &mut impl Vocabulary,
) -> Result<Rule, String> {
    let divider = lines
        .iter()
        .position(|line| line.starts_with("---"))
        .ok_or("no line of dashes between premises and conclusions")?;
    let mut variables: HashMap<String, u8> = HashMap::new();
    let term = |token: &Token,
                variables: &mut HashMap<String, u8>,
                vocabulary: &mut _|
     -> Result<Term, String> {
        match token {
            Token::Var(v) => {
                let next = u8::try_from(variables.len()).map_err(|_| "too many variables")?;
                Ok(Term::Var(*variables.entry(v.clone()).or_insert(next)))
            }
            other => constant(other, prefixes, vocabulary).map(Term::Const),
        }
    };
    // Statements of a block of lines, with the lines' constraints.
    let statements = |lines: &[String]| -> Result<(Vec<[Token; 3]>, Vec<String>), String> {
        let mut statements = Vec::new();
        let mut constraints = Vec::new();
        for line in lines {
            let (terms, raw) = tokens(line)?;
            let options = options(raw)?;
            constraints.extend(options.constraints);
            for triple in terms.chunks(3) {
                let [s, p, o] = triple else {
                    return Err(format!("a statement isn't three terms: {line}"));
                };
                let p = match &options.context {
                    Some(context) => context_predicate(context, p, prefixes)?,
                    None => p.clone(),
                };
                statements.push([s.clone(), p, o.clone()]);
            }
        }
        Ok((statements, constraints))
    };
    let (premises, mut constraints) = statements(&lines[..divider])?;
    let (conclusions, more) = statements(&lines[divider + 1..])?;
    constraints.extend(more);
    let mut body = Vec::new();
    for [s, p, o] in &premises {
        body.push(Atom([
            term(s, &mut variables, vocabulary)?,
            term(p, &mut variables, vocabulary)?,
            term(o, &mut variables, vocabulary)?,
        ]));
    }
    let bound = variables.clone();
    let mut guards = Vec::new();
    for constraint in constraints {
        let (left, right) = constraint
            .split_once("!=")
            .ok_or_else(|| format!("a constraint isn't 'a != b': {constraint}"))?;
        let side = |text: &str, vocabulary: &mut _| -> Result<Term, String> {
            let (tokens, _) = tokens(text.trim())?;
            let [token] = tokens.as_slice() else {
                return Err(format!("a constraint side isn't one term: {text}"));
            };
            match token {
                Token::Var(v) => bound
                    .get(v)
                    .map(|&v| Term::Var(v))
                    .ok_or_else(|| format!("constraint variable {v} isn't in a premise")),
                other => constant(other, prefixes, vocabulary).map(Term::Const),
            }
        };
        if right.trim() == "blank_node" {
            let (low, high) = vocabulary
                .blank_node_ids()
                .ok_or("this store can't tell blank nodes in a rule")?;
            guards.push(Guard::NotIn(side(left, vocabulary)?, low, high));
        } else {
            guards.push(Guard::NotEqual(
                side(left, vocabulary)?,
                side(right, vocabulary)?,
            ));
        }
    }
    let mut head_atoms = Vec::new();
    for [s, p, o] in &conclusions {
        for token in [s, p, o] {
            if let Token::Var(v) = token
                && !bound.contains_key(v)
            {
                return Err(format!("conclusion variable {v} isn't bound by a premise"));
            }
        }
        head_atoms.push(Atom([
            term(s, &mut variables, vocabulary)?,
            term(p, &mut variables, vocabulary)?,
            term(o, &mut variables, vocabulary)?,
        ]));
    }
    let head = match kind {
        "Consistency" if head_atoms.is_empty() => Head::Inconsistent,
        "Consistency" => return Err("a consistency check has conclusions".into()),
        _ if head_atoms.is_empty() => return Err("a rule without conclusions".into()),
        _ => Head::Facts(head_atoms),
    };
    if body.is_empty() {
        return Err("a rule without premises".into());
    }
    Ok(Rule {
        name: name.to_owned(),
        body,
        guards,
        head,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2::testing::LocalVocabulary;

    #[test]
    fn errors_name_the_line_of_their_rule_or_axiom() {
        let line = |text: &str, start: &str| {
            text.lines()
                .position(|line| line.starts_with(start))
                .unwrap()
                + 1
        };
        let mut vocabulary = LocalVocabulary::default();
        // A conclusion variable no premise binds: the line of its rule's `Id:`.
        let broken = RULES.replace("  x  <rdf:type>  z\n", "  w  <rdf:type>  z\n");
        let error = compile("test", &broken, &mut vocabulary).unwrap_err();
        assert_eq!(error.rule, "test#rdfs2");
        assert_eq!(
            error.position.map(|p| p.line),
            Some(line(&broken, "Id: rdfs2")),
            "{error}"
        );
        // An axiom of two terms: its own line, after a multi-line comment.
        let broken = RULES
            .replace("// A test ruleset.", "/* A test\n   ruleset. */")
            .replace("<ex:a> <rdfs:label> \"A label\"@en", "<ex:a> <rdfs:label>");
        let error = compile("test", &broken, &mut vocabulary).unwrap_err();
        assert_eq!(
            error.position.map(|p| p.line),
            Some(line(&broken, "  <ex:a> <rdfs:label>")),
            "{error}"
        );
    }

    /// A ruleset in GraphDB's syntax, written for this test.
    const RULES: &str = r#"
// A test ruleset.
Prefices
{
  rdf  : http://www.w3.org/1999/02/22-rdf-syntax-ns#
  rdfs : http://www.w3.org/2000/01/rdf-schema#
  ex   : http://example.com/
}

Axioms
{
  <rdf:type> <rdf:type> <rdf:Property>   /* a comment */
  <ex:a> <rdfs:label> "A label"@en
}

Rules
{
Id: rdfs2
  x  a  y                 [Constraint a != <rdf:type>]
  a  <rdfs:domain>  z     [Cut]
  -------------------------------
  x  <rdf:type>  z

Id: two_heads
  x <ex:p> y
  ----------
  y <ex:q> x
  x <ex:r> "1"^^<http://www.w3.org/2001/XMLSchema#integer>

Consistency: self_friend
  x <ex:friend> y      [Constraint x != <ex:nobody>]
  -------------------------------
}
"#;

    #[test]
    fn a_ruleset_compiles_into_rules_axioms_and_checks() {
        let mut vocabulary = LocalVocabulary::default();
        let program = compile("test.pie", RULES, &mut vocabulary).unwrap();
        assert_eq!(program.facts.len(), 2);
        assert_eq!(program.rules.len(), 3);
        let rdfs2 = &program.rules[0];
        assert_eq!(rdfs2.name, "rdfs2");
        assert_eq!(rdfs2.body.len(), 2);
        assert_eq!(rdfs2.guards.len(), 1);
        let rdf_type = vocabulary.iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
        assert_eq!(
            rdfs2.head,
            Head::Facts(vec![Atom([
                Term::Var(0),
                Term::Const(rdf_type),
                Term::Var(3)
            ])])
        );
        assert_eq!(
            rdfs2.guards[0],
            Guard::NotEqual(Term::Var(1), Term::Const(rdf_type))
        );
        let Head::Facts(two) = &program.rules[1].head else {
            panic!("facts")
        };
        assert_eq!(two.len(), 2);
        assert_eq!(program.rules[2].head, Head::Inconsistent);
    }

    #[test]
    fn unsupported_parts_are_errors_that_name_them() {
        let mut vocabulary = LocalVocabulary::default();
        let cases = [
            (
                "Rules { Id: r\n x p y [Context <g>]\n ---\n x <p:b> y\n }",
                "a context on",
            ),
            (
                "Rules { Id: r\n x <p:a> y\n ---\n x <p:b> z\n }",
                "isn't bound",
            ),
            (
                "Rules { Id: r\n x <p:a> _:b\n ---\n x <p:b> x\n }",
                "blank nodes",
            ),
            ("Rules { Id: r\n x <p:a> y\n x <p:b> y\n }", "dashes"),
            ("Things { }", "unknown section"),
        ];
        for (text, expected) in cases {
            let error = compile("bad.pie", text, &mut vocabulary).unwrap_err();
            assert!(error.message.contains(expected), "{text}: {error}");
        }
    }
}

/// GraphDB's own rulesets (`builtin_*.pie` from its distribution), where `NRESE_PIE_DIR`
/// points to them: each compiles. They are Ontotext's files and aren't in this repository.
#[cfg(test)]
#[test]
fn graphdb_rulesets_compile() {
    let Some(dir) = std::env::var_os("NRESE_PIE_DIR") else {
        return;
    };
    let mut failures = Vec::new();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("NRESE_PIE_DIR")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|e| e == "pie"))
        .collect();
    files.sort();
    for path in &files {
        let text = std::fs::read_to_string(path).expect("read");
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut vocabulary = crate::v2::testing::LocalVocabulary::default();
        // GraphDB's OWL 2 QL and RL rulesets use what the IR doesn't express (module docs).
        let unsupported = if name.contains("owl2-ql") {
            Some("isn't bound by a premise")
        } else if name.contains("owl2-rl") {
            Some("predicate is a variable")
        } else {
            None
        };
        match (compile(&name, &text, &mut vocabulary), unsupported) {
            (Ok(program), None) => eprintln!(
                "{name}: {} rules, {} axioms",
                program.rules.len(),
                program.facts.len()
            ),
            (Err(error), Some(expected)) if error.message.contains(expected) => {
                eprintln!("{name}: unsupported as expected ({error})");
            }
            (Ok(_), Some(_)) => failures.push(format!("{name}: compiles now; update this test")),
            (Err(error), _) => failures.push(format!("{name}: {error}")),
        }
    }
    assert!(!files.is_empty(), "no .pie files in NRESE_PIE_DIR");
    assert!(failures.is_empty(), "{failures:#?}");
}
