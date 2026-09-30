use std::fmt::{self, Write};
use std::str::FromStr;

use crate::{Boolean, Double, Float, Integer, ParseError, RangeError};

/// Fractional digits.
const DIGITS: u32 = 18;
/// 10^DIGITS.
const SCALE: i128 = 1_000_000_000_000_000_000;

/// `xsd:decimal`: 128-bit fixed point with 18 fractional digits (the value times 10¹⁸).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Decimal(i128);

impl Decimal {
    pub const ZERO: Self = Self(0);
    pub const ONE: Self = Self(SCALE);
    pub const MAX: Self = Self(i128::MAX);
    pub const MIN: Self = Self(i128::MIN);
    /// The smallest positive value, 10⁻¹⁸.
    pub const STEP: Self = Self(1);

    /// `i × 10⁻ⁿ`; digits past the 18th fractional one are truncated.
    pub fn new(i: i128, n: u32) -> Result<Self, RangeError> {
        if n <= DIGITS {
            i.checked_mul(10_i128.pow(DIGITS - n))
                .map(Self)
                .ok_or(RangeError("decimal"))
        } else {
            let shift = n - DIGITS;
            Ok(Self(if shift >= 39 {
                0
            } else {
                i / 10_i128.pow(shift)
            }))
        }
    }

    /// `op:numeric-add`.
    pub fn checked_add(self, rhs: impl Into<Self>) -> Option<Self> {
        self.0.checked_add(rhs.into().0).map(Self)
    }

    /// `op:numeric-subtract`.
    pub fn checked_sub(self, rhs: impl Into<Self>) -> Option<Self> {
        self.0.checked_sub(rhs.into().0).map(Self)
    }

    /// `op:numeric-multiply`, exact up to truncation past the 18th fractional digit.
    pub fn checked_mul(self, rhs: impl Into<Self>) -> Option<Self> {
        let (a, b) = (self.0, rhs.into().0);
        if let Some(product) = a.checked_mul(b) {
            return Some(Self(product / SCALE));
        }
        let magnitude =
            wide::div_by_u64(wide::mul(a.unsigned_abs(), b.unsigned_abs()), SCALE as u64)?;
        signed(magnitude, (a < 0) != (b < 0)).map(Self)
    }

    /// `op:numeric-divide`, exact up to truncation past the 18th fractional digit; `None`
    /// for a zero divisor.
    pub fn checked_div(self, rhs: impl Into<Self>) -> Option<Self> {
        let (a, b) = (self.0, rhs.into().0);
        if b == 0 {
            return None;
        }
        if let Some(shifted) = a.checked_mul(SCALE) {
            return shifted.checked_div(b).map(Self);
        }
        let numerator = wide::mul(a.unsigned_abs(), SCALE as u128);
        let divisor = b.unsigned_abs();
        let magnitude = match u64::try_from(divisor) {
            Ok(small) => wide::div_by_u64(numerator, small)?,
            Err(_) => wide::div(numerator, divisor)?,
        };
        signed(magnitude, (a < 0) != (b < 0)).map(Self)
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

    /// `fn:round`: to the nearest integer, halves towards positive infinity.
    pub fn checked_round(self) -> Option<Self> {
        Self(self.0.checked_add(SCALE / 2)?).checked_floor()
    }

    /// `fn:ceiling`.
    pub fn checked_ceil(self) -> Option<Self> {
        Self(self.0.checked_neg()?).checked_floor()?.checked_neg()
    }

    /// `fn:floor`.
    pub fn checked_floor(self) -> Option<Self> {
        self.0.div_euclid(SCALE).checked_mul(SCALE).map(Self)
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

    /// The integer part (truncated towards zero).
    pub(crate) const fn trunc_i128(self) -> i128 {
        self.0 / SCALE
    }

    /// The value times 10¹⁸.
    pub(crate) const fn raw(self) -> i128 {
        self.0
    }

    pub(crate) const fn from_raw(raw: i128) -> Self {
        Self(raw)
    }

    pub const fn to_be_bytes(self) -> [u8; 16] {
        self.0.to_be_bytes()
    }

    pub const fn from_be_bytes(bytes: [u8; 16]) -> Self {
        Self(i128::from_be_bytes(bytes))
    }

    /// The XSD 1.1 canonical form, which for decimals is also `Display`'s.
    pub fn canonical(self) -> String {
        self.to_string()
    }

    /// The value as `mantissa × 10^-exponent` with no trailing zeros in the mantissa.
    fn reduced(self) -> (i128, u32) {
        let (mut mantissa, mut exponent) = (self.0, DIGITS);
        while exponent > 0 && mantissa % 10 == 0 {
            mantissa /= 10;
            exponent -= 1;
        }
        (mantissa, exponent)
    }
}

/// `magnitude` with a sign, if it fits.
fn signed(magnitude: u128, negative: bool) -> Option<i128> {
    if negative {
        0_i128.checked_sub_unsigned(magnitude)
    } else {
        i128::try_from(magnitude).ok()
    }
}

/// 256-bit unsigned arithmetic, as (high, low) halves.
mod wide {
    const MASK: u128 = u64::MAX as u128;

    /// The full product.
    pub fn mul(a: u128, b: u128) -> (u128, u128) {
        let (a1, a0) = (a >> 64, a & MASK);
        let (b1, b0) = (b >> 64, b & MASK);
        let (ll, lh, hl, hh) = (a0 * b0, a0 * b1, a1 * b0, a1 * b1);
        let middle = (ll >> 64) + (lh & MASK) + (hl & MASK);
        let low = (ll & MASK) | (middle << 64);
        let high = hh + (lh >> 64) + (hl >> 64) + (middle >> 64);
        (high, low)
    }

    /// The quotient by a 64-bit divisor, if it fits in 128 bits: four 128/64 steps.
    pub fn div_by_u64((high, low): (u128, u128), divisor: u64) -> Option<u128> {
        let d = u128::from(divisor);
        let limbs = [(high >> 64), high & MASK, low >> 64, low & MASK];
        let mut remainder = 0_u128;
        let mut quotient = [0_u128; 4];
        for (i, limb) in limbs.into_iter().enumerate() {
            let current = (remainder << 64) | limb;
            quotient[i] = current / d;
            remainder = current % d;
        }
        if quotient[0] != 0 || quotient[1] != 0 {
            return None;
        }
        Some((quotient[2] << 64) | quotient[3])
    }

    /// The quotient by a 128-bit divisor, if it fits in 128 bits: shift and subtract.
    pub fn div((high, low): (u128, u128), divisor: u128) -> Option<u128> {
        if high >= divisor {
            return None;
        }
        let mut remainder = high;
        let mut quotient = 0_u128;
        for i in (0..128).rev() {
            let carry = remainder >> 127;
            remainder = (remainder << 1) | ((low >> i) & 1);
            quotient <<= 1;
            if carry == 1 || remainder >= divisor {
                remainder = remainder.wrapping_sub(divisor);
                quotient |= 1;
            }
        }
        Some(quotient)
    }
}

macro_rules! from_primitive {
    ($($t:ty),*) => {$(
        impl From<$t> for Decimal {
            fn from(value: $t) -> Self {
                Self(i128::from(value) * SCALE)
            }
        }
    )*};
}
from_primitive!(bool, i8, i16, i32, i64, u8, u16, u32, u64);

impl From<Integer> for Decimal {
    fn from(value: Integer) -> Self {
        i64::from(value).into()
    }
}

impl From<Boolean> for Decimal {
    fn from(value: Boolean) -> Self {
        bool::from(value).into()
    }
}

impl TryFrom<i128> for Decimal {
    type Error = RangeError;

    fn try_from(value: i128) -> Result<Self, RangeError> {
        value
            .checked_mul(SCALE)
            .map(Self)
            .ok_or(RangeError("decimal"))
    }
}

impl TryFrom<u128> for Decimal {
    type Error = RangeError;

    fn try_from(value: u128) -> Result<Self, RangeError> {
        i128::try_from(value)
            .map_err(|_| RangeError("decimal"))?
            .try_into()
    }
}

/// XPath cast: the decimal the shortest representation of the double denotes (so `0.1e0`
/// gives `0.1`), truncated past 18 fractional digits; NaN and the infinities fail.
impl TryFrom<Double> for Decimal {
    type Error = RangeError;

    fn try_from(value: Double) -> Result<Self, RangeError> {
        let v = f64::from(value);
        if !v.is_finite() {
            return Err(RangeError("decimal"));
        }
        format!("{v}").parse().map_err(|_| RangeError("decimal"))
    }
}

impl TryFrom<Float> for Decimal {
    type Error = RangeError;

    fn try_from(value: Float) -> Result<Self, RangeError> {
        let v = f32::from(value);
        if !v.is_finite() {
            return Err(RangeError("decimal"));
        }
        format!("{v}").parse().map_err(|_| RangeError("decimal"))
    }
}

/// Correctly rounded: exact operands and one IEEE division where that is exact enough
/// (Clinger's fast path), otherwise the decimal text through Rust's parser.
impl From<Decimal> for Double {
    #[allow(clippy::cast_precision_loss)]
    fn from(value: Decimal) -> Self {
        let (mantissa, exponent) = value.reduced();
        if mantissa.unsigned_abs() <= 1 << 53 && exponent <= 22 {
            return Double::from(mantissa as f64 / 10_f64.powi(exponent as i32));
        }
        Double::from(value.to_string().parse::<f64>().unwrap_or(f64::NAN))
    }
}

impl From<Decimal> for Float {
    #[allow(clippy::cast_precision_loss)]
    fn from(value: Decimal) -> Self {
        let (mantissa, exponent) = value.reduced();
        if mantissa.unsigned_abs() <= 1 << 24 && exponent <= 10 {
            return Float::from(mantissa as f32 / 10_f32.powi(exponent as i32));
        }
        Float::from(value.to_string().parse::<f32>().unwrap_or(f32::NAN))
    }
}

/// `[+-]?([0-9]+(\.[0-9]*)?|\.[0-9]+)`; digits past the 18th fractional one are
/// truncated.
impl FromStr for Decimal {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, ParseError> {
        const E: &str = "decimal";
        let bytes = input.as_bytes();
        let (negative, rest) = match bytes.first() {
            Some(b'-') => (true, &bytes[1..]),
            Some(b'+') => (false, &bytes[1..]),
            _ => (false, bytes),
        };
        let (whole, fraction) = match rest.iter().position(|&b| b == b'.') {
            Some(dot) => (&rest[..dot], &rest[dot + 1..]),
            None => (rest, &[][..]),
        };
        if whole.is_empty() && fraction.is_empty() {
            return Err(ParseError::new(E, "no digits"));
        }
        let too_large = ParseError::new(E, "too large");
        let mut magnitude: u128 = 0;
        for &b in whole {
            if !b.is_ascii_digit() {
                return Err(ParseError::new(E, "a character other than a digit or '.'"));
            }
            magnitude = magnitude
                .checked_mul(10)
                .and_then(|m| m.checked_add(u128::from(b - b'0')))
                .ok_or(too_large)?;
        }
        magnitude = magnitude.checked_mul(SCALE as u128).ok_or(too_large)?;
        let mut unit = SCALE as u128;
        for &b in fraction {
            if !b.is_ascii_digit() {
                return Err(ParseError::new(E, "a character other than a digit or '.'"));
            }
            unit /= 10;
            magnitude = magnitude
                .checked_add(u128::from(b - b'0') * unit)
                .ok_or(too_large)?;
        }
        signed(magnitude, negative).map(Self).ok_or(too_large)
    }
}

/// The XPath string form, which is also the XSD 1.1 canonical form: no fractional part
/// for an integer value, otherwise no trailing zeros (`1`, `-0.5`, `12.25`).
impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let magnitude = self.0.unsigned_abs();
        let whole = magnitude / SCALE as u128;
        let mut fraction = magnitude % SCALE as u128;
        let mut out = String::with_capacity(42);
        if self.0 < 0 {
            out.push('-');
        }
        write!(out, "{whole}")?;
        if fraction != 0 {
            let mut width = DIGITS as usize;
            while fraction.is_multiple_of(10) {
                fraction /= 10;
                width -= 1;
            }
            write!(out, ".{fraction:0width$}")?;
        }
        f.pad(&out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Decimal {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn lexical_forms() {
        for (text, shown) in [
            ("0", "0"),
            ("-0", "0"),
            ("+100000.00", "100000"),
            ("0.1220", "0.122"),
            (".122", "0.122"),
            ("1.", "1"),
            ("01.0", "1"),
            ("-1.23", "-1.23"),
            ("12678967.543233", "12678967.543233"),
            ("0.000000000000000001", "0.000000000000000001"),
            ("0.0000000000000000019", "0.000000000000000001"),
            ("0.100000000000000000000000000", "0.1"),
            (
                "170141183460469231731.687303715884105727",
                "170141183460469231731.687303715884105727",
            ),
            (
                "-170141183460469231731.687303715884105728",
                "-170141183460469231731.687303715884105728",
            ),
        ] {
            assert_eq!(d(text).to_string(), shown, "{text}");
        }
        assert_eq!(d(&Decimal::MAX.to_string()), Decimal::MAX);
        assert_eq!(d(&Decimal::MIN.to_string()), Decimal::MIN);
        for bad in [
            "",
            "+",
            "-",
            ".",
            "+.",
            "a",
            ".a",
            "1e3",
            "1.2.3",
            " 1",
            "INF",
            "1000000000000000000000",
        ] {
            assert!(bad.parse::<Decimal>().is_err(), "{bad}");
        }
    }

    #[test]
    fn arithmetic_is_exact_to_truncation() {
        assert_eq!(d("0").checked_mul(d("1.5")), Some(d("0")));
        assert_eq!(d("0").checked_div(d("1.5")), Some(d("0")));
        assert_eq!(d("1").checked_div(d("0")), None);
        assert_eq!(
            d("1").checked_div(d("3")).unwrap().to_string(),
            "0.333333333333333333"
        );
        assert_eq!(
            d("2").checked_div(d("3")).unwrap().to_string(),
            "0.666666666666666666"
        );
        assert_eq!(
            d("-2").checked_div(d("3")).unwrap().to_string(),
            "-0.666666666666666666"
        );
        assert_eq!(d("1.5").checked_mul(d("2.25")), Some(d("3.375")));
        // Beyond an i128 product: the 256-bit path.
        assert_eq!(
            d("123456789.123456789")
                .checked_mul(d("987654321.987654321"))
                .unwrap()
                .to_string(),
            "121932631356500531.347203169112635269"
        );
        assert_eq!(
            d("-99999999999.5").checked_mul(d("2")),
            Some(d("-199999999999"))
        );
        assert_eq!(d("100000000000000000000").checked_mul(d("2")), None);
        assert_eq!(
            d("0.000000001")
                .checked_mul(d("0.000000001"))
                .unwrap()
                .to_string(),
            "0.000000000000000001"
        );
        assert_eq!(
            d("0.000000001").checked_mul(d("0.0000000001")),
            Some(d("0"))
        );
        // Large dividends: the 64-bit and 128-bit divisor paths.
        assert_eq!(
            d("100000000000000000000")
                .checked_div(d("8"))
                .unwrap()
                .to_string(),
            "12500000000000000000"
        );
        assert_eq!(
            d("100000000000000000000")
                .checked_div(d("40000000000000000000"))
                .unwrap()
                .to_string(),
            "2.5"
        );
        assert_eq!(d("100000000000000000000").checked_div(d("0.5")), None);
        assert_eq!(Decimal::MAX.checked_add(Decimal::STEP), None);
        assert_eq!(d("7.5").checked_rem(d("2")), Some(d("1.5")));
        assert_eq!(d("-7.5").checked_rem(d("2")), Some(d("-1.5")));
    }

    #[test]
    fn rounding() {
        for (x, round, ceil, floor) in [
            ("2.5", "3", "3", "2"),
            ("2.4999", "2", "3", "2"),
            ("-2.5", "-2", "-2", "-3"),
            ("-2.6", "-3", "-2", "-3"),
            ("-0.5", "0", "0", "-1"),
            ("3", "3", "3", "3"),
        ] {
            assert_eq!(d(x).checked_round(), Some(d(round)), "round {x}");
            assert_eq!(d(x).checked_ceil(), Some(d(ceil)), "ceil {x}");
            assert_eq!(d(x).checked_floor(), Some(d(floor)), "floor {x}");
        }
        assert_eq!(Decimal::MAX.checked_round(), None);
    }

    #[test]
    fn conversions_round_correctly() {
        assert_eq!(f64::from(Double::from(d("0.1"))), 0.1);
        assert_eq!(f64::from(Double::from(d("-12.5"))), -12.5);
        assert_eq!(
            f64::from(Double::from(d("123456789012345678.9"))),
            123_456_789_012_345_678.9
        );
        assert_eq!(f32::from(Float::from(d("0.1"))), 0.1_f32);
        assert_eq!(f32::from(Float::from(d("3.4028234"))), 3.402_823_4_f32);
        assert_eq!(Decimal::try_from(Double::from(0.1)), Ok(d("0.1")));
        assert_eq!(
            Decimal::try_from(Double::from(-1.5e10)),
            Ok(d("-15000000000"))
        );
        assert_eq!(Decimal::try_from(Float::from(0.1_f32)), Ok(d("0.1")));
        assert!(Decimal::try_from(Double::from(f64::INFINITY)).is_err());
        assert!(Decimal::try_from(Double::from(1e300)).is_err());
        assert_eq!(Decimal::try_from(Double::from(1e-300)), Ok(d("0")));
        assert_eq!(Integer::try_from(d("-3.9")), Ok(Integer::from(-3)));
    }

    #[test]
    fn wide_division_matches_narrow() {
        let cases: [(u128, u128); 4] = [
            (1, 3),
            (u128::MAX, 7),
            (1 << 100, (1 << 70) + 12345),
            (98765, 98765),
        ];
        for (a, b) in cases {
            let product = wide::mul(a, SCALE as u128);
            let expected = a.checked_mul(SCALE as u128).map(|p| p / b);
            if let Some(expected) = expected {
                assert_eq!(wide::div(product, b), Some(expected));
                if let Ok(small) = u64::try_from(b) {
                    assert_eq!(wide::div_by_u64(product, small), Some(expected));
                }
            }
        }
        assert_eq!(wide::mul(u128::MAX, u128::MAX), (u128::MAX - 1, 1));
    }
}
