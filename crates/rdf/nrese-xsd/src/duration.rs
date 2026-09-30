//! `xsd:duration` and its two totally ordered subtypes, `xsd:yearMonthDuration` and
//! `xsd:dayTimeDuration` (XSD 1.1 §3.3.6, §3.4.26, §3.4.27).

use std::cmp::Ordering;
use std::fmt::{self, Write};
use std::str::FromStr;

use crate::date_time::reference_plus;
use crate::{Decimal, ParseError};

const MINUTE: i128 = 60;
const HOUR: i128 = 3600;
const DAY: i128 = 86400;

/// `xsd:duration`: months and seconds, both of one sign.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Duration {
    months: i64,
    seconds: Decimal,
}

/// `xsd:yearMonthDuration`: months.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct YearMonthDuration {
    months: i64,
}

/// `xsd:dayTimeDuration`: seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct DayTimeDuration {
    seconds: Decimal,
}

impl Duration {
    /// `None` if the two parts have opposite signs.
    pub fn new(months: i64, seconds: Decimal) -> Option<Self> {
        let opposite =
            (months < 0 && seconds.is_positive()) || (months > 0 && seconds.is_negative());
        (!opposite).then_some(Self { months, seconds })
    }

    /// `fn:years-from-duration`.
    pub fn years(self) -> i64 {
        self.months / 12
    }

    /// `fn:months-from-duration`.
    pub fn months(self) -> i64 {
        self.months % 12
    }

    /// `fn:days-from-duration`.
    pub fn days(self) -> i64 {
        DayTimeDuration::from(self).days()
    }

    /// `fn:hours-from-duration`.
    pub fn hours(self) -> i64 {
        DayTimeDuration::from(self).hours()
    }

    /// `fn:minutes-from-duration`.
    pub fn minutes(self) -> i64 {
        DayTimeDuration::from(self).minutes()
    }

    /// `fn:seconds-from-duration`.
    pub fn seconds(self) -> Decimal {
        DayTimeDuration::from(self).seconds()
    }

    pub fn all_months(self) -> i64 {
        self.months
    }

    pub fn as_seconds(self) -> Decimal {
        self.seconds
    }

    pub fn checked_add(self, rhs: impl Into<Self>) -> Option<Self> {
        let rhs = rhs.into();
        Self::new(
            self.months.checked_add(rhs.months)?,
            self.seconds.checked_add(rhs.seconds)?,
        )
    }

    pub fn checked_sub(self, rhs: impl Into<Self>) -> Option<Self> {
        self.checked_add(rhs.into().checked_neg()?)
    }

    pub fn checked_neg(self) -> Option<Self> {
        Some(Self {
            months: self.months.checked_neg()?,
            seconds: self.seconds.checked_neg()?,
        })
    }

    pub fn is_identical_with(self, other: Self) -> bool {
        self == other
    }
}

/// XSD 1.1's partial order: comparable if adding both to each of four reference
/// dateTimes orders them the same way (`P1M` > `P27D`, `P1M` ? `P30D`).
impl PartialOrd for Duration {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.months == other.months {
            return Some(self.seconds.cmp(&other.seconds));
        }
        if self.seconds == other.seconds {
            return Some(self.months.cmp(&other.months));
        }
        let mut order = None;
        for reference in 0..4 {
            let a = reference_plus(reference, self.months, self.seconds)?;
            let b = reference_plus(reference, other.months, other.seconds)?;
            let this = a.cmp(&b);
            if order.is_some_and(|o| o != this) {
                return None;
            }
            order = Some(this);
        }
        order
    }
}

impl YearMonthDuration {
    pub const fn new(months: i64) -> Self {
        Self { months }
    }

    pub fn years(self) -> i64 {
        self.months / 12
    }

    pub fn months(self) -> i64 {
        self.months % 12
    }

    pub fn all_months(self) -> i64 {
        self.months
    }

    /// `op:add-yearMonthDurations`.
    pub fn checked_add(self, rhs: impl Into<Self>) -> Option<Self> {
        self.months.checked_add(rhs.into().months).map(Self::new)
    }

    /// `op:subtract-yearMonthDurations`.
    pub fn checked_sub(self, rhs: impl Into<Self>) -> Option<Self> {
        self.months.checked_sub(rhs.into().months).map(Self::new)
    }

    pub fn checked_neg(self) -> Option<Self> {
        self.months.checked_neg().map(Self::new)
    }

    /// `op:multiply-yearMonthDuration`: rounded to whole months, halves up.
    pub fn checked_mul(self, factor: Decimal) -> Option<Self> {
        let months = Decimal::from(self.months)
            .checked_mul(factor)?
            .checked_round()?;
        i64::try_from(months.trunc_i128()).ok().map(Self::new)
    }

    /// `op:divide-yearMonthDuration`: rounded to whole months, halves up.
    pub fn checked_div(self, divisor: Decimal) -> Option<Self> {
        let months = Decimal::from(self.months)
            .checked_div(divisor)?
            .checked_round()?;
        i64::try_from(months.trunc_i128()).ok().map(Self::new)
    }

    /// `op:divide-yearMonthDuration-by-yearMonthDuration`.
    pub fn checked_div_duration(self, divisor: Self) -> Option<Decimal> {
        Decimal::from(self.months).checked_div(Decimal::from(divisor.months))
    }

    pub fn is_identical_with(self, other: Self) -> bool {
        self == other
    }
}

impl DayTimeDuration {
    pub const fn new(seconds: Decimal) -> Self {
        Self { seconds }
    }

    /// Whole days, truncated towards zero.
    pub fn days(self) -> i64 {
        (self.seconds.trunc_i128() / DAY) as i64
    }

    pub fn hours(self) -> i64 {
        (self.seconds.trunc_i128() % DAY / HOUR) as i64
    }

    pub fn minutes(self) -> i64 {
        (self.seconds.trunc_i128() % HOUR / MINUTE) as i64
    }

    /// The seconds within the minute, with their fraction.
    pub fn seconds(self) -> Decimal {
        self.seconds
            .checked_rem(Decimal::from(60))
            .unwrap_or_default()
    }

    pub fn as_seconds(self) -> Decimal {
        self.seconds
    }

    /// `op:add-dayTimeDurations`.
    pub fn checked_add(self, rhs: impl Into<Self>) -> Option<Self> {
        self.seconds.checked_add(rhs.into().seconds).map(Self::new)
    }

    /// `op:subtract-dayTimeDurations`.
    pub fn checked_sub(self, rhs: impl Into<Self>) -> Option<Self> {
        self.seconds.checked_sub(rhs.into().seconds).map(Self::new)
    }

    pub fn checked_neg(self) -> Option<Self> {
        self.seconds.checked_neg().map(Self::new)
    }

    /// `op:multiply-dayTimeDuration`.
    pub fn checked_mul(self, factor: Decimal) -> Option<Self> {
        self.seconds.checked_mul(factor).map(Self::new)
    }

    /// `op:divide-dayTimeDuration`.
    pub fn checked_div(self, divisor: Decimal) -> Option<Self> {
        self.seconds.checked_div(divisor).map(Self::new)
    }

    /// `op:divide-dayTimeDuration-by-dayTimeDuration`.
    pub fn checked_div_duration(self, divisor: Self) -> Option<Decimal> {
        self.seconds.checked_div(divisor.seconds)
    }

    pub fn is_identical_with(self, other: Self) -> bool {
        self == other
    }
}

impl From<YearMonthDuration> for Duration {
    fn from(value: YearMonthDuration) -> Self {
        Self {
            months: value.months,
            seconds: Decimal::ZERO,
        }
    }
}

impl From<DayTimeDuration> for Duration {
    fn from(value: DayTimeDuration) -> Self {
        Self {
            months: 0,
            seconds: value.seconds,
        }
    }
}

/// XPath cast: the day and time part is dropped.
impl From<Duration> for YearMonthDuration {
    fn from(value: Duration) -> Self {
        Self::new(value.months)
    }
}

/// XPath cast: the year and month part is dropped.
impl From<Duration> for DayTimeDuration {
    fn from(value: Duration) -> Self {
        Self::new(value.seconds)
    }
}

// ---------------------------------------------------------------------------------------
// Lexical forms

/// Which parts a duration's lexical form has.
#[derive(Default)]
struct Lexical {
    negative: bool,
    months: i64,
    seconds: Decimal,
    year_month: bool,
    day_time: bool,
}

/// `-?P(nY)?(nM)?(nD)?(T(nH)?(nM)?(n(.n)?S)?)?` with at least one part, and one after
/// `T` if there is a `T`.
fn parse(input: &str, datatype: &'static str) -> Result<Lexical, ParseError> {
    let err = |reason| ParseError::new(datatype, reason);
    let mut s = input.as_bytes();
    let mut out = Lexical::default();
    if let [b'-', rest @ ..] = s {
        out.negative = true;
        s = rest;
    }
    let [b'P', rest @ ..] = s else {
        return Err(err("no P"));
    };
    s = rest;
    // Designators in order: Y M D, then after T: H M S.
    let mut next = 0;
    let mut in_time = false;
    let mut time_parts = 0;
    let mut days: i128 = 0;
    let mut clock: i128 = 0;
    let mut second_fraction = Decimal::ZERO;
    while !s.is_empty() {
        if s[0] == b'T' {
            if in_time {
                return Err(err("two T"));
            }
            in_time = true;
            next = next.max(3);
            s = &s[1..];
            continue;
        }
        let digits = s.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 {
            return Err(err("a part without digits"));
        }
        let number = &s[..digits];
        s = &s[digits..];
        let mut fraction: &[u8] = &[];
        if in_time && s.first() == Some(&b'.') {
            let more = s[1..].iter().take_while(|b| b.is_ascii_digit()).count();
            if more == 0 {
                return Err(err("a '.' without digits after it"));
            }
            fraction = &s[1..=more];
            s = &s[more + 1..];
            if s.first() != Some(&b'S') {
                return Err(err("a fraction on a part other than seconds"));
            }
        }
        let Some((&designator, rest)) = s.split_first() else {
            return Err(err("a number without designator"));
        };
        s = rest;
        let slot = match (in_time, designator) {
            (false, b'Y') => 0,
            (false, b'M') => 1,
            (false, b'D') => 2,
            (true, b'H') => 3,
            (true, b'M') => 4,
            (true, b'S') => 5,
            _ => return Err(err("an unknown or misplaced designator")),
        };
        if slot < next {
            return Err(err("parts out of order"));
        }
        next = slot + 1;
        let value: i128 = std::str::from_utf8(number)
            .ok()
            .and_then(|n| n.parse().ok())
            .filter(|&v: &i128| v < 1 << 100)
            .ok_or(err("a number too large"))?;
        let too_large = || err("too large");
        match slot {
            0 | 1 => {
                let months = if slot == 0 {
                    value.checked_mul(12).ok_or_else(too_large)?
                } else {
                    value
                };
                out.months =
                    i64::try_from(i128::from(out.months) + months).map_err(|_| too_large())?;
                out.year_month = true;
            }
            2 => {
                days = value;
                out.day_time = true;
            }
            _ => {
                clock = clock
                    .checked_add(value * [HOUR, MINUTE, 1][slot - 3])
                    .ok_or_else(too_large)?;
                if slot == 5 && !fraction.is_empty() {
                    let text = format!("0.{}", std::str::from_utf8(fraction).unwrap_or("0"));
                    second_fraction = text.parse().map_err(|_| too_large())?;
                }
                time_parts += 1;
                out.day_time = true;
            }
        }
    }
    if !(out.year_month || out.day_time) {
        return Err(err("no parts"));
    }
    if in_time && time_parts == 0 {
        return Err(err("a T without parts after it"));
    }
    let whole = days
        .checked_mul(DAY)
        .and_then(|d| d.checked_add(clock))
        .ok_or_else(|| err("too large"))?;
    out.seconds = Decimal::try_from(whole)
        .ok()
        .and_then(|w| w.checked_add(second_fraction))
        .ok_or(err("too large"))?;
    if out.negative {
        out.months = -out.months;
        out.seconds = out.seconds.checked_neg().ok_or(err("too large"))?;
    }
    Ok(out)
}

impl FromStr for Duration {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, ParseError> {
        let l = parse(input, "duration")?;
        Ok(Self {
            months: l.months,
            seconds: l.seconds,
        })
    }
}

impl FromStr for YearMonthDuration {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, ParseError> {
        let l = parse(input, "yearMonthDuration")?;
        if l.day_time {
            return Err(ParseError::new("yearMonthDuration", "a day or time part"));
        }
        Ok(Self::new(l.months))
    }
}

impl FromStr for DayTimeDuration {
    type Err = ParseError;

    fn from_str(input: &str) -> Result<Self, ParseError> {
        let l = parse(input, "dayTimeDuration")?;
        if l.year_month {
            return Err(ParseError::new("dayTimeDuration", "a year or month part"));
        }
        Ok(Self::new(l.seconds))
    }
}

/// The canonical form: `-?P(nY)?(nM)?(nD)?(T(nH)?(nM)?(nS)?)?`, zero parts left out.
fn write(f: &mut fmt::Formatter<'_>, months: i64, seconds: Decimal, zero: &str) -> fmt::Result {
    if months == 0 && seconds == Decimal::ZERO {
        return f.pad(zero);
    }
    let mut out = String::with_capacity(32);
    if months < 0 || seconds.is_negative() {
        out.push('-');
    }
    out.push('P');
    let (years, months) = (months.unsigned_abs() / 12, months.unsigned_abs() % 12);
    if years != 0 {
        write!(out, "{years}Y")?;
    }
    if months != 0 {
        write!(out, "{months}M")?;
    }
    let seconds = seconds.checked_abs().unwrap_or(Decimal::MAX);
    let whole = seconds.trunc_i128();
    let (days, hours, minutes) = (whole / DAY, whole % DAY / HOUR, whole % HOUR / MINUTE);
    let rest = seconds.checked_rem(Decimal::from(60)).unwrap_or_default();
    if days != 0 {
        write!(out, "{days}D")?;
    }
    if hours != 0 || minutes != 0 || rest != Decimal::ZERO {
        out.push('T');
        if hours != 0 {
            write!(out, "{hours}H")?;
        }
        if minutes != 0 {
            write!(out, "{minutes}M")?;
        }
        if rest != Decimal::ZERO {
            write!(out, "{rest}S")?;
        }
    }
    f.pad(&out)
}

impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write(f, self.months, self.seconds, "PT0S")
    }
}

impl fmt::Display for YearMonthDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write(f, self.months, Decimal::ZERO, "P0M")
    }
}

impl fmt::Display for DayTimeDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write(f, 0, self.seconds, "PT0S")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dur(text: &str) -> Duration {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn lexical_forms() {
        for (text, shown) in [
            ("P1Y2M3DT10H30M", "P1Y2M3DT10H30M"),
            ("-P120D", "-P120D"),
            ("P0Y", "PT0S"),
            ("PT0S", "PT0S"),
            ("P13M", "P1Y1M"),
            ("PT36H", "P1DT12H"),
            ("PT1.50S", "PT1.5S"),
            ("PT0.000000000000000001S", "PT0.000000000000000001S"),
            ("P1DT0H", "P1D"),
            ("-PT1M30.5S", "-PT1M30.5S"),
        ] {
            assert_eq!(dur(text).to_string(), shown, "{text}");
        }
        for bad in [
            "", "P", "PT", "P1D T1H", "P1H", "PT1D", "P1M1Y", "P1.5Y", "PT1.S", "P-1D", "1D",
            "P1Y2", "PT1H1H", "P1DT", "+P1D",
        ] {
            assert!(bad.parse::<Duration>().is_err(), "{bad}");
        }
        assert_eq!(
            "P1Y".parse::<YearMonthDuration>().unwrap().to_string(),
            "P1Y"
        );
        assert_eq!(
            "P0Y".parse::<YearMonthDuration>().unwrap().to_string(),
            "P0M"
        );
        assert!("P1D".parse::<YearMonthDuration>().is_err());
        assert!("P1M".parse::<DayTimeDuration>().is_err());
        assert_eq!(
            "PT0S".parse::<DayTimeDuration>().unwrap().to_string(),
            "PT0S"
        );
    }

    #[test]
    fn components_as_xpath_reads_them() {
        let d = dur("-P1Y14M3DT25H61M1.5S");
        assert_eq!((d.years(), d.months()), (-2, -2));
        assert_eq!((d.days(), d.hours(), d.minutes()), (-4, -2, -1));
        assert_eq!(d.seconds(), "-1.5".parse().unwrap());
    }

    #[test]
    fn order_and_equality() {
        assert_eq!(dur("P1Y"), dur("P12M"));
        assert_eq!(dur("PT24H"), dur("P1D"));
        assert_ne!(dur("P1M"), dur("P30D"));
        assert_eq!(dur("P1M").partial_cmp(&dur("P30D")), None);
        assert_eq!(
            dur("P1M").partial_cmp(&dur("P27D")),
            Some(Ordering::Greater)
        );
        assert_eq!(dur("P1M").partial_cmp(&dur("P32D")), Some(Ordering::Less));
        assert_eq!(dur("P1Y").partial_cmp(&dur("P365D")), None);
        assert_eq!(dur("P1Y").partial_cmp(&dur("P367D")), Some(Ordering::Less));
    }

    #[test]
    fn arithmetic() {
        let ym = |t: &str| t.parse::<YearMonthDuration>().unwrap();
        let dt = |t: &str| t.parse::<DayTimeDuration>().unwrap();
        assert_eq!(ym("P2Y11M").checked_add(ym("P3Y3M")), Some(ym("P6Y2M")));
        assert_eq!(
            ym("P2Y11M").checked_mul("2.3".parse().unwrap()),
            Some(ym("P6Y9M"))
        );
        assert_eq!(
            ym("P2Y11M").checked_div("1.5".parse().unwrap()),
            Some(ym("P1Y11M"))
        );
        assert_eq!(
            ym("P3Y4M").checked_div_duration(ym("-P1Y4M")),
            Some("-2.5".parse().unwrap())
        );
        assert_eq!(
            dt("P2DT12H5M").checked_sub(dt("P1DT10H30M")),
            Some(dt("P1DT1H35M"))
        );
        assert_eq!(
            dt("PT2H10M").checked_mul("2.1".parse().unwrap()),
            Some(dt("PT4H33M"))
        );
        assert_eq!(
            dt("P1DT2H30M10.5S").checked_div("1.5".parse().unwrap()),
            Some(dt("PT17H40M7S"))
        );
        assert_eq!(dur("P1M").checked_add(dur("-P1D")), None);
    }
}
