//! The regular expressions of XML Schema 1.1 Part 2 (Appendix G): what `xsd:pattern`
//! takes. A pattern matches a whole string (there are no anchors: `^` and `$` are
//! ordinary characters), classes subtract (`[a-z-[aeiou]]`), and `\i`, `\c`, `\p{…}` name
//! XML's and Unicode's classes.

use super::chars::CharSet;

/// A parsed expression.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Ast {
    /// One character of a set.
    Class(CharSet),
    /// Each in turn (none: the empty string).
    Concat(Vec<Ast>),
    /// Any one of them.
    Alt(Vec<Ast>),
    /// Between `min` and `max` (`None`: unbounded) times.
    Repeat(Box<Ast>, u32, Option<u32>),
}

/// Why a pattern isn't a regular expression of XML Schema.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an XML Schema regular expression at character {at}: {message}")]
pub struct PatternError {
    pub at: usize,
    pub message: String,
}

pub(crate) fn parse(pattern: &str) -> Result<Ast, PatternError> {
    let mut p = Parser {
        chars: pattern.chars().collect(),
        at: 0,
    };
    let ast = p.alternatives()?;
    match p.peek() {
        None => Ok(ast),
        Some(c) => Err(p.error(format!("unexpected {c:?}"))),
    }
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.at += 1;
        Some(c)
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn error(&self, message: String) -> PatternError {
        PatternError {
            at: self.at,
            message,
        }
    }

    /// `regExp ::= branch ( '|' branch )*`.
    fn alternatives(&mut self) -> Result<Ast, PatternError> {
        let mut branches = vec![self.branch()?];
        while self.eat('|') {
            branches.push(self.branch()?);
        }
        Ok(if branches.len() == 1 {
            branches.pop().unwrap_or(Ast::Concat(Vec::new()))
        } else {
            Ast::Alt(branches)
        })
    }

    /// `branch ::= piece*`.
    fn branch(&mut self) -> Result<Ast, PatternError> {
        let mut pieces = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let atom = self.atom()?;
            pieces.push(self.quantified(atom)?);
        }
        Ok(if pieces.len() == 1 {
            pieces.pop().unwrap_or(Ast::Concat(Vec::new()))
        } else {
            Ast::Concat(pieces)
        })
    }

    /// `quantifier ::= [?*+] | ( '{' quantity '}' )`.
    fn quantified(&mut self, atom: Ast) -> Result<Ast, PatternError> {
        let (min, max) = match self.peek() {
            Some('?') => (0, Some(1)),
            Some('*') => (0, None),
            Some('+') => (1, None),
            Some('{') => {
                self.at += 1;
                let min = self.number()?;
                let max = if self.eat(',') {
                    if self.peek() == Some('}') {
                        None
                    } else {
                        Some(self.number()?)
                    }
                } else {
                    Some(min)
                };
                if !self.eat('}') {
                    return Err(self.error("a quantity ends with '}'".into()));
                }
                if max.is_some_and(|m| m < min) {
                    return Err(self.error(format!("{{{min},{max:?}}} is empty")));
                }
                return Ok(Ast::Repeat(Box::new(atom), min, max));
            }
            _ => return Ok(atom),
        };
        self.at += 1;
        Ok(Ast::Repeat(Box::new(atom), min, max))
    }

    fn number(&mut self) -> Result<u32, PatternError> {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        let digits: String = self.chars[start..self.at].iter().collect();
        digits
            .parse()
            .map_err(|_| self.error("a quantity is a number".into()))
    }

    /// `atom ::= NormalChar | charClass | ( '(' regExp ')' )`.
    fn atom(&mut self) -> Result<Ast, PatternError> {
        let Some(c) = self.next() else {
            return Err(self.error("an atom expected".into()));
        };
        Ok(match c {
            '(' => {
                let inner = self.alternatives()?;
                if !self.eat(')') {
                    return Err(self.error("unclosed '('".into()));
                }
                inner
            }
            '[' => Ast::Class(self.class_expression()?),
            '.' => Ast::Class(CharSet::dot()),
            '\\' => Ast::Class(self.escape()?.into_set()),
            '?' | '*' | '+' | '{' | '}' | ']' => {
                self.at -= 1;
                return Err(self.error(format!("{c:?} must be escaped")));
            }
            c => Ast::Class(CharSet::single(c)),
        })
    }

    /// After `\`: a single character, or a class.
    fn escape(&mut self) -> Result<Escape, PatternError> {
        let Some(c) = self.next() else {
            return Err(self.error("a '\\' ends the pattern".into()));
        };
        Ok(match c {
            'n' => Escape::Char('\n'),
            'r' => Escape::Char('\r'),
            't' => Escape::Char('\t'),
            '\\' | '|' | '.' | '?' | '*' | '+' | '(' | ')' | '{' | '}' | '-' | '[' | ']' | '^'
            | '$' => Escape::Char(c),
            's' => Escape::Set(CharSet::space()),
            'S' => Escape::Set(CharSet::space().complement()),
            'i' => Escape::Set(CharSet::name_start()),
            'I' => Escape::Set(CharSet::name_start().complement()),
            'c' => Escape::Set(CharSet::name_char()),
            'C' => Escape::Set(CharSet::name_char().complement()),
            'd' => Escape::Set(CharSet::digit()),
            'D' => Escape::Set(CharSet::digit().complement()),
            'w' => Escape::Set(CharSet::word()),
            'W' => Escape::Set(CharSet::word().complement()),
            'p' | 'P' => {
                if !self.eat('{') {
                    return Err(self.error("\\p takes {name}".into()));
                }
                let start = self.at;
                while self.peek().is_some_and(|c| c != '}') {
                    self.at += 1;
                }
                let name: String = self.chars[start..self.at].iter().collect();
                if !self.eat('}') {
                    return Err(self.error("unclosed \\p{".into()));
                }
                let set = CharSet::property(&name)
                    .ok_or_else(|| self.error(format!("unknown property {name:?}")))?;
                Escape::Set(if c == 'p' { set } else { set.complement() })
            }
            c => {
                self.at -= 1;
                return Err(self.error(format!("unknown escape \\{c}")));
            }
        })
    }

    /// After `[`: `charGroup ']'`, where `charGroup ::= ( posCharGroup | negCharGroup )
    /// ( '-' charClassExpr )?`.
    fn class_expression(&mut self) -> Result<CharSet, PatternError> {
        let negated = self.eat('^');
        let mut set = CharSet::empty();
        let mut first = true;
        loop {
            match self.peek() {
                None => return Err(self.error("unclosed '['".into())),
                Some(']') if !first => {
                    self.at += 1;
                    break;
                }
                Some('-') if self.peek_at(1) == Some('[') && !first => {
                    self.at += 2;
                    let subtracted = self.class_expression()?;
                    if !self.eat(']') {
                        return Err(self.error("a subtraction ends the class".into()));
                    }
                    let set = if negated { set.complement() } else { set };
                    return Ok(set.minus(&subtracted));
                }
                _ => {}
            }
            first = false;
            let from = match self.next() {
                Some('\\') => match self.escape()? {
                    Escape::Char(c) => c,
                    Escape::Set(s) => {
                        set = set.union(&s);
                        continue;
                    }
                },
                Some('[') => return Err(self.error("'[' in a class must be escaped".into())),
                Some(c) => c,
                None => return Err(self.error("unclosed '['".into())),
            };
            // A range, unless the '-' ends the group or starts a subtraction.
            let range = self.peek() == Some('-') && !matches!(self.peek_at(1), Some(']' | '['));
            if !range {
                set = set.union(&CharSet::single(from));
                continue;
            }
            self.at += 1;
            let to = match self.next() {
                Some('\\') => match self.escape()? {
                    Escape::Char(c) => c,
                    Escape::Set(_) => {
                        return Err(self.error("a range ends with a character".into()));
                    }
                },
                Some(c) => c,
                None => return Err(self.error("unclosed '['".into())),
            };
            if to < from {
                return Err(self.error(format!("the range {from:?}-{to:?} is reversed")));
            }
            set = set.union(&CharSet::from_ranges([(u32::from(from), u32::from(to))]));
        }
        Ok(if negated { set.complement() } else { set })
    }
}

enum Escape {
    Char(char),
    Set(CharSet),
}

impl Escape {
    fn into_set(self) -> CharSet {
        match self {
            Escape::Char(c) => CharSet::single(c),
            Escape::Set(s) => s,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grammar_reads_and_refuses() {
        for ok in [
            "",
            "a(b|c)",
            "[0-9]{3}-[0-9]{4}",
            "[a-z-[aeiou]]+",
            r"\p{Lu}\P{IsBasicLatin}*",
            r"[\i-[:]][\c-[:]]*",
            "x{2,}y{0,3}",
            "^$",
            "[-a]",
            "[a-]",
            r"\.\?\*\+\(\)\{\}\[\]\-\^\\\|",
            "a|",
            "()",
        ] {
            assert!(parse(ok).is_ok(), "{ok:?}: {:?}", parse(ok));
        }
        for bad in [
            "(",
            ")",
            "[",
            "[]",
            "a{2,1}",
            "*a",
            "a{x}",
            r"\q",
            r"\p{Nope}",
            "[z-a]",
            "]",
            "a{",
            "[a-[b]c]",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn classes_subtract_and_negate() {
        let Ast::Repeat(inner, 1, None) = parse("[a-z-[aeiou]]+").unwrap() else {
            panic!()
        };
        let Ast::Class(set) = *inner else { panic!() };
        assert_eq!(set.len(), 21);
        assert!(!set.contains('e') && set.contains('b'));
        let Ast::Class(set) = parse("[^a-z]").unwrap() else {
            panic!()
        };
        assert!(!set.contains('q') && set.contains('Q'));
    }
}
