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
    /// What `complete` refers to: the semantics whose certain answers the answers are
    /// measured against, set by whoever produces the status.
    pub regime: Option<Regime>,
}

/// The semantics a status is relative to. `complete` under one is no claim under another:
/// complete under `owl2-ql` or `owl2-rl` means every answer the closure's rules (and the
/// QL rewriting's existentials) entail, not every certain answer under OWL 2 DL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Regime {
    /// The OWL 2 QL closure, with the QL rewriting's existentials.
    Owl2Ql,
    /// The OWL 2 RL closure (with the QL rewriting's existentials where it runs).
    Owl2Rl,
    /// The OWL 2 Direct Semantics: certain answers under OWL 2 DL (the DL bounds).
    Owl2Dl,
    /// The RDFS closure.
    Rdfs,
    /// Another ruleset's closure (user rules, other profiles).
    Custom,
}

impl Regime {
    /// `owl2-ql`, `owl2-rl`, `owl2-dl`, `rdfs` or `custom`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owl2Ql => "owl2-ql",
            Self::Owl2Rl => "owl2-rl",
            Self::Owl2Dl => "owl2-dl",
            Self::Rdfs => "rdfs",
            Self::Custom => "custom",
        }
    }

    /// The regime of a ruleset's closure, by its name.
    pub fn of_ruleset(name: &str) -> Self {
        match name {
            "owl2-ql" => Self::Owl2Ql,
            "owl2-rl" => Self::Owl2Rl,
            "rdfs" => Self::Rdfs,
            _ => Self::Custom,
        }
    }
}

impl fmt::Display for Regime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
            regime: None,
        }
    }
}

/// The reasons a header shows; the rest are counted.
const SHOWN: usize = 5;

impl Completeness {
    /// Sound and complete under `regime`, nothing more to say.
    pub fn under(regime: Regime) -> Self {
        Self {
            regime: Some(regime),
            ..Self::default()
        }
    }

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

    /// Notes that some answers may not be certain, and why: neither sound nor complete.
    pub fn unsound(&mut self, source: &'static str, text: String) {
        self.sound = false;
        self.incomplete(source, text);
    }

    /// Both statuses at once: what either lacks, the other's reasons too; the regime is
    /// the first one set.
    pub fn merge(&mut self, other: Completeness) {
        self.sound &= other.sound;
        self.complete &= other.complete;
        for r in other.reasons {
            if !self.reasons.contains(&r) {
                self.reasons.push(r);
            }
        }
        self.bounds = self.bounds.or(other.bounds);
        self.regime = self.regime.or(other.regime);
    }

    /// The value of the `NRESE-Completeness` header: the status, its regime, then the
    /// bounds and the first reasons, ASCII only (other characters escaped as `\u{…}`,
    /// quotes and backslashes with a backslash): `complete; regime=owl2-ql`, or
    /// `sound-only; regime=owl2-rl; reasons="ql: …; ql: …"`, with
    /// `; lower=…; upper=…; unresolved=…` where bounds are known.
    pub fn header(&self) -> String {
        let mut out = self.as_str().to_owned();
        if let Some(regime) = self.regime {
            out.push_str("; regime=");
            out.push_str(regime.as_str());
        }
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
        let mut c = Completeness::under(Regime::Owl2Rl);
        assert_eq!(c.header(), "complete; regime=owl2-rl");
        c.incomplete("ql", "<http://e/partOf> is transitive".to_owned());
        c.incomplete("ql", "<http://e/partOf> is transitive".to_owned());
        c.incomplete("ql", "a \"quoted\" café".to_owned());
        assert_eq!(
            c.header(),
            "sound-only; regime=owl2-rl; reasons=\"ql: <http://e/partOf> is transitive; ql: a \\\"quoted\\\" caf\\u{e9}\""
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
        assert!(c.header().starts_with(
            "sound-only; regime=owl2-rl; lower=10; upper=12; unresolved=2; reasons=\""
        ));
        assert_eq!(c.reasons.len(), 3);
        // An answer that may not be certain says so.
        c.unsound(
            "transaction",
            "deleted statements' inferences remain".to_owned(),
        );
        assert!(c.header().starts_with("unsound; regime=owl2-rl;"));
    }
}
