//! Dates, times and durations in expressions (SEP-0002, as SPARQL 1.2 takes it up): the
//! arithmetic XPath and Functions 3.1 defines on them (§9.6–9.7 on durations, §10.8 on
//! dates and times), `ADJUST` (§10.7), and sums and averages of durations (`fn:sum`,
//! `fn:avg`). The values come from `nrese-xsd`; this module only maps the operators.
//!
//! What XPath doesn't define is an error, as everywhere in SPARQL: `xsd:duration` (the
//! general type) has no arithmetic of its own, a time has no years or months to add, and
//! a sum mixes no two duration types.

use nrese_xsd::{DayTimeDuration, Decimal, TimezoneOffset, YearMonthDuration};
use oxrdf::vocab::xsd;
use oxrdf::{Literal, NamedNodeRef, Term};

use super::value::Value;

fn typed(lexical: String, datatype: NamedNodeRef<'_>) -> Term {
    Literal::new_typed_literal(lexical, datatype).into()
}

/// The term of a calendar or duration value.
fn term(value: Value) -> Option<Term> {
    Some(match value {
        Value::DateTime(v) => typed(v.to_string(), xsd::DATE_TIME),
        Value::Date(v) => typed(v.to_string(), xsd::DATE),
        Value::Time(v) => typed(v.to_string(), xsd::TIME),
        Value::Duration(v) => typed(v.to_string(), xsd::DURATION),
        Value::YearMonthDuration(v) => typed(v.to_string(), xsd::YEAR_MONTH_DURATION),
        Value::DayTimeDuration(v) => typed(v.to_string(), xsd::DAY_TIME_DURATION),
        Value::Decimal(v) => typed(v.to_string(), xsd::DECIMAL),
        _ => return None,
    })
}

/// A duration that is all days and time (a day-time duration, or a general one without
/// months): what can be added to a time.
fn day_time(value: &Value) -> Option<DayTimeDuration> {
    match value {
        Value::DayTimeDuration(d) => Some(*d),
        Value::Duration(d) if d.months() == 0 => Some((*d).into()),
        _ => None,
    }
}

/// A number as a decimal, for multiplying and dividing durations. XPath multiplies by a
/// double; NaN and infinities are errors there too.
fn factor(value: &Value) -> Option<Decimal> {
    match value {
        Value::Integer(i) => Some(Decimal::from(*i)),
        Value::Decimal(d) => Some(*d),
        Value::Float(f) => Decimal::try_from(*f).ok(),
        Value::Double(d) => Decimal::try_from(*d).ok(),
        _ => None,
    }
}

/// `a + b`: duration + duration of the same type, and a date, time or dateTime plus a
/// duration (either way round).
pub(crate) fn add(a: &Value, b: &Value) -> Option<Term> {
    use Value as V;
    term(match (a, b) {
        (V::YearMonthDuration(x), V::YearMonthDuration(y)) => {
            V::YearMonthDuration(x.checked_add(*y)?)
        }
        (V::DayTimeDuration(x), V::DayTimeDuration(y)) => V::DayTimeDuration(x.checked_add(*y)?),
        (V::DateTime(d), x) | (x, V::DateTime(d)) => {
            V::DateTime(d.checked_add_duration(x.duration()?)?)
        }
        (V::Date(d), x) | (x, V::Date(d)) => V::Date(d.checked_add_duration(x.duration()?)?),
        (V::Time(t), x) | (x, V::Time(t)) => V::Time(t.checked_add_duration(day_time(x)?)?),
        _ => return None,
    })
}

/// `a - b`: the duration between two dates, times or dateTimes; duration − duration of
/// the same type; a date, time or dateTime minus a duration.
pub(crate) fn subtract(a: &Value, b: &Value) -> Option<Term> {
    use Value as V;
    term(match (a, b) {
        (V::DateTime(x), V::DateTime(y)) => V::DayTimeDuration(x.checked_sub(*y)?),
        (V::Date(x), V::Date(y)) => V::DayTimeDuration(x.checked_sub(*y)?),
        (V::Time(x), V::Time(y)) => V::DayTimeDuration(x.checked_sub(*y)?),
        (V::YearMonthDuration(x), V::YearMonthDuration(y)) => {
            V::YearMonthDuration(x.checked_sub(*y)?)
        }
        (V::DayTimeDuration(x), V::DayTimeDuration(y)) => V::DayTimeDuration(x.checked_sub(*y)?),
        (V::DateTime(d), x) => V::DateTime(d.checked_sub_duration(x.duration()?)?),
        (V::Date(d), x) => V::Date(d.checked_sub_duration(x.duration()?)?),
        (V::Time(t), x) => V::Time(t.checked_sub_duration(day_time(x)?)?),
        _ => return None,
    })
}

/// `a * b`: a year-month or day-time duration times a number (either way round).
pub(crate) fn multiply(a: &Value, b: &Value) -> Option<Term> {
    use Value as V;
    term(match (a, b) {
        (V::YearMonthDuration(d), n) | (n, V::YearMonthDuration(d)) => {
            V::YearMonthDuration(d.checked_mul(factor(n)?)?)
        }
        (V::DayTimeDuration(d), n) | (n, V::DayTimeDuration(d)) => {
            V::DayTimeDuration(d.checked_mul(factor(n)?)?)
        }
        _ => return None,
    })
}

/// `a / b`: a duration by a number, or by a duration of the same type (a decimal).
pub(crate) fn divide(a: &Value, b: &Value) -> Option<Term> {
    use Value as V;
    term(match (a, b) {
        (V::YearMonthDuration(x), V::YearMonthDuration(y)) => {
            V::Decimal(x.checked_div_duration(*y)?)
        }
        (V::DayTimeDuration(x), V::DayTimeDuration(y)) => V::Decimal(x.checked_div_duration(*y)?),
        (V::YearMonthDuration(d), n) => V::YearMonthDuration(d.checked_div(factor(n)?)?),
        (V::DayTimeDuration(d), n) => V::DayTimeDuration(d.checked_div(factor(n)?)?),
        _ => return None,
    })
}

/// `-a` of a duration.
pub(crate) fn negate(a: &Value) -> Option<Term> {
    term(match a {
        Value::Duration(d) => Value::Duration(d.checked_neg()?),
        Value::YearMonthDuration(d) => Value::YearMonthDuration(d.checked_neg()?),
        Value::DayTimeDuration(d) => Value::DayTimeDuration(d.checked_neg()?),
        _ => return None,
    })
}

/// `ADJUST(value, timezone)`: a dateTime, date or time moved to the timezone (a day-time
/// duration between -PT14H and PT14H), as XPath's `fn:adjust-*-to-timezone`.
pub(crate) fn adjust(value: &Value, timezone: &Value) -> Option<Term> {
    let offset = Some(TimezoneOffset::try_from(day_time(timezone)?).ok()?);
    term(match value {
        Value::DateTime(d) => Value::DateTime(d.adjust(offset)?),
        Value::Date(d) => Value::Date(d.adjust(offset)?),
        Value::Time(t) => Value::Time(t.adjust(offset)?),
        _ => return None,
    })
}

/// Whether `value` is a year-month or day-time duration (what `SUM` and `AVG` take
/// besides numbers).
pub(crate) fn is_summable_duration(value: &Value) -> bool {
    matches!(
        value,
        Value::YearMonthDuration(_) | Value::DayTimeDuration(_)
    )
}

/// `SUM` of durations, all of one type; `None` if a value isn't one or overflows.
pub(crate) fn sum(values: &[Value]) -> Option<Term> {
    term(sum_value(values)?)
}

fn sum_value(values: &[Value]) -> Option<Value> {
    match values.first()? {
        Value::YearMonthDuration(_) => {
            let mut total = YearMonthDuration::default();
            for value in values {
                let Value::YearMonthDuration(d) = value else {
                    return None;
                };
                total = total.checked_add(*d)?;
            }
            Some(Value::YearMonthDuration(total))
        }
        Value::DayTimeDuration(_) => {
            let mut total = DayTimeDuration::default();
            for value in values {
                let Value::DayTimeDuration(d) = value else {
                    return None;
                };
                total = total.checked_add(*d)?;
            }
            Some(Value::DayTimeDuration(total))
        }
        _ => None,
    }
}

/// `AVG` of durations, all of one type.
pub(crate) fn average(values: &[Value]) -> Option<Term> {
    let count = Decimal::from(i64::try_from(values.len()).ok()?);
    term(match sum_value(values)? {
        Value::YearMonthDuration(d) => Value::YearMonthDuration(d.checked_div(count)?),
        Value::DayTimeDuration(d) => Value::DayTimeDuration(d.checked_div(count)?),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(lexical: &str, datatype: NamedNodeRef<'_>) -> Value {
        Value::of(&typed(lexical.to_owned(), datatype))
    }

    fn lexical(term: Option<Term>) -> String {
        match term {
            Some(Term::Literal(l)) => format!(
                "{}^^{}",
                l.value(),
                l.datatype().as_str().rsplit('#').next().unwrap_or("")
            ),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn xpath_examples() {
        let dt = |s| value(s, xsd::DATE_TIME);
        let d = |s| value(s, xsd::DATE);
        let t = |s| value(s, xsd::TIME);
        let ym = |s| value(s, xsd::YEAR_MONTH_DURATION);
        let dtd = |s| value(s, xsd::DAY_TIME_DURATION);
        let int = |s| value(s, xsd::INTEGER);
        let dbl = |s| value(s, xsd::DOUBLE);
        // F&O 3.1 examples, §9.6 and §10.8.
        assert_eq!(
            lexical(subtract(
                &dt("2000-10-30T06:12:00-05:00"),
                &dt("1999-11-28T09:00:00Z")
            )),
            "P337DT2H12M^^dayTimeDuration"
        );
        assert_eq!(
            lexical(subtract(&d("2000-10-30"), &d("1999-11-28"))),
            "P337D^^dayTimeDuration"
        );
        assert_eq!(
            lexical(subtract(&t("11:12:00Z"), &t("04:00:00-05:00"))),
            "PT2H12M^^dayTimeDuration"
        );
        assert_eq!(
            lexical(add(&dt("2000-10-30T11:12:00"), &ym("P1Y2M"))),
            "2001-12-30T11:12:00^^dateTime"
        );
        assert_eq!(
            lexical(add(&ym("P1Y2M"), &dt("2000-10-30T11:12:00"))),
            "2001-12-30T11:12:00^^dateTime"
        );
        assert_eq!(
            lexical(add(&d("2004-10-30Z"), &dtd("P2DT2H30M0S"))),
            "2004-11-01Z^^date"
        );
        assert_eq!(
            lexical(add(&t("11:12:00"), &dtd("P3DT1H15M"))),
            "12:27:00^^time"
        );
        assert_eq!(
            lexical(subtract(&d("2000-10-30"), &ym("P1Y2M"))),
            "1999-08-30^^date"
        );
        assert_eq!(
            lexical(add(&ym("P2Y11M"), &ym("P3Y3M"))),
            "P6Y2M^^yearMonthDuration"
        );
        assert_eq!(
            lexical(subtract(&dtd("P2DT12H"), &dtd("P1DT10H30M"))),
            "P1DT1H30M^^dayTimeDuration"
        );
        assert_eq!(
            lexical(multiply(&ym("P2Y11M"), &dbl("2.3"))),
            "P6Y9M^^yearMonthDuration"
        );
        assert_eq!(
            lexical(multiply(&int("2"), &dtd("PT2H10M"))),
            "PT4H20M^^dayTimeDuration"
        );
        assert_eq!(
            lexical(divide(&ym("P2Y11M"), &dbl("1.5"))),
            "P1Y11M^^yearMonthDuration"
        );
        assert_eq!(
            lexical(divide(&ym("P3Y4M"), &ym("-P1Y4M"))),
            "-2.5^^decimal"
        );
        // "1.4378349…": the digits after these are the implementation's.
        assert!(lexical(divide(&dtd("P2DT53M11S"), &dtd("P1DT10H"))).starts_with("1.4378349"));
        assert_eq!(lexical(negate(&ym("P1Y"))), "-P1Y^^yearMonthDuration");
        assert_eq!(
            lexical(adjust(&dt("2002-03-07T10:00:00-07:00"), &dtd("PT10H"))),
            "2002-03-08T03:00:00+10:00^^dateTime"
        );
        // What XPath doesn't define is an error.
        assert!(add(&t("11:12:00"), &ym("P1M")).is_none());
        assert!(add(&dt("2000-10-30T11:12:00"), &dt("2000-10-30T11:12:00")).is_none());
        assert!(divide(&dtd("PT1H"), &int("0")).is_none());
        assert!(adjust(&dt("2002-03-07T10:00:00"), &dtd("PT15H")).is_none());
        assert!(multiply(&value("P1Y", xsd::DURATION), &int("2")).is_none());
    }

    #[test]
    fn duration_aggregates() {
        let ym = |s| value(s, xsd::YEAR_MONTH_DURATION);
        let dtd = |s| value(s, xsd::DAY_TIME_DURATION);
        assert_eq!(
            lexical(sum(&[ym("P1Y"), ym("P6M")])),
            "P1Y6M^^yearMonthDuration"
        );
        assert_eq!(
            lexical(average(&[dtd("PT1H"), dtd("PT2H")])),
            "PT1H30M^^dayTimeDuration"
        );
        assert!(sum(&[ym("P1Y"), dtd("PT1H")]).is_none());
    }
}
