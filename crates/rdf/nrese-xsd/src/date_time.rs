//! `xsd:dateTime`, `xsd:date`, `xsd:time`, the five `xsd:g…` types and timezone offsets
//! (XSD 1.1 §3.3.7–3.3.15, §D).
//!
//! Every type is XSD's seven-property model (year, month, day, hour, minute, second,
//! timezone offset) with the properties it lacks at XSD's reference values (1972-12-31,
//! midnight), so comparison and arithmetic are one implementation on the timeline. The
//! calendar is the proleptic Gregorian one with a year 0 (1 BCE), as XSD 1.1 has it.
//!
//! Comparison without an implicit timezone is XSD's partial order: a value without a
//! timezone and one with it are ordered only if they are more than 14 hours apart. XPath
//! instead fills in an implicit timezone; [`DateTime::or_timezone`] and its siblings do
//! that before comparing.

use std::cmp::Ordering;
use std::fmt::{self, Write};
use std::hash::{Hash, Hasher};
use std::str::FromStr;

use crate::{DayTimeDuration, Decimal, Duration, ParseError, RangeError};

/// Years beyond ±10¹¹ are out of range (the timeline in seconds must fit a `Decimal`).
const MAX_YEAR: i64 = 100_000_000_000;
const SCALE: i128 = 1_000_000_000_000_000_000;
const DAY: i128 = 86_400;

// ---------------------------------------------------------------------------------------
// Calendar

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: u8) -> u8 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    let (month, day) = (i64::from(month), i64::from(day));
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date `days` after 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u8;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u8;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

// ---------------------------------------------------------------------------------------
// Timezone offsets

/// A timezone offset from UTC, in minutes (−14:00 to +14:00).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct TimezoneOffset {
    minutes: i16,
}

impl TimezoneOffset {
    pub const UTC: Self = Self { minutes: 0 };
    pub const MIN: Self = Self { minutes: -840 };
    pub const MAX: Self = Self { minutes: 840 };

    pub fn new(minutes: i16) -> Result<Self, RangeError> {
        if (-840..=840).contains(&minutes) {
            Ok(Self { minutes })
        } else {
            Err(RangeError("timezoneOffset"))
        }
    }

    pub fn in_minutes(self) -> i16 {
        self.minutes
    }
}

impl From<TimezoneOffset> for DayTimeDuration {
    fn from(value: TimezoneOffset) -> Self {
        DayTimeDuration::new(Decimal::from(i64::from(value.minutes) * 60))
    }
}

/// Whole minutes within ±14 hours.
impl TryFrom<DayTimeDuration> for TimezoneOffset {
    type Error = RangeError;

    fn try_from(value: DayTimeDuration) -> Result<Self, RangeError> {
        let raw = value.as_seconds().raw();
        if raw % (60 * SCALE) != 0 {
            return Err(RangeError("timezoneOffset"));
        }
        i16::try_from(raw / (60 * SCALE))
            .map_err(|_| RangeError("timezoneOffset"))
            .and_then(Self::new)
    }
}

/// `Z` for UTC, otherwise `±hh:mm`.
impl fmt::Display for TimezoneOffset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.minutes == 0 {
            return f.pad("Z");
        }
        let sign = if self.minutes < 0 { '-' } else { '+' };
        let m = self.minutes.unsigned_abs();
        f.pad(&format!("{sign}{:02}:{:02}", m / 60, m % 60))
    }
}

// ---------------------------------------------------------------------------------------
// The seven-property model

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Props {
    year: i64,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    /// `0 <= second < 60`.
    second: Decimal,
    timezone: Option<TimezoneOffset>,
}

impl Props {
    /// XSD's reference values for absent properties.
    const REFERENCE: Self = Self {
        year: 1972,
        month: 12,
        day: 31,
        hour: 0,
        minute: 0,
        second: Decimal::ZERO,
        timezone: None,
    };

    /// Local time on the timeline: seconds since 1970-01-01T00:00:00 of the same zone.
    fn local(&self) -> i128 {
        let days = i128::from(days_from_civil(self.year, self.month, self.day));
        let seconds = days * DAY + i128::from(self.hour) * 3600 + i128::from(self.minute) * 60;
        seconds * SCALE + self.second.raw()
    }

    /// The instant, as raw `Decimal` seconds since the epoch in UTC (a missing timezone
    /// counts as UTC).
    fn timeline(&self) -> i128 {
        let offset = self
            .timezone
            .map_or(0, |tz| i128::from(tz.minutes) * 60 * SCALE);
        self.local() - offset
    }

    /// The properties of local time `local` (raw `Decimal` seconds), with `timezone`.
    fn from_local(local: i128, timezone: Option<TimezoneOffset>) -> Option<Self> {
        let day = DAY * SCALE;
        let days = i64::try_from(local.div_euclid(day)).ok()?;
        let in_day = local.rem_euclid(day);
        let (year, month, day_of_month) = civil_from_days(days);
        if year.abs() > MAX_YEAR {
            return None;
        }
        Some(Self {
            year,
            month,
            day: day_of_month,
            hour: (in_day / (3600 * SCALE)) as u8,
            minute: (in_day % (3600 * SCALE) / (60 * SCALE)) as u8,
            second: Decimal::from_raw(in_day % (60 * SCALE)),
            timezone,
        })
    }

    /// XSD 1.1 §E.3.3, `dateTimePlusDuration`: the months first, the day pinned to the
    /// end of a shorter month, then the seconds.
    fn plus(&self, months: i64, seconds: Decimal) -> Option<Self> {
        let total = i128::from(self.year) * 12 + i128::from(self.month) - 1 + i128::from(months);
        let year = i64::try_from(total.div_euclid(12)).ok()?;
        if year.abs() > MAX_YEAR {
            return None;
        }
        let month = (total.rem_euclid(12) + 1) as u8;
        let day = self.day.min(days_in_month(year, month));
        let moved = Self {
            year,
            month,
            day,
            ..*self
        };
        Self::from_local(moved.local().checked_add(seconds.raw())?, self.timezone)
    }

    /// XSD's partial order (or the total one when both or neither have a timezone), of
    /// instants `a` and `b` computed with [`Props::timeline`].
    fn order(a: i128, a_zoned: bool, b: i128, b_zoned: bool) -> Option<Ordering> {
        let window = 14 * 3600 * SCALE;
        match (a_zoned, b_zoned) {
            (true, true) | (false, false) => Some(a.cmp(&b)),
            (true, false) => {
                let (early, late) = (a.cmp(&(b - window)), a.cmp(&(b + window)));
                (early == late).then_some(early)
            }
            (false, true) => {
                let (early, late) = ((a - window).cmp(&b), (a + window).cmp(&b));
                (early == late).then_some(early)
            }
        }
    }

    /// `fn:adjust-dateTime-to-timezone`: `None` drops the timezone keeping the local
    /// time; otherwise a value without a timezone gets it, keeping the local time, and a
    /// value with one moves to it, keeping the instant.
    fn adjust(&self, timezone: Option<TimezoneOffset>) -> Option<Self> {
        match (self.timezone, timezone) {
            (_, None) | (None, Some(_)) => Some(Self { timezone, ..*self }),
            (Some(_), Some(tz)) => Self::from_local(
                self.timeline() + i128::from(tz.minutes) * 60 * SCALE,
                timezone,
            ),
        }
    }
}

/// The properties of the `g` types: XSD's reference values for what they lack, the day
/// being the last of the month (timeOnTimeline, §E.3.4).
fn g_props(
    year: Option<i64>,
    month: Option<u8>,
    day: Option<u8>,
    timezone: Option<TimezoneOffset>,
) -> Props {
    let year = year.unwrap_or(Props::REFERENCE.year);
    let month = month.unwrap_or(Props::REFERENCE.month);
    let day = day.unwrap_or_else(|| days_in_month(year, month));
    Props {
        year,
        month,
        day,
        timezone,
        ..Props::REFERENCE
    }
}

/// The instant of reference dateTime `reference` (0 to 3) plus a duration, for XSD's
/// duration order (§E.2.2: 1696-09-01, 1697-02-01, 1903-03-01, 1903-07-01, all UTC).
pub(crate) fn reference_plus(reference: usize, months: i64, seconds: Decimal) -> Option<i128> {
    let (year, month) = [(1696, 9), (1697, 2), (1903, 3), (1903, 7)][reference];
    let start = Props {
        year,
        month,
        day: 1,
        timezone: Some(TimezoneOffset::UTC),
        ..Props::REFERENCE
    };
    Some(start.plus(months, seconds)?.timeline())
}

// ---------------------------------------------------------------------------------------
// Lexical forms

struct Cursor<'a> {
    text: &'a [u8],
    datatype: &'static str,
}

impl<'a> Cursor<'a> {
    fn fail<T>(&self, reason: &'static str) -> Result<T, ParseError> {
        Err(ParseError::new(self.datatype, reason))
    }

    fn expect(&mut self, byte: u8) -> Result<(), ParseError> {
        match self.text.split_first() {
            Some((&b, rest)) if b == byte => {
                self.text = rest;
                Ok(())
            }
            _ => self.fail("a missing separator"),
        }
    }

    fn two_digits(&mut self) -> Result<u8, ParseError> {
        match self.text {
            [a @ b'0'..=b'9', b @ b'0'..=b'9', rest @ ..] => {
                self.text = rest;
                Ok((a - b'0') * 10 + (b - b'0'))
            }
            _ => self.fail("two digits expected"),
        }
    }

    /// `-?([1-9][0-9]{3,}|0[0-9]{3})`
    fn year(&mut self) -> Result<i64, ParseError> {
        let negative = self.text.first() == Some(&b'-');
        if negative {
            self.text = &self.text[1..];
        }
        let digits = self.text.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits < 4 {
            return self.fail("a year of fewer than four digits");
        }
        if digits > 4 && self.text[0] == b'0' {
            return self.fail("a year of more than four digits with a leading zero");
        }
        let year: i64 = std::str::from_utf8(&self.text[..digits])
            .ok()
            .and_then(|t| t.parse().ok())
            .filter(|y| *y <= MAX_YEAR)
            .map_or_else(|| self.fail("a year out of range"), Ok)?;
        self.text = &self.text[digits..];
        Ok(if negative { -year } else { year })
    }

    fn month(&mut self) -> Result<u8, ParseError> {
        let month = self.two_digits()?;
        if (1..=12).contains(&month) {
            Ok(month)
        } else {
            self.fail("a month out of range")
        }
    }

    /// `hh:mm:ss(.s+)?`; `24:00:00` is returned as hour 24.
    fn time(&mut self) -> Result<(u8, u8, Decimal), ParseError> {
        let hour = self.two_digits()?;
        self.expect(b':')?;
        let minute = self.two_digits()?;
        self.expect(b':')?;
        let whole = self.two_digits()?;
        let mut second = Decimal::from(whole);
        if self.text.first() == Some(&b'.') {
            let digits = self.text[1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if digits == 0 {
                return self.fail("a '.' without digits after it");
            }
            let fraction = &self.text[1..=digits];
            let mut raw = 0_i128;
            let mut unit = SCALE;
            for &b in fraction.iter().take(18) {
                unit /= 10;
                raw += i128::from(b - b'0') * unit;
            }
            second = Decimal::from_raw(second.raw() + raw);
            self.text = &self.text[digits + 1..];
        }
        if minute > 59 || whole > 59 {
            return self.fail("minutes or seconds out of range");
        }
        if hour > 24 || (hour == 24 && (minute != 0 || second != Decimal::ZERO)) {
            return self.fail("an hour out of range");
        }
        Ok((hour, minute, second))
    }

    /// `(Z|(+|-)hh:mm)?` and the end of the text.
    fn timezone_and_end(&mut self) -> Result<Option<TimezoneOffset>, ParseError> {
        let timezone = match self.text {
            [] => None,
            [b'Z'] => Some(TimezoneOffset::UTC),
            [sign @ (b'+' | b'-'), rest @ ..] => {
                let negative = *sign == b'-';
                self.text = rest;
                let hours = self.two_digits()?;
                self.expect(b':')?;
                let minutes = self.two_digits()?;
                if minutes > 59 || hours > 14 || (hours == 14 && minutes != 0) {
                    return self.fail("a timezone out of range");
                }
                let total = i16::from(hours) * 60 + i16::from(minutes);
                if !self.text.is_empty() {
                    return self.fail("text after the timezone");
                }
                return Ok(Some(TimezoneOffset {
                    minutes: if negative { -total } else { total },
                }));
            }
            _ => return self.fail("unexpected text"),
        };
        Ok(timezone)
    }
}

fn write_year(out: &mut String, year: i64) {
    if year < 0 {
        out.push('-');
    }
    let _ = write!(out, "{:04}", year.unsigned_abs());
}

fn write_seconds(out: &mut String, second: Decimal) {
    if second < Decimal::from(10) {
        out.push('0');
    }
    let _ = write!(out, "{second}");
}

fn write_timezone(out: &mut String, timezone: Option<TimezoneOffset>) {
    if let Some(tz) = timezone {
        let _ = write!(out, "{tz}");
    }
}

// ---------------------------------------------------------------------------------------
// The types

macro_rules! temporal {
    ($(#[$doc:meta])* $name:ident, $xsd:literal) => {
        $(#[$doc])*
        ///
        /// Stored as its instant on the timeline and its timezone (32 bytes): comparing
        /// reads the instant, and the seven properties are derived from the two.
        #[derive(Debug, Clone, Copy)]
        pub struct $name {
            instant: i128,
            timezone: Option<TimezoneOffset>,
        }

        impl $name {
            fn wrap(props: Props) -> Self {
                $name { instant: props.timeline(), timezone: props.timezone }
            }

            /// The seven properties: the local time is the instant shifted by the zone.
            fn props(self) -> Props {
                let offset = self.timezone.map_or(0, |tz| i128::from(tz.minutes) * 60 * SCALE);
                Props::from_local(self.instant + offset, self.timezone)
                    .unwrap_or(Props::REFERENCE)
            }

            /// The timezone as a duration (`fn:timezone-from-…`).
            pub fn timezone(self) -> Option<DayTimeDuration> {
                self.timezone.map(Into::into)
            }

            pub fn timezone_offset(self) -> Option<TimezoneOffset> {
                self.timezone
            }

            /// With `timezone` if it has none: XPath's implicit timezone.
            pub fn or_timezone(self, timezone: TimezoneOffset) -> Self {
                let props = self.props();
                Self::wrap(Props { timezone: Some(props.timezone.unwrap_or(timezone)), ..props })
            }

            /// XSD identity: the same properties, timezone included.
            pub fn is_identical_with(self, other: Self) -> bool {
                self.instant == other.instant && self.timezone == other.timezone
            }
        }

        /// The same instant, both with a timezone or both without.
        impl PartialEq for $name {
            #[inline]
            fn eq(&self, other: &Self) -> bool {
                self.instant == other.instant
                    && self.timezone.is_some() == other.timezone.is_some()
            }
        }

        impl PartialOrd for $name {
            #[inline]
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                // Both with a timezone or both without: a total order of instants.
                if self.timezone.is_some() == other.timezone.is_some() {
                    return Some(self.instant.cmp(&other.instant));
                }
                Props::order(
                    self.instant,
                    self.timezone.is_some(),
                    other.instant,
                    other.timezone.is_some(),
                )
            }
        }

        impl Hash for $name {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.instant.hash(state);
                self.timezone.is_some().hash(state);
            }
        }

        impl FromStr for $name {
            type Err = ParseError;

            fn from_str(input: &str) -> Result<Self, ParseError> {
                let mut cursor = Cursor { text: input.as_bytes(), datatype: $xsd };
                Self::read(&mut cursor)
            }
        }
    };
}

temporal!(
    /// `xsd:dateTime` (and `xsd:dateTimeStamp` when it has a timezone).
    DateTime,
    "dateTime"
);
temporal!(
    /// `xsd:date`.
    Date,
    "date"
);
temporal!(
    /// `xsd:time`.
    Time,
    "time"
);
temporal!(
    /// `xsd:gYearMonth`.
    GYearMonth,
    "gYearMonth"
);
temporal!(
    /// `xsd:gYear`.
    GYear,
    "gYear"
);
temporal!(
    /// `xsd:gMonthDay`.
    GMonthDay,
    "gMonthDay"
);
temporal!(
    /// `xsd:gDay`.
    GDay,
    "gDay"
);
temporal!(
    /// `xsd:gMonth`.
    GMonth,
    "gMonth"
);

impl DateTime {
    /// The instant (raw `Decimal` seconds since the epoch in UTC, a missing timezone read
    /// as UTC) and the timezone in minutes: together what identifies the value (the OWL 2
    /// datatype map's identity, `owl` module).
    pub(crate) fn timeline(self) -> (i128, Option<i16>) {
        (self.instant, self.timezone.map(|t| t.minutes))
    }

    /// The current time, in UTC.
    pub fn now() -> Self {
        let since = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let raw =
            i128::from(since.as_secs()) * SCALE + i128::from(since.subsec_nanos()) * 1_000_000_000;
        Self::wrap(Props::from_local(raw, Some(TimezoneOffset::UTC)).unwrap_or(Props::REFERENCE))
    }

    fn read(c: &mut Cursor<'_>) -> Result<Self, ParseError> {
        let year = c.year()?;
        c.expect(b'-')?;
        let month = c.month()?;
        c.expect(b'-')?;
        let day = c.two_digits()?;
        if day == 0 || day > days_in_month(year, month) {
            return c.fail("a day out of range");
        }
        c.expect(b'T')?;
        let (hour, minute, second) = c.time()?;
        let timezone = c.timezone_and_end()?;
        let props = Props {
            year,
            month,
            day,
            hour: hour % 24,
            minute,
            second,
            timezone,
        };
        if hour == 24 {
            // 24:00:00 is midnight at the end of the day.
            return props
                .plus(0, Decimal::from(86_400))
                .map(Self::wrap)
                .ok_or(ParseError::new("dateTime", "a year out of range"));
        }
        Ok(Self::wrap(props))
    }

    pub fn year(self) -> i64 {
        self.props().year
    }

    pub fn month(self) -> u8 {
        self.props().month
    }

    pub fn day(self) -> u8 {
        self.props().day
    }

    pub fn hour(self) -> u8 {
        self.props().hour
    }

    pub fn minute(self) -> u8 {
        self.props().minute
    }

    pub fn second(self) -> Decimal {
        self.props().second
    }

    /// `fn:adjust-dateTime-to-timezone`.
    pub fn adjust(self, timezone: Option<TimezoneOffset>) -> Option<Self> {
        self.props().adjust(timezone).map(Self::wrap)
    }

    /// `op:add-yearMonthDuration-to-dateTime`, `op:add-dayTimeDuration-to-dateTime`.
    pub fn checked_add_duration(self, duration: impl Into<Duration>) -> Option<Self> {
        let d = duration.into();
        self.props()
            .plus(d.all_months(), d.as_seconds())
            .map(Self::wrap)
    }

    /// `op:subtract-…Duration-from-dateTime`.
    pub fn checked_sub_duration(self, duration: impl Into<Duration>) -> Option<Self> {
        self.checked_add_duration(duration.into().checked_neg()?)
    }

    /// `op:subtract-dateTimes`; `None` if only one has a timezone (see
    /// [`DateTime::or_timezone`]).
    pub fn checked_sub(self, other: Self) -> Option<DayTimeDuration> {
        difference(&self.props(), &other.props())
    }
}

impl Date {
    fn read(c: &mut Cursor<'_>) -> Result<Self, ParseError> {
        let year = c.year()?;
        c.expect(b'-')?;
        let month = c.month()?;
        c.expect(b'-')?;
        let day = c.two_digits()?;
        if day == 0 || day > days_in_month(year, month) {
            return c.fail("a day out of range");
        }
        let timezone = c.timezone_and_end()?;
        Ok(Self::wrap(Props {
            year,
            month,
            day,
            timezone,
            ..Props::REFERENCE
        }))
    }

    pub fn year(self) -> i64 {
        self.props().year
    }

    pub fn month(self) -> u8 {
        self.props().month
    }

    pub fn day(self) -> u8 {
        self.props().day
    }

    /// `fn:adjust-date-to-timezone`.
    pub fn adjust(self, timezone: Option<TimezoneOffset>) -> Option<Self> {
        let moved = self.props().adjust(timezone)?;
        Some(Self::wrap(Props {
            hour: 0,
            minute: 0,
            second: Decimal::ZERO,
            ..moved
        }))
    }

    /// `op:add-yearMonthDuration-to-date`, `op:add-dayTimeDuration-to-date`: the date
    /// part of the dateTime result.
    pub fn checked_add_duration(self, duration: impl Into<Duration>) -> Option<Self> {
        let d = duration.into();
        let moved = self.props().plus(d.all_months(), d.as_seconds())?;
        Some(Self::wrap(Props {
            hour: 0,
            minute: 0,
            second: Decimal::ZERO,
            ..moved
        }))
    }

    pub fn checked_sub_duration(self, duration: impl Into<Duration>) -> Option<Self> {
        self.checked_add_duration(duration.into().checked_neg()?)
    }

    /// `op:subtract-dates`.
    pub fn checked_sub(self, other: Self) -> Option<DayTimeDuration> {
        difference(&self.props(), &other.props())
    }
}

impl Time {
    fn read(c: &mut Cursor<'_>) -> Result<Self, ParseError> {
        let (hour, minute, second) = c.time()?;
        let timezone = c.timezone_and_end()?;
        Ok(Self::wrap(Props {
            hour: hour % 24,
            minute,
            second,
            timezone,
            ..Props::REFERENCE
        }))
    }

    pub fn hour(self) -> u8 {
        self.props().hour
    }

    pub fn minute(self) -> u8 {
        self.props().minute
    }

    pub fn second(self) -> Decimal {
        self.props().second
    }

    /// `fn:adjust-time-to-timezone`.
    pub fn adjust(self, timezone: Option<TimezoneOffset>) -> Option<Self> {
        let moved = self.props().adjust(timezone)?;
        Some(Self::wrap(Props {
            year: 1972,
            month: 12,
            day: 31,
            ..moved
        }))
    }

    /// `op:add-dayTimeDuration-to-time`: modulo 24 hours.
    pub fn checked_add_duration(self, duration: impl Into<DayTimeDuration>) -> Option<Self> {
        let moved = self.props().plus(0, duration.into().as_seconds())?;
        Some(Self::wrap(Props {
            year: 1972,
            month: 12,
            day: 31,
            ..moved
        }))
    }

    pub fn checked_sub_duration(self, duration: impl Into<DayTimeDuration>) -> Option<Self> {
        self.checked_add_duration(duration.into().checked_neg()?)
    }

    /// `op:subtract-times`.
    pub fn checked_sub(self, other: Self) -> Option<DayTimeDuration> {
        difference(&self.props(), &other.props())
    }
}

fn difference(a: &Props, b: &Props) -> Option<DayTimeDuration> {
    if a.timezone.is_some() != b.timezone.is_some() {
        return None;
    }
    Some(DayTimeDuration::new(Decimal::from_raw(
        a.timeline().checked_sub(b.timeline())?,
    )))
}

impl GYearMonth {
    fn read(c: &mut Cursor<'_>) -> Result<Self, ParseError> {
        let year = c.year()?;
        c.expect(b'-')?;
        let month = c.month()?;
        let timezone = c.timezone_and_end()?;
        Ok(Self::wrap(g_props(Some(year), Some(month), None, timezone)))
    }

    pub fn year(self) -> i64 {
        self.props().year
    }

    pub fn month(self) -> u8 {
        self.props().month
    }
}

impl GYear {
    fn read(c: &mut Cursor<'_>) -> Result<Self, ParseError> {
        let year = c.year()?;
        let timezone = c.timezone_and_end()?;
        Ok(Self::wrap(g_props(Some(year), None, None, timezone)))
    }

    pub fn year(self) -> i64 {
        self.props().year
    }
}

impl GMonthDay {
    fn read(c: &mut Cursor<'_>) -> Result<Self, ParseError> {
        c.expect(b'-')?;
        c.expect(b'-')?;
        let month = c.month()?;
        c.expect(b'-')?;
        let day = c.two_digits()?;
        // Any day the month can have: February 29 is allowed.
        if day == 0 || day > days_in_month(2000, month) {
            return c.fail("a day out of range");
        }
        let timezone = c.timezone_and_end()?;
        Ok(Self::wrap(g_props(None, Some(month), Some(day), timezone)))
    }

    pub fn month(self) -> u8 {
        self.props().month
    }

    pub fn day(self) -> u8 {
        self.props().day
    }
}

impl GDay {
    fn read(c: &mut Cursor<'_>) -> Result<Self, ParseError> {
        for _ in 0..3 {
            c.expect(b'-')?;
        }
        let day = c.two_digits()?;
        if day == 0 || day > 31 {
            return c.fail("a day out of range");
        }
        let timezone = c.timezone_and_end()?;
        Ok(Self::wrap(g_props(None, None, Some(day), timezone)))
    }

    pub fn day(self) -> u8 {
        self.props().day
    }
}

impl GMonth {
    fn read(c: &mut Cursor<'_>) -> Result<Self, ParseError> {
        c.expect(b'-')?;
        c.expect(b'-')?;
        let month = c.month()?;
        let timezone = c.timezone_and_end()?;
        Ok(Self::wrap(g_props(None, Some(month), None, timezone)))
    }

    pub fn month(self) -> u8 {
        self.props().month
    }
}

// ---------------------------------------------------------------------------------------
// Output: the canonical forms (XSD 1.1 keeps the timezone as given; UTC prints as Z).

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.props();
        let mut out = String::with_capacity(32);
        write_year(&mut out, p.year);
        let _ = write!(
            out,
            "-{:02}-{:02}T{:02}:{:02}:",
            p.month, p.day, p.hour, p.minute
        );
        write_seconds(&mut out, p.second);
        write_timezone(&mut out, p.timezone);
        f.pad(&out)
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.props();
        let mut out = String::with_capacity(16);
        write_year(&mut out, p.year);
        let _ = write!(out, "-{:02}-{:02}", p.month, p.day);
        write_timezone(&mut out, p.timezone);
        f.pad(&out)
    }
}

impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.props();
        let mut out = String::with_capacity(24);
        let _ = write!(out, "{:02}:{:02}:", p.hour, p.minute);
        write_seconds(&mut out, p.second);
        write_timezone(&mut out, p.timezone);
        f.pad(&out)
    }
}

impl fmt::Display for GYearMonth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(16);
        write_year(&mut out, self.props().year);
        let _ = write!(out, "-{:02}", self.props().month);
        write_timezone(&mut out, self.props().timezone);
        f.pad(&out)
    }
}

impl fmt::Display for GYear {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::with_capacity(12);
        write_year(&mut out, self.props().year);
        write_timezone(&mut out, self.props().timezone);
        f.pad(&out)
    }
}

impl fmt::Display for GMonthDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = format!("--{:02}-{:02}", self.props().month, self.props().day);
        write_timezone(&mut out, self.props().timezone);
        f.pad(&out)
    }
}

impl fmt::Display for GDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = format!("---{:02}", self.props().day);
        write_timezone(&mut out, self.props().timezone);
        f.pad(&out)
    }
}

impl fmt::Display for GMonth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = format!("--{:02}", self.props().month);
        write_timezone(&mut out, self.props().timezone);
        f.pad(&out)
    }
}

// ---------------------------------------------------------------------------------------
// Casts (XPath §19)

impl From<DateTime> for Date {
    fn from(value: DateTime) -> Self {
        Self::wrap(Props {
            hour: 0,
            minute: 0,
            second: Decimal::ZERO,
            ..value.props()
        })
    }
}

impl From<Date> for DateTime {
    fn from(value: Date) -> Self {
        Self::wrap(value.props())
    }
}

impl From<DateTime> for Time {
    fn from(value: DateTime) -> Self {
        Self::wrap(Props {
            year: 1972,
            month: 12,
            day: 31,
            ..value.props()
        })
    }
}

macro_rules! cast_to_g {
    ($from:ident => $to:ident ($year:expr, $month:expr, $day:expr)) => {
        impl From<$from> for $to {
            fn from(value: $from) -> Self {
                let p = value.props();
                let (year, month, day): (Option<i64>, Option<u8>, Option<u8>) =
                    ($year(p.year), $month(p.month), $day(p.day));
                Self::wrap(g_props(year, month, day, p.timezone))
            }
        }
    };
}
cast_to_g!(DateTime => GYearMonth (Some, Some, |_| None));
cast_to_g!(Date => GYearMonth (Some, Some, |_| None));
cast_to_g!(DateTime => GYear (Some, |_| None, |_| None));
cast_to_g!(Date => GYear (Some, |_| None, |_| None));
cast_to_g!(DateTime => GMonthDay (|_| None, Some, Some));
cast_to_g!(Date => GMonthDay (|_| None, Some, Some));
cast_to_g!(DateTime => GDay (|_| None, |_| None, Some));
cast_to_g!(Date => GDay (|_| None, |_| None, Some));
cast_to_g!(DateTime => GMonth (|_| None, Some, |_| None));
cast_to_g!(Date => GMonth (|_| None, Some, |_| None));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::YearMonthDuration;

    fn dt(text: &str) -> DateTime {
        text.parse().unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    #[test]
    fn the_calendar_round_trips() {
        for days in (-800_000..800_000).step_by(997) {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(-719_528), (0, 1, 1));
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(0, 2), 29);
    }

    #[test]
    fn lexical_forms() {
        for (text, shown) in [
            ("2002-10-10T12:00:00-05:00", "2002-10-10T12:00:00-05:00"),
            ("2002-10-10T17:00:00Z", "2002-10-10T17:00:00Z"),
            ("2002-10-10T17:00:00+00:00", "2002-10-10T17:00:00Z"),
            ("2002-10-10T12:00:00.500", "2002-10-10T12:00:00.5"),
            ("2002-10-10T12:00:09.0", "2002-10-10T12:00:09"),
            ("-0045-01-01T00:00:00", "-0045-01-01T00:00:00"),
            ("0000-01-01T00:00:00", "0000-01-01T00:00:00"),
            ("12000-02-29T00:00:00", "12000-02-29T00:00:00"),
            ("1999-12-31T24:00:00Z", "2000-01-01T00:00:00Z"),
            (
                "2000-01-01T00:00:00.0000000000000000019",
                "2000-01-01T00:00:00.000000000000000001",
            ),
        ] {
            assert_eq!(dt(text).to_string(), shown, "{text}");
        }
        for bad in [
            "",
            "2002-10-10",
            "02-10-10T12:00:00",
            "02002-10-10T12:00:00",
            "2002-13-10T12:00:00",
            "2002-02-29T12:00:00",
            "2002-10-32T12:00:00",
            "2002-10-10T25:00:00",
            "2002-10-10T24:00:01",
            "2002-10-10T12:60:00",
            "2002-10-10T12:00:60",
            "2002-10-10T12:00:00+14:01",
            "2002-10-10T12:00:00+15:00",
            "2002-10-10T12:00:00z",
            "2002-10-10T12:00",
            "2002-10-10T12:00:00.",
            "2002-10-10 12:00:00",
            "2002-10-10T12:00:00Z ",
            "+2002-10-10T12:00:00",
        ] {
            assert!(bad.parse::<DateTime>().is_err(), "{bad}");
        }
        assert_eq!(
            "2002-10-10+13:00".parse::<Date>().unwrap().to_string(),
            "2002-10-10+13:00"
        );
        assert_eq!("24:00:00".parse::<Time>().unwrap().to_string(), "00:00:00");
        assert_eq!(
            "13:20:00.123-05:00".parse::<Time>().unwrap().to_string(),
            "13:20:00.123-05:00"
        );
        assert_eq!(
            "2002-10Z".parse::<GYearMonth>().unwrap().to_string(),
            "2002-10Z"
        );
        assert_eq!("-0002".parse::<GYear>().unwrap().to_string(), "-0002");
        assert_eq!(
            "--02-29".parse::<GMonthDay>().unwrap().to_string(),
            "--02-29"
        );
        assert!("--02-30".parse::<GMonthDay>().is_err());
        assert_eq!(
            "---05+01:00".parse::<GDay>().unwrap().to_string(),
            "---05+01:00"
        );
        assert_eq!("--11".parse::<GMonth>().unwrap().to_string(), "--11");
        assert!("--11--".parse::<GMonth>().is_err());
    }

    #[test]
    fn comparison_on_the_timeline() {
        assert_eq!(
            dt("2002-04-02T12:00:00-01:00"),
            dt("2002-04-02T17:00:00+04:00")
        );
        assert_eq!(
            dt("2002-04-02T23:00:00-04:00"),
            dt("2002-04-03T02:00:00-01:00")
        );
        assert!(dt("2002-04-02T12:00:00") != dt("2002-04-02T12:00:00Z"));
        assert!(dt("2000-01-15T00:00:00") < dt("2000-02-15T00:00:00Z"));
        assert_eq!(
            dt("2000-01-01T12:00:00").partial_cmp(&dt("1999-12-31T23:00:00Z")),
            None
        );
        assert_eq!(
            dt("2000-01-01T12:00:00").or_timezone(TimezoneOffset::UTC),
            dt("2000-01-01T12:00:00Z")
        );
        let a: Date = "2004-12-25Z".parse().unwrap();
        let b: Date = "2004-12-25+07:00".parse().unwrap();
        assert!(a > b);
        let t1: Time = "08:00:00+09:00".parse().unwrap();
        let t2: Time = "17:00:00-06:00".parse().unwrap();
        assert!(t1 < t2);
        let g1: GYear = "2005-12:00".parse().unwrap();
        let g2: GYear = "2005+12:00".parse().unwrap();
        assert!(g1 != g2);
        assert!(dt("2000-01-01T00:00:00Z").is_identical_with(dt("2000-01-01T00:00:00Z")));
        assert!(!dt("2000-01-01T00:00:00Z").is_identical_with(dt("2000-01-01T01:00:00+01:00")));
    }

    #[test]
    fn arithmetic_as_xpath_shows() {
        let ym = |t: &str| t.parse::<YearMonthDuration>().unwrap();
        let dtd = |t: &str| t.parse::<DayTimeDuration>().unwrap();
        assert_eq!(
            dt("2000-10-30T11:12:00").checked_add_duration(ym("P1Y2M")),
            Some(dt("2001-12-30T11:12:00"))
        );
        assert_eq!(
            dt("2000-10-30T11:12:00").checked_add_duration(dtd("P3DT1H15M")),
            Some(dt("2000-11-02T12:27:00"))
        );
        assert_eq!(
            dt("2000-10-30T11:12:00").checked_sub_duration(ym("P1Y2M")),
            Some(dt("1999-08-30T11:12:00"))
        );
        assert_eq!(
            dt("2000-02-29T00:00:00").checked_add_duration(ym("P1Y")),
            Some(dt("2001-02-28T00:00:00"))
        );
        assert_eq!(
            dt("2000-01-31T00:00:00").checked_add_duration(ym("P1M")),
            Some(dt("2000-02-29T00:00:00"))
        );
        assert_eq!(
            dt("2000-10-30T06:12:00-05:00").checked_sub(dt("1999-11-28T09:00:00Z")),
            Some(dtd("P337DT2H12M"))
        );
        assert_eq!(
            dt("2000-10-30T06:12:00").checked_sub(dt("1999-11-28T09:00:00Z")),
            None
        );
        let date = |t: &str| t.parse::<Date>().unwrap();
        assert_eq!(
            date("2000-10-30").checked_sub(date("1999-11-28")),
            Some(dtd("P337D"))
        );
        assert_eq!(
            date("2004-10-30Z").checked_add_duration(dtd("P2DT2H30M0S")),
            Some(date("2004-11-01Z"))
        );
        assert_eq!(
            date("2000-10-30").checked_sub_duration(dtd("P3DT1H15M")),
            Some(date("2000-10-26"))
        );
        let time = |t: &str| t.parse::<Time>().unwrap();
        assert_eq!(
            time("11:12:00").checked_add_duration(dtd("P3DT1H15M")),
            Some(time("12:27:00"))
        );
        assert_eq!(
            time("23:12:00+03:00").checked_add_duration(dtd("P1DT3H15M")),
            Some(time("02:27:00+03:00"))
        );
        assert_eq!(
            time("11:12:00").checked_sub(time("04:00:00")),
            Some(dtd("PT7H12M"))
        );
        assert_eq!(
            time("11:00:00-05:00").checked_sub(time("21:30:00+05:30")),
            Some(dtd("PT0S"))
        );
        assert_eq!(
            time("17:00:00-06:00").checked_sub(time("08:00:00+09:00")),
            Some(dtd("P1D"))
        );
    }

    #[test]
    fn components_and_timezones() {
        let d = dt("1999-05-31T13:20:00.25-05:00");
        assert_eq!(
            (d.year(), d.month(), d.day(), d.hour(), d.minute()),
            (1999, 5, 31, 13, 20)
        );
        assert_eq!(d.second(), "0.25".parse().unwrap());
        assert_eq!(d.timezone().unwrap().to_string(), "-PT5H");
        assert_eq!(d.timezone_offset().unwrap().to_string(), "-05:00");
        assert_eq!(
            dt("2000-01-01T00:00:00Z").timezone().unwrap().to_string(),
            "PT0S"
        );
        assert_eq!(dt("2000-01-01T00:00:00").timezone(), None);
        let moved = d.adjust(Some(TimezoneOffset::new(600).unwrap())).unwrap();
        assert_eq!(moved.to_string(), "1999-06-01T04:20:00.25+10:00");
        assert_eq!(
            d.adjust(None).unwrap().to_string(),
            "1999-05-31T13:20:00.25"
        );
        assert_eq!(
            TimezoneOffset::try_from("-PT10H".parse::<DayTimeDuration>().unwrap()),
            TimezoneOffset::new(-600)
        );
        assert!(TimezoneOffset::try_from("PT15H".parse::<DayTimeDuration>().unwrap()).is_err());
        let now = DateTime::now();
        assert!(now.year() >= 2026 && now.timezone_offset() == Some(TimezoneOffset::UTC));
    }

    #[test]
    fn casts() {
        let d = dt("2002-10-10T12:00:00-05:00");
        assert_eq!(Date::from(d).to_string(), "2002-10-10-05:00");
        assert_eq!(Time::from(d).to_string(), "12:00:00-05:00");
        assert_eq!(GYearMonth::from(d).to_string(), "2002-10-05:00");
        assert_eq!(GMonthDay::from(Date::from(d)).to_string(), "--10-10-05:00");
        assert_eq!(
            DateTime::from(Date::from(d)).to_string(),
            "2002-10-10T00:00:00-05:00"
        );
        // A cast and a parse give the same value.
        assert!(GYear::from(d).is_identical_with("2002-05:00".parse().unwrap()));
        assert!(GYearMonth::from(d).is_identical_with("2002-10-05:00".parse().unwrap()));
        assert!(GMonth::from(d).is_identical_with("--10-05:00".parse().unwrap()));
        assert!(GDay::from(d).is_identical_with("---10-05:00".parse().unwrap()));
        assert!(GMonthDay::from(d).is_identical_with("--10-10-05:00".parse().unwrap()));
    }
}
