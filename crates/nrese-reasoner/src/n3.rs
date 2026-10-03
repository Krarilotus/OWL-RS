//! User rules in Notation3, compiled into the rule IR ([`super::ir`]).
//!
//! What compiles (the datalog part of N3, which every N3 reasoner shares):
//! - `{ body } => { head } .` and `{ head } <= { body } .`: a rule. Quick variables
//!   (`?x`) and the blank nodes of the body are its variables; every variable of the head
//!   must occur in a body triple.
//! - `{ body } => false .`: a consistency check (the commit is rejected, like an OWL 2 RL
//!   violation).
//! - `log:notEqualTo` in the body: a guard. `log:equalTo`: the two terms are made one
//!   (a variable takes the other term's place); a rule that equates two different
//!   constants can never fire, and is left out.
//! - Triples outside formulas: facts that hold whatever the data (axioms).
//!
//! What doesn't, with an error that names it: other builtins (`math:`, `string:`, `list:`,
//! `time:`, the rest of `log:`), blank nodes in a head (existential conclusions), formulas
//! as terms, rules inside formulas, and blank nodes in facts.

use std::collections::HashMap;

use nrese_rdf::vocab::xsd;
use nrese_rdf::{BlankNode, GraphName, Literal, Term};
use nrese_rdf_io::n3::{N3Parser, N3Quad, N3Term};

use super::ir::{Atom, Guard, Head, ParseError, Rule, Term as IrTerm, Vocabulary};

const LOG: &str = "http://www.w3.org/2000/10/swap/log#";
/// Builtins live in the SWAP namespaces (`log:`, `math:`, `string:`, `list:`, `time:`…).
const SWAP: &str = "http://www.w3.org/2000/10/swap/";

/// The rules and facts of an N3 rules document, as ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct N3Program {
    pub rules: Vec<Rule>,
    /// Facts outside formulas: they seed the closure and are never retracted.
    pub facts: Vec<[u64; 3]>,
}

/// Compiles `text`, an N3 document (`name` names its rules: `name#1`, `name#2`, …).
pub fn compile(
    name: &str,
    text: &str,
    vocabulary: &mut impl Vocabulary,
) -> Result<N3Program, ParseError> {
    let quads: Vec<N3Quad> = N3Parser::new()
        .for_slice(text.as_bytes())
        .collect::<Result<_, _>>()
        .map_err(|e| match e {
            nrese_rdf_io::RdfParseError::Syntax(syntax) => {
                let start = syntax.location().start;
                ParseError::new(name, format!("not valid N3: {}", syntax.message())).at(
                    usize::try_from(start.line)
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                    usize::try_from(start.column)
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                )
            }
            e => ParseError::new(name, format!("not valid N3: {e}")),
        })?;
    compile_quads(name, &quads, vocabulary)
}

/// [`compile`] on a document already read.
pub fn compile_quads(
    name: &str,
    quads: &[N3Quad],
    vocabulary: &mut impl Vocabulary,
) -> Result<N3Program, ParseError> {
    let error = |message: String| ParseError::new(name, message);
    // The statements of each formula, by its blank node.
    let mut formulas: HashMap<&BlankNode, Vec<&N3Quad>> = HashMap::new();
    for quad in quads {
        if let GraphName::BlankNode(formula) = &quad.graph_name {
            formulas.entry(formula).or_default().push(quad);
        }
    }
    let implies = format!("{LOG}implies");
    let implied_by = format!("{LOG}isImpliedBy");
    let mut program = N3Program {
        rules: Vec::new(),
        facts: Vec::new(),
    };
    for quad in quads.iter().filter(|q| q.graph_name.is_default_graph()) {
        let rule_name = format!("{name}#{}", program.rules.len() + 1);
        let (body, head) = match &quad.predicate {
            N3Term::NamedNode(p) if p.as_str() == implies => (&quad.subject, &quad.object),
            N3Term::NamedNode(p) if p.as_str() == implied_by => (&quad.object, &quad.subject),
            _ => {
                program.facts.push(fact(quad, vocabulary).map_err(&error)?);
                continue;
            }
        };
        let rule = Compiler::default()
            .rule(&rule_name, body, head, &formulas, vocabulary)
            .map_err(|message| ParseError::new(rule_name.clone(), message))?;
        program.rules.extend(rule);
    }
    Ok(program)
}

/// One rule's variables, by name, and the substitutions `log:equalTo` makes.
#[derive(Default)]
struct Compiler {
    variables: Vec<String>,
    equal: HashMap<String, Resolved>,
}

/// A body or head term before it becomes an id: a variable (by name) or a constant.
#[derive(Clone, PartialEq, Eq)]
enum Resolved {
    Var(String),
    Const(Term),
}

impl Compiler {
    /// The rule `body => head`, or `None` if it can never fire.
    fn rule(
        mut self,
        name: &str,
        body: &N3Term,
        head: &N3Term,
        formulas: &HashMap<&BlankNode, Vec<&N3Quad>>,
        vocabulary: &mut impl Vocabulary,
    ) -> Result<Option<Rule>, String> {
        let body = formula(body, formulas, "premise")?;
        if body.is_empty() {
            return Err("a rule without premises".to_owned());
        }
        // Builtins first: log:equalTo substitutes, log:notEqualTo guards.
        let mut atoms = Vec::new();
        let mut not_equal = Vec::new();
        for quad in &body {
            let predicate = match &quad.predicate {
                N3Term::NamedNode(p) => p.as_str(),
                _ => "",
            };
            match predicate.strip_prefix(LOG) {
                Some("equalTo") => {
                    let (a, b) = (self.resolve(&quad.subject)?, self.resolve(&quad.object)?);
                    if !self.unify(a, b) {
                        return Ok(None);
                    }
                }
                Some("notEqualTo") => not_equal.push((&quad.subject, &quad.object)),
                _ if predicate.starts_with(SWAP) => {
                    return Err(format!("the builtin <{predicate}> isn't supported"));
                }
                _ => atoms.push(*quad),
            }
        }
        let mut compiled = Vec::new();
        for quad in atoms {
            compiled.push(self.atom(quad, vocabulary)?);
        }
        let bound: Vec<IrTerm> = compiled.iter().flat_map(|a: &Atom| a.0).collect();
        let check_bound = |terms: &[IrTerm], what: &str| -> Result<(), String> {
            match terms
                .iter()
                .find(|t| matches!(t, IrTerm::Var(_)) && !bound.contains(t))
            {
                Some(_) => Err(format!("a variable in {what} isn't bound by a premise")),
                None => Ok(()),
            }
        };
        let mut guards = Vec::new();
        for (a, b) in not_equal {
            let a = self.term(&self.resolve(a)?, vocabulary)?;
            let b = self.term(&self.resolve(b)?, vocabulary)?;
            check_bound(&[a, b], "log:notEqualTo")?;
            guards.push(Guard::NotEqual(a, b));
        }
        let head = match head {
            N3Term::Literal(l) if l.value() == "false" && l.datatype() == xsd::BOOLEAN => {
                Head::Inconsistent
            }
            head => {
                let quads = formula(head, formulas, "conclusion")?;
                let mut facts = Vec::new();
                for quad in quads {
                    for term in [&quad.subject, &quad.predicate, &quad.object] {
                        if matches!(term, N3Term::BlankNode(_)) {
                            return Err(
                                "a blank node in the conclusion (an existential) isn't supported"
                                    .to_owned(),
                            );
                        }
                    }
                    let atom = self.atom(quad, vocabulary)?;
                    check_bound(&atom.0, "the conclusion")?;
                    facts.push(atom);
                }
                Head::Facts(facts)
            }
        };
        if compiled.is_empty() {
            return Err("a rule whose premises are only builtins".to_owned());
        }
        Ok(Some(Rule {
            name: name.to_owned(),
            body: compiled,
            guards,
            head,
        }))
    }

    /// A term with the substitutions so far applied.
    fn resolve(&self, term: &N3Term) -> Result<Resolved, String> {
        let resolved = match term {
            N3Term::Variable(v) => Resolved::Var(format!("?{}", v.as_str())),
            // A blank node in a premise is a variable of the rule.
            N3Term::BlankNode(b) => Resolved::Var(format!("_:{}", b.as_str())),
            N3Term::NamedNode(n) => Resolved::Const(n.clone().into()),
            N3Term::Literal(l) => Resolved::Const(l.clone().into()),
            N3Term::Triple(t) => Resolved::Const(Term::Triple(t.clone())),
        };
        Ok(self.substitute(resolved))
    }

    fn substitute(&self, mut term: Resolved) -> Resolved {
        while let Resolved::Var(name) = &term {
            match self.equal.get(name) {
                Some(next) => term = next.clone(),
                None => break,
            }
        }
        term
    }

    /// Makes `a` and `b` one; `false` if they are two different constants.
    fn unify(&mut self, a: Resolved, b: Resolved) -> bool {
        match (a, b) {
            (a, b) if a == b => true,
            (Resolved::Var(name), other) | (other, Resolved::Var(name)) => {
                self.equal.insert(name, other);
                true
            }
            _ => false,
        }
    }

    fn atom(&mut self, quad: &N3Quad, vocabulary: &mut impl Vocabulary) -> Result<Atom, String> {
        let mut terms = [IrTerm::Var(0); 3];
        for (slot, term) in terms
            .iter_mut()
            .zip([&quad.subject, &quad.predicate, &quad.object])
        {
            let resolved = self.resolve(term)?;
            *slot = self.term(&resolved, vocabulary)?;
        }
        Ok(Atom(terms))
    }

    fn term(
        &mut self,
        term: &Resolved,
        vocabulary: &mut impl Vocabulary,
    ) -> Result<IrTerm, String> {
        match term {
            Resolved::Var(name) => {
                let index = match self.variables.iter().position(|v| v == name) {
                    Some(i) => i,
                    None => {
                        self.variables.push(name.clone());
                        self.variables.len() - 1
                    }
                };
                u8::try_from(index)
                    .map(IrTerm::Var)
                    .map_err(|_| "more than 256 variables in one rule".to_owned())
            }
            Resolved::Const(term) => constant(term, vocabulary).map(IrTerm::Const),
        }
    }
}

/// The statements of the formula `term` stands for.
fn formula<'a>(
    term: &N3Term,
    formulas: &'a HashMap<&BlankNode, Vec<&'a N3Quad>>,
    what: &str,
) -> Result<Vec<&'a N3Quad>, String> {
    let N3Term::BlankNode(node) = term else {
        return Err(format!("the {what} isn't a formula {{ … }}"));
    };
    let quads = formulas.get(node).cloned().unwrap_or_default();
    for quad in &quads {
        for part in [&quad.subject, &quad.object] {
            if let N3Term::BlankNode(b) = part
                && formulas.contains_key(b)
            {
                return Err(format!("a formula inside the {what} isn't supported"));
            }
        }
        if let N3Term::NamedNode(p) = &quad.predicate
            && (p.as_str() == format!("{LOG}implies") || p.as_str() == format!("{LOG}isImpliedBy"))
        {
            return Err(format!("a rule inside the {what} isn't supported"));
        }
    }
    Ok(quads)
}

/// A fact outside formulas, as ids.
fn fact(quad: &N3Quad, vocabulary: &mut impl Vocabulary) -> Result<[u64; 3], String> {
    let mut ids = [0; 3];
    for (slot, term) in ids
        .iter_mut()
        .zip([&quad.subject, &quad.predicate, &quad.object])
    {
        *slot = match term {
            N3Term::NamedNode(n) => vocabulary.iri(n.as_str()),
            N3Term::Literal(l) => literal(l, vocabulary)?,
            N3Term::BlankNode(_) => {
                return Err(format!(
                    "a blank node in the fact {quad} (or a formula outside a rule) isn't supported"
                ));
            }
            other => return Err(format!("{other} can't be part of a fact")),
        };
    }
    Ok(ids)
}

fn constant(term: &Term, vocabulary: &mut impl Vocabulary) -> Result<u64, String> {
    match term {
        Term::NamedNode(n) => Ok(vocabulary.iri(n.as_str())),
        Term::Literal(l) => literal(l, vocabulary),
        other => Err(format!("{other} can't be a constant of a rule yet")),
    }
}

fn literal(literal: &Literal, vocabulary: &mut impl Vocabulary) -> Result<u64, String> {
    if literal.direction().is_some() {
        return Err(format!(
            "the directional literal {literal} isn't supported in rules yet"
        ));
    }
    Ok(match literal.language() {
        Some(language) => vocabulary.language_literal(literal.value(), language),
        None => vocabulary.literal(literal.value(), literal.datatype().as_str()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vocabulary::LocalVocabulary;

    fn compiled(text: &str) -> Result<(N3Program, LocalVocabulary), ParseError> {
        let mut vocabulary = LocalVocabulary::default();
        let program = compile("test", text, &mut vocabulary)?;
        Ok((program, vocabulary))
    }

    #[test]
    fn syntax_errors_name_their_line_and_column() {
        let error = compiled("@prefix : <http://e/> .\n{ ?x :p ?y } => { ?y :p ?x \n:a :p .\n")
            .unwrap_err();
        let position = error.position.expect("a position");
        assert_eq!(position.line, 3, "{error}");
        assert!(position.column >= 1);
        assert!(error.to_string().starts_with("line 3, column "), "{error}");
    }

    #[test]
    fn a_datalog_rule() {
        let (program, mut vocabulary) = compiled(
            "@prefix : <http://e/> .
             { ?x :parent ?y . ?y :parent ?z } => { ?x :grandparent ?z } .
             :anna :parent :ben .",
        )
        .unwrap();
        assert_eq!(program.rules.len(), 1);
        let rule = &program.rules[0];
        assert_eq!(rule.name, "test#1");
        assert_eq!(rule.body.len(), 2);
        let parent = vocabulary.iri("http://e/parent");
        assert_eq!(
            rule.body[0],
            Atom([IrTerm::Var(0), IrTerm::Const(parent), IrTerm::Var(1)])
        );
        assert_eq!(
            rule.head,
            Head::Facts(vec![Atom([
                IrTerm::Var(0),
                IrTerm::Const(vocabulary.iri("http://e/grandparent")),
                IrTerm::Var(2)
            ])])
        );
        assert_eq!(
            program.facts,
            vec![[
                vocabulary.iri("http://e/anna"),
                parent,
                vocabulary.iri("http://e/ben")
            ]]
        );
    }

    #[test]
    fn builtins_inconsistency_and_reverse_rules() {
        let (program, _) = compiled(
            "@prefix : <http://e/> . @prefix log: <http://www.w3.org/2000/10/swap/log#> .
             { ?x :sameName ?y . ?x log:notEqualTo ?y } => { ?x :twin ?y } .
             { ?x :a ?y . ?y log:equalTo :b } => { ?x :ab true } .
             { ?x :age ?a . ?x :noAge true } => false .
             { ?x :child ?y } <= { ?y :parent ?x } .",
        )
        .unwrap();
        assert_eq!(program.rules.len(), 4);
        assert_eq!(program.rules[0].guards.len(), 1);
        // `?y log:equalTo :b` put :b in ?y's place.
        assert!(matches!(program.rules[1].body[0].0[2], IrTerm::Const(_)));
        assert_eq!(program.rules[2].head, Head::Inconsistent);
        assert_eq!(program.rules[3].body.len(), 1);
    }

    #[test]
    fn what_isnt_supported_is_an_error() {
        for (text, expected) in [
            (
                "{ ?x <http://e/p> ?y } => { ?x <http://e/q> [] } .",
                "existential",
            ),
            (
                "{ ?x <http://e/p> ?y } => { ?x <http://e/q> ?z } .",
                "isn't bound",
            ),
            (
                "{ ?x <http://www.w3.org/2000/10/swap/math#greaterThan> 1 } => { ?x <http://e/q> 1 } .",
                "builtin",
            ),
            ("_:b <http://e/p> 1 .", "blank node in the fact"),
            (
                "{ <http://e/a> <http://www.w3.org/2000/10/swap/log#equalTo> <http://e/b> . ?x <http://e/p> ?y } => { ?x <http://e/q> ?y } .",
                "",
            ),
        ] {
            match compiled(text) {
                Err(error) => assert!(error.message.contains(expected), "{text}: {error}"),
                // Two different constants equal: the rule is dropped, not an error.
                Ok((program, _)) => {
                    assert!(expected.is_empty() && program.rules.is_empty(), "{text}")
                }
            }
        }
    }
}
