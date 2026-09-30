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
}

/// A value outside what the target type can hold (XPath `FOCA0001`, `FOCA0003`,
/// `FODT0001`), or not a number where one was needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("value out of range for xsd:{0}")]
pub struct RangeError(pub(crate) &'static str);
