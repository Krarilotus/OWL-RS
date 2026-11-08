/// A text that isn't a lexical form of the datatype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a lexical form of xsd:{datatype}: {reason}")]
pub struct ParseError {
    pub(crate) datatype: &'static str,
    pub(crate) reason: &'static str,
}

impl ParseError {
    pub(crate) const fn new(datatype: &'static str, reason: &'static str) -> Self {
        Self { datatype, reason }
    }

    /// The local name of the datatype, such as `decimal`.
    pub fn datatype(&self) -> &'static str {
        self.datatype
    }

    /// A valid date-time lexical form, every part checked, whose year is beyond what is
    /// represented (XML Schema 1.1 Part 2 §3.3.7.2 puts no bound on a year); `zoned`: it has
    /// a timezone.
    pub(crate) const fn unrepresented_year(datatype: &'static str, zoned: bool) -> Self {
        Self::new(
            datatype,
            if zoned {
                UNREPRESENTED_ZONED
            } else {
                UNREPRESENTED_LOCAL
            },
        )
    }

    /// For [`Self::unrepresented_year`]'s errors, whether the lexical form has a timezone:
    /// valid, so never ill-typed for a datatype whose forms need none; `None` for any other.
    pub(crate) fn unrepresented(&self) -> Option<bool> {
        match self.reason {
            UNREPRESENTED_ZONED => Some(true),
            UNREPRESENTED_LOCAL => Some(false),
            _ => None,
        }
    }
}

const UNREPRESENTED_ZONED: &str = "a year beyond what is represented";
const UNREPRESENTED_LOCAL: &str = "a year beyond what is represented (no timezone)";

/// A value outside what the target type can hold (XPath `FOCA0001`, `FOCA0003`,
/// `FODT0001`), or not a number where one was needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("value out of range for xsd:{0}")]
pub struct RangeError(pub(crate) &'static str);
