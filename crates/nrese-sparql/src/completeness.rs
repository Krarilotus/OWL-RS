//! Whether a query's answers are sound and complete, and why not: one status for every
//! reasoning path that can fall short (docs/design/ql-rewriting.md §7; the DL bounds'
//! status in docs/design/owl2-dl.md, "Every answer carries its status"). The OWL 2 QL
//! rewriting reports `sound` and, where an axiom it doesn't follow meets anonymous
//! individuals, not `complete`; the DL bounds add their counts. Sources add reasons;
//! answers are never changed for them.

use std::fmt;

/// A query's completeness. The default: sound and complete, nothing to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completeness {
    /// Every answer is certain.
    pub sound: bool,
    /// Every certain answer is there.
    pub complete: bool,
    /// Why not, each from its source.
    pub reasons: Vec<Reason>,
    /// The bounds' counts, where a source knows them.
    pub bounds: Option<Bounds>,
}

/// Why the answers may fall short: the source that says so (`ql`, `dl`) and what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reason {
    pub source: &'static str,
    pub text: String,
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.source, self.text)
    }
}

/// Answer counts between a lower and an upper bound (the DL bounds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub lower: u64,
    pub upper: u64,
    pub unresolved: u64,
}

impl Default for Completeness {
    fn default() -> Self {
        Self {
            sound: true,
            complete: true,
            reasons: Vec::new(),
            bounds: None,
        }
    }
}

/// The reasons a header shows; the rest are counted.
const SHOWN: usize = 5;

impl Completeness {
    /// `complete`, `sound-only` or `unsound`.
    pub fn as_str(&self) -> &'static str {
        match (self.sound, self.complete) {
            (true, true) => "complete",
            (true, false) => "sound-only",
            (false, _) => "unsound",
        }
    }

    /// Notes that some certain answers may be missing, and why (each reason once).
    pub fn incomplete(&mut self, source: &'static str, text: String) {
        self.complete = false;
        if !self
            .reasons
            .iter()
            .any(|r| r.source == source && r.text == text)
        {
            self.reasons.push(Reason { source, text });
        }
    }

    /// Both statuses at once: what either lacks, the other's reasons too.
    pub fn merge(&mut self, other: Completeness) {
        self.sound &= other.sound;
        self.complete &= other.complete;
        for r in other.reasons {
            if !self.reasons.contains(&r) {
                self.reasons.push(r);
            }
        }
        self.bounds = self.bounds.or(other.bounds);
    }

    /// The value of the `NRESE-Completeness` header: the status, then the bounds and the
    /// first reasons, ASCII only (other characters escaped as `\u{…}`, quotes and
    /// backslashes with a backslash): `complete`, or
    /// `sound-only; reasons="ql: …; ql: …"`, with `; lower=…; upper=…; unresolved=…` where
    /// bounds are known.
    pub fn header(&self) -> String {
        let mut out = self.as_str().to_owned();
        if let Some(b) = self.bounds {
            out.push_str(&format!(
                "; lower={}; upper={}; unresolved={}",
                b.lower, b.upper, b.unresolved
            ));
        }
        if self.reasons.is_empty() {
            return out;
        }
        let mut text: Vec<String> = self
            .reasons
            .iter()
            .take(SHOWN)
            .map(ToString::to_string)
            .collect();
        if self.reasons.len() > SHOWN {
            text.push(format!("and {} more", self.reasons.len() - SHOWN));
        }
        out.push_str("; reasons=\"");
        for c in text.join("; ").chars() {
            match c {
                '"' | '\\' => {
                    out.push('\\');
                    out.push(c);
                }
                c if c.is_ascii_graphic() || c == ' ' => out.push(c),
                c => out.extend(c.escape_unicode()),
            }
        }
        out.push('"');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_say_the_status_and_its_reasons_in_ascii() {
        let mut c = Completeness::default();
        assert_eq!(c.header(), "complete");
        c.incomplete("ql", "<http://e/partOf> is transitive".to_owned());
        c.incomplete("ql", "<http://e/partOf> is transitive".to_owned());
        c.incomplete("ql", "a \"quoted\" café".to_owned());
        assert_eq!(
            c.header(),
            "sound-only; reasons=\"ql: <http://e/partOf> is transitive; ql: a \\\"quoted\\\" caf\\u{e9}\""
        );
        let mut dl = Completeness {
            bounds: Some(Bounds {
                lower: 10,
                upper: 12,
                unresolved: 2,
            }),
            ..Completeness::default()
        };
        dl.incomplete("dl", "2 candidates unresolved".to_owned());
        c.merge(dl);
        assert!(
            c.header()
                .starts_with("sound-only; lower=10; upper=12; unresolved=2; reasons=\"")
        );
        assert_eq!(c.reasons.len(), 3);
    }
}
