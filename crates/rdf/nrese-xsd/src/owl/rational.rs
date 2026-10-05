//! Exact rational numbers: the values of `owl:rational`, `xsd:decimal`, `xsd:integer` and
//! the integer types in the OWL 2 datatype map, compared exactly across them (OWL 2
//! Structural Specification §4.1).
//!
//! A numerator and a positive denominator of 128 bits each, reduced. Lexical forms whose
//! value doesn't fit are a [`RangeError`], never rounded: the datatype theory must not
//! confuse two values (`Decimal` truncates past 18 fractional digits, which is right for
//! SPARQL's arithmetic but not for identity).

use std::cmp::Ordering;
use std::fmt;

use crate::{ParseError, RangeError};

/// A rational number `numerator / denominator`, reduced, the denominator positive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rational {
    num: i128,
    den: i128,
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// The full 256-bit product of two magnitudes, as (high, low).
fn wide_mul(a: u128, b: u128) -> (u128, u128) {
    const MASK: u128 = u64::MAX as u128;
    let (a1, a0) = (a >> 64, a & MASK);
    let (b1, b0) = (b >> 64, b & MASK);
    let (ll, lh, hl, hh) = (a0 * b0, a0 * b1, a1 * b0, a1 * b1);
    let middle = (ll >> 64) + (lh & MASK) + (hl & MASK);
    let low = (ll & MASK) | (middle << 64);
    let high = hh + (lh >> 64) + (hl >> 64) + (middle >> 64);
    (high, low)
}

impl Rational {
    pub const ZERO: Self = Self { num: 0, den: 1 };
    pub const ONE: Self = Self { num: 1, den: 1 };

    /// `num / den`, reduced; `None` for a zero denominator, or `i128::MIN` anywhere (its
    /// negation doesn't fit).
    pub fn new(num: i128, den: i128) -> Option<Self> {
        if den == 0 || num == i128::MIN || den == i128::MIN {
            return None;
        }
        let (num, den) = if den < 0 { (-num, -den) } else { (num, den) };
        let g = gcd(num.unsigned_abs(), den.unsigned_abs()) as i128;
        Some(Self {
            num: num / g,
            den: den / g,
        })
    }

    /// The integer `i` (`None` for `i128::MIN`).
    pub fn integer(i: i128) -> Option<Self> {
        Self::new(i, 1)
    }

    pub fn numerator(self) -> i128 {
        self.num
    }

    pub fn denominator(self) -> i128 {
        self.den
    }

    pub fn is_integer(self) -> bool {
        self.den == 1
    }

    /// Whether it has a finite decimal expansion (an `xsd:decimal` value): the reduced
    /// denominator has no prime factors but 2 and 5.
    pub fn is_decimal(self) -> bool {
        let mut d = self.den;
        while d % 2 == 0 {
            d /= 2;
        }
        while d % 5 == 0 {
            d /= 5;
        }
        d == 1
    }

    /// The largest integer not above it.
    pub fn floor(self) -> i128 {
        self.num.div_euclid(self.den)
    }

    /// The smallest integer not below it.
    pub fn ceil(self) -> i128 {
        let f = self.floor();
        if self.num.rem_euclid(self.den) == 0 {
            f
        } else {
            f + 1
        }
    }

    /// `[+-]?[0-9]+` (`xsd:integer` and its subtypes).
    pub fn parse_integer(input: &str) -> Result<Self, NumberError> {
        let (negative, digits) = sign(input);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(NumberError::Lexical(ParseError::new(
                "integer",
                "not [+-]?[0-9]+",
            )));
        }
        let magnitude = accumulate(digits.trim_start_matches('0').bytes())?;
        Self::signed(magnitude, 1, negative)
    }

    /// `[+-]?([0-9]+(\.[0-9]*)?|\.[0-9]+)` (`xsd:decimal`).
    pub fn parse_decimal(input: &str) -> Result<Self, NumberError> {
        let invalid = || NumberError::Lexical(ParseError::new("decimal", "not a decimal numeral"));
        let (negative, rest) = sign(input);
        let (whole, fraction) = rest.split_once('.').unwrap_or((rest, ""));
        if (whole.is_empty() && fraction.is_empty())
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid());
        }
        let fraction = fraction.trim_end_matches('0');
        let digits = whole
            .trim_start_matches('0')
            .bytes()
            .chain(fraction.bytes());
        let magnitude = accumulate(digits)?;
        let exponent = u32::try_from(fraction.len()).map_err(|_| NumberError::Range)?;
        let den = 10_i128.checked_pow(exponent).ok_or(NumberError::Range)?;
        Self::signed(magnitude, den, negative)
    }

    /// `[+-]?[0-9]+ '/' [0-9]+` with a nonzero denominator (`owl:rational`).
    pub fn parse_rational(input: &str) -> Result<Self, NumberError> {
        let invalid =
            || NumberError::Lexical(ParseError::new("rational", "not numerator/denominator"));
        let (numerator, denominator) = input.split_once('/').ok_or_else(invalid)?;
        let n = Self::parse_integer(numerator).map_err(|e| match e {
            NumberError::Range => NumberError::Range,
            NumberError::Lexical(_) => invalid(),
        })?;
        if denominator.is_empty() || !denominator.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        let d = accumulate(denominator.trim_start_matches('0').bytes())?;
        if d == 0 {
            return Err(invalid());
        }
        let d = i128::try_from(d).map_err(|_| NumberError::Range)?;
        Self::new(n.num, d).ok_or(NumberError::Range)
    }

    fn signed(magnitude: u128, den: i128, negative: bool) -> Result<Self, NumberError> {
        let m = i128::try_from(magnitude).map_err(|_| NumberError::Range)?;
        Self::new(if negative { -m } else { m }, den).ok_or(NumberError::Range)
    }
}

/// Why a numeral has no `Rational`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberError {
    /// Not a lexical form of the datatype.
    Lexical(ParseError),
    /// A lexical form whose value is beyond 128 bits (numerator or denominator).
    Range,
}

impl From<RangeError> for NumberError {
    fn from(_: RangeError) -> Self {
        Self::Range
    }
}

fn sign(input: &str) -> (bool, &str) {
    match input.as_bytes().first() {
        Some(b'-') => (true, &input[1..]),
        Some(b'+') => (false, &input[1..]),
        _ => (false, input),
    }
}

fn accumulate(digits: impl Iterator<Item = u8>) -> Result<u128, NumberError> {
    let mut m: u128 = 0;
    for b in digits {
        m = m
            .checked_mul(10)
            .and_then(|m| m.checked_add(u128::from(b - b'0')))
            .ok_or(NumberError::Range)?;
    }
    Ok(m)
}

impl Ord for Rational {
    fn cmp(&self, other: &Self) -> Ordering {
        let (a, b) = (self.num, other.num);
        let sa = a.signum();
        let sb = b.signum();
        if sa != sb {
            return sa.cmp(&sb);
        }
        // Same sign: compare |a|·d₂ with |b|·d₁, reversed for negatives.
        let left = wide_mul(a.unsigned_abs(), other.den.unsigned_abs());
        let right = wide_mul(b.unsigned_abs(), self.den.unsigned_abs());
        let magnitude = left.cmp(&right);
        if sa < 0 {
            magnitude.reverse()
        } else {
            magnitude
        }
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(n: i128, d: i128) -> Rational {
        Rational::new(n, d).unwrap()
    }

    #[test]
    fn lexical_forms() {
        assert_eq!(Rational::parse_integer("-007"), Ok(r(-7, 1)));
        assert_eq!(Rational::parse_decimal("1.50"), Ok(r(3, 2)));
        assert_eq!(Rational::parse_decimal("-.5"), Ok(r(-1, 2)));
        assert_eq!(Rational::parse_decimal("2."), Ok(r(2, 1)));
        assert_eq!(Rational::parse_rational("1/2"), Ok(r(1, 2)));
        assert_eq!(Rational::parse_rational("-4/6"), Ok(r(-2, 3)));
        assert!(Rational::parse_rational("1/0").is_err());
        assert!(Rational::parse_rational("1/-2").is_err());
        assert!(Rational::parse_decimal("1e3").is_err());
        assert!(Rational::parse_integer("1.0").is_err());
        assert!(Rational::parse_integer("").is_err());
        // Digits past 128 bits are an error, never rounded.
        assert_eq!(
            Rational::parse_decimal("1.0000000000000000000000000000000000000000001"),
            Err(NumberError::Range)
        );
        assert_eq!(
            Rational::parse_decimal("1.000000000000000000000000000000000000000000"),
            Ok(r(1, 1))
        );
        // The decimal 0.5 and the rational 1/2 are one value.
        assert_eq!(
            Rational::parse_decimal("0.5").unwrap(),
            Rational::parse_rational("1/2").unwrap()
        );
    }

    #[test]
    fn order_and_classes() {
        assert!(r(1, 3) < r(1, 2));
        assert!(r(-1, 2) < r(-1, 3));
        assert!(r(-1, 3) < r(0, 1));
        let big = r(i128::MAX, 3);
        let bigger = r(i128::MAX, 2);
        assert!(big < bigger);
        assert!(r(-i128::MAX, 2) < r(-i128::MAX, 3));
        assert_eq!(r(7, 2).floor(), 3);
        assert_eq!(r(7, 2).ceil(), 4);
        assert_eq!(r(-7, 2).floor(), -4);
        assert_eq!(r(-7, 2).ceil(), -3);
        assert_eq!(r(4, 1).ceil(), 4);
        assert!(r(3, 40).is_decimal());
        assert!(!r(1, 3).is_decimal());
        assert!(r(5, 1).is_integer());
    }
}
