//! Literals of XSD values (`xsd` feature): each value as its XPath string form with its
//! datatype, and back from a literal of that datatype.

use nrese_xsd::{
    Boolean, Date, DateTime, DayTimeDuration, Decimal, Double, Duration, Float, GDay, GMonth,
    GMonthDay, GYear, GYearMonth, Integer, Time, YearMonthDuration,
};

use crate::term::{Literal, LiteralRef, NamedNodeRef};
use crate::vocab::xsd;

macro_rules! xsd_literal {
    ($($type:ident => $datatype:ident),* $(,)?) => {$(
        impl From<$type> for Literal {
            fn from(value: $type) -> Self {
                Literal::new_typed_literal(value.to_string(), xsd::$datatype)
            }
        }

        impl From<&$type> for Literal {
            fn from(value: &$type) -> Self {
                Literal::from(*value)
            }
        }

        /// The value of a literal of exactly this datatype.
        impl TryFrom<LiteralRef<'_>> for $type {
            type Error = NotOfDatatype;

            fn try_from(literal: LiteralRef<'_>) -> Result<Self, NotOfDatatype> {
                if literal.datatype() != xsd::$datatype {
                    return Err(NotOfDatatype(xsd::$datatype));
                }
                literal.value().parse().map_err(|_| NotOfDatatype(xsd::$datatype))
            }
        }

        impl TryFrom<&Literal> for $type {
            type Error = NotOfDatatype;

            fn try_from(literal: &Literal) -> Result<Self, NotOfDatatype> {
                literal.as_ref().try_into()
            }
        }
    )*};
}

xsd_literal!(
    Boolean => BOOLEAN,
    Integer => INTEGER,
    Decimal => DECIMAL,
    Float => FLOAT,
    Double => DOUBLE,
    DateTime => DATE_TIME,
    Date => DATE,
    Time => TIME,
    GYearMonth => G_YEAR_MONTH,
    GYear => G_YEAR,
    GMonthDay => G_MONTH_DAY,
    GDay => G_DAY,
    GMonth => G_MONTH,
    Duration => DURATION,
    YearMonthDuration => YEAR_MONTH_DURATION,
    DayTimeDuration => DAY_TIME_DURATION,
);

/// A literal that isn't a well-formed literal of the datatype.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("not a well-formed literal of {0}")]
pub struct NotOfDatatype(pub NamedNodeRef<'static>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_through_literals() {
        let d: Decimal = "12.50".parse().unwrap();
        let literal = Literal::from(d);
        assert_eq!(
            literal.to_string(),
            "\"12.5\"^^<http://www.w3.org/2001/XMLSchema#decimal>"
        );
        assert_eq!(Decimal::try_from(&literal), Ok(d));
        assert!(Integer::try_from(&literal).is_err());
        assert_eq!(Literal::from(Double::from(1e20)).value(), "1.0E20");
        let bad = Literal::new_typed_literal("x", xsd::DATE);
        assert!(Date::try_from(&bad).is_err());
    }
}
