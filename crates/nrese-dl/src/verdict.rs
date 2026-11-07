//! Whether an ontology has a model, as the DL engines decided it, or why they didn't: the
//! one verdict classification and the store's consistency check give. No default: a
//! producer states which of the three it found, so "not decided" can't read as
//! "consistent".

/// What a consistency check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Consistent,
    Inconsistent,
    /// Not decided: a budget ran out, the engines lack something the ontology needs, or
    /// the check is off. Answers can't be certain then.
    Unknown(String),
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Consistent => "consistent",
            Self::Inconsistent => "inconsistent",
            Self::Unknown(_) => "unknown",
        }
    }

    /// Why it isn't decided, if it isn't.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Unknown(why) => Some(why),
            _ => None,
        }
    }

    /// `Some(true)` or `Some(false)` where decided, `None` where not.
    pub fn decided(&self) -> Option<bool> {
        match self {
            Self::Consistent => Some(true),
            Self::Inconsistent => Some(false),
            Self::Unknown(_) => None,
        }
    }
}
