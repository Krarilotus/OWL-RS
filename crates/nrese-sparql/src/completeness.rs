//! Whether a query's answers are sound and complete, and why not: one status for every
//! reasoning path that can fall short (docs/design/ql-rewriting.md §7; the DL bounds'
//! status in docs/design/owl2-dl.md, "Every answer carries its status"). The OWL 2 QL
//! rewriting reports `sound` and, where an axiom it doesn't follow meets anonymous
//! individuals, not `complete`; the DL bounds add their counts. Sources add reasons;
//! answers are never changed for them.

use std::fmt;

use crate::native::cache_key::Encoder;

/// A query's completeness. The default: sound and complete, nothing to say.
#[derive(Clone, PartialEq, Eq)]
pub struct Completeness {
    /// Every answer is certain.
    pub sound: bool,
    /// Every certain answer is there.
    pub complete: bool,
    /// Neither guarantee is asserted ([`Self::not_certified`]): no assessment of the
    /// answers, not a finding that they are wrong.
    pub certification_withheld: bool,
    /// Why not, each from its source.
    pub reasons: Vec<Reason>,
    /// The bounds' counts, where a source knows them.
    pub bounds: Option<Bounds>,
    /// What `complete` refers to: the semantics whose certain answers the answers are
    /// measured against, set by whoever produces the status.
    pub regime: Option<Regime>,
    /// Set where a procedure outside the regime's profile gave the answers.
    pub outside: Option<Outside>,
}

/// Answers a procedure gave on an input outside the regime's profile, and what they
/// guarantee (docs/design/owl2-dl.md §16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outside {
    /// What in the input is outside the profile: `an irregular role hierarchy`.
    pub what: Vec<&'static str>,
    /// The procedure that answered: `el`.
    pub by: &'static str,
    pub guaranteed: &'static str,
    pub not_guaranteed: &'static str,
}

// For logs; the store's output cache keys on [`Completeness::write_cache_context`].
impl fmt::Debug for Completeness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("Completeness");
        out.field("sound", &self.sound)
            .field("complete", &self.complete)
            .field("reasons", &self.reasons)
            .field("bounds", &self.bounds)
            .field("regime", &self.regime);
        if self.certification_withheld {
            out.field("certification_withheld", &true);
        }
        if let Some(outside) = &self.outside {
            out.field("outside", outside);
        }
        out.finish()
    }
}

/// The semantics a status is relative to. `complete` under one is no claim under another.
/// Under `owl2-ql` or `owl2-rl` the closure's rules and the QL rewriting answer, and an
/// axiom outside the profile that the answers can meet is reported: answered or reported,
/// a `complete` answer misses no certain answer under the OWL 2 Direct Semantics
/// (I24c; decided on 8 Nov).
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
            certification_withheld: false,
            reasons: Vec::new(),
            bounds: None,
            regime: None,
            outside: None,
        }
    }
}

/// The reasons a header shows; the rest are counted.
const SHOWN: usize = 5;

impl Completeness {
    /// No guarantee and no detail: what a reader is told whose status would be read from
    /// graphs it can't read. It absorbs whatever it is merged with.
    pub fn not_certified() -> Self {
        Self {
            sound: false,
            complete: false,
            certification_withheld: true,
            reasons: Vec::new(),
            bounds: None,
            regime: None,
            outside: None,
        }
    }

    /// Sound and complete under `regime`, nothing more to say.
    pub fn under(regime: Regime) -> Self {
        Self {
            regime: Some(regime),
            ..Self::default()
        }
    }

    /// Writes every field to `out` with the result cache's own key writers
    /// (`native::cache_key::Encoder`): a byte per bool and tag, a count before a list, a
    /// presence byte before an option, each text as its length and its bytes. Injective, so
    /// two statuses share bytes exactly when they are equal.
    pub fn write_cache_context(&self, out: &mut Vec<u8>) {
        let Self {
            sound,
            complete,
            certification_withheld,
            reasons,
            bounds,
            regime,
            outside,
        } = self;
        let mut e = Encoder::default();
        e.bytes = std::mem::take(out);
        for flag in [*sound, *complete, *certification_withheld] {
            e.tag(u8::from(flag));
        }
        e.number(reasons.len() as u64);
        for Reason { source, text: said } in reasons {
            e.text(source);
            e.text(said);
        }
        e.tag(u8::from(bounds.is_some()));
        if let Some(Bounds {
            lower,
            upper,
            unresolved,
        }) = bounds
        {
            for n in [*lower, *upper, *unresolved] {
                e.number(n);
            }
        }
        e.tag(match regime {
            None => 0,
            Some(Regime::Owl2Ql) => 1,
            Some(Regime::Owl2Rl) => 2,
            Some(Regime::Owl2Dl) => 3,
            Some(Regime::Rdfs) => 4,
            Some(Regime::Custom) => 5,
        });
        e.tag(u8::from(outside.is_some()));
        if let Some(Outside {
            what,
            by,
            guaranteed,
            not_guaranteed,
        }) = outside
        {
            e.number(what.len() as u64);
            for t in what.iter().chain([by, guaranteed, not_guaranteed]) {
                e.text(t);
            }
        }
        *out = e.bytes;
    }

    /// `complete`, `sound-only`, `unsound` or `not-certified`.
    pub fn as_str(&self) -> &'static str {
        if self.certification_withheld {
            return "not-certified";
        }
        match (self.sound, self.complete) {
            (true, true) => "complete",
            (true, false) => "sound-only",
            (false, _) => "unsound",
        }
    }

    /// Notes that some certain answers may be missing, and why (each reason once).
    pub fn incomplete(&mut self, source: &'static str, text: String) {
        if self.certification_withheld {
            *self = Self::not_certified();
            return;
        }
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
        if self.certification_withheld || other.certification_withheld {
            *self = Self::not_certified();
            return;
        }
        self.sound &= other.sound;
        self.complete &= other.complete;
        for r in other.reasons {
            if !self.reasons.contains(&r) {
                self.reasons.push(r);
            }
        }
        self.bounds = self.bounds.or(other.bounds);
        self.regime = self.regime.or(other.regime);
        self.outside = self.outside.take().or(other.outside);
    }

    /// The value of the `NRESE-Completeness` header: the status, its regime, what is
    /// outside its profile and by which procedure, then the bounds and the first reasons,
    /// ASCII only (other characters escaped as `\u{…}`, quotes and backslashes with a
    /// backslash): `complete; regime=owl2-ql`, or
    /// `sound-only; regime=owl2-rl; reasons="ql: …; ql: …"`, with
    /// `; outside="…"; by=…` where a procedure outside the profile answered and
    /// `; lower=…; upper=…; unresolved=…` where bounds are known.
    pub fn header(&self) -> String {
        if self.certification_withheld {
            return "not-certified".to_owned();
        }
        let mut out = self.as_str().to_owned();
        if let Some(regime) = self.regime {
            out.push_str("; regime=");
            out.push_str(regime.as_str());
        }
        if let Some(outside) = &self.outside {
            out.push_str("; outside=");
            quoted(&mut out, &outside.what.join(", "));
            out.push_str("; by=");
            out.push_str(outside.by);
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
        out.push_str("; reasons=");
        quoted(&mut out, &text.join("; "));
        out
    }
}

/// `text` in quotes, ASCII only (as [`Completeness::header`] says).
fn quoted(out: &mut String, text: &str) {
    out.push('"');
    // Runs of plain ASCII are copied at once; the character after each is escaped.
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if (c.is_ascii_graphic() || c == ' ') && c != '"' && c != '\\' {
            continue;
        }
        out.push_str(&text[start..i]);
        start = i + c.len_utf8();
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c => out.extend(c.escape_unicode()),
        }
    }
    out.push_str(&text[start..]);
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The result cache tells statuses apart by [`Completeness::write_cache_context`]: a
    /// change to any one field, or to one reason's text alone, gives other bytes. The
    /// status is built field by field, so a field added later fails to compile here until
    /// it has its own change below.
    #[test]
    fn every_field_of_a_status_changes_its_cache_context() {
        let reason = |text: &str| Reason {
            source: "ql",
            text: text.to_owned(),
        };
        let base = Completeness {
            sound: true,
            complete: false,
            certification_withheld: false,
            reasons: vec![reason("a"), reason("b")],
            bounds: Some(Bounds {
                lower: 1,
                upper: 2,
                unresolved: 1,
            }),
            regime: Some(Regime::Owl2Ql),
            outside: Some(Outside {
                what: vec!["x"],
                by: "el",
                guaranteed: "g",
                not_guaranteed: "n",
            }),
        };
        let changed: [fn(&mut Completeness); 19] = [
            |c| c.sound = false,
            |c| c.complete = true,
            |c| c.certification_withheld = true,
            |c| c.reasons[1].text = "c".to_owned(),
            |c| c.reasons[0].source = "dl",
            |c| c.reasons.swap(0, 1),
            |c| {
                c.reasons.pop();
            },
            |c| c.bounds = None,
            |c| c.bounds.as_mut().unwrap().lower = 0,
            |c| c.bounds.as_mut().unwrap().upper = 3,
            |c| c.bounds.as_mut().unwrap().unresolved = 0,
            |c| c.regime = Some(Regime::Owl2Rl),
            |c| c.regime = None,
            |c| c.outside = None,
            |c| c.outside.as_mut().unwrap().what.push("y"),
            |c| c.outside.as_mut().unwrap().what[0] = "z",
            |c| c.outside.as_mut().unwrap().by = "dl",
            |c| c.outside.as_mut().unwrap().guaranteed = "h",
            |c| c.outside.as_mut().unwrap().not_guaranteed = "m",
        ];
        let bytes = |c: &Completeness| {
            let mut out = Vec::new();
            c.write_cache_context(&mut out);
            out
        };
        let mut seen = vec![bytes(&base)];
        for (i, change) in changed.iter().enumerate() {
            let mut c = base.clone();
            change(&mut c);
            let b = bytes(&c);
            assert!(!seen.contains(&b), "change {i} gives bytes already seen");
            seen.push(b);
        }
        // Equal statuses, equal bytes.
        assert_eq!(bytes(&base), bytes(&base.clone()));
    }

    /// The header's quoting writes, for any text, what quoting it one character at a time
    /// writes (`by_char`, the reference): over a fixed seed's strings that mix plain runs with
    /// every character it escapes, at the start, at the end and back to back, non-ASCII ones,
    /// and the empty string.
    #[test]
    fn quoting_writes_what_quoting_by_character_writes() {
        fn by_char(out: &mut String, text: &str) {
            out.push('"');
            for c in text.chars() {
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
        }
        let pieces = [
            "",
            "a",
            "plain run, of: <text>",
            " ",
            "~",
            "\"",
            "\\",
            "\t",
            "\n",
            "\r",
            "\0",
            "\u{1f}",
            "\u{7f}",
            "é",
            "→",
            "😀",
            "ql: ",
            "\"\\\"",
        ];
        let mut texts: Vec<String> = pieces.iter().map(|p| (*p).to_owned()).collect();
        for p in &pieces[4..] {
            texts.push(format!("{p}a{p}"));
            texts.push(format!("a{p}{p}a"));
        }
        let mut seed: u64 = 0x5eed_1116;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..2000 {
            let n = next() % 9;
            texts.push(
                (0..n)
                    .map(|_| pieces[(next() % pieces.len() as u64) as usize])
                    .collect(),
            );
        }
        for text in &texts {
            let (mut written, mut reference) = (String::new(), String::new());
            quoted(&mut written, text);
            by_char(&mut reference, text);
            assert_eq!(written, reference, "{text:?}");
        }
    }

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
        // Withholding is an absorbing absence of guarantees, never an assessment of
        // unsoundness or a container for another source's private detail.
        let withheld = Completeness::not_certified();
        assert!(!withheld.sound && !withheld.complete);
        assert_eq!(withheld.header(), "not-certified");
        let mut left = c.clone();
        left.merge(withheld.clone());
        let mut right = withheld.clone();
        right.merge(c);
        assert_eq!(left, withheld);
        assert_eq!(right, withheld);
        right.unsound("transaction", "private detail".to_owned());
        assert_eq!(right, withheld);
    }

    /// An answer inside the profile has the header and the Debug form it had before the
    /// `outside` record (the output cache keys on the Debug form); one a procedure outside
    /// the profile gave names what is outside and the procedure, before the reasons.
    #[test]
    fn an_answer_inside_the_profile_prints_as_before_and_one_outside_says_so() {
        let inside = Completeness::under(Regime::Owl2Dl);
        assert_eq!(inside.header(), "complete; regime=owl2-dl");
        assert_eq!(
            format!("{inside:?}"),
            "Completeness { sound: true, complete: true, reasons: [], bounds: None, regime: Some(Owl2Dl) }"
        );
        let mut outside = Completeness {
            outside: Some(Outside {
                what: vec!["an irregular role hierarchy"],
                by: "el",
                guaranteed: "",
                not_guaranteed: "",
            }),
            ..inside
        };
        assert_eq!(
            outside.header(),
            "complete; regime=owl2-dl; outside=\"an irregular role hierarchy\"; by=el"
        );
        outside.incomplete("dl", "a range".to_owned());
        assert_eq!(
            outside.header(),
            "sound-only; regime=owl2-dl; outside=\"an irregular role hierarchy\"; by=el; reasons=\"dl: a range\""
        );
    }
}
