use std::fmt;
use std::str::FromStr;

use crate::{Boolean, Decimal, Double, Float, ParseError, RangeError};

/// `xsd:integer`, 64-bit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Integer(i64);

impl Integer {
    pub const MAX: Self = Self(i64::MAX);
    pub const MIN: Self = Self(i64::MIN);

    /// `op:numeric-add`.
    pub fn checked_add(self, rhs: impl Into<Self>) -> Option<Self> {
        self.0.checked_add(rhs.into().0).map(Self)
    }

    /// `op:numeric-subtract`.
    pub fn checked_sub(self, rhs: impl Into<Self>) -> Option<Self> {
        self.0.checked_sub(rhs.into().0).map(Self)
    }

    /// `op:numeric-multiply`.
    pub fn checked_mul(self, rhs: impl Into<Self>) -> Option<Self> {
        self.0.checked_mul(rhs.into().0).map(Self)
    }

    /// `op:numeric-integer-divide`: the quotient truncated towards zero. (SPARQL's `/`
    /// on integers is decimal division: `Decimal::checked_div`.)
    pub fn checked_div(self, rhs: impl Into<Self>) -> Option<Self> {
        self.0.checked_div(rhs.into().0).map(Self)
    }

    /// `op:numeric-mod`: the remainder with the dividend's sign.
    pub fn checked_rem(self, rhs: impl Into<Self>) -> Option<Self> {
        self.0.checked_rem(rhs.into().0).map(Self)
    }

    /// The remainder in `0..|rhs|`.
    pub fn checked_rem_euclid(self, rhs: impl Into<Self>) -> Option<Self> {
        self.0.checked_rem_euclid(rhs.into().0).map(Self)
    }

    /// `op:numeric-unary-minus`.
    pub fn checked_neg(self) -> Option<Self> {
        self.0.checked_neg().map(Self)
    }

    /// `fn:abs`.
    pub fn checked_abs(self) -> Option<Self> {
        self.0.checked_abs().map(Self)
    }

    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    pub fn is_identical_with(self, other: Self) -> bool {
        self == other
    }

    pub const fn to_be_bytes(self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    pub const fn from_be_bytes(bytes: [u8; 8]) -> Self {
        Self(i64::from_be_bytes(bytes))
    }
}

macro_rules! from_primitive {
    ($($t:ty),*) => {$(
        impl From<$t> for Integer {
            fn from(value: $t) -> Self {
                Self(i64::from(value))
            }
        }
    )*};
}
from_primitive!(bool, i8, i16, i32, i64, u8, u16, u32);

impl TryFrom<u64> for Integer {
    type Error = RangeError;

    fn try_from(value: u64) -> Result<Self, RangeError> {
        i64::try_from(value)
            .map(Self)
            .map_err(|_| RangeError("integer"))
    }
}

impl TryFrom<i128> for Integer {
    type Error = RangeError;

    fn try_from(value: i128) -> Result<Self, RangeError> {
        i64::try_from(value)
            .map(Self)
            .map_err(|_| RangeError("integer"))
    }
}

impl From<Integer> for i64 {
    fn from(value: Integer) -> Self {
        value.0
    }
}

impl From<Integer> for i128 {
    fn from(value: Integer) -> Self {
        value.0.into()
    }
}

impl From<Boolean> for Integer {
    fn from(value: Boolean) -> Self {
        Self(bool::from(value).into())
    }
}

/// XPath cast: truncation towards zero.
impl TryFrom<Decimal> for Integer {
    type Error = RangeError;

    fn try_from(value: Decimal) -> Result<Self, RangeError> {
        Self::try_from(value.trunc_i128())
    }
}

/// XPath cast: truncation towards zero; NaN and the infinities fail.
impl TryFrom<Double> for Integer {
    type Error = RangeError;

    #[expect(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    fn try_from(value: Double) -> Result<Self, RangeError> {
        let v = f64::from(value).trunc();
        // i64::MAX as f64 rounds up to 2^63, which is out of range.
        if v.is_finite() && v >= i64::MIN as f64 && v < i64::MAX as f64 {
            Ok(Self(v as i64))
        } else {
            Err(RangeError("integer"))
        }
    }
}

impl TryFrom<Float> for Integer {
    type Error = RangeError;

    fn try_from(value: Float) -> Result<Self, RangeError> {
        Double::from(value).try_into()
    }
}

/// `[+-]?[0-9]+`.
impl FromStr for Integer {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, ParseError> {
        const E: &str = "integer";
        let bytes = input.as_bytes();
        let (negative, digits) = match bytes.first() {
            Some(b'-') => (true, &bytes[1..]),
            Some(b'+') => (false, &bytes[1..]),
            _ => (false, bytes),
        };
        if digits.is_empty() {
            return Err(ParseError::new(E, "no digits"));
        }
        let mut value: i64 = 0;
        for &b in digits {
            if !b.is_ascii_digit() {
                return Err(ParseError::new(E, "a character other than a digit"));
            }
            let digit = i64::from(b - b'0');
            // Accumulate negatively so that i64::MIN parses.
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_sub(digit))
                .ok_or(ParseError::new(E, "too large for 64 bits"))?;
        }
        if negative {
            Ok(Self(value))
        } else {
            value
                .checked_neg()
                .map(Self)
                .ok_or(ParseError::new(E, "too large for 64 bits"))
        }
    }
}

impl fmt::Display for Integer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_forms() {
        for (text, value) in [
            ("0", 0),
            ("+12", 12),
            ("-007", -7),
            ("9223372036854775807", i64::MAX),
        ] {
            assert_eq!(text.parse::<Integer>(), Ok(Integer(value)), "{text}");
        }
        assert_eq!("-9223372036854775808".parse::<Integer>(), Ok(Integer::MIN));
        for bad in [
            "",
            "+",
            "-",
            "1.0",
            " 1",
            "1e3",
            "9223372036854775808",
            "0x1",
        ] {
            assert!(bad.parse::<Integer>().is_err(), "{bad}");
        }
        assert_eq!(Integer(-5).to_string(), "-5");
    }

    #[test]
    fn operations_and_casts() {
        assert_eq!(Integer(7).checked_div(2), Some(Integer(3)));
        assert_eq!(Integer(-7).checked_div(2), Some(Integer(-3)));
        assert_eq!(Integer(-7).checked_rem(2), Some(Integer(-1)));
        assert_eq!(Integer(1).checked_div(0), None);
        assert_eq!(Integer::MAX.checked_add(1), None);
        assert_eq!(Integer::MIN.checked_abs(), None);
        assert_eq!(Integer::try_from(Double::from(-2.9)), Ok(Integer(-2)));
        assert!(Integer::try_from(Double::from(f64::NAN)).is_err());
        assert!(Integer::try_from(Double::from(9.3e18)).is_err());
        assert_eq!(
            Integer::try_from("-12.75".parse::<Decimal>().unwrap()),
            Ok(Integer(-12))
        );
    }
}
