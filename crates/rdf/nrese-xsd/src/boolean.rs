use std::fmt;
use std::str::FromStr;

use crate::{Decimal, Double, Float, Integer, ParseError};

/// `xsd:boolean`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Boolean(bool);

impl Boolean {
    pub fn is_identical_with(self, other: Self) -> bool {
        self == other
    }
}

impl From<bool> for Boolean {
    fn from(value: bool) -> Self {
        Self(value)
    }
}

impl From<Boolean> for bool {
    fn from(value: Boolean) -> Self {
        value.0
    }
}

/// XPath cast: zero (and NaN) is false, any other number true.
impl From<Integer> for Boolean {
    fn from(value: Integer) -> Self {
        Self(i64::from(value) != 0)
    }
}

impl From<Decimal> for Boolean {
    fn from(value: Decimal) -> Self {
        Self(value != Decimal::ZERO)
    }
}

impl From<Float> for Boolean {
    fn from(value: Float) -> Self {
        let v = f32::from(value);
        Self(!(v == 0. || v.is_nan()))
    }
}

impl From<Double> for Boolean {
    fn from(value: Double) -> Self {
        let v = f64::from(value);
        Self(!(v == 0. || v.is_nan()))
    }
}

/// `true`, `false`, `1`, `0`.
impl FromStr for Boolean {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, ParseError> {
        match input {
            "true" | "1" => Ok(Self(true)),
            "false" | "0" => Ok(Self(false)),
            _ => Err(ParseError::new("boolean", "expected true, false, 1 or 0")),
        }
    }
}

impl fmt::Display for Boolean {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0 { "true" } else { "false" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_forms_and_casts() {
        assert_eq!("1".parse::<Boolean>(), Ok(Boolean(true)));
        assert_eq!("false".parse::<Boolean>(), Ok(Boolean(false)));
        assert!("TRUE".parse::<Boolean>().is_err());
        assert!(" true".parse::<Boolean>().is_err());
        assert_eq!(Boolean(true).to_string(), "true");
        assert_eq!(Boolean::from(Double::from(f64::NAN)), Boolean(false));
        assert_eq!(Boolean::from(Float::from(-0.0f32)), Boolean(false));
        assert_eq!(Boolean::from(Integer::from(-3)), Boolean(true));
        assert_eq!(Boolean::from(Decimal::from(0)), Boolean(false));
    }
}
