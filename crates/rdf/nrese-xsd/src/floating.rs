//! `xsd:float` and `xsd:double`: IEEE 754 binary32 and binary64, one implementation for
//! both.

use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, Div, Mul, Neg, Sub};
use std::str::FromStr;

use crate::{Boolean, Integer, ParseError};

/// Whether `text` is an XSD float or double lexical form:
/// `(\+|-)?([0-9]+(\.[0-9]*)?|\.[0-9]+)([Ee](\+|-)?[0-9]+)?|(\+|-)?INF|NaN`.
fn is_lexical_form(text: &[u8]) -> bool {
    if text == b"NaN" {
        return true;
    }
    let unsigned = match text.first() {
        Some(b'+' | b'-') => &text[1..],
        _ => text,
    };
    if unsigned == b"INF" {
        return true;
    }
    let digits = |s: &[u8]| s.iter().take_while(|b| b.is_ascii_digit()).count();
    let whole = digits(unsigned);
    let mut rest = &unsigned[whole..];
    let mut fraction = 0;
    if let Some(after) = rest.strip_prefix(b".") {
        fraction = digits(after);
        rest = &after[fraction..];
    }
    if whole + fraction == 0 {
        return false;
    }
    if let Some(after) = rest.strip_prefix(b"e").or_else(|| rest.strip_prefix(b"E")) {
        let after = match after.first() {
            Some(b'+' | b'-') => &after[1..],
            _ => after,
        };
        let exponent = digits(after);
        return exponent > 0 && exponent == after.len();
    }
    rest.is_empty()
}

/// Inserts ".0" into a mantissa without a point: `1E20` → `1.0E20`.
fn with_point(mut scientific: String) -> String {
    if let Some(e) = scientific.find('E')
        && !scientific[..e].contains('.')
    {
        scientific.insert_str(e, ".0");
    }
    scientific
}

macro_rules! floating {
    ($name:ident, $t:ty, $xsd:literal, exact: [$($exact:ty),*]) => {
        #[doc = concat!("`xsd:", $xsd, "`.")]
        #[derive(Debug, Clone, Copy, Default)]
        pub struct $name($t);

        impl $name {
            pub const MAX: Self = Self(<$t>::MAX);
            pub const MIN: Self = Self(<$t>::MIN);
            pub const INFINITY: Self = Self(<$t>::INFINITY);
            pub const NEG_INFINITY: Self = Self(<$t>::NEG_INFINITY);
            pub const NAN: Self = Self(<$t>::NAN);

            /// `fn:abs`.
            pub fn abs(self) -> Self {
                Self(self.0.abs())
            }

            /// `fn:ceiling`.
            pub fn ceil(self) -> Self {
                Self(self.0.ceil())
            }

            /// `fn:floor`.
            pub fn floor(self) -> Self {
                Self(self.0.floor())
            }

            /// `fn:round`: to the nearest integer, halves towards positive infinity
            /// (`-2.5` → `-2`), keeping the sign of zero (`-0.3` → `-0`).
            pub fn round(self) -> Self {
                let x = self.0;
                let floor = x.floor();
                // x - floor(x) is exact.
                let rounded = if x - floor >= 0.5 { floor + 1. } else { floor };
                Self(if rounded == 0. { (0.0 as $t).copysign(x) } else { rounded })
            }

            pub fn is_nan(self) -> bool {
                self.0.is_nan()
            }

            pub fn is_finite(self) -> bool {
                self.0.is_finite()
            }

            /// XSD identity: NaN is identical to NaN, `0` and `-0` are not identical.
            pub fn is_identical_with(self, other: Self) -> bool {
                (self.0.is_nan() && other.0.is_nan()) || self.0.to_bits() == other.0.to_bits()
            }

            pub fn to_be_bytes(self) -> [u8; std::mem::size_of::<$t>()] {
                self.0.to_be_bytes()
            }

            pub fn from_be_bytes(bytes: [u8; std::mem::size_of::<$t>()]) -> Self {
                Self(<$t>::from_be_bytes(bytes))
            }

            /// The XSD 1.1 canonical form: always scientific (`1.0E1`, `0.0E0`, `INF`).
            pub fn canonical(self) -> String {
                let x = self.0;
                if !x.is_finite() {
                    return self.to_string();
                }
                if x == 0. {
                    return if x.is_sign_negative() { "-0.0E0" } else { "0.0E0" }.to_owned();
                }
                with_point(format!("{x:E}"))
            }
        }

        $(
            impl From<$exact> for $name {
                fn from(value: $exact) -> Self {
                    Self(<$t>::from(value))
                }
            }
        )*

        impl From<$name> for $t {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl From<Boolean> for $name {
            fn from(value: Boolean) -> Self {
                Self(if bool::from(value) { 1. } else { 0. })
            }
        }

        /// Rounded to nearest.
        impl From<Integer> for $name {
            #[allow(clippy::cast_precision_loss)]
            fn from(value: Integer) -> Self {
                Self(i64::from(value) as $t)
            }
        }

        /// Only the XSD lexical forms (not Rust's `inf`, `infinity` or `nan`).
        impl FromStr for $name {
            type Err = ParseError;

            fn from_str(input: &str) -> Result<Self, ParseError> {
                if !is_lexical_form(input.as_bytes()) {
                    return Err(ParseError::new($xsd, "not a number, INF, -INF or NaN"));
                }
                Ok(Self(match input {
                    "INF" | "+INF" => <$t>::INFINITY,
                    "-INF" => <$t>::NEG_INFINITY,
                    "NaN" => <$t>::NAN,
                    _ => input
                        .parse()
                        .map_err(|_| ParseError::new($xsd, "not a number"))?,
                }))
            }
        }

        /// XPath's cast to `xs:string`: `NaN`, `INF`, `-INF`, `0`, `-0`; the decimal form
        /// for magnitudes from 10⁻⁶ up to 10⁶ (`10`, `0.5`), otherwise scientific
        /// (`1.0E20`, `1.5E-7`). The digits are the shortest that read back the same.
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let x = self.0;
                if x.is_nan() {
                    f.pad("NaN")
                } else if x == <$t>::INFINITY {
                    f.pad("INF")
                } else if x == <$t>::NEG_INFINITY {
                    f.pad("-INF")
                } else if x == 0. || (1e-6..1e6).contains(&x.abs()) {
                    f.pad(&format!("{x}"))
                } else {
                    f.pad(&with_point(format!("{x:E}")))
                }
            }
        }

        /// IEEE equality: NaN equals nothing, `0` equals `-0`.
        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.0 == other.0
            }
        }

        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                self.0.partial_cmp(&other.0)
            }
        }

        impl Neg for $name {
            type Output = Self;

            fn neg(self) -> Self {
                Self(-self.0)
            }
        }

        impl Add for $name {
            type Output = Self;

            fn add(self, rhs: Self) -> Self {
                Self(self.0 + rhs.0)
            }
        }

        impl Sub for $name {
            type Output = Self;

            fn sub(self, rhs: Self) -> Self {
                Self(self.0 - rhs.0)
            }
        }

        impl Mul for $name {
            type Output = Self;

            fn mul(self, rhs: Self) -> Self {
                Self(self.0 * rhs.0)
            }
        }

        impl Div for $name {
            type Output = Self;

            fn div(self, rhs: Self) -> Self {
                Self(self.0 / rhs.0)
            }
        }
    };
}

floating!(Float, f32, "float", exact: [f32, i8, i16, u8, u16]);
floating!(Double, f64, "double", exact: [f64, f32, i8, i16, i32, u8, u16, u32]);

/// Exact.
impl From<Float> for Double {
    fn from(value: Float) -> Self {
        Self(f64::from(value.0))
    }
}

/// Rounded to nearest.
impl From<Double> for Float {
    #[allow(clippy::cast_possible_truncation)]
    fn from(value: Double) -> Self {
        Self(value.0 as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_forms() {
        for (text, shown) in [
            ("NaN", "NaN"),
            ("INF", "INF"),
            ("+INF", "INF"),
            ("-INF", "-INF"),
            ("0.0E0", "0"),
            ("-0.0E0", "-0"),
            ("0.1e1", "1"),
            ("1.e1", "10"),
            ("-1.", "-1"),
            (".5", "0.5"),
            ("+1E+2", "100"),
            ("123456.5", "123456.5"),
            ("1000000", "1.0E6"),
            ("1e20", "1.0E20"),
            ("1.5e-7", "1.5E-7"),
            ("0.000001", "0.000001"),
            ("-2.5E300", "-2.5E300"),
        ] {
            let value: Double = text.parse().unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(value.to_string(), shown, "{text}");
        }
        for bad in [
            "", "inf", "infinity", "nan", "+NaN", "-NaN", "1e", "e1", ".", "1.0f", " 1", "0x1",
            "1_0", "INF1",
        ] {
            assert!(bad.parse::<Double>().is_err(), "{bad}");
            assert!(bad.parse::<Float>().is_err(), "{bad}");
        }
        assert_eq!("0.1".parse::<Float>().unwrap().to_string(), "0.1");
        assert_eq!("3.4028235E38".parse::<Float>().unwrap(), Float::MAX);
        assert_eq!(Float::from(1e10_f32).to_string(), "1.0E10");
    }

    #[test]
    fn canonical_forms() {
        assert_eq!(Double::from(10.).canonical(), "1.0E1");
        assert_eq!(Double::from(0.).canonical(), "0.0E0");
        assert_eq!(Double::from(-0.).canonical(), "-0.0E0");
        assert_eq!(Double::from(0.125).canonical(), "1.25E-1");
        assert_eq!(Double::NAN.canonical(), "NaN");
        assert_eq!(Float::NEG_INFINITY.canonical(), "-INF");
    }

    #[test]
    fn rounding_follows_xpath() {
        for (x, round) in [
            (2.5, 3.),
            (-2.5, -2.),
            (-2.6, -3.),
            (0.49999999999999994, 0.),
            (-0.5, -0.),
            (1e300, 1e300),
        ] {
            let r = f64::from(Double::from(x).round());
            assert!(
                r == round && r.is_sign_negative() == round.is_sign_negative(),
                "{x}: {r}"
            );
        }
        assert!(f64::from(Double::from(-0.3).round()).is_sign_negative());
        assert!(Double::NAN.round().is_nan());
        assert_eq!(f64::from(Double::INFINITY.round()), f64::INFINITY);
        assert_eq!(f32::from(Float::from(-2.5_f32).round()), -2.);
    }

    #[test]
    fn identity_and_equality() {
        assert!(Double::NAN != Double::NAN);
        assert!(Double::NAN.is_identical_with(Double::NAN));
        assert!(Double::from(0.) == Double::from(-0.));
        assert!(!Double::from(0.).is_identical_with(Double::from(-0.)));
        assert_eq!(
            Double::from(Float::from(0.1_f32)),
            Double::from(f64::from(0.1_f32))
        );
        assert_eq!(Float::from(Double::from(0.1)), Float::from(0.1_f32));
        assert_eq!(
            Double::from(Integer::from(i64::MAX)),
            Double::from(9.223_372_036_854_776e18)
        );
    }
}
