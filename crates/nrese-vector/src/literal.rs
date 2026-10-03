//! Vectors as RDF literals: `"[0.12, -0.5, 3e-2]"^^nrv:vector`, the numbers as in JSON
//! (an array of numbers: what embedding services return), separated by commas or white
//! space. The lexical form is canonical when written by [`lexical`]: shortest round-trip
//! decimal forms, `", "` between them.

/// The namespace of NRESE's vector vocabulary (`PREFIX nrv: <urn:nrese:vector:>`).
pub const NAMESPACE: &str = "urn:nrese:vector:";

/// The datatype of vector literals.
pub const DATATYPE: &str = "urn:nrese:vector:vector";

/// Why a lexical form isn't a vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// No brackets around it.
    Brackets,
    /// A part that isn't a finite number.
    Number(String),
    /// No numbers.
    Empty,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Brackets => write!(f, "a vector is written in brackets: [0.1, 0.2]"),
            Self::Number(part) => write!(f, "'{part}' is not a finite number"),
            Self::Empty => write!(f, "a vector has at least one number"),
        }
    }
}

impl std::error::Error for ParseError {}

/// The numbers of the lexical form `text`.
pub fn parse(text: &str) -> Result<Vec<f32>, ParseError> {
    let inner = text
        .trim()
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .ok_or(ParseError::Brackets)?;
    let mut values = Vec::new();
    for part in inner.split(|c: char| c == ',' || c.is_whitespace()) {
        if part.is_empty() {
            continue;
        }
        let value: f32 = part
            .parse()
            .map_err(|_| ParseError::Number(part.to_owned()))?;
        if !value.is_finite() {
            return Err(ParseError::Number(part.to_owned()));
        }
        values.push(value);
    }
    if values.is_empty() {
        return Err(ParseError::Empty);
    }
    Ok(values)
}

/// The canonical lexical form of `values`.
pub fn lexical(values: &[f32]) -> String {
    let parts: Vec<String> = values.iter().map(|v| format!("{v:?}")).collect();
    format!("[{}]", parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors_parse_and_print() {
        assert_eq!(parse("[1, -2.5, 3e-2]").unwrap(), vec![1.0, -2.5, 0.03]);
        assert_eq!(parse(" [ 1 2\n3 ] ").unwrap(), vec![1.0, 2.0, 3.0]);
        assert_eq!(parse("1, 2"), Err(ParseError::Brackets));
        assert_eq!(parse("[]"), Err(ParseError::Empty));
        assert!(matches!(parse("[1, NaN]"), Err(ParseError::Number(_))));
        assert!(matches!(parse("[1, x]"), Err(ParseError::Number(_))));
        let values = [0.1f32, -3.25, 1e-7];
        assert_eq!(parse(&lexical(&values)).unwrap(), values);
        assert_eq!(lexical(&[1.0, 0.5]), "[1.0, 0.5]");
    }
}
